use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Write;
use std::{fs, path::PathBuf, process::ExitCode};
use webhook_reconcile::types::{RefEntry, ReferenceSet, Status};
use webhook_reconcile::{
    decode_missing, diff, fetch_reference, parse_received, parse_reference, raw_for_missing,
    refetch_missing, webhook_payload, with_logs_from_raw, HeliusRpc, Idl, RecordedPages,
    RecordedSource, ReferenceSource, TxSource, Window,
};

/// The provenance tags a diff report may carry. Any other value, or none, is unknown provenance.
const KNOWN_TAGS: [&str; 3] = ["run live", "mocked", "litesvm"];

#[derive(Debug, Parser)]
#[command(
    name = "webhook-reconcile",
    about = "Find Helius webhook events that were never delivered"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Print signatures the reference has and the webhook never received, plus duplicates.
    #[command(group(ArgGroup::new("reference_source").required(true).args(["reference", "reference_pages", "live_reference"])))]
    Diff {
        /// Saved webhook payloads, or a list of signatures.
        #[arg(long)]
        received: PathBuf,
        /// Saved getTransactionsForAddress response.
        #[arg(long)]
        reference: Option<PathBuf>,
        /// Recorded getTransactionsForAddress pages, replayed through the paging loop (offline, no key).
        #[arg(long)]
        reference_pages: Option<PathBuf>,
        /// Page through getTransactionsForAddress for --address on Helius. Reads HELIUS_API_KEY from the environment.
        #[arg(long, requires = "address")]
        live_reference: bool,
        /// Address whose history is read.
        #[arg(long, conflicts_with = "reference")]
        address: Option<String>,
        /// First slot of the outage window.
        #[arg(long, conflicts_with = "reference")]
        slot_gte: Option<u64>,
        /// Last slot of the outage window.
        #[arg(long, conflicts_with = "reference")]
        slot_lte: Option<u64>,
        /// First block time of the outage window, Unix seconds.
        #[arg(long, conflicts_with = "reference")]
        time_gte: Option<u64>,
        /// Last block time of the outage window, Unix seconds.
        #[arg(long, conflicts_with = "reference")]
        time_lte: Option<u64>,
        /// Rows per page, 1 to 1000.
        #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u32).range(1..=1000))]
        page_limit: u32,
        /// Stop with an error after this many pages if the server still has more.
        #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u64).range(1..))]
        max_pages: u64,
        /// Which reference rows can count as missing. Enhanced webhooks never send failed
        /// transactions, so the default leaves them out.
        #[arg(long, value_enum, default_value_t = Status::Succeeded)]
        status: Status,
        #[arg(long)]
        json: bool,
        /// The operator asserts where the received file came from, for a file with no `tag` of its
        /// own. Pass `run live` only when the file is the customer's real endpoint log.
        /// A file whose own tag differs is an error.
        #[arg(long, value_parser = clap::builder::PossibleValuesParser::new(KNOWN_TAGS))]
        received_tag: Option<String>,
        /// Same for a saved reference file (`--reference` or `--reference-pages`).
        #[arg(long, conflicts_with = "live_reference", value_parser = clap::builder::PossibleValuesParser::new(KNOWN_TAGS))]
        reference_tag: Option<String>,
        /// Anchor IDL. Decode the `Program data:` events of the missing transactions.
        #[arg(long)]
        idl: Option<PathBuf>,
        /// Replay recorded getTransaction results instead of calling Helius (offline, no key).
        #[arg(long, conflicts_with = "live")]
        refetch_from: Option<PathBuf>,
        /// Refetch the missing signatures from Helius. Reads HELIUS_API_KEY from the environment.
        #[arg(long)]
        live: bool,
        /// Write the missing transactions as an array of raw webhook payloads, to a file or to `-` for
        /// stdout (the report then goes to stderr). Needs --refetch-from or --live.
        #[arg(long)]
        backfill_out: Option<PathBuf>,
    },
}

fn load(p: &PathBuf) -> Result<Value> {
    serde_json::from_str(&fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

/// The outage window the four window flags name.
fn window_of(cmd: &Cmd) -> Window {
    let Cmd::Diff {
        slot_gte,
        slot_lte,
        time_gte,
        time_lte,
        ..
    } = cmd;
    Window {
        slot_gte: *slot_gte,
        slot_lte: *slot_lte,
        time_gte: *time_gte,
        time_lte: *time_lte,
    }
}

/// Everything one paged read of the reference set needs. The recorded and the live read start
/// from the same value, so each flag has one path to the request.
#[derive(Debug, PartialEq, Eq)]
struct Paging {
    address: String,
    window: Window,
    limit: u32,
    max_pages: usize,
}

fn paging_of(cmd: &Cmd) -> Paging {
    let Cmd::Diff {
        address,
        page_limit,
        max_pages,
        ..
    } = cmd;
    Paging {
        address: address.clone().unwrap_or_default(),
        window: window_of(cmd),
        limit: *page_limit,
        max_pages: *max_pages as usize,
    }
}

impl Paging {
    fn read(&self, source: &dyn ReferenceSource) -> Result<Vec<RefEntry>> {
        fetch_reference(
            source,
            &self.address,
            &self.window,
            self.limit,
            self.max_pages,
        )
    }

    fn read_live(&self, source: &dyn ReferenceSource) -> Result<ReferenceSet> {
        Ok(ReferenceSet {
            tag: Some("run live".into()),
            entries: self.read(source)?,
        })
    }
}

/// The tag of a file, after the operator's assertion. A file with no tag takes the asserted one.
/// A file whose own tag differs from the assertion is an error, so a flag cannot relabel data
/// that already says where it came from.
fn asserted_tag(
    file: Option<String>,
    asserted: Option<&str>,
    what: &str,
) -> Result<Option<String>> {
    match (file, asserted) {
        (Some(f), Some(a)) if f != a => {
            bail!("the {what} file is tagged `{f}`, which conflicts with --{what}-tag `{a}`")
        }
        (None, Some(a)) => Ok(Some(a.to_string())),
        (file, _) => Ok(file),
    }
}

fn run() -> Result<bool> {
    let cli = Cli::parse();
    let paging = paging_of(&cli.cmd);
    let Cmd::Diff {
        received,
        reference,
        reference_pages,
        status,
        json,
        received_tag,
        reference_tag,
        idl,
        refetch_from,
        live,
        backfill_out,
        ..
    } = cli.cmd;

    let has_source = refetch_from.is_some() || live;
    if has_source && idl.is_none() && backfill_out.is_none() {
        bail!("--refetch-from and --live need --idl or --backfill-out");
    }
    if backfill_out.is_some() && !has_source {
        bail!("--backfill-out needs --refetch-from or --live");
    }
    let reference = match (&reference, &reference_pages) {
        (Some(p), _) => {
            let mut set = parse_reference(&load(p)?)?;
            set.tag = asserted_tag(set.tag, reference_tag.as_deref(), "reference")?;
            set
        }
        (None, Some(p)) => {
            let file = load(p)?;
            ReferenceSet {
                tag: asserted_tag(
                    file.get("tag").and_then(Value::as_str).map(String::from),
                    reference_tag.as_deref(),
                    "reference",
                )?,
                entries: paging.read(&RecordedPages::from_json(&file)?)?,
            }
        }
        (None, None) => paging.read_live(&HeliusRpc::from_env()?)?,
    };
    let mut received = parse_received(&load(&received)?)?;
    received.tag = asserted_tag(received.tag, received_tag.as_deref(), "received")?;
    let report = diff(&reference, &received, status);

    let source: Option<Box<dyn TxSource>> = match (&refetch_from, live) {
        (Some(f), _) => Some(Box::new(RecordedSource::from_json(&load(f)?)?)),
        (None, true) => Some(Box::new(HeliusRpc::from_env()?)),
        (None, false) => None,
    };
    let (missing, payloads) = match (&source, &backfill_out) {
        (Some(s), Some(_)) => {
            let raws = raw_for_missing(&report.missing, s.as_ref())?;
            (
                with_logs_from_raw(&report.missing, &raws)?,
                Some(webhook_payload(&raws)?),
            )
        }
        (Some(s), None) => (refetch_missing(&report.missing, s.as_ref())?, None),
        (None, _) => (report.missing.clone(), None),
    };
    let decoded = match &idl {
        Some(p) => Some(decode_missing(&Idl::parse(&load(p)?)?, &missing)?),
        None => None,
    };

    let to_stdout = backfill_out
        .as_deref()
        .is_some_and(|p| p.as_os_str() == "-");
    let mut out: Box<dyn Write> = if to_stdout {
        Box::new(std::io::stderr())
    } else {
        Box::new(std::io::stdout())
    };
    if let (Some(path), Some(payloads)) = (&backfill_out, &payloads) {
        let text = serde_json::to_string_pretty(payloads)?;
        if to_stdout {
            writeln!(std::io::stdout().lock(), "{text}")?;
        } else {
            fs::write(path, text + "\n").with_context(|| format!("write {}", path.display()))?;
        }
    }

    if json {
        let report_json = json!({
            "status": report.status,
            "reference_tag": reference.tag, "received_tag": received.tag,
            "reference_count": reference.entries.len(), "received_count": received.signatures.len(),
            "missing": report.missing,
            "skipped_by_status": report.skipped_by_status,
            "duplicates": report.duplicates.iter().map(|(s, n)| json!({"signature": s, "times": n})).collect::<Vec<_>>(),
            "extra": report.extra,
            "decoded": decoded,
            "backfill_count": payloads.as_ref().map(Vec::len),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&report_json)?)?;
    } else {
        writeln!(
            out,
            "reference {} ({}), received {} ({}), status {}",
            reference.entries.len(),
            reference.tag.as_deref().unwrap_or("untagged"),
            received.signatures.len(),
            received.tag.as_deref().unwrap_or("untagged"),
            serde_json::to_value(report.status)?.as_str().unwrap_or("?"),
        )?;
        writeln!(out, "missing: {}", report.missing.len())?;
        for m in &report.missing {
            writeln!(
                out,
                "  {} slot={} failed={}",
                m.signature.0,
                m.slot.map_or("?".into(), |s| s.to_string()),
                m.failed
            )?;
        }
        if !report.skipped_by_status.is_empty() {
            writeln!(
                out,
                "not counted by --status: {} (never received; rerun with --status any to list them)",
                report.skipped_by_status.len()
            )?;
        }
        writeln!(out, "duplicates: {}", report.duplicates.len())?;
        for (s, n) in &report.duplicates {
            writeln!(out, "  {} x{}", s.0, n)?;
        }
        writeln!(
            out,
            "extra (received, not in reference): {}",
            report.extra.len()
        )?;
        for s in &report.extra {
            writeln!(out, "  {}", s.0)?;
        }
        if let Some(events) = &decoded {
            writeln!(out, "decoded events from missing txs: {}", events.len())?;
            for e in events {
                writeln!(
                    out,
                    "  {} {} {}",
                    e.signature.0,
                    e.event,
                    serde_json::to_string(&e.fields)?
                )?;
            }
        }
        if let (Some(path), Some(payloads)) = (&backfill_out, &payloads) {
            writeln!(
                out,
                "backfill: {} webhook payloads written to {}",
                payloads.len(),
                path.display()
            )?;
        }
    }
    Ok(!report.missing.is_empty())
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::from(1),
        Ok(false) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use serde_json::json;
    use webhook_reconcile::PageToken;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(
            ["webhook-reconcile", "diff", "--received", "r"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    fn kind(args: &[&str]) -> Option<ErrorKind> {
        parse(args).err().map(|e| e.kind())
    }

    const WINDOW_FLAGS: [&str; 8] = [
        "--slot-gte",
        "1",
        "--slot-lte",
        "2",
        "--time-gte",
        "3",
        "--time-lte",
        "4",
    ];

    #[test]
    fn every_window_flag_reaches_the_window() {
        let mut args = vec!["--reference-pages", "p"];
        args.extend(WINDOW_FLAGS);
        let cli = parse(&args).unwrap();
        assert_eq!(
            window_of(&cli.cmd),
            Window {
                slot_gte: Some(1),
                slot_lte: Some(2),
                time_gte: Some(3),
                time_lte: Some(4),
            }
        );
    }

    #[test]
    fn every_paging_flag_reaches_the_page_reader() {
        let mut args = vec![
            "--reference-pages",
            "p",
            "--address",
            "addr",
            "--page-limit",
            "7",
            "--max-pages",
            "3",
        ];
        args.extend(WINDOW_FLAGS);
        let cli = parse(&args).unwrap();
        assert_eq!(
            paging_of(&cli.cmd),
            Paging {
                address: "addr".into(),
                window: Window {
                    slot_gte: Some(1),
                    slot_lte: Some(2),
                    time_gte: Some(3),
                    time_lte: Some(4),
                },
                limit: 7,
                max_pages: 3,
            }
        );
    }

    #[test]
    fn the_defaults_read_the_whole_window_with_a_wide_page_and_a_cap() {
        let cli = parse(&["--reference-pages", "p"]).unwrap();
        assert_eq!(
            paging_of(&cli.cmd),
            Paging {
                address: String::new(),
                window: Window::default(),
                limit: 1000,
                max_pages: 50,
            }
        );
        let Cmd::Diff { status, .. } = cli.cmd;
        assert_eq!(status, Status::Succeeded);
    }

    #[test]
    fn the_tag_flags_reach_the_diff_arguments() {
        let Cmd::Diff {
            received_tag,
            reference_tag,
            ..
        } = parse(&[
            "--reference",
            "r",
            "--received-tag",
            "run live",
            "--reference-tag",
            "mocked",
        ])
        .unwrap()
        .cmd;
        assert_eq!(received_tag.as_deref(), Some("run live"));
        assert_eq!(reference_tag.as_deref(), Some("mocked"));
        let Cmd::Diff { received_tag, .. } = parse(&["--reference", "r"]).unwrap().cmd;
        assert_eq!(received_tag, None);
        assert_eq!(
            kind(&["--reference", "r", "--received-tag", "real"]),
            Some(ErrorKind::InvalidValue)
        );
        assert_eq!(
            kind(&[
                "--live-reference",
                "--address",
                "a",
                "--reference-tag",
                "run live"
            ]),
            Some(ErrorKind::ArgumentConflict)
        );
    }

    #[test]
    fn an_asserted_tag_fills_a_missing_tag_and_never_overrides_one() {
        let some = |t: &str| Some(t.to_string());
        assert_eq!(asserted_tag(None, None, "x").unwrap(), None);
        assert_eq!(
            asserted_tag(None, Some("run live"), "x").unwrap(),
            some("run live")
        );
        assert_eq!(
            asserted_tag(some("mocked"), None, "x").unwrap(),
            some("mocked")
        );
        assert_eq!(
            asserted_tag(some("run live"), Some("run live"), "x").unwrap(),
            some("run live")
        );
        let err = asserted_tag(some("mocked"), Some("run live"), "received").unwrap_err();
        assert!(
            err.to_string().contains("conflicts with --received-tag"),
            "{err}"
        );
    }

    #[test]
    fn the_status_flag_reaches_the_diff_arguments() {
        for (flag, want) in [
            ("succeeded", Status::Succeeded),
            ("failed", Status::Failed),
            ("any", Status::Any),
        ] {
            let Cmd::Diff { status, .. } =
                parse(&["--reference", "r", "--status", flag]).unwrap().cmd;
            assert_eq!(status, want);
        }
    }

    #[test]
    fn page_limit_and_max_pages_keep_their_ranges() {
        let with = |flag: &str, value: &str| parse(&["--reference-pages", "p", flag, value]);
        assert!(with("--page-limit", "1").is_ok());
        assert!(with("--page-limit", "1000").is_ok());
        assert!(with("--page-limit", "0").is_err());
        assert!(with("--page-limit", "1001").is_err());
        assert!(with("--max-pages", "1").is_ok());
        assert!(with("--max-pages", "100000").is_ok());
        assert!(with("--max-pages", "0").is_err());
    }

    #[test]
    fn exactly_one_reference_source_is_required() {
        assert_eq!(kind(&[]), Some(ErrorKind::MissingRequiredArgument));
        assert_eq!(kind(&["--reference", "r"]), None);
        assert_eq!(kind(&["--reference-pages", "p"]), None);
        assert_eq!(kind(&["--live-reference", "--address", "a"]), None);
        assert_eq!(
            kind(&["--reference", "r", "--reference-pages", "p"]),
            Some(ErrorKind::ArgumentConflict)
        );
        assert_eq!(
            kind(&[
                "--reference-pages",
                "p",
                "--live-reference",
                "--address",
                "a"
            ]),
            Some(ErrorKind::ArgumentConflict)
        );
        assert_eq!(
            kind(&["--live-reference"]),
            Some(ErrorKind::MissingRequiredArgument)
        );
    }

    #[test]
    fn window_and_address_flags_conflict_with_a_saved_reference_only() {
        for flag in [
            ["--address", "a"],
            ["--slot-gte", "1"],
            ["--slot-lte", "2"],
            ["--time-gte", "3"],
            ["--time-lte", "4"],
        ] {
            let mut saved = vec!["--reference", "r"];
            saved.extend(flag);
            assert_eq!(kind(&saved), Some(ErrorKind::ArgumentConflict), "{flag:?}");
            let mut paged = vec!["--reference-pages", "p"];
            paged.extend(flag);
            assert_eq!(kind(&paged), None, "{flag:?}");
        }
    }

    fn recorded_two_pages() -> RecordedPages {
        RecordedPages::from_json(&json!({"pages": [
            {"after": null, "result": {"data": [{"signature": "a", "slot": 1, "err": null}],
                                       "paginationToken": "1:0"}},
            {"after": "1:0", "result": {"data": [{"signature": "b", "slot": 2, "err": null}]}},
        ]}))
        .unwrap()
    }

    #[test]
    fn the_page_reader_sends_the_address_window_and_limit_on_every_page() {
        let mut args = vec![
            "--reference-pages",
            "p",
            "--address",
            "addr",
            "--page-limit",
            "7",
        ];
        args.extend(WINDOW_FLAGS);
        let paging = paging_of(&parse(&args).unwrap().cmd);
        let source = recorded_two_pages();
        assert_eq!(paging.read(&source).unwrap().len(), 2);
        let requests = source.requests();
        assert_eq!(requests.len(), 2);
        for req in &requests {
            assert_eq!(req.address, "addr");
            assert_eq!(req.limit, 7);
            assert_eq!(req.window, paging.window);
        }
        assert_eq!(requests[1].token, Some(PageToken::new("1:0")));
    }

    #[test]
    fn the_page_reader_stops_at_max_pages() {
        let cli = parse(&["--reference-pages", "p", "--max-pages", "1"]).unwrap();
        let err = paging_of(&cli.cmd).read(&recorded_two_pages()).unwrap_err();
        assert!(err.to_string().contains("stopped after 1 pages"), "{err}");
    }

    #[test]
    fn a_live_read_is_tagged_run_live() {
        let cli = parse(&["--reference-pages", "p"]).unwrap();
        let set = paging_of(&cli.cmd)
            .read_live(&recorded_two_pages())
            .unwrap();
        assert_eq!(set.tag.as_deref(), Some("run live"));
        assert_eq!(set.entries.len(), 2);
    }
}

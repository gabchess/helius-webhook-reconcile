//! webhook-reconcile diff --received <json> (--reference <json> | --reference-pages <json> | --live-reference --address <pubkey>)
//!   [--slot-gte N --slot-lte N --time-gte N --time-lte N --page-limit N --max-pages N]
//!   [--status succeeded|failed|any] [--json]
//!   [--idl <json>] [--refetch-from <json> | --live] [--backfill-out <path|->]
use anyhow::{bail, Context, Result};
use clap::{ArgGroup, Parser, Subcommand};
use serde_json::{json, Value};
use std::io::Write;
use std::{fs, path::PathBuf, process::ExitCode};
use webhook_reconcile::types::{ReferenceSet, Status};
use webhook_reconcile::{
    decode_missing, diff, fetch_reference, parse_received, parse_reference, raw_for_missing,
    refetch_missing, webhook_payload, with_logs_from_raw, HeliusRpc, Idl, RecordedPages,
    RecordedSource, TxSource, Window,
};

#[derive(Parser)]
#[command(
    name = "webhook-reconcile",
    about = "Find Helius webhook events that were never delivered"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
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

fn run() -> Result<bool> {
    let Cmd::Diff {
        received,
        reference,
        reference_pages,
        live_reference: _,
        address,
        slot_gte,
        slot_lte,
        time_gte,
        time_lte,
        page_limit,
        max_pages,
        status,
        json,
        idl,
        refetch_from,
        live,
        backfill_out,
    } = Cli::parse().cmd;

    let has_source = refetch_from.is_some() || live;
    if has_source && idl.is_none() && backfill_out.is_none() {
        bail!("--refetch-from and --live need --idl or --backfill-out");
    }
    if backfill_out.is_some() && !has_source {
        bail!("--backfill-out needs --refetch-from or --live");
    }
    let window = Window {
        slot_gte,
        slot_lte,
        time_gte,
        time_lte,
    };
    let reference = match (&reference, &reference_pages) {
        (Some(p), _) => parse_reference(&load(p)?)?,
        (None, Some(p)) => {
            let file = load(p)?;
            let pages = RecordedPages::from_json(&file)?;
            let entries = fetch_reference(
                &pages,
                address.as_deref().unwrap_or(""),
                &window,
                page_limit,
                max_pages as usize,
            )?;
            ReferenceSet {
                tag: file.get("tag").and_then(Value::as_str).map(String::from),
                entries,
            }
        }
        (None, None) => {
            let entries = fetch_reference(
                &HeliusRpc::from_env()?,
                address.as_deref().unwrap_or(""),
                &window,
                page_limit,
                max_pages as usize,
            )?;
            ReferenceSet {
                tag: Some("run live".into()),
                entries,
            }
        }
    };
    let received = parse_received(&load(&received)?)?;
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
            println!("{text}");
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

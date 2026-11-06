//! webhook-reconcile diff --received <json> --reference <json> [--status succeeded|failed|any] [--json]
use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::ExitCode};
use webhook_reconcile::{diff, parse_received, parse_reference, types::Status};

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
    Diff {
        /// Saved webhook payloads, or a list of signatures.
        #[arg(long)]
        received: PathBuf,
        /// Saved getTransactionsForAddress response.
        #[arg(long)]
        reference: PathBuf,
        /// Which reference rows can count as missing. Enhanced webhooks never send failed
        /// transactions, so the default leaves them out.
        #[arg(long, value_enum, default_value_t = Status::Succeeded)]
        status: Status,
        #[arg(long)]
        json: bool,
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
        status,
        json,
    } = Cli::parse().cmd;
    let reference = parse_reference(&load(&reference)?)?;
    let received = parse_received(&load(&received)?)?;
    let report = diff(&reference, &received, status);

    if json {
        let out = json!({
            "status": report.status,
            "reference_tag": reference.tag, "received_tag": received.tag,
            "reference_count": reference.entries.len(), "received_count": received.signatures.len(),
            "missing": report.missing,
            "skipped_by_status": report.skipped_by_status,
            "duplicates": report.duplicates.iter().map(|(s, n)| json!({"signature": s, "times": n})).collect::<Vec<_>>(),
            "extra": report.extra,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
    } else {
        println!(
            "reference {} ({}), received {} ({}), status {}",
            reference.entries.len(),
            reference.tag.as_deref().unwrap_or("untagged"),
            received.signatures.len(),
            received.tag.as_deref().unwrap_or("untagged"),
            serde_json::to_value(report.status)?.as_str().unwrap_or("?"),
        );
        println!("missing: {}", report.missing.len());
        for m in &report.missing {
            println!(
                "  {} slot={} failed={}",
                m.signature.0,
                m.slot.map_or("?".into(), |s| s.to_string()),
                m.failed
            );
        }
        if !report.skipped_by_status.is_empty() {
            println!(
                "not counted by --status: {} (never received; rerun with --status any to list them)",
                report.skipped_by_status.len()
            );
        }
        println!("duplicates: {}", report.duplicates.len());
        for (s, n) in &report.duplicates {
            println!("  {} x{}", s.0, n);
        }
        println!("extra (received, not in reference): {}", report.extra.len());
        for s in &report.extra {
            println!("  {}", s.0);
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

//! helius-triage --ticket <text file> --report <diff --json file> [--ai [--jev-from <fixture>]]
//! Prints a drafted support reply. A person reviews it before it is sent.
//! Exit 0: a reply was drafted. Exit 1: a person must read the ticket. Exit 2: bad input.
use anyhow::{Context, Result};
use clap::Parser;
use serde_json::Value;
use std::{fs, path::PathBuf, process::ExitCode};
use webhook_reconcile::triage::{
    select_classifier, triage, Classifier, Draft, JevClassifier, JevHttp, RecordedJev, Ticket,
    TriageReport,
};

#[derive(Debug, Parser)]
#[command(
    name = "helius-triage",
    about = "Draft a reply to a \"my webhook stopped\" ticket"
)]
struct Cli {
    /// The customer's ticket as a text file.
    #[arg(long)]
    ticket: PathBuf,
    /// The output of `webhook-reconcile diff --json`.
    #[arg(long)]
    report: PathBuf,
    /// Classify the ticket with the TypeSafe Jev model. Off by default: the keyword rules run and no model is called.
    #[arg(long)]
    ai: bool,
    /// Replay a hand-built mocked Jev fixture instead of calling the API (offline, no key).
    #[arg(long, requires = "ai")]
    jev_from: Option<PathBuf>,
}

fn load(p: &PathBuf) -> Result<Value> {
    serde_json::from_str(&fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

fn run() -> Result<bool> {
    let cli = Cli::parse();
    let ticket = Ticket {
        text: fs::read_to_string(&cli.ticket)
            .with_context(|| format!("read {}", cli.ticket.display()))?,
    };
    let report = TriageReport::from_json(&load(&cli.report)?)?;
    let jev_from = cli.jev_from;
    let classifier = select_classifier(cli.ai, || {
        let classifier: Box<dyn Classifier> = match &jev_from {
            Some(p) => Box::new(JevClassifier::new(RecordedJev::from_json(&load(p)?)?)),
            None => Box::new(JevClassifier::new(JevHttp::from_env()?)),
        };
        Ok(classifier)
    })?;
    match triage(&ticket, &report, classifier.as_ref())? {
        Draft::Reply(text) => {
            print!("{text}");
            Ok(true)
        }
        Draft::NeedsHuman(reason) => {
            println!("NEEDS HUMAN: {reason}");
            Ok(false)
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

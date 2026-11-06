//! All offline, no key. Tags: gtfa-live.json is "run live" data recorded earlier, the *-received.json
//! files are "mocked", event-lab-reference.json is "litesvm".
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use webhook_reconcile::{diff, parse_received, parse_reference, types::Status};

fn fx(name: &str) -> Value {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name);
    serde_json::from_str(&fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap()
}

fn sigs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect()
}

fn names(entries: &[webhook_reconcile::types::RefEntry]) -> Vec<String> {
    entries.iter().map(|m| m.signature.0.clone()).collect()
}

#[test]
fn failed_tx_is_not_missing_by_default() {
    let reference = parse_reference(&json!({"result": {"data": [
        {"signature": "ok1", "slot": 1, "err": null},
        {"signature": "bad", "slot": 2, "err": {"InstructionError": [0, "Custom"]}},
        {"signature": "ok2", "slot": 3, "err": null},
    ]}}))
    .unwrap();
    let received = parse_received(&json!(["ok1"])).unwrap();

    let default = diff(&reference, &received, Status::default());
    assert_eq!(names(&default.missing), ["ok2"]);
    assert_eq!(names(&default.skipped_by_status), ["bad"]);

    let any = diff(&reference, &received, Status::Any);
    assert_eq!(names(&any.missing), ["bad", "ok2"]);
    assert!(any.skipped_by_status.is_empty());

    let failed = diff(&reference, &received, Status::Failed);
    assert_eq!(names(&failed.missing), ["bad"]);
    assert_eq!(names(&failed.skipped_by_status), ["ok2"]);
}

#[test]
fn live_reference_vs_mocked_received() {
    let reference = parse_reference(&fx("gtfa-live.json")).unwrap();
    let received = parse_received(&fx("gtfa-live-received.json")).unwrap();
    assert_eq!(reference.tag.as_deref(), Some("run live"));
    assert_eq!(received.tag.as_deref(), Some("mocked"));
    assert_eq!(reference.entries.len(), 100);
    let exp = fx("gtfa-live-expected.json");

    let any = diff(&reference, &received, Status::Any);
    assert_eq!(names(&any.missing), sigs(&exp["missing_any"]));
    let dups: Vec<String> = any
        .duplicates
        .iter()
        .map(|(s, n)| {
            assert_eq!(*n, 2);
            s.0.clone()
        })
        .collect();
    assert_eq!(dups, sigs(&exp["duplicates"]));
    assert!(any.extra.is_empty());

    let default = diff(&reference, &received, Status::default());
    assert_eq!(names(&default.missing), sigs(&exp["missing_succeeded"]));
    assert_eq!(
        names(&default.skipped_by_status),
        sigs(&exp["skipped_failed"])
    );
}

#[test]
fn live_reference_contains_failed_txs() {
    // gTFA returns failed txs too, so the parsed reference must carry `failed`.
    let reference = parse_reference(&fx("gtfa-live.json")).unwrap();
    assert!(
        reference.entries.iter().any(|e| e.failed),
        "expected at least one failed tx in the live window"
    );
}

#[test]
fn full_mode_reference_reads_signature_and_err_from_transaction_and_meta() {
    let reference = parse_reference(&fx("event-lab-reference.json")).unwrap();
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    assert_eq!(reference.tag.as_deref(), Some("litesvm"));
    let report = diff(&reference, &received, Status::Any);
    assert_eq!(report.missing.len(), 3);
    assert_eq!(report.duplicates.len(), 2);
    assert_eq!(report.extra.len(), 1);
}

#[test]
fn failed_tx_that_was_received_is_neither_missing_nor_extra() {
    let reference = parse_reference(&json!([
        {"signature": "bad", "err": {"InstructionError": [0, "Custom"]}}
    ]))
    .unwrap();
    let received = parse_received(&json!(["bad"])).unwrap();
    let report = diff(&reference, &received, Status::Succeeded);
    assert!(report.missing.is_empty());
    assert!(report.skipped_by_status.is_empty());
    assert!(report.extra.is_empty());
}

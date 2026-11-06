//! All offline, no key. Tags: gtfa-live.json is "run live" data recorded earlier, the *-received.json
//! files are "mocked", the other event-lab-*.json files and the IDL come from the LiteSVM run ("litesvm").
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use webhook_reconcile::decode::event_discriminator;
use webhook_reconcile::refetch::{parse_batch_response, TxSource};
use webhook_reconcile::types::{RefEntry, Signature};
use webhook_reconcile::{
    decode_missing, diff, parse_received, parse_reference, program_data_lines, refetch_missing,
    types::Status, DecodeError, HeliusRpc, Idl, RecordedSource,
};

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
fn full_mode_reference_reads_signature_from_transaction() {
    let reference = parse_reference(&fx("event-lab-reference.json")).unwrap();
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    assert_eq!(reference.tag.as_deref(), Some("litesvm"));
    let report = diff(&reference, &received, Status::Any);
    assert_eq!(report.missing.len(), 3);
    assert_eq!(report.duplicates.len(), 2);
    assert_eq!(report.extra.len(), 1);
}

#[test]
fn full_mode_failed_tx_is_read_from_meta_err() {
    let reference = parse_reference(&json!([
        {"transaction": {"signatures": ["bad"]}, "meta": {"err": {"InstructionError": [0, "Custom"]}}},
        {"transaction": {"signatures": ["ok"]}, "meta": {"err": null}}
    ]))
    .unwrap();
    let received = parse_received(&json!([])).unwrap();

    let default = diff(&reference, &received, Status::default());
    assert_eq!(names(&default.missing), vec!["ok"]);
    assert_eq!(names(&default.skipped_by_status), vec!["bad"]);

    let any = diff(&reference, &received, Status::Any);
    assert_eq!(names(&any.missing), vec!["bad", "ok"]);
    assert!(any.skipped_by_status.is_empty());
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

const PROGRAM: &str = "9LQwY9ThwtXFhpgMZ1MmRBfA5wc9YUSTUGDcmoKcKTZ6";

fn idl() -> Idl {
    Idl::parse(&fx("event_lab.idl.json")).unwrap()
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn sig(s: &str) -> Signature {
    Signature(s.to_string())
}

fn entry_with_logs(logs: Vec<String>) -> RefEntry {
    RefEntry {
        signature: sig("x"),
        slot: None,
        failed: false,
        logs: Some(logs),
    }
}

/// The `Program data:` payload of the recorded event-lab transaction at `index`.
fn recorded_payload(index: usize) -> String {
    fx("event-lab-reference.json")["result"]["data"][index]["meta"]["logMessages"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|l| l.as_str().unwrap().strip_prefix("Program data: "))
        .unwrap()
        .to_string()
}

fn base64_of(b: &[u8]) -> String {
    STANDARD.encode(b)
}

fn base64_decode(s: &str) -> Vec<u8> {
    STANDARD.decode(s).unwrap()
}

#[test]
fn event_lab_decodes_the_dropped_events() {
    let reference = parse_reference(&fx("event-lab-reference.json")).unwrap();
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    let exp = fx("event-lab-expected.json");

    let report = diff(&reference, &received, Status::Any);
    assert_eq!(report.missing.len(), 3);
    assert_eq!(report.duplicates.len(), 2);
    assert_eq!(report.extra.len(), 1);

    let events = decode_missing(&idl(), &report.missing).unwrap();
    assert_eq!(events.len(), 3);
    for (ev, want) in events.iter().zip(exp["missing"].as_array().unwrap()) {
        assert_eq!(ev.signature.0, want["signature"].as_str().unwrap());
        assert_eq!(ev.event, "TransferRecorded");
        assert_eq!(ev.fields["id"], want["id"]);
        assert_eq!(ev.fields["amount"], want["amount"]);
        assert_eq!(ev.fields["who"], want["who"]);
        assert_eq!(ev.fields["memo"], want["memo"]);
    }
}

#[test]
fn signatures_mode_reference_cannot_be_decoded() {
    let reference = parse_reference(&fx("gtfa-live.json")).unwrap();
    let received = parse_received(&fx("gtfa-live-received.json")).unwrap();
    let report = diff(&reference, &received, Status::Any);
    let err = decode_missing(&idl(), &report.missing).unwrap_err();
    assert!(
        err.to_string().contains("signatures mode"),
        "unclear error: {err}"
    );
    match err.downcast_ref::<DecodeError>() {
        Some(DecodeError::NoLogs(sigs)) => assert_eq!(sigs.len(), 5),
        other => panic!("expected NoLogs, got {other:?}"),
    }
}

#[test]
fn signatures_mode_diff_equals_full_mode_diff() {
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    let full = parse_reference(&fx("event-lab-reference.json")).unwrap();
    let sigs_only = parse_reference(&fx("event-lab-signatures.json")).unwrap();
    assert!(sigs_only.entries.iter().all(|e| e.logs.is_none()));
    assert!(full.entries.iter().all(|e| e.logs.is_some()));
    let a = diff(&full, &received, Status::Any);
    let b = diff(&sigs_only, &received, Status::Any);
    assert_eq!(names(&a.missing), names(&b.missing));
    assert_eq!(a.duplicates, b.duplicates);
    assert_eq!(a.extra, b.extra);
}

#[test]
fn refetch_asks_only_for_the_missing_signatures() {
    let reference = parse_reference(&fx("event-lab-signatures.json")).unwrap();
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    let report = diff(&reference, &received, Status::Any);
    assert_eq!(report.missing.len(), 3);
    let source = RecordedSource::from_json(&fx("event-lab-refetch.json")).unwrap();

    let filled = refetch_missing(&report.missing, &source).unwrap();

    let calls = source.calls();
    assert_eq!(calls.len(), 1, "one batched call");
    let asked: Vec<String> = calls[0].iter().map(|s| s.0.clone()).collect();
    let exp = fx("event-lab-expected.json");
    let want: Vec<String> = exp["missing"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["signature"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(asked, want);
    assert!(filled.iter().all(|e| e.logs.is_some()));

    let events = decode_missing(&idl(), &filled).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].fields["memo"], "order-2");
}

#[test]
fn refetch_makes_no_call_when_logs_are_already_there() {
    let reference = parse_reference(&fx("event-lab-reference.json")).unwrap();
    let received = parse_received(&fx("event-lab-received.json")).unwrap();
    let report = diff(&reference, &received, Status::Any);
    let source = RecordedSource::from_json(&fx("event-lab-refetch.json")).unwrap();
    let filled = refetch_missing(&report.missing, &source).unwrap();
    assert!(source.calls().is_empty());
    assert_eq!(filled, report.missing);
}

#[test]
fn refetch_fails_when_the_source_does_not_have_a_signature() {
    let source = RecordedSource::from_json(&json!({"responses": {}})).unwrap();
    let missing = vec![RefEntry {
        signature: sig("gone"),
        slot: Some(1),
        failed: false,
        logs: None,
    }];
    let err = refetch_missing(&missing, &source).unwrap_err();
    assert!(err.to_string().contains("gone"), "{err}");
}

#[test]
fn batch_response_errors_are_loud() {
    let requested = [sig("a")];
    let rpc_error = json!([{"id": "0", "error": {"code": -32000, "message": "boom"}}]);
    assert!(parse_batch_response(&requested, &rpc_error)
        .unwrap_err()
        .to_string()
        .contains("boom"));
    let not_found = json!([{"id": "0", "result": null}]);
    assert!(parse_batch_response(&requested, &not_found)
        .unwrap_err()
        .to_string()
        .contains("not found"));
    let wrong_tx = json!([{"id": "0", "result": {"transaction": {"signatures": ["b"]}, "meta": {"logMessages": []}}}]);
    assert!(parse_batch_response(&requested, &wrong_tx).is_err());
    let no_logs =
        json!([{"id": "0", "result": {"transaction": {"signatures": ["a"]}, "meta": {}}}]);
    assert!(parse_batch_response(&requested, &no_logs).is_err());
    assert!(parse_batch_response(&requested, &json!([])).is_err());
}

#[test]
fn helius_rpc_never_shows_its_key() {
    let rpc = HeliusRpc::new("https://example.invalid", "SECRET-KEY-123");
    assert!(!format!("{rpc:?}").contains("SECRET-KEY-123"));
}

#[test]
fn transport_error_text_never_contains_the_key() {
    let rpc = HeliusRpc::new("http://127.0.0.1:1", "SECRET-KEY-123");
    let err = rpc.fetch_full(&[sig("a")]).unwrap_err();
    let text = format!("{err:#} {err:?}");
    assert!(!text.contains("SECRET-KEY-123"), "key leaked: {text}");
    assert!(!text.contains("api-key"), "key parameter leaked: {text}");
}

struct AnswersWith(Vec<RefEntry>);

impl TxSource for AnswersWith {
    fn fetch_full(&self, _: &[Signature]) -> anyhow::Result<Vec<RefEntry>> {
        Ok(self.0.clone())
    }
}

#[test]
fn refetch_rejects_a_signature_nobody_asked_for() {
    let missing = vec![RefEntry {
        signature: sig("asked"),
        slot: Some(1),
        failed: false,
        logs: None,
    }];
    let source = AnswersWith(vec![RefEntry {
        signature: sig("stranger"),
        slot: Some(2),
        failed: false,
        logs: Some(vec![]),
    }]);
    let err = refetch_missing(&missing, &source).unwrap_err();
    assert!(err.to_string().contains("stranger"), "{err}");
    assert!(err.to_string().contains("not requested"), "{err}");
}

#[test]
fn data_lines_from_other_programs_are_skipped() {
    let logs = lines(&[
        "Program AAA invoke [1]",
        "Program BBB invoke [2]",
        "Program data: Zm9v",
        "Program BBB success",
        "Program data: YmFy",
        "Program AAA success",
    ]);
    assert_eq!(program_data_lines(&logs, Some("AAA")), vec!["YmFy"]);
    assert_eq!(program_data_lines(&logs, Some("BBB")), vec!["Zm9v"]);
    assert_eq!(program_data_lines(&logs, None).len(), 2);
}

#[test]
fn a_data_line_after_a_failed_call_belongs_to_the_caller_again() {
    let logs = lines(&[
        "Program AAA invoke [1]",
        "Program BBB invoke [2]",
        "Program BBB failed: custom program error: 0x1",
        "Program data: YmFy",
        "Program AAA success",
    ]);
    assert_eq!(program_data_lines(&logs, Some("AAA")), vec!["YmFy"]);
    assert!(program_data_lines(&logs, Some("BBB")).is_empty());
}

#[test]
fn log_text_that_looks_like_an_invoke_does_not_change_the_stack() {
    let logs = lines(&[
        "Program AAA invoke [1]",
        "Program log: BBB invoke [2]",
        "Program data: YmFy",
        "Program AAA success",
    ]);
    assert_eq!(program_data_lines(&logs, Some("AAA")), vec!["YmFy"]);
    let spoof = lines(&[
        "Program AAA invoke [1]",
        "Program log: invoke [2]",
        "Program data: YmFy",
    ]);
    assert_eq!(program_data_lines(&spoof, Some("AAA")), vec!["YmFy"]);
}

#[test]
fn an_event_forged_by_a_called_program_is_not_decoded() {
    let data = format!("Program data: {}", recorded_payload(2));
    let outer = format!("Program {PROGRAM} invoke [1]");
    let logs = lines(&[
        &outer,
        "Program BBB invoke [2]",
        &data,
        "Program BBB success",
    ]);
    let events = decode_missing(&idl(), &[entry_with_logs(logs)]).unwrap();
    assert!(events.is_empty());
}

#[test]
fn an_event_emitted_by_the_program_itself_is_decoded() {
    let data = format!("Program data: {}", recorded_payload(2));
    let outer = format!("Program {PROGRAM} invoke [1]");
    let success = format!("Program {PROGRAM} success");
    let logs = lines(&[&outer, &data, &success]);
    let events = decode_missing(&idl(), &[entry_with_logs(logs)]).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].fields["id"], 1002);
}

#[test]
fn unknown_discriminator_is_not_an_event() {
    let junk = base64_of(&[1, 2, 3, 4, 5, 6, 7, 8, 9]);
    assert!(idl().decode(&sig("x"), &junk).unwrap().is_none());
}

#[test]
fn the_idl_discriminator_is_sha256_of_event_name() {
    let committed = fx("event_lab.idl.json");
    let want: Vec<u8> = committed["events"][0]["discriminator"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b.as_u64().unwrap() as u8)
        .collect();
    assert_eq!(event_discriminator("TransferRecorded").to_vec(), want);

    let mut older = committed.clone();
    older["events"][0]
        .as_object_mut()
        .unwrap()
        .remove("discriminator");
    let payload = base64_decode(&recorded_payload(2));
    let ev = Idl::parse(&older)
        .unwrap()
        .decode(&sig("x"), &base64_of(&payload))
        .unwrap();
    assert_eq!(ev.unwrap().event, "TransferRecorded");
}

#[test]
fn truncated_event_is_an_error_not_a_panic() {
    let mut raw = base64_decode(&recorded_payload(2));
    raw.truncate(raw.len() - 4);
    assert!(idl().decode(&sig("x"), &base64_of(&raw)).is_err());
}

#[test]
fn an_event_with_extra_bytes_is_an_error() {
    let mut raw = base64_decode(&recorded_payload(2));
    raw.extend_from_slice(&[0, 0]);
    let err = idl().decode(&sig("x"), &base64_of(&raw)).unwrap_err();
    assert!(err.to_string().contains("trailing"), "{err}");
}

#[test]
fn truncated_logs_are_a_loud_error() {
    let data = format!("Program data: {}", recorded_payload(2));
    let outer = format!("Program {PROGRAM} invoke [1]");
    let logs = lines(&[&outer, &data, "Log truncated"]);
    let err = decode_missing(&idl(), &[entry_with_logs(logs)]).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<DecodeError>(),
        Some(DecodeError::LogsTruncated(_))
    ));
}

#[test]
fn an_idl_without_an_address_cannot_decode() {
    let mut no_address = fx("event_lab.idl.json");
    no_address.as_object_mut().unwrap().remove("address");
    let idl = Idl::parse(&no_address).unwrap();
    let err = decode_missing(&idl, &[entry_with_logs(vec![])]).unwrap_err();
    assert!(matches!(
        err.downcast_ref::<DecodeError>(),
        Some(DecodeError::IdlWithoutAddress)
    ));
}

#[test]
fn the_stack_realigns_on_the_depth_in_each_invoke_line() {
    let logs = lines(&[
        "Program AAA invoke [1]",
        "Program BBB invoke [2]",
        "Program CCC invoke [1]",
        "Program CCC success",
        "Program data: Wg==",
    ]);
    assert!(program_data_lines(&logs, Some("BBB")).is_empty());
    assert!(program_data_lines(&logs, Some("AAA")).is_empty());
}

#[test]
fn a_discriminator_that_differs_in_the_last_byte_is_not_an_event() {
    let mut raw = base64_decode(&recorded_payload(2));
    raw[7] ^= 0xff;
    assert!(idl().decode(&sig("x"), &base64_of(&raw)).unwrap().is_none());
}

//! All offline, no key. Tags: gtfa-live.json is "run live" data recorded earlier, the *-received.json
//! files are "mocked", the other event-lab-*.json files and the IDL come from the LiteSVM run ("litesvm").
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};
use webhook_reconcile::decode::event_discriminator;
use webhook_reconcile::paging::{page_request_body, parse_page_response};
use webhook_reconcile::refetch::{parse_batch_response, TxSource};
use webhook_reconcile::types::{RefEntry, Signature};
use webhook_reconcile::{
    decode_missing, diff, fetch_reference, parse_received, parse_reference, program_data_lines,
    raw_for_missing, refetch_missing, types::Status, webhook_payload, DecodeError, HeliusRpc, Idl,
    PageRequest, PageToken, PagingError, RecordedPages, RecordedSource, ReferenceSource, RpcError,
    Window,
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
    let err = parse_batch_response(&requested, &rpc_error).unwrap_err();
    assert_eq!(
        err,
        RpcError::JsonRpc {
            code: Some(-32000),
            signature: sig("a")
        }
    );
    assert!(!err.to_string().contains("boom"));
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

fn serve_once(status_line: &'static str, body: &str) -> String {
    let body = body.to_string();
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
        }
        let mut request_body = vec![0u8; length];
        reader.read_exact(&mut request_body).unwrap();
        let reply = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).unwrap();
    });
    format!("http://{addr}")
}

fn assert_key_absent(err: &anyhow::Error) {
    let text = format!("{err:#} {err:?}");
    assert!(!text.contains("SECRET-KEY-123"), "key leaked: {text}");
    assert!(!text.contains("api-key="), "key parameter leaked: {text}");
}

#[test]
fn jsonrpc_error_text_never_contains_the_key() {
    let base = serve_once(
        "200 OK",
        r#"[{"jsonrpc":"2.0","id":"0","error":{"code":-32602,"message":"invalid api-key SECRET-KEY-123"}}]"#,
    );
    let err = HeliusRpc::new(&base, "SECRET-KEY-123")
        .fetch_full(&[sig("a")])
        .unwrap_err();
    assert_key_absent(&err);
}

#[test]
fn status_error_text_never_contains_the_key() {
    let base = serve_once(
        "401 Unauthorized",
        r#"{"error":"invalid api-key SECRET-KEY-123"}"#,
    );
    let err = HeliusRpc::new(&base, "SECRET-KEY-123")
        .fetch_full(&[sig("a")])
        .unwrap_err();
    assert_key_absent(&err);
}

#[test]
fn status_error_text_key_across_the_cut_never_leaks() {
    const KEY: &str = "0123abcd-4567-89ef-0123-456789abcdef";
    let head = r#"{"error":"unauthorized","detail":""#;
    let body = format!("{head}{}{KEY}\"}}", "x".repeat(170 - head.len()));
    assert_eq!(body.find(KEY), Some(170));
    let base = serve_once("401 Unauthorized", &body);
    let err = HeliusRpc::new(&base, KEY)
        .fetch_full(&[sig("a")])
        .unwrap_err();
    let text = format!("{err:#} {err:?}");
    assert!(!text.contains(&KEY[..20]), "key prefix leaked: {text}");
    assert!(!text.contains("api-key="), "key parameter leaked: {text}");
}

#[test]
fn rpc_error_display_is_exact_for_every_variant() {
    let cases = [
        (
            RpcError::Transport {
                kind: ureq::ErrorKind::ConnectionFailed,
            },
            "Helius RPC transport error: ConnectionFailed",
        ),
        (
            RpcError::Status { code: 401 },
            "Helius RPC returned HTTP 401",
        ),
        (
            RpcError::JsonRpc {
                code: Some(-32602),
                signature: sig("a"),
            },
            "getTransaction failed for a with code -32602",
        ),
        (
            RpcError::JsonRpc {
                code: None,
                signature: sig("a"),
            },
            "getTransaction failed for a with no code",
        ),
        (
            RpcError::NotFound {
                signature: sig("a"),
            },
            "transaction a not found",
        ),
        (
            RpcError::BadResult {
                what: "no meta.logMessages",
                signature: sig("a"),
            },
            "result for a: no meta.logMessages",
        ),
        (
            RpcError::Decode {
                what: "body is not valid JSON",
            },
            "Helius RPC response: body is not valid JSON",
        ),
        (
            RpcError::JsonRpcMethod { code: Some(-32602) },
            "getTransactionsForAddress failed with code -32602",
        ),
        (
            RpcError::JsonRpcMethod { code: None },
            "getTransactionsForAddress failed with no code",
        ),
    ];
    for (err, text) in cases {
        assert_eq!(err.to_string(), text);
    }
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

fn pages_fixture() -> RecordedPages {
    RecordedPages::from_json(&fx("event-lab-pages.json")).unwrap()
}

fn window() -> Window {
    Window {
        slot_gte: Some(100),
        slot_lte: Some(109),
        time_gte: Some(1_700_000_000),
        time_lte: None,
    }
}

fn read_pages(source: &dyn ReferenceSource, max_pages: usize) -> anyhow::Result<Vec<RefEntry>> {
    fetch_reference(source, "addr", &window(), 4, max_pages)
}

fn row(signature: &str) -> Value {
    json!({"signature": signature, "slot": 1, "err": null})
}

fn recorded(pages: Value) -> RecordedPages {
    RecordedPages::from_json(&json!({ "pages": pages })).unwrap()
}

#[test]
fn paging_follows_the_token_chain_over_the_whole_window() {
    let source = pages_fixture();
    let entries = read_pages(&source, 10).unwrap();
    let tokens: Vec<Option<String>> = source
        .requests()
        .iter()
        .map(|r| r.token.as_ref().map(|t| t.as_str().to_string()))
        .collect();
    assert_eq!(
        tokens,
        [
            None,
            Some("104:0".to_string()),
            Some("105:0".to_string()),
            Some("108:0".to_string())
        ]
    );
    let all = parse_reference(&fx("event-lab-signatures.json")).unwrap();
    assert_eq!(names(&entries), names(&all.entries));
}

#[test]
fn paging_stops_when_the_token_is_absent() {
    for last in [
        json!({"data": [row("a")]}),
        json!({"data": [row("a")], "paginationToken": null}),
    ] {
        let source = recorded(json!([{"after": null, "result": last}]));
        let entries = read_pages(&source, 10).unwrap();
        assert_eq!(names(&entries), ["a"]);
        assert_eq!(source.requests().len(), 1, "no request after the last page");
    }
}

#[test]
fn paging_keeps_the_window_on_every_page() {
    let source = pages_fixture();
    read_pages(&source, 10).unwrap();
    let requests = source.requests();
    assert_eq!(requests.len(), 4);
    for req in &requests {
        assert_eq!(req.window, window());
        assert_eq!(req.address, "addr");
        assert_eq!(req.limit, 4);
        let body = page_request_body(req);
        let filters = &body["params"][1]["filters"];
        assert_eq!(filters["slot"], json!({"gte": 100, "lte": 109}));
        assert_eq!(filters["blockTime"], json!({"gte": 1_700_000_000u64}));
        assert_eq!(filters["status"], "any");
    }
}

#[test]
fn page_request_body_is_exact() {
    let first = PageRequest {
        address: "addr".into(),
        window: window(),
        limit: 4,
        token: None,
    };
    assert_eq!(
        page_request_body(&first),
        json!({"jsonrpc": "2.0", "id": "1", "method": "getTransactionsForAddress",
               "params": ["addr", {"transactionDetails": "signatures", "sortOrder": "asc",
                                   "commitment": "finalized", "limit": 4,
                                   "filters": {"status": "any",
                                               "slot": {"gte": 100, "lte": 109},
                                               "blockTime": {"gte": 1_700_000_000u64}}}]})
    );
    let next = PageRequest {
        window: Window::default(),
        token: Some(PageToken::new("104:0")),
        ..first
    };
    let body = page_request_body(&next);
    assert_eq!(body["params"][1]["paginationToken"], "104:0");
    assert_eq!(body["params"][1]["filters"], json!({"status": "any"}));
}

#[test]
fn an_empty_page_without_a_token_ends_the_read() {
    let source =
        recorded(json!([{"after": null, "result": {"data": [], "paginationToken": null}}]));
    assert!(read_pages(&source, 10).unwrap().is_empty());
    assert_eq!(source.requests().len(), 1);
}

#[test]
fn an_empty_page_with_a_token_does_not_end_the_read() {
    let source = pages_fixture();
    let entries = read_pages(&source, 10).unwrap();
    assert_eq!(entries.len(), 10, "rows after the empty page 2 were read");
    assert_eq!(source.requests().len(), 4);
}

#[test]
fn a_repeated_token_is_a_loud_error_not_a_loop() {
    let source = recorded(json!([
        {"after": null, "result": {"data": [row("a")], "paginationToken": "9999:7"}},
        {"after": "9999:7", "result": {"data": [row("a")], "paginationToken": "9999:7"}},
    ]));
    let err = read_pages(&source, 1000).unwrap_err();
    assert_eq!(
        err.downcast_ref::<PagingError>(),
        Some(&PagingError::TokenRepeated { page: 2 })
    );
    assert_eq!(source.requests().len(), 2, "stopped at the repeat");
    let text = format!("{err:#} {err:?}");
    assert!(!text.contains("9999:7"), "token leaked: {text}");
}

#[test]
fn a_token_cycle_is_caught_too() {
    let source = recorded(json!([
        {"after": null, "result": {"data": [], "paginationToken": "1:0"}},
        {"after": "1:0", "result": {"data": [], "paginationToken": "2:0"}},
        {"after": "2:0", "result": {"data": [], "paginationToken": "1:0"}},
    ]));
    let err = read_pages(&source, 1000).unwrap_err();
    assert_eq!(
        err.downcast_ref::<PagingError>(),
        Some(&PagingError::TokenRepeated { page: 3 })
    );
    assert_eq!(source.requests().len(), 3);
}

#[test]
fn the_page_cap_stops_a_long_read_with_an_error() {
    let source = pages_fixture();
    let err = read_pages(&source, 2).unwrap_err();
    assert_eq!(
        err.downcast_ref::<PagingError>(),
        Some(&PagingError::PageCapReached { max_pages: 2 })
    );
    assert_eq!(source.requests().len(), 2);
    assert!(err.to_string().contains("--max-pages"), "{err}");
}

#[test]
fn a_cap_equal_to_the_page_count_is_not_an_error() {
    let source = pages_fixture();
    assert_eq!(read_pages(&source, 4).unwrap().len(), 10);
    assert!(read_pages(&pages_fixture(), 3).is_err());
}

#[test]
fn a_page_token_never_shows_in_debug_output() {
    let t = PageToken::new("9999:7");
    assert!(!format!("{t:?}").contains("9999"));
}

#[test]
fn a_signature_repeated_across_pages_is_kept_once() {
    let source = recorded(json!([
        {"after": null, "result": {"data": [row("a"), row("dup")], "paginationToken": "1:0"}},
        {"after": "1:0", "result": {"data": [row("dup"), row("b")], "paginationToken": null}},
    ]));
    let entries = read_pages(&source, 10).unwrap();
    assert_eq!(names(&entries), ["a", "dup", "b"]);

    let received = parse_received(&json!(["a"])).unwrap();
    let reference = webhook_reconcile::types::ReferenceSet { tag: None, entries };
    let report = diff(&reference, &received, Status::Any);
    assert_eq!(names(&report.missing), ["dup", "b"]);

    let full = |s: &str| json!({"slot": 1, "transaction": {"signatures": [s]}, "meta": {"err": null, "logMessages": []}});
    let refetch =
        RecordedSource::from_json(&json!({"responses": {"dup": full("dup"), "b": full("b")}}))
            .unwrap();
    let filled = refetch_missing(&report.missing, &refetch).unwrap();
    assert_eq!(names(&filled), ["dup", "b"]);
}

#[test]
fn a_signature_repeated_in_a_saved_reference_is_kept_once() {
    let reference = parse_reference(&json!([row("dup"), row("dup"), row("b")])).unwrap();
    assert_eq!(names(&reference.entries), ["dup", "b"]);
    let report = diff(
        &reference,
        &parse_received(&json!([])).unwrap(),
        Status::Any,
    );
    assert_eq!(names(&report.missing), ["dup", "b"]);
}

#[test]
fn page_responses_parse_and_fail_without_echoing_the_server() {
    let ok =
        parse_page_response(&json!({"result": {"data": [row("a")], "paginationToken": "104:0"}}))
            .unwrap();
    assert_eq!(ok.rows.len(), 1);
    assert_eq!(ok.next, Some(PageToken::new("104:0")));

    let err = parse_page_response(
        &json!({"error": {"code": -32602, "message": "invalid api-key SECRET-KEY-123"}}),
    )
    .unwrap_err();
    assert_eq!(err, RpcError::JsonRpcMethod { code: Some(-32602) });
    assert!(!err.to_string().contains("SECRET-KEY-123"));

    for bad in [
        json!({}),
        json!({"result": {}}),
        json!({"result": {"data": [], "paginationToken": 5}}),
    ] {
        assert!(
            matches!(parse_page_response(&bad), Err(RpcError::Decode { .. })),
            "{bad}"
        );
    }
}

#[test]
fn live_page_fetch_reads_a_page_and_never_leaks_the_key_or_token() {
    let base = serve_once(
        "200 OK",
        r#"{"jsonrpc":"2.0","id":"1","result":{"data":[{"signature":"a","slot":1,"err":null}],"paginationToken":"104:0"}}"#,
    );
    let req = PageRequest {
        address: "addr".into(),
        window: window(),
        limit: 4,
        token: None,
    };
    let page = HeliusRpc::new(&base, "SECRET-KEY-123")
        .fetch_page(&req)
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.next, Some(PageToken::new("104:0")));

    let base = serve_once(
        "200 OK",
        r#"{"jsonrpc":"2.0","id":"1","error":{"code":-32602,"message":"invalid api-key SECRET-KEY-123 token 9999:7"}}"#,
    );
    let err = HeliusRpc::new(&base, "SECRET-KEY-123")
        .fetch_page(&req)
        .unwrap_err();
    assert_key_absent(&err);
    assert!(!format!("{err:#}").contains("9999:7"));
    assert_eq!(
        err.downcast_ref::<RpcError>(),
        Some(&RpcError::JsonRpcMethod { code: Some(-32602) })
    );

    let err = HeliusRpc::new("http://127.0.0.1:1", "SECRET-KEY-123")
        .fetch_page(&req)
        .unwrap_err();
    assert_key_absent(&err);
}

fn failed_event_logs(failed: bool) -> RefEntry {
    RefEntry {
        signature: sig("f"),
        slot: Some(1),
        failed,
        logs: Some(vec![
            format!("Program {PROGRAM} invoke [1]"),
            format!("Program data: {}", recorded_payload(0)),
            format!("Program {PROGRAM} failed: custom program error: 0x1"),
        ]),
    }
}

#[test]
fn events_of_a_failed_transaction_are_not_decoded() {
    assert!(decode_missing(&idl(), &[failed_event_logs(true)])
        .unwrap()
        .is_empty());
    let kept = decode_missing(&idl(), &[failed_event_logs(false)]).unwrap();
    assert_eq!(kept.len(), 1, "the same logs decode when the tx succeeded");
}

#[test]
fn a_failed_transaction_is_skipped_but_its_neighbours_still_decode() {
    let mut ok = failed_event_logs(false);
    ok.signature = sig("ok");
    let events = decode_missing(&idl(), &[failed_event_logs(true), ok]).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].signature.0, "ok");
}

fn full_tx(signature: &str, slot: u64, err: Value) -> Value {
    json!({"slot": slot, "blockTime": 1_700_000_000u64 + slot,
           "transaction": {"signatures": [signature], "message": {}},
           "meta": {"err": err, "fee": 5000, "logMessages": []}})
}

#[test]
fn backfill_has_the_raw_webhook_shape_and_replays_the_gap() {
    let reference = parse_reference(&fx("event-lab-signatures.json")).unwrap();
    let received_file = fx("event-lab-received.json");
    let received = parse_received(&received_file).unwrap();
    let report = diff(&reference, &received, Status::Any);
    let source = RecordedSource::from_json(&fx("event-lab-refetch.json")).unwrap();

    let raws = raw_for_missing(&report.missing, &source).unwrap();
    assert_eq!(source.calls().len(), 1, "one call for all missing rows");
    let payload = webhook_payload(&raws).unwrap();

    assert_eq!(payload.len(), 3);
    for p in &payload {
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert!(
            keys.iter()
                .all(|k| ["slot", "blockTime", "transaction", "meta", "version"].contains(k)),
            "unexpected key in {keys:?}"
        );
        for k in ["slot", "blockTime", "transaction", "meta"] {
            assert!(p.get(k).is_some(), "missing {k}");
        }
        assert!(p.get("type").is_none() && p.get("description").is_none());
    }
    let slots: Vec<u64> = payload
        .iter()
        .map(|p| p["slot"].as_u64().unwrap())
        .collect();
    assert!(
        slots.windows(2).all(|w| w[0] <= w[1]),
        "not oldest first: {slots:?}"
    );

    let mut all = received_file["payloads"].as_array().unwrap().clone();
    all.extend(payload);
    let replayed = parse_received(&Value::Array(all)).unwrap();
    let after = diff(&reference, &replayed, Status::Any);
    assert!(
        after.missing.is_empty(),
        "the replay left a gap: {:?}",
        names(&after.missing)
    );
}

#[test]
fn backfill_is_ordered_oldest_slot_first_and_stable() {
    let rows = [
        full_tx("s5", 5, Value::Null),
        full_tx("s3a", 3, Value::Null),
        full_tx("s3b", 3, Value::Null),
        full_tx("s4", 4, Value::Null),
    ];
    let payload = webhook_payload(&rows).unwrap();
    let order: Vec<&str> = payload
        .iter()
        .map(|p| p["transaction"]["signatures"][0].as_str().unwrap())
        .collect();
    assert_eq!(order, ["s3a", "s3b", "s4", "s5"]);
}

#[test]
fn backfill_refuses_rows_that_are_not_full_transactions() {
    let signatures_row = json!({"signature": "a", "slot": 1, "err": null});
    let err = webhook_payload(&[signatures_row]).unwrap_err();
    assert!(err.to_string().contains("not a full transaction"), "{err}");
    let mut no_slot = full_tx("a", 1, Value::Null);
    no_slot.as_object_mut().unwrap().remove("slot");
    assert!(webhook_payload(&[no_slot]).is_err());
    let mut no_meta = full_tx("a", 1, Value::Null);
    no_meta.as_object_mut().unwrap().remove("meta");
    assert!(webhook_payload(&[full_tx("ok", 1, Value::Null), no_meta]).is_err());
}

#[test]
fn backfill_follows_the_status_filter_and_keeps_failed_transactions_as_failed() {
    let reference = parse_reference(&json!([
        {"signature": "ok", "slot": 1, "err": null},
        {"signature": "bad", "slot": 2, "err": {"InstructionError": [0, "Custom"]}},
    ]))
    .unwrap();
    let received = parse_received(&json!([])).unwrap();
    let source = RecordedSource::from_json(&json!({"responses": {
        "ok": full_tx("ok", 1, Value::Null),
        "bad": full_tx("bad", 2, json!({"InstructionError": [0, "Custom"]})),
    }}))
    .unwrap();

    let default = diff(&reference, &received, Status::default());
    let raws = raw_for_missing(&default.missing, &source).unwrap();
    assert_eq!(webhook_payload(&raws).unwrap().len(), 1);

    let any = diff(&reference, &received, Status::Any);
    let raws = raw_for_missing(&any.missing, &source).unwrap();
    let payload = webhook_payload(&raws).unwrap();
    assert_eq!(payload.len(), 2);
    assert!(payload[0]["meta"]["err"].is_null());
    assert!(
        !payload[1]["meta"]["err"].is_null(),
        "a raw webhook carries failed transactions with their err"
    );
}

struct AnswersRaw(Vec<Value>);

impl TxSource for AnswersRaw {
    fn fetch_full(&self, _: &[Signature]) -> anyhow::Result<Vec<RefEntry>> {
        unreachable!("raw only")
    }

    fn fetch_raw(&self, _: &[Signature]) -> anyhow::Result<Vec<Value>> {
        Ok(self.0.clone())
    }
}

fn entry(signature: &str) -> RefEntry {
    RefEntry {
        signature: sig(signature),
        slot: Some(1),
        failed: false,
        logs: None,
    }
}

#[test]
fn raw_fetch_checks_that_each_result_answers_its_row() {
    let missing = [entry("a"), entry("b")];
    let swapped = AnswersRaw(vec![
        full_tx("b", 2, Value::Null),
        full_tx("a", 1, Value::Null),
    ]);
    let err = raw_for_missing(&missing, &swapped).unwrap_err();
    assert!(err.to_string().contains("not a"), "{err}");
    let short = AnswersRaw(vec![full_tx("a", 1, Value::Null)]);
    assert!(raw_for_missing(&missing, &short).is_err());
    let fine = AnswersRaw(vec![
        full_tx("a", 1, Value::Null),
        full_tx("b", 2, Value::Null),
    ]);
    assert_eq!(raw_for_missing(&missing, &fine).unwrap().len(), 2);
    assert!(raw_for_missing(&[], &swapped).unwrap().is_empty());
}

fn cli(args: &[&str]) -> std::process::Output {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::process::Command::new(env!("CARGO_BIN_EXE_webhook-reconcile"))
        .current_dir(root)
        .arg("diff")
        .args(args)
        .output()
        .unwrap()
}

const E2E: [&str; 8] = [
    "--received",
    "fixtures/event-lab-received.json",
    "--reference-pages",
    "fixtures/event-lab-pages.json",
    "--status",
    "any",
    "--refetch-from",
    "fixtures/event-lab-refetch.json",
];

#[test]
fn cli_pages_the_outage_window_and_writes_the_backfill_file() {
    let out_path = std::env::temp_dir().join(format!("wr-backfill-{}.json", std::process::id()));
    let out_arg = out_path.to_str().unwrap();
    let mut args = E2E.to_vec();
    args.extend([
        "--idl",
        "fixtures/event_lab.idl.json",
        "--backfill-out",
        out_arg,
    ]);
    let out = cli(&args);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{stdout} {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(stdout.contains("reference 10 (mocked)"), "{stdout}");
    assert!(stdout.contains("missing: 3"), "{stdout}");
    assert!(
        stdout.contains("decoded events from missing txs: 3"),
        "{stdout}"
    );
    assert!(
        stdout.contains("backfill: 3 webhook payloads written"),
        "{stdout}"
    );
    let written: Value = serde_json::from_str(&fs::read_to_string(&out_path).unwrap()).unwrap();
    fs::remove_file(&out_path).unwrap();
    assert_eq!(written.as_array().unwrap().len(), 3);
    assert!(written[0]["transaction"]["signatures"][0].is_string());
}

#[test]
fn cli_writes_the_backfill_to_stdout_and_the_report_to_stderr() {
    let mut args = E2E.to_vec();
    args.extend(["--backfill-out", "-"]);
    let out = cli(&args);
    let payload: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(payload.as_array().unwrap().len(), 3);
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing: 3"));
}

#[test]
fn cli_fails_loudly_when_the_page_cap_is_hit() {
    let mut args = E2E.to_vec();
    args.extend(["--max-pages", "2", "--idl", "fixtures/event_lab.idl.json"]);
    let out = cli(&args);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("stopped after 2 pages"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn cli_refuses_a_backfill_without_a_source() {
    let out = cli(&[
        "--received",
        "fixtures/event-lab-received.json",
        "--reference-pages",
        "fixtures/event-lab-pages.json",
        "--backfill-out",
        "-",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("--backfill-out needs"));
}

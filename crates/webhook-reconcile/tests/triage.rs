//! Triage tests. All offline, no key. The Jev exchanges in fixtures/jev-classify-mocked.json are
//! "mocked": hand-built from the documented response shape, not recorded from a live call.
//! fixtures/triage-report-gtfa.json is the `diff --json` output of the gtfa-live demo
//! (reference "run live", received "mocked").
use anyhow::Result;
use serde_json::{json, Value};
use std::{cell::Cell, fs, path::PathBuf, rc::Rc};
use webhook_reconcile::triage::*;

fn path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

fn text(name: &str) -> String {
    fs::read_to_string(path(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn json_fx(name: &str) -> Value {
    serde_json::from_str(&text(name)).unwrap()
}

fn ticket(name: &str) -> Ticket {
    Ticket { text: text(name) }
}

fn stopped_ticket() -> Ticket {
    ticket("triage-ticket-webhook-stopped.txt")
}

/// A valid signature (Base58 of 64 bytes) that carries the name, so a test can find it again.
fn sig(name: &str) -> String {
    let mut bytes = [0u8; 64];
    for (i, b) in name.bytes().enumerate() {
        bytes[i + 1] = b;
    }
    bs58::encode(bytes).into_string()
}

fn row(name: &str, failed: bool) -> Value {
    json!({"signature": sig(name), "slot": 1, "failed": failed, "logs": null})
}

/// A report in the shape of `diff --json`.
fn report_of(
    reference_tag: Value,
    received_tag: Value,
    missing: Vec<Value>,
    skipped: Vec<Value>,
    duplicates: usize,
    extra: usize,
) -> TriageReport {
    TriageReport::from_json(&json!({
        "status": "succeeded",
        "reference_tag": reference_tag,
        "received_tag": received_tag,
        "reference_count": 10,
        "received_count": 9,
        "missing": missing,
        "skipped_by_status": skipped,
        "duplicates": vec![json!({"signature": "dup", "times": 2}); duplicates],
        "extra": vec![json!("extra"); extra],
        "decoded": null,
        "backfill_count": null,
    }))
    .unwrap()
}

/// Both sides tagged `run live`: the only provenance the reply treats as real.
fn customer_report(missing: Vec<Value>) -> TriageReport {
    report_of(json!("run live"), json!("run live"), missing, vec![], 0, 0)
}

fn gtfa_report() -> TriageReport {
    TriageReport::from_json(&json_fx("triage-report-gtfa.json")).unwrap()
}

fn reply(r: &TriageReport) -> String {
    render_reply(r)
}

struct Recording {
    calls: Rc<Cell<usize>>,
    kind: TicketKind,
    confidence: Option<f64>,
}

impl Classifier for Recording {
    fn classify(&self, _: &Ticket) -> Result<Classification> {
        self.calls.set(self.calls.get() + 1);
        Ok(Classification {
            kind: self.kind,
            confidence: self.confidence,
        })
    }
}

fn recording(calls: &Rc<Cell<usize>>, confidence: Option<f64>) -> Result<Box<dyn Classifier>> {
    Ok(Box::new(Recording {
        calls: calls.clone(),
        kind: TicketKind::WebhookStopped,
        confidence,
    }))
}

struct CountingJev {
    calls: Rc<Cell<usize>>,
    inner: RecordedJev,
}

impl JevApi for CountingJev {
    fn system_one(&self, request: &Value) -> Result<Value> {
        self.calls.set(self.calls.get() + 1);
        self.inner.system_one(request)
    }
}

fn recorded_jev() -> RecordedJev {
    RecordedJev::from_json(&json_fx("jev-classify-mocked.json")).unwrap()
}

fn draft_text(d: Draft) -> String {
    match d {
        Draft::Reply(t) | Draft::NeedsHuman(t) => t,
    }
}

#[test]
fn triage_flag_off_calls_no_model() {
    let calls = Rc::new(Cell::new(0));
    let jev_calls = calls.clone();
    let off = select_classifier(false, || {
        let c: Box<dyn Classifier> = Box::new(JevClassifier::new(CountingJev {
            calls: jev_calls,
            inner: recorded_jev(),
        }));
        Ok(c)
    })
    .unwrap();
    let draft = triage(&stopped_ticket(), &gtfa_report(), off.as_ref()).unwrap();
    assert!(
        matches!(draft, Draft::Reply(_)),
        "the keyword rules draft the reply"
    );
    assert_eq!(calls.get(), 0, "flag off must not reach the model");

    let jev_calls = calls.clone();
    let on = select_classifier(true, || {
        let c: Box<dyn Classifier> = Box::new(JevClassifier::new(CountingJev {
            calls: jev_calls,
            inner: recorded_jev(),
        }));
        Ok(c)
    })
    .unwrap();
    triage(&stopped_ticket(), &gtfa_report(), on.as_ref()).unwrap();
    assert_eq!(calls.get(), 1, "flag on reaches the model exactly once");
}

#[test]
fn triage_flag_off_never_builds_the_model_side() {
    let built = Cell::new(false);
    select_classifier(false, || {
        built.set(true);
        recording(&Rc::new(Cell::new(0)), None)
    })
    .unwrap();
    assert!(
        !built.get(),
        "flag off must not build the Jev classifier or read its key"
    );
}

#[test]
fn triage_flag_on_uses_the_recorded_fixture() {
    let fixture = json_fx("jev-classify-mocked.json");
    assert_eq!(fixture["tag"], "mocked");
    for exchange in fixture["exchanges"].as_array().unwrap() {
        let file = exchange["ticket_file"].as_str().unwrap();
        assert_eq!(
            exchange["request"],
            jev_request(&ticket(file)),
            "{file}: the recorded prompt must equal the prompt the code sends"
        );
    }
    let jev = JevClassifier::new(recorded_jev());
    let report = gtfa_report();

    let stopped = triage(&stopped_ticket(), &report, &jev).unwrap();
    assert_eq!(stopped, Draft::Reply(render_reply(&report)));

    let billing = triage(&ticket("triage-ticket-billing.txt"), &report, &jev).unwrap();
    assert!(draft_text(billing).contains("not a webhook delivery ticket"));

    let vague = triage(&ticket("triage-ticket-vague.txt"), &report, &jev).unwrap();
    let vague = draft_text(vague);
    assert!(vague.contains("confidence 0.55 is below 0.60"), "{vague}");
}

#[test]
fn a_confidence_that_is_not_a_probability_is_an_error() {
    let answer = |confidence: Value| json!({"answers": {"kind": {"type": "choice", "choice": "webhook_stopped", "confidence": confidence}}});
    for bad in [json!(9.0), json!(1.01), json!(-0.1), json!(1e300)] {
        assert!(parse_jev_response(&answer(bad.clone())).is_err(), "{bad}");
    }
    for ok in [json!(0.0), json!(1.0), json!(0.6)] {
        assert!(parse_jev_response(&answer(ok.clone())).is_ok(), "{ok}");
    }
}

#[test]
fn the_keyword_rules_ignore_a_webhook_that_is_not_a_delivery_problem() {
    let kind = |s: &str| NoModel.classify(&Ticket { text: s.into() }).unwrap().kind;
    assert_eq!(
        kind("My webhook isn't on the paid plan. How do I upgrade?"),
        TicketKind::Other
    );
    assert_eq!(
        kind("My webhook isn't receiving events"),
        TicketKind::WebhookStopped
    );
    assert_eq!(
        kind("My webhook has stopped delivering"),
        TicketKind::WebhookStopped
    );
}

#[test]
fn the_ai_path_refuses_a_ticket_with_a_secret() {
    let secrets = [
        "My webhook stopped. url https://example.com/hook?api-key=abc",
        "My webhook stopped. I set api-key=abc123 in the header",
        "My webhook stopped. Authorization: Bearer abcdefgh12345678",
        "My webhook stopped. See https://example.com/hook?token=1",
        "My webhook stopped. key 3Zk9aXq7LmN4pRt8VwYb2CdEf5GhJ6KsMn1PqRs",
        "My webhook stopped. key 0123456789abcdef0123456789abcdef",
    ];
    for text in secrets {
        let calls = Rc::new(Cell::new(0));
        let jev = JevClassifier::new(CountingJev {
            calls: calls.clone(),
            inner: recorded_jev(),
        });
        let draft = triage(&Ticket { text: text.into() }, &gtfa_report(), &jev).unwrap();
        assert_eq!(draft, Draft::NeedsHuman(SECRET_REASON.into()), "{text}");
        assert_eq!(calls.get(), 0, "the ticket was sent: {text}");
    }
    // The fixture tickets carry no secret, so they still reach the model.
    for name in [
        "triage-ticket-webhook-stopped.txt",
        "triage-ticket-billing.txt",
        "triage-ticket-vague.txt",
    ] {
        assert!(!looks_like_secret(&text(name)), "{name}");
    }
    let calls = Rc::new(Cell::new(0));
    let jev = JevClassifier::new(CountingJev {
        calls: calls.clone(),
        inner: recorded_jev(),
    });
    triage(&stopped_ticket(), &gtfa_report(), &jev).unwrap();
    assert_eq!(calls.get(), 1);
}

#[test]
fn text_fixtures_have_no_carriage_returns() {
    // The Jev replay matches the ticket text byte for byte. A CRLF checkout (Git for Windows with
    // autocrlf) would break it, so `.gitattributes` pins these files to LF.
    let dir = path("");
    let mut checked = 0;
    for entry in fs::read_dir(&dir).unwrap() {
        let file = entry.unwrap().path();
        let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
        if ["txt", "md", "json"].contains(&ext) {
            let bytes = fs::read(&file).unwrap();
            assert!(!bytes.contains(&b'\r'), "{} has a CR byte", file.display());
            checked += 1;
        }
    }
    assert!(
        checked >= 10,
        "only {checked} text fixtures found in {dir:?}"
    );
}

#[test]
fn each_recorded_confidence_is_the_top_probability() {
    let fixture = json_fx("jev-classify-mocked.json");
    for exchange in fixture["exchanges"].as_array().unwrap() {
        let file = exchange["ticket_file"].as_str().unwrap();
        let answer = &exchange["response"]["answers"]["kind"];
        let top = answer["probabilities"]
            .as_object()
            .unwrap()
            .values()
            .map(|p| p.as_f64().unwrap())
            .fold(0.0, f64::max);
        assert_eq!(answer["confidence"].as_f64().unwrap(), top, "{file}");
    }
}

#[test]
fn a_request_missing_from_the_fixture_is_an_error() {
    let unknown = Ticket {
        text: "a ticket the fixture never saw".into(),
    };
    let err = JevClassifier::new(recorded_jev())
        .classify(&unknown)
        .unwrap_err();
    assert!(
        err.to_string().contains("no recorded Jev exchange"),
        "{err}"
    );
}

#[test]
fn a_jev_answer_the_code_cannot_read_is_an_error() {
    let answer = |choice: Value, confidence: Value| json!({"answers": {"kind": {"type": "choice", "choice": choice, "confidence": confidence}}});
    assert!(parse_jev_response(&json!({"answers": {}})).is_err());
    assert!(parse_jev_response(&answer(json!("billing"), json!(0.9))).is_err());
    assert!(parse_jev_response(&answer(json!("other"), Value::Null)).is_err());
    assert!(parse_jev_response(&answer(Value::Null, json!(0.9))).is_err());
    let ok = parse_jev_response(&answer(json!("webhook_stopped"), json!(0.9))).unwrap();
    assert_eq!(ok.kind, TicketKind::WebhookStopped);
    assert_eq!(ok.confidence, Some(0.9));
    let other = parse_jev_response(&answer(json!("other"), json!(0.7))).unwrap();
    assert_eq!(other.kind, TicketKind::Other);
}

#[test]
fn the_confidence_bar_is_inclusive() {
    let report = gtfa_report();
    let at = |c: f64| {
        let calls = Rc::new(Cell::new(0));
        let classifier = recording(&calls, Some(c)).unwrap();
        triage(&stopped_ticket(), &report, classifier.as_ref()).unwrap()
    };
    assert!(matches!(at(MIN_CONFIDENCE), Draft::Reply(_)));
    assert!(matches!(at(MIN_CONFIDENCE - 0.01), Draft::NeedsHuman(_)));
}

#[test]
fn the_keyword_rules_need_a_webhook_and_a_stopped_word() {
    let kind = |s: &str| NoModel.classify(&Ticket { text: s.into() }).unwrap().kind;
    assert_eq!(kind("My webhook stopped"), TicketKind::WebhookStopped);
    assert_eq!(
        kind("webhook MISSED two events"),
        TicketKind::WebhookStopped
    );
    // "missing" and "isn't" are too loose: they also describe invoices and dashboards. A real
    // missed-event ticket that uses neither word goes to a human, which is the safe side.
    assert_eq!(kind("My webhook invoice is missing"), TicketKind::Other);
    assert_eq!(kind("My webhook isn't in the dashboard"), TicketKind::Other);
    assert_eq!(kind("How do I pay for webhooks?"), TicketKind::Other);
    assert_eq!(kind("My RPC calls stopped working"), TicketKind::Other);
    assert_eq!(
        NoModel.classify(&stopped_ticket()).unwrap().confidence,
        None
    );
}

fn quotes_of(reply: &str) -> Vec<String> {
    let parts: Vec<&str> = reply.split('"').collect();
    assert_eq!(parts.len() % 2, 1, "unbalanced double quotes in:\n{reply}");
    parts
        .iter()
        .skip(1)
        .step_by(2)
        .map(|s| s.to_string())
        .collect()
}

fn has_date(s: &str) -> bool {
    s.as_bytes().windows(10).any(|w| {
        w.iter().enumerate().all(|(i, b)| match i {
            4 | 7 => *b == b'-',
            _ => b.is_ascii_digit(),
        })
    })
}

#[test]
fn triage_reply_quotes_are_verbatim() {
    let snapshot = text("helius-faq-webhooks.md");
    let header = snapshot.lines().next().unwrap();
    assert!(
        header.contains("https://www.helius.dev/docs/faqs/webhooks"),
        "{header}"
    );
    assert!(
        has_date(header),
        "the header must carry the fetch date: {header}"
    );
    let sentences = faq_sentences(&snapshot);
    assert!(
        !sentences.iter().any(|s| s == "We retry up to 3 times"),
        "the sentence splitter must not cut a sentence short"
    );

    let everything = report_of(
        json!("run live"),
        json!("run live"),
        vec![row("lostA", false), row("failedA", true)],
        vec![row("failedB", true)],
        2,
        1,
    );
    let replies = [
        reply(&everything),
        reply(&gtfa_report()),
        reply(&customer_report(vec![])),
        reply(&customer_report(vec![row("lostA", false)])),
    ];
    let mut seen = Vec::new();
    for r in &replies {
        for q in quotes_of(r) {
            assert!(sentences.contains(&q), "not a whole FAQ sentence: {q}");
            seen.push(q);
        }
    }
    for q in [
        RETRY_QUOTE,
        LOST_QUOTE,
        DUPLICATE_QUOTE,
        DUPLICATE_CAUSE_QUOTE,
        DEDUPE_QUOTE,
        FAILED_QUOTE,
        TYPE_QUOTE,
    ] {
        assert!(seen.iter().any(|s| s == q), "no reply quotes: {q}");
    }
    // A fragment of a sentence is not a whole sentence.
    assert!(!sentences.iter().any(|s| s == "We retry up to 3 times"));
}

/// The snapshot with Markdown emphasis and quote marks removed, cut into sentences.
fn faq_sentences(snapshot: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in snapshot.lines() {
        let line = line.trim_start_matches('>').trim().replace("**", "");
        let mut current = String::new();
        let chars: Vec<char> = line.chars().collect();
        for (i, c) in chars.iter().enumerate() {
            current.push(*c);
            let ends = matches!(c, '.' | '?' | '!');
            if ends && chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
                out.push(current.trim().to_string());
                current.clear();
            }
        }
        if !current.trim().is_empty() {
            out.push(current.trim().to_string());
        }
    }
    out
}

/// Every claim that needs real data: loss, retry failure, endpoint failure, backfill, watchdog.
const REAL_ONLY_CLAIMS: [&str; 12] = [
    "never reached",
    "did not log",
    "permanently lost",
    "will not arrive",
    "We retry up to",
    "may have returned an error",
    "timed out",
    "backfill",
    "disables a webhook",
    "confirm the webhook type",
    "Nothing is lost",
    "ready to replay",
];

fn assert_set_difference_only(r: &str) {
    for banned in REAL_ONLY_CLAIMS {
        assert!(!r.contains(banned), "`{banned}` in:\n{r}");
    }
    assert!(r.contains("not a confirmed loss"), "{r}");
}

#[test]
fn triage_reply_carries_both_tags() {
    let r = reply(&gtfa_report());
    assert!(r.contains("reference `run live`, received `mocked`"), "{r}");
    assert!(
        r.contains("the reference holds 2 transactions that the received set lacks"),
        "{r}"
    );
    assert_set_difference_only(&r);

    for (reference, received) in [
        ("litesvm", "run live"),
        ("run live", "litesvm"),
        ("mocked", "mocked"),
    ] {
        let r = reply(&report_of(
            json!(reference),
            json!(received),
            vec![row("s", false)],
            vec![],
            0,
            0,
        ));
        assert!(
            r.contains(&format!("reference `{reference}`, received `{received}`")),
            "{r}"
        );
        assert_set_difference_only(&r);
    }

    let live = customer_report(vec![row("s", false)]);
    let r = reply(&live);
    assert!(
        r.contains("reference `run live`, received `run live`"),
        "{r}"
    );
    assert!(
        r.contains(&format!(
            "1 transaction in the reference that the endpoint did not log: `{}`.",
            sig("s")
        )),
        "{r}"
    );
    assert!(!r.contains("never reached"), "{r}");
    assert!(!r.contains("recorded sample"), "{r}");
}

#[test]
fn the_real_case_asks_for_the_webhook_type_before_calling_anything_lost() {
    let r = reply(&customer_report(vec![row("s", false)]));
    assert!(
        r.contains("please confirm the webhook type and its transaction filters"),
        "{r}"
    );
    assert!(r.contains(TYPE_QUOTE), "{r}");
    let confirm = r.find("please confirm the webhook type").unwrap();
    let lost = r.find(LOST_QUOTE).unwrap();
    assert!(
        confirm < lost,
        "the question comes before the loss quote:\n{r}"
    );
}

#[test]
fn missing_or_unknown_provenance_is_never_real() {
    // No tag at all, and a tag that is not defined.
    for (reference, received) in [
        (Value::Null, Value::Null),
        (json!("run live"), Value::Null),
        (json!("Run Live"), json!("run live")),
        (json!(42), json!("run live")),
    ] {
        let report = report_of(reference, received, vec![row("s", false)], vec![], 0, 0);
        assert!(!report.is_real());
        let r = reply(&report);
        assert!(r.contains("`unknown`"), "{r}");
        assert_set_difference_only(&r);
        let calls = Rc::new(Cell::new(0));
        let classifier = recording(&calls, Some(0.9)).unwrap();
        let draft = triage(&stopped_ticket(), &report, classifier.as_ref()).unwrap();
        assert!(
            matches!(&draft, Draft::NeedsHuman(why) if why.contains("tags are missing or unknown")),
            "{draft:?}"
        );
    }
}

#[test]
fn an_injected_tag_is_dropped_and_never_rendered() {
    let report = report_of(
        json!("run live SECRET-KEY-123\n\nAPI key: SECRET-KEY-123"),
        json!("run live"),
        vec![row("s", false)],
        vec![],
        0,
        0,
    );
    assert_eq!(report.reference_tag, None);
    assert!(!reply(&report).contains("SECRET-KEY-123"));
}

#[test]
fn a_signature_that_is_not_base58_of_64_bytes_is_an_error() {
    let with = |signature: &str| {
        TriageReport::from_json(&json!({
            "reference_tag": "run live", "received_tag": "run live",
            "reference_count": 1, "received_count": 0,
            "missing": [{"signature": signature, "slot": 1, "failed": false, "logs": null}],
            "skipped_by_status": [], "duplicates": [], "extra": [], "backfill_count": null,
        }))
    };
    assert!(with(&sig("ok")).is_ok());
    for bad in [
        "s",
        "",
        "s`\n\nAPI key: SECRET-KEY-123",
        "0OIl",
        &sig("ok")[1..],
        &format!("{}1", sig("ok")),
    ] {
        assert!(with(bad).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn triage_missing_zero_says_nothing_lost() {
    let r = reply(&customer_report(vec![]));
    assert!(r.contains("Nothing is lost in this window."), "{r}");
    for banned in ["never reached", "permanently lost", "retry", "backfill"] {
        assert!(!r.contains(banned), "{banned} in:\n{r}");
    }

    let recorded = reply(&report_of(
        json!("mocked"),
        json!("mocked"),
        vec![],
        vec![],
        0,
        0,
    ));
    assert!(
        recorded.contains("In this data set nothing is missing."),
        "{recorded}"
    );
    assert!(recorded.contains("not proof"), "{recorded}");
    assert!(!recorded.contains("Nothing is lost"), "{recorded}");
}

#[test]
fn a_status_filter_that_hides_lost_rows_does_not_say_nothing_lost() {
    let v = json!({
        "status": "failed", "reference_tag": "run live", "received_tag": "run live",
        "reference_count": 3, "received_count": 2,
        "missing": [row("badOne", true)], "skipped_by_status": [row("goodOne", false)],
        "duplicates": [], "extra": [], "backfill_count": null,
    });
    let report = TriageReport::from_json(&v).unwrap();
    assert_eq!(report.lost, [sig("goodOne")]);
    assert_eq!(report.failed, [sig("badOne")]);
    let r = reply(&report);
    assert!(r.contains(&sig("goodOne")), "{r}");
    assert!(!r.contains("Nothing is lost"), "{r}");
}

#[test]
fn triage_duplicates_are_observed_not_blamed_on_retries() {
    let r = reply(&report_of(Value::Null, Value::Null, vec![], vec![], 2, 0));
    assert!(
        r.contains(
            "2 signatures appeared more than once in the received set. The data does not show why."
        ),
        "{r}"
    );
    for banned in ["comes from retries", "is not a bug", "Helius retried"] {
        assert!(!r.contains(banned), "`{banned}` in:\n{r}");
    }
    assert!(
        r.contains(DUPLICATE_QUOTE)
            && r.contains(DUPLICATE_CAUSE_QUOTE)
            && r.contains(DEDUPE_QUOTE),
        "{r}"
    );

    let one = reply(&report_of(Value::Null, Value::Null, vec![], vec![], 1, 0));
    assert!(one.contains("1 signature appeared more than once"), "{one}");

    let none = reply(&report_of(Value::Null, Value::Null, vec![], vec![], 0, 0));
    assert!(
        !none.contains("more than once") && !none.contains(DEDUPE_QUOTE),
        "{none}"
    );
}

#[test]
fn triage_failed_rows_are_not_called_lost() {
    let report = report_of(
        json!("run live"),
        json!("run live"),
        vec![row("okLost", false), row("failedMissing", true)],
        vec![row("failedSkipped", true)],
        0,
        0,
    );
    assert_eq!(report.lost, [sig("okLost")]);
    assert_eq!(report.failed, [sig("failedMissing"), sig("failedSkipped")]);
    let r = reply(&report);
    assert!(
        r.contains(&format!(
            "1 transaction in the reference that the endpoint did not log: `{}`.",
            sig("okLost")
        )),
        "{r}"
    );
    assert!(
        !r.contains(&sig("failedMissing")) && !r.contains(&sig("failedSkipped")),
        "{r}"
    );
    assert!(
        r.contains("Of the never-received rows, 2 failed on chain. They are not lost events."),
        "{r}"
    );
    assert!(r.contains(FAILED_QUOTE), "{r}");

    let only_failed = reply(&report_of(
        json!("run live"),
        json!("run live"),
        vec![],
        vec![row("f", true)],
        0,
        0,
    ));
    assert!(
        only_failed.contains("Nothing is lost in this window."),
        "{only_failed}"
    );
    assert!(only_failed.contains("1 failed on chain"), "{only_failed}");
}

#[test]
fn the_lost_signatures_are_capped_at_three() {
    let rows: Vec<Value> = (1..=5).map(|i| row(&format!("sig{i}"), false)).collect();
    let r = reply(&customer_report(rows));
    let (a, b, c) = (sig("sig1"), sig("sig2"), sig("sig3"));
    assert!(r.contains(&format!("5 transactions in the reference that the endpoint did not log: `{a}`, `{b}`, `{c}`, and 2 more.")), "{r}");
    assert!(!r.contains(&sig("sig4")), "{r}");
}

#[test]
fn extra_signatures_ask_for_a_wider_window() {
    let r = reply(&report_of(Value::Null, Value::Null, vec![], vec![], 0, 2));
    assert!(r.contains("The received set holds 2 signatures outside the reference window. Widen the reference window"), "{r}");
    let none = reply(&customer_report(vec![]));
    assert!(!none.contains("Widen"), "{none}");
}

#[test]
fn the_backfill_line_follows_backfill_count() {
    let with = |count: Value| {
        TriageReport::from_json(&json!({
            "reference_tag": "run live", "received_tag": "run live",
            "reference_count": 2, "received_count": 1,
            "missing": [row("s", false)], "skipped_by_status": [],
            "duplicates": [], "extra": [], "backfill_count": count,
        }))
        .unwrap()
    };
    assert!(reply(&with(json!(3)))
        .contains("A backfill file with 3 webhook-shaped payloads is ready to replay"));
    assert!(reply(&with(Value::Null)).contains("I can prepare a backfill file"));
    assert!(reply(&with(json!(0))).contains("I can prepare a backfill file"));
}

#[test]
fn the_report_reads_the_demo_diff() {
    let r = gtfa_report();
    assert_eq!(r.reference_tag.as_deref(), Some("run live"));
    assert_eq!(r.received_tag.as_deref(), Some("mocked"));
    assert_eq!((r.reference_count, r.received_count), (100, 97));
    assert_eq!((r.lost.len(), r.failed.len()), (2, 3));
    assert_eq!((r.duplicates, r.extra), (2, 0));
    assert!(!r.is_real());
}

#[test]
fn a_report_without_the_diff_fields_is_an_error() {
    for key in [
        "missing",
        "skipped_by_status",
        "duplicates",
        "extra",
        "reference_count",
        "received_count",
    ] {
        let mut v = json_fx("triage-report-gtfa.json");
        v.as_object_mut().unwrap().remove(key);
        assert!(TriageReport::from_json(&v).is_err(), "{key}");
    }
    let mut v = json_fx("triage-report-gtfa.json");
    v["missing"][0].as_object_mut().unwrap().remove("failed");
    assert!(TriageReport::from_json(&v).is_err());
    let mut v = json_fx("triage-report-gtfa.json");
    v["missing"][0].as_object_mut().unwrap().remove("signature");
    assert!(TriageReport::from_json(&v).is_err());
}

fn cli(ticket_file: &str, extra: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_helius-triage"))
        .env_remove("TYPESAFE_API_KEY")
        .args(["--ticket"])
        .arg(path(ticket_file))
        .args(["--report"])
        .arg(path("triage-report-gtfa.json"))
        .args(extra)
        .output()
        .unwrap()
}

#[test]
fn cli_flag_off_drafts_a_reply_with_no_key() {
    let out = cli("triage-ticket-webhook-stopped.txt", &[]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains("reference `run live`, received `mocked`"),
        "{stdout}"
    );
}

#[test]
fn cli_flag_on_without_a_fixture_asks_for_the_key() {
    let out = cli("triage-ticket-webhook-stopped.txt", &["--ai"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("TYPESAFE_API_KEY is not set"));
}

#[test]
fn the_help_text_calls_the_fixture_hand_built_and_mocked() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_helius-triage"))
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&out.stdout).replace('\n', " ");
    let help = help.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(help.contains("hand-built mocked Jev fixture"), "{help}");
    assert!(!help.contains("Replay recorded"), "{help}");
}

#[test]
fn cli_flag_on_replays_the_recorded_fixture() {
    let fixture = path("jev-classify-mocked.json");
    let fixture = fixture.to_str().unwrap();
    let ok = cli(
        "triage-ticket-webhook-stopped.txt",
        &["--ai", "--jev-from", fixture],
    );
    assert_eq!(ok.status.code(), Some(0));
    let billing = cli(
        "triage-ticket-billing.txt",
        &["--ai", "--jev-from", fixture],
    );
    assert_eq!(billing.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&billing.stdout).starts_with("NEEDS HUMAN"));
}

#[test]
fn cli_jev_from_needs_the_ai_flag() {
    let out = cli("triage-ticket-webhook-stopped.txt", &["--jev-from", "x"]);
    assert_eq!(out.status.code(), Some(2));
}

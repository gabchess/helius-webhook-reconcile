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

const SIGNATURE_88: &str =
    "64GSSkdqb81kjFRxqwrcKGvu3Yooq2XUF9G9y6xTSohpbekxTV2ttmxhZe9FTNTg39B2TC9iuGpBwdybtPrwWCNn";
const GHP_TOKEN: &str = "ghp_l0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OIl0OI";
const BEARER: &str = "eyJhbGciOiJIUzI1NiJ9.abc123XYZ789";
const URL_WITH_QUERY: &str = "https://example.com/hook?api-key=abc123&x=9";

/// Each ticket paired with the secret it holds.
fn secret_tickets() -> Vec<(String, String)> {
    let with = |secret: &str| {
        (
            format!("My webhook stopped. Here it is: {secret}."),
            secret.to_string(),
        )
    };
    vec![
        (
            "My webhook stopped. My Helius API key is SECRET-KEY-123.".into(),
            "SECRET-KEY-123".into(),
        ),
        with("0123abcd-4567-89ef-0123-456789abcdef"),
        with(GHP_TOKEN),
        with(r#"{"api_key":"sk_live_abc123"}"#), // gitleaks:allow
        with("api_key = abc123"),
        with(&format!("Authorization: Bearer {BEARER}")),
        with(URL_WITH_QUERY),
        with(SIGNATURE_88),
    ]
}

/// Every maximal run of non-letters, split on spaces, that holds a digit.
fn digit_runs(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
        .filter(|run| run.chars().any(|c| c.is_ascii_digit()))
        .map(String::from)
        .collect()
}

#[test]
fn no_secret_shape_reaches_the_jev_request() {
    assert_eq!(GHP_TOKEN.len(), 40);
    assert_eq!(SIGNATURE_88.len(), 88);
    for (text, secret) in secret_tickets() {
        let request = jev_request(&Ticket { text: text.clone() });
        let wire = request.to_string().to_lowercase();
        assert!(
            !wire.contains(&secret.to_lowercase()),
            "the secret is in the request: {text}\n{wire}"
        );
        let state = request["state"].to_string();
        let runs = digit_runs(&text);
        assert!(!runs.is_empty(), "{text}");
        for run in runs {
            assert!(
                !state.contains(&run),
                "`{run}` is in the request state: {text}"
            );
        }
        assert!(
            !state.chars().any(|c| c.is_ascii_digit()),
            "a digit is in the request state: {text}\n{state}"
        );
        assert!(
            request.pointer("/state/ticket/text").is_none(),
            "the free ticket text is still in the request: {text}"
        );
        let words = request["state"]["ticket"]["words"].as_array().unwrap();
        for w in words {
            let w = w.as_str().unwrap();
            assert!(
                JEV_WORDS.contains(&w),
                "`{w}` is not a vocabulary word: {text}"
            );
        }
    }
}

#[test]
fn a_ticket_that_holds_a_secret_still_reaches_the_model_as_words_only() {
    struct Capture(Rc<std::cell::RefCell<Vec<Value>>>);
    impl JevApi for Capture {
        fn system_one(&self, request: &Value) -> Result<Value> {
            self.0.borrow_mut().push(request.clone());
            Ok(
                json!({"answers": {"kind": {"type": "choice", "choice": "webhook_stopped", "confidence": 0.97}}}),
            )
        }
    }
    for (text, secret) in secret_tickets() {
        let seen = Rc::new(std::cell::RefCell::new(Vec::new()));
        let jev = JevClassifier::new(Capture(seen.clone()));
        let ticket = Ticket { text: text.clone() };
        let draft = triage(&ticket, &gtfa_report(), &jev).unwrap();
        assert!(matches!(draft, Draft::Reply(_)), "{text}: {draft:?}");
        let seen = seen.borrow();
        assert_eq!(seen.len(), 1, "{text}");
        assert_eq!(seen[0], jev_request(&ticket), "{text}");
        assert!(
            !seen[0]
                .to_string()
                .to_lowercase()
                .contains(&secret.to_lowercase()),
            "{text}"
        );
    }
}

#[test]
fn the_words_are_vocabulary_words_in_order() {
    let words = |s: &str| ticket_words(&Ticket { text: s.into() });
    assert_eq!(
        words("My WEBHOOK Stopped, my webhook"),
        ["my", "webhook", "stopped", "my", "webhook"]
    );
    assert_eq!(
        words("My webhook stopped at 14:00 UTC, event_id=7 isn't delivering"),
        [
            "my",
            "webhook",
            "stopped",
            "event",
            "isn",
            "t",
            "delivering"
        ]
    );
    assert_eq!(words("a1b-c_d.e/f?g=h"), Vec::<&str>::new());
    assert_eq!(words("café naïve"), Vec::<&str>::new());
    assert_eq!(words("123 -- ?? 4_5"), Vec::<&str>::new());
    assert_eq!(words(""), Vec::<&str>::new());
    assert_eq!(words("webhookstopped"), Vec::<&str>::new());
}

#[test]
fn the_vocabulary_is_sorted_so_the_binary_search_finds_every_word() {
    assert!(JEV_WORDS.len() > 50);
    for pair in JEV_WORDS.windows(2) {
        assert!(
            pair[0] < pair[1],
            "`{}` is not before `{}`",
            pair[0],
            pair[1]
        );
    }
    for w in JEV_WORDS {
        assert!(
            !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()),
            "`{w}` is not a lowercase a to z word"
        );
        let found = ticket_words(&Ticket {
            text: w.to_string(),
        });
        assert_eq!(found, [*w], "`{w}` is in the list but is not found");
    }
}

#[test]
fn each_request_word_is_a_constant_from_the_vocabulary() {
    let mut texts: Vec<String> = secret_tickets().into_iter().map(|(t, _)| t).collect();
    for f in [
        "triage-ticket-webhook-stopped.txt",
        "triage-ticket-billing.txt",
        "triage-ticket-vague.txt",
    ] {
        texts.push(text(f));
    }
    texts.push(JEV_WORDS.join(" ").to_uppercase());
    for t in texts {
        let ticket = Ticket { text: t.clone() };
        let request = jev_request(&ticket);
        let sent = request["state"]["ticket"]["words"].as_array().unwrap();
        let words = ticket_words(&ticket);
        assert_eq!(sent.len(), words.len(), "{t}");
        for (sent, word) in sent.iter().zip(&words) {
            assert_eq!(sent.as_str().unwrap(), *word, "{t}");
            assert!(
                JEV_WORDS.contains(word),
                "`{word}` is not in JEV_WORDS: {t}"
            );
            assert!(
                JEV_WORDS
                    .iter()
                    .any(|v| std::ptr::eq(v.as_ptr(), word.as_ptr())),
                "`{word}` is a copy of ticket text, not a constant from JEV_WORDS: {t}"
            );
        }
    }
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
fn the_real_case_does_not_claim_delivery_or_completeness_it_cannot_see() {
    let r = reply(&customer_report(vec![row("s", false)]));
    for gone in [
        "while they were sent",
        "will not arrive again",
        "lists every successful transaction",
    ] {
        assert!(!r.contains(gone), "`{gone}` in:\n{r}");
    }
    for kept in [
        "This report does not show whether Helius attempted delivery or used up its retries.",
        "your log may have missed these events",
        "or the webhook may have been disabled.",
        "The reference lists the address's successful transactions, not only the ones your webhook matches",
    ] {
        assert!(r.contains(kept), "`{kept}` missing in:\n{r}");
    }
    assert!(r.contains(RETRY_QUOTE) && r.contains(LOST_QUOTE), "{r}");
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
        r.contains("Of the never-received rows, 2 failed on chain. An enhanced webhook never sends these. A raw webhook does send failed transactions, so for a raw webhook please confirm its filters before treating them as missing."),
        "{r}"
    );
    assert!(r.contains(FAILED_QUOTE), "{r}");
    assert!(!r.contains("not lost events"), "{r}");
    assert!(!r.contains("Nothing is lost"), "{r}");

    for tags in [
        ("run live", "run live"),
        ("mocked", "run live"),
        ("mocked", "mocked"),
    ] {
        let only_failed = reply(&report_of(
            json!(tags.0),
            json!(tags.1),
            vec![],
            vec![row("f", true)],
            0,
            0,
        ));
        assert!(
            !only_failed.contains("Nothing is lost") && !only_failed.contains("nothing is missing"),
            "{tags:?}\n{only_failed}"
        );
        assert!(only_failed.contains("1 failed on chain"), "{only_failed}");
        assert!(
            only_failed.contains("A raw webhook does send failed transactions, so for a raw webhook please confirm its filters before treating them as missing."),
            "{only_failed}"
        );
        assert!(!only_failed.contains("not lost events"), "{only_failed}");
    }
    let live = reply(&report_of(
        json!("run live"),
        json!("run live"),
        vec![],
        vec![row("f", true)],
        0,
        0,
    ));
    assert!(
        live.contains("No successful transaction is missing in this window. Every successful transaction in the reference is in the set your endpoint logged."),
        "{live}"
    );
    let recorded = reply(&report_of(
        json!("mocked"),
        json!("mocked"),
        vec![],
        vec![row("f", true)],
        0,
        0,
    ));
    assert!(
        recorded
            .contains("In this data set no successful transaction is missing. That is not proof"),
        "{recorded}"
    );

    let none = reply(&customer_report(vec![]));
    assert!(
        none.contains("Nothing is lost in this window.") && !none.contains("failed on chain"),
        "{none}"
    );
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

#[test]
fn the_opening_claims_the_customers_endpoint_only_for_run_live_data() {
    let opening = |r: &TriageReport| reply(r).split("\n\n").next().unwrap().to_string();
    let live = opening(&customer_report(vec![row("s", false)]));
    assert!(live.contains("your endpoint received"), "{live}");
    assert!(live.contains("your webhook address"), "{live}");

    let not_live = [
        (json!("run live"), json!("mocked")),
        (json!("mocked"), json!("run live")),
        (json!("mocked"), json!("mocked")),
        (json!("litesvm"), json!("litesvm")),
        (json!("run live"), json!(null)),
        (json!(null), json!(null)),
    ];
    for (reference, received) in not_live {
        for missing in [vec![], vec![row("s", false)]] {
            let r = report_of(reference.clone(), received.clone(), missing, vec![], 0, 0);
            let o = opening(&r);
            for banned in ["your endpoint", "your webhook", "Helius's own"] {
                assert!(
                    !o.contains(banned),
                    "`{banned}` in the opening for {reference} / {received}: {o}"
                );
            }
            assert!(o.contains("in this report"), "{o}");
        }
    }
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

/// The commands in SKILL.md that start with `program`, as argument lists. A trailing `> file` is dropped.
fn skill_commands(program: &str) -> Vec<Vec<String>> {
    let skill = fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills/helius-triage/SKILL.md"),
    )
    .unwrap();
    skill
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with(program))
        .map(|l| {
            let mut args = Vec::new();
            let (mut cur, mut quoted, mut started) = (String::new(), false, false);
            for c in l.split(" > ").next().unwrap().chars() {
                match c {
                    '"' => {
                        quoted = !quoted;
                        started = true;
                    }
                    ' ' if !quoted => {
                        if started {
                            args.push(std::mem::take(&mut cur));
                            started = false;
                        }
                    }
                    c => {
                        cur.push(c);
                        started = true;
                    }
                }
            }
            if started {
                args.push(cur);
            }
            args
        })
        .collect()
}

/// Run SKILL.md's step 2 and step 3 commands on a customer-shaped input set: a bare array of
/// `{signature}` rows and a saved reference response with no tag of its own.
fn run_documented_path(workdir: &str, drop_tag_flags: bool) -> std::process::Output {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(workdir);
    fs::create_dir_all(&dir).unwrap();
    let bare: Vec<Value> = json_fx("gtfa-live-received.json")["payloads"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!({"signature": p["signature"]}))
        .collect();
    fs::write(dir.join("received.json"), Value::Array(bare).to_string()).unwrap();
    let mut reference = json_fx("gtfa-live.json");
    reference.as_object_mut().unwrap().remove("tag");
    fs::write(dir.join("reference.json"), reference.to_string()).unwrap();
    fs::write(
        dir.join("ticket.txt"),
        text("triage-ticket-webhook-stopped.txt"),
    )
    .unwrap();

    let at = |a: String| {
        if a.ends_with(".json") || a.ends_with(".txt") {
            dir.join(a).to_str().unwrap().to_string()
        } else {
            a
        }
    };
    let mut diff_args = skill_commands("webhook-reconcile diff").remove(0);
    assert_eq!(&diff_args[..2], ["webhook-reconcile", "diff"]);
    diff_args.drain(..2);
    if drop_tag_flags {
        while let Some(i) = diff_args.iter().position(|a| a.ends_with("-tag")) {
            diff_args.drain(i..i + 2);
        }
    }
    let diff = std::process::Command::new(env!("CARGO_BIN_EXE_webhook-reconcile"))
        .arg("diff")
        .args(diff_args.into_iter().map(at))
        .output()
        .unwrap();
    assert!(
        matches!(diff.status.code(), Some(0 | 1)),
        "{}",
        String::from_utf8_lossy(&diff.stderr)
    );
    fs::write(dir.join("report.json"), &diff.stdout).unwrap();

    let triage_args = skill_commands("helius-triage").remove(0);
    std::process::Command::new(env!("CARGO_BIN_EXE_helius-triage"))
        .env_remove("TYPESAFE_API_KEY")
        .args(triage_args.into_iter().skip(1).map(at))
        .output()
        .unwrap()
}

#[test]
fn the_documented_skill_path_ends_in_a_draft() {
    let out = run_documented_path("skill-path-draft", false);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{stdout}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("reference `run live`, received `run live`"),
        "{stdout}"
    );
    assert!(
        stdout.contains("please confirm the webhook type"),
        "{stdout}"
    );
}

#[test]
fn without_the_tag_flags_the_same_path_asks_a_person() {
    let out = run_documented_path("skill-path-no-flags", true);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(
        stdout.starts_with("NEEDS HUMAN: the report's data tags are missing or unknown"),
        "{stdout}"
    );
}

#[test]
fn a_tag_flag_cannot_relabel_a_file_that_has_its_own_tag() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_webhook-reconcile"))
        .args(["diff", "--received"])
        .arg(path("gtfa-live-received.json"))
        .arg("--reference")
        .arg(path("gtfa-live.json"))
        .args(["--received-tag", "run live"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("conflicts with --received-tag"), "{stderr}");
}

#[test]
fn the_reference_tag_flag_reaches_a_paged_reference_with_no_tag_of_its_own() {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("pages-tag");
    fs::create_dir_all(&dir).unwrap();
    let mut pages = json_fx("event-lab-pages.json");
    pages.as_object_mut().unwrap().remove("tag");
    fs::write(dir.join("pages.json"), pages.to_string()).unwrap();
    let report_tags = |extra: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_webhook-reconcile"))
            .args(["diff", "--json", "--received"])
            .arg(path("event-lab-received.json"))
            .arg("--reference-pages")
            .arg(dir.join("pages.json"))
            .args(extra)
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&out.stdout).unwrap();
        (
            report["reference_tag"].clone(),
            report["received_tag"].clone(),
        )
    };
    assert_eq!(report_tags(&[]), (Value::Null, json!("mocked")));
    assert_eq!(
        report_tags(&["--reference-tag", "litesvm"]),
        (json!("litesvm"), json!("mocked"))
    );
}

#[test]
fn a_letters_only_secret_and_every_probe_secret_are_absent_from_the_request() {
    let letters_only = [
        "huntertwo",
        "correcthorse",
        "SECRET",
        "hunter",
        "two",
        "sk",
        "ghp",
        "oil",
        "abc",
        "password",
    ];
    for secret in letters_only {
        let t = Ticket {
            text: format!("My webhook stopped. My password is {secret} and {secret}."),
        };
        let wire = jev_request(&t).to_string().to_lowercase();
        assert!(
            !wire.contains(&secret.to_lowercase()),
            "`{secret}` is in the request:
{wire}"
        );
    }
    let t = Ticket {
        text: "Password huntertwo".into(),
    };
    assert!(ticket_words(&t).is_empty());
    assert_eq!(jev_request(&t)["state"], json!({"ticket": {"words": []}}));
    for (text, secret) in secret_tickets() {
        let wire = jev_request(&Ticket { text: text.clone() })
            .to_string()
            .to_lowercase();
        assert!(!wire.contains(&secret.to_lowercase()), "{text}");
        for fragment in [
            "sk",
            "live",
            "abc",
            "oil",
            "ghp",
            "secret",
            "bearer",
            "authorization",
        ] {
            assert!(
                !jev_request(&Ticket { text: text.clone() })["state"]
                    .to_string()
                    .contains(&format!("\"{fragment}\"")),
                "fragment `{fragment}` in the request for: {text}"
            );
        }
    }
}

#[test]
fn a_secret_that_is_a_vocabulary_word_is_the_only_one_that_reaches_the_model() {
    let t = Ticket {
        text: "My key is webhook".into(),
    };
    assert_eq!(ticket_words(&t), ["my", "key", "is", "webhook"]);
}

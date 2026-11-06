//! Triage for a "my webhook stopped" ticket. The diff report goes in, a drafted support reply comes out.
//! A person reviews the draft before it is sent.
//!
//! Data shape:
//! - `Ticket`: the customer's text.
//! - `ticket_words`: the only part of a ticket that can reach a model. It is the ticket's words that
//!   are in `JEV_WORDS`, a fixed support vocabulary, and each one is returned as the constant from
//!   that list. No other text from the ticket is sent, and no byte of a request is copied from the
//!   ticket. A secret reaches the model only if it is itself a vocabulary word.
//! - `TriageReport`: the `diff --json` output, reduced to what a reply needs.
//! - `Classifier`: decides whether the ticket is a webhook-delivery ticket. `NoModel` is the default
//!   and runs keyword rules. `JevClassifier` asks the TypeSafe Jev model and sits behind the `--ai` flag.
//! - `Draft`: a reply rendered from a template, or a note that a person must read the ticket first.
//!
//! The classifier only decides whether to draft. Every fact in the reply comes from the report, and
//! every quote in it is a whole sentence from the vendored Helius FAQ page.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

pub const FAQ_URL: &str = "https://www.helius.dev/docs/faqs/webhooks";

pub const RETRY_QUOTE: &str = "We retry up to 3 times with a 1-second gap between each attempt.";
pub const LOST_QUOTE: &str = "If all retry attempts fail, the event is permanently lost — there is no re-queue or second delivery cycle.";
pub const DUPLICATE_QUOTE: &str = "Helius may deliver the same webhook event multiple times.";
pub const DUPLICATE_CAUSE_QUOTE: &str = "This may happen due to retry logic or network issues.";
pub const TYPE_QUOTE: &str =
    "Ensure the selected transaction type matches the transactions you're monitoring.";
pub const DEDUPE_QUOTE: &str = "To handle duplicates, use the transaction signature or a unique event identifier to track which events you've already processed.";
pub const FAILED_QUOTE: &str = "Enhanced webhooks do not send failed transactions, but raw webhooks do include both successful and failed transactions.";

/// A model answer below this confidence is read by a person before any draft is made.
pub const MIN_CONFIDENCE: f64 = 0.6;
/// The provenance tags a diff report may carry. Any other value, or none, is unknown provenance.
pub const KNOWN_TAGS: [&str; 3] = ["run live", "mocked", "litesvm"];
pub const JEV_MODEL: &str = "jev-1.13.0";
const JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
const SIGNATURES_SHOWN: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TicketKind {
    WebhookStopped,
    Other,
}

/// `confidence` is `None` for rule-based classification, which has no probabilities.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Classification {
    pub kind: TicketKind,
    pub confidence: Option<f64>,
}

pub trait Classifier {
    fn classify(&self, ticket: &Ticket) -> Result<Classification>;
}

/// The default classifier. Keyword rules, no key, no network.
pub struct NoModel;

/// Delivery-specific phrases. A bare "missing" or "isn't" also fits invoices and dashboards.
const STOPPED_WORDS: [&str; 14] = [
    "webhook stopped",
    "webhook has stopped",
    "webhook just stopped",
    "stopped delivering",
    "stopped sending",
    "stopped receiving",
    "stopped firing",
    "missed",
    "not getting",
    "not receiving",
    "no events",
    "dropped event",
    "isn't receiving",
    "isn't delivering",
];

impl Classifier for NoModel {
    fn classify(&self, ticket: &Ticket) -> Result<Classification> {
        let text = ticket.text.to_lowercase();
        let stopped = text.contains("webhook") && STOPPED_WORDS.iter().any(|w| text.contains(w));
        Ok(Classification {
            kind: if stopped {
                TicketKind::WebhookStopped
            } else {
                TicketKind::Other
            },
            confidence: None,
        })
    }
}

/// The only words a model request may carry, sorted for binary search. They are the words that
/// classification needs: the webhook and delivery words of `STOPPED_WORDS`, the billing, plan and
/// code words that the `not_for` text names, and the function words of the three mocked tickets.
/// A name, a password, a token, a signature or a number that is not in it cannot be sent.
pub static JEV_WORDS: &[&str] = &[
    "about",
    "all",
    "and",
    "any",
    "api",
    "are",
    "before",
    "billing",
    "but",
    "can",
    "code",
    "credits",
    "day",
    "days",
    "delivering",
    "delivery",
    "did",
    "didn",
    "do",
    "does",
    "don",
    "drop",
    "dropped",
    "duplicate",
    "duplicates",
    "endpoint",
    "event",
    "events",
    "expected",
    "failed",
    "fewer",
    "firing",
    "for",
    "from",
    "getting",
    "has",
    "hasn",
    "have",
    "haven",
    "helius",
    "hour",
    "hours",
    "how",
    "i",
    "in",
    "integration",
    "is",
    "isn",
    "it",
    "just",
    "key",
    "last",
    "longer",
    "missed",
    "missing",
    "more",
    "my",
    "need",
    "no",
    "not",
    "of",
    "off",
    "on",
    "or",
    "plan",
    "receive",
    "received",
    "receives",
    "receiving",
    "registered",
    "retry",
    "sending",
    "since",
    "some",
    "something",
    "stop",
    "stopped",
    "stops",
    "t",
    "than",
    "the",
    "them",
    "this",
    "to",
    "today",
    "transaction",
    "transactions",
    "transfers",
    "upgrade",
    "wallet",
    "was",
    "we",
    "webhook",
    "webhooks",
    "week",
    "were",
    "what",
    "when",
    "why",
    "with",
    "working",
    "yesterday",
    "you",
    "your",
];

/// The ticket's words that are in `JEV_WORDS`, in order. The text is lowercased and split on
/// anything that is not a letter a to z. Each kept word is the constant from `JEV_WORDS`, so no byte
/// of the result comes from the ticket. A secret reaches the model only if it is itself a
/// vocabulary word.
pub fn ticket_words(ticket: &Ticket) -> Vec<&'static str> {
    ticket
        .text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_lowercase())
        .filter_map(|w| JEV_WORDS.binary_search(&w).ok().map(|i| JEV_WORDS[i]))
        .collect()
}

/// One call to the TypeSafe `systemone` endpoint. The live implementation is `JevHttp`.
/// `RecordedJev` replays a hand-built mocked Jev fixture.
pub trait JevApi {
    fn system_one(&self, request: &Value) -> Result<Value>;
}

/// The question set sent to Jev for one ticket. The state holds `ticket_words`, which are constants
/// from `JEV_WORDS`, and nothing else from the ticket. The question points at it by path.
pub fn jev_request(ticket: &Ticket) -> Value {
    json!({
        "state": {"ticket": {"words": ticket_words(ticket)}},
        "model": JEV_MODEL,
        "questions": {
            "kind": {
                "type": "choice",
                "instructions": "Which kind of support request do the words in `ticket.words` describe? They are the ticket's words that are in a fixed support vocabulary, in order. Every other word was removed.",
                "criteria": {
                    "webhook_stopped": {
                        "what": "The customer says a Helius webhook stopped delivering events or missed events.",
                        "not_for": "Billing, plan, API key or code questions that only mention webhooks."
                    },
                    "other": {
                        "what": "Any other support request.",
                        "not_for": "A webhook that stopped delivering or missed events."
                    }
                }
            }
        }
    })
}

/// Read the `kind` answer of a Jev response.
pub fn parse_jev_response(response: &Value) -> Result<Classification> {
    let answer = response
        .pointer("/answers/kind")
        .ok_or_else(|| anyhow!("Jev response has no `kind` answer"))?;
    let choice = answer
        .get("choice")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Jev `kind` answer has no choice"))?;
    let kind = match choice {
        "webhook_stopped" => TicketKind::WebhookStopped,
        "other" => TicketKind::Other,
        unknown => bail!("Jev returned an unknown kind `{unknown}`"),
    };
    let confidence = answer
        .get("confidence")
        .and_then(Value::as_f64)
        .ok_or_else(|| anyhow!("Jev `kind` answer has no confidence"))?;
    if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
        bail!("Jev returned a confidence outside 0 to 1");
    }
    Ok(Classification {
        kind,
        confidence: Some(confidence),
    })
}

pub struct JevClassifier<A: JevApi> {
    api: A,
}

impl<A: JevApi> JevClassifier<A> {
    pub fn new(api: A) -> Self {
        Self { api }
    }
}

impl<A: JevApi> Classifier for JevClassifier<A> {
    fn classify(&self, ticket: &Ticket) -> Result<Classification> {
        parse_jev_response(&self.api.system_one(&jev_request(ticket))?)
    }
}

/// Live Jev call. Reads `TYPESAFE_API_KEY` from the environment and never prints it.
pub struct JevHttp {
    key: String,
}

impl JevHttp {
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("TYPESAFE_API_KEY")
            .map_err(|_| anyhow!("TYPESAFE_API_KEY is not set"))?;
        Ok(Self { key })
    }
}

impl JevApi for JevHttp {
    fn system_one(&self, request: &Value) -> Result<Value> {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(15))
            .build();
        match agent
            .post(JEV_URL)
            .set("Authorization", &format!("Bearer {}", self.key))
            .send_json(request)
        {
            Ok(response) => response.into_json().context("parse the Jev response"),
            Err(ureq::Error::Status(code, _)) => bail!("Jev returned HTTP {code}"),
            Err(ureq::Error::Transport(t)) => bail!("Jev request failed: {:?}", t.kind()),
        }
    }
}

/// Replays a hand-built mocked Jev fixture. A request that is not in the fixture is an error, so the fixture
/// holds every prompt the tests send.
pub struct RecordedJev {
    exchanges: Vec<(Value, Value)>,
}

impl RecordedJev {
    pub fn from_json(v: &Value) -> Result<Self> {
        let exchanges = v
            .get("exchanges")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("Jev fixture has no `exchanges` array"))?
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let part = |k: &str| {
                    e.get(k)
                        .cloned()
                        .ok_or_else(|| anyhow!("Jev fixture exchange {i} has no `{k}`"))
                };
                Ok((part("request")?, part("response")?))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { exchanges })
    }
}

impl JevApi for RecordedJev {
    fn system_one(&self, request: &Value) -> Result<Value> {
        self.exchanges
            .iter()
            .find(|(recorded, _)| recorded == request)
            .map(|(_, response)| response.clone())
            .ok_or_else(|| anyhow!("no recorded Jev exchange matches this request"))
    }
}

/// The flag lives here. With `ai` false the Jev side is never built, so no key is read and no
/// model is reached.
pub fn select_classifier(
    ai: bool,
    jev: impl FnOnce() -> Result<Box<dyn Classifier>>,
) -> Result<Box<dyn Classifier>> {
    if ai {
        jev()
    } else {
        Ok(Box::new(NoModel))
    }
}

/// The `diff --json` output, reduced to what a reply needs. A never-received row is listed as
/// missing only when it did not fail on chain, whatever `--status` the diff ran with. A failed row
/// is reported on its own, because a raw webhook does send failed transactions.
#[derive(Debug, Clone, PartialEq)]
pub struct TriageReport {
    pub reference_tag: Option<String>,
    pub received_tag: Option<String>,
    pub reference_count: u64,
    pub received_count: u64,
    pub lost: Vec<String>,
    pub failed: Vec<String>,
    pub duplicates: usize,
    pub extra: usize,
    pub backfill_count: Option<u64>,
}

impl TriageReport {
    pub fn from_json(v: &Value) -> Result<Self> {
        let rows = |key: &str| {
            v.get(key)
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow!("report has no `{key}` array"))
        };
        let count = |key: &str| {
            v.get(key)
                .and_then(Value::as_u64)
                .ok_or_else(|| anyhow!("report has no `{key}` number"))
        };
        // An unknown tag is dropped, never rendered: it could carry injected text.
        let tag = |key: &str| {
            v.get(key)
                .and_then(Value::as_str)
                .filter(|t| KNOWN_TAGS.contains(t))
                .map(String::from)
        };
        let mut lost = Vec::new();
        let mut failed = Vec::new();
        for key in ["missing", "skipped_by_status"] {
            for row in rows(key)? {
                let signature = row
                    .get("signature")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("a `{key}` row has no signature"))?;
                if !is_signature(signature) {
                    bail!("a `{key}` row has a signature that is not Base58 of 64 bytes");
                }
                let row_failed = row
                    .get("failed")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| anyhow!("a `{key}` row has no `failed` flag"))?;
                if row_failed {
                    failed.push(signature.to_string());
                } else {
                    lost.push(signature.to_string());
                }
            }
        }
        Ok(Self {
            reference_tag: tag("reference_tag"),
            received_tag: tag("received_tag"),
            reference_count: count("reference_count")?,
            received_count: count("received_count")?,
            lost,
            failed,
            duplicates: rows("duplicates")?.len(),
            extra: rows("extra")?.len(),
            backfill_count: v.get("backfill_count").and_then(Value::as_u64),
        })
    }

    /// True only when both sides carry the `run live` tag. A missing, unknown, `mocked` or
    /// `litesvm` tag is never real data, so a gap there is a test of the tool and not a loss.
    pub fn is_real(&self) -> bool {
        [&self.reference_tag, &self.received_tag]
            .into_iter()
            .all(|t| t.as_deref() == Some("run live"))
    }

    pub fn has_known_tags(&self) -> bool {
        self.reference_tag.is_some() && self.received_tag.is_some()
    }
}

/// A transaction signature is Base58 and decodes to 64 bytes.
pub fn is_signature(s: &str) -> bool {
    bs58::decode(s).into_vec().is_ok_and(|b| b.len() == 64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draft {
    Reply(String),
    NeedsHuman(String),
}

/// Classify the ticket. Draft a reply only for a confident webhook-stopped ticket.
pub fn triage(
    ticket: &Ticket,
    report: &TriageReport,
    classifier: &dyn Classifier,
) -> Result<Draft> {
    let c = classifier.classify(ticket)?;
    if c.kind != TicketKind::WebhookStopped {
        return Ok(Draft::NeedsHuman(
            "this is not a webhook delivery ticket".into(),
        ));
    }
    if let Some(conf) = c.confidence.filter(|c| *c < MIN_CONFIDENCE) {
        return Ok(Draft::NeedsHuman(format!(
            "the classifier confidence {conf:.2} is below {MIN_CONFIDENCE:.2}"
        )));
    }
    if !report.has_known_tags() {
        return Ok(Draft::NeedsHuman(
            "the report's data tags are missing or unknown, so a person must check where the data came from".into(),
        ));
    }
    Ok(Draft::Reply(render_reply(report)))
}

fn tag_label(tag: &Option<String>) -> &str {
    tag.as_deref().unwrap_or("unknown")
}

fn noun(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn signature_list(signatures: &[String]) -> String {
    let shown: Vec<String> = signatures
        .iter()
        .take(SIGNATURES_SHOWN)
        .map(|s| format!("`{s}`"))
        .collect();
    let more = signatures.len().saturating_sub(SIGNATURES_SHOWN);
    if more == 0 {
        shown.join(", ")
    } else {
        format!("{}, and {more} more", shown.join(", "))
    }
}

/// Render the reply. Quotes are whole FAQ sentences in straight double quotes, and nothing else
/// in the reply uses double quotes. Unless both tags are `run live`, the opening names no customer
/// endpoint or webhook address, and the reply stops after the set difference: no retry,
/// permanent-loss, endpoint or backfill claim.
pub fn render_reply(report: &TriageReport) -> String {
    let real = report.is_real();
    let mut paras = vec![
        if real {
            "Thanks for the detail. I compared the signatures your endpoint received with Helius's own transaction history for your webhook address."
        } else {
            "Thanks for the detail. I compared the reference and received signature sets in this report."
        }
        .to_string(),
        format!(
            "Data used: reference `{}`, received `{}`.",
            tag_label(&report.reference_tag),
            tag_label(&report.received_tag)
        ),
    ];
    if !real {
        paras.push("This is recorded sample data or data of unknown origin, so any gap below is a test of the tool and not a confirmed loss on your webhook.".to_string());
    }

    if report.lost.is_empty() {
        let none = if report.failed.is_empty() {
            "Nothing is lost in this window."
        } else {
            "No successful transaction is missing in this window."
        };
        paras.push(if real {
            format!("{none} Every successful transaction in the reference is in the set your endpoint logged.")
        } else if report.failed.is_empty() {
            "In this data set nothing is missing. That is not proof that nothing was lost on your webhook.".to_string()
        } else {
            "In this data set no successful transaction is missing. That is not proof that nothing was lost on your webhook.".to_string()
        });
    } else {
        let n = noun(report.lost.len(), "transaction", "transactions");
        if real {
            paras.push(format!(
                "{n} in the reference that the endpoint did not log: {}.",
                signature_list(&report.lost)
            ));
            paras.push(format!(
                "The reference lists the address's successful transactions, not only the ones your webhook matches, so I cannot tell from it whether each one matched your webhook. Before treating them as lost, please confirm the webhook type and its transaction filters. The Helius FAQ ({FAQ_URL}) says: \"{TYPE_QUOTE}\""
            ));
            paras.push(format!(
                "If they do match, the FAQ says: \"{RETRY_QUOTE}\" It also says: \"{LOST_QUOTE}\""
            ));
            paras.push("This report does not show whether Helius attempted delivery or used up its retries. Your endpoint may have returned an error or timed out, your log may have missed these events, or the webhook may have been disabled. Helius disables a webhook on its own when the endpoint failure rate is high, so check in the dashboard that your webhook is still enabled.".to_string());
            paras.push(match report.backfill_count {
                Some(n) if n > 0 => format!(
                    "A backfill file with {n} webhook-shaped payloads is ready to replay into your endpoint."
                ),
                _ => "I can prepare a backfill file in webhook shape for these signatures so you can replay them.".to_string(),
            });
        } else {
            paras.push(format!(
                "In this data set, the reference holds {n} that the received set lacks: {}.",
                signature_list(&report.lost)
            ));
        }
    }

    if report.duplicates > 0 {
        paras.push(format!(
            "{} appeared more than once in the received set. The data does not show why. The FAQ says: \"{DUPLICATE_QUOTE}\" \"{DUPLICATE_CAUSE_QUOTE}\" It also says: \"{DEDUPE_QUOTE}\"",
            noun(report.duplicates, "signature", "signatures")
        ));
    }
    if !report.failed.is_empty() {
        paras.push(format!(
            "Of the never-received rows, {} failed on chain. An enhanced webhook never sends these. A raw webhook does send failed transactions, so for a raw webhook please confirm its filters before treating them as missing. The FAQ says: \"{FAILED_QUOTE}\"",
            report.failed.len()
        ));
    }
    if report.extra > 0 {
        paras.push(format!(
            "The received set holds {} outside the reference window. Widen the reference window before drawing a conclusion.",
            noun(report.extra, "signature", "signatures")
        ));
    }
    paras.join("\n\n") + "\n"
}

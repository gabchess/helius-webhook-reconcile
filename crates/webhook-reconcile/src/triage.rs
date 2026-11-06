//! Triage for a "my webhook stopped" ticket. The diff report goes in, a drafted support reply comes out.
//! A person reviews the draft before it is sent.
//!
//! Data shape:
//! - `Ticket`: the customer's text.
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
pub const DEDUPE_QUOTE: &str = "To handle duplicates, use the transaction signature or a unique event identifier to track which events you've already processed.";
pub const FAILED_QUOTE: &str = "Enhanced webhooks do not send failed transactions, but raw webhooks do include both successful and failed transactions.";

/// A model answer below this confidence is read by a person before any draft is made.
pub const MIN_CONFIDENCE: f64 = 0.6;
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

const STOPPED_WORDS: [&str; 8] = [
    "stopped",
    "missed",
    "missing",
    "not getting",
    "not receiving",
    "no events",
    "dropped",
    "isn't",
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

/// One call to the TypeSafe `systemone` endpoint. The live implementation is `JevHttp`.
/// `RecordedJev` replays a fixture.
pub trait JevApi {
    fn system_one(&self, request: &Value) -> Result<Value>;
}

/// The question set sent to Jev for one ticket. The ticket text sits in its own state field and
/// the question points at it by path.
pub fn jev_request(ticket: &Ticket) -> Value {
    json!({
        "state": {"ticket": {"text": ticket.text}},
        "model": JEV_MODEL,
        "questions": {
            "kind": {
                "type": "choice",
                "instructions": "Which kind of support request is the text in `ticket.text`?",
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

/// Replays recorded exchanges. A request that is not in the fixture is an error, so the fixture
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

/// The `diff --json` output, reduced to what a reply needs. A never-received row counts as lost
/// only when it did not fail on chain, whatever `--status` the diff ran with.
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
        let tag = |key: &str| v.get(key).and_then(Value::as_str).map(String::from);
        let mut lost = Vec::new();
        let mut failed = Vec::new();
        for key in ["missing", "skipped_by_status"] {
            for row in rows(key)? {
                let signature = row
                    .get("signature")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("a `{key}` row has no signature"))?;
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

    /// True when either side is recorded or simulated data, so a gap is a test of the tool and not
    /// a loss on a real webhook. Untagged data is the customer's own.
    pub fn is_recorded(&self) -> bool {
        [&self.reference_tag, &self.received_tag]
            .into_iter()
            .any(|t| matches!(t.as_deref(), Some("mocked") | Some("litesvm")))
    }
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
    Ok(Draft::Reply(render_reply(report)))
}

fn tag_label(tag: &Option<String>) -> &str {
    tag.as_deref().unwrap_or("untagged")
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
/// in the reply uses double quotes.
pub fn render_reply(report: &TriageReport) -> String {
    let recorded = report.is_recorded();
    let mut paras = vec![
        "Thanks for the detail. I compared the signatures your endpoint received with Helius's own transaction history for your webhook address.".to_string(),
        format!(
            "Data used: reference `{}`, received `{}`.",
            tag_label(&report.reference_tag),
            tag_label(&report.received_tag)
        ),
    ];
    if recorded {
        paras.push("This is recorded sample data, so any gap below is a test of the tool and not a confirmed loss on your webhook.".to_string());
    }

    if report.lost.is_empty() {
        paras.push(if recorded {
            "In this data set nothing is missing. That is not proof that nothing was lost on your webhook.".to_string()
        } else {
            "Nothing is lost in this window. Every successful transaction in the reference reached your endpoint.".to_string()
        });
    } else {
        let n = noun(report.lost.len(), "transaction", "transactions");
        let lead = if recorded {
            format!(
                "In this data set, the reference holds {n} that the received set lacks: {}.",
                signature_list(&report.lost)
            )
        } else {
            format!(
                "{n} in the reference never reached your endpoint: {}.",
                signature_list(&report.lost)
            )
        };
        paras.push(lead);
        paras.push(format!(
            "The Helius FAQ ({FAQ_URL}) says: \"{RETRY_QUOTE}\" It also says: \"{LOST_QUOTE}\""
        ));
        paras.push("That means these events will not arrive again by themselves. Your endpoint likely returned an error or timed out while they were sent. Helius also disables a webhook on its own when the endpoint failure rate is high, so check in the dashboard that your webhook is still enabled.".to_string());
        paras.push(match report.backfill_count {
            Some(n) if n > 0 => format!(
                "A backfill file with {n} webhook-shaped payloads is ready to replay into your endpoint."
            ),
            _ => "I can prepare a backfill file in webhook shape for these signatures so you can replay them.".to_string(),
        });
    }

    if report.duplicates > 0 {
        paras.push(format!(
            "{} arrived more than once. That comes from retries and is not a bug. The FAQ says: \"{DUPLICATE_QUOTE}\" It also says: \"{DEDUPE_QUOTE}\"",
            noun(report.duplicates, "signature", "signatures")
        ));
    }
    if !report.failed.is_empty() {
        paras.push(format!(
            "Of the never-received rows, {} failed on chain. They are not lost events. The FAQ says: \"{FAILED_QUOTE}\"",
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

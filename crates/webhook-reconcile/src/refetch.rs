//! Refetch only the missing signatures in full mode. The fetch sits behind `TxSource`, so tests replay
//! recorded responses with no key and no network, and the live path is one small struct.
use crate::{parse_ref_row, types::*};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

/// Where full transactions come from. One call may ask for many signatures.
/// An implementation returns one entry per requested signature, with `logs` set, or an error.
pub trait TxSource {
    /// Fetch the given signatures in full mode.
    fn fetch_full(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>>;
}

/// Turn a JSON-RPC batch response of `getTransaction` calls into reference entries.
/// Request `id`s are the indexes into `requested`. Every requested signature must come back with
/// logs. A JSON-RPC error, a null result (transaction not found) or an unrequested signature fails.
pub fn parse_batch_response(requested: &[Signature], batch: &Value) -> Result<Vec<RefEntry>> {
    let items = batch
        .as_array()
        .ok_or_else(|| anyhow!("batch response is not a JSON array"))?;
    let mut by_sig: HashMap<&Signature, RefEntry> = HashMap::new();
    for item in items {
        let idx = item
            .get("id")
            .and_then(|i| {
                i.as_str()
                    .map(String::from)
                    .or_else(|| i.as_u64().map(|n| n.to_string()))
            })
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|i| *i < requested.len())
            .ok_or_else(|| anyhow!("batch item has no valid id"))?;
        let want = &requested[idx];
        if let Some(err) = item.get("error") {
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("?");
            bail!("getTransaction failed for {}: {msg}", want.0);
        }
        let result = item
            .get("result")
            .filter(|r| !r.is_null())
            .ok_or_else(|| anyhow!("transaction {} not found", want.0))?;
        let got = result
            .pointer("/transaction/signatures/0")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("result for {} has no signature", want.0))?;
        if got != want.0 {
            bail!("asked for {}, got {got}", want.0);
        }
        let entry = parse_ref_row(result, want.clone());
        if entry.logs.is_none() {
            bail!("result for {} has no meta.logMessages", want.0);
        }
        by_sig.insert(want, entry);
    }
    requested
        .iter()
        .map(|s| {
            by_sig
                .remove(s)
                .ok_or_else(|| anyhow!("no response for {}", s.0))
        })
        .collect()
}

/// Give every missing row without logs its logs, fetching only those signatures in one call.
/// Rows that already have logs are kept and not fetched again. Makes no call when nothing is needed.
pub fn refetch_missing(missing: &[RefEntry], source: &dyn TxSource) -> Result<Vec<RefEntry>> {
    let wanted: Vec<Signature> = missing
        .iter()
        .filter(|m| m.logs.is_none())
        .map(|m| m.signature.clone())
        .collect();
    if wanted.is_empty() {
        return Ok(missing.to_vec());
    }
    let fetched = source.fetch_full(&wanted)?;
    let wanted_set: HashSet<&Signature> = wanted.iter().collect();
    let mut logs: HashMap<Signature, Vec<String>> = HashMap::new();
    for f in fetched {
        if !wanted_set.contains(&f.signature) {
            bail!("source returned {} which was not requested", f.signature.0);
        }
        let l = f
            .logs
            .ok_or_else(|| anyhow!("source returned {} without logs", f.signature.0))?;
        logs.insert(f.signature, l);
    }
    missing
        .iter()
        .map(|m| {
            let mut e = m.clone();
            if e.logs.is_none() {
                e.logs = Some(
                    logs.remove(&m.signature)
                        .ok_or_else(|| anyhow!("source did not return {}", m.signature.0))?,
                );
            }
            Ok(e)
        })
        .collect()
}

/// Replays recorded `getTransaction` results, keyed by signature. Records every request, so a test
/// can prove that only the missing signatures were asked for.
pub struct RecordedSource {
    results: HashMap<Signature, Value>,
    calls: RefCell<Vec<Vec<Signature>>>,
}

impl RecordedSource {
    /// Build from a saved file of the shape `{"responses": {"<signature>": <getTransaction result>}}`.
    pub fn from_json(v: &Value) -> Result<Self> {
        let map = v
            .get("responses")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("recorded refetch file has no `responses` object"))?;
        Ok(Self {
            results: map
                .iter()
                .map(|(k, r)| (Signature(k.clone()), r.clone()))
                .collect(),
            calls: RefCell::new(Vec::new()),
        })
    }

    /// Every request so far, one list of signatures per call.
    pub fn calls(&self) -> Vec<Vec<Signature>> {
        self.calls.borrow().clone()
    }
}

impl TxSource for RecordedSource {
    fn fetch_full(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>> {
        self.calls.borrow_mut().push(signatures.to_vec());
        let batch: Vec<Value> = signatures
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({"jsonrpc": "2.0", "id": i.to_string(),
                       "result": self.results.get(s).cloned().unwrap_or(Value::Null)})
            })
            .collect();
        parse_batch_response(signatures, &Value::Array(batch))
    }
}

/// Live Helius JSON-RPC source. It sends `getTransaction` in batches. The API key lives only in
/// the request URL held in memory. Debug output never contains it, and every error leaves
/// `fetch_full` through `redact`, the one place that removes the key and the URL.
pub struct HeliusRpc {
    url: String,
    key: String,
}

const BATCH_SIZE: usize = 100;

impl HeliusRpc {
    /// Read `HELIUS_API_KEY` from the environment. The error names the variable, never its value.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("HELIUS_API_KEY")
            .ok()
            .filter(|k| !k.is_empty())
            .ok_or_else(|| anyhow!("HELIUS_API_KEY is not set"))?;
        Ok(Self::new("https://mainnet.helius-rpc.com", &key))
    }

    /// Build with an explicit base URL and key.
    pub fn new(base: &str, key: &str) -> Self {
        Self {
            url: format!("{base}/?api-key={key}"),
            key: key.to_string(),
        }
    }

    fn redact(&self, text: &str) -> String {
        text.replace(&self.url, "<redacted-url>")
            .replace(&self.key, "<redacted>")
    }

    fn post(&self, body: &Value) -> Result<Value> {
        let resp = ureq::post(&self.url)
            .timeout(Duration::from_secs(30))
            .send_json(body.clone())
            .map_err(|e| match e {
                ureq::Error::Status(code, resp) => {
                    let body = resp.into_string().unwrap_or_default();
                    let body: String = body.chars().take(200).collect();
                    anyhow!("Helius RPC returned HTTP {code}: {body}")
                }
                ureq::Error::Transport(t) => anyhow!("Helius RPC transport error: {t}"),
            })?;
        resp.into_json::<Value>()
            .context("read Helius RPC response")
    }

    fn fetch_batches(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>> {
        let mut out = Vec::new();
        for chunk in signatures.chunks(BATCH_SIZE) {
            let body: Vec<Value> = chunk
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    json!({"jsonrpc": "2.0", "id": i.to_string(), "method": "getTransaction",
                           "params": [s.0, {"encoding": "json", "commitment": "finalized",
                                            "maxSupportedTransactionVersion": 1}]})
                })
                .collect();
            let resp = self.post(&Value::Array(body))?;
            out.extend(parse_batch_response(chunk, &resp)?);
        }
        Ok(out)
    }
}

impl fmt::Debug for HeliusRpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeliusRpc")
            .field("key", &"<redacted>")
            .finish()
    }
}

impl TxSource for HeliusRpc {
    fn fetch_full(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>> {
        self.fetch_batches(signatures)
            .map_err(|e| anyhow!("{}", self.redact(&format!("{e:#}"))))
    }
}

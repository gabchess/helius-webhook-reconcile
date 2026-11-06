//! Refetch only the missing signatures in full mode. The fetch sits behind `TxSource`, so tests replay
//! recorded responses with no key and no network, and the live path is one small struct.
use crate::paging::{page_request_body, parse_page_response, Page, PageRequest, ReferenceSource};
use crate::{parse_ref_row, types::*};
use anyhow::{anyhow, bail, Result};
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

    /// Fetch the given signatures as full transaction results, one per signature, in request order.
    /// A source that cannot return them keeps this default, which fails.
    fn fetch_raw(&self, _signatures: &[Signature]) -> Result<Vec<Value>> {
        bail!("this source cannot return raw transactions")
    }
}

/// Why a live fetch or a batch parse failed. Every field is a number, a fixed label, an error
/// class or a signature we sent. Nothing here holds text from the server or from the request URL,
/// so no error message can carry the API key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcError {
    /// The request did not complete. Only the ureq error class is kept.
    Transport { kind: ureq::ErrorKind },
    /// The server answered with a non-success HTTP status. The body is dropped.
    Status { code: u16 },
    /// The server answered a request with a JSON-RPC error. The message is dropped.
    JsonRpc {
        code: Option<i64>,
        signature: Signature,
    },
    /// The requested transaction came back as null.
    NotFound { signature: Signature },
    /// A result for a requested signature was unusable.
    BadResult {
        what: &'static str,
        signature: Signature,
    },
    /// The response did not have the expected shape.
    Decode { what: &'static str },
    /// The server answered a getTransactionsForAddress call with a JSON-RPC error. The message is dropped.
    JsonRpcMethod { code: Option<i64> },
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transport { kind } => write!(f, "Helius RPC transport error: {kind:?}"),
            Self::Status { code } => write!(f, "Helius RPC returned HTTP {code}"),
            Self::JsonRpc {
                code: Some(code),
                signature,
            } => write!(
                f,
                "getTransaction failed for {} with code {code}",
                signature.0
            ),
            Self::JsonRpc {
                code: None,
                signature,
            } => write!(f, "getTransaction failed for {} with no code", signature.0),
            Self::NotFound { signature } => write!(f, "transaction {} not found", signature.0),
            Self::BadResult { what, signature } => write!(f, "result for {}: {what}", signature.0),
            Self::Decode { what } => write!(f, "Helius RPC response: {what}"),
            Self::JsonRpcMethod { code: Some(code) } => {
                write!(f, "getTransactionsForAddress failed with code {code}")
            }
            Self::JsonRpcMethod { code: None } => {
                write!(f, "getTransactionsForAddress failed with no code")
            }
        }
    }
}

impl std::error::Error for RpcError {}

/// Turn a JSON-RPC batch response of `getTransaction` calls into reference entries.
/// Request `id`s are the indexes into `requested`. Every requested signature must come back with
/// logs. A JSON-RPC error, a null result (transaction not found) or an unrequested signature fails.
pub fn parse_batch_response(
    requested: &[Signature],
    batch: &Value,
) -> Result<Vec<RefEntry>, RpcError> {
    Ok(parse_batch(requested, batch)?
        .into_iter()
        .map(|(entry, _)| entry)
        .collect())
}

/// The same validation as `parse_batch_response`, returning the transaction results as received.
pub fn parse_batch_raw(requested: &[Signature], batch: &Value) -> Result<Vec<Value>, RpcError> {
    Ok(parse_batch(requested, batch)?
        .into_iter()
        .map(|(_, raw)| raw)
        .collect())
}

fn parse_batch(requested: &[Signature], batch: &Value) -> Result<Vec<(RefEntry, Value)>, RpcError> {
    let items = batch.as_array().ok_or(RpcError::Decode {
        what: "batch is not a JSON array",
    })?;
    let mut by_sig: HashMap<&Signature, (RefEntry, Value)> = HashMap::new();
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
            .ok_or(RpcError::Decode {
                what: "batch item has no valid id",
            })?;
        let want = &requested[idx];
        if let Some(err) = item.get("error") {
            return Err(RpcError::JsonRpc {
                code: err.get("code").and_then(Value::as_i64),
                signature: want.clone(),
            });
        }
        let bad = |what| RpcError::BadResult {
            what,
            signature: want.clone(),
        };
        let result =
            item.get("result")
                .filter(|r| !r.is_null())
                .ok_or_else(|| RpcError::NotFound {
                    signature: want.clone(),
                })?;
        let got = result
            .pointer("/transaction/signatures/0")
            .and_then(Value::as_str)
            .ok_or_else(|| bad("no signature"))?;
        if got != want.0 {
            return Err(bad("signature differs from the one requested"));
        }
        let entry = parse_ref_row(result, want.clone());
        if entry.logs.is_none() {
            return Err(bad("no meta.logMessages"));
        }
        by_sig.insert(want, (entry, result.clone()));
    }
    requested
        .iter()
        .map(|s| {
            by_sig.remove(s).ok_or_else(|| RpcError::BadResult {
                what: "no response",
                signature: s.clone(),
            })
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

/// Fetch the full transaction of every missing row in one call, in the order of `missing`.
/// Makes no call when nothing is missing. A result whose signature differs from the row it answers fails.
pub fn raw_for_missing(missing: &[RefEntry], source: &dyn TxSource) -> Result<Vec<Value>> {
    if missing.is_empty() {
        return Ok(Vec::new());
    }
    let wanted: Vec<Signature> = missing.iter().map(|m| m.signature.clone()).collect();
    let raws = source.fetch_raw(&wanted)?;
    if raws.len() != wanted.len() {
        bail!(
            "source returned {} transactions for {} signatures",
            raws.len(),
            wanted.len()
        );
    }
    for (want, raw) in wanted.iter().zip(&raws) {
        let got = raw
            .pointer("/transaction/signatures/0")
            .and_then(Value::as_str);
        if got != Some(want.0.as_str()) {
            bail!("source returned a transaction that is not {}", want.0);
        }
    }
    Ok(raws)
}

/// Give each missing row the logs found in its full transaction. `raws` must hold one result per row,
/// in the same order, as `raw_for_missing` returns them.
pub fn with_logs_from_raw(missing: &[RefEntry], raws: &[Value]) -> Result<Vec<RefEntry>> {
    missing
        .iter()
        .zip(raws)
        .map(|(m, raw)| {
            let logs = parse_ref_row(raw, m.signature.clone())
                .logs
                .ok_or_else(|| anyhow!("transaction {} has no meta.logMessages", m.signature.0))?;
            Ok(RefEntry {
                logs: Some(logs),
                ..m.clone()
            })
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

impl RecordedSource {
    fn batch_for(&self, signatures: &[Signature]) -> Value {
        self.calls.borrow_mut().push(signatures.to_vec());
        Value::Array(
            signatures
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    json!({"jsonrpc": "2.0", "id": i.to_string(),
                           "result": self.results.get(s).cloned().unwrap_or(Value::Null)})
                })
                .collect(),
        )
    }
}

impl TxSource for RecordedSource {
    fn fetch_full(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>> {
        Ok(parse_batch_response(
            signatures,
            &self.batch_for(signatures),
        )?)
    }

    fn fetch_raw(&self, signatures: &[Signature]) -> Result<Vec<Value>> {
        Ok(parse_batch_raw(signatures, &self.batch_for(signatures))?)
    }
}

/// Live Helius JSON-RPC source. It sends `getTransaction` in batches. The API key lives only in
/// the request URL held in memory. Every error leaves as an `RpcError`, which holds no text from
/// the server or the URL, so there is nothing to redact.
pub struct HeliusRpc {
    url: String,
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
        }
    }

    fn post(&self, body: &Value) -> Result<Value, RpcError> {
        let resp = ureq::post(&self.url)
            .timeout(Duration::from_secs(30))
            .send_json(body.clone())
            .map_err(|e| match e {
                ureq::Error::Status(code, _) => RpcError::Status { code },
                ureq::Error::Transport(t) => RpcError::Transport { kind: t.kind() },
            })?;
        resp.into_json::<Value>().map_err(|_| RpcError::Decode {
            what: "body is not valid JSON",
        })
    }

    fn fetch_batches(&self, signatures: &[Signature]) -> Result<Vec<Value>, RpcError> {
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
            out.extend(parse_batch_raw(chunk, &resp)?);
        }
        Ok(out)
    }
}

impl fmt::Debug for HeliusRpc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeliusRpc")
            .field("url", &"<redacted>")
            .finish()
    }
}

impl TxSource for HeliusRpc {
    fn fetch_full(&self, signatures: &[Signature]) -> Result<Vec<RefEntry>> {
        let raws = self.fetch_batches(signatures)?;
        Ok(signatures
            .iter()
            .zip(&raws)
            .map(|(s, raw)| parse_ref_row(raw, s.clone()))
            .collect())
    }

    fn fetch_raw(&self, signatures: &[Signature]) -> Result<Vec<Value>> {
        Ok(self.fetch_batches(signatures)?)
    }
}

impl ReferenceSource for HeliusRpc {
    fn fetch_page(&self, req: &PageRequest) -> Result<Page> {
        let resp = self.post(&page_request_body(req))?;
        Ok(parse_page_response(&resp)?)
    }
}

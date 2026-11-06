//! The Helius HTTP client. One struct serves both reads: full transactions (`TxSource`) and
//! reference pages (`ReferenceSource`). Every error leaves as an `RpcError`.
use crate::reference::{
    page_request_body, parse_page_response, parse_ref_row, Page, PageRequest, RefEntry,
    ReferenceSource, Signature,
};
use crate::refetch::{parse_batch_raw, TxSource};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::fmt;
use std::time::Duration;

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

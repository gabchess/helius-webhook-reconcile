//! The Helius reference set: parse a saved file, or read a whole outage window one
//! `getTransactionsForAddress` page at a time. The fetch sits behind `ReferenceSource`, so tests replay
//! recorded multi-page responses with no key and no network. The loop is the only place that follows `paginationToken`.
use crate::helius::RpcError;
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

/// A base58 transaction signature. Kept as a string because the diff only needs equality.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub struct Signature(pub String);

/// One row of the Helius reference set (getTransactionsForAddress).
/// `failed` is true when the row carries a non-null `err`.
/// `logs` is `None` in signatures mode, which carries no logs, and `Some` in full mode.
/// Decoding events needs `Some`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefEntry {
    pub signature: Signature,
    pub slot: Option<u64>,
    pub failed: bool,
    pub logs: Option<Vec<String>>,
}

/// What Helius says happened. `tag` records where the data came from ("run live", "mocked", "litesvm").
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceSet {
    pub tag: Option<String>,
    pub entries: Vec<RefEntry>,
}

/// Find the array of rows in a saved file: a bare array, or nested under data/result/response/payloads/events.
pub(crate) fn find_rows(v: &Value) -> Option<&Vec<Value>> {
    if let Value::Array(a) = v {
        return Some(a);
    }
    for k in ["data", "result", "response", "payloads", "events"] {
        if let Some(a) = v.get(k).and_then(find_rows) {
            return Some(a);
        }
    }
    None
}

/// Signature of one row. Handles getTransactionsForAddress in signatures mode ({signature}) and
/// full mode ({transaction:{signatures:[..]}}), Helius enhanced webhooks ({signature}), and plain strings.
pub(crate) fn row_signature(row: &Value) -> Option<Signature> {
    if let Some(s) = row.as_str() {
        return Some(Signature(s.to_string()));
    }
    if let Some(s) = row.get("signature").and_then(Value::as_str) {
        return Some(Signature(s.to_string()));
    }
    row.pointer("/transaction/signatures/0")
        .and_then(Value::as_str)
        .map(|s| Signature(s.to_string()))
}

pub(crate) fn tag_of(v: &Value) -> Option<String> {
    v.get("tag").and_then(Value::as_str).map(String::from)
}

/// Build one reference entry from a signatures-mode row or a full-mode row.
pub(crate) fn parse_ref_row(row: &Value, signature: Signature) -> RefEntry {
    // signatures mode puts `err` on the row, full mode puts it under `meta`
    let failed = [row.get("err"), row.pointer("/meta/err")]
        .into_iter()
        .flatten()
        .any(|e| !e.is_null());
    let logs = row
        .pointer("/meta/logMessages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|l| l.as_str().map(String::from))
                .collect()
        });
    RefEntry {
        signature,
        slot: row.get("slot").and_then(Value::as_u64),
        failed,
        logs,
    }
}

/// Keep the first row of each signature. Overlapping pages and concatenated saved files repeat
/// rows, and a repeated row would be counted as missing twice.
pub fn dedupe_by_signature(entries: Vec<RefEntry>) -> Vec<RefEntry> {
    let mut seen = HashSet::new();
    entries
        .into_iter()
        .filter(|e| seen.insert(e.signature.clone()))
        .collect()
}

/// Parse a saved Helius reference file into signatures, slots and a `failed` flag per row.
/// A signature that appears in more than one row is kept once.
pub fn parse_reference(v: &Value) -> Result<ReferenceSet> {
    let rows = find_rows(v).ok_or_else(|| anyhow!("no array of rows found in reference file"))?;
    let mut entries = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let signature =
            row_signature(row).ok_or_else(|| anyhow!("reference row {i} has no signature"))?;
        entries.push(parse_ref_row(row, signature));
    }
    Ok(ReferenceSet {
        tag: tag_of(v),
        entries: dedupe_by_signature(entries),
    })
}

/// The outage window. Each bound is optional and inclusive, as `gte` and `lte` in the
/// `filters.slot` and `filters.blockTime` of getTransactionsForAddress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub slot_gte: Option<u64>,
    pub slot_lte: Option<u64>,
    pub time_gte: Option<u64>,
    pub time_lte: Option<u64>,
}

/// An opaque `paginationToken` ("slot:position"). The value never appears in `Debug` output or in
/// any error text, so it cannot end up in a log.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PageToken(String);

impl PageToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for PageToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PageToken(<redacted>)")
    }
}

/// One page request. The window and the limit stay the same for every page of one run, and only
/// `token` changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    pub address: String,
    pub window: Window,
    /// Rows per page, 1 to 1000.
    pub limit: u32,
    /// Where to continue. `None` asks for the first page.
    pub token: Option<PageToken>,
}

/// One page of signatures-mode rows and the token for the next page. `next` is `None` on the last page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub rows: Vec<Value>,
    pub next: Option<PageToken>,
}

pub trait ReferenceSource {
    fn fetch_page(&self, req: &PageRequest) -> Result<Page>;
}

/// Why paging stopped before the window was read in full. Fields are counts only.
/// Reading on would give a reference set that is short or never ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagingError {
    /// The server returned a token it had already returned, so the loop would never end.
    TokenRepeated {
        /// Page whose response repeated a token, counting from 1.
        page: usize,
    },
    /// The page cap was reached while the server still offered a next page.
    PageCapReached {
        /// The cap that was set.
        max_pages: usize,
    },
}

impl fmt::Display for PagingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenRepeated { page } => write!(
                f,
                "page {page} returned a paginationToken that an earlier page already returned, \
                 so paging would never end"
            ),
            Self::PageCapReached { max_pages } => write!(
                f,
                "stopped after {max_pages} pages and the server still has more. The reference is \
                 incomplete. Raise --max-pages or narrow the window"
            ),
        }
    }
}

impl std::error::Error for PagingError {}

/// Build the JSON-RPC body for one page. Signatures mode, ascending, `status: any`, so failed
/// transactions are present and the diff's `--status` filter decides what counts.
pub fn page_request_body(req: &PageRequest) -> Value {
    let mut filters = Map::new();
    filters.insert("status".into(), json!("any"));
    for (name, gte, lte) in [
        ("slot", req.window.slot_gte, req.window.slot_lte),
        ("blockTime", req.window.time_gte, req.window.time_lte),
    ] {
        let mut bounds = Map::new();
        if let Some(v) = gte {
            bounds.insert("gte".into(), json!(v));
        }
        if let Some(v) = lte {
            bounds.insert("lte".into(), json!(v));
        }
        if !bounds.is_empty() {
            filters.insert(name.into(), Value::Object(bounds));
        }
    }
    let mut options = json!({
        "transactionDetails": "signatures",
        "sortOrder": "asc",
        "commitment": "finalized",
        "limit": req.limit,
        "filters": filters,
    });
    if let Some(token) = &req.token {
        options["paginationToken"] = json!(token.as_str());
    }
    json!({"jsonrpc": "2.0", "id": "1", "method": "getTransactionsForAddress",
           "params": [req.address, options]})
}

/// Turn one JSON-RPC response of getTransactionsForAddress into a `Page`.
/// A JSON-RPC error drops its message. `paginationToken` must be a string or null.
pub fn parse_page_response(resp: &Value) -> Result<Page, RpcError> {
    if let Some(err) = resp.get("error") {
        return Err(RpcError::JsonRpcMethod {
            code: err.get("code").and_then(Value::as_i64),
        });
    }
    let result = resp.get("result").ok_or(RpcError::Decode {
        what: "response has no result",
    })?;
    let rows = result
        .get("data")
        .and_then(Value::as_array)
        .ok_or(RpcError::Decode {
            what: "result has no data array",
        })?
        .clone();
    let next = match result.get("paginationToken") {
        None | Some(Value::Null) => None,
        Some(Value::String(t)) => Some(PageToken::new(t.clone())),
        Some(_) => {
            return Err(RpcError::Decode {
                what: "paginationToken is not a string or null",
            })
        }
    };
    Ok(Page { rows, next })
}

/// Read every page of the window and return one entry per signature. Stops when a response has no
/// token. Fails loudly when a token repeats or when `max_pages` pages were read and more remain.
/// A signature that appears on two pages is kept once, in the position of its first appearance.
pub fn fetch_reference(
    source: &dyn ReferenceSource,
    address: &str,
    window: &Window,
    limit: u32,
    max_pages: usize,
) -> Result<Vec<RefEntry>> {
    let mut entries = Vec::new();
    let mut returned: HashSet<PageToken> = HashSet::new();
    let mut token: Option<PageToken> = None;
    for page_no in 1..=max_pages {
        let page = source.fetch_page(&PageRequest {
            address: address.to_string(),
            window: window.clone(),
            limit,
            token: token.take(),
        })?;
        for (i, row) in page.rows.iter().enumerate() {
            let signature = row_signature(row)
                .ok_or_else(|| anyhow!("page {page_no} row {i} has no signature"))?;
            entries.push(parse_ref_row(row, signature));
        }
        let Some(next) = page.next else {
            return Ok(dedupe_by_signature(entries));
        };
        if !returned.insert(next.clone()) {
            return Err(PagingError::TokenRepeated { page: page_no }.into());
        }
        token = Some(next);
    }
    Err(PagingError::PageCapReached { max_pages }.into())
}

/// Replays recorded pages, looked up by the token each request carries. Records every request, so a
/// test can prove the window and the token chain.
pub struct RecordedPages {
    pages: Vec<(Option<String>, Value)>,
    requests: RefCell<Vec<PageRequest>>,
}

impl RecordedPages {
    /// Build from `{"pages": [{"after": <token or null>, "result": <getTransactionsForAddress result>}]}`.
    pub fn from_json(v: &Value) -> Result<Self> {
        let pages = v
            .get("pages")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("recorded pages file has no `pages` array"))?;
        let mut out = Vec::new();
        for (i, p) in pages.iter().enumerate() {
            let after = match p.get("after") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => Some(s.clone()),
                Some(_) => bail!("recorded page {i} has an `after` that is not a string or null"),
            };
            let result = p
                .get("result")
                .ok_or_else(|| anyhow!("recorded page {i} has no `result`"))?;
            out.push((after, result.clone()));
        }
        Ok(Self {
            pages: out,
            requests: RefCell::new(Vec::new()),
        })
    }

    pub fn requests(&self) -> Vec<PageRequest> {
        self.requests.borrow().clone()
    }
}

impl ReferenceSource for RecordedPages {
    fn fetch_page(&self, req: &PageRequest) -> Result<Page> {
        self.requests.borrow_mut().push(req.clone());
        let want = req.token.as_ref().map(PageToken::as_str);
        let (_, result) = self
            .pages
            .iter()
            .find(|(after, _)| after.as_deref() == want)
            .ok_or_else(|| {
                anyhow!(
                    "recorded pages have no page for request number {}",
                    self.requests.borrow().len()
                )
            })?;
        Ok(parse_page_response(&json!({"result": result}))?)
    }
}

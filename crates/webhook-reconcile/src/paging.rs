//! Read the reference set for a whole outage window, one `getTransactionsForAddress` page at a time.
//! The fetch sits behind `ReferenceSource`, so tests replay recorded multi-page responses with no key
//! and no network. The loop is the only place that follows `paginationToken`.
use crate::refetch::RpcError;
use crate::{dedupe_by_signature, parse_ref_row, row_signature, types::RefEntry};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;

/// The outage window. Each bound is optional and inclusive, as `gte` and `lte` in the
/// `filters.slot` and `filters.blockTime` of getTransactionsForAddress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    /// First slot to include.
    pub slot_gte: Option<u64>,
    /// Last slot to include.
    pub slot_lte: Option<u64>,
    /// First block time (Unix seconds) to include.
    pub time_gte: Option<u64>,
    /// Last block time (Unix seconds) to include.
    pub time_lte: Option<u64>,
}

/// An opaque `paginationToken` ("slot:position"). The value never appears in `Debug` output or in
/// any error text, so it cannot end up in a log.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PageToken(String);

impl PageToken {
    /// Wrap a token string from a response.
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token text, for the next request.
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
    /// Address whose history is read.
    pub address: String,
    /// The outage window.
    pub window: Window,
    /// Rows per page, 1 to 1000.
    pub limit: u32,
    /// Where to continue. `None` asks for the first page.
    pub token: Option<PageToken>,
}

/// One page of signatures-mode rows and the token for the next page. `next` is `None` on the last page.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// Rows exactly as the server returned them.
    pub rows: Vec<Value>,
    /// `paginationToken` of the response.
    pub next: Option<PageToken>,
}

/// Where reference pages come from.
pub trait ReferenceSource {
    /// Fetch the page the request names.
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

    /// Every request so far, in order.
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

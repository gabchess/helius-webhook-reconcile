//! Diff received signatures against a Helius reference set, refetch the missing ones, decode their Anchor events.
pub mod backfill;
pub mod decode;
pub mod diff;
pub mod helius;
pub mod reference;
pub mod refetch;
pub mod report;

pub use backfill::webhook_payload;
pub use decode::{decode_missing, program_data_lines, DecodeError, DecodedEvent, Idl};
pub use diff::{diff, parse_received, DiffReport, ReceivedSet, Status};
pub use helius::{HeliusRpc, RpcError};
pub use reference::{
    dedupe_by_signature, fetch_reference, parse_reference, PageRequest, PageToken, PagingError,
    RecordedPages, RefEntry, ReferenceSet, ReferenceSource, Signature, Window,
};
pub use refetch::{raw_for_missing, refetch_missing, with_logs_from_raw, RecordedSource, TxSource};

use anyhow::{Context, Result};
use serde_json::Value;
use std::{fs, path::Path};

/// Read a file and parse it as JSON. Both errors name the path.
pub fn load_json(p: &Path) -> Result<Value> {
    serde_json::from_str(&fs::read_to_string(p).with_context(|| format!("read {}", p.display()))?)
        .with_context(|| format!("parse {}", p.display()))
}

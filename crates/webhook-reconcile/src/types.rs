//! Data shapes. Everything else in this crate is a function between these types.
use clap::ValueEnum;
use serde::Serialize;
use serde_json::{Map, Value};

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

/// What the webhook endpoint received. Order and repeats are kept, so retries show up as duplicates.
#[derive(Debug, Clone, Serialize)]
pub struct ReceivedSet {
    pub tag: Option<String>,
    pub signatures: Vec<Signature>,
}

/// Which reference rows may count as missing. The names match `filters.status` of
/// getTransactionsForAddress. Enhanced webhooks never send failed transactions, so
/// `Succeeded` is the default and `Any` is the opt-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Succeeded,
    Failed,
    Any,
}

impl Status {
    /// True when a reference row with this failure state is counted under the chosen status.
    pub fn admits(self, failed: bool) -> bool {
        match self {
            Status::Succeeded => !failed,
            Status::Failed => failed,
            Status::Any => true,
        }
    }
}

/// The result of one diff.
/// - `missing`: in the reference, never received, and admitted by the status filter.
/// - `skipped_by_status`: in the reference, never received, but removed by the status filter.
/// - `duplicates`: received more than once, with the count.
/// - `extra`: received but absent from the whole reference (wrong address, other cluster, or a reference window that is too short).
#[derive(Debug, Clone, Serialize)]
pub struct DiffReport {
    pub status: Status,
    pub missing: Vec<RefEntry>,
    pub skipped_by_status: Vec<RefEntry>,
    pub duplicates: Vec<(Signature, usize)>,
    pub extra: Vec<Signature>,
}

/// An Anchor event decoded from a `Program data:` log line through the IDL.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecodedEvent {
    pub signature: Signature,
    pub event: String,
    pub fields: Map<String, Value>,
}

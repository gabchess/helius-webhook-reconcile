//! Compare the signatures a webhook received with the reference set, and parse the received file.
use crate::reference::{find_rows, row_signature, tag_of, RefEntry, ReferenceSet, Signature};
use anyhow::{anyhow, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};

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

/// Parse a saved set of received webhook payloads into the signatures the consumer saw.
pub fn parse_received(v: &Value) -> Result<ReceivedSet> {
    let rows =
        find_rows(v).ok_or_else(|| anyhow!("no array of payloads found in received file"))?;
    let mut signatures = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        signatures
            .push(row_signature(row).ok_or_else(|| anyhow!("received row {i} has no signature"))?);
    }
    Ok(ReceivedSet {
        tag: tag_of(v),
        signatures,
    })
}

/// Rows the reference has and the webhook never received, split by `status`. A failed row that was
/// received still counts as seen, so it never shows up as `extra` and never as missing.
pub fn diff(reference: &ReferenceSet, received: &ReceivedSet, status: Status) -> DiffReport {
    let mut counts: HashMap<&Signature, usize> = HashMap::new();
    for s in &received.signatures {
        *counts.entry(s).or_default() += 1;
    }
    let ref_set: HashSet<&Signature> = reference.entries.iter().map(|e| &e.signature).collect();
    let (missing, skipped_by_status) = reference
        .entries
        .iter()
        .filter(|e| !counts.contains_key(&e.signature))
        .cloned()
        .partition(|e| status.admits(e.failed));
    let mut seen = HashSet::new();
    let mut duplicates = Vec::new();
    let mut extra = Vec::new();
    for s in &received.signatures {
        if !seen.insert(s) {
            continue;
        }
        if counts[s] > 1 {
            duplicates.push((s.clone(), counts[s]));
        }
        if !ref_set.contains(s) {
            extra.push(s.clone());
        }
    }
    DiffReport {
        status,
        missing,
        skipped_by_status,
        duplicates,
        extra,
    }
}

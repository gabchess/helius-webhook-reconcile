//! Diff the signatures a webhook consumer received against a Helius reference set, refetch only the
//! missing transactions in full mode, and decode their Anchor events through the IDL.
//! The diff and the decoder do no network I/O. The refetch sits behind the `TxSource` trait.
pub mod decode;
pub mod refetch;
pub mod types;

pub use decode::{decode_missing, program_data_lines, DecodeError, Idl};
pub use refetch::{refetch_missing, HeliusRpc, RecordedSource, TxSource};

use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use types::*;

/// Find the array of rows in a saved file: a bare array, or nested under data/result/response/payloads/events.
fn find_rows(v: &Value) -> Option<&Vec<Value>> {
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
fn row_signature(row: &Value) -> Option<Signature> {
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

fn tag_of(v: &Value) -> Option<String> {
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

/// Parse a saved Helius reference file into signatures, slots and a `failed` flag per row.
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
        entries,
    })
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

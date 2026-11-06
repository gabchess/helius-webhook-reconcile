//! Shape missing transactions as webhook payloads, so a team can post them to the endpoint that
//! never received them.
//! Output is the raw webhook shape. Helius says raw webhooks "return regular Solana transaction data
//! directly from the blockchain", which is the `getTransaction` result shape: `slot`, `blockTime`,
//! `transaction` and `meta`. Enhanced webhooks carry Helius's own parsed fields (`type`, `source`,
//! `description`, `events`), which only Helius can compute, so this tool does not emit them.
use anyhow::{bail, Result};
use serde_json::{Map, Value};

/// Keys of a full transaction result that make up one raw webhook payload.
const PAYLOAD_KEYS: [&str; 5] = ["slot", "blockTime", "transaction", "meta", "version"];

/// Turn full transaction results into an array of raw webhook payloads, oldest slot first.
/// Rows with the same slot keep their input order. A row that lacks a slot, a `transaction` or a
/// `meta` is not a full transaction and fails the whole call, so a short list is never returned
/// as if it were complete.
pub fn webhook_payload(raw: &[Value]) -> Result<Vec<Value>> {
    let mut out: Vec<(u64, Value)> = Vec::with_capacity(raw.len());
    for (i, row) in raw.iter().enumerate() {
        let signature = row
            .pointer("/transaction/signatures/0")
            .and_then(Value::as_str)
            .unwrap_or("?");
        let Some(slot) = row.get("slot").and_then(Value::as_u64) else {
            bail!("row {i} ({signature}) has no slot, so it is not a full transaction");
        };
        for key in ["transaction", "meta"] {
            if !row.get(key).is_some_and(Value::is_object) {
                bail!("row {i} ({signature}) has no `{key}`, so it is not a full transaction");
            }
        }
        let mut payload = Map::new();
        for key in PAYLOAD_KEYS {
            match row.get(key) {
                Some(v) => payload.insert(key.to_string(), v.clone()),
                None if key == "blockTime" => payload.insert(key.to_string(), Value::Null),
                None => None,
            };
        }
        out.push((slot, Value::Object(payload)));
    }
    out.sort_by_key(|(slot, _)| *slot);
    Ok(out.into_iter().map(|(_, v)| v).collect())
}

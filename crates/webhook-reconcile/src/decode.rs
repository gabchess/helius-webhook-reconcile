//! Decode Anchor events from transaction logs through an IDL.
//! An event is a `Program data: <base64>` log line. Its first 8 bytes are the event discriminator,
//! the rest is the Borsh encoding of the event struct. Logs are the only input, so no network.
use crate::types::{DecodedEvent, RefEntry, Signature};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;

/// Why a decode stopped. Each variant means the answer could be wrong if the decoder went on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Signatures-mode rows carry no logs. Refetch these signatures in full mode first.
    NoLogs(Vec<Signature>),
    /// The RPC cut the logs ("Log truncated"), so events may be missing from them.
    LogsTruncated(Signature),
    /// The IDL has no address, so events cannot be tied to the program that emitted them.
    IdlWithoutAddress,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::NoLogs(sigs) => write!(
                f,
                "cannot decode {} missing transaction(s): the reference is in signatures mode and has no logs \
                 (first: {}). Refetch them in full mode with --refetch-from or --live",
                sigs.len(),
                sigs.first().map_or("?", |s| s.0.as_str())
            ),
            DecodeError::LogsTruncated(sig) => write!(
                f,
                "logs of {} are truncated (\"Log truncated\"), so events may be missing. \
                 Use a source that returns complete logs",
                sig.0
            ),
            DecodeError::IdlWithoutAddress => {
                write!(f, "the IDL has no `address`, so events cannot be tied to a program")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// An Anchor IDL reduced to what event decoding needs: event discriminators and struct types.
pub struct Idl {
    /// Program id that emits these events.
    pub address: Option<String>,
    events: Vec<(String, [u8; 8])>,
    types: HashMap<String, Value>,
}

/// The discriminator Anchor derives for an event: the first 8 bytes of `sha256("event:<Name>")`.
pub fn event_discriminator(name: &str) -> [u8; 8] {
    let mut out = [0u8; 8];
    out.copy_from_slice(&Sha256::digest(format!("event:{name}"))[..8]);
    out
}

impl Idl {
    /// Read events and types from an IDL JSON document (Anchor 0.30 and later layout).
    pub fn parse(v: &Value) -> Result<Idl> {
        let mut events = Vec::new();
        for e in v
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = e["name"]
                .as_str()
                .context("event without name")?
                .to_string();
            let disc = match e.get("discriminator").and_then(Value::as_array) {
                Some(a) => a
                    .iter()
                    .map(|b| {
                        b.as_u64()
                            .and_then(|n| u8::try_from(n).ok())
                            .ok_or_else(|| anyhow!("discriminator of {name} holds a non-byte"))
                    })
                    .collect::<Result<Vec<u8>>>()?
                    .try_into()
                    .map_err(|_| anyhow!("discriminator of {name} is not 8 bytes"))?,
                None => event_discriminator(&name),
            };
            events.push((name, disc));
        }
        let mut types = HashMap::new();
        for t in v
            .get("types")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(n) = t["name"].as_str() {
                types.insert(n.to_string(), t["type"].clone());
            }
        }
        Ok(Idl {
            address: v.get("address").and_then(Value::as_str).map(String::from),
            events,
            types,
        })
    }

    /// Decode one `Program data:` payload (base64). `Ok(None)` when no event discriminator matches,
    /// which is normal for data lines that are not events. A matching payload that is cut short or
    /// has extra bytes is an error, never a partial event.
    pub fn decode(&self, signature: &Signature, b64: &str) -> Result<Option<DecodedEvent>> {
        let bytes = STANDARD
            .decode(b64.trim())
            .context("Program data is not base64")?;
        if bytes.len() < 8 {
            return Ok(None);
        }
        let Some((name, _)) = self.events.iter().find(|(_, d)| d[..] == bytes[..8]) else {
            return Ok(None);
        };
        let mut cur = Cursor {
            b: &bytes[8..],
            pos: 0,
        };
        let ty = self
            .types
            .get(name)
            .ok_or_else(|| anyhow!("IDL has event {name} but no type for it"))?;
        let fields = match self.read_struct(ty, &mut cur)? {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        if cur.pos != cur.b.len() {
            bail!(
                "event {name} in {} has {} trailing byte(s); the IDL does not match the program",
                signature.0,
                cur.b.len() - cur.pos
            );
        }
        Ok(Some(DecodedEvent {
            signature: signature.clone(),
            event: name.clone(),
            fields,
        }))
    }

    fn read_struct(&self, ty: &Value, cur: &mut Cursor) -> Result<Value> {
        let mut out = Map::new();
        for f in ty["fields"].as_array().into_iter().flatten() {
            out.insert(
                f["name"].as_str().unwrap_or("?").to_string(),
                self.read(&f["type"], cur)?,
            );
        }
        Ok(Value::Object(out))
    }

    fn read(&self, ty: &Value, cur: &mut Cursor) -> Result<Value> {
        if let Some(p) = ty.as_str() {
            return Ok(match p {
                "bool" => json!(cur.take(1)?[0] != 0),
                "u8" => json!(cur.take(1)?[0]),
                "i8" => json!(i8::from_le_bytes(cur.arr()?)),
                "u16" => json!(u16::from_le_bytes(cur.arr()?)),
                "i16" => json!(i16::from_le_bytes(cur.arr()?)),
                "u32" => json!(u32::from_le_bytes(cur.arr()?)),
                "i32" => json!(i32::from_le_bytes(cur.arr()?)),
                "u64" => json!(u64::from_le_bytes(cur.arr()?)),
                "i64" => json!(i64::from_le_bytes(cur.arr()?)),
                // 128-bit ints as strings, because JSON numbers lose precision
                "u128" => json!(u128::from_le_bytes(cur.arr()?).to_string()),
                "i128" => json!(i128::from_le_bytes(cur.arr()?).to_string()),
                "pubkey" => json!(bs58::encode(cur.take(32)?).into_string()),
                "string" => {
                    let n = u32::from_le_bytes(cur.arr()?) as usize;
                    json!(String::from_utf8(cur.take(n)?.to_vec()).context("string is not utf8")?)
                }
                "bytes" => {
                    let n = u32::from_le_bytes(cur.arr()?) as usize;
                    json!(STANDARD.encode(cur.take(n)?))
                }
                other => bail!("IDL type {other} is not supported"),
            });
        }
        if let Some(inner) = ty.get("vec") {
            let n = u32::from_le_bytes(cur.arr()?) as usize;
            let mut items = Vec::new();
            for _ in 0..n {
                items.push(self.read(inner, cur)?);
            }
            return Ok(Value::Array(items));
        }
        if let Some(inner) = ty.get("option") {
            return if cur.take(1)?[0] == 0 {
                Ok(Value::Null)
            } else {
                self.read(inner, cur)
            };
        }
        if let Some(name) = ty.pointer("/defined/name").and_then(Value::as_str) {
            let def = self
                .types
                .get(name)
                .ok_or_else(|| anyhow!("undefined type {name}"))?;
            if def["kind"] == "struct" {
                return self.read_struct(def, cur);
            }
            bail!("defined type {name} is not a struct; enums are not supported");
        }
        bail!("IDL type {ty} is not supported")
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or_else(|| anyhow!("event data ended early (truncated payload)"))?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn arr<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }
}

/// `Program data:` payloads emitted while `program_id` is the executing program, per the invoke
/// stack. With `None`, every payload is returned. A payload from a program that was called by
/// `program_id` is skipped, so it is never decoded with the wrong IDL.
pub fn program_data_lines<'a>(logs: &'a [String], program_id: Option<&str>) -> Vec<&'a str> {
    let mut stack: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    for l in logs {
        let Some(rest) = l.strip_prefix("Program ") else {
            continue;
        };
        if let Some(data) = rest.strip_prefix("data: ") {
            if program_id.is_none() || stack.last().copied() == program_id {
                out.push(data);
            }
            continue;
        }
        let Some((id, tail)) = rest.split_once(' ') else {
            continue;
        };
        // "Program log: ..." and "Program return: ..." carry free text, never a program id
        if id.ends_with(':') {
            continue;
        }
        if let Some(depth) = tail
            .strip_prefix("invoke [")
            .and_then(|t| t.strip_suffix(']'))
            .and_then(|n| n.parse::<usize>().ok())
        {
            stack.truncate(depth.saturating_sub(1));
            stack.push(id);
        } else if tail == "success" || tail.starts_with("failed") {
            stack.pop();
        }
    }
    out
}

/// Decode the events of every missing transaction. A failed transaction is skipped: the runtime
/// rolled its state back, so an event in its logs never took effect, and replaying it as an event
/// would be wrong. Fails loudly instead of returning a short list:
/// - a row without logs (signatures mode) gives `DecodeError::NoLogs`
/// - a row whose logs say "Log truncated" gives `DecodeError::LogsTruncated`
/// - a cut or oversized event payload gives an error from `Idl::decode`
pub fn decode_missing(idl: &Idl, missing: &[RefEntry]) -> Result<Vec<DecodedEvent>> {
    let program = idl
        .address
        .as_deref()
        .ok_or(DecodeError::IdlWithoutAddress)?;
    let no_logs: Vec<Signature> = missing
        .iter()
        .filter(|m| m.logs.is_none())
        .map(|m| m.signature.clone())
        .collect();
    if !no_logs.is_empty() {
        return Err(DecodeError::NoLogs(no_logs).into());
    }
    let mut events = Vec::new();
    for m in missing.iter().filter(|m| !m.failed) {
        let logs = m.logs.as_deref().unwrap_or_default();
        if logs.iter().any(|l| l.starts_with("Log truncated")) {
            return Err(DecodeError::LogsTruncated(m.signature.clone()).into());
        }
        for data in program_data_lines(logs, Some(program)) {
            if let Some(ev) = idl.decode(&m.signature, data)? {
                events.push(ev);
            }
        }
    }
    Ok(events)
}

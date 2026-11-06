//! Print one diff run: the JSON report or the text report.
use crate::{DecodedEvent, DiffReport, ReceivedSet, ReferenceSet};
use anyhow::Result;
use serde_json::json;
use std::io::Write;
use std::path::Path;

/// Everything one diff run found, borrowed from the caller.
#[derive(Clone, Copy)]
pub struct Findings<'a> {
    pub reference: &'a ReferenceSet,
    pub received: &'a ReceivedSet,
    pub diff: &'a DiffReport,
    pub decoded: Option<&'a [DecodedEvent]>,
    /// The backfill file and the number of payloads written to it.
    pub backfill: Option<(&'a Path, usize)>,
}

/// Write the report of one run as JSON when `json` is set, as text otherwise.
pub fn write(out: &mut dyn Write, found: Findings, json: bool) -> Result<()> {
    let Findings {
        reference,
        received,
        diff: report,
        decoded,
        backfill,
    } = found;
    if json {
        let report_json = json!({
            "status": report.status,
            "reference_tag": reference.tag, "received_tag": received.tag,
            "reference_count": reference.entries.len(), "received_count": received.signatures.len(),
            "missing": report.missing,
            "skipped_by_status": report.skipped_by_status,
            "duplicates": report.duplicates.iter().map(|(s, n)| json!({"signature": s, "times": n})).collect::<Vec<_>>(),
            "extra": report.extra,
            "decoded": decoded,
            "backfill_count": backfill.map(|(_, n)| n),
        });
        writeln!(out, "{}", serde_json::to_string_pretty(&report_json)?)?;
    } else {
        writeln!(
            out,
            "reference {} ({}), received {} ({}), status {}",
            reference.entries.len(),
            reference.tag.as_deref().unwrap_or("untagged"),
            received.signatures.len(),
            received.tag.as_deref().unwrap_or("untagged"),
            serde_json::to_value(report.status)?.as_str().unwrap_or("?"),
        )?;
        writeln!(out, "missing: {}", report.missing.len())?;
        for m in &report.missing {
            writeln!(
                out,
                "  {} slot={} failed={}",
                m.signature.0,
                m.slot.map_or("?".into(), |s| s.to_string()),
                m.failed
            )?;
        }
        if !report.skipped_by_status.is_empty() {
            writeln!(
                out,
                "not counted by --status: {} (never received; rerun with --status any to list them)",
                report.skipped_by_status.len()
            )?;
        }
        writeln!(out, "duplicates: {}", report.duplicates.len())?;
        for (s, n) in &report.duplicates {
            writeln!(out, "  {} x{}", s.0, n)?;
        }
        writeln!(
            out,
            "extra (received, not in reference): {}",
            report.extra.len()
        )?;
        for s in &report.extra {
            writeln!(out, "  {}", s.0)?;
        }
        if let Some(events) = decoded {
            writeln!(out, "decoded events from missing txs: {}", events.len())?;
            for e in events {
                writeln!(
                    out,
                    "  {} {} {}",
                    e.signature.0,
                    e.event,
                    serde_json::to_string(&e.fields)?
                )?;
            }
        }
        if let Some((path, count)) = backfill {
            writeln!(
                out,
                "backfill: {count} webhook payloads written to {}",
                path.display()
            )?;
        }
    }
    Ok(())
}

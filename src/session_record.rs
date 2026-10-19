//! Claude Code's `<config dir>/sessions/<pid>.json` record — the fields clauth
//! reads, and the one reader every caller shares.
//!
//! Claude Code rewrites the record in place, so one read can land mid-write and
//! the stamp is never re-taken: the shared reader retries once, so the torn-read
//! retry lives here rather than in each caller.

use std::path::Path;

use serde::Deserialize;

/// The fields of a Claude Code session record (`<config dir>/sessions/<pid>.json`)
/// clauth reads.
#[derive(serde::Deserialize)]
pub(crate) struct SessionRecord {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: String,
    pub(crate) pid: u32,
    /// The process start time the record carries, as the record spells it. Only
    /// the bridge's linux resolver compares it (against `/proc/<pid>/stat` field
    /// 22, measured equal); the macOS form is unmeasured, so nothing there
    /// compares it. `default` is the upgrade gate: a record written before the
    /// field existed reads empty and fails the linux compare rather than being
    /// trusted. The bridge is unix-only, so Windows reads no field beyond
    /// `session_id` and `pid`.
    ///
    /// Read leniently: every other caller parses this struct for `session_id`/
    /// `pid`, and a future Claude Code version writing `procStart` as a
    /// non-string must not fail their parses, so a non-string reads as empty
    /// (A#16) — which the linux compare then rejects.
    #[serde(rename = "procStart", default, deserialize_with = "proc_start_lenient")]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) proc_start: String,
}

/// `procStart` as a string, else empty: the bridge is the only reader, and a
/// non-string there must not fail every other `SessionRecord` parse.
fn proc_start_lenient<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().map(str::to_owned).unwrap_or_default())
}

/// The record for `pid`, `None` when it does not name `pid` itself with a
/// non-empty session id. The torn-read retry lives here: a first read that lands
/// mid-write fails to parse, so a second read decides and the stamp is never
/// re-taken.
pub(crate) fn session_record(dir: &Path, pid: u32) -> Option<SessionRecord> {
    let path = dir.join(format!("{pid}.json"));
    let record = (0..2)
        .find_map(|_| serde_json::from_slice::<SessionRecord>(&std::fs::read(&path).ok()?).ok())?;
    (record.pid == pid && !record.session_id.is_empty()).then_some(record)
}

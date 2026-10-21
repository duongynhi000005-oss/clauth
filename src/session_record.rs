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
    #[serde(rename = "procStart", default, deserialize_with = "lenient_string")]
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) proc_start: String,
}

/// A well-formed JSON string as itself, any other value as empty; a token
/// serde_json refuses (an out-of-range number, a lone-surrogate escape) fails
/// the parse. The one lenient-string reader shared by `procStart` and the job
/// store's `state`, so a future Claude Code version retyping either field must
/// not fail every record parse. A non-string degrades to empty, which the
/// field's own rule then reads (an empty `state` is unfinished).
pub(crate) fn lenient_string<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let flag = LenientString::deserialize(deserializer)?;
    Ok(flag.0.unwrap_or_default())
}

/// A well-formed JSON string as itself, any other shape as `None`; a token
/// serde_json refuses (an out-of-range number, a lone-surrogate escape) fails
/// the parse. The one lenient element reader shared by [`lenient_string`] and
/// the job store's `respawnFlags` decoder. A nested sequence or map drains
/// itself element by element through [`serde::de::IgnoredAny`], which
/// serde_json skips iteratively and outside its recursion limit.
pub(crate) struct LenientString(pub(crate) Option<String>);

impl<'de> serde::Deserialize<'de> for LenientString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(LenientStringVisitor)
    }
}

struct LenientStringVisitor;

impl<'de> serde::de::Visitor<'de> for LenientStringVisitor {
    type Value = LenientString;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a string or any other JSON value")
    }

    fn visit_str<E>(self, v: &str) -> Result<LenientString, E> {
        Ok(LenientString(Some(v.to_owned())))
    }

    fn visit_string<E>(self, v: String) -> Result<LenientString, E> {
        Ok(LenientString(Some(v)))
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<LenientString, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {}
        Ok(LenientString(None))
    }

    fn visit_map<A>(self, mut map: A) -> Result<LenientString, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        while map
            .next_entry::<serde::de::IgnoredAny, serde::de::IgnoredAny>()?
            .is_some()
        {}
        Ok(LenientString(None))
    }

    fn visit_unit<E>(self) -> Result<LenientString, E> {
        Ok(LenientString(None))
    }

    fn visit_bool<E>(self, _: bool) -> Result<LenientString, E> {
        Ok(LenientString(None))
    }

    fn visit_i64<E>(self, _: i64) -> Result<LenientString, E> {
        Ok(LenientString(None))
    }

    fn visit_u64<E>(self, _: u64) -> Result<LenientString, E> {
        Ok(LenientString(None))
    }

    fn visit_f64<E>(self, _: f64) -> Result<LenientString, E> {
        Ok(LenientString(None))
    }
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

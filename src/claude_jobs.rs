//! Reader for Claude Code's background-job store
//! (`~/.claude/jobs/<id>/state.json`).
//!
//! `claude --bg` hands its session to Claude Code's background service and
//! returns at once, so a `clauth start <p> --bg` teardown — and any later GC
//! sweep — can find the runtime tree still in use by a session clauth's marker
//! flock never sees (the session is orphaned: `starting…`, no pid). The job
//! record is Claude Code's own statement that a session uses a config dir.
//!
//! One read yields the tree and settings-file name sets. The spare term
//! ([`JobTrees::spares`] / [`JobTrees::spares_under`], GC and the destructive
//! gates) counts a job as live while its job dir exists (until `claude rm`),
//! whatever its `state`: a finished job may be revived (`claude attach`) and
//! the cost of sparing its tree is disk. The settings term
//! ([`JobTrees::spares_settings`]) spares a spawn-settings file a job's
//! `respawnFlags` name. The rotation term ([`JobTrees::unfinished_under`])
//! counts only UNFINISHED jobs: a `stopped`/`done`/`failed` job must not freeze
//! the chain, and only `state` can say "finished" — `firstTerminalAt` cannot (a
//! revived `running` job keeps the stamp).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::profile::claude_dir;
use crate::session_record::LenientString;

// Test seam: how many times THIS thread has read the job store, so a
// pass-level caller's test can pin "one read per pass". Thread-local: a
// concurrent test thread's reads land in its own cell and never pollute this
// count.
#[cfg(test)]
thread_local! {
    static JOB_TREES_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn job_trees_reads() -> usize {
    JOB_TREES_READS.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_job_trees_reads() {
    JOB_TREES_READS.with(|c| c.set(0));
}

/// A job's `state.json`, reduced to the fields the spare, rotation and
/// settings-file rules need. Unknown fields are ignored: the record's key set
/// grew across Claude Code versions.
#[derive(Deserialize)]
struct JobState {
    #[serde(rename = "providerEnv")]
    provider_env: Option<ProviderEnv>,
    /// `String`, never a closed enum and never a strict parse: a future Claude
    /// Code shape writing `state` as a non-string reads empty
    /// ([`crate::session_record::lenient_string`]), and empty reads unfinished —
    /// a false "finished" would sign a running session out.
    #[serde(default, deserialize_with = "crate::session_record::lenient_string")]
    state: String,
    /// Never a strict parse: absent reads [`RespawnFlags::Absent`]; a list
    /// decodes through [`read_respawn_flags`] to [`RespawnFlags::Named`] or
    /// [`RespawnFlags::Unknown`]; any other value, or a token serde_json
    /// refuses inside it, fails this parse and [`JobCore`] reads the flags
    /// [`RespawnFlags::Unknown`]. Only a record that fails that parse too (a
    /// syntax error, or a `providerEnv` or `state` token serde_json refuses)
    /// flips the store `Unreadable`.
    #[serde(
        default,
        rename = "respawnFlags",
        deserialize_with = "read_respawn_flags"
    )]
    respawn_flags: RespawnFlags,
}

/// [`JobState`] minus the `respawnFlags` field: the fallback parse when
/// [`JobState`] fails. `respawnFlags` is unmodelled, so serde skips it through
/// [`serde::de::IgnoredAny`] — a path a token serde_json rejects inside the
/// field survives — and a record only this struct parses is one whose sole
/// fault is in `respawnFlags`. `providerEnv` and `state` share [`JobState`]'s
/// attributes, so a fault in either still fails both parses.
#[derive(Deserialize)]
struct JobCore {
    #[serde(rename = "providerEnv")]
    provider_env: Option<ProviderEnv>,
    #[serde(default, deserialize_with = "crate::session_record::lenient_string")]
    state: String,
}

/// Claude Code's terminal agent states. `stopped` measured on 2.1.304
/// (`claude stop`); `done`/`failed` from `claude agents --json`'s state list,
/// never seen in a record; any other string reads unfinished.
const FINISHED_STATES: [&str; 3] = ["stopped", "done", "failed"];

#[derive(Deserialize)]
struct ProviderEnv {
    #[serde(rename = "CLAUDE_CONFIG_DIR")]
    claude_config_dir: Option<String>,
}

/// The name sets one readable job-store walk yields. The fields are private
/// behind the query methods: `unfinished ⊆ named` must hold, and a caller
/// reading one set directly could break it.
#[derive(Debug, Default)]
pub(crate) struct NamedTrees {
    /// Canonical identities of every config dir a live job names, whatever its
    /// `state`. Empty when no job names a tree (or no job exists). A job
    /// naming a dir that no longer resolves contributes nothing: its tree is
    /// already gone.
    named: HashSet<PathBuf>,
    /// Canonical identities of every config dir an UNFINISHED job names — a
    /// subset of `named` (an unfinished job is also live for the spare term).
    unfinished: HashSet<PathBuf>,
    /// Canonical identities of every settings file a job's `respawnFlags` name
    /// (a path following a `--settings` element).
    named_settings: HashSet<PathBuf>,
    /// Canonical identities of every config dir whose job's `respawnFlags` are
    /// not a list of strings, so no settings file is recoverable from them and
    /// the caller falls back to the tree rule.
    unknown_flags_trees: HashSet<PathBuf>,
}

/// The answer to "which runtime trees and settings files must a sweep or gate
/// spare?".
#[derive(Debug)]
pub(crate) enum JobTrees {
    /// A readable store, as [`NamedTrees`].
    Named(NamedTrees),
    /// The store could not be trusted — a job dir's `state.json` was missing,
    /// unreadable, or unparseable, or a named config dir could not be
    /// canonicalized while it might still exist. Carries the offending dir for
    /// a log line that never carries the record's contents.
    Unreadable(PathBuf),
}

impl JobTrees {
    /// Whether `tree` must be spared: a live job names it, or the store was
    /// unreadable (fail closed, the direction `is_session_alive` takes).
    ///
    /// `tree` is canonicalized here so it and the named set share one spelling
    /// ([`crate::canonical::identity`], the compare-or-key purpose). A
    /// `NotFound` canonicalize — the tree is already gone — reads as unnamed:
    /// nothing to spare, and the caller still collects an orphaned sessions
    /// dir. Any other canonicalize failure reads as named.
    pub(crate) fn spares(&self, tree: &Path) -> bool {
        match self {
            JobTrees::Unreadable(_) => true,
            JobTrees::Named(sets) if sets.named.is_empty() => false,
            JobTrees::Named(sets) => match crate::canonical::identity(tree) {
                Ok(id) => sets.named.contains(&id),
                Err(e) if e.kind() == io::ErrorKind::NotFound => false,
                Err(_) => true,
            },
        }
    }

    /// Whether a live job names a tree UNDER `dir`, or the store is unreadable
    /// (fail closed). Used by [`crate::runtime::has_live_session`]: a job
    /// naming any tree under a profile's dir is a live session for every gate
    /// that carries (delete, disable, rename), since the marker flock never
    /// sees the `claude --bg` session still reading its config dir. The macOS
    /// rotation gate uses the narrower [`JobTrees::unfinished_under`] term
    /// instead.
    pub(crate) fn spares_under(&self, dir: &Path) -> bool {
        match self {
            JobTrees::Unreadable(_) => true,
            JobTrees::Named(sets) => named_under(&sets.named, dir),
        }
    }

    /// Whether `file` must be spared: a job's `respawnFlags` name it, or a job
    /// whose flags are unreadable names one of `own_trees` (the tree rule's
    /// fallback), or the store was unreadable (fail closed). `file` and each
    /// own tree are canonicalized like the tree set. The fallback is loose by
    /// design: when `own_trees` holds both layouts of a shared file, a job
    /// naming the bare fake `runtime` spares the Real-mode file beside it too.
    pub(crate) fn spares_settings(&self, file: &Path, own_trees: &[PathBuf]) -> bool {
        match self {
            JobTrees::Unreadable(_) => true,
            JobTrees::Named(sets) => {
                if sets.named_settings.is_empty() && sets.unknown_flags_trees.is_empty() {
                    return false;
                }
                holds_canonical(&sets.named_settings, file)
                    || own_trees
                        .iter()
                        .any(|tree| holds_canonical(&sets.unknown_flags_trees, tree))
            }
        }
    }

    /// The canonical tree paths every UNFINISHED job names UNDER `dir`, so the
    /// rotation gate can read each tree's `.credentials.json` and apply the
    /// same holds-a-refresh-token narrowing the marker term gets. `None` when
    /// the store is unreadable (fail closed: the caller blocks). A finished job
    /// (`stopped`/`done`/`failed`) is not mid-task, so it names nothing here;
    /// an unknown or missing `state` reads unfinished by construction.
    ///
    /// `dir` is canonicalized so it and the set share one spelling; a
    /// `NotFound` canonicalize — the dir is already gone — reads as empty, and
    /// any other failure reads as unreadable, like [`JobTrees::spares`].
    pub(crate) fn unfinished_under(&self, dir: &Path) -> Option<Vec<PathBuf>> {
        match self {
            JobTrees::Unreadable(_) => None,
            JobTrees::Named(sets) if sets.unfinished.is_empty() => Some(Vec::new()),
            JobTrees::Named(sets) => {
                let dir_id = match crate::canonical::identity(dir) {
                    Ok(id) => id,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Some(Vec::new()),
                    Err(_) => return None,
                };
                Some(
                    sets.unfinished
                        .iter()
                        .filter(|named| named.starts_with(&dir_id))
                        .cloned()
                        .collect(),
                )
            }
        }
    }
}

/// Whether any canonical identity in `set` lies under `dir`'s canonical
/// identity. `dir` is canonicalized so it and the set share one spelling (the
/// same [`crate::canonical::identity`] form `spares` uses). A `NotFound`
/// canonicalize — the dir is already gone — reads as unnamed: nothing under it
/// exists to spare. Any other failure reads as named.
fn named_under(set: &HashSet<PathBuf>, dir: &Path) -> bool {
    if set.is_empty() {
        return false;
    }
    match crate::canonical::identity(dir) {
        Ok(dir_id) => set.iter().any(|named| named.starts_with(&dir_id)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Whether `set` holds `path`'s canonical identity, fail closed: a `NotFound`
/// canonicalize — the path is already gone — reads as not held (nothing to
/// spare), any other failure as held.
fn holds_canonical(set: &HashSet<PathBuf>, path: &Path) -> bool {
    match crate::canonical::identity(path) {
        Ok(id) => set.contains(&id),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// Deserializer for [`JobState::respawn_flags`]: walks the value into a
/// [`RespawnFlags`] without ever building a `serde_json::Value`; a non-list
/// value errors, and [`JobCore`]'s fallback then reads the flags
/// [`RespawnFlags::Unknown`].
fn read_respawn_flags<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<RespawnFlags, D::Error> {
    deserializer.deserialize_any(RespawnFlagsVisitor)
}

/// What a job's `respawnFlags` name: [`RespawnFlags::Absent`] when the field
/// is missing, [`RespawnFlags::Named`] the settings values after `--settings`
/// (either spelling), or [`RespawnFlags::Unknown`] when the field is present
/// but not a list of strings, and the caller must fall back to the tree rule.
#[derive(Default)]
enum RespawnFlags {
    #[default]
    Absent,
    Named(Vec<String>),
    Unknown,
}

struct RespawnFlagsVisitor;

impl<'de> serde::de::Visitor<'de> for RespawnFlagsVisitor {
    type Value = RespawnFlags;

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("a list of strings")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<RespawnFlags, A::Error>
    where
        A: serde::de::SeqAccess<'de>,
    {
        let mut named = Vec::new();
        let mut unknown = false;
        while let Some(flag) = seq.next_element::<LenientString>()? {
            let Some(flag) = flag.0 else {
                unknown = true;
                continue;
            };
            if unknown {
                continue;
            }
            if let Some(value) = flag.strip_prefix("--settings=") {
                named.push(value.to_string());
            } else if flag == "--settings" {
                match seq.next_element::<LenientString>()? {
                    Some(LenientString(Some(value))) => named.push(value),
                    _ => unknown = true,
                }
            }
        }
        if unknown {
            Ok(RespawnFlags::Unknown)
        } else {
            Ok(RespawnFlags::Named(named))
        }
    }
}

/// Read Claude Code's job store once. Every entry that is a DIRECTORY is a job
/// (plain files like `pins.json` are not); a missing `~/.claude/jobs` dir reads
/// as no jobs. Any job dir whose `state.json` is missing, unreadable, or
/// unparseable makes the whole store unreadable, so the caller spares every
/// tree rather than trusting a torn read.
pub(crate) fn job_trees() -> JobTrees {
    #[cfg(test)]
    JOB_TREES_READS.with(|c| c.set(c.get() + 1));
    let jobs_dir = match claude_dir() {
        Ok(dir) => dir.join("jobs"),
        // Fail closed: a store this reader cannot even locate must spare, never
        // collect. `claude_dir` fails only when `home_dir` fails (no home), so
        // there is no real path to name — log the conventional location rather
        // than an empty path.
        Err(_) => return JobTrees::Unreadable(PathBuf::from("~/.claude/jobs")),
    };
    let entries = match std::fs::read_dir(&jobs_dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return JobTrees::Named(NamedTrees::default());
        }
        Err(_) => return JobTrees::Unreadable(jobs_dir),
    };
    let mut named = HashSet::new();
    let mut unfinished = HashSet::new();
    let mut named_settings = HashSet::new();
    let mut unknown_flags_trees = HashSet::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return JobTrees::Unreadable(jobs_dir),
        };
        let job_dir = entry.path();
        let is_dir = match entry.file_type() {
            Ok(file_type) => file_type.is_dir(),
            Err(_) => return JobTrees::Unreadable(job_dir),
        };
        if !is_dir {
            continue;
        }
        let record = match std::fs::read(job_dir.join("state.json")) {
            Ok(bytes) => bytes,
            Err(_) => return JobTrees::Unreadable(job_dir),
        };
        let state: JobState = match serde_json::from_slice(&record) {
            Ok(state) => state,
            // A non-list `respawnFlags`, or a token serde_json rejects inside
            // it (an out-of-range number, a lone surrogate, a repeated key),
            // fails the derive parse; re-read the record without the field
            // rather than spare every tree. A record that fails this too is
            // torn or holds a `providerEnv` or `state` token serde_json
            // refuses, and stays unreadable.
            Err(_) => match serde_json::from_slice::<JobCore>(&record) {
                Ok(core) => JobState {
                    provider_env: core.provider_env,
                    state: core.state,
                    respawn_flags: RespawnFlags::Unknown,
                },
                Err(_) => return JobTrees::Unreadable(job_dir),
            },
        };
        let Some(config_dir) = state.provider_env.and_then(|env| env.claude_config_dir) else {
            // A plain `claude --bg` on `~/.claude` names nothing.
            continue;
        };
        match crate::canonical::identity(&config_dir) {
            Ok(tree_id) => {
                let finished = FINISHED_STATES.contains(&state.state.as_str());
                named.insert(tree_id.clone());
                if !finished {
                    unfinished.insert(tree_id.clone());
                }
                match state.respawn_flags {
                    RespawnFlags::Absent => {}
                    RespawnFlags::Named(values) => {
                        for value in values {
                            // Inline JSON names no file; a value that cannot
                            // resolve as a path scopes the fallback to this
                            // job's own tree, never the whole store.
                            if crate::cc_exempt::is_inline_settings(&value) {
                                continue;
                            }
                            match crate::canonical::identity(&value) {
                                Ok(settings_id) => {
                                    named_settings.insert(settings_id);
                                }
                                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                                Err(_) => {
                                    unknown_flags_trees.insert(tree_id.clone());
                                }
                            }
                        }
                    }
                    RespawnFlags::Unknown => {
                        unknown_flags_trees.insert(tree_id);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return JobTrees::Unreadable(job_dir),
        }
    }
    JobTrees::Named(NamedTrees {
        named,
        unfinished,
        named_settings,
        unknown_flags_trees,
    })
}

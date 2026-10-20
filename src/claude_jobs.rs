//! Reader for Claude Code's background-job store
//! (`~/.claude/jobs/<id>/state.json`).
//!
//! `claude --bg` hands its session to Claude Code's background service and
//! returns at once, so a `clauth start <p> --bg` teardown — and any later GC
//! sweep — can find the runtime tree still in use by a session clauth's marker
//! flock never sees (the session is orphaned: `starting…`, no pid). The job
//! record is Claude Code's own statement that a session uses a config dir.
//!
//! One read yields two name sets. The spare term ([`JobTrees::spares`] /
//! [`JobTrees::spares_under`], GC and the destructive gates) counts a job as
//! live while its job dir exists (until `claude rm`), whatever its `state`: a
//! finished job may be revived (`claude attach`) and the cost of sparing its
//! tree is disk. The rotation term ([`JobTrees::unfinished_under`]) counts only
//! UNFINISHED jobs: a `stopped`/`done`/`failed` job must not freeze the chain,
//! and only `state` can say "finished" — `firstTerminalAt` cannot (a revived
//! `running` job keeps the stamp).

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::profile::claude_dir;

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

/// A job's `state.json`, reduced to the fields the spare and rotation rules
/// need. Unknown fields are ignored: the record's key set grew across Claude
/// Code versions.
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

/// The two name sets one readable job-store walk yields. The fields are
/// private behind the query methods: `unfinished ⊆ named` must hold, and a
/// caller reading one set directly could break it.
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
}

/// The answer to "which runtime trees must a sweep or gate spare?".
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
            Err(_) => return JobTrees::Unreadable(job_dir),
        };
        let Some(config_dir) = state.provider_env.and_then(|env| env.claude_config_dir) else {
            // A plain `claude --bg` on `~/.claude` names nothing.
            continue;
        };
        match crate::canonical::identity(&config_dir) {
            Ok(id) => {
                let finished = FINISHED_STATES.contains(&state.state.as_str());
                if finished {
                    named.insert(id);
                } else {
                    named.insert(id.clone());
                    unfinished.insert(id);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return JobTrees::Unreadable(job_dir),
        }
    }
    JobTrees::Named(NamedTrees { named, unfinished })
}

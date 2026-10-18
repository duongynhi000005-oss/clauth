//! Live token speed (tps) read off Claude Code transcripts.
//!
//! speed = a response's output tokens over its generation span; rate = output
//! tokens generated inside a wall-clock window over its length. The process
//! holding the single-fetcher lease produces a [`Snapshot`] into
//! `~/.clauth/tps_cache.json`; every other surface reads that cache.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub(crate) const TPS_CACHE_FILE: &str = "tps_cache.json";
pub(crate) const CACHE_VERSION: u16 = 1;
pub(crate) const LIVE_WINDOW_MS: u64 = 10_000;
pub(crate) const M5_WINDOW_MS: u64 = 5 * 60_000;
pub(crate) const H1_WINDOW_MS: u64 = 60 * 60_000;
/// A cache older than this reads as no producer.
pub(crate) const STALE_AFTER_MS: u64 = 30_000;

/// The reserved day-sum account for unattributed sessions: no profile name can
/// hold it, and per-account views skip it.
pub(crate) const UNATTRIBUTED: &str = "*";

/// How often the producer re-walks the transcript stores for new files. Between
/// walks it only tails the bytes appended to the files it already knows, so a
/// 1 s tick never pays a whole-tree walk.
const DISCOVER_INTERVAL_MS: u64 = 5_000;

/// Recursion depth of the store walk (`projects/<slug>/<session>/subagents/`).
const WALK_DEPTH: usize = 8;

const DAY_MS: u64 = 86_400_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Figure {
    pub(crate) rate: f64,
    pub(crate) speed: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub(crate) struct Figures {
    pub(crate) live: Figure,
    pub(crate) m5: Figure,
    pub(crate) h1: Figure,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SessionFigures {
    pub(crate) account: Option<String>,
    #[serde(flatten)]
    pub(crate) figures: Figures,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DaySum {
    pub(crate) output: u64,
    pub(crate) gen_ms: u64,
}

/// date (UTC `YYYY-MM-DD`, the ledger's day key) → account → model → sums.
pub(crate) type DaySums = BTreeMap<String, BTreeMap<String, BTreeMap<String, DaySum>>>;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub(crate) version: u16,
    /// The time of the cache's last write, as `+00:00` RFC 3339 with
    /// milliseconds. (Not the producer's last tick: the write is throttled.)
    pub(crate) generated_at: String,
    pub(crate) total: Figures,
    pub(crate) accounts: BTreeMap<String, Figures>,
    pub(crate) sessions: BTreeMap<String, SessionFigures>,
    pub(crate) days: DaySums,
}

impl Snapshot {
    /// Milliseconds since `generated_at`; `None` when it does not parse or is
    /// in the future.
    pub(crate) fn age_ms(&self, now_ms: u64) -> Option<u64> {
        now_ms.checked_sub(parse_ts(&self.generated_at)?)
    }
}

/// One response the tail has seen, kept as its latest (largest-output) shape.
/// Keyed by message id (or `(session, preceding user ts)` for an id-less line),
/// so growing streamed partials collapse into one response and are never
/// counted twice.
#[derive(Debug, Clone)]
struct Resp {
    session: String,
    /// The member that spent this response, frozen at first read (or `*` for an
    /// unattributed session).
    account: String,
    model: String,
    /// Latest `user` line before this response's first line; `None` when there
    /// is none, which leaves the response without a measurable span.
    start_ms: Option<u64>,
    /// Timestamp of the last line carrying the largest output seen so far.
    end_ms: u64,
    /// Largest output seen so far.
    output: u64,
    /// The UTC day its sums sit on: its end line's day, moved when the end
    /// crosses midnight.
    day: String,
    /// False while every line read so far sits at or before the restart
    /// watermark (those lines are the previous producer's counted baseline);
    /// turns true at the first later line, from which on its deltas fold into
    /// `days`.
    added: bool,
    /// The output already folded into `days`.
    added_output: u64,
    /// The generation ms already folded into `days`.
    added_gen_ms: u64,
}

impl Resp {
    /// `(start, generation span in ms)`, or `None` for a zero-length span.
    fn span(&self) -> Option<(u64, u64)> {
        let start = self.start_ms?;
        (self.end_ms > start).then(|| (start, self.end_ms - start))
    }
}

/// Byte offset, last-walk mtime and length for one transcript file.
#[derive(Debug, Clone, Copy)]
struct FileEntry {
    offset: u64,
    mtime_ms: u64,
    len: u64,
}

/// A memoized store fallback owner for one session key, keyed on the owner
/// store's mtime (the hook's `resolved` is read fresh each lookup).
#[derive(Debug, Clone)]
struct OwnerMemo {
    owner: Option<String>,
    store_mtime: u64,
}

/// The tail reader + attribution + windowing, owned by the lease holder.
#[derive(Debug, Default)]
pub(crate) struct Producer {
    /// Byte offset + mtime + len per transcript file; only appended bytes are
    /// re-read.
    files: HashMap<PathBuf, FileEntry>,
    /// Every response still relevant (the last hour + today + yesterday),
    /// keyed by message id or `(session, user ts)`.
    responses: HashMap<String, Resp>,
    /// Accumulated day sums (additive), loaded from the cache on start.
    days: DaySums,
    /// Latest user line per transcript PATH, for the next response's span start.
    last_user: HashMap<PathBuf, u64>,
    /// Store fallback owners, memoized on `session_profiles.json`'s mtime.
    owner_cache: HashMap<String, OwnerMemo>,
    /// Batch-resolved shared-session owners, refreshed when the store mtime
    /// moves (so one parse serves the snapshot's row re-resolution).
    store_owners: HashMap<String, Option<String>>,
    /// The store mtime `store_owners` was refreshed at.
    store_owners_mtime: u64,
    /// Session key → a representative transcript path, for re-resolving the
    /// session's current member at snapshot time.
    session_path: HashMap<String, PathBuf>,
    /// `session_profiles.json` mtime, refreshed once per tick.
    store_mtime: u64,
    /// The previous cache's `generated_at` (epoch ms): lines at or before it
    /// were counted by the previous producer (they set the baseline); only
    /// lines after it fold. A new file inside the ≤ 5 s discovery lag and
    /// lines landing between the tail and the last write are the same class of
    /// residual: they are never counted. A baseline built from lines the
    /// previous producer never read (the residual lines above) lets the
    /// response's later lines fold only their extension; the extension of a
    /// response straddling the restart folds under the session's member at
    /// re-read, so a member swap between the previous producer's read and the
    /// restart puts the extension on the new member, and when that later line
    /// also crosses midnight and the new member has a row that day, the move
    /// takes the baseline off it and adds it to the next, so the turn counts
    /// under both members (with no row, the turn splits between them); a
    /// restart on the second UTC day or later after a turn straddled midnight,
    /// the last cache write before that midnight, skips the turn's pre-cutoff
    /// lines, so its later line folds as a new response while the old day
    /// folds into the speed ledger: that turn counts twice.
    watermark_ms: u64,
    /// The speed ledger the producer folds each finalized day into at rollover.
    speed_ledger: crate::token_ledger::SpeedLedger,
    /// A save failed; the folded days are back in the cache and retried every
    /// tick until a save succeeds.
    unsaved: bool,
    /// The "newer/unreadable ledger" line has been logged once.
    unusable_logged: bool,
    /// The "save failed" line has been logged for the current failure state.
    save_failed_logged: bool,
    /// `token_speed.json`'s (ctime_secs, ctime_nsecs, mtime_ms, len) at the last
    /// reload check (ctime is 0 off unix).
    speed_ledger_sig: (u64, u64, u64, u64),
    /// The last time an unreadable speed ledger was reloaded (the 5 min timer).
    last_reload_ms: u64,
    /// When the last whole-tree walk ran.
    last_discover_ms: u64,
}

impl Producer {
    pub(crate) fn new() -> Self {
        let cache = load_cache();
        let days = cache.as_ref().map(|s| s.days.clone()).unwrap_or_default();
        let watermark_ms = cache
            .as_ref()
            .and_then(|s| parse_ts(&s.generated_at))
            .unwrap_or(0);
        let speed_ledger = crate::profile::clauth_dir()
            .ok()
            .map(|d| crate::token_ledger::SpeedLedger::load(&d))
            .unwrap_or_default();
        Self {
            files: HashMap::new(),
            responses: HashMap::new(),
            days,
            last_user: HashMap::new(),
            owner_cache: HashMap::new(),
            store_owners: HashMap::new(),
            store_owners_mtime: 0,
            session_path: HashMap::new(),
            store_mtime: 0,
            watermark_ms,
            speed_ledger,
            unsaved: false,
            unusable_logged: false,
            save_failed_logged: false,
            speed_ledger_sig: (0, 0, 0, 0),
            last_reload_ms: 0,
            last_discover_ms: 0,
        }
    }

    /// One pass at `now_ms`: tail every transcript store under the home dir,
    /// attribute each session, and rebuild the snapshot.
    pub(crate) fn tick(&mut self, now_ms: u64) -> Snapshot {
        self.store_mtime = store_mtime_now();
        if self.files.is_empty()
            || now_ms.saturating_sub(self.last_discover_ms) >= DISCOVER_INTERVAL_MS
        {
            self.discover(now_ms);
            self.last_discover_ms = now_ms;
        }
        self.tail_all(now_ms);

        let today = day_of_ms(now_ms);
        let yesterday = day_of_ms(now_ms - DAY_MS);
        let hour_ago = now_ms.saturating_sub(H1_WINDOW_MS);
        self.responses.retain(|_, r| {
            let day = day_of_ms(r.end_ms);
            r.end_ms >= hour_ago || day == today || day == yesterday
        });
        // At day rollover, fold every day that fell out of {today, yesterday}
        // into the speed ledger (the producer is its only writer) and drop it
        // from the cache; on a failed save the day stays for the next rollover.
        self.fold_stale_days(&today, &yesterday, now_ms);
        let live_keys: HashSet<&str> = self
            .responses
            .values()
            .map(|r| r.session.as_str())
            .collect();
        self.session_path
            .retain(|k, _| live_keys.contains(k.as_str()));
        self.owner_cache
            .retain(|k, _| live_keys.contains(k.as_str()));

        self.build_snapshot(now_ms)
    }

    /// Fold each day strictly before `yesterday` into the speed ledger and
    /// drop it from the cache. A day whose save fails stays in the cache and
    /// is retried every tick until a save succeeds; a newer/unreadable ledger
    /// is never saved and the days stay in the cache (re-loaded when its file
    /// changes).
    fn fold_stale_days(&mut self, today: &str, yesterday: &str, now_ms: u64) {
        let stale: Vec<String> = self
            .days
            .keys()
            .filter(|d| d.as_str() < yesterday)
            .cloned()
            .collect();
        if stale.is_empty() {
            return;
        }
        let Some(dir) = crate::profile::clauth_dir().ok() else {
            return;
        };
        if self.speed_ledger.unusable() {
            // A newer file is never re-loaded (G1 residual). An unreadable one
            // is re-loaded when its (ctime, mtime, len) moves — a chmod or a
            // rewrite — or on a 5 min timer, because a cleared EIO moves no
            // stamp. Both exist because a signature alone misses a permission
            // fix (only ctime moves) and a permission fix alone misses a
            // cleared transient I/O error (no stamp moves at all).
            let timer = self.last_reload_ms == 0
                || now_ms.saturating_sub(self.last_reload_ms) >= 5 * 60_000;
            if !self.speed_ledger.newer() && (timer || self.reload_speed_ledger_if_changed(&dir)) {
                self.speed_ledger = crate::token_ledger::SpeedLedger::load(&dir);
                self.unusable_logged = false;
                self.last_reload_ms = now_ms;
            }
            if self.speed_ledger.unusable() {
                if !self.unusable_logged {
                    crate::logline::logline!(
                        "clauth: token_speed.json is newer or unreadable; keeping speed days in the cache"
                    );
                    self.unusable_logged = true;
                }
                return; // the days stay in the cache
            }
        }
        let mut batch = crate::tps::DaySums::new();
        for day in &stale {
            if let Some(accounts) = self.days.remove(day) {
                batch.insert(day.clone(), accounts);
            }
        }
        let changed = self.speed_ledger.record(&batch, today);
        if (changed || self.unsaved) && !self.speed_ledger.save(&dir) {
            if !self.save_failed_logged {
                crate::logline::logline!(
                    "clauth: failed to save token_speed.json; retrying and keeping the days in the cache"
                );
                self.save_failed_logged = true;
            }
            self.unsaved = true;
            for (day, accounts) in batch {
                self.days.insert(day, accounts);
            }
        } else {
            if self.save_failed_logged {
                crate::logline::logline!("clauth: token_speed.json saves recovered");
                self.save_failed_logged = false;
            }
            self.unsaved = false;
        }
    }

    /// True when `token_speed.json`'s (ctime, mtime, len) changed since last
    /// seen. ctime is included because a chmod (the common permission-blip
    /// fix) moves only ctime.
    fn reload_speed_ledger_if_changed(&mut self, dir: &std::path::Path) -> bool {
        let path = dir.join("token_speed.json");
        let sig = std::fs::metadata(&path)
            .ok()
            .map(|m| {
                #[cfg(unix)]
                let ctime = {
                    use std::os::unix::fs::MetadataExt;
                    (m.ctime() as u64, m.ctime_nsec() as u64)
                };
                #[cfg(not(unix))]
                let ctime = (0, 0);
                let mtime = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                (ctime.0, ctime.1, mtime, m.len())
            })
            .unwrap_or((0, 0, 0, 0));
        if sig != self.speed_ledger_sig {
            self.speed_ledger_sig = sig;
            true
        } else {
            false
        }
    }

    /// Re-walk every transcript store, recording each file's mtime and len. A
    /// NEW file whose mtime predates the cutoff joins at `offset = len` (its
    /// lines cannot be newer than its mtime), so a cold start never reads the
    /// whole history; a hot new file joins at 0 and is read with the cutoff
    /// skip. A file that vanished is dropped.
    fn discover(&mut self, now_ms: u64) {
        let cutoff = cutoff_ms(now_ms);
        let mut found: HashMap<PathBuf, (u64, u64)> = HashMap::new();
        if let Ok(projects) = crate::profile::claude_dir().map(|d| d.join("projects")) {
            walk_jsonl(&projects, WALK_DEPTH, &mut found);
        }
        // Isolated runtimes keep their own store under `profiles/<name>/runtime-isolated-<sid>/projects`.
        if let Ok(profiles) = crate::profile::clauth_dir().map(|d| d.join("profiles"))
            && let Ok(entries) = std::fs::read_dir(&profiles)
        {
            for profile in entries.flatten() {
                let Ok(children) = std::fs::read_dir(profile.path()) else {
                    continue;
                };
                for child in children.flatten() {
                    let Some(name) = child.file_name().to_str().map(str::to_string) else {
                        continue;
                    };
                    if !name.starts_with("runtime-isolated") {
                        continue;
                    }
                    if crate::runtime::sid_of_runtime_dir_name(&name).is_none() {
                        continue;
                    }
                    let projects = child.path().join("projects");
                    if projects.is_dir() {
                        walk_jsonl(&projects, WALK_DEPTH, &mut found);
                    }
                }
            }
        }
        self.files.retain(|path, _| found.contains_key(path));
        self.last_user
            .retain(|path, _| self.files.contains_key(path));
        for (path, (mtime, len)) in found {
            match self.files.entry(path) {
                Entry::Occupied(mut e) => {
                    e.get_mut().mtime_ms = mtime;
                    e.get_mut().len = len;
                }
                Entry::Vacant(v) => {
                    let offset = if mtime < cutoff { len } else { 0 };
                    v.insert(FileEntry {
                        offset,
                        mtime_ms: mtime,
                        len,
                    });
                }
            }
        }
    }

    /// Tail only the hot set — files whose recorded mtime is within the
    /// retention cutoff — so an unchanged tree costs nothing but a map lookup
    /// per known file.
    fn tail_all(&mut self, now_ms: u64) {
        let cutoff = cutoff_ms(now_ms);
        let hot: Vec<(PathBuf, u64)> = self
            .files
            .iter()
            .filter(|(_, e)| e.mtime_ms >= cutoff)
            .map(|(p, e)| (p.clone(), e.offset))
            .collect();
        for (path, offset) in hot {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let len = meta.len();
            let (start, first) = if len < offset {
                // Truncated/replaced: restart this file from byte 0.
                (0, true)
            } else if len == offset {
                continue;
            } else {
                // A read starting at 0 is a first read: apply the cutoff skip.
                (offset, offset == 0)
            };
            let new_offset = self.read_append(&path, start, now_ms, first);
            if let Some(e) = self.files.get_mut(&path) {
                e.offset = new_offset;
            }
        }
    }

    /// Read complete lines from `start` and fold them into the response set.
    /// Returns the offset to resume from: only whole (newline-terminated) lines
    /// are consumed, so a line torn mid-write is re-read on the next tick.
    fn read_append(&mut self, path: &Path, start: u64, now_ms: u64, first: bool) -> u64 {
        let Ok(mut file) = std::fs::File::open(path) else {
            return start;
        };
        if file.seek(SeekFrom::Start(start)).is_err() {
            return start;
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            return start;
        }
        let key = session_key(path).unwrap_or_default();
        let account = self.attribute(path, &key);
        self.session_path
            .entry(key.clone())
            .or_insert_with(|| path.to_path_buf());
        let cutoff = if first { cutoff_ms(now_ms) } else { 0 };
        let mut consumed = 0usize;
        for seg in buf
            .split_inclusive(|&b| b == b'\n')
            .filter(|s| s.last() == Some(&b'\n'))
        {
            consumed += seg.len();
            let line = strip_crlf(seg);
            self.process_line(path, &key, account.as_deref(), line, cutoff);
        }
        start + consumed as u64
    }

    fn process_line(
        &mut self,
        path: &Path,
        key: &str,
        attr: Option<&str>,
        line: &str,
        cutoff_ms: u64,
    ) {
        let Some(parsed) = parse_line(line) else {
            return;
        };
        match parsed {
            Parsed::User { ts } => {
                self.last_user.insert(path.to_path_buf(), ts);
            }
            Parsed::Assistant {
                ts,
                id,
                model,
                output,
            } => {
                // A pre-cutoff user line still updates the span start; a pre-cutoff
                // response is simply not retained.
                if ts < cutoff_ms {
                    return;
                }
                let user_ts = self.last_user.get(path).copied().unwrap_or(0);
                let msg_key = response_key(key, id.as_deref(), user_ts);
                let start_ms = self.last_user.get(path).copied();
                let account = attr
                    .map(str::to_owned)
                    .unwrap_or_else(|| UNATTRIBUTED.to_owned());
                let resp = upsert_response(
                    &mut self.responses,
                    msg_key,
                    Resp {
                        session: key.to_owned(),
                        account,
                        model,
                        start_ms,
                        end_ms: ts,
                        output,
                        day: day_of_ms(ts),
                        added: false,
                        added_output: 0,
                        added_gen_ms: 0,
                    },
                );
                // Lines at or before the watermark were counted by the previous
                // producer: record this response's counted baseline (its output,
                // span and the day those sums sit on) without touching `days`.
                if ts <= self.watermark_ms && !resp.added {
                    if let Some((_, span)) = resp.span() {
                        resp.added_output = resp.output;
                        resp.added_gen_ms = span;
                    }
                    resp.day = day_of_ms(resp.end_ms);
                    return;
                }
                resp.added = true;
                // A response whose end crossed UTC midnight moves its already-added
                // sums to the end day (the ledger keys by end).
                let end_day = day_of_ms(resp.end_ms);
                if end_day != resp.day {
                    let had = resp.added_output > 0 || resp.added_gen_ms > 0;
                    if had {
                        let old = self
                            .days
                            .get_mut(&resp.day)
                            .and_then(|a| a.get_mut(&resp.account))
                            .and_then(|m| m.get_mut(&resp.model));
                        if let Some(old) = old {
                            old.output = old.output.saturating_sub(resp.added_output);
                            old.gen_ms = old.gen_ms.saturating_sub(resp.added_gen_ms);
                            let entry = self
                                .days
                                .entry(end_day.clone())
                                .or_default()
                                .entry(resp.account.clone())
                                .or_default()
                                .entry(resp.model.clone())
                                .or_default();
                            entry.output = entry.output.saturating_add(resp.added_output);
                            entry.gen_ms = entry.gen_ms.saturating_add(resp.added_gen_ms);
                        }
                    }
                    resp.day = end_day;
                }
                // A later line folds only its delta, under the account frozen at
                // first read and the response's current day. A span-less
                // response is out of day sums entirely (a day sum is a speed
                // basis: output paired with gen_ms).
                let Some((_, span)) = resp.span() else {
                    return;
                };
                let d_out = resp.output.saturating_sub(resp.added_output);
                let d_gen = span.saturating_sub(resp.added_gen_ms);
                if d_out > 0 || d_gen > 0 {
                    let entry = self
                        .days
                        .entry(resp.day.clone())
                        .or_default()
                        .entry(resp.account.clone())
                        .or_default()
                        .entry(resp.model.clone())
                        .or_default();
                    entry.output = entry.output.saturating_add(d_out);
                    entry.gen_ms = entry.gen_ms.saturating_add(d_gen);
                    resp.added_output = resp.output;
                    resp.added_gen_ms = span;
                }
            }
        }
    }

    fn build_snapshot(&mut self, now_ms: u64) -> Snapshot {
        let h1_start = now_ms.saturating_sub(H1_WINDOW_MS);
        // 1. which sessions are h1-touched (owned, so the borrow ends here).
        let h1_sessions: HashSet<String> = self
            .responses
            .values()
            .filter(|r| r.end_ms >= h1_start)
            .map(|r| r.session.clone())
            .collect();
        // 2. resolve each h1-touched session's CURRENT member. The owner store is
        // batch-loaded once, only when its mtime moved and there are sessions to
        // resolve; isolated sessions read their live row (fresh).
        if !h1_sessions.is_empty() && self.store_owners_mtime != self.store_mtime {
            self.store_owners = crate::sessions::owners_of(h1_sessions.iter().cloned());
            self.store_owners_mtime = self.store_mtime;
        }
        let mut current: HashMap<String, Option<String>> = HashMap::new();
        for sid in &h1_sessions {
            let path = self.session_path.get(sid).cloned();
            let account = match path.as_deref() {
                Some(p) if let Some((isid, profile)) = isolated_sid(p) => {
                    if let Some(row) = crate::live_sessions::get(&isid) {
                        Some(row.current_member.unwrap_or(row.start_profile))
                    } else {
                        crate::hook_note::resolved_account(sid)
                            .or_else(|| self.store_owners.get(sid).cloned().flatten())
                            .or(Some(profile))
                    }
                }
                _ => crate::hook_note::resolved_account(sid)
                    .or_else(|| self.store_owners.get(sid).cloned().flatten()),
            };
            current.insert(sid.clone(), account);
        }
        // 3. group responses (immutable borrow, after the mutation).
        let mut by_account: BTreeMap<String, Vec<&Resp>> = BTreeMap::new();
        let mut by_session: BTreeMap<&str, Vec<&Resp>> = BTreeMap::new();
        let mut all: Vec<&Resp> = Vec::new();
        for r in self.responses.values() {
            all.push(r);
            if r.end_ms >= h1_start {
                by_session.entry(r.session.as_str()).or_default().push(r);
                if r.account != UNATTRIBUTED {
                    by_account.entry(r.account.clone()).or_default().push(r);
                }
            }
        }
        let mut sessions = BTreeMap::new();
        for (sid, rs) in by_session {
            sessions.insert(
                sid.to_owned(),
                SessionFigures {
                    account: current.get(sid).cloned().flatten(),
                    figures: figures_for(&rs, now_ms),
                },
            );
        }
        let mut accounts = BTreeMap::new();
        for (name, rs) in by_account {
            accounts.insert(name, figures_for(&rs, now_ms));
        }
        Snapshot {
            version: CACHE_VERSION,
            generated_at: iso_ms_utc(now_ms),
            total: figures_for(&all, now_ms),
            accounts,
            sessions,
            days: self.days.clone(),
        }
    }

    /// The account a transcript counts under. An isolated runtime's store names
    /// the runtime sid, whose live row's `current_member` follows swaps; with no
    /// live row (session ended, store not yet rescued) the hook + owner store
    /// answer first, and only then the launch profile the path names. A
    /// shared/bare session maps through the hook's resolved account, else
    /// `session_profiles.json`.
    fn attribute(&mut self, path: &Path, key: &str) -> Option<String> {
        if let Some((sid, profile)) = isolated_sid(path) {
            if let Some(row) = crate::live_sessions::get(&sid) {
                return Some(row.current_member.unwrap_or(row.start_profile));
            }
            return self.owner_of_memoized(key).or(Some(profile));
        }
        self.owner_of_memoized(key)
    }

    /// The hook's `resolved` is read fresh each lookup (a small file); only the
    /// `session_profiles.json` fallback is memoized, gated on the store's mtime.
    fn owner_of_memoized(&mut self, key: &str) -> Option<String> {
        if let Some(hook) = crate::hook_note::resolved_account(key) {
            return Some(hook);
        }
        if let Some(memo) = self.owner_cache.get(key)
            && memo.store_mtime == self.store_mtime
        {
            return memo.owner.clone();
        }
        let owner = crate::sessions::owner_of(key);
        self.owner_cache.insert(
            key.to_owned(),
            OwnerMemo {
                owner: owner.clone(),
                store_mtime: self.store_mtime,
            },
        );
        owner
    }
}

/// One transcript's whole-life sums over every finished response (a finished
/// delegate job's own average); `None` when unreadable or holding none.
pub(crate) fn transcript_speed(path: &Path) -> Option<DaySum> {
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let key = session_key(path).unwrap_or_default();
    let mut responses: HashMap<String, Resp> = HashMap::new();
    let mut last_user: Option<u64> = None;
    for seg in buf
        .split_inclusive(|&b| b == b'\n')
        .filter(|s| s.last() == Some(&b'\n'))
    {
        let line = strip_crlf(seg);
        let Some(parsed) = parse_line(line) else {
            continue;
        };
        match parsed {
            Parsed::User { ts } => last_user = Some(ts),
            Parsed::Assistant {
                ts,
                id,
                model,
                output,
            } => {
                let msg_key = response_key(&key, id.as_deref(), last_user.unwrap_or(0));
                upsert_response(
                    &mut responses,
                    msg_key,
                    Resp {
                        session: key.clone(),
                        account: UNATTRIBUTED.to_owned(),
                        model,
                        start_ms: last_user,
                        end_ms: ts,
                        output,
                        day: String::new(),
                        added: false,
                        added_output: 0,
                        added_gen_ms: 0,
                    },
                );
            }
        }
    }
    let mut sum = DaySum::default();
    for r in responses.values() {
        if let Some((_, span)) = r.span() {
            sum.output = sum.output.saturating_add(r.output);
            sum.gen_ms = sum.gen_ms.saturating_add(span);
        }
    }
    (sum.output > 0 || sum.gen_ms > 0).then_some(sum)
}

/// The producer's cache, `None` when absent, unreadable, or written by a newer
/// version.
pub(crate) fn load_cache() -> Option<Snapshot> {
    let path = crate::profile::clauth_dir().ok()?.join(TPS_CACHE_FILE);
    let bytes = std::fs::read(path).ok()?;
    let snap: Snapshot = serde_json::from_slice(&bytes).ok()?;
    (snap.version <= CACHE_VERSION).then_some(snap)
}

/// `0` at zero, one decimal below 10, whole from 10. Branches on the rounded
/// value so `[9.95, 10)` reads "10", never "10.0".
pub(crate) fn format_figure(v: f64) -> String {
    let rounded = (v * 10.0).round() / 10.0;
    if rounded == 0.0 {
        "0".to_owned()
    } else if rounded < 10.0 {
        format!("{rounded:.1}")
    } else {
        format!("{rounded:.0}")
    }
}

/// A rate figure for a fixed-width cell: `format_figure` while it fits
/// `width`, a whole-`k` form (`13k`) once it would not, capped at `999k` for a
/// figure ≥ 999 500 so a number is never truncated into `…`. The overview's
/// t/s cell and the header's tok/s chip share this one formatter, so the chip's
/// figure stays within its four-cell ceiling past `9999`.
pub(crate) fn format_figure_capped(v: f64, width: usize) -> String {
    if v >= 999_500.0 {
        return "999k".to_owned();
    }
    let figure = format_figure(v);
    if figure.chars().count() > width {
        format!("{:.0}k", v / 1000.0)
    } else {
        figure
    }
}

// ── helpers ───────────────────────────────────────────────────────────────────

enum Parsed {
    User {
        ts: u64,
    },
    Assistant {
        ts: u64,
        /// The raw `message.id`, `None` when absent (keyed by path + user ts).
        id: Option<String>,
        model: String,
        output: u64,
    },
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireLine {
    timestamp: Option<String>,
    message: Option<WireMsg>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireMsg {
    id: Option<String>,
    model: Option<String>,
    role: Option<String>,
    usage: Option<WireUsage>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct WireUsage {
    output_tokens: u64,
}

/// Parse one JSONL line into a user line or an assistant usage line. `None`
/// for a malformed/timestamp-less/message-less line, a placeholder model, or a
/// zero-output line (the synthetic API-error shape).
fn parse_line(line: &str) -> Option<Parsed> {
    let wire: WireLine = serde_json::from_str(line).ok()?;
    let ts = parse_ts(wire.timestamp.as_deref()?)?;
    let msg = wire.message.as_ref()?;
    if msg.role.as_deref() == Some("user") {
        return Some(Parsed::User { ts });
    }
    let usage = msg.usage.as_ref()?;
    if usage.output_tokens == 0 {
        return None;
    }
    let raw = msg.model.as_deref().unwrap_or("unknown");
    let model = crate::providers::deepseek_served_model(raw, msg.id.as_deref().unwrap_or_default())
        .unwrap_or(raw)
        .to_owned();
    if crate::tokens::is_placeholder_model(&model) {
        return None;
    }
    Some(Parsed::Assistant {
        ts,
        id: msg
            .id
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        model,
        output: usage.output_tokens,
    })
}

/// The dedup key for a response: the message id, or `(session, preceding user
/// ts)` for an id-less line so its growing partials share one key.
fn response_key(session: &str, id: Option<&str>, user_ts: u64) -> String {
    match id {
        Some(id) => id.to_owned(),
        None => format!("{session}@{user_ts}"),
    }
}

/// The shared fold both [`process_line`] and [`transcript_speed`] drive, so the
/// span/attribution/partial rules cannot drift between them. A line whose output
/// is at least the largest seen so far updates `output` and moves `end_ms` to the
/// later of the stored end and this line's timestamp; a smaller output changes
/// nothing.
fn upsert_response(
    responses: &mut HashMap<String, Resp>,
    key: String,
    candidate: Resp,
) -> &mut Resp {
    let output = candidate.output;
    let end_ms = candidate.end_ms;
    let resp = responses.entry(key).or_insert(candidate);
    if output >= resp.output {
        resp.output = output;
        resp.end_ms = resp.end_ms.max(end_ms);
    }
    resp
}

/// RFC 3339 (with or without a `Z`/offset, milliseconds) → epoch milliseconds.
fn parse_ts(ts: &str) -> Option<u64> {
    let dt = chrono::DateTime::parse_from_rfc3339(ts).ok()?;
    Some(dt.timestamp_millis().max(0) as u64)
}

fn day_of_ms(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn iso_ms_utc(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3f+00:00").to_string())
        .unwrap_or_default()
}

/// The oldest timestamp a cold-start read retains: the earlier of "now - 1 h"
/// and yesterday's UTC midnight — every line the live/5m/1h windows or today's
/// and yesterday's day sums need.
fn cutoff_ms(now_ms: u64) -> u64 {
    let today_midnight = now_ms - (now_ms % DAY_MS);
    (today_midnight - DAY_MS).min(now_ms.saturating_sub(H1_WINDOW_MS))
}

fn mtime_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn path_mtime_ms(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| mtime_of(&m))
}

/// `~/.clauth/session_profiles.json`'s mtime; the same path
/// `sessions::store_path` uses.
fn store_mtime_now() -> u64 {
    crate::profile::clauth_dir()
        .ok()
        .map(|d| d.join("session_profiles.json"))
        .and_then(|p| path_mtime_ms(&p))
        .unwrap_or(0)
}

/// Recursively collect `*.jsonl` paths with their mtime (epoch ms) and length.
fn walk_jsonl(dir: &Path, depth: usize, out: &mut HashMap<PathBuf, (u64, u64)>) {
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            walk_jsonl(&path, depth - 1, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            out.insert(path, (mtime_of(&meta), meta.len()));
        }
    }
}

/// Strip a trailing `\r\n` or `\n` off a raw line; non-UTF-8 reads as empty.
fn strip_crlf(seg: &[u8]) -> &str {
    let mut s = seg;
    if s.last() == Some(&b'\n') {
        s = &s[..s.len() - 1];
    }
    if s.last() == Some(&b'\r') {
        s = &s[..s.len() - 1];
    }
    std::str::from_utf8(s).unwrap_or("")
}

/// The session a transcript belongs to: a subagent file's parent dir, else the
/// file stem. Mirrors the path → session attribution of the stores.
fn session_key(path: &Path) -> Option<String> {
    let comps: Vec<&str> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect();
    if let Some(pos) = comps.iter().rposition(|c| *c == "subagents")
        && pos >= 1
    {
        return Some(comps[pos - 1].to_owned());
    }
    path.file_stem()?
        .to_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The profile and runtime sid an isolated store's path names, `None` for the
/// global store or a path outside `~/.clauth/profiles`.
fn isolated_sid(path: &Path) -> Option<(String, String)> {
    let profiles_root = crate::profile::clauth_dir().ok()?.join("profiles");
    let rel = path.strip_prefix(&profiles_root).ok()?;
    let mut comps = rel.components();
    let name = comps.next()?.as_os_str().to_str()?.to_owned();
    let runtime = comps.next()?.as_os_str().to_str()?;
    let sid = crate::runtime::sid_of_runtime_dir_name(runtime)?;
    Some((sid, name))
}

/// Windowed figures over a set of responses: speed = output ÷ generation
/// seconds of the responses whose span touches the window (absent when none);
/// rate = output generated inside the window ÷ its length.
fn figures_for(rs: &[&Resp], now_ms: u64) -> Figures {
    Figures {
        live: window_figure(rs, now_ms, LIVE_WINDOW_MS),
        m5: window_figure(rs, now_ms, M5_WINDOW_MS),
        h1: window_figure(rs, now_ms, H1_WINDOW_MS),
    }
}

fn window_figure(rs: &[&Resp], now_ms: u64, len_ms: u64) -> Figure {
    let win_start = now_ms.saturating_sub(len_ms);
    let mut out = 0u64;
    let mut gen_ms = 0u64;
    let mut tok_ms = 0.0f64;
    for r in rs {
        match r.span() {
            Some((start, span)) => {
                if r.end_ms < win_start || start > now_ms {
                    continue;
                }
                out += r.output;
                gen_ms += span;
                let ov_start = start.max(win_start);
                let ov_end = r.end_ms.min(now_ms);
                if ov_end > ov_start {
                    tok_ms += r.output as f64 * (ov_end - ov_start) as f64 / span as f64;
                }
            }
            None => {
                // No span: its tokens land as a point at `end_ms` and count
                // whole when that instant is inside the window.
                if r.end_ms >= win_start && r.end_ms <= now_ms {
                    tok_ms += r.output as f64;
                }
            }
        }
    }
    Figure {
        speed: (gen_ms > 0).then(|| out as f64 / (gen_ms as f64 / 1000.0)),
        rate: tok_ms / (len_ms as f64 / 1000.0),
    }
}

#[cfg(test)]
#[path = "../tests/inline/tps.rs"]
mod tests;

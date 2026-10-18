//! The mod bridge: `~/.clauth/bridge.sock`, the session a socket peer resolves
//! to, and the control routes that queue commands for a mod.
//!
//! The daemon binds the socket only under `--listen`, below the singleton claim.
//! Every event, poll and command resolves the peer pid back to an
//! [`Instance`] through the [`PeerResolver`] — never taking a session id off the
//! wire — and keys the session per `(pid, procStart, sessionId)` so a recycled
//! pid, a `/clear` or a second live process cannot cross sessions or swallow
//! another session's commands. Linux and macOS only: the whole module is
//! `#[cfg(unix)]` at its declaration in `mod.rs`.

use std::collections::hash_map::Entry;
use std::collections::{HashSet, VecDeque};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::daemon::api::http::sanitize_for_log;
use crate::logline::logline;

/// One event body, in bytes. A mod event is small by construction (a row wake
/// carries its `uuid`, never its content; an ask carries its tool input), so a
/// body past this bound is refused 413 and the mod falls back to Claude Code's
/// own dialog rather than holding a check the daemon cannot relay.
const BODY_LIMIT: usize = 1024 * 1024;

/// An instance whose poll has been silent this long closes: twice the server's
/// poll wait, the bound the mod's `$.http.fetch` stays under.
const SILENCE_LIMIT: Duration = Duration::from_secs(40);

/// The wait the bridge server hands every `GET /poll` that finds no command.
const POLL_WAIT: Duration = Duration::from_secs(20);

/// Cap on one request's head (request line plus headers). A real bridge head
/// is a few hundred bytes, so 16 KiB is far above it and no honest request
/// nears the cap, while a peer cannot make the daemon buffer an unbounded
/// head (SB-10).
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// The whole request's wall-clock budget — head and body together — so a peer
/// trickling one byte just under any per-read bound is cut off here, not after
/// days (SB-10). Deliberately not per read.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// How long one answer write may block: a peer that never reads a large poll
/// answer must not hold a connection thread forever (SB-10).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on concurrent event requests and every request's read phase — the work
/// the bridge does before a poll parks. Commands never arrive over the socket:
/// they leave inside poll answers. A parked poll drops this slot before it
/// parks, so every live session's parked poll never eats the slot an event
/// needs; the size matches the REST API's own connection cap (SB-10).
const MAX_EVENT_CONNECTIONS: usize = 32;

/// Cap on parked polls: each live mod parks one for up to [`POLL_WAIT`]. Far
/// above any realistic session count, while still bounding the threads a
/// same-user flood can park (SB-10's daemon-wide poll cap).
const MAX_PARKED_POLLS: usize = 256;

/// Cap on one instance's queued commands, counted together with any a poll has
/// drained and is still writing (the in-flight count). A live mod drains the
/// queue on every poll, so the queue stays empty or near-empty; the cap only
/// bites a mod that stopped polling, whose commands would otherwise grow
/// without bound while every command route kept answering 200 (SB-10, SB-19).
/// 8192 doubles the write-timeout pin's 4096-command batch, so the cap never
/// bites a live mod while still bounding one stopped-reading instance.
pub(crate) const MAX_QUEUED_COMMANDS: usize = 8192;

/// How long the accept loop pauses after an error it cannot attribute to one
/// connection, so a persistent failure cannot spin a core (mirrors the REST
/// API's accept loop).
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// The two wall-clock bounds on one connection, injected so a test drives a
/// deadline in milliseconds instead of minutes; production passes
/// [`Limits::DEFAULT`] (mirrors the REST API's `Limits`).
#[derive(Clone, Copy)]
struct Limits {
    request_deadline: Duration,
    write_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            request_deadline: REQUEST_DEADLINE,
            write_timeout: WRITE_TIMEOUT,
        }
    }
}

/// Releases one connection-budget slot however the handler thread ends, panic
/// included — a leaked slot would permanently shrink the cap (mirrors the REST
/// API's `ConnectionSlot`).
struct Slot<'a>(&'a AtomicUsize);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// A resolved session: the id the peer's process is in now, and the process
/// start the record carries. The start keys a recycled pid apart from its
/// predecessor; the id keys a `/clear` apart.
#[derive(Clone, Debug)]
pub(crate) struct Instance {
    pub(crate) session_id: String,
    pub(crate) proc_start: String,
}

/// Resolves a socket peer pid to its [`Instance`], or `None` for a pid nothing
/// knows. The daemon fills it with [`production_resolver`]; a test fills it
/// with a table it can rewrite mid-test.
pub(crate) type PeerResolver = Box<dyn Fn(u32) -> Option<Instance> + Send + Sync>;

/// The hub: one per daemon, holding every mod instance in memory (never a file,
/// never a `live_sessions` row — SB-5) and the one place a command becomes a
/// queued mod command.
///
/// The hub mutex is a leaf: nothing ranked or flocked is acquired under it, so
/// it needs no rank. The resolver and the device-store read do file IO under
/// it — never a lock, which is what the leaf property protects.
pub(crate) struct Hub {
    listening: bool,
    resolver: PeerResolver,
    inner: Mutex<Inner>,
    changed: Condvar,
    /// Test-only: the virtual clock's offset past [`Instant::now`], so a test
    /// pins the silence close without sleeping 40 s.
    #[cfg(test)]
    clock_offset: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct Inner {
    instances: std::collections::HashMap<u32, Live>,
    /// The next [`Live::generation`]; bumped only when a new instance is made.
    next_generation: u64,
    /// Pids already logged as unknown, for the SB-3 "not registered, logged
    /// once" memo.
    unknown_logged: HashSet<u32>,
}

/// One live claude instance, keyed by its peer pid. Liveness is the poll:
/// `last_poll` moves on every poll, and a poll silent past [`SILENCE_LIMIT`]
/// closes the instance (SB-13).
struct Live {
    proc_start: String,
    session_id: String,
    seq_high: u64,
    open_asks: HashSet<String>,
    closed_asks: HashSet<String>,
    commands: VecDeque<Value>,
    /// Commands a poll has drained and is still writing its answer for. They
    /// stay counted against [`MAX_QUEUED_COMMANDS`] until the write settles, so
    /// a write in flight never opens headroom a command route fills past the
    /// cap (A#1).
    inflight: usize,
    /// Bumped whenever a poll parks on this instance, so a replaced poll can
    /// tell it was replaced and stop waiting.
    poll_epoch: u64,
    /// Monotonic per hub: a parked poll captures this and compares on wake, so
    /// a `/clear` or recycled pid that replaced the instance never feeds its
    /// commands to the abandoned poll (SB-4).
    generation: u64,
    last_poll: Instant,
}

impl Live {
    fn new(resolved: &Instance, now: Instant, generation: u64) -> Self {
        Self {
            proc_start: resolved.proc_start.clone(),
            session_id: resolved.session_id.clone(),
            seq_high: 0,
            open_asks: HashSet::new(),
            closed_asks: HashSet::new(),
            commands: VecDeque::new(),
            inflight: 0,
            poll_epoch: 0,
            generation,
            last_poll: now,
        }
    }

    /// The commands this instance owns right now: its queued commands plus any
    /// a poll has drained and is still writing. The cap counts both, so a
    /// stopped-reading mod can never pin more than [`MAX_QUEUED_COMMANDS`]
    /// commands even while one poll answer after another fails to write.
    fn owned(&self) -> usize {
        self.commands.len() + self.inflight
    }
}

/// What [`Hub::deliver`] found on the mod side of a prompt or interrupt.
pub(crate) enum ModDelivery {
    /// Exactly one live instance: the command is queued for its poll.
    Delivered,
    /// Two live instances resolve to the id: refused, never handed to either.
    Ambiguous,
    /// The one live instance's queue is at [`MAX_QUEUED_COMMANDS`]: refused
    /// 503 rather than queued past the cap (A#1).
    Busy,
    /// No live instance: the route falls back to herdr.
    NoMod,
}

/// What [`Hub::answer`] found on the mod side of an ask answer.
pub(crate) enum AskOutcome {
    /// The ask was open; the answer is queued and the ask closes.
    Queued,
    /// Two live instances resolve to the id.
    Ambiguous,
    /// The ask is open but the instance's queue is at [`MAX_QUEUED_COMMANDS`]:
    /// refused 503, the ask stays open for a retry (A#1).
    Busy,
    /// The ask was answered (or closed at the desk) already.
    AskClosed,
    /// The instance never held that ask id.
    AskNotFound,
    /// No live instance for the id.
    NoSession,
}

/// The commands one poll drained, kept so `serve` can put them back when the
/// answer write fails (A#1). `generation` names the instance they came from: a
/// closed or replaced instance drops them rather than leaking them into its
/// successor's queue, and `inflight` stays counted on the live instance until
/// the write settles.
struct Drained {
    peer_pid: u32,
    generation: u64,
    commands: Vec<Value>,
}

/// One poll's full answer: the wire pair every caller reads, plus the drained
/// commands only `serve` uses to requeue on a failed write.
struct PollAnswer {
    status: u16,
    body: Value,
    drained: Option<Drained>,
}

impl PollAnswer {
    /// An answer that drained nothing, so there is nothing to requeue.
    fn empty(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            drained: None,
        }
    }

    /// A 200 carrying `commands`, and the drained token when it is non-empty.
    fn commands(peer_pid: u32, generation: u64, remote: bool, commands: Vec<Value>) -> Self {
        let body = poll_body(remote, &commands);
        let drained = (!commands.is_empty()).then(|| Drained {
            peer_pid,
            generation,
            commands,
        });
        Self {
            status: 200,
            body,
            drained,
        }
    }
}

impl Hub {
    pub(crate) fn new(listening: bool, resolver: PeerResolver) -> Arc<Self> {
        Arc::new(Self {
            listening,
            resolver,
            inner: Mutex::new(Inner::default()),
            changed: Condvar::new(),
            #[cfg(test)]
            clock_offset: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The hub's clock. Test builds move it with [`advance_clock`]; production
    /// runs on the real monotonic clock.
    fn now(&self) -> Instant {
        #[cfg(test)]
        {
            Instant::now() + Duration::from_nanos(self.clock_offset.load(Ordering::Relaxed))
        }
        #[cfg(not(test))]
        {
            Instant::now()
        }
    }

    /// Move the hub's virtual clock forward, so a test drives the silence close
    /// ([`SILENCE_LIMIT`]) without sleeping.
    #[cfg(test)]
    pub(crate) fn advance_clock(&self, delta: Duration) {
        self.clock_offset
            .fetch_add(delta.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Test-only: wait until a poll has parked on `pid`'s instance — its poll
    /// epoch reached `min_epoch` — signalled through `changed`, so a test
    /// orders two threads by a flag rather than a sleep (B#4). Returns `false`
    /// when the timeout elapses first. Sound because a poll holds the hub mutex
    /// from its epoch bump through `Condvar::wait_timeout`, so the epoch is
    /// visible here exactly once the poll is parked.
    #[cfg(test)]
    pub(crate) fn wait_until_parked(&self, pid: u32, min_epoch: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        while guard
            .instances
            .get(&pid)
            .is_none_or(|live| live.poll_epoch < min_epoch)
        {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (g, _) = self
                .changed
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(|p| p.into_inner());
            guard = g;
        }
        true
    }

    /// One event from the mod at `peer_pid`, resolved to a session and handled.
    pub(crate) fn event(&self, peer_pid: u32, body: &[u8]) -> (u16, Value) {
        if body.len() > BODY_LIMIT {
            logline!(
                "clauth bridge: peer {peer_pid} refused: body_len={}",
                body.len()
            );
            return (413, error_value("body_too_large"));
        }
        let Some(resolved) = (self.resolver)(peer_pid) else {
            self.log_unknown_peer(peer_pid);
            return (403, error_value("unknown_peer"));
        };
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return (400, error_value("bad_request"));
        };
        let Some(named) = value.get("session").and_then(Value::as_str) else {
            return (400, error_value("bad_request"));
        };
        let Some(seq) = value.get("seq").and_then(Value::as_u64) else {
            return (400, error_value("bad_request"));
        };
        if named != resolved.session_id {
            logline!(
                "clauth bridge: peer {peer_pid} refused session '{}' (resolved '{}') body_len={}",
                sanitize_for_log(named),
                sanitize_for_log(&resolved.session_id),
                body.len()
            );
            return (403, error_value("session_mismatch"));
        }
        // Validate the kind and its fields before the sequence advances or the
        // instance is created (A#13): a refused event must not consume its seq.
        let kind = match value.get("kind").and_then(Value::as_str) {
            Some(k @ ("start" | "turn_start" | "turn_end" | "append" | "end")) => k,
            Some("ask_opened") if value.get("ask").and_then(Value::as_str).is_some() => {
                "ask_opened"
            }
            Some("ask_closed") if value.get("ask").and_then(Value::as_str).is_some() => {
                "ask_closed"
            }
            _ => return (400, error_value("bad_request")),
        };

        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = self.now();
        Self::close_silent(&mut inner, now);

        // `end` goes through the same reconcile path so a moved record closes
        // the old instance before the sequence is judged (A#14), then removes
        // and wakes the instance's parked poll.
        if kind == "end" {
            let stale = {
                let live = reconcile(&mut inner, peer_pid, &resolved, now);
                seq <= live.seq_high
            };
            if stale {
                return (409, error_value("stale_sequence"));
            }
            inner.instances.remove(&peer_pid);
            self.changed.notify_all();
            return (200, json!({ "ok": true }));
        }

        let live = reconcile(&mut inner, peer_pid, &resolved, now);
        if seq <= live.seq_high {
            return (409, error_value("stale_sequence"));
        }
        live.seq_high = seq;

        match kind {
            "start" | "turn_start" | "turn_end" | "append" => (200, json!({ "ok": true })),
            "ask_opened" => {
                let ask = value.get("ask").and_then(Value::as_str).unwrap_or_default();
                // The device store is read only for an `ask_opened` (A#15),
                // once, and only a held ask is recorded open (A#5, SB-19): the
                // mod passed an unheld ask to Claude Code's own dialog, so a
                // later answer reads `ask_not_found`.
                let hold = self.remote_flag();
                if hold {
                    live.open_asks.insert(ask.to_string());
                    live.closed_asks.remove(ask);
                }
                (200, json!({ "ok": true, "remote": hold, "hold": hold }))
            }
            "ask_closed" => {
                let ask = value.get("ask").and_then(Value::as_str).unwrap_or_default();
                live.open_asks.remove(ask);
                live.closed_asks.insert(ask.to_string());
                (200, json!({ "ok": true }))
            }
            _ => unreachable!("the kind was validated above"),
        }
    }

    /// One poll from the mod at `peer_pid` naming `session`, blocking up to
    /// `wait` for a command. Commands are delivered once, to their own
    /// instance's queue alone. A direct caller (a test) that receives the
    /// answer has the write succeed by construction, so its drained commands
    /// are consumed here. Production polls only through [`poll_answer`], so the
    /// consume-or-requeue choice stays in `serve`.
    #[cfg(test)]
    pub(crate) fn poll(&self, peer_pid: u32, session: &str, wait: Duration) -> (u16, Value) {
        let PollAnswer {
            status,
            body,
            drained,
        } = self.poll_answer(peer_pid, session, wait);
        if let Some(drained) = drained {
            self.consume(drained);
        }
        (status, body)
    }

    /// [`poll`] with the drained commands carried back, so `serve` can requeue
    /// them when the answer write fails rather than letting a 200'd command
    /// vanish (A#1).
    fn poll_answer(&self, peer_pid: u32, session: &str, wait: Duration) -> PollAnswer {
        let Some(resolved) = (self.resolver)(peer_pid) else {
            self.log_unknown_peer(peer_pid);
            return PollAnswer::empty(403, error_value("unknown_peer"));
        };
        if resolved.session_id != session {
            logline!(
                "clauth bridge: peer {peer_pid} refused poll for '{}' (resolved '{}')",
                sanitize_for_log(session),
                sanitize_for_log(&resolved.session_id)
            );
            return PollAnswer::empty(403, error_value("session_mismatch"));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let now = self.now();
        Self::close_silent(&mut inner, now);
        let live = reconcile(&mut inner, peer_pid, &resolved, now);
        live.last_poll = now;
        // The device store is read once per poll answer (A#15), not per wake.
        let remote = self.remote_flag();
        let commands: Vec<Value> = live.commands.drain(..).collect();
        live.inflight += commands.len();
        if !commands.is_empty() || wait.is_zero() {
            return PollAnswer::commands(peer_pid, live.generation, remote, commands);
        }

        // Park: this poll replaces any prior parked poll of the same instance,
        // and a parked poll only ever drains its own instance's queue (SB-4).
        live.poll_epoch = live.poll_epoch.wrapping_add(1);
        let my_epoch = live.poll_epoch;
        let my_generation = live.generation;
        let _ = live;
        // Wake the poll this one replaced (A#2).
        self.changed.notify_all();
        let mut guard = inner;
        let deadline = Instant::now() + wait;
        loop {
            let now = Instant::now();
            if now >= deadline {
                return PollAnswer::empty(200, poll_body(remote, &[]));
            }
            let (g, timeout) = self
                .changed
                .wait_timeout(guard, deadline - now)
                .unwrap_or_else(|p| p.into_inner());
            guard = g;
            let (commands, replaced) = match guard.instances.get_mut(&peer_pid) {
                // The instance was replaced (a `/clear` or recycled pid): empty,
                // never the new instance's commands (A#3).
                Some(live) if live.generation != my_generation => (Vec::new(), true),
                // A newer poll parked on the same instance: empty, never a
                // command the replacement owns (A#1).
                Some(live) if live.poll_epoch != my_epoch => (Vec::new(), true),
                Some(live) => {
                    let commands: Vec<Value> = live.commands.drain(..).collect();
                    live.inflight += commands.len();
                    (commands, false)
                }
                None => (Vec::new(), true),
            };
            if !commands.is_empty() {
                return PollAnswer::commands(peer_pid, my_generation, remote, commands);
            }
            if replaced || timeout.timed_out() {
                return PollAnswer::empty(200, poll_body(remote, &[]));
            }
        }
    }

    /// Mark a poll's drained commands delivered: the write succeeded, so the
    /// commands are consumed (dropped) and the instance's in-flight count
    /// falls. A replaced or closed instance needs nothing — its count died
    /// with it — and the token's commands drop here either way.
    fn consume(&self, drained: Drained) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(live) = inner.instances.get_mut(&drained.peer_pid) else {
            return;
        };
        if live.generation == drained.generation {
            live.inflight = live.inflight.saturating_sub(drained.commands.len());
        }
    }

    /// Put a poll's drained commands back at the front of their instance's
    /// queue, in order, when the answer write failed (A#1). Only when that
    /// instance is still the live one at `peer_pid`; a closed or replaced
    /// instance drops them — its poll already took them off its own queue and
    /// the mod will re-poll, so neither its successor nor a dead session
    /// inherits them. Never checked against the cap: these commands were
    /// already in the queue and are still counted in `inflight` until now, so
    /// restoring them never adds headroom a command route already used.
    fn requeue(&self, drained: Drained) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(live) = inner.instances.get_mut(&drained.peer_pid) else {
            return;
        };
        if live.generation != drained.generation {
            return;
        }
        live.inflight = live.inflight.saturating_sub(drained.commands.len());
        for command in drained.commands.into_iter().rev() {
            live.commands.push_front(command);
        }
        self.changed.notify_all();
    }

    /// Queue `command` for the named session's live mod instance. Re-resolves
    /// only the instances holding the named id first (A#15), so a moved or
    /// silent instance closes before the count decides (SB-4, SB-13).
    pub(crate) fn deliver(&self, session: &str, command: Value) -> ModDelivery {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::close_silent(&mut inner, self.now());
        self.revalidate_session(&mut inner, session);
        let matching: Vec<u32> = inner
            .instances
            .iter()
            .filter(|(_, live)| live.session_id == session)
            .map(|(pid, _)| *pid)
            .collect();
        match matching.len() {
            0 => ModDelivery::NoMod,
            1 => {
                let live = inner.instances.get_mut(&matching[0]).unwrap_or_else(|| {
                    unreachable!("the matching pid was counted from the same map")
                });
                if live.owned() >= MAX_QUEUED_COMMANDS {
                    return ModDelivery::Busy;
                }
                live.commands.push_back(command);
                self.changed.notify_all();
                ModDelivery::Delivered
            }
            _ => ModDelivery::Ambiguous,
        }
    }

    /// Answer a held ask on the named session's live mod instance: the first
    /// answer wins and closes the ask, whatever its form.
    pub(crate) fn answer(&self, session: &str, ask: &str, command: Value) -> AskOutcome {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::close_silent(&mut inner, self.now());
        self.revalidate_session(&mut inner, session);
        let matching: Vec<u32> = inner
            .instances
            .iter()
            .filter(|(_, live)| live.session_id == session)
            .map(|(pid, _)| *pid)
            .collect();
        match matching.len() {
            0 => AskOutcome::NoSession,
            2.. => AskOutcome::Ambiguous,
            1 => {
                let live = inner.instances.get_mut(&matching[0]).unwrap_or_else(|| {
                    unreachable!("the matching pid was counted from the same map")
                });
                if live.open_asks.contains(ask) {
                    if live.owned() >= MAX_QUEUED_COMMANDS {
                        return AskOutcome::Busy;
                    }
                    live.open_asks.remove(ask);
                    live.closed_asks.insert(ask.to_string());
                    live.commands.push_back(command);
                    self.changed.notify_all();
                    AskOutcome::Queued
                } else if live.closed_asks.contains(ask) {
                    AskOutcome::AskClosed
                } else {
                    AskOutcome::AskNotFound
                }
            }
        }
    }

    /// The session ids with a live mod instance, for the listing's `via`. Every
    /// live peer is re-resolved once — the listing has no named id to scope by
    /// (A#15) — so a moved or silent instance cannot claim its old id.
    pub(crate) fn live_session_ids(&self) -> Vec<String> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        Self::close_silent(&mut inner, self.now());
        let pids: Vec<u32> = inner.instances.keys().copied().collect();
        for pid in pids {
            self.revalidate_pid(&mut inner, pid);
        }
        inner
            .instances
            .values()
            .map(|live| live.session_id.clone())
            .collect()
    }

    /// Whether a remote answer is possible: the daemon's listener is bound and a
    /// control-tier device is paired, read fresh on every call (SB-9, MB-1).
    fn remote_flag(&self) -> bool {
        self.listening && crate::daemon::api::devices::has_control_device()
    }

    /// Log the SB-3 "not registered, logged once" line for `peer_pid`.
    fn log_unknown_peer(&self, peer_pid: u32) {
        let first = self
            .inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .unknown_logged
            .insert(peer_pid);
        if first {
            logline!("clauth bridge: peer {peer_pid} not registered");
        }
    }

    /// Close every instance whose poll is silent past [`SILENCE_LIMIT`]. No
    /// resolver call: liveness is the poll alone (SB-13).
    fn close_silent(inner: &mut Inner, now: Instant) {
        inner
            .instances
            .retain(|_, live| now.saturating_duration_since(live.last_poll) <= SILENCE_LIMIT);
    }

    /// Re-resolve `pid` and close its instance when the record moved (a
    /// recycled pid, a `/clear`), or when the record is gone and the process
    /// is too: a claude that exited without `end` drops its record at exit, and
    /// its instance must not answer 200 `via: mod` into a dead queue (SB-19).
    /// A resolver `None` for a live, same-start process keeps the instance
    /// (SB-13): the record is briefly unreadable, not the session gone.
    fn revalidate_pid(&self, inner: &mut Inner, pid: u32) {
        let moved = match (self.resolver)(pid) {
            None => inner
                .instances
                .get(&pid)
                .is_some_and(|live| process_gone_or_moved(pid, live)),
            Some(resolved) => inner.instances.get(&pid).is_some_and(|live| {
                live.proc_start != resolved.proc_start || live.session_id != resolved.session_id
            }),
        };
        if moved {
            inner.instances.remove(&pid);
        }
    }

    /// Re-resolve only the instances holding `session`, closing the moved ones
    /// before the count decides (A#15, SB-4).
    fn revalidate_session(&self, inner: &mut Inner, session: &str) {
        let pids: Vec<u32> = inner
            .instances
            .iter()
            .filter(|(_, live)| live.session_id == session)
            .map(|(pid, _)| *pid)
            .collect();
        for pid in pids {
            self.revalidate_pid(inner, pid);
        }
    }
}

/// Whether the process behind a resolver-`None` pid is gone or is another
/// process: the record vanished, so the instance closes only when the peer it
/// was keyed to no longer exists (a live, same-start process's brief
/// unreadability keeps it — SB-13). Linux compares field 22 of
/// `/proc/<pid>/stat` against the recorded start (a dead pid fails the read, a
/// recycled pid the compare); macOS compares nothing but liveness, since the
/// `procStart` form there is unmeasured.
fn process_gone_or_moved(pid: u32, live: &Live) -> bool {
    #[cfg(target_os = "linux")]
    {
        !proc_start_matches(pid, &live.proc_start)
    }
    #[cfg(target_os = "macos")]
    {
        let _ = live;
        !crate::runtime::namespaced_keychain_ledger::pid_alive(pid)
    }
}

/// Find or create the instance at `pid`; a record whose id or process start
/// moved closes the old instance first, so a recycled pid or `/clear` never
/// inherits the session that used to be there. A second live instance on an id
/// another instance holds is audited (SB-4).
fn reconcile<'a>(
    inner: &'a mut Inner,
    pid: u32,
    resolved: &Instance,
    now: Instant,
) -> &'a mut Live {
    let moved = inner.instances.get(&pid).is_some_and(|live| {
        live.proc_start != resolved.proc_start || live.session_id != resolved.session_id
    });
    if moved {
        inner.instances.remove(&pid);
    }
    let existed = inner.instances.contains_key(&pid);
    let peers: Vec<u32> = if existed {
        Vec::new()
    } else {
        inner
            .instances
            .iter()
            .filter(|&(other, live)| *other != pid && live.session_id == resolved.session_id)
            .map(|(other, _)| *other)
            .collect()
    };
    let live = match inner.instances.entry(pid) {
        Entry::Occupied(slot) => slot.into_mut(),
        Entry::Vacant(slot) => {
            inner.next_generation = inner.next_generation.wrapping_add(1);
            slot.insert(Live::new(resolved, now, inner.next_generation))
        }
    };
    // A pid that resolves is registered, so forget its "unknown, logged once"
    // memo: the set stays bounded to pids still unknown (A#7).
    inner.unknown_logged.remove(&pid);
    if !peers.is_empty() {
        let mut pids = peers;
        pids.push(pid);
        pids.sort_unstable();
        logline!(
            "clauth bridge: session '{}' now has {} live instances (pids {})",
            sanitize_for_log(&resolved.session_id),
            pids.len(),
            pids.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    live
}

fn error_value(code: &str) -> Value {
    json!({ "error": code })
}

fn poll_body(remote: bool, commands: &[Value]) -> Value {
    json!({ "remote": remote, "commands": commands })
}

/// Bind the bridge socket at `path`: the dir is created 0700 and the socket
/// lands 0600, owned by the daemon alone. A path already holding a live
/// listener is refused; a refused connect proves the file stale, and only then
/// is it unlinked and re-bound (SB-1). The daemon singleton claim is the
/// caller's to hold — this bind's job is the connect probe.
pub(crate) fn bind(path: &Path) -> std::io::Result<UnixListener> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "socket path has no parent",
        )
    })?;
    crate::profile::mkdir_700(dir)?;
    match bind_0600(path) {
        Ok(listener) => Ok(listener),
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Only a refused connect proves the file stale; any other connect
            // error (a mode-000 live socket, say) refuses rather than unlink.
            match UnixStream::connect(path) {
                Ok(_) => Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "a live listener holds the bridge socket",
                )),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    std::fs::remove_file(path)?;
                    bind_0600(path)
                }
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    }
}

fn bind_0600(path: &Path) -> std::io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Serve HTTP/1.1 (`POST /event`, `GET /poll?session=<id>`) over the socket,
/// reading the peer pid off each accepted connection. One thread per
/// connection, so a parked 20 s poll never blocks another connection's event.
pub(crate) fn spawn_server(listener: UnixListener, hub: Arc<Hub>) -> std::io::Result<Server> {
    spawn_server_with_limits(listener, hub, Limits::default())
}

/// The bridge server's join handle and, in test builds, a stop handle so a test
/// that starts a server can join its accept thread before the sandbox drops
/// (A#6). Production holds no handle: the loop runs for the daemon's life and
/// the thread is detached.
pub(crate) struct Server {
    #[cfg(test)]
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(test)]
    stop: TestStop,
}

/// Stops the accept loop so the server thread can be joined (A#6). Test-only:
/// production never stops the loop.
#[cfg(test)]
struct TestStop {
    flag: Arc<AtomicBool>,
    path: Option<PathBuf>,
    /// Live connection threads (spawned, not yet returned): `stop` waits for
    /// this to reach zero before the sandbox drops (A#2).
    conns: Arc<AtomicUsize>,
}

/// How long `stop` waits for in-flight connection threads to finish. One
/// thread lives at most a parked poll's [`POLL_WAIT`] (or a request read plus
/// answer write in production), so 30 s covers every injected test limit with
/// margin. A bound, never a join-forever: a thread that outlives it is left
/// detached rather than hanging the test.
#[cfg(test)]
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Releases one live-connection count when a connection thread ends, panic
/// included — `stop` waits on this count reaching zero (A#2). The accept loop
/// increments before spawning, so the count never under-reports a thread that
/// was spawned but has not yet run.
struct ConnDone<'a>(&'a AtomicUsize);

impl Drop for ConnDone<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
impl Server {
    /// Signal the accept loop and join it and its connection threads. Panics
    /// when the self-connect that must wake the accept loop fails: without the
    /// wake the join would hang, so the loud failure names the gone socket
    /// instead (A#2).
    pub(crate) fn stop(mut self) {
        self.stop_impl(false);
    }

    /// The stop machinery shared with [`Drop`]: `from_drop` keeps it silent —
    /// a panic during unwind would abort — and best-effort where the explicit
    /// `stop` would panic.
    fn stop_impl(&mut self, from_drop: bool) {
        let Some(thread) = self.thread.take() else {
            return; // already stopped
        };
        self.stop.flag.store(true, Ordering::Relaxed);
        // One self-connect wakes `incoming()` out of its accept, so the loop
        // observes the flag and breaks; the connection is never read. A failed
        // connect means the loop cannot be woken, so the join would hang.
        let woke = match &self.stop.path {
            Some(path) => UnixStream::connect(path).is_ok(),
            None => false,
        };
        if !woke {
            let reason = match &self.stop.path {
                Some(path) => format!("the socket at {} is gone or unreachable", path.display()),
                None => "the listener has no path".to_string(),
            };
            if from_drop {
                // Cannot wake the loop; dropping `thread` detaches it rather
                // than joining forever. Say so, but never panic during unwind.
                logline!("clauth bridge: server stop could not wake the accept loop: {reason}");
                return;
            }
            panic!("bridge server stop could not wake the accept loop: {reason}");
        }
        let _ = thread.join();
        // Wait for the in-flight connection threads to finish, bounded (A#2).
        let deadline = Instant::now() + STOP_JOIN_TIMEOUT;
        while self.stop.conns.load(Ordering::Relaxed) != 0 {
            if Instant::now() >= deadline {
                // A thread still running past the bound is left detached. The
                // explicit stop fails loudly naming the count; the Drop path,
                // already unwinding, logs it instead.
                let count = self.stop.conns.load(Ordering::Relaxed);
                if from_drop {
                    logline!("clauth bridge: server stop left {count} live connection(s)");
                    return;
                }
                panic!("bridge server stop timed out with {count} live connection(s)");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[cfg(test)]
impl Drop for Server {
    /// Run on the panic path too: a failing assertion must still stop the
    /// server before the sandbox drops (B#3). Never panics here.
    fn drop(&mut self) {
        self.stop_impl(true);
    }
}

/// The same server over injected [`Limits`], so a test drives a deadline in
/// milliseconds rather than minutes.
fn spawn_server_with_limits(
    listener: UnixListener,
    hub: Arc<Hub>,
    limits: Limits,
) -> std::io::Result<Server> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let accept_flag = Arc::clone(&stop_flag);
    let conns = Arc::new(AtomicUsize::new(0));
    #[cfg(test)]
    let conns_stop = Arc::clone(&conns);
    #[cfg(test)]
    let path = listener
        .local_addr()
        .ok()
        .and_then(|addr| addr.as_pathname().map(PathBuf::from));
    let spawned = std::thread::Builder::new()
        .name("clauth-bridge".to_string())
        .spawn(move || {
            let events = Arc::new(AtomicUsize::new(0));
            let parked = Arc::new(AtomicUsize::new(0));
            for conn in listener.incoming() {
                if accept_flag.load(Ordering::Relaxed) {
                    break;
                }
                let stream = match conn {
                    Ok(stream) => stream,
                    Err(e) => {
                        logline!("clauth bridge: accept failed: {e}");
                        std::thread::sleep(ACCEPT_BACKOFF);
                        continue;
                    }
                };
                // Claim the event slot in the accept loop, BEFORE spawning the
                // per-connection thread (A#4): a reconnect storm is bounded by
                // the cap, never by thread-spawn failure. The thread re-wraps
                // the claim in a [`Slot`] so every exit path releases it.
                let Some(stream) = claim_event(stream, &events, limits) else {
                    continue;
                };
                let hub = Arc::clone(&hub);
                let events_conn = Arc::clone(&events);
                let parked = Arc::clone(&parked);
                let conns_conn = Arc::clone(&conns);
                // Count the connection before the spawn so `stop` never joins
                // past a thread that was spawned but has not yet run; the
                // thread's `ConnDone` releases it however it ends.
                conns.fetch_add(1, Ordering::Relaxed);
                if std::thread::Builder::new()
                    .name("clauth-bridge-conn".to_string())
                    .spawn(move || {
                        let _conn = ConnDone(&conns_conn);
                        let event = Slot(&events_conn);
                        serve(stream, &hub, event, &parked, limits);
                    })
                    .is_err()
                {
                    // The claim was made in the accept loop; a failed spawn never
                    // ran the `Slot` or `ConnDone` guards, so release both here
                    // (A#4).
                    events.fetch_sub(1, Ordering::Relaxed);
                    conns.fetch_sub(1, Ordering::Relaxed);
                    logline!("clauth bridge: failed to spawn a connection thread");
                }
            }
        });
    #[cfg(not(test))]
    let _ = spawned?;
    #[cfg(test)]
    let thread = spawned?;
    Ok(Server {
        #[cfg(test)]
        thread: Some(thread),
        #[cfg(test)]
        stop: TestStop {
            flag: stop_flag,
            path,
            conns: conns_stop,
        },
    })
}

/// Claim one event-budget slot, refusing the connection with a 503 and one log
/// line when the budget is full. The accept loop calls this BEFORE spawning a
/// per-connection thread (A#4); the stream is only handed to a thread when the
/// slot is held.
fn claim_event(mut stream: UnixStream, events: &AtomicUsize, limits: Limits) -> Option<UnixStream> {
    if events.fetch_add(1, Ordering::Relaxed) >= MAX_EVENT_CONNECTIONS {
        events.fetch_sub(1, Ordering::Relaxed);
        logline!(
            "clauth bridge: refused a connection: {MAX_EVENT_CONNECTIONS} concurrent event connections"
        );
        let _ = stream.set_write_timeout(Some(limits.write_timeout));
        write_response(&mut stream, 503, &error_value("busy"));
        None
    } else {
        Some(stream)
    }
}

fn serve(
    mut stream: UnixStream,
    hub: &Arc<Hub>,
    event: Slot<'_>,
    parked: &AtomicUsize,
    limits: Limits,
) {
    // The write bound applies to every answer, the budget refusals included, so
    // a peer that never reads cannot hold the thread in `write!` (A#3).
    if stream
        .set_write_timeout(Some(limits.write_timeout))
        .is_err()
    {
        return;
    }
    let Some(pid) = peer_pid(&stream) else {
        write_response(&mut stream, 403, &error_value("unknown_peer"));
        return;
    };
    let request = match read_request(&mut stream, Instant::now() + limits.request_deadline) {
        Err(ReadError::Eof) => return,
        Err(ReadError::HeadTooLarge) => {
            logline!("clauth bridge: peer {pid} refused: request head over the cap");
            write_response(&mut stream, 400, &error_value("bad_request"));
            return;
        }
        Err(ReadError::Chunked) => {
            logline!("clauth bridge: peer {pid} refused: chunked transfer encoding");
            write_response(&mut stream, 400, &error_value("bad_request"));
            return;
        }
        Err(ReadError::BodyTooLarge(len)) => {
            logline!("clauth bridge: peer {pid} refused: body_len={len}");
            write_response(&mut stream, 413, &error_value("body_too_large"));
            return;
        }
        Err(ReadError::Deadline) => {
            logline!("clauth bridge: peer {pid} refused: request deadline exceeded");
            write_response(&mut stream, 503, &error_value("busy"));
            return;
        }
        Err(ReadError::Malformed) => {
            logline!("clauth bridge: peer {pid} refused: malformed request");
            write_response(&mut stream, 400, &error_value("bad_request"));
            return;
        }
        Ok(request) => request,
    };
    match (request.method.as_str(), request.path.as_str()) {
        ("POST", "/event") => {
            let (status, answer) = hub.event(pid, &request.body);
            write_response(&mut stream, status, &answer);
        }
        ("GET", path) if path.starts_with("/poll") => {
            let session = parse_poll_session(path).unwrap_or_default();
            // Leave the event budget before parking: a parked poll holds the
            // poll budget, never an event slot (SB-10).
            drop(event);
            if parked.fetch_add(1, Ordering::Relaxed) >= MAX_PARKED_POLLS {
                parked.fetch_sub(1, Ordering::Relaxed);
                logline!("clauth bridge: refused a poll: {MAX_PARKED_POLLS} parked polls");
                write_response(&mut stream, 503, &error_value("busy"));
                return;
            }
            let _poll_slot = Slot(parked);
            let PollAnswer {
                status,
                body,
                drained,
            } = hub.poll_answer(pid, session, POLL_WAIT);
            // `written` only means the kernel took the bytes, not that the mod
            // processed them: a mod that closes after a short buffered answer
            // loses those commands, and a failed write settling after a newer
            // poll delivered newer commands requeues the older ones behind them
            // (order holds within the queue, not across that race). An
            // application-level ack per applied command would close both.
            let written = write_response(&mut stream, status, &body);
            match drained {
                // A failed write never delivered them: put them back at the
                // front for the next poll, in order (A#1).
                Some(drained) if !written => hub.requeue(drained),
                Some(drained) => hub.consume(drained),
                None => {}
            }
        }
        _ => {
            write_response(&mut stream, 404, &error_value("not_found"));
        }
    }
}

/// The peer pid of an accepted bridge connection: `SO_PEERCRED` on linux,
/// `LOCAL_PEERPID` on macOS. `None` only when the kernel refuses to say, which
/// reads as an unknown peer rather than trusting any id off the wire.
#[cfg(target_os = "linux")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    #[allow(unsafe_code)]
    // SAFETY: `getsockopt` copies the kernel's credential for this live socket
    // into `cred`, which we own and whose size `len` names; the fd is open and
    // belongs to `stream`.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(cred.pid as u32)
}

#[cfg(target_os = "macos")]
fn peer_pid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    #[allow(unsafe_code)]
    // SAFETY: `getsockopt` copies the kernel's peer pid for this live socket
    // into a `pid_t` we own; the length is that type's, and the fd is open.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut libc::pid_t as *mut libc::c_void,
            &mut len,
        )
    };
    (rc == 0).then_some(pid as u32)
}

struct Request {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Why [`read_request`] could not produce a [`Request`].
#[derive(Debug)]
enum ReadError {
    /// The peer closed cleanly before a request completed: nothing to answer.
    Eof,
    /// The head grew past [`MAX_HEAD_BYTES`].
    HeadTooLarge,
    /// `Transfer-Encoding` was present: chunked is refused, not implemented.
    Chunked,
    /// `Content-Length` named a body past [`BODY_LIMIT`], decided before the
    /// body is read (SB-10).
    BodyTooLarge(usize),
    /// The whole request outlived its deadline (a trickling peer), not one read.
    Deadline,
    /// Truncated, unreadable, or self-contradictory.
    Malformed,
}

/// One deadline-bounded read. The socket's timeout is re-armed to the remaining
/// budget before every read, so the WHOLE request — head and body — is bounded
/// by `deadline`, never a per-read window a trickling peer can reset (SB-10).
fn read_chunk(
    stream: &mut UnixStream,
    deadline: Instant,
    tmp: &mut [u8; 4096],
) -> Result<usize, ReadError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(ReadError::Deadline);
    }
    stream
        .set_read_timeout(Some(remaining))
        .map_err(|_| ReadError::Malformed)?;
    stream.read(tmp).map_err(|e| {
        if matches!(
            e.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        ) {
            ReadError::Deadline
        } else {
            ReadError::Malformed
        }
    })
}

/// Read one request: the head through `\r\n\r\n` bounded by [`MAX_HEAD_BYTES`],
/// a case-insensitive `Content-Length`, then exactly that many body bytes, all
/// under one wall-clock `deadline`. A body past [`BODY_LIMIT`] is refused from
/// the header alone, before any of it is read (SB-10); chunked is refused
/// outright.
fn read_request(stream: &mut UnixStream, deadline: Instant) -> Result<Request, ReadError> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(ReadError::HeadTooLarge);
        }
        let n = read_chunk(stream, deadline, &mut tmp)?;
        if n == 0 {
            return Err(ReadError::Eof);
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().ok_or(ReadError::Malformed)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or(ReadError::Malformed)?.to_string();
    let path = parts.next().ok_or(ReadError::Malformed)?.to_string();
    let mut content_length: Option<usize> = None;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(ReadError::Malformed);
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(ReadError::Chunked);
        }
        if name.eq_ignore_ascii_case("content-length") {
            // `1*DIGIT` only (RFC 9110 §8.6), so a repeated header that
            // disagrees is a hard error rather than a second reading of where
            // one message ends.
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err(ReadError::Malformed);
            }
            let parsed: usize = value.parse().map_err(|_| ReadError::Malformed)?;
            if content_length.is_some_and(|seen| seen != parsed) {
                return Err(ReadError::Malformed);
            }
            content_length = Some(parsed);
        }
    }
    let content_length = content_length.unwrap_or(0);
    if content_length > BODY_LIMIT {
        return Err(ReadError::BodyTooLarge(content_length));
    }
    let body_start = head_end + 4;
    while buf.len() < body_start + content_length {
        let n = read_chunk(stream, deadline, &mut tmp)?;
        if n == 0 {
            return Err(ReadError::Malformed);
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = buf[body_start..body_start + content_length].to_vec();
    Ok(Request { method, path, body })
}

fn parse_poll_session(path: &str) -> Option<&str> {
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == "session").then_some(value)
    })
}

fn write_response(stream: &mut UnixStream, status: u16, body: &Value) -> bool {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .is_ok()
}

/// The production resolver: reads the peer pid's Claude Code session record
/// under the config dir the peer's `live_sessions` row joins (else `~/.claude`)
/// and verifies `procStart` against `/proc/<pid>/stat` field 22 on linux.
pub(crate) fn production_resolver() -> PeerResolver {
    Box::new(|pid| {
        let dir = session_config_dir_for(pid)?;
        let record = crate::session_record::session_record(&dir.join("sessions"), pid)?;
        // The record's `procStart` must equal field 22 of `/proc/<pid>/stat`
        // (measured equal, `docs/domain-knowledge-claude-code.md`); a mismatch
        // is a stale record a recycled pid left, so the peer reads as unknown.
        // The macOS `procStart` form is unmeasured, so nothing there compares it
        // and a recycled pid is not detectable on macOS until it is measured.
        #[cfg(target_os = "linux")]
        if !proc_start_matches(pid, &record.proc_start) {
            return None;
        }
        Some(Instance {
            session_id: record.session_id,
            proc_start: record.proc_start,
        })
    })
}

/// Bind `~/.clauth/bridge.sock` and serve it, returning the hub for the
/// [`ApiContext`]. A bind or spawn failure logs one line naming the path and
/// why, and the daemon runs on with no bridge (`None`) — rotation and fallback
/// never stop for it (SB-1). A `--standby` daemon never reaches this before it
/// takes over: `serve_prepared` runs only below the singleton claim and a
/// promotion.
pub(crate) fn start() -> Option<Arc<Hub>> {
    let hub = Hub::new(true, production_resolver());
    let path = match crate::profile::clauth_dir() {
        Ok(dir) => dir.join("bridge.sock"),
        Err(e) => {
            logline!("clauth daemon: no mod bridge: {e:#}");
            return None;
        }
    };
    let listener = match bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            logline!("clauth daemon: no mod bridge on {}: {e}", path.display());
            return None;
        }
    };
    match spawn_server(listener, Arc::clone(&hub)) {
        Ok(_server) => Some(hub),
        Err(e) => {
            logline!("clauth daemon: no mod bridge: failed to spawn the bridge server: {e}");
            None
        }
    }
}

/// The config dir the peer's session reads: the runtime tree of the
/// `live_sessions` row whose pid is the peer itself (a delegate's row carries
/// the claude pid), else the peer's direct parent when that parent is a
/// `clauth start`/`resume`; `~/.claude` when no row joins (SB-3, SB-5).
fn session_config_dir_for(pid: u32) -> Option<PathBuf> {
    let rows = crate::live_sessions::list();
    let row = rows.iter().find(|row| row.pid == pid).or_else(|| {
        let parent = parent_pid(pid)?;
        if !is_clauth_launch(parent) {
            return None;
        }
        rows.iter().find(|row| row.pid == parent)
    });
    match row {
        Some(row) => {
            let profile = crate::profile::ProfileName::from(row.start_profile.as_str());
            let trees =
                crate::runtime::session_runtime_trees(&profile, row.isolated, &row.session_id)
                    .ok()?;
            trees
                .into_iter()
                .find(|tree| tree.join("sessions").is_dir())
        }
        None => crate::profile::claude_dir().ok(),
    }
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    // Field 4 of `/proc/<pid>/stat` is the ppid; the comm (field 2) may hold
    // spaces and parentheses, so the split runs from the closing parenthesis.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// Whether field 22 of `/proc/<pid>/stat` (the start time in clock ticks since
/// boot) equals the record's `procStart` (measured 2.1.301). Split out so a
/// test can feed it the test process's own `/proc/self/stat`.
#[cfg(target_os = "linux")]
fn proc_start_matches(pid: u32, proc_start: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    let Some(rest) = stat.rsplit_once(')') else {
        return false;
    };
    let Some(field) = rest.1.split_whitespace().nth(19) else {
        return false;
    };
    field == proc_start
}

#[cfg(target_os = "linux")]
fn is_clauth_launch(pid: u32) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let mut args = cmdline.split(|byte| *byte == 0).filter(|a| !a.is_empty());
    let (Some(argv0), Some(argv1)) = (args.next(), args.next()) else {
        return false;
    };
    let name = Path::new(std::ffi::OsStr::from_bytes(argv0)).file_name();
    name == Some(std::ffi::OsStr::new("clauth")) && matches!(argv1, b"start" | b"resume")
}

#[cfg(target_os = "macos")]
fn parent_pid(pid: u32) -> Option<u32> {
    #[allow(unsafe_code)]
    // SAFETY: `proc_bsdinfo` is all integers and fixed-size `c_char` arrays, so
    // the zeroed representation is a valid value and no field is read before
    // `proc_pidinfo` fills the struct.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    #[allow(unsafe_code)]
    // SAFETY: `proc_pidinfo` copies the BSD process info for `pid` into `info`,
    // which we own; the size passed is that struct's.
    let rc = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
            std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int,
        )
    };
    (rc >= std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int).then_some(info.pbi_ppid as u32)
}

#[cfg(target_os = "macos")]
fn is_clauth_launch(pid: u32) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    #[allow(unsafe_code)]
    // SAFETY: `proc_pidpath` copies the executable path for `pid` into `buf`,
    // which we own; the return is the byte length, 0 on failure.
    let rc = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if rc <= 0 {
        return false;
    }
    let path = Path::new(std::ffi::OsStr::from_bytes(&buf[..rc as usize]));
    // The executable alone is readable here (proc_pidpath gives no argv), so a
    // `clauth` whose second argument is not `start`/`resume` still reads as a
    // launch: the argv check is unmeasured on macOS.
    path.file_name() == Some(std::ffi::OsStr::new("clauth"))
}

#[cfg(test)]
#[path = "../../tests/inline/daemon_bridge.rs"]
mod tests;

#[cfg(test)]
#[path = "../../tests/inline/daemon_bridge_extra.rs"]
mod extra_tests;

#[cfg(test)]
#[path = "../../tests/inline/daemon_bridge_fix.rs"]
mod fix_tests;

#[cfg(test)]
#[path = "../../tests/inline/daemon_bridge_fix2.rs"]
mod fix2_tests;

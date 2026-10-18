#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(unsafe_code)]
//! The tps producer against sandboxed transcript stores: speed, the windowed
//! rate, growing partials, and attribution (unattributed, isolated runtime
//! after a swap, the hook's resolved account). Expected values are hand
//! computed from the fixture's timestamps.

use std::path::{Path, PathBuf};

use super::*;
use crate::testutil::HomeSandbox;

/// The record's measured response: 1222 output tokens from the latest user
/// line `20:46:09.417` (an earlier prompt at `20:45:50.000` is not the start)
/// to the first line carrying them, `20:46:23.014` (the id's second line 5 ms
/// later is not the end).
const FIXTURE: &str = include_str!("../fixtures/tps-one-response.jsonl");
const FIXTURE_SESSION: &str = "0c1f6a52-7d4e-4b0a-9a51-3f2b8e6c1d01";
const START: &str = "2026-09-22T20:46:09.417Z";
const END: &str = "2026-09-22T20:46:23.014Z";
const MODEL: &str = "claude-opus-5-5";

fn ms(iso: &str) -> u64 {
    u64::try_from(
        chrono::DateTime::parse_from_rfc3339(iso)
            .unwrap()
            .timestamp_millis(),
    )
    .unwrap()
}

fn iso(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(i64::try_from(ms).unwrap())
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn user(ts: &str, session: &str) -> String {
    serde_json::json!({
        "type": "user",
        "timestamp": ts,
        "sessionId": session,
        "uuid": format!("u-{ts}"),
        "message": {"role": "user", "content": "go"},
    })
    .to_string()
}

fn assistant(ts: &str, session: &str, id: &str, output: u64) -> String {
    serde_json::json!({
        "type": "assistant",
        "timestamp": ts,
        "sessionId": session,
        "uuid": format!("a-{id}-{ts}"),
        "message": {
            "id": id,
            "model": MODEL,
            "role": "assistant",
            "content": [{"type": "text", "text": "ok"}],
            "usage": {
                "input_tokens": 12,
                "output_tokens": output,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
            },
        },
    })
    .to_string()
}

fn global_store(home: &Path) -> PathBuf {
    home.join(".claude/projects/-home-u-proj")
}

fn write_lines(path: &Path, lines: &[String]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut body = lines.join("\n");
    body.push('\n');
    std::fs::write(path, body).unwrap();
}

fn append_lines(path: &Path, lines: &[String]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for l in lines {
        writeln!(f, "{l}").unwrap();
    }
}

/// One 1222-token response, 13.597 s from its user line to its end, landed as
/// a transcript in the global store with no owner anywhere.
fn seed_fixture(home: &Path) {
    let path = global_store(home).join(format!("{FIXTURE_SESSION}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, FIXTURE).unwrap();
}

fn close(actual: f64, want: f64, what: &str) {
    assert!(
        (actual - want).abs() < 1e-3,
        "{what}: got {actual}, want {want}"
    );
}

// 1222 / 13.597 = 89.8727…
const SPEED: f64 = 1222.0 / 13.597;

#[test]
fn a_fixture_response_pins_speed_and_each_window_rate() {
    let home = HomeSandbox::new();
    seed_fixture(home.home());

    // 4 s after the end, the 10 s live window [end-6 s, end+4 s] holds 6 s of
    // the 13.597 s span: 1222 * 6 / 13.597 = 539.236 tokens -> 53.924 tok/s.
    let snap = Producer::new().tick(ms(END) + 4_000);
    let s = &snap.sessions[FIXTURE_SESSION];
    close(s.figures.live.speed.unwrap(), SPEED, "live speed");
    assert_eq!(format_figure(s.figures.live.speed.unwrap()), "90");
    close(
        s.figures.live.rate,
        1222.0 * 6.0 / 13.597 / 10.0,
        "live rate",
    );
    // The whole span sits inside 5 min and 1 h.
    close(s.figures.m5.rate, 1222.0 / 300.0, "5 min rate");
    assert_eq!(format_figure(s.figures.m5.rate), "4.1");
    close(s.figures.h1.rate, 1222.0 / 3600.0, "1 h rate");
    assert_eq!(format_figure(s.figures.h1.rate), "0.3");
    close(s.figures.m5.speed.unwrap(), SPEED, "5 min speed");
    assert_eq!(snap.total, s.figures, "one session: the total is it");
    assert_eq!(snap.version, CACHE_VERSION);

    // 11 s after the end the live window no longer touches the span.
    let later = Producer::new().tick(ms(END) + 11_000);
    let s = &later.sessions[FIXTURE_SESSION];
    assert_eq!(s.figures.live.speed, None, "no response touches live");
    close(s.figures.live.rate, 0.0, "live rate past the window");
    close(s.figures.m5.rate, 1222.0 / 300.0, "5 min still holds it");
}

#[test]
fn format_figure_spells_zero_bare_one_decimal_below_ten_whole_above() {
    for (v, want) in [
        (0.0, "0"),
        (0.04, "0"),
        (0.05, "0.1"),
        (0.34, "0.3"),
        (1.0, "1.0"),
        (9.94, "9.9"),
        (10.0, "10"),
        (89.87, "90"),
        (1234.4, "1234"),
    ] {
        assert_eq!(format_figure(v), want, "{v}");
    }
}

#[test]
fn growing_partials_give_the_end_landing_speed_counted_once() {
    let home = HomeSandbox::new();
    let path = global_store(home.home()).join("grow.jsonl");
    let start = ms(START);
    // Two partials of one message id land first, the final line later.
    write_lines(
        &path,
        &[
            user(START, "grow"),
            assistant(&iso(start + 2_000), "grow", "msg_grow", 40),
            assistant(&iso(start + 8_000), "grow", "msg_grow", 700),
        ],
    );
    let mut producer = Producer::new();
    producer.tick(start + 8_500);
    append_lines(&path, &[assistant(END, "grow", "msg_grow", 1222)]);

    let snap = producer.tick(ms(END) + 4_000);
    let s = &snap.sessions["grow"];
    close(s.figures.m5.speed.unwrap(), SPEED, "end-landing speed");
    // 1222 once, never 40 + 700 + 1222.
    close(
        s.figures.m5.rate,
        1222.0 / 300.0,
        "the response counts once",
    );
}

#[test]
fn an_unattributed_session_counts_in_the_total_only() {
    let home = HomeSandbox::new();
    let start = ms(START);
    // `kerry`'s isolated runtime: its store names the profile and the sid.
    let iso_store = home
        .home()
        .join(".clauth/profiles/kerry/runtime-isolated-4242-1/projects/-home-u-proj");
    crate::live_sessions::register(&crate::live_sessions::LiveSession {
        isolated: true,
        ..crate::testutil::live_row("4242-1", "kerry")
    })
    .unwrap();
    write_lines(
        &iso_store.join("owned.jsonl"),
        &[
            user(START, "owned"),
            assistant(END, "owned", "msg_owned", 1222),
        ],
    );
    // A bare session with no hook record, no owner stamp, no runtime.
    write_lines(
        &global_store(home.home()).join("stray.jsonl"),
        &[
            user(START, "stray"),
            assistant(&iso(start + 10_000), "stray", "msg_stray", 500),
        ],
    );

    let snap = Producer::new().tick(ms(END) + 4_000);
    assert_eq!(
        snap.accounts.keys().collect::<Vec<_>>(),
        vec!["kerry"],
        "only the attributed session reaches an account"
    );
    close(snap.accounts["kerry"].m5.rate, 1222.0 / 300.0, "kerry");
    assert_eq!(snap.sessions["stray"].account, None);
    assert_eq!(snap.sessions["owned"].account.as_deref(), Some("kerry"));
    close(
        snap.total.m5.rate,
        (1222.0 + 500.0) / 300.0,
        "total has both",
    );
}

#[test]
fn a_swapped_session_lands_on_its_current_member() {
    let home = HomeSandbox::new();
    // Launched on `kerry`, swapped onto `mommy`: the store path still names
    // the launch profile, the live row's `current_member` follows the swap.
    let iso_store = home
        .home()
        .join(".clauth/profiles/kerry/runtime-isolated-4242-1/projects/-home-u-proj");
    crate::live_sessions::register(&crate::live_sessions::LiveSession {
        isolated: true,
        current_member: Some("mommy".to_owned()),
        ..crate::testutil::live_row("4242-1", "kerry")
    })
    .unwrap();
    write_lines(
        &iso_store.join("swapped.jsonl"),
        &[
            user(START, "swapped"),
            assistant(END, "swapped", "msg_swapped", 1222),
        ],
    );

    let snap = Producer::new().tick(ms(END) + 4_000);
    assert_eq!(snap.accounts.keys().collect::<Vec<_>>(), vec!["mommy"]);
    assert_eq!(snap.sessions["swapped"].account.as_deref(), Some("mommy"));
    let day = &snap.days["2026-09-22"];
    assert_eq!(
        day.keys().collect::<Vec<_>>(),
        vec!["mommy"],
        "the day sums follow the member that spent them"
    );
    assert_eq!(
        day["mommy"][MODEL],
        DaySum {
            output: 1222,
            gen_ms: 13_597
        }
    );
}

#[test]
fn a_shared_session_attributes_through_the_hooks_resolved_account() {
    let home = HomeSandbox::new();
    let sid = "5d7e2a10-1b3c-4d5e-8f90-a1b2c3d4e5f6";
    let record = crate::hook_note::record_path(sid, None).unwrap();
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, r#"{"resolved":"mommy"}"#).unwrap();
    write_lines(
        &global_store(home.home()).join(format!("{sid}.jsonl")),
        &[user(START, sid), assistant(END, sid, "msg_shared", 1222)],
    );
    // A subagent of that session attributes through its parent.
    write_lines(
        &global_store(home.home()).join(format!("{sid}/subagents/agent-a1b2c3.jsonl")),
        &[user(START, sid), assistant(END, sid, "msg_sub", 600)],
    );

    let snap = Producer::new().tick(ms(END) + 4_000);
    assert_eq!(snap.accounts.keys().collect::<Vec<_>>(), vec!["mommy"]);
    close(
        snap.accounts["mommy"].m5.rate,
        (1222.0 + 600.0) / 300.0,
        "the session and its subagent",
    );
}

#[test]
fn the_cache_round_trips_and_ages_off_generated_at() {
    let home = HomeSandbox::new();
    seed_fixture(home.home());
    let now = ms(END) + 4_000;
    let snap = Producer::new().tick(now);
    assert_eq!(snap.age_ms(now + 7_000), Some(7_000));
    let path = home.home().join(".clauth").join(TPS_CACHE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&snap).unwrap()).unwrap();
    assert_eq!(load_cache(), Some(snap));
}

// ── extra: truncation restart, restart dedupe, transcript_speed ───────────────

/// A transcript truncated below the stored tail offset restarts that file at
/// byte 0 and still reads the lines appended afterwards.
#[test]
fn a_truncated_transcript_restarts_that_file_at_byte_zero() {
    let home = HomeSandbox::new();
    let path = global_store(home.home()).join("trunc.jsonl");
    write_lines(
        &path,
        &[
            user(START, "trunc"),
            assistant(END, "trunc", "msg_old", 1222),
        ],
    );
    let mut producer = Producer::new();
    producer.tick(ms(END) + 4_000);

    // Truncate the file to nothing, then append a fresh response: the stored
    // tail offset is now past the shorter length, so the file restarts at byte
    // 0 and the new line is read. `msg_old` stays counted once (it happened
    // inside the window), never twice.
    std::fs::write(&path, "").unwrap();
    append_lines(
        &path,
        &[
            user(START, "trunc"),
            assistant(&iso(ms(END) + 20_000), "trunc", "msg_new", 400),
        ],
    );
    let snap = producer.tick(ms(END) + 24_000);
    let s = &snap.sessions["trunc"];
    close(
        s.figures.m5.rate,
        (1222.0 + 400.0) / 300.0,
        "both responses, each once",
    );
}

/// A restart loads the persisted day sums and re-reads today's lines; the
/// response must not be added to the day twice.
#[test]
fn a_restart_does_not_double_count_day_sums() {
    let home = HomeSandbox::new();
    let iso_store = home
        .home()
        .join(".clauth/profiles/kerry/runtime-isolated-4242-1/projects/-home-u-proj");
    crate::live_sessions::register(&crate::live_sessions::LiveSession {
        isolated: true,
        ..crate::testutil::live_row("4242-1", "kerry")
    })
    .unwrap();
    write_lines(
        &iso_store.join("restart.jsonl"),
        &[
            user(START, "restart"),
            assistant(END, "restart", "msg_restart", 1222),
        ],
    );
    let now = ms(END) + 4_000;
    let mut producer = Producer::new();
    let snap = producer.tick(now);
    assert_eq!(
        snap.days["2026-09-22"]["kerry"][MODEL],
        DaySum {
            output: 1222,
            gen_ms: 13_597
        }
    );
    // Persist the cache the way the scheduler leg does.
    let path = home.home().join(".clauth").join(TPS_CACHE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&snap).unwrap()).unwrap();

    // A fresh producer loads the cache and re-derives today's sum from the
    // re-read lines: 1222 once, never 2444.
    let mut restarted = Producer::new();
    let snap2 = restarted.tick(now + 1_000);
    assert_eq!(
        snap2.days["2026-09-22"]["kerry"][MODEL],
        DaySum {
            output: 1222,
            gen_ms: 13_597
        }
    );
}

/// One transcript's whole-life sums over every finished response: the fixture's
/// single response is 1222 output over 13 597 ms.
#[test]
fn transcript_speed_sums_the_fixture_response() {
    let home = HomeSandbox::new();
    let path = global_store(home.home()).join(format!("{FIXTURE_SESSION}.jsonl"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, FIXTURE).unwrap();
    assert_eq!(
        transcript_speed(&global_store(home.home()).join(format!("{FIXTURE_SESSION}.jsonl"))),
        Some(DaySum {
            output: 1222,
            gen_ms: 13_597
        })
    );
}

// ── round-2 fixes (reviewer probes re-derived through Producer::tick) ────────

fn iso_kerry(home: &Path) -> PathBuf {
    home.join(".clauth/profiles/kerry/runtime-isolated-4242-1/projects/-home-u-proj")
}

fn reg(member: Option<&str>) {
    crate::live_sessions::register(&crate::live_sessions::LiveSession {
        isolated: true,
        current_member: member.map(str::to_owned),
        ..crate::testutil::live_row("4242-1", "kerry")
    })
    .unwrap();
}

fn write_cache(home: &Path, snap: &Snapshot) {
    let path = home.join(".clauth").join(TPS_CACHE_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(snap).unwrap()).unwrap();
}

/// `[9.95, 10)` rounds up to a whole number, never "10.0".
#[test]
fn format_figure_rounds_the_95_band_up_to_whole() {
    assert_eq!(format_figure(9.94), "9.9");
    assert_eq!(format_figure(9.95), "10");
    assert_eq!(format_figure(9.96), "10");
}

/// A response with no user line before it has no span: out of speed and out of
/// the day sums (a day sum is a speed basis), but its tokens land as a point in
/// the rate.
#[test]
fn a_response_with_no_user_line_has_no_span() {
    let home = HomeSandbox::new();
    reg(None);
    write_lines(
        &iso_kerry(home.home()).join("nouser.jsonl"),
        &[assistant(END, "nouser", "msg_nouser", 1222)],
    );
    let snap = Producer::new().tick(ms(END) + 4_000);
    assert_eq!(
        snap.sessions["nouser"].figures.m5.speed, None,
        "no span, no speed"
    );
    assert!(!snap.days.contains_key("2026-09-22"), "no span, no day sum");
    // Its tokens count whole in the rate: end_ms is inside the 5 min window.
    close(
        snap.sessions["nouser"].figures.m5.rate,
        1222.0 / 300.0,
        "point tokens land in the rate",
    );
}

/// Parallel subagents share a parent session key, but each transcript's span
/// start is its OWN latest user line: sibling B's later user line must not
/// become A's span start. speed = 1222 / 12 s.
#[test]
fn parallel_subagents_keep_their_own_span_start() {
    let home = HomeSandbox::new();
    let s = ms(START);
    let dir = global_store(home.home()).join("par/subagents");
    let a = dir.join("agent-a.jsonl");
    let b = dir.join("agent-b.jsonl");
    write_lines(&a, &[user(&iso(s), "par")]);
    let mut p = Producer::new();
    p.tick(s + 1_000);
    write_lines(&b, &[user(&iso(s + 5_000), "par")]);
    p.tick(s + 6_500);
    append_lines(&a, &[assistant(&iso(s + 12_000), "par", "msg_a", 1222)]);
    let snap = p.tick(s + 13_000);
    let sp = snap.sessions["par"].figures.m5.speed.unwrap();
    close(
        sp,
        1222.0 / 12.0,
        "subagent A span starts at A's own user line",
    );
}

/// A live swap: each response stays on the member that spent it, and the
/// session row names the current member (deterministically).
#[test]
fn a_live_swap_keeps_each_response_on_its_member() {
    let home = HomeSandbox::new();
    let path = iso_kerry(home.home()).join("sw.jsonl");
    let mut row_values = std::collections::BTreeSet::new();
    let mut last = None;
    for _ in 0..40 {
        reg(None);
        write_lines(
            &path,
            &[user(START, "sw"), assistant(END, "sw", "msg_a", 1222)],
        );
        let mut p = Producer::new();
        p.tick(ms(END) + 1_000);
        reg(Some("mommy"));
        append_lines(
            &path,
            &[
                user(&iso(ms(END) + 2_000), "sw"),
                assistant(&iso(ms(END) + 12_000), "sw", "msg_b", 500),
            ],
        );
        let snap = p.tick(ms(END) + 13_000);
        row_values.insert(snap.sessions["sw"].account.clone());
        last = Some(snap);
    }
    let snap = last.unwrap();
    assert_eq!(
        row_values.len(),
        1,
        "session row account is order-dependent"
    );
    assert_eq!(snap.sessions["sw"].account.as_deref(), Some("mommy"));
    assert_eq!(
        snap.accounts.keys().collect::<Vec<_>>(),
        vec!["kerry", "mommy"],
        "each response stays on the member that spent it"
    );
    assert_eq!(snap.days["2026-09-22"]["kerry"][MODEL].output, 1222);
    assert_eq!(snap.days["2026-09-22"]["mommy"][MODEL].output, 500);
}

/// A producer restart after a swap reuses the frozen account from the cache:
/// the pre-swap response is never re-attributed to the current member.
#[test]
fn a_restart_after_a_swap_never_reattributes_a_counted_response() {
    let home = HomeSandbox::new();
    let path = iso_kerry(home.home()).join("rst.jsonl");
    reg(None);
    write_lines(
        &path,
        &[user(START, "rst"), assistant(END, "rst", "msg_a", 1222)],
    );
    let snap = Producer::new().tick(ms(END) + 1_000);
    assert_eq!(snap.days["2026-09-22"]["kerry"][MODEL].output, 1222);
    write_cache(home.home(), &snap);

    reg(Some("mommy"));
    let restarted = Producer::new().tick(ms(END) + 2_000);
    assert_eq!(
        restarted.days["2026-09-22"]["kerry"][MODEL].output, 1222,
        "the frozen account survives the restart"
    );
    assert!(
        !restarted.days["2026-09-22"].contains_key("mommy"),
        "the pre-swap response was re-attributed"
    );
    assert_eq!(
        restarted.sessions["rst"].account.as_deref(),
        Some("mommy"),
        "the session row names the current member"
    );
}

/// A response that ends just before UTC midnight reaches its day, even though
/// the tick first reads it after midnight.
#[test]
fn a_response_landing_just_before_utc_midnight_reaches_its_day() {
    let home = HomeSandbox::new();
    reg(None);
    let path = iso_kerry(home.home()).join("mid.jsonl");
    write_lines(&path, &[user("2026-09-22T23:59:50.000Z", "mid")]);
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T23:59:59.000Z"));
    append_lines(
        &path,
        &[assistant(
            "2026-09-22T23:59:59.800Z",
            "mid",
            "msg_mid",
            1222,
        )],
    );
    let snap = p.tick(ms("2026-09-23T00:00:00.500Z"));
    assert_eq!(
        snap.days
            .get("2026-09-22")
            .and_then(|d| d.get("kerry"))
            .and_then(|m| m.get(MODEL))
            .map(|s| s.output),
        Some(1222),
        "the 23:59:59.8 response is missing from its day"
    );
}

/// A restart after midnight completes the previous day from its retained lines:
/// the 23:30 response joins the 21:00 response.
#[test]
fn a_restart_after_midnight_completes_the_previous_day() {
    let home = HomeSandbox::new();
    reg(None);
    let path = iso_kerry(home.home()).join("late.jsonl");
    write_lines(
        &path,
        &[
            user("2026-09-22T21:00:00.000Z", "late"),
            assistant("2026-09-22T21:00:10.000Z", "late", "msg_early", 1000),
        ],
    );
    let snap = Producer::new().tick(ms("2026-09-22T21:00:20.000Z"));
    write_cache(home.home(), &snap);
    append_lines(
        &path,
        &[
            user("2026-09-22T23:30:00.000Z", "late"),
            assistant("2026-09-22T23:30:10.000Z", "late", "msg_late", 500),
        ],
    );
    let snap2 = Producer::new().tick(ms("2026-09-23T00:10:00.000Z"));
    assert_eq!(
        snap2.days["2026-09-22"]["kerry"][MODEL].output, 1500,
        "the 23:30 response is missing from its day"
    );
}

/// A cold start reads back only the recent tail: a file whose lines are all
/// days old retains nothing.
#[test]
fn a_cold_start_reads_back_only_the_recent_tail() {
    let home = HomeSandbox::new();
    let now = ms(END) + 3 * 86_400_000;
    seed_fixture(home.home());
    let mut p = Producer::new();
    p.tick(now);
    assert_eq!(
        p.responses.len(),
        0,
        "3-day-old lines are all before the cutoff"
    );
}

/// A synthetic API-error line (placeholder model, zero output) never drags the
/// speed denominator nor pollutes the day sums.
#[test]
fn a_synthetic_error_line_does_not_drag_speed() {
    let home = HomeSandbox::new();
    reg(None);
    let synth = serde_json::json!({
        "type": "assistant", "timestamp": iso(ms(END) + 61_000), "sessionId": "syn",
        "message": {"id": "2e6d3668-da9b-40b0-aafe-1a8b4f614e94", "model": "<synthetic>", "role": "assistant",
            "content": [{"type": "text", "text": "API Error"}],
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}},
    })
    .to_string();
    write_lines(
        &iso_kerry(home.home()).join("syn.jsonl"),
        &[
            user(START, "syn"),
            assistant(END, "syn", "msg_real", 1222),
            user(&iso(ms(END) + 1_000), "syn"),
            synth,
        ],
    );
    let snap = Producer::new().tick(ms(END) + 62_000);
    close(
        snap.sessions["syn"].figures.m5.speed.unwrap(),
        SPEED,
        "the synthetic line must not enter the speed denominator",
    );
    assert!(
        !snap.days["2026-09-22"]["kerry"].contains_key("<synthetic>"),
        "no synthetic day row"
    );
}

/// The core tail property: a tick advances the file's byte offset to its length,
/// and an equal-length in-place rewrite before that offset is not re-read.
#[test]
fn a_tick_advances_the_tail_offset_and_never_rereads_whole_files() {
    let home = HomeSandbox::new();
    let path = global_store(home.home()).join("tail.jsonl");
    write_lines(
        &path,
        &[
            user(START, "tail"),
            assistant(END, "tail", "msg_tail", 1222),
        ],
    );
    let mut p = Producer::new();
    p.tick(ms(END) + 4_000);
    let len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(p.files.get(&path).map(|e| e.offset), Some(len));
    // Equal-length in-place rewrite: the same offset means nothing is re-read.
    let before = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, before.replace("msg_tail", "msg_evil")).unwrap();
    p.tick(ms(END) + 5_000);
    assert!(
        p.responses.contains_key("msg_tail"),
        "the original response remains"
    );
    assert!(
        !p.responses.contains_key("msg_evil"),
        "the rewrite was re-read"
    );
}

// ── round-3 fixes ─────────────────────────────────────────────────────────────

/// A shared session's owner follows the hook record's own change event: after a
/// switch the later response lands on the new member, the earlier stays put.
#[test]
fn a_shared_session_switch_after_first_read_reattributes_later_responses() {
    let home = HomeSandbox::new();
    let sid = "5d7e2a10-1b3c-4d5e-8f90-a1b2c3d4e5f6";
    let record = crate::hook_note::record_path(sid, None).unwrap();
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, r#"{"resolved":"kerry"}"#).unwrap();
    let path = global_store(home.home()).join(format!("{sid}.jsonl"));
    write_lines(
        &path,
        &[user(START, sid), assistant(END, sid, "msg_a", 1222)],
    );
    let mut p = Producer::new();
    p.tick(ms(END) + 1_000);
    std::fs::write(&record, r#"{"resolved":"mommy"}"#).unwrap();
    append_lines(
        &path,
        &[
            user(&iso(ms(END) + 2_000), sid),
            assistant(&iso(ms(END) + 12_000), sid, "msg_b", 500),
        ],
    );
    let snap = p.tick(ms(END) + 13_000);
    assert_eq!(snap.sessions[sid].account.as_deref(), Some("mommy"));
    assert_eq!(
        snap.accounts.keys().collect::<Vec<_>>(),
        vec!["kerry", "mommy"]
    );
    assert_eq!(snap.days["2026-09-22"]["kerry"][MODEL].output, 1222);
    assert_eq!(snap.days["2026-09-22"]["mommy"][MODEL].output, 500);
}

/// A hook record that appears after the first read re-attaches the session: the
/// later response lands on the new member, the earlier stays unattributed.
#[test]
fn a_hook_record_landing_after_first_read_reattributes_later_responses() {
    let home = HomeSandbox::new();
    let sid = "5d7e2a10-1b3c-4d5e-8f90-a1b2c3d4e5f6";
    let record = crate::hook_note::record_path(sid, None).unwrap();
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    let path = global_store(home.home()).join(format!("{sid}.jsonl"));
    write_lines(
        &path,
        &[user(START, sid), assistant(END, sid, "msg_a", 1222)],
    );
    let mut p = Producer::new();
    p.tick(ms(END) + 1_000);
    std::fs::write(&record, r#"{"resolved":"mommy"}"#).unwrap();
    append_lines(
        &path,
        &[
            user(&iso(ms(END) + 2_000), sid),
            assistant(&iso(ms(END) + 12_000), sid, "msg_b", 500),
        ],
    );
    let snap = p.tick(ms(END) + 13_000);
    assert_eq!(snap.sessions[sid].account.as_deref(), Some("mommy"));
    assert_eq!(snap.accounts.keys().collect::<Vec<_>>(), vec!["mommy"]);
    assert_eq!(snap.days["2026-09-22"]["*"][MODEL].output, 1222);
    assert_eq!(snap.days["2026-09-22"]["mommy"][MODEL].output, 500);
}

/// An unfolded past day stays in the cache — the only source the ledger folds.
#[test]
fn a_cache_keeps_unfolded_past_days() {
    let home = HomeSandbox::new();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-19".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 1,
                gen_ms: 2,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-25T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);
    let mut p = Producer::new();
    p.tick(ms("2026-09-25T00:01:00.000Z"));
    // A day older than yesterday now folds into token_speed.json and leaves the
    // cache (the producer is the ledger's only writer).
    let speed = crate::token_ledger::SpeedLedger::load(&home.home().join(".clauth"));
    assert_eq!(
        speed.days["2026-09-19"]["kerry"][MODEL],
        crate::tps::DaySum {
            output: 1,
            gen_ms: 2
        }
    );
    assert!(!p.days.contains_key("2026-09-19"));
}

/// Id-less growing partials share the (path, preceding user ts) key, so they
/// count once with their final output.
#[test]
fn idless_growing_partials_count_once() {
    let home = HomeSandbox::new();
    reg(None);
    let path = iso_kerry(home.home()).join("idless.jsonl");
    let start = ms(START);
    write_lines(
        &path,
        &[
            user(START, "idless"),
            assistant(&iso(start + 2_000), "idless", "", 40),
            assistant(&iso(start + 8_000), "idless", "", 1222),
        ],
    );
    let snap = Producer::new().tick(start + 13_000);
    close(
        snap.sessions["idless"].figures.m5.rate,
        1222.0 / 300.0,
        "id-less partials counted per partial",
    );
}

/// A cold file joins at its length (no whole-history read); after it grows hot
/// only the appended line is read and the offset advances.
#[test]
fn a_cold_file_joins_at_len_and_reads_only_the_appended_tail() {
    let home = HomeSandbox::new();
    let path = global_store(home.home()).join("cold.jsonl");
    write_lines(
        &path,
        &[user(START, "cold"), assistant(END, "cold", "msg_old", 1222)],
    );
    let old = std::time::UNIX_EPOCH + std::time::Duration::from_millis(ms(START));
    crate::testutil::set_mtime(&path, old);
    let mut p = Producer::new();
    let now = ms(END) + 3 * 86_400_000;
    p.tick(now);
    assert_eq!(p.responses.len(), 0, "a cold file is skipped");
    let old_len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(p.files.get(&path).map(|e| e.offset), Some(old_len));

    append_lines(
        &path,
        &[
            user(&iso(now - 1_000), "cold"),
            assistant(&iso(now), "cold", "msg_new", 500),
        ],
    );
    crate::testutil::set_mtime(&path, std::time::SystemTime::now());
    p.tick(now + 6_000);
    assert!(
        p.responses.contains_key("msg_new"),
        "the appended line was read"
    );
    assert!(
        !p.responses.contains_key("msg_old"),
        "the old lines stayed unread"
    );
    let new_len = std::fs::metadata(&path).unwrap().len();
    assert_eq!(p.files.get(&path).map(|e| e.offset), Some(new_len));
}

// ── round-4 fixes ─────────────────────────────────────────────────────────────

/// An out-of-order subagent reply (read after the parent's later reply) keeps
/// its member across a restart: the sorted interval's `from` is lowered to the
/// earliest reply it covers.
#[test]
fn out_of_order_subagent_reply_keeps_its_member_after_a_restart() {
    let home = HomeSandbox::new();
    reg(None);
    let s = ms(START);
    let parent = iso_kerry(home.home()).join("par.jsonl");
    let sub = iso_kerry(home.home()).join("par/subagents/agent-a.jsonl");
    write_lines(
        &parent,
        &[
            user(START, "par"),
            assistant(&iso(s + 20_000), "par", "msg_parent", 1000),
        ],
    );
    let mut p = Producer::new();
    p.tick(s + 26_000);
    write_lines(
        &sub,
        &[
            user(START, "par"),
            assistant(&iso(s + 15_000), "par", "msg_sub", 500),
        ],
    );
    let snap = p.tick(s + 32_000);
    assert_eq!(snap.days["2026-09-22"]["kerry"][MODEL].output, 1500);
    write_cache(home.home(), &snap);

    reg(Some("mommy"));
    let restarted = Producer::new().tick(s + 33_000);
    assert_eq!(
        restarted.days["2026-09-22"]["kerry"][MODEL].output, 1500,
        "both replies stay on the member that spent them"
    );
    assert!(!restarted.days["2026-09-22"].contains_key("mommy"));
}

/// A new file read after a swap uses the fresh member for its replies, not the
/// persisted interval; the split survives a restart.
#[test]
fn a_new_file_after_a_swap_uses_the_member_that_spent_it() {
    let home = HomeSandbox::new();
    reg(None);
    let s = ms(START);
    let parent = iso_kerry(home.home()).join("p2.jsonl");
    write_lines(
        &parent,
        &[
            user(START, "p2"),
            assistant(&iso(s + 10_000), "p2", "msg_parent", 1000),
        ],
    );
    let mut p = Producer::new();
    let snap = p.tick(s + 11_000);
    write_cache(home.home(), &snap);

    reg(Some("mommy"));
    let sub = iso_kerry(home.home()).join("p2/subagents/agent-a.jsonl");
    write_lines(
        &sub,
        &[
            user(&iso(s + 12_000), "p2"),
            assistant(&iso(s + 20_000), "p2", "msg_sub", 500),
        ],
    );
    let snap2 = p.tick(s + 21_000);
    assert_eq!(snap2.days["2026-09-22"]["kerry"][MODEL].output, 1000);
    assert_eq!(snap2.days["2026-09-22"]["mommy"][MODEL].output, 500);
    write_cache(home.home(), &snap2);

    let restarted = Producer::new().tick(s + 22_000);
    assert_eq!(restarted.days["2026-09-22"]["kerry"][MODEL].output, 1000);
    assert_eq!(restarted.days["2026-09-22"]["mommy"][MODEL].output, 500);
}

/// Out-of-order reads across a swap keep their FIRST count: additive day sums
/// never re-attribute a counted response on a restart.
#[test]
fn out_of_order_reads_across_a_swap_keep_their_first_count() {
    let home = HomeSandbox::new();
    reg(None);
    let s = ms(START);
    let parent = iso_kerry(home.home()).join("p3.jsonl");
    let sub = iso_kerry(home.home()).join("p3/subagents/agent-a.jsonl");
    write_lines(
        &parent,
        &[
            user(START, "p3"),
            assistant(&iso(s + 10_000), "p3", "msg_a", 1000),
            assistant(&iso(s + 30_000), "p3", "msg_b", 300),
        ],
    );
    let mut p = Producer::new();
    p.tick(s + 31_000);
    reg(Some("mommy"));
    write_lines(
        &sub,
        &[
            user(&iso(s + 18_000), "p3"),
            assistant(&iso(s + 20_000), "p3", "msg_sub", 500),
        ],
    );
    let snap2 = p.tick(s + 40_000);
    assert_eq!(snap2.days["2026-09-22"]["kerry"][MODEL].output, 1300);
    assert_eq!(snap2.days["2026-09-22"]["mommy"][MODEL].output, 500);
    write_cache(home.home(), &snap2);

    let restarted = Producer::new().tick(s + 41_000);
    assert_eq!(restarted.days["2026-09-22"]["kerry"][MODEL].output, 1300);
    assert_eq!(restarted.days["2026-09-22"]["mommy"][MODEL].output, 500);
}

// ── round-6 ──────────────────────────────────────────────────────────────────

/// A growing partial adds only its delta to the day sums (the additive core).
#[test]
fn a_growing_partial_adds_only_its_delta_to_the_day_sums() {
    let home = HomeSandbox::new();
    reg(None);
    let path = iso_kerry(home.home()).join("grow.jsonl");
    let start = ms(START);
    write_lines(
        &path,
        &[
            user(START, "grow"),
            assistant(&iso(start + 2_000), "grow", "msg_grow", 40),
        ],
    );
    let mut p = Producer::new();
    p.tick(start + 8_500);
    append_lines(
        &path,
        &[assistant(&iso(start + 8_000), "grow", "msg_grow", 700)],
    );
    p.tick(start + 12_000);
    append_lines(&path, &[assistant(END, "grow", "msg_grow", 1222)]);
    let snap = p.tick(ms(END) + 1_000);
    assert_eq!(
        snap.days["2026-09-22"]["kerry"][MODEL],
        DaySum {
            output: 1222,
            gen_ms: 13_597
        }
    );
}

/// A day that rolls out of yesterday folds into token_speed.json and leaves the
/// cache (the producer is the speed ledger's only writer).
#[test]
fn a_day_rolling_out_of_yesterday_folds_into_the_speed_ledger() {
    let home = HomeSandbox::new();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-20".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 7,
                gen_ms: 3,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-22T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T00:01:00.000Z"));
    let speed = crate::token_ledger::SpeedLedger::load(&home.home().join(".clauth"));
    assert_eq!(
        speed.days["2026-09-20"]["kerry"][MODEL],
        crate::tps::DaySum {
            output: 7,
            gen_ms: 3
        }
    );
    assert!(!p.days.contains_key("2026-09-20"));
}

/// A failed fold save (a newer token_speed.json) keeps the day in the cache.
#[test]
fn a_failed_fold_save_keeps_the_day_in_the_cache() {
    let home = HomeSandbox::new();
    let clauth = home.home().join(".clauth");
    std::fs::create_dir_all(&clauth).unwrap();
    std::fs::write(
        clauth.join("token_speed.json"),
        r#"{"version":99,"days":{}}"#,
    )
    .unwrap();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-20".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 7,
                gen_ms: 3,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-22T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T00:01:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "tick 1 keeps it");
    p.tick(ms("2026-09-22T00:02:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "tick 2 keeps it");
    // Restart off the producer's OWN last snapshot (not the initial cache), so a
    // producer that dropped the day reds here.
    let snap = p.tick(ms("2026-09-22T00:03:00.000Z"));
    write_cache(home.home(), &snap);
    let restarted = Producer::new();
    assert!(
        restarted.days.contains_key("2026-09-20"),
        "a restart keeps it"
    );
}

/// A plain save failure (a non-writable `.clauth`) keeps the day across two
/// ticks, and it lands once the dir is writable again.
#[test]
fn a_plain_save_failure_retries_and_lands_once_writable() {
    let home = HomeSandbox::new();
    let clauth = home.home().join(".clauth");
    std::fs::create_dir_all(&clauth).unwrap();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-20".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 7,
                gen_ms: 3,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-22T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);

    let unwritable = crate::testutil::UnwritableDir::new(&clauth);
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T00:01:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "t1 keeps it");
    p.tick(ms("2026-09-22T00:02:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "t2 keeps it");
    drop(unwritable);
    p.tick(ms("2026-09-22T00:03:00.000Z"));
    assert!(!p.days.contains_key("2026-09-20"), "t3 folds it");
    let speed = crate::token_ledger::SpeedLedger::load(&clauth);
    assert_eq!(
        speed.days["2026-09-20"]["kerry"][MODEL],
        crate::tps::DaySum {
            output: 7,
            gen_ms: 3
        }
    );
}

/// The session row reads the hook's resolved fresh, even when the store batch
/// cached an older value.
#[test]
fn the_session_row_reads_the_hook_resolved_fresh() {
    let home = HomeSandbox::new();
    let sid = "5d7e2a10-1b3c-4d5e-8f90-a1b2c3d4e5f6";
    let record = crate::hook_note::record_path(sid, None).unwrap();
    std::fs::create_dir_all(record.parent().unwrap()).unwrap();
    std::fs::write(&record, r#"{"resolved":"kerry"}"#).unwrap();
    let sp = home.home().join(".clauth").join("session_profiles.json");
    std::fs::create_dir_all(sp.parent().unwrap()).unwrap();
    std::fs::write(
        &sp,
        r#"{"sessions":{"5d7e2a10-1b3c-4d5e-8f90-a1b2c3d4e5f6":{"known":"kerry"}}}"#,
    )
    .unwrap();
    let path = global_store(home.home()).join(format!("{sid}.jsonl"));
    write_lines(
        &path,
        &[user(START, sid), assistant(END, sid, "msg_a", 1222)],
    );
    let mut p = Producer::new();
    p.tick(ms(END) + 1_000);
    std::fs::write(&record, r#"{"resolved":"mommy"}"#).unwrap();
    let snap = p.tick(ms(END) + 2_000);
    assert_eq!(
        snap.sessions[sid].account.as_deref(),
        Some("mommy"),
        "the row reads the hook fresh"
    );
}

/// A growing partial whose end crosses UTC midnight lands wholly on the end
/// day (the ledger keys by end).
#[test]
fn a_growing_partial_crossing_midnight_moves_to_the_end_day() {
    let home = HomeSandbox::new();
    reg(None);
    let path = iso_kerry(home.home()).join("midgrow.jsonl");
    write_lines(
        &path,
        &[
            user("2026-09-22T23:59:50.000Z", "midgrow"),
            assistant("2026-09-22T23:59:58.000Z", "midgrow", "msg_mg", 40),
        ],
    );
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T23:59:59.000Z"));
    append_lines(
        &path,
        &[assistant(
            "2026-09-23T00:00:05.000Z",
            "midgrow",
            "msg_mg",
            1222,
        )],
    );
    let snap = p.tick(ms("2026-09-23T00:00:06.000Z"));
    assert_eq!(
        snap.days["2026-09-23"]["kerry"][MODEL],
        DaySum {
            output: 1222,
            gen_ms: 15_000
        }
    );
}

// ── round-9 ──────────────────────────────────────────────────────────────────

/// A permission blip on the speed ledger recovers via ctime: chmod 000 at
/// start (unreadable), chmod 600 one tick later, and the held day folds.
#[cfg(unix)]
#[test]
fn a_permission_blip_on_the_speed_ledger_recovers_via_ctime() {
    use std::os::unix::fs::PermissionsExt;
    // The named guard: root ignores permission bits, so the chmod-000 leg
    // cannot fail and this test proves nothing.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root ignores permission bits");
        return;
    }
    let home = HomeSandbox::new();
    let clauth = home.home().join(".clauth");
    std::fs::create_dir_all(&clauth).unwrap();
    std::fs::write(
        clauth.join("token_speed.json"),
        r#"{"version":1,"days":{"2026-09-10":{"kerry":{"claude-opus-5-5":{"output":1,"gen_ms":2}}}}}"#,
    )
    .unwrap();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-20".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 7,
                gen_ms: 3,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-22T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);

    std::fs::set_permissions(
        clauth.join("token_speed.json"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T00:01:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "held while unreadable");

    std::fs::set_permissions(
        clauth.join("token_speed.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    p.tick(ms("2026-09-22T00:02:00.000Z"));
    assert!(!p.days.contains_key("2026-09-20"), "folded once readable");
    let speed = crate::token_ledger::SpeedLedger::load(&clauth);
    assert_eq!(
        speed.days["2026-09-20"]["kerry"][MODEL],
        crate::tps::DaySum {
            output: 7,
            gen_ms: 3
        }
    );
}

/// A content rewrite of the speed ledger (mtime moves) folds the held day.
#[cfg(unix)]
#[test]
fn a_speed_ledger_content_rewrite_folds_the_held_day() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root ignores permission bits");
        return;
    }
    let home = HomeSandbox::new();
    let clauth = home.home().join(".clauth");
    std::fs::create_dir_all(&clauth).unwrap();
    std::fs::write(
        clauth.join("token_speed.json"),
        r#"{"version":1,"days":{"2026-09-10":{"kerry":{"claude-opus-5-5":{"output":1,"gen_ms":2}}}}}"#,
    )
    .unwrap();
    let mut days = crate::tps::DaySums::new();
    days.entry("2026-09-20".to_owned())
        .or_default()
        .entry("kerry".to_owned())
        .or_default()
        .insert(
            MODEL.to_owned(),
            crate::tps::DaySum {
                output: 7,
                gen_ms: 3,
            },
        );
    let snap = crate::tps::Snapshot {
        version: CACHE_VERSION,
        generated_at: "2026-09-22T00:00:00.000+00:00".to_owned(),
        days,
        ..crate::tps::Snapshot::default()
    };
    write_cache(home.home(), &snap);

    std::fs::set_permissions(
        clauth.join("token_speed.json"),
        std::fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let mut p = Producer::new();
    p.tick(ms("2026-09-22T00:01:00.000Z"));
    assert!(p.days.contains_key("2026-09-20"), "held while unreadable");

    std::fs::set_permissions(
        clauth.join("token_speed.json"),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    std::fs::write(
        clauth.join("token_speed.json"),
        r#"{"version":1,"days":{"2026-09-10":{"kerry":{"claude-opus-5-5":{"output":1,"gen_ms":2}}}}}"#,
    )
    .unwrap();
    p.tick(ms("2026-09-22T00:02:00.000Z"));
    assert!(
        !p.days.contains_key("2026-09-20"),
        "folded after the rewrite"
    );
    let speed = crate::token_ledger::SpeedLedger::load(&clauth);
    assert_eq!(
        speed.days["2026-09-20"]["kerry"][MODEL],
        crate::tps::DaySum {
            output: 7,
            gen_ms: 3
        }
    );
}

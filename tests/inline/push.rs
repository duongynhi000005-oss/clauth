//! Web Push delivery: RFC 8291's published vector, the VAPID JWT, the
//! allowlist at send, the payload a device decrypts, per-device event
//! filtering, a dead endpoint dropping its row, the latches that keep one
//! state change to one push, and the outbound cap.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

use crate::testutil::HomeSandbox;

fn d(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(text)
        .expect("fixture base64url")
}

// RFC 8291 Appendix A, verbatim.
const RFC_PLAINTEXT: &str = "V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24";
const RFC_AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
const RFC_UA_PUBLIC: &str =
    "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
const RFC_UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
const RFC_SALT: &str = "DGv6ra1nlYgDCS1FRnbzlw";
const RFC_AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
const RFC_MESSAGE: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";

/// ring draws an EC private scalar straight from its RNG, so a fixed slice
/// IS the key; its own test RNG is the only way to hand it one.
#[allow(
    deprecated,
    reason = "ring's fixed-slice RNG is the only seam that hands it a known ephemeral key"
)]
fn fixed(bytes: &[u8]) -> ring::test::rand::FixedSliceRandom<'_> {
    ring::test::rand::FixedSliceRandom { bytes }
}

#[test]
fn the_encryption_reproduces_rfc_8291_appendix_a() {
    let as_private = d(RFC_AS_PRIVATE);
    let salt: [u8; 16] = d(RFC_SALT).try_into().expect("16-byte salt");

    let message = encrypt(
        &d(RFC_UA_PUBLIC),
        &d(RFC_AUTH),
        &d(RFC_PLAINTEXT),
        &fixed(&as_private),
        salt,
    )
    .expect("encrypt");

    assert_eq!(message, d(RFC_MESSAGE));
}

/// The browser's side of RFC 8291, written apart from [`encrypt`]: what a
/// device holding `ua_private` reads out of `message`.
fn decrypt(message: &[u8], ua_private: &[u8], ua_public: &[u8], auth: &[u8]) -> Vec<u8> {
    let salt = &message[..16];
    let key_len = usize::from(message[20]);
    let as_public = &message[21..21 + key_len];
    let mut record = message[21 + key_len..].to_vec();

    let private = ring::agreement::EphemeralPrivateKey::generate(
        &ring::agreement::ECDH_P256,
        &fixed(ua_private),
    )
    .expect("the browser key");
    let peer = ring::agreement::UnparsedPublicKey::new(&ring::agreement::ECDH_P256, as_public);
    let ikm = ring::agreement::agree_ephemeral(private, &peer, |secret| {
        let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, auth).extract(secret);
        let mut ikm = [0u8; 32];
        prk.expand(&[b"WebPush: info\0", ua_public, as_public], Len(32))
            .expect("expand")
            .fill(&mut ikm)
            .expect("fill");
        ikm
    })
    .expect("ecdh");
    let prk = ring::hkdf::Salt::new(ring::hkdf::HKDF_SHA256, salt).extract(&ikm);
    let mut cek = [0u8; 16];
    prk.expand(&[b"Content-Encoding: aes128gcm\0"], Len(16))
        .expect("expand")
        .fill(&mut cek)
        .expect("fill");
    let mut nonce = [0u8; 12];
    prk.expand(&[b"Content-Encoding: nonce\0"], Len(12))
        .expect("expand")
        .fill(&mut nonce)
        .expect("fill");
    let key = ring::aead::LessSafeKey::new(
        ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &cek).expect("cek"),
    );
    let plain = key
        .open_in_place(
            ring::aead::Nonce::assume_unique_for_key(nonce),
            ring::aead::Aad::empty(),
            &mut record,
        )
        .expect("the record opens");
    let (delimiter, body) = plain.split_last().expect("a delimiter");
    assert_eq!(*delimiter, 0x02, "one last record, unpadded");
    body.to_vec()
}

#[test]
fn the_browser_side_reads_the_rfc_message() {
    // Pins the test's own decrypt against the published bytes before any
    // delivery test trusts it.
    assert_eq!(
        decrypt(
            &d(RFC_MESSAGE),
            &d(RFC_UA_PRIVATE),
            &d(RFC_UA_PUBLIC),
            &d(RFC_AUTH)
        ),
        d(RFC_PLAINTEXT)
    );
}

#[test]
fn the_vapid_authorization_signs_the_origin_for_twelve_hours() {
    let _home = HomeSandbox::new();
    let key = vapid_key().expect("mint the key");

    let header =
        vapid_authorization(&key, "https://fcm.googleapis.com", 1_700_000_000).expect("sign");

    let (token, public) = header
        .strip_prefix("vapid t=")
        .and_then(|rest| rest.split_once(", k="))
        .expect("vapid t=<jwt>, k=<key>");
    assert_eq!(public, key.public_key_b64());
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "a JWT has three parts");
    assert_eq!(d(parts[0]), br#"{"typ":"JWT","alg":"ES256"}"#);
    assert_eq!(
        String::from_utf8(d(parts[1])).expect("utf-8 claims"),
        r#"{"aud":"https://fcm.googleapis.com","exp":1700043200,"sub":"https://github.com/uwuclxdy/clauth"}"#
    );
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, d(public))
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &d(parts[2]),
        )
        .expect("the signature verifies under the published key");
}

// ------------------------------------------------------------ delivery rig

/// The delivery tests: the route harness that registers their rows is
/// unix-only, as every route test is.
#[cfg(unix)]
mod delivery {
    use super::*;

    use std::cell::RefCell;

    use crate::daemon::api::devices::Tier;
    use crate::daemon::api::routes::ApiContext;
    use crate::profile::{AppConfig, AppState};
    use crate::testutil::{OTHER_TOKEN, call, ctx_with, req, seed_device};

    /// One POST the recording transport took, and what it answered.
    struct Taken {
        url: String,
        headers: Vec<(&'static str, String)>,
        body: Vec<u8>,
    }

    thread_local! {
        static TAKEN: RefCell<Vec<Taken>> = const { RefCell::new(Vec::new()) };
        static ANSWER: std::cell::Cell<u16> = const { std::cell::Cell::new(201) };
        /// Runs inside the POST, before it answers: a device re-registering
        /// while its push is in flight.
        static DURING_POST: std::cell::Cell<Option<fn()>> = const { std::cell::Cell::new(None) };
    }

    /// The transport field's test value: records the request on the calling
    /// thread (delivery runs on it) and answers [`ANSWER`].
    fn recording(post: &Post) -> std::result::Result<u16, &'static str> {
        if let Some(during) = DURING_POST.with(std::cell::Cell::take) {
            during();
        }
        TAKEN.with(|taken| {
            taken.borrow_mut().push(Taken {
                url: post.url.clone(),
                headers: post.headers.clone(),
                body: post.body.clone(),
            });
        });
        Ok(ANSWER.with(std::cell::Cell::get))
    }

    fn taken() -> Vec<Taken> {
        TAKEN.with(|taken| std::mem::take(&mut *taken.borrow_mut()))
    }

    fn sender() -> Sender {
        Sender::new(
            recording,
            crate::daemon::api::panes::absent_probe(),
            RateCap::new(30, Duration::from_secs(600)),
        )
    }

    const TABLET_TOKEN: &str = "abababababababababababababababababababababababababababababababab";
    const PHONE_ENDPOINT: &str = "https://fcm.googleapis.com/fcm/send/phone";
    const TABLET_ENDPOINT: &str = "https://web.push.apple.com/tablet";

    fn ctx() -> std::sync::Arc<ApiContext> {
        let config = std::sync::Arc::new(crate::lockorder::RankedMutex::new(AppConfig {
            state: AppState::default(),
            profiles: Vec::new(),
        }));
        let ctx = ctx_with(config);
        seed_device("phone", Tier::View, OTHER_TOKEN);
        seed_device("tablet", Tier::View, TABLET_TOKEN);
        ctx
    }

    /// Register a subscription through the route, with RFC 8291's browser keys.
    fn subscribe(ctx: &ApiContext, token: &str, endpoint: &str, events: &str) {
        let body = format!(
            r#"{{"endpoint":"{endpoint}","keys":{{"p256dh":"{RFC_UA_PUBLIC}","auth":"{RFC_AUTH}"}},"events":{events}}}"#
        );
        let resp = call(
            ctx,
            &req("PUT", "/api/v1/push/subscription", Some(token), &body),
        );
        assert_eq!(resp.status, 200, "subscribe {endpoint}");
    }

    /// Rewrite one stored row's field on disk, as a user's hand edit would.
    fn edit_row(device: &str, field: &str, value: &str) {
        let path = crate::profile::clauth_dir()
            .expect("clauth dir")
            .join("push-subscriptions.json");
        let mut store: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("store")).expect("store json");
        for row in store["subscriptions"].as_array_mut().expect("rows") {
            if row["device"] == device {
                row[field] = value.into();
            }
        }
        std::fs::write(&path, serde_json::to_vec(&store).expect("encode")).expect("write store");
    }

    fn header<'a>(taken: &'a Taken, name: &str) -> Option<&'a str> {
        taken
            .headers
            .iter()
            .find(|(have, _)| *have == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_device_decrypts_the_event_and_the_account_and_nothing_else() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);

        sender().deliver(&Trigger::Account {
            profile: "alpha".to_string(),
        });

        let posts = taken();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].url, PHONE_ENDPOINT);
        assert_eq!(
            decrypt(
                &posts[0].body,
                &d(RFC_UA_PRIVATE),
                &d(RFC_UA_PUBLIC),
                &d(RFC_AUTH)
            ),
            br#"{"event":"account","account":"alpha"}"#
        );
        assert_eq!(
            (
                header(&posts[0], "Content-Encoding"),
                header(&posts[0], "TTL"),
                header(&posts[0], "Urgency"),
            ),
            (Some("aes128gcm"), Some("86400"), Some("normal"))
        );
        let token = header(&posts[0], "Authorization")
            .and_then(|value| value.strip_prefix("vapid t="))
            .and_then(|rest| rest.split_once(", k="))
            .map(|(token, _)| token.to_string())
            .expect("a VAPID authorization rides every push");
        let claims: serde_json::Value =
            serde_json::from_slice(&d(token.split('.').nth(1).expect("a claims part")))
                .expect("JSON claims");
        assert_eq!(
            (&claims["aud"], &claims["sub"]),
            (
                &serde_json::json!("https://fcm.googleapis.com"),
                &serde_json::json!("https://github.com/uwuclxdy/clauth")
            ),
            "the audience is the endpoint's origin"
        );
    }

    #[test]
    fn a_blocked_pane_with_no_known_account_pushes_the_event_alone() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["blocked"]"#);

        sender().deliver(&Trigger::Blocked {
            pane_id: "w1:p2".to_string(),
        });

        let posts = taken();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            decrypt(
                &posts[0].body,
                &d(RFC_UA_PRIVATE),
                &d(RFC_UA_PUBLIC),
                &d(RFC_AUTH)
            ),
            br#"{"event":"blocked"}"#
        );
        assert_eq!(
            (header(&posts[0], "TTL"), header(&posts[0], "Urgency")),
            (Some("3600"), Some("high"))
        );
    }

    #[test]
    fn a_blocked_pane_names_the_profile_its_session_spends() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["blocked"]"#);
        // pid 4242 leads the pane's foreground group, so its row is the pane's
        // own session.
        crate::live_sessions::register(&crate::testutil::live_row("4242-0", "alpha"))
            .expect("register the pane's session");
        let probe: crate::daemon::api::panes::PaneProbe = Box::new(|args, _| {
            match args {
            ["pane", "process-info", "--pane", "w1:p2"] => {
                crate::daemon::api::panes::HerdrProbeOut::Ran(Some(crate::daemon::api::panes::HerdrOut {
                    success: true,
                    stdout: br#"{"result":{"process_info":{"foreground_process_group_id":4242,"foreground_processes":[]}}}"#.to_vec(),
                    stderr: Vec::new(),
                }))
            }
            _ => crate::daemon::api::panes::HerdrProbeOut::Ran(None),
        }
        });
        let mut sender = Sender::new(recording, probe, RateCap::new(30, Duration::from_secs(600)));

        sender.deliver(&Trigger::Blocked {
            pane_id: "w1:p2".to_string(),
        });

        let posts = taken();
        assert_eq!(posts.len(), 1);
        assert_eq!(
            decrypt(
                &posts[0].body,
                &d(RFC_UA_PRIVATE),
                &d(RFC_UA_PUBLIC),
                &d(RFC_AUTH)
            ),
            br#"{"event":"blocked","account":"alpha"}"#
        );
    }

    #[test]
    fn each_device_gets_only_the_events_it_named() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(
            &ctx,
            OTHER_TOKEN,
            PHONE_ENDPOINT,
            r#"["blocked","account"]"#,
        );
        subscribe(&ctx, TABLET_TOKEN, TABLET_ENDPOINT, r#"["window"]"#);

        let mut sender = sender();
        sender.deliver(&Trigger::Window {
            profile: "alpha".to_string(),
        });
        let window: Vec<String> = taken().into_iter().map(|post| post.url).collect();
        sender.deliver(&Trigger::Account {
            profile: "alpha".to_string(),
        });
        let account: Vec<String> = taken().into_iter().map(|post| post.url).collect();

        assert_eq!(
            (window, account),
            (
                vec![TABLET_ENDPOINT.to_string()],
                vec![PHONE_ENDPOINT.to_string()]
            )
        );
    }

    #[test]
    fn an_endpoint_off_the_allowlist_is_refused_again_at_send() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);
        subscribe(&ctx, TABLET_TOKEN, TABLET_ENDPOINT, r#"["account"]"#);
        edit_row(
            "phone",
            "endpoint",
            "https://169.254.169.254/latest/meta-data",
        );

        sender().deliver(&Trigger::Account {
            profile: "alpha".to_string(),
        });

        let urls: Vec<String> = taken().into_iter().map(|post| post.url).collect();
        assert_eq!(urls, vec![TABLET_ENDPOINT.to_string()]);
    }

    #[test]
    fn a_row_its_pairing_no_longer_holds_gets_no_push() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);
        subscribe(&ctx, TABLET_TOKEN, TABLET_ENDPOINT, r#"["account"]"#);
        // The row a revoke could not drop, now under a re-paired `phone`.
        edit_row(
            "phone",
            "pairing",
            "a-pairing-the-device-list-no-longer-holds",
        );

        sender().deliver(&Trigger::Account {
            profile: "alpha".to_string(),
        });

        let urls: Vec<String> = taken().into_iter().map(|post| post.url).collect();
        assert_eq!(urls, vec![TABLET_ENDPOINT.to_string()]);
    }

    #[test]
    fn a_gone_endpoint_drops_its_row_and_a_failing_one_keeps_it() {
        for (status, kept) in [(404, false), (410, false), (500, true), (301, true)] {
            let _home = HomeSandbox::new();
            let ctx = ctx();
            subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);
            ANSWER.with(|answer| answer.set(status));

            sender().deliver(&Trigger::Account {
                profile: "alpha".to_string(),
            });
            ANSWER.with(|answer| answer.set(201));

            assert_eq!(taken().len(), 1, "status {status}: one POST");
            let rows: Vec<String> = crate::daemon::api::push::read_store()
                .expect("store")
                .subscriptions
                .into_iter()
                .map(|row| row.device)
                .collect();
            assert_eq!(
                rows,
                if kept {
                    vec!["phone".to_string()]
                } else {
                    Vec::new()
                },
                "status {status}"
            );
        }
    }

    #[test]
    fn the_outbound_cap_admits_its_limit_per_window() {
        let mut cap = RateCap::new(2, Duration::from_secs(60));
        let t0 = Instant::now();

        let admitted = [
            cap.admit(t0),
            cap.admit(t0 + Duration::from_secs(1)),
            cap.admit(t0 + Duration::from_secs(2)),
            cap.admit(t0 + Duration::from_secs(60)),
            cap.admit(t0 + Duration::from_secs(61)),
            cap.admit(t0 + Duration::from_secs(62)),
        ];

        assert_eq!(admitted, [true, true, false, true, true, false]);
    }

    #[test]
    fn a_gone_answer_for_an_endpoint_replaced_mid_push_keeps_the_new_row() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);
        ANSWER.with(|answer| answer.set(410));
        DURING_POST.with(|during| {
            during.set(Some(|| {
                edit_row(
                    "phone",
                    "endpoint",
                    "https://fcm.googleapis.com/fcm/send/phone-2",
                );
            }));
        });

        sender().deliver(&Trigger::Account {
            profile: "alpha".to_string(),
        });
        ANSWER.with(|answer| answer.set(201));

        assert_eq!(taken().len(), 1);
        let rows: Vec<(String, String)> = crate::daemon::api::push::read_store()
            .expect("store")
            .subscriptions
            .into_iter()
            .map(|row| (row.device, row.endpoint))
            .collect();
        assert_eq!(
            rows,
            [(
                "phone".to_string(),
                "https://fcm.googleapis.com/fcm/send/phone-2".to_string()
            )]
        );
    }

    #[test]
    fn a_capped_sender_stops_posting_at_the_cap() {
        let _home = HomeSandbox::new();
        let ctx = ctx();
        subscribe(&ctx, OTHER_TOKEN, PHONE_ENDPOINT, r#"["account"]"#);
        let mut sender = Sender::new(
            recording,
            crate::daemon::api::panes::absent_probe(),
            RateCap::new(2, Duration::from_secs(600)),
        );

        for _ in 0..3 {
            sender.deliver(&Trigger::Account {
                profile: "alpha".to_string(),
            });
        }

        assert_eq!(taken().len(), 2);
    }
}

// --------------------------------------------------------------- latches

fn reading(name: &str, failing: bool, util_5h: f64, fresh: bool) -> Reading<'_> {
    Reading {
        name,
        failing,
        five_hour: WindowReading {
            line: Some(90.0),
            util: Some(util_5h),
        },
        seven_day: WindowReading {
            line: Some(80.0),
            util: Some(10.0),
        },
        fresh,
    }
}

#[test]
fn a_seven_day_window_reaching_its_weekly_line_fires_once() {
    let mut latches = StatusLatches::default();
    let weekly = |util: f64| Reading {
        seven_day: WindowReading {
            line: Some(80.0),
            util: Some(util),
        },
        ..reading("alpha", false, 40.0, true)
    };

    let fired = [
        latches.observe([weekly(85.0)]).len(),
        latches.observe([weekly(86.0)]).len(),
        latches.observe([weekly(20.0)]).len(),
        latches.observe([weekly(81.0)]).len(),
    ];

    assert_eq!(fired, [1, 0, 0, 1]);
}

#[test]
fn both_windows_crossing_in_one_body_push_once() {
    let mut latches = StatusLatches::default();
    let both = Reading {
        seven_day: WindowReading {
            line: Some(80.0),
            util: Some(85.0),
        },
        ..reading("alpha", false, 95.0, true)
    };

    assert_eq!(
        latches.observe([both]),
        [Trigger::Window {
            profile: "alpha".to_string()
        }]
    );
}

#[test]
fn an_account_gone_from_the_body_announces_again_when_it_returns_failing() {
    let mut latches = StatusLatches::default();
    let failing = || [reading("alpha", true, 10.0, true)];

    let fired = [
        latches.observe(failing()).len(),
        latches.observe([]).len(),
        latches.observe(failing()).len(),
    ];

    assert_eq!(fired, [1, 0, 1]);
}

#[test]
fn a_repeated_status_body_fires_each_trigger_once() {
    let mut latches = StatusLatches::default();

    let first = latches.observe([reading("alpha", true, 95.0, true)]);
    let again = latches.observe([reading("alpha", true, 96.0, true)]);
    let healed = latches.observe([reading("alpha", false, 40.0, true)]);
    let relapsed = latches.observe([reading("alpha", true, 91.0, true)]);

    assert_eq!(
        (first, again, healed, relapsed),
        (
            vec![
                Trigger::Account {
                    profile: "alpha".to_string()
                },
                Trigger::Window {
                    profile: "alpha".to_string()
                },
            ],
            Vec::new(),
            Vec::new(),
            vec![
                Trigger::Account {
                    profile: "alpha".to_string()
                },
                Trigger::Window {
                    profile: "alpha".to_string()
                },
            ],
        )
    );
}

#[test]
fn a_stale_reading_never_moves_the_window_latch() {
    let mut latches = StatusLatches::default();

    let stale = latches.observe([reading("alpha", false, 95.0, false)]);
    let fresh = latches.observe([reading("alpha", false, 95.0, true)]);

    assert_eq!(
        (stale, fresh),
        (
            Vec::new(),
            vec![Trigger::Window {
                profile: "alpha".to_string()
            }]
        )
    );
}

#[test]
fn a_pane_fires_once_per_move_into_blocked_across_reconnects() {
    let mut latch = BlockedLatch::default();
    let pane = |status: &str| vec![("w1:p2".to_string(), status.to_string())];

    let fired = [
        latch.snapshot(&pane("working")).len(),
        usize::from(latch.changed("w1:p2", "blocked")),
        usize::from(latch.changed("w1:p2", "blocked")),
        // A reconnect's snapshot repeating the standing state.
        latch.snapshot(&pane("blocked")).len(),
        usize::from(latch.changed("w1:p2", "working")),
        latch.snapshot(&pane("blocked")).len(),
    ];

    assert_eq!(fired, [0, 1, 0, 0, 0, 1]);
}

#[test]
fn a_pane_that_left_and_came_back_blocked_fires_again() {
    let mut latch = BlockedLatch::default();
    let blocked = vec![("w1:p2".to_string(), "blocked".to_string())];

    let fired = [
        latch.snapshot(&blocked).len(),
        latch.snapshot(&[]).len(),
        latch.snapshot(&blocked).len(),
    ];

    assert_eq!(fired, [1, 0, 1]);
}

// ------------------------------------------------------------- transport

#[test]
fn the_real_transport_refuses_plain_http_before_sending() {
    let (listener, base) = crate::testutil::deaf_listener();

    let answer = ureq_transport(&Post {
        url: format!("{base}/push"),
        headers: Vec::new(),
        body: b"x".to_vec(),
    });

    assert_eq!(answer, Err("https_only"));
    crate::testutil::assert_nothing_sent(&listener);
}

#[test]
fn the_shared_agent_verifies_tls() {
    assert!(
        !crate::usage::http_agent()
            .config()
            .tls_config()
            .disable_verification()
    );
}

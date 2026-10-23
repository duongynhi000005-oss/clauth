use super::*;

fn rows(frame: &Frame) -> Vec<String> {
    render(frame)
        .iter()
        .map(|row| row.iter().map(|(glyph, _)| *glyph).collect())
        .collect()
}

#[test]
fn rest_pose_matches_the_live_claude_code_capture() {
    assert_eq!(
        rows(&REST),
        [" ▐▛███▛█ ", "▝▜██████▀", " ▝▝   ▝▝ "].map(String::from)
    );
}

#[test]
fn animation_frame_counts_match_the_extracted_sequences() {
    let expected = [
        (Anim::Jump, 12),
        (Anim::Look, 11),
        (Anim::Celebrate, 15),
        (Anim::Skip, 14),
        (Anim::Spin, 10),
        (Anim::Peekaboo, 21),
        (Anim::Drop, 13),
        (Anim::Waddle, 19),
        (Anim::Peek, 20),
        (Anim::Wink, 6),
        (Anim::Boop, 11),
        (Anim::Tap, 14),
        (Anim::Sneeze, 12),
        (Anim::Turn, 19),
        (Anim::CoinHop, 14),
    ];
    for (anim, count) in expected {
        assert_eq!(anim.frames().len(), count, "{}", anim.name());
    }
}

#[test]
fn the_five_in_place_animations_are_the_idle_set() {
    assert_eq!(
        SUBTLE.map(Anim::name),
        ["look", "wink", "boop", "tap", "sneeze"]
    );
    assert_eq!(
        BIG.map(Anim::name),
        [
            "jump",
            "celebrate",
            "skip",
            "spin",
            "peekaboo",
            "drop",
            "waddle",
            "peek",
            "turn",
            "coin-hop",
        ]
    );
    let names: std::collections::HashSet<&str> = SUBTLE
        .iter()
        .chain(BIG.iter())
        .map(|anim| anim.name())
        .collect();
    assert_eq!(names.len(), ANIM_COUNT);
}

#[test]
fn launch_opens_with_a_big_animation() {
    let clawd = Clawd::new();
    let playing = match clawd.playing {
        Some(playing) => playing,
        None => panic!("no launch animation"),
    };
    assert!(BIG.contains(&playing.anim));
    assert_eq!(playing.start_ms, 0);
    assert!(clawd.frame(0).is_some());
}

#[test]
fn each_ten_second_slot_opens_a_subtle_animation() {
    let mut clawd = Clawd::new();
    clawd.tick(SUBTLE_PERIOD_MS);
    let playing = match clawd.playing {
        Some(playing) => playing,
        None => panic!("no animation at the first idle slot"),
    };
    assert!(SUBTLE.contains(&playing.anim));
    assert_eq!(playing.start_ms, SUBTLE_PERIOD_MS);
}

#[test]
fn each_thirty_minute_slot_opens_a_big_animation() {
    let mut clawd = Clawd::new();
    clawd.tick(BIG_PERIOD_MS);
    let playing = match clawd.playing {
        Some(playing) => playing,
        None => panic!("no animation at the first big slot"),
    };
    assert!(BIG.contains(&playing.anim));
    assert_eq!(playing.start_ms, BIG_PERIOD_MS);
}

#[test]
fn a_running_animation_is_never_cut_short() {
    let mut clawd = Clawd::new();
    clawd.tick(SUBTLE_PERIOD_MS);
    let before = clawd
        .playing
        .map(|playing| (playing.anim, playing.start_ms));
    clawd.tick(SUBTLE_PERIOD_MS + FRAME_MS);
    assert_eq!(
        clawd
            .playing
            .map(|playing| (playing.anim, playing.start_ms)),
        before
    );
}

#[test]
fn frames_advance_once_per_tui_tick() {
    let frames = Anim::Wink.frames();
    let mut clawd = Clawd::new();
    clawd.playing = Some(Playing {
        anim: Anim::Wink,
        start_ms: 0,
    });
    assert_eq!(clawd.frame(0), Some(&frames[0]));
    assert_eq!(clawd.frame(FRAME_MS - 1), Some(&frames[0]));
    assert_eq!(clawd.frame(FRAME_MS), Some(&frames[1]));
    assert_eq!(clawd.frame(FRAME_MS * frames.len() as u64), None);
}

#[test]
fn the_eye_facing_shift_and_effect_art_matches_the_extracted_frames() {
    assert_eq!(
        rows(&Anim::Wink.frames()[0]),
        [" ▐▛███▂█ ", "▝▜██████▀", " ▝▝   ▝▝ "].map(String::from)
    );
    assert_eq!(
        rows(&Anim::Turn.frames()[0]),
        [" ▐█▜██▛█ ", "▝▜██████▀", " ▝▝   ▝▝ "].map(String::from)
    );
    assert_eq!(
        rows(&Anim::Drop.frames()[0]),
        ["         ", "         ", "         "].map(String::from)
    );
    assert_eq!(
        rows(&Anim::CoinHop.frames()[2]),
        ["▗▟▛███▛█▄", " ▜██████▘", " ▝▝▁▁▁▝▝ "].map(String::from)
    );
    assert_eq!(
        rows(&Anim::Waddle.frames()[1]),
        ["█▟       ", "██▀      ", "         "].map(String::from)
    );
}

#[test]
fn every_animation_opens_on_its_extracted_first_frame() {
    let expected = [
        (Anim::Jump, ["         ", " ▐▛███▛█ ", "·▜██████·"]),
        (Anim::Look, [" ▐█▟███▟ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Anim::Celebrate, ["         ", " ▐▛███▛█ ", "·▜██████·"]),
        (Anim::Skip, ["         ", "         ", "         "]),
        (Anim::Spin, [" ▐▟███▟█ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Anim::Peekaboo, ["         ", "         ", "         "]),
        (Anim::Drop, ["         ", "         ", "         "]),
        (Anim::Waddle, ["         ", "         ", "         "]),
        (Anim::Peek, ["         ", "         ", "         "]),
        (Anim::Wink, [" ▐▛███▂█ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Anim::Boop, ["         ", " ▐▂███▂█ ", "·▜██████·"]),
        (Anim::Tap, [" ▐▛███▛█ ", "▝▜██████▀", " ▝▝      "]),
        (Anim::Sneeze, ["▗▟▂███▂█▄", " ▜██████▘", " ▝▝   ▝▝ "]),
        (Anim::Turn, [" ▐█▜██▛█ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Anim::CoinHop, ["         ", " ▐▛███▛█ ", "▝▜██████▀"]),
    ];
    for (anim, art) in expected {
        assert_eq!(rows(&anim.frames()[0]), art.map(String::from), "{}", {
            anim.name()
        });
    }
}

#[test]
fn every_facing_rotation_matches_its_extracted_art() {
    let expected = [
        (Facing::Right12, [" ▐█▜██▛█ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Facing::Right30, ["  █▛██▛▌ ", " ▝█████▛ ", "  ▘▘  ▘▘ "]),
        (Facing::Right55, ["  ▐█▛█▜  ", "  ▐████  ", "  ▝▝ ▝▝  "]),
        (Facing::Right75, ["   ██▛▌  ", "   ███▌  ", "   ▘  ▘  "]),
        (Facing::Edge, ["   ▐██   ", "   ▐██   ", "   ▝ ▝   "]),
        (Facing::Back105, ["   ███▌  ", "   ███▌  ", "   ▘  ▘  "]),
        (Facing::Back125, ["  ▐████  ", "  ▐████  ", "  ▝▝ ▝▝  "]),
        (Facing::Back150, ["  █████▌ ", " ▝█████▛ ", "  ▘▘  ▘▘ "]),
        (Facing::Back, [" ▐██████ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
        (Facing::Left75, ["   ▛██▌  ", "   ███▌  ", "   ▘  ▘  "]),
        (Facing::Left55, ["  ▐▜▛██  ", "  ▐████  ", "  ▝▝ ▝▝  "]),
        (Facing::Left30, ["  ▛██▛█▌ ", " ▝█████▛ ", "  ▘▘  ▘▘ "]),
        (Facing::Left12, [" ▐▛███▜█ ", "▝▜██████▀", " ▝▝   ▝▝ "]),
    ];
    for (facing, art) in expected {
        let frame = Frame {
            pose: Pose::Facing(facing),
            ..REST
        };
        assert_eq!(rows(&frame), art.map(String::from), "{facing:?}");
    }
}

#[test]
fn a_finished_animation_clears_at_its_last_frame_boundary() {
    let frames = Anim::Wink.frames();
    let end = FRAME_MS * frames.len() as u64;
    let mut clawd = Clawd::new();
    clawd.playing = Some(Playing {
        anim: Anim::Wink,
        start_ms: 0,
    });
    clawd.next_subtle_ms = end;
    clawd.tick(end);
    let playing = match clawd.playing {
        Some(playing) => playing,
        None => panic!("the finished slot never opened the due subtle animation"),
    };
    assert_eq!(playing.start_ms, end);
    assert!(SUBTLE.contains(&playing.anim));
}

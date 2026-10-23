//! Clawd: the 9×3 block mascot the header draws.
//!
//! The sprite, its pose tables and its fifteen animations are extracted from
//! Claude Code 2.1.309 (its embedded build `1ce7e4ff2da8d4b0…`), where the
//! same component paints the wordmark's left column. Frames advance on the
//! TUI's own 80 ms tick rather than the source's 60 ms redraw, so every frame
//! is shown once at the cadence the rest of the TUI animates at.
//!
//! Idle motion is the five in-place animations (eyes, arms, feet): one every
//! ten seconds, replacing the old one-frame blink. The ten travelling or
//! turning ones play as the bigger moment on launch and every thirty minutes.
//! cloud code also gates the full sprite behind a non-Apple-Terminal check and
//! paints poof/shadow effects in its `inactive` colour; both are ported, the
//! terminal fallback's simplified eyes are not. The ultra-effort wand beside
//! the sprite is untouched.

use std::sync::LazyLock;

/// Sprite width in cells; the header gives it one more column of padding.
pub(crate) const COLS: usize = 9;
pub(crate) const ROWS: usize = 3;

const FRAME_MS: u64 = 80;
const SUBTLE_PERIOD_MS: u64 = 10_000;
const BIG_PERIOD_MS: u64 = 30 * 60 * 1000;

const ANIM_COUNT: usize = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Eyes {
    Open,
    Left,
    Right,
    Closed,
    Wink,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arms {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Feet {
    Both,
    Left,
    Right,
}

/// Body orientations used by the turn/coin-hop sequences. Each names the
/// rotation the source art draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Facing {
    Right12,
    Right30,
    Right55,
    Right75,
    Edge,
    Back105,
    Back125,
    Back150,
    Back,
    Left75,
    Left55,
    Left30,
    Left12,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pose {
    Face { eyes: Eyes, arms: Arms, feet: Feet },
    Facing(Facing),
}

const fn face(eyes: Eyes, arms: Arms, feet: Feet) -> Pose {
    Pose::Face { eyes, arms, feet }
}

const DEFAULT: Pose = face(Eyes::Open, Arms::Down, Feet::Both);
const ARMS_UP: Pose = face(Eyes::Open, Arms::Up, Feet::Both);
const LOOK_LEFT: Pose = face(Eyes::Left, Arms::Down, Feet::Both);
const LOOK_RIGHT: Pose = face(Eyes::Right, Arms::Down, Feet::Both);
const CLOSED: Pose = face(Eyes::Closed, Arms::Down, Feet::Both);
const CLOSED_UP: Pose = face(Eyes::Closed, Arms::Up, Feet::Both);
const WINK: Pose = face(Eyes::Wink, Arms::Down, Feet::Both);
const TAP_LEFT: Pose = face(Eyes::Open, Arms::Down, Feet::Left);
const TAP_RIGHT: Pose = face(Eyes::Open, Arms::Down, Feet::Right);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Poof {
    #[default]
    None,
    Dot,
    Wave,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Shadow {
    #[default]
    None,
    Wide,
    Narrow,
}

#[derive(Clone, Copy, Default)]
struct Opts {
    offset: i8,
    x: i8,
    poof: Poof,
    shadow: Shadow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Frame {
    pose: Pose,
    offset: i8,
    x: i8,
    poof: Poof,
    shadow: Shadow,
}

const REST: Frame = Frame {
    pose: DEFAULT,
    offset: 0,
    x: 0,
    poof: Poof::None,
    shadow: Shadow::None,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Anim {
    Jump,
    Look,
    Celebrate,
    Skip,
    Spin,
    Peekaboo,
    Drop,
    Waddle,
    Peek,
    Wink,
    Boop,
    Tap,
    Sneeze,
    Turn,
    CoinHop,
}

impl Anim {
    #[cfg(test)]
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Jump => "jump",
            Self::Look => "look",
            Self::Celebrate => "celebrate",
            Self::Skip => "skip",
            Self::Spin => "spin",
            Self::Peekaboo => "peekaboo",
            Self::Drop => "drop",
            Self::Waddle => "waddle",
            Self::Peek => "peek",
            Self::Wink => "wink",
            Self::Boop => "boop",
            Self::Tap => "tap",
            Self::Sneeze => "sneeze",
            Self::Turn => "turn",
            Self::CoinHop => "coin-hop",
        }
    }

    fn frames(self) -> &'static [Frame] {
        ANIMS[self as usize].as_slice()
    }
}

/// In-place animations: the subtle idle set, one per ten-second slot.
const SUBTLE: [Anim; 5] = [Anim::Look, Anim::Wink, Anim::Boop, Anim::Tap, Anim::Sneeze];

/// Travelling or turning animations: the bigger moments, on launch and every
/// thirty minutes.
const BIG: [Anim; 10] = [
    Anim::Jump,
    Anim::Celebrate,
    Anim::Skip,
    Anim::Spin,
    Anim::Peekaboo,
    Anim::Drop,
    Anim::Waddle,
    Anim::Peek,
    Anim::Turn,
    Anim::CoinHop,
];

static ANIMS: LazyLock<[Vec<Frame>; ANIM_COUNT]> = LazyLock::new(build_anims);

fn frame(pose: Pose, opts: Opts) -> Frame {
    Frame {
        pose,
        offset: opts.offset,
        x: opts.x,
        poof: opts.poof,
        shadow: opts.shadow,
    }
}

fn push(frames: &mut Vec<Frame>, pose: Pose, count: usize, opts: Opts) {
    frames.extend(std::iter::repeat_n(frame(pose, opts), count));
}

fn push_poof_pair(frames: &mut Vec<Frame>) {
    frames.push(frame(
        DEFAULT,
        Opts {
            offset: 1,
            poof: Poof::Dot,
            ..Opts::default()
        },
    ));
    frames.push(frame(
        DEFAULT,
        Opts {
            offset: 1,
            poof: Poof::Wave,
            ..Opts::default()
        },
    ));
}

/// The eye-close + rest pair most sequences land on.
fn push_blink(frames: &mut Vec<Frame>) {
    push(frames, CLOSED, 1, Opts::default());
    frames.push(frame(DEFAULT, Opts::default()));
}

fn push_facings(frames: &mut Vec<Frame>, facings: &[Facing], shadow: Shadow) {
    for &facing in facings {
        frames.push(frame(
            Pose::Facing(facing),
            Opts {
                shadow,
                ..Opts::default()
            },
        ));
    }
}

fn jump_frames() -> Vec<Frame> {
    let mut frames = Vec::new();
    push_poof_pair(&mut frames);
    push(&mut frames, ARMS_UP, 3, Opts::default());
    push(&mut frames, DEFAULT, 1, Opts::default());
    push_poof_pair(&mut frames);
    push(&mut frames, ARMS_UP, 3, Opts::default());
    push(&mut frames, DEFAULT, 1, Opts::default());
    frames
}

fn build_anims() -> [Vec<Frame>; ANIM_COUNT] {
    let jump = jump_frames();

    let mut look = Vec::new();
    push(&mut look, LOOK_RIGHT, 5, Opts::default());
    push(&mut look, LOOK_LEFT, 5, Opts::default());
    push(&mut look, DEFAULT, 1, Opts::default());

    let mut celebrate = jump.clone();
    push(
        &mut celebrate,
        DEFAULT,
        3,
        Opts {
            offset: 1,
            ..Opts::default()
        },
    );

    let mut skip = Vec::new();
    push(
        &mut skip,
        DEFAULT,
        1,
        Opts {
            offset: 1,
            x: -9,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        ARMS_UP,
        2,
        Opts {
            x: -6,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        DEFAULT,
        1,
        Opts {
            x: -6,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        DEFAULT,
        1,
        Opts {
            offset: 1,
            x: -6,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        ARMS_UP,
        2,
        Opts {
            x: -3,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        DEFAULT,
        1,
        Opts {
            x: -3,
            ..Opts::default()
        },
    );
    push(
        &mut skip,
        DEFAULT,
        1,
        Opts {
            offset: 1,
            x: -3,
            ..Opts::default()
        },
    );
    push(&mut skip, ARMS_UP, 2, Opts::default());
    push_poof_pair(&mut skip);
    push(&mut skip, DEFAULT, 1, Opts::default());

    let mut spin = Vec::new();
    push(&mut spin, LOOK_LEFT, 2, Opts::default());
    push(&mut spin, LOOK_RIGHT, 2, Opts::default());
    push(&mut spin, LOOK_LEFT, 2, Opts::default());
    push(&mut spin, ARMS_UP, 3, Opts::default());
    push(&mut spin, DEFAULT, 1, Opts::default());

    let mut peekaboo = Vec::new();
    push(
        &mut peekaboo,
        DEFAULT,
        1,
        Opts {
            offset: 3,
            ..Opts::default()
        },
    );
    push(
        &mut peekaboo,
        DEFAULT,
        3,
        Opts {
            offset: 2,
            ..Opts::default()
        },
    );
    push(
        &mut peekaboo,
        LOOK_RIGHT,
        3,
        Opts {
            offset: 2,
            ..Opts::default()
        },
    );
    push(
        &mut peekaboo,
        LOOK_LEFT,
        3,
        Opts {
            offset: 2,
            ..Opts::default()
        },
    );
    push(
        &mut peekaboo,
        DEFAULT,
        2,
        Opts {
            offset: 2,
            ..Opts::default()
        },
    );
    push(
        &mut peekaboo,
        DEFAULT,
        1,
        Opts {
            offset: 1,
            ..Opts::default()
        },
    );
    push(&mut peekaboo, ARMS_UP, 4, Opts::default());
    push(&mut peekaboo, DEFAULT, 2, Opts::default());
    push_blink(&mut peekaboo);

    let mut drop = Vec::new();
    push(
        &mut drop,
        ARMS_UP,
        1,
        Opts {
            offset: -3,
            ..Opts::default()
        },
    );
    push(
        &mut drop,
        ARMS_UP,
        2,
        Opts {
            offset: -2,
            ..Opts::default()
        },
    );
    push(
        &mut drop,
        ARMS_UP,
        2,
        Opts {
            offset: -1,
            ..Opts::default()
        },
    );
    push(&mut drop, ARMS_UP, 1, Opts::default());
    push_poof_pair(&mut drop);
    push(&mut drop, DEFAULT, 3, Opts::default());
    push_blink(&mut drop);

    let mut waddle = Vec::new();
    waddle.push(frame(
        LOOK_RIGHT,
        Opts {
            x: -9,
            ..Opts::default()
        },
    ));
    for x in [-6, -5, -4, -3, -2, -1] {
        let feet = if x % 2 == 0 { Feet::Left } else { Feet::Right };
        push(
            &mut waddle,
            face(Eyes::Right, Arms::Down, feet),
            2,
            Opts {
                x,
                ..Opts::default()
            },
        );
    }
    push(&mut waddle, LOOK_RIGHT, 2, Opts::default());
    push(&mut waddle, DEFAULT, 2, Opts::default());
    push_blink(&mut waddle);

    let mut peek = Vec::new();
    push(
        &mut peek,
        LOOK_RIGHT,
        1,
        Opts {
            x: -9,
            ..Opts::default()
        },
    );
    push(
        &mut peek,
        LOOK_RIGHT,
        5,
        Opts {
            x: -6,
            ..Opts::default()
        },
    );
    push(
        &mut peek,
        LOOK_RIGHT,
        3,
        Opts {
            x: -9,
            ..Opts::default()
        },
    );
    push(
        &mut peek,
        LOOK_RIGHT,
        4,
        Opts {
            x: -5,
            ..Opts::default()
        },
    );
    push(
        &mut peek,
        ARMS_UP,
        2,
        Opts {
            x: -3,
            ..Opts::default()
        },
    );
    push(&mut peek, ARMS_UP, 2, Opts::default());
    push_poof_pair(&mut peek);
    push(&mut peek, DEFAULT, 1, Opts::default());

    let mut wink = Vec::new();
    push(&mut wink, WINK, 5, Opts::default());
    push(&mut wink, DEFAULT, 1, Opts::default());

    let mut boop = Vec::new();
    push(
        &mut boop,
        CLOSED,
        1,
        Opts {
            offset: 1,
            poof: Poof::Dot,
            ..Opts::default()
        },
    );
    push(
        &mut boop,
        CLOSED,
        2,
        Opts {
            offset: 1,
            poof: Poof::Wave,
            ..Opts::default()
        },
    );
    push(&mut boop, LOOK_LEFT, 4, Opts::default());
    push(&mut boop, DEFAULT, 2, Opts::default());
    push_blink(&mut boop);

    let mut tap = Vec::new();
    push(&mut tap, TAP_LEFT, 2, Opts::default());
    push(&mut tap, TAP_RIGHT, 2, Opts::default());
    push(&mut tap, TAP_LEFT, 2, Opts::default());
    push(&mut tap, TAP_RIGHT, 2, Opts::default());
    push(&mut tap, TAP_LEFT, 1, Opts::default());
    push(&mut tap, TAP_RIGHT, 1, Opts::default());
    push(&mut tap, ARMS_UP, 3, Opts::default());
    push(&mut tap, DEFAULT, 1, Opts::default());

    let mut sneeze = Vec::new();
    push(&mut sneeze, CLOSED_UP, 4, Opts::default());
    push(
        &mut sneeze,
        CLOSED,
        2,
        Opts {
            offset: 1,
            poof: Poof::Wave,
            ..Opts::default()
        },
    );
    push(&mut sneeze, CLOSED, 2, Opts::default());
    push(&mut sneeze, DEFAULT, 2, Opts::default());
    push_blink(&mut sneeze);

    let mut turn = Vec::new();
    push_facings(
        &mut turn,
        &[
            Facing::Right12,
            Facing::Right30,
            Facing::Right55,
            Facing::Right75,
            Facing::Edge,
        ],
        Shadow::None,
    );
    push_facings(
        &mut turn,
        &[
            Facing::Back105,
            Facing::Back125,
            Facing::Back150,
            Facing::Back,
            Facing::Back,
        ],
        Shadow::None,
    );
    push_facings(
        &mut turn,
        &[
            Facing::Back150,
            Facing::Back125,
            Facing::Back105,
            Facing::Edge,
        ],
        Shadow::None,
    );
    push_facings(
        &mut turn,
        &[
            Facing::Left75,
            Facing::Left55,
            Facing::Left30,
            Facing::Left12,
        ],
        Shadow::None,
    );
    push(&mut turn, DEFAULT, 1, Opts::default());

    let mut coin_hop = Vec::new();
    push(
        &mut coin_hop,
        DEFAULT,
        2,
        Opts {
            offset: 1,
            ..Opts::default()
        },
    );
    push(
        &mut coin_hop,
        ARMS_UP,
        1,
        Opts {
            shadow: Shadow::Wide,
            ..Opts::default()
        },
    );
    push_facings(
        &mut coin_hop,
        &[Facing::Right55, Facing::Edge, Facing::Back125, Facing::Back],
        Shadow::Narrow,
    );
    push_facings(
        &mut coin_hop,
        &[Facing::Back125, Facing::Edge, Facing::Left55],
        Shadow::Narrow,
    );
    push(
        &mut coin_hop,
        ARMS_UP,
        1,
        Opts {
            shadow: Shadow::Wide,
            ..Opts::default()
        },
    );
    push_poof_pair(&mut coin_hop);
    push(&mut coin_hop, DEFAULT, 1, Opts::default());

    [
        jump, look, celebrate, skip, spin, peekaboo, drop, waddle, peek, wink, boop, tap, sneeze,
        turn, coin_hop,
    ]
}

/// One rendered cell: the glyph plus what its colours mean. Only the header
/// maps kinds to styles, so the sprite tables stay colour-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CellKind {
    Empty,
    Body,
    /// Body-coloured glyph over the app background — the eye cut-outs.
    Eye,
    /// Background-coloured glyph on the body — closed lids and winks.
    Lid,
    FacingBody,
    /// The turned-away cut in a facing frame, over the app background.
    FacingCut,
    /// Poof and shadow marks, the source's `inactive` colour.
    Inactive,
}

pub(crate) type Art = [[(char, CellKind); COLS]; ROWS];

pub(crate) fn rest_art() -> Art {
    render(&REST)
}

pub(crate) fn render(frame: &Frame) -> Art {
    let mut art = [[(' ', CellKind::Empty); COLS]; ROWS];
    match frame.pose {
        Pose::Face { eyes, arms, feet } => draw_face(&mut art, eyes, arms, feet),
        Pose::Facing(facing) => draw_facing(&mut art, facing),
    }
    if frame.offset != 0 || frame.x != 0 {
        art = shift(&art, frame.x, frame.offset);
    }
    draw_effects(&mut art, frame);
    art
}

fn draw_face(art: &mut Art, eyes: Eyes, arms: Arms, feet: Feet) {
    let arm = arm_art(arms);
    place(art, 0, 0, arm.r1l, CellKind::Body);
    let mut col = 2;
    for &(glyphs, lid) in eye_spans(eyes) {
        let kind = if lid { CellKind::Lid } else { CellKind::Eye };
        for glyph in glyphs.chars() {
            place_cell(art, col, 0, glyph, kind);
            col += 1;
        }
    }
    place(art, 8, 0, arm.r1r, CellKind::Body);
    place(art, 0, 1, arm.r2l, CellKind::Body);
    place(
        art,
        2,
        1,
        "\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}",
        CellKind::Eye,
    );
    place(art, 7, 1, arm.r2r, CellKind::Body);
    place(art, 0, 2, feet_art(feet), CellKind::Body);
}

fn draw_facing(art: &mut Art, facing: Facing) {
    for (row, &(glyphs, from, to)) in facing_rows(facing).iter().enumerate() {
        for (col, glyph) in glyphs.chars().enumerate() {
            let kind = if (from..to).contains(&col) {
                CellKind::FacingCut
            } else {
                CellKind::FacingBody
            };
            place_cell(art, col, row, glyph, kind);
        }
    }
}

fn draw_effects(art: &mut Art, frame: &Frame) {
    let poof = match frame.poof {
        Poof::None => None,
        Poof::Dot => Some('\u{00b7}'),
        Poof::Wave => Some('~'),
    };
    if let Some(glyph) = poof {
        art[2][0] = (glyph, CellKind::Inactive);
        art[2][COLS - 1] = (glyph, CellKind::Inactive);
    }
    match frame.shadow {
        Shadow::None => {}
        Shadow::Wide => {
            for (col, glyph) in "\u{2581}\u{2581}\u{2581}".chars().enumerate() {
                art[2][3 + col] = (glyph, CellKind::Inactive);
            }
        }
        Shadow::Narrow => art[2][4] = ('\u{2581}', CellKind::Inactive),
    }
}

fn shift(art: &Art, dx: i8, dy: i8) -> Art {
    let mut shifted = [[(' ', CellKind::Empty); COLS]; ROWS];
    for (row, cells) in art.iter().enumerate() {
        for (col, &(glyph, kind)) in cells.iter().enumerate() {
            if kind == CellKind::Empty {
                continue;
            }
            let x = col as isize + dx as isize;
            let y = row as isize + dy as isize;
            if (0..COLS as isize).contains(&x) && (0..ROWS as isize).contains(&y) {
                shifted[y as usize][x as usize] = (glyph, kind);
            }
        }
    }
    shifted
}

fn place(art: &mut Art, col: usize, row: usize, glyphs: &str, kind: CellKind) {
    for (offset, glyph) in glyphs.chars().enumerate() {
        place_cell(art, col + offset, row, glyph, kind);
    }
}

fn place_cell(art: &mut Art, col: usize, row: usize, glyph: char, kind: CellKind) {
    if let Some(cell) = art.get_mut(row).and_then(|cells| cells.get_mut(col)) {
        *cell = (glyph, kind);
    }
}

struct ArmArt {
    r1l: &'static str,
    r1r: &'static str,
    r2l: &'static str,
    r2r: &'static str,
}

fn arm_art(arms: Arms) -> ArmArt {
    match arms {
        Arms::Down => ArmArt {
            r1l: " \u{2590}",
            r1r: "",
            r2l: "\u{259d}\u{259c}",
            r2r: "\u{2588}\u{2580}",
        },
        Arms::Up => ArmArt {
            r1l: "\u{2597}\u{259f}",
            r1r: "\u{2584}",
            r2l: " \u{259c}",
            r2r: "\u{2588}\u{2598}",
        },
    }
}

fn eye_spans(eyes: Eyes) -> &'static [(&'static str, bool)] {
    const OPEN: &[(&str, bool)] = &[("\u{259b}\u{2588}\u{2588}\u{2588}\u{259b}\u{2588}", false)];
    const LEFT: &[(&str, bool)] = &[("\u{259f}\u{2588}\u{2588}\u{2588}\u{259f}\u{2588}", false)];
    const RIGHT: &[(&str, bool)] = &[("\u{2588}\u{259f}\u{2588}\u{2588}\u{2588}\u{259f}", false)];
    const CLOSED: &[(&str, bool)] = &[
        ("\u{2582}", true),
        ("\u{2588}\u{2588}\u{2588}", false),
        ("\u{2582}", true),
        ("\u{2588}", false),
    ];
    const WINK: &[(&str, bool)] = &[
        ("\u{259b}\u{2588}\u{2588}\u{2588}", false),
        ("\u{2582}", true),
        ("\u{2588}", false),
    ];
    match eyes {
        Eyes::Open => OPEN,
        Eyes::Left => LEFT,
        Eyes::Right => RIGHT,
        Eyes::Closed => CLOSED,
        Eyes::Wink => WINK,
    }
}

fn feet_art(feet: Feet) -> &'static str {
    match feet {
        Feet::Both => " \u{259d}\u{259d}   \u{259d}\u{259d} ",
        Feet::Left => " \u{259d}\u{259d}      ",
        Feet::Right => "      \u{259d}\u{259d} ",
    }
}

fn facing_rows(facing: Facing) -> &'static [(&'static str, usize, usize)] {
    const RIGHT_12: &[(&str, usize, usize)] = &[
        (
            " \u{2590}\u{2588}\u{259c}\u{2588}\u{2588}\u{259b}\u{2588} ",
            2,
            8,
        ),
        (
            "\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}",
            2,
            7,
        ),
        (" \u{259d}\u{259d}   \u{259d}\u{259d} ", 0, 0),
    ];
    const RIGHT_30: &[(&str, usize, usize)] = &[
        ("  \u{2588}\u{259b}\u{2588}\u{2588}\u{259b}\u{258c} ", 2, 7),
        (
            " \u{259d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259b} ",
            2,
            7,
        ),
        ("  \u{2598}\u{2598}  \u{2598}\u{2598} ", 0, 0),
    ];
    const RIGHT_55: &[(&str, usize, usize)] = &[
        ("  \u{2590}\u{2588}\u{259b}\u{2588}\u{259c}  ", 3, 7),
        ("  \u{2590}\u{2588}\u{2588}\u{2588}\u{2588}  ", 3, 7),
        ("  \u{259d}\u{259d} \u{259d}\u{259d}  ", 0, 0),
    ];
    const RIGHT_75: &[(&str, usize, usize)] = &[
        ("   \u{2588}\u{2588}\u{259b}\u{258c}  ", 3, 6),
        ("   \u{2588}\u{2588}\u{2588}\u{258c}  ", 3, 6),
        ("   \u{2598}  \u{2598}  ", 0, 0),
    ];
    const EDGE: &[(&str, usize, usize)] = &[
        ("   \u{2590}\u{2588}\u{2588}   ", 4, 6),
        ("   \u{2590}\u{2588}\u{2588}   ", 4, 6),
        ("   \u{259d} \u{259d}   ", 0, 0),
    ];
    const BACK_105: &[(&str, usize, usize)] = &[
        ("   \u{2588}\u{2588}\u{2588}\u{258c}  ", 3, 6),
        ("   \u{2588}\u{2588}\u{2588}\u{258c}  ", 3, 6),
        ("   \u{2598}  \u{2598}  ", 0, 0),
    ];
    const BACK_125: &[(&str, usize, usize)] = &[
        ("  \u{2590}\u{2588}\u{2588}\u{2588}\u{2588}  ", 3, 7),
        ("  \u{2590}\u{2588}\u{2588}\u{2588}\u{2588}  ", 3, 7),
        ("  \u{259d}\u{259d} \u{259d}\u{259d}  ", 0, 0),
    ];
    const BACK_150: &[(&str, usize, usize)] = &[
        ("  \u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{258c} ", 2, 7),
        (
            " \u{259d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259b} ",
            2,
            7,
        ),
        ("  \u{2598}\u{2598}  \u{2598}\u{2598} ", 0, 0),
    ];
    const BACK: &[(&str, usize, usize)] = &[
        (
            " \u{2590}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588} ",
            2,
            8,
        ),
        (
            "\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}",
            2,
            7,
        ),
        (" \u{259d}\u{259d}   \u{259d}\u{259d} ", 0, 0),
    ];
    const LEFT_75: &[(&str, usize, usize)] = &[
        ("   \u{259b}\u{2588}\u{2588}\u{258c}  ", 3, 6),
        ("   \u{2588}\u{2588}\u{2588}\u{258c}  ", 3, 6),
        ("   \u{2598}  \u{2598}  ", 0, 0),
    ];
    const LEFT_55: &[(&str, usize, usize)] = &[
        ("  \u{2590}\u{259c}\u{259b}\u{2588}\u{2588}  ", 3, 7),
        ("  \u{2590}\u{2588}\u{2588}\u{2588}\u{2588}  ", 3, 7),
        ("  \u{259d}\u{259d} \u{259d}\u{259d}  ", 0, 0),
    ];
    const LEFT_30: &[(&str, usize, usize)] = &[
        ("  \u{259b}\u{2588}\u{2588}\u{259b}\u{2588}\u{258c} ", 2, 7),
        (
            " \u{259d}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{259b} ",
            2,
            7,
        ),
        ("  \u{2598}\u{2598}  \u{2598}\u{2598} ", 0, 0),
    ];
    const LEFT_12: &[(&str, usize, usize)] = &[
        (
            " \u{2590}\u{259b}\u{2588}\u{2588}\u{2588}\u{259c}\u{2588} ",
            2,
            8,
        ),
        (
            "\u{259d}\u{259c}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2588}\u{2580}",
            2,
            7,
        ),
        (" \u{259d}\u{259d}   \u{259d}\u{259d} ", 0, 0),
    ];
    match facing {
        Facing::Right12 => RIGHT_12,
        Facing::Right30 => RIGHT_30,
        Facing::Right55 => RIGHT_55,
        Facing::Right75 => RIGHT_75,
        Facing::Edge => EDGE,
        Facing::Back105 => BACK_105,
        Facing::Back125 => BACK_125,
        Facing::Back150 => BACK_150,
        Facing::Back => BACK,
        Facing::Left75 => LEFT_75,
        Facing::Left55 => LEFT_55,
        Facing::Left30 => LEFT_30,
        Facing::Left12 => LEFT_12,
    }
}

#[derive(Clone, Copy)]
struct Playing {
    anim: Anim,
    start_ms: u64,
}

/// The header mascot's playback state: which animation runs, and when the
/// next idle or big moment is due. The render path reads [`Clawd::frame`] and
/// never mutates it; [`Clawd::tick`] moves the schedule.
pub(crate) struct Clawd {
    playing: Option<Playing>,
    next_subtle_ms: u64,
    next_big_ms: u64,
    seed: u64,
}

impl Clawd {
    pub(crate) fn new() -> Self {
        let mut clawd = Self {
            playing: None,
            next_subtle_ms: SUBTLE_PERIOD_MS,
            next_big_ms: BIG_PERIOD_MS,
            seed: random_seed(),
        };
        let anim = BIG[clawd.pick(BIG.len())];
        clawd.playing = Some(Playing { anim, start_ms: 0 });
        clawd
    }

    pub(crate) fn tick(&mut self, now_ms: u64) {
        if let Some(playing) = self.playing {
            let elapsed = now_ms.saturating_sub(playing.start_ms);
            if elapsed < frames_ms(playing.anim) {
                return;
            }
            self.playing = None;
        }
        if now_ms >= self.next_big_ms {
            let anim = BIG[self.pick(BIG.len())];
            self.playing = Some(Playing {
                anim,
                start_ms: now_ms,
            });
            self.next_big_ms = now_ms.saturating_add(BIG_PERIOD_MS);
            // A big moment restarts the subtle rhythm, so the next small one
            // lands a full period after it rather than in its shadow.
            self.next_subtle_ms = now_ms.saturating_add(SUBTLE_PERIOD_MS);
        } else if now_ms >= self.next_subtle_ms {
            let anim = SUBTLE[self.pick(SUBTLE.len())];
            self.playing = Some(Playing {
                anim,
                start_ms: now_ms,
            });
            self.next_subtle_ms = now_ms.saturating_add(SUBTLE_PERIOD_MS);
        }
    }

    pub(crate) fn frame(&self, now_ms: u64) -> Option<&'static Frame> {
        let playing = self.playing?;
        let index = now_ms.saturating_sub(playing.start_ms) / FRAME_MS;
        playing.anim.frames().get(usize::try_from(index).ok()?)
    }

    /// Render tests pin one animation instead of sampling the random launch
    /// choice. Never compiled into the binary.
    #[cfg(test)]
    pub(crate) fn play(&mut self, anim: Anim, start_ms: u64) {
        self.playing = Some(Playing { anim, start_ms });
    }

    fn pick(&mut self, len: usize) -> usize {
        (self.next_random() % len as u64) as usize
    }

    fn next_random(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }
}

fn frames_ms(anim: Anim) -> u64 {
    anim.frames().len() as u64 * FRAME_MS
}

#[cfg(not(test))]
fn random_seed() -> u64 {
    let mut bytes = [0u8; 8];
    match getrandom::fill(&mut bytes) {
        Ok(()) => u64::from_le_bytes(bytes) | 1,
        Err(_) => 1,
    }
}

/// Render tests sample specific animations; a fixed seed keeps every run on
/// the same choice. The player tests overwrite it where they need another.
#[cfg(test)]
fn random_seed() -> u64 {
    0x9E37_79B9_7F4A_7C15
}

#[cfg(test)]
#[path = "../../tests/inline/tui_clawd.rs"]
mod tests;

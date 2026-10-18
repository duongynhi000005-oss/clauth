//! Footer hint-bar pins.

use super::*;
use crate::profile::{AppConfig, AppState};

fn bare_app() -> App {
    App::new(AppConfig {
        state: AppState::default(),
        profiles: vec![],
    })
}

/// The frame the derivation test reads a scroll stamp against: 80x24, matching
/// [`bare_app`]'s modal-open generation 0 — the same size the render pins draw
/// at, so a `prep` that fakes a bound also fakes the stamp the footer would
/// have read had that frame really drawn.
fn stamp_area() -> ratatui::layout::Rect {
    ratatui::layout::Rect::new(0, 0, 80, 24)
}

/// The stamp a faked bound carries in the derivation fixtures: [`stamp_area`]
/// plus `bare_app`'s generation 0.
fn fresh_stamp() -> ScrollStamp {
    ScrollStamp {
        width: 80,
        height: 24,
        generation: 0,
    }
}

/// The Usage tab advertises the note editor key while an account exists, and
/// drops it on an empty roster — there `n` still starts a new account (the
/// hint derives from the key's behavior on this frame).
#[test]
fn the_usage_hints_advertise_the_note_key() {
    use crate::profile::Profile;
    use std::collections::BTreeMap;
    let _home = crate::testutil::HomeSandbox::new();
    let profile = Profile {
        name: "alice".into(),
        base_url: None,
        api_key: None,
        auto_start: false,
        env: BTreeMap::new(),
        models: Default::default(),
        fallback_threshold: None,
        weekly_threshold: None,
        last_resort: false,
        preferred: false,
        preferred_days: Vec::new(),
        rolling_token: false,
        max_auto_spend: None,
        check_weekly: true,
        check_scoped: true,
        bell_threshold: None,
        disabled: false,
        console: None,
        credentials: None,
        provider: None,
    };
    let with_account = App::new(crate::profile::AppConfig {
        state: crate::profile::AppState::default(),
        profiles: vec![profile],
    });
    let mut app = with_account;
    app.tab = Tab::Usage;
    let hints = tab_hints(&app);
    assert!(
        hints.contains(&("n", "note")),
        "usage hints must name the note key, got {hints:?}"
    );

    let mut empty = bare_app();
    empty.tab = Tab::Usage;
    let hints = tab_hints(&empty);
    assert!(
        hints.iter().all(|(k, _)| *k != "n"),
        "an empty roster keeps n = new account off the hint bar, got {hints:?}"
    );
}

/// On the Setup tab's `alert at` row the bar names the stepper keys, the same
/// pair the Fallback card's threshold row names.
#[test]
fn the_setup_alert_row_advertises_its_stepper_keys() {
    use crate::tui::app::{ConfigFocus, ConfigRow, config_rows};
    let _home = crate::testutil::HomeSandbox::new();
    let mut app = App::new(AppConfig {
        state: AppState::default(),
        profiles: vec![crate::profile::Profile::new("a".to_string(), None, None)],
    });
    app.tab = Tab::Setup;
    app.config_focus = ConfigFocus::Actions;
    app.profile_cursor = 0;
    app.config_action_cursor = config_rows(&app)
        .iter()
        .position(|r| *r == ConfigRow::BellThreshold)
        .expect("the alert row is listed");
    assert_eq!(
        tab_hints(&app),
        [
            ("↑↓", "row"),
            ("+", "raise"),
            ("-", "lower"),
            ("↵", "type"),
            ("a", "actions"),
            ("?", "help"),
            ("q", "back"),
        ]
    );
}

/// A draft field's typed editor reads `↵ done   ←→ caret   esc revert`, a
/// draft cycle row's pending pick `↵ done   esc revert` — the Setup `+ new`
/// form's `done` word, `esc revert` kept because the draft editor's `esc`
/// restores the saved value instead of dropping it.
#[test]
fn the_draft_editors_read_done_not_save() {
    use crate::tui::app::{
        InputState, ShuntAddKind, ShuntDraftField, ShuntFieldEdit, ShuntFieldEditState,
        ShuntFieldKey, UpstreamFieldKind,
    };
    let _home = crate::testutil::HomeSandbox::new();
    let draft_key = ShuntFieldKey::Draft {
        kind: ShuntAddKind::Upstream,
        field: ShuntDraftField::Upstream(UpstreamFieldKind::Name),
    };

    let mut typed = bare_app();
    typed.services.shunt_field_edit = Some(ShuntFieldEditState {
        key: draft_key.clone(),
        edit: ShuntFieldEdit::Typed {
            input: InputState::new(""),
            saved: None,
        },
        refusal: None,
    });
    assert_eq!(
        owner_hints(KeyOwner::ShuntField, &typed, stamp_area()),
        Some(&[("↵", "done"), ("←→", "caret"), ("esc", "revert")][..]),
        "a draft field's typed editor reads `↵ done`, never `↵ save`"
    );

    let mut pick = bare_app();
    pick.services.shunt_field_edit = Some(ShuntFieldEditState {
        key: draft_key,
        edit: ShuntFieldEdit::Pick {
            saved: None,
            picked: "api_key".into(),
        },
        refusal: None,
    });
    assert_eq!(
        owner_hints(KeyOwner::ShuntField, &pick, stamp_area()),
        Some(&[("↵", "done"), ("esc", "revert")][..]),
        "a draft cycle row's pending pick reads `↵ done   esc revert`"
    );
}

/// The re-bind confirm's footer row is pinned at both sizes: the ruled
/// `↑↓ row   space toggle   ↵ choose   q not now` copy (an account list the
/// cursor walks) plus the derived `? help` before `q`; a narrow terminal sheds
/// `↵ choose` first.
#[test]
fn the_rebind_confirm_footer_row_is_pinned() {
    use crate::profile::Profile;
    let _home = crate::testutil::HomeSandbox::new();
    let profiles = vec![
        Profile::new(
            "D1".to_string(),
            Some("https://api.deepseek.com/anthropic".to_string()),
            None,
        ),
        Profile::new(
            "D2".to_string(),
            Some("https://api.deepseek.com/anthropic".to_string()),
            None,
        ),
    ];
    let app = App::new(AppConfig {
        state: AppState {
            profiles: profiles.iter().map(|p| p.name.clone()).collect(),
            ..AppState::default()
        },
        profiles,
    });
    assert!(
        matches!(app.modals.first(), Some(Modal::Rebind(_))),
        "the re-bind confirm is open"
    );

    for (w, h, expected) in [
        (
            80u16,
            40u16,
            "↑↓ row   space toggle   ↵ choose   ? help   q not now",
        ),
        (45u16, 24u16, "↑↓ row   space toggle   ? help   q not now"),
    ] {
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("backend");
        term.draw(|f| crate::tui::render::draw(f, &app))
            .expect("draw");
        let rows = crate::testutil::buffer_rows(term.backend().buffer());
        let footer = rows.last().expect("the footer row").trim();
        assert_eq!(footer, expected, "the rebind footer row at {w}x{h}");
    }
}

// ── modal footer derivation ──────────────────────────────────────────────────

/// The hint pairs one modal's footer derives from.
type HintRow = &'static [(&'static str, &'static str)];

/// One `Modal` variant state whose footer `owner_hints` derives. `row` is the
/// settled 80-col footer row (`None` for a derivation-only frame whose content
/// cannot produce it at the two pinned sizes); `row_45` the 45-col row when it
/// sheds. `prep` sets the frame state the `hints` describe — the scroll bound,
/// a built-in pick — so the derivation drive is deterministic without a draw.
struct ModalCase {
    name: &'static str,
    modal: Modal,
    row: Option<&'static str>,
    row_45: Option<&'static str>,
    hints: HintRow,
    prep: fn(&mut App),
}

/// Draw `app` twice at `w`x`h` and return the buffer's rows.
fn settled_rows(app: &App, w: u16, h: u16) -> Vec<String> {
    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("backend");
    term.draw(|f| crate::tui::render::draw(f, app))
        .expect("draw");
    term.draw(|f| crate::tui::render::draw(f, app))
        .expect("draw");
    crate::testutil::buffer_rows(term.backend().buffer())
}

/// Draw `app` twice at `w`x`h` and return the settled footer row (trimmed).
fn settled_footer(app: &App, w: u16, h: u16) -> String {
    settled_rows(app, w, h)
        .last()
        .expect("the footer row")
        .trim()
        .to_string()
}

fn modal_cases() -> Vec<ModalCase> {
    use crate::actions::{AnchorAction, CaptureSnapshot};
    use crate::presets::{Preset, RebindBlock, RowMove};
    use crate::tui::app::{
        ActionMenuAction, ActionMenuState, CaptureNameForm, ConfirmAction, ConfirmScroll,
        ConfirmState, DivergenceForm, DivergenceTargetForm, EnvCollisionForm, InputState,
        LossWarningDrop, LossWarningState, NamePromptAction, NamePromptForm, PresetApplyButton,
        PresetApplyForm, PresetPickerForm, PresetTarget, RebindForm, TokenPickerForm,
    };
    use std::cell::{Cell, RefCell};

    let preset = Preset {
        name: "p".into(),
        base_url: None,
        models: Default::default(),
        builtin: false,
    };
    let builtin = Preset {
        name: "qwen".into(),
        base_url: None,
        models: Default::default(),
        builtin: true,
    };
    let collision = |reviewing_overwrite: bool| {
        Modal::EnvCollision(EnvCollisionForm {
            profile: "a".into(),
            key: "K".into(),
            reason: "set by the base url field".into(),
            existing_idx: None,
            cursor: 0,
            scroll: Cell::new(0),
            reviewing_overwrite,
            confirm_overwrite: false,
            max_scroll: Cell::new(0),
            max_scroll_stamp: Cell::new(ScrollStamp::default()),
            viewport: Cell::new(0),
            overwrite_needs_review: Cell::new(false),
        })
    };
    let confirm = |detail: Option<String>| {
        Modal::Confirm(ConfirmState {
            message: "rotate 'x'?".into(),
            detail,
            choice: false,
            scroll: ConfirmScroll::default(),
            on_confirm: ConfirmAction::RotateOne("x".into()),
        })
    };
    let preset_apply = |message: String, leaves: Vec<&'static str>| {
        Modal::PresetApply(PresetApplyForm {
            target: PresetTarget::NewAccount,
            preset: preset.clone(),
            message,
            replaces: vec!["model"],
            leaves,
            focus: PresetApplyButton::Keep,
            scroll: ConfirmScroll::default(),
        })
    };
    vec![
        ModalCase {
            name: "help",
            modal: Modal::Help,
            row: Some("↑↓ scroll   q back"),
            row_45: None,
            hints: &[("↑↓", "scroll"), ("q", "back")],
            prep: |a| {
                a.help_max_scroll.set(1);
                a.help_max_scroll_stamp.set(fresh_stamp());
            },
        },
        ModalCase {
            name: "help fit",
            modal: Modal::Help,
            row: None,
            row_45: None,
            hints: &[("q", "back")],
            prep: |a| a.help_max_scroll.set(0),
        },
        ModalCase {
            name: "check output",
            modal: Modal::CheckOutput("masked stderr line\n".repeat(60)),
            row: Some("↑↓ scroll   q back"),
            row_45: None,
            hints: &[("↑↓", "scroll"), ("q", "back")],
            prep: |a| {
                a.check_output_max_scroll.set(1);
                a.check_output_max_scroll_stamp.set(fresh_stamp());
            },
        },
        ModalCase {
            name: "check output fit",
            modal: Modal::CheckOutput("one line".into()),
            row: Some("q back"),
            row_45: None,
            hints: &[("q", "back")],
            prep: |a| a.check_output_max_scroll.set(0),
        },
        ModalCase {
            name: "confirm",
            modal: confirm(None),
            row: Some("←→ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("←→", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "confirm scroll",
            modal: confirm(Some("x".repeat(1200))),
            row: Some("←→ pick   ↵ choose   ↑↓ scroll   q back"),
            row_45: None,
            hints: &[
                ("←→", "pick"),
                ("↵", "choose"),
                ("↑↓", "scroll"),
                ("q", "back"),
            ],
            prep: |a| {
                if let Some(Modal::Confirm(s)) = a.modals.last_mut() {
                    s.scroll.max_scroll.set(1);
                    s.scroll.stamp.set(fresh_stamp());
                }
            },
        },
        ModalCase {
            name: "loss warning",
            modal: Modal::LossWarning(LossWarningState {
                choice: false,
                target: Tab::Usage,
                drop: LossWarningDrop::SetupDraft,
            }),
            row: Some("←→ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("←→", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "preset apply",
            modal: preset_apply("apply 'p'?".into(), vec!["base url"]),
            row: Some("←→ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("←→", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "preset apply scroll",
            modal: preset_apply("x".repeat(1200), vec!["base url"]),
            row: Some("←→ pick   ↵ choose   ↑↓ scroll   q back"),
            row_45: None,
            hints: &[
                ("←→", "pick"),
                ("↵", "choose"),
                ("↑↓", "scroll"),
                ("q", "back"),
            ],
            prep: |a| {
                if let Some(Modal::PresetApply(f)) = a.modals.last_mut() {
                    f.scroll.max_scroll.set(1);
                    f.scroll.stamp.set(fresh_stamp());
                }
            },
        },
        ModalCase {
            name: "divergence",
            modal: Modal::Divergence(DivergenceForm {
                active: "x".into(),
                sibling: None,
                // `discard` sits last so ↵ only opens its confirm, never the
                // overwrite path's credential work, inside a test.
                cursor: 2,
                scroll: Cell::new(0),
                viewport: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
            }),
            row: Some("↑↓ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "divergence target",
            modal: Modal::DivergenceTarget(DivergenceTargetForm {
                targets: vec!["b".into()],
                // A profile row, so ↵ takes the pick's toast path, never the
                // `+ new` capture flow.
                cursor: 1,
                scroll: Cell::new(0),
                viewport: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
            }),
            row: Some("↑↓ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "token picker",
            modal: Modal::TokenPicker(TokenPickerForm {
                targets: vec![("b".into(), false)],
                // A profile row, so ↵ opens the overwrite confirm, never the
                // `+ new profile` form.
                cursor: 1,
                scroll: Cell::new(0),
                viewport: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
                require: false,
            }),
            row: Some("↑↓ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "preset picker",
            modal: Modal::PresetPicker(PresetPickerForm {
                target: PresetTarget::NewAccount,
                presets: vec![preset.clone(), preset],
                cursor: 0,
                scroll: Cell::new(0),
                viewport: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
            }),
            row: Some("↑↓ pick   ↵ apply   d delete   q back"),
            row_45: None,
            hints: &[
                ("↑↓", "pick"),
                ("↵", "apply"),
                ("d", "delete"),
                ("q", "back"),
            ],
            prep: |_| {},
        },
        ModalCase {
            name: "preset picker builtin",
            modal: Modal::PresetPicker(PresetPickerForm {
                target: PresetTarget::NewAccount,
                presets: vec![builtin.clone(), builtin],
                cursor: 0,
                scroll: Cell::new(0),
                viewport: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
            }),
            row: Some("↑↓ pick   ↵ apply   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "apply"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "action menu",
            modal: Modal::ActionMenu(ActionMenuState::new(
                vec![ActionMenuAction::TogglePace],
                vec![ActionMenuAction::ToggleEstimates],
                None,
            )),
            row: Some("↑↓ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "env collision",
            modal: collision(false),
            row: Some("↑↓ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("↑↓", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "env collision review",
            modal: collision(true),
            row: Some("←→ pick   ↵ choose   q back"),
            row_45: None,
            hints: &[("←→", "pick"), ("↵", "choose"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "env collision review scroll",
            modal: collision(true),
            row: None,
            row_45: None,
            hints: &[
                ("←→", "pick"),
                ("↵", "choose"),
                ("↑↓", "scroll"),
                ("q", "back"),
            ],
            prep: |a| {
                if let Some(Modal::EnvCollision(s)) = a.modals.last_mut() {
                    s.max_scroll.set(1);
                    s.max_scroll_stamp.set(fresh_stamp());
                }
            },
        },
        ModalCase {
            name: "capture name",
            modal: Modal::CaptureName(CaptureNameForm {
                snapshot: Box::new(CaptureSnapshot {
                    credentials: None,
                    base_url: None,
                    api_key: None,
                    anchor: AnchorAction::Unproven,
                }),
                input: InputState::new(""),
                from_divergence: false,
            }),
            row: Some("↵ save   ←→ caret   q back"),
            row_45: None,
            hints: &[("↵", "save"), ("←→", "caret"), ("q", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "name prompt",
            modal: Modal::NamePrompt(NamePromptForm {
                input: InputState::new(""),
                action: NamePromptAction::DuplicateProfile("src".into()),
            }),
            row: Some("↵ save   ←→ caret   esc back"),
            row_45: None,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "back")],
            prep: |_| {},
        },
        ModalCase {
            name: "rebind",
            modal: Modal::Rebind(RebindForm {
                blocks: vec![RebindBlock {
                    rows: vec![RowMove {
                        row: "model",
                        old: None,
                        new: None,
                    }],
                    // Two accounts, so ↓ moves the cursor instead of wrapping
                    // onto itself.
                    accounts: vec!["a".into(), "b".into()],
                }],
                checked: vec![vec![false, false]],
                cursor: 0,
                choice: false,
                scroll: Cell::new(0),
                starts: RefCell::new(vec![]),
                block_ends: RefCell::new(vec![]),
                total: Cell::new(0),
                viewport: Cell::new(0),
                digest: "d".into(),
            }),
            row: Some("↑↓ row   space toggle   ↵ choose   ? help   q not now"),
            row_45: Some("↑↓ row   space toggle   ? help   q not now"),
            hints: &[
                ("↑↓", "row"),
                ("space", "toggle"),
                ("↵", "choose"),
                ("q", "not now"),
            ],
            prep: |_| {},
        },
    ]
}

/// A stable index per `Modal` variant. Exhaustive (no wildcard): a new variant
/// fails to compile until it gets an arm here, and the coverage test below
/// fails until a case drives it. That guard fires only once `MODAL_COUNT` is
/// bumped past the new arm's index; an arm numbered at or past `MODAL_COUNT`
/// reds on first use via the assert in [`modal_selection`].
fn modal_index(modal: &Modal) -> usize {
    match modal {
        Modal::Help => 0,
        Modal::CheckOutput(_) => 1,
        Modal::Confirm(_) => 2,
        Modal::LossWarning(_) => 3,
        Modal::PresetApply(_) => 4,
        Modal::EnvCollision(_) => 5,
        Modal::Divergence(_) => 6,
        Modal::DivergenceTarget(_) => 7,
        Modal::TokenPicker(_) => 8,
        Modal::PresetPicker(_) => 9,
        Modal::ActionMenu(_) => 10,
        Modal::CaptureName(_) => 11,
        Modal::NamePrompt(_) => 12,
        Modal::Rebind(_) => 13,
        Modal::Login => 14,
    }
}

/// The number of `Modal` variants; [`modal_index`] returns `0..MODAL_COUNT`.
/// Bumped with `modal_index` — the coverage test reds when an index has no
/// case. That guard fires only once `MODAL_COUNT` is bumped past the new arm's
/// index.
const MODAL_COUNT: usize = 15;

/// Every `Modal` variant has a case driving its row; the login modal is the
/// one variant a case cannot cover (its live-stage line owns the footer), so
/// its index is skipped.
#[test]
fn the_modal_case_list_covers_every_variant() {
    let mut covered = [false; MODAL_COUNT];
    for case in modal_cases() {
        covered[modal_index(&case.modal)] = true;
    }
    let login = modal_index(&Modal::Login);
    for (index, is_covered) in covered.iter().enumerate() {
        assert!(
            *is_covered || index == login,
            "no modal case covers variant index {index}"
        );
    }
}

/// A stable fingerprint of the modal's movable state, so a drive can assert the
/// key did something without naming every field per modal. Exhaustive by
/// construction: a new `Modal` variant fails to compile here too.
fn modal_selection(app: &App) -> String {
    let top = app.modals.last();
    if let Some(modal) = top {
        let index = modal_index(modal);
        assert!(
            index < MODAL_COUNT,
            "modal arm {index} is at or past MODAL_COUNT {MODAL_COUNT}; bump MODAL_COUNT"
        );
    }
    match top {
        Some(Modal::Confirm(s)) => format!("{}:{}", s.choice, s.scroll.offset),
        Some(Modal::LossWarning(s)) => format!("{}", s.choice),
        Some(Modal::PresetApply(f)) => format!("{:?}:{}", f.focus, f.scroll.offset),
        Some(Modal::Divergence(f)) => format!("{}", f.cursor),
        Some(Modal::DivergenceTarget(f)) => format!("{}", f.cursor),
        Some(Modal::TokenPicker(f)) => format!("{}", f.cursor),
        Some(Modal::PresetPicker(f)) => format!("{}", f.cursor),
        Some(Modal::ActionMenu(f)) => format!("{}", f.cursor),
        Some(Modal::EnvCollision(f)) => format!(
            "{}:{}:{}:{}",
            f.cursor,
            f.reviewing_overwrite,
            f.confirm_overwrite,
            f.scroll.get()
        ),
        Some(Modal::CaptureName(f)) => format!("{}:{}", f.input.value, f.input.cursor),
        Some(Modal::NamePrompt(f)) => format!("{}:{}", f.input.value, f.input.cursor),
        Some(Modal::Rebind(f)) => format!("{}:{}:{:?}", f.choice, f.cursor, f.checked),
        Some(Modal::Help) => format!("{}", app.help_scroll),
        Some(Modal::CheckOutput(_)) => format!("{}", app.check_output_scroll),
        Some(Modal::Login) => String::new(),
        None => String::new(),
    }
}

/// A stable fingerprint of the keyboard owner's own movable state. Exhaustive
/// by construction over `KeyOwner`.
fn owner_selection(app: &App) -> String {
    let owner = keyboard_owner(app);
    if let Some(owner) = owner {
        let index = owner_index(owner);
        assert!(
            index < OWNER_COUNT,
            "owner arm {index} is at or past OWNER_COUNT {OWNER_COUNT}; bump OWNER_COUNT"
        );
    }
    match owner {
        Some(KeyOwner::Modal) => modal_selection(app),
        Some(KeyOwner::SetupField | KeyOwner::NewAccountField) => app
            .config_draft
            .as_ref()
            .map(|d| format!("{d:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::MemberField) => app
            .fallback_edit
            .as_ref()
            .map(|e| format!("{e:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::DayPicker) => {
            let days = app
                .config()
                .find(&crate::profile::ProfileName::from("a".to_string()))
                .map(|p| p.preferred_days.len())
                .unwrap_or(0);
            format!("{:?}|{}", app.fallback_edit, days)
        }
        Some(KeyOwner::RefreshInterval) => app
            .refresh_interval_draft
            .as_ref()
            .map(|i| format!("{i:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::ContextNudge) => app
            .context_nudge_draft
            .as_ref()
            .map(|i| format!("{i:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::WeeklyThreshold) => app
            .weekly_threshold_draft
            .as_ref()
            .map(|i| format!("{i:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::HerdrTag) => app
            .services
            .herdr_tag_draft
            .as_ref()
            .map(|i| format!("{i:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::ShuntField) => app
            .services
            .shunt_field_edit
            .as_ref()
            .map(|s| format!("{s:?}"))
            .unwrap_or_default(),
        Some(KeyOwner::NoteEditor) => app
            .note_editor
            .as_ref()
            .map(|n| format!("{n:?}"))
            .unwrap_or_default(),
        None => String::new(),
    }
}

/// Every observable surface a drive can change: the top modal's variant and
/// state, the keyboard owner's variant and state, and the toast / quit / alert
/// surfaces.
fn fingerprint(app: &App) -> String {
    format!(
        "{:?}|{}|{:?}|{}|{}|{}|{}|{}",
        app.modals.last().map(std::mem::discriminant),
        modal_selection(app),
        keyboard_owner(app),
        owner_selection(app),
        app.toasts.len(),
        app.armed_quit,
        app.footer_alert.is_some(),
        app.services.shunt_save.is_some(),
    )
}

/// Drive `event` through the real handler and report whether anything
/// observable changed. A control pins its own correctness (see
/// [`the_acted_probe_has_a_control`]).
fn acted(app: &mut App, event: ratatui::crossterm::event::KeyEvent) -> bool {
    let before = fingerprint(app);
    crate::tui::app::handle_key(app, event);
    before != fingerprint(app)
}

/// The footer shows the modal's own keys alone: every modal's full row, pinned
/// by equality at the two sizes the task names, with no screen-level group.
#[test]
fn the_each_modal_footer_shows_only_its_own_keys() {
    let _home = crate::testutil::HomeSandbox::new();
    for case in modal_cases() {
        let Some(row) = case.row else { continue };
        let mut app = bare_app();
        app.modals.push(case.modal.clone());
        for (w, h) in [(80u16, 24u16), (45u16, 24u16)] {
            let expected = if w == 45 {
                case.row_45.unwrap_or(row)
            } else {
                row
            };
            let footer = settled_footer(&app, w, h);
            assert_eq!(footer, expected, "{} footer at {w}x{h}", case.name);
        }
    }
}

/// The ruled rows where `←`/`→` act without a `←→` hint group: the re-bind
/// confirm's `←`/`→` flip the focused button, so its ruled copy leaves them
/// unhinted and the derivation drive names it instead of asserting inert.
const OMITTED_ARROWS: &[(&str, &str)] = &[("rebind", "←/→ flip the focused button")];

/// Every hinted key is derived: it acts through the real handler, the `↑↓`
/// lead is offered exactly where a press scrolls, and the screen-level keys the
/// row drops are genuinely inert under the modal (no tab switch, no action
/// menu, no quit arm).
#[test]
fn the_every_modal_hint_derives_from_the_handler() {
    use ratatui::crossterm::event::KeyCode;

    let _home = crate::testutil::HomeSandbox::new();

    for case in modal_cases() {
        let expected = case.hints;

        let mut app = bare_app();
        app.modals.push(case.modal.clone());
        (case.prep)(&mut app);
        assert_eq!(
            owner_hints(KeyOwner::Modal, &app, stamp_area()),
            Some(expected),
            "{}: the row derives from owner_hints",
            case.name
        );

        // Every hinted key acts through handle_key.
        for (hint, _) in expected {
            let mut app = bare_app();
            app.modals.push(case.modal.clone());
            (case.prep)(&mut app);
            let code = match *hint {
                "↑↓" => KeyCode::Down,
                "←→" if case.name == "capture name" || case.name == "name prompt" => {
                    KeyCode::Left
                }
                "←→" => KeyCode::Right,
                "↵" => KeyCode::Enter,
                "space" => KeyCode::Char(' '),
                "esc" => KeyCode::Esc,
                "q" => KeyCode::Char('q'),
                "d" => KeyCode::Char('d'),
                other => unreachable!("unmapped hint key {other}"),
            };
            // A typed field's caret sits at the end of an empty value, where →
            // is a no-op: give it a character so the arrow moves the caret.
            if *hint == "←→" && (case.name == "capture name" || case.name == "name prompt") {
                crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('a')));
            }
            assert!(
                acted(&mut app, crate::testutil::key(code)),
                "{}: `{hint}` acts",
                case.name
            );
        }

        // The `↑↓` lead is offered exactly where a press scrolls: drive ↓ on
        // every modal and require it acts iff the row names it.
        let mut app = bare_app();
        app.modals.push(case.modal.clone());
        (case.prep)(&mut app);
        let has_updown = expected.iter().any(|(k, _)| *k == "↑↓");
        assert_eq!(
            acted(&mut app, crate::testutil::key(KeyCode::Down)),
            has_updown,
            "{}: ↓ acts iff the row offers ↑↓",
            case.name
        );

        // `←` acts iff the row offers `←→` — except the ruled rows in
        // [`OMITTED_ARROWS`], whose `←`/`→` act while the copy leaves them
        // unhinted. A typed-entry field's caret sits at the end of an empty
        // value, so give it a character first.
        let mut app = bare_app();
        app.modals.push(case.modal.clone());
        (case.prep)(&mut app);
        if case.name == "capture name" || case.name == "name prompt" {
            crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('a')));
        }
        let has_lr = expected.iter().any(|(k, _)| *k == "←→");
        let left_acts = acted(&mut app, crate::testutil::key(KeyCode::Left));
        if let Some((_, reason)) = OMITTED_ARROWS.iter().find(|(n, _)| *n == case.name) {
            assert!(left_acts, "{}: {reason}", case.name);
        } else {
            assert_eq!(
                left_acts, has_lr,
                "{}: ← acts iff the row offers ←→",
                case.name
            );
        }

        // The screen-level keys the row drops are inert under the modal.
        let mut app = bare_app();
        app.modals.push(case.modal.clone());
        let tab_before = app.tab;
        crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Left));
        crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Right));
        assert_eq!(app.tab, tab_before, "{}: ←/→ do not switch tabs", case.name);
        let stack_before = app.modals.len();
        crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('a')));
        assert_eq!(
            app.modals.len(),
            stack_before,
            "{}: a opens no action menu",
            case.name
        );

        let mut app = bare_app();
        app.modals.push(case.modal);
        crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('q')));
        assert!(!app.armed_quit, "{}: q does not arm quit", case.name);
        assert!(
            app.footer_alert.is_none(),
            "{}: q raises no quit alert",
            case.name
        );
    }
}

/// At least one fixture per `KeyOwner` variant (the shunt field editor gets one
/// per row state): enter that owner state and derive its row, drive every
/// hinted key, and prove `←→` is inert exactly where the row sheds it (a `Pick`
/// cycle row has no caret).
#[test]
fn the_every_key_owner_hint_derives_from_the_handler() {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let _home = crate::testutil::HomeSandbox::new();

    for case in owner_cases() {
        let OwnerCase {
            name,
            owner,
            hints,
            build,
        } = case;
        let app = build();
        assert_eq!(
            owner_hints(owner, &app, stamp_area()),
            Some(hints),
            "{name}: the row derives from owner_hints"
        );

        for (hint, _) in hints {
            let mut app = build();
            if *hint == "←→" {
                crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('a')));
            }
            let event = match *hint {
                "↑↓" => crate::testutil::key(KeyCode::Down),
                "←→" => crate::testutil::key(KeyCode::Left),
                "↵" => crate::testutil::key(KeyCode::Enter),
                "space" => crate::testutil::key(KeyCode::Char(' ')),
                "esc" => crate::testutil::key(KeyCode::Esc),
                "q" => crate::testutil::key(KeyCode::Char('q')),
                "⌃j" => KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
                other => unreachable!("unmapped hint key {other}"),
            };
            assert!(acted(&mut app, event), "{name}: `{hint}` acts");
        }

        // `←→` is inert exactly where the row sheds it: a Pick cycle row has
        // no caret, a typed field's does.
        let mut app = build();
        crate::tui::app::handle_key(&mut app, crate::testutil::key(KeyCode::Char('a')));
        let has_lr = hints.iter().any(|(k, _)| *k == "←→");
        assert_eq!(
            acted(&mut app, crate::testutil::key(KeyCode::Left)),
            has_lr,
            "{name}: ←→ acts iff the row offers it"
        );
    }
}

/// One `KeyOwner` fixture: enter that owner state (`build`) and derive its row.
struct OwnerCase {
    name: &'static str,
    owner: KeyOwner,
    hints: HintRow,
    build: fn() -> App,
}

fn owner_cases() -> Vec<OwnerCase> {
    vec![
        OwnerCase {
            name: "setup field",
            owner: KeyOwner::SetupField,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: setup_field_app,
        },
        OwnerCase {
            name: "new account field",
            owner: KeyOwner::NewAccountField,
            hints: &[("↵", "done"), ("←→", "caret"), ("esc", "done")],
            build: new_account_field_app,
        },
        OwnerCase {
            name: "member field",
            owner: KeyOwner::MemberField,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: member_field_app,
        },
        OwnerCase {
            name: "day picker",
            owner: KeyOwner::DayPicker,
            hints: &[
                ("←→", "day"),
                ("space", "toggle"),
                ("↑↓", "row"),
                ("q", "back"),
            ],
            build: day_picker_app,
        },
        OwnerCase {
            name: "refresh interval",
            owner: KeyOwner::RefreshInterval,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: refresh_interval_app,
        },
        OwnerCase {
            name: "context nudge",
            owner: KeyOwner::ContextNudge,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: context_nudge_app,
        },
        OwnerCase {
            name: "weekly threshold",
            owner: KeyOwner::WeeklyThreshold,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: weekly_threshold_app,
        },
        OwnerCase {
            name: "herdr tag",
            owner: KeyOwner::HerdrTag,
            hints: &[("↵", "save"), ("←→", "caret"), ("esc", "revert")],
            build: herdr_tag_app,
        },
        OwnerCase {
            name: "shunt draft pick",
            owner: KeyOwner::ShuntField,
            hints: &[("↵", "done"), ("esc", "revert")],
            build: shunt_draft_pick_app,
        },
        OwnerCase {
            name: "shunt pick",
            owner: KeyOwner::ShuntField,
            hints: &[("↵", "save"), ("esc", "revert")],
            build: shunt_pick_app,
        },
        OwnerCase {
            name: "shunt draft field",
            owner: KeyOwner::ShuntField,
            hints: &[("↵", "done"), ("←→", "caret"), ("esc", "revert")],
            build: shunt_draft_field_app,
        },
        OwnerCase {
            name: "shunt field locked",
            owner: KeyOwner::ShuntField,
            hints: &[("↑↓", "row"), ("q", "back")],
            build: shunt_locked_app,
        },
        OwnerCase {
            name: "note editor",
            owner: KeyOwner::NoteEditor,
            hints: &[
                ("↵", "save"),
                ("⌃j", "newline"),
                ("←→", "caret"),
                ("esc", "cancel"),
            ],
            build: note_editor_app,
        },
    ]
}

/// A stable index per `KeyOwner` variant. Exhaustive (no wildcard): a new
/// variant fails to compile until it gets an arm here, and the coverage test
/// below fails until a case drives it. That guard fires only once `OWNER_COUNT`
/// is bumped past the new arm's index; an arm numbered at or past `OWNER_COUNT`
/// reds on first use via the assert in [`owner_selection`].
fn owner_index(owner: KeyOwner) -> usize {
    match owner {
        KeyOwner::Modal => 0,
        KeyOwner::SetupField => 1,
        KeyOwner::NewAccountField => 2,
        KeyOwner::MemberField => 3,
        KeyOwner::DayPicker => 4,
        KeyOwner::RefreshInterval => 5,
        KeyOwner::ContextNudge => 6,
        KeyOwner::WeeklyThreshold => 7,
        KeyOwner::HerdrTag => 8,
        KeyOwner::ShuntField => 9,
        KeyOwner::NoteEditor => 10,
    }
}

/// The number of `KeyOwner` variants; [`owner_index`] returns `0..OWNER_COUNT`.
/// Bumped with `owner_index` — the coverage test reds when an index has no
/// case. That guard fires only once `OWNER_COUNT` is bumped past the new arm's
/// index.
const OWNER_COUNT: usize = 11;

/// Every `KeyOwner` variant has a case driving its row. The modal owner's row
/// is the top modal's, driven by [`modal_cases`], so its index is skipped.
#[test]
fn the_owner_case_list_covers_every_variant() {
    let mut covered = [false; OWNER_COUNT];
    for case in owner_cases() {
        covered[owner_index(case.owner)] = true;
    }
    let modal = owner_index(KeyOwner::Modal);
    for (index, is_covered) in covered.iter().enumerate() {
        assert!(
            *is_covered || index == modal,
            "no owner case covers variant index {index}"
        );
    }
}

fn setup_field_app() -> App {
    use crate::tui::app::{ConfigFocus, ConfigRow, InputState, build_draft_new};
    let mut app = bare_app();
    app.tab = Tab::Setup;
    app.config_focus = ConfigFocus::Actions;
    let mut draft = build_draft_new();
    draft.editing_name = Some("a".into());
    draft.active = Some(ConfigRow::BaseUrl);
    draft.base_url = InputState::new("http://127.0.0.1:1234");
    app.config_draft = Some(draft);
    app
}

fn new_account_field_app() -> App {
    use crate::tui::app::{ConfigFocus, ConfigRow, InputState, build_draft_new};
    let mut app = bare_app();
    app.tab = Tab::Setup;
    app.config_focus = ConfigFocus::Actions;
    let mut draft = build_draft_new();
    draft.active = Some(ConfigRow::BaseUrl);
    draft.base_url = InputState::new("http://127.0.0.1:1234");
    app.config_draft = Some(draft);
    app
}

fn member_field_app() -> App {
    use crate::tui::app::{CardEdit, FallbackFocus, InputState, MemberEdit};
    let mut app = bare_app();
    app.tab = Tab::Fallback;
    app.fallback_focus = FallbackFocus::Detail;
    app.fallback_edit = Some(MemberEdit {
        member: "a".into(),
        state: CardEdit::Threshold(InputState::new("70")),
    });
    app
}

fn day_picker_app() -> App {
    use crate::profile::Profile;
    use crate::tui::app::{CardEdit, DayPicker, FallbackFocus, MemberEdit};
    let mut app = App::new(AppConfig {
        state: AppState {
            profiles: vec!["a".into()],
            fallback_chain: vec!["a".into()],
            ..AppState::default()
        },
        profiles: vec![Profile::new("a".to_string(), None, None)],
    });
    app.tab = Tab::Fallback;
    app.fallback_focus = FallbackFocus::Detail;
    app.fallback_edit = Some(MemberEdit {
        member: "a".into(),
        state: CardEdit::Days(DayPicker::default()),
    });
    app
}

fn refresh_interval_app() -> App {
    use crate::tui::app::InputState;
    let mut app = bare_app();
    app.tab = Tab::Config;
    app.refresh_interval_draft = Some(InputState::new("30"));
    app
}

fn context_nudge_app() -> App {
    use crate::tui::app::InputState;
    let mut app = bare_app();
    app.tab = Tab::Config;
    app.context_nudge_draft = Some(InputState::new("300k"));
    app
}

fn weekly_threshold_app() -> App {
    use crate::tui::app::InputState;
    let mut app = bare_app();
    app.tab = Tab::Config;
    app.weekly_threshold_draft = Some(InputState::new("50"));
    app
}

fn herdr_tag_app() -> App {
    use crate::tui::app::InputState;
    let mut app = bare_app();
    app.tab = Tab::Services;
    app.services.herdr_tag_draft = Some(InputState::new("30"));
    app
}

fn shunt_draft_pick_app() -> App {
    use crate::tui::app::{
        ShuntAddKind, ShuntDraftField, ShuntFieldEdit, ShuntFieldEditState, ShuntFieldKey,
        UpstreamFieldKind,
    };
    let key = ShuntFieldKey::Draft {
        kind: ShuntAddKind::Upstream,
        field: ShuntDraftField::Upstream(UpstreamFieldKind::Name),
    };
    let mut app = bare_app();
    app.tab = Tab::Services;
    app.services.shunt_field_edit = Some(ShuntFieldEditState {
        key,
        edit: ShuntFieldEdit::Pick {
            saved: None,
            picked: "api_key".into(),
        },
        refusal: None,
    });
    app
}

/// A non-draft field's pending pick: `↵ save` commits through the gateway
/// record (the save worker marks the row saving), `esc revert` closes it.
fn shunt_pick_app() -> App {
    use crate::tui::app::{
        ShuntConfigRead, ShuntFieldEdit, ShuntFieldEditState, ShuntFieldKey, UpstreamFieldKind,
    };
    let home = crate::profile::home_dir().expect("sandboxed home");
    let config = home.join("etc").join("shunt.toml");
    std::fs::create_dir_all(config.parent().expect("parent")).expect("mkdir");
    std::fs::write(
        &config,
        "[server]\nbind = \"127.0.0.1:9\"\n\n[[upstreams]]\nname = \"a\"\nauth = \"claude_oauth\"\n",
    )
    .expect("write config");
    let record = crate::gateway::GatewayRecord::new(config).expect("adoptable");
    let view = crate::gateway_edit::read_view(&record).expect("readable");
    let mut app = bare_app();
    app.tab = Tab::Services;
    app.services.shunt_record = Some(record);
    app.services.shunt_config = Some(ShuntConfigRead::Ok(Box::new(view)));
    app.services.shunt_field_edit = Some(ShuntFieldEditState {
        key: ShuntFieldKey::Upstream {
            name: "a".into(),
            occurrence: 0,
            field: UpstreamFieldKind::Effort,
        },
        edit: ShuntFieldEdit::Pick {
            saved: None,
            picked: "low".into(),
        },
        refusal: None,
    });
    app
}

/// A draft field's typed editor: `↵ done` lands in the draft (nothing saves
/// until the form's own save row), `←→` move the caret, `esc` reverts.
fn shunt_draft_field_app() -> App {
    use crate::tui::app::{
        InputState, ShuntAddKind, ShuntDraftField, ShuntFieldEdit, ShuntFieldEditState,
        ShuntFieldKey, UpstreamFieldKind,
    };
    let key = ShuntFieldKey::Draft {
        kind: ShuntAddKind::Upstream,
        field: ShuntDraftField::Upstream(UpstreamFieldKind::Name),
    };
    let mut app = bare_app();
    app.tab = Tab::Services;
    app.services.shunt_field_edit = Some(ShuntFieldEditState {
        key,
        edit: ShuntFieldEdit::Typed {
            input: InputState::new("x"),
            saved: None,
        },
        refusal: None,
    });
    app
}

fn shunt_locked_app() -> App {
    use crate::tui::app::{
        InputState, ShuntFieldEdit, ShuntFieldEditState, ShuntFieldKey, ShuntSave, ShuntSaveKey,
        UpstreamFieldKind,
    };
    use std::time::Instant;
    let key = ShuntFieldKey::Upstream {
        name: "a".into(),
        occurrence: 0,
        field: UpstreamFieldKind::Effort,
    };
    let mut app = bare_app();
    app.tab = Tab::Services;
    app.services.shunt_field_edit = Some(ShuntFieldEditState {
        key: key.clone(),
        edit: ShuntFieldEdit::Typed {
            input: InputState::new("x"),
            saved: None,
        },
        refusal: None,
    });
    app.services.shunt_save = Some(ShuntSave {
        key: ShuntSaveKey::Field(key),
        started: Instant::now(),
        value: None,
        entry: None,
    });
    app
}

fn note_editor_app() -> App {
    use crate::tui::app::{NoteBuffer, NoteEditor};
    let mut app = bare_app();
    app.tab = Tab::Usage;
    app.note_editor = Some(NoteEditor {
        profile: "a".into(),
        buf: NoteBuffer::new("x"),
    });
    app
}

/// The `acted` probe's own control: an inert key reads `!acted`, and a state
/// change reads `acted`, so a broken `acted` (always true or always false) reds.
#[test]
fn the_acted_probe_has_a_control() {
    use crate::tui::app::{ConfirmAction, ConfirmScroll, ConfirmState};
    use ratatui::crossterm::event::KeyCode;
    let _home = crate::testutil::HomeSandbox::new();
    let mut app = bare_app();
    app.modals.push(Modal::Confirm(ConfirmState {
        message: "rotate 'x'?".into(),
        detail: None,
        choice: false,
        scroll: ConfirmScroll::default(),
        on_confirm: ConfirmAction::RotateOne("x".into()),
    }));
    assert!(
        !acted(&mut app, crate::testutil::key(KeyCode::Char('?'))),
        "? is inert on a confirm"
    );
    assert!(
        acted(&mut app, crate::testutil::key(KeyCode::Right)),
        "→ flips the confirm choice"
    );
}

/// A login in flight keeps its lead line and shows the open modal's own row
/// behind it (not the base `q back`, and never `q quit`). The name prompt reads
/// `esc back` there, so the bare `cancel` can't read as the login's.
#[test]
fn the_login_footer_shows_the_open_modals_row() {
    use crate::tui::app::{
        ActionMenuAction, ActionMenuState, ConfirmAction, ConfirmScroll, ConfirmState, InputState,
        LoginMethod, LoginStage, NamePromptAction, NamePromptForm,
    };
    let _home = crate::testutil::HomeSandbox::new();

    let login = || LoginSession {
        name: "fresh".into(),
        is_new: false,
        generation: 0,
        url: None,
        stage: LoginStage::WaitingBrowser,
        method: LoginMethod::Browser,
        paste: None,
        paste_field: None,
    };
    let confirm = Modal::Confirm(ConfirmState {
        message: "rotate 'x'?".into(),
        detail: None,
        choice: false,
        scroll: ConfirmScroll::default(),
        on_confirm: ConfirmAction::RotateOne("x".into()),
    });
    let action_menu = Modal::ActionMenu(ActionMenuState::new(
        vec![ActionMenuAction::TogglePace],
        vec![],
        None,
    ));
    let name_prompt = Modal::NamePrompt(NamePromptForm {
        input: InputState::new(""),
        action: NamePromptAction::DuplicateProfile("src".into()),
    });

    let cases: Vec<(&str, Modal, &str, &str)> = vec![
        (
            "help",
            Modal::Help,
            "⠋ logging in 'fresh'   ↑↓ scroll   q back",
            "⠋ logging in 'fresh'   ↑↓ scroll   q back",
        ),
        (
            "confirm",
            confirm,
            "⠋ logging in 'fresh'   ←→ pick   ↵ choose   q back",
            "⠋ logging in 'fresh'   ←→ pick   q back",
        ),
        (
            "action menu",
            action_menu,
            "⠋ logging in 'fresh'   ↑↓ pick   ↵ choose   q back",
            "⠋ logging in 'fresh'   ↑↓ pick   q back",
        ),
        (
            "name prompt",
            name_prompt,
            "⠋ logging in 'fresh'   ↵ save   ←→ caret   esc back",
            "⠋ logging in 'fresh'   ↵ save   esc back",
        ),
    ];

    for (name, modal, row_80, row_45) in cases {
        for (w, h, expected) in [(80u16, 24u16, row_80), (45u16, 24u16, row_45)] {
            let mut app = bare_app();
            app.login = Some(login());
            app.modals.push(modal.clone());
            let footer = settled_footer(&app, w, h);
            assert_eq!(footer, expected, "{name} login footer at {w}x{h}");
        }
    }
}

/// With the base frame order the footer draws before the modal, so the bound it
/// reads is the previous frame's. The stamp rule keeps that honest: a bound
/// whose stamp does not match this frame's area (a resize) or the modal-open
/// generation (an open or close) errs to the no-scroll row.
#[test]
fn the_footer_reads_the_scroll_bound_on_the_first_frame() {
    use crate::tui::app::{ConfirmAction, ConfirmScroll, ConfirmState};
    let _home = crate::testutil::HomeSandbox::new();

    // Open help at 80x24 (it overflows), then resize to 200x100 where it fits:
    // the one frame after the resize must already drop the dead `↑↓ scroll` —
    // the bound is stamped with the 80x24 area, which no longer matches.
    let mut app = bare_app();
    app.modals.push(Modal::Help);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    term.backend_mut().resize(200, 100);
    term.autoresize().unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "q back",
        "help at 200x100 shows no dead ↑↓ scroll on the first resized frame"
    );

    // A long confirm's first frame errs to the no-scroll row — the footer draws
    // before the modal, so the bound is not published yet — and the second
    // frame names its scroll.
    let mut app = bare_app();
    app.modals.push(Modal::Confirm(ConfirmState {
        message: "rotate 'x'?".into(),
        detail: Some("x".repeat(1200)),
        choice: false,
        scroll: ConfirmScroll::default(),
        on_confirm: ConfirmAction::RotateOne("x".into()),
    }));
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "←→ pick   ↵ choose   q back",
        "a long confirm's first frame errs to the no-scroll row"
    );
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "←→ pick   ↵ choose   ↑↓ scroll   q back",
        "a long confirm's second frame names its ↑↓ scroll"
    );
}

/// A bound that survives a close is not trusted after a reopen: the close and
/// the reopen bump the modal-open generation and the resize changes the area,
/// so the first frame after the reopen errs to `q back` whatever the size.
#[test]
fn a_reopened_modal_never_shows_a_stale_scroll() {
    let _home = crate::testutil::HomeSandbox::new();

    let mut app = bare_app();
    app.open_modal(Modal::Help);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    // `help_max_scroll` survives the close, then a resize and a reopen leave it
    // stale for the footer's first frame.
    app.modals.pop();
    term.backend_mut().resize(200, 100);
    term.autoresize().unwrap();
    app.open_modal(Modal::Help);
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "q back",
        "a help modal reopened after a resize shows no stale ↑↓ scroll on its first frame"
    );
}

/// The generation counter alone errs a stale bound: a reopen at the same size
/// leaves the area matching, but the open and close bumped the generation, so
/// the first frame still errs to `q back` and the second shows the bound.
#[test]
fn a_reopened_modal_errs_until_it_redraws() {
    let _home = crate::testutil::HomeSandbox::new();

    let mut app = bare_app();
    app.open_modal(Modal::Help);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    app.modals.pop();
    app.open_modal(Modal::Help);
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "q back",
        "a help modal reopened at the same size errs to q back on its first frame"
    );
    term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
    let rows = crate::testutil::buffer_rows(term.backend().buffer());
    let footer = rows.last().expect("the footer row").trim();
    assert_eq!(
        footer, "↑↓ scroll   q back",
        "the second frame after a same-size reopen shows the scroll"
    );
}

/// The env-collision list stage must not refresh the review stage's scroll
/// stamp: only a frame that drew the review stage stamps its bound, so a
/// resize while the list is open leaves the review bound stale and the first
/// frame after re-entering review errs to the no-scroll row instead of a dead
/// `↑↓ scroll`.
#[test]
fn the_env_collision_review_never_shows_a_stale_scroll_after_a_resize() {
    use crate::tui::app::EnvCollisionForm;
    use ratatui::crossterm::event::KeyCode;
    use std::cell::Cell;
    let _home = crate::testutil::HomeSandbox::new();

    let form = |reviewing_overwrite: bool| {
        Modal::EnvCollision(EnvCollisionForm {
            profile: "a".into(),
            key: "K".into(),
            reason: "set by the base url field ".repeat(80),
            existing_idx: None,
            cursor: 0,
            scroll: Cell::new(0),
            reviewing_overwrite,
            confirm_overwrite: false,
            max_scroll: Cell::new(0),
            max_scroll_stamp: Cell::new(ScrollStamp::default()),
            viewport: Cell::new(0),
            overwrite_needs_review: Cell::new(true),
        })
    };
    let footer = |term: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        crate::testutil::buffer_rows(term.backend().buffer())
            .last()
            .expect("the footer row")
            .trim()
            .to_string()
    };

    // One probe per entry route into the review stage. The handler route
    // re-enters via ↵ on the overwrite option, which the list draw only offers
    // while the list overflows, so it resizes to 80x30 (list still overflows);
    // the direct field flip resizes to 200x100, where the list fits and the
    // flip is the only way back in.
    struct ReentryCase {
        name: &'static str,
        w: u16,
        h: u16,
        settled: &'static str,
        enter: fn(&mut App),
    }
    let cases = [
        ReentryCase {
            name: "the handler's overwrite activation",
            w: 80,
            h: 30,
            settled: "←→ pick   ↵ choose   ↑↓ scroll   q back",
            enter: |app| {
                crate::tui::app::handle_key(app, crate::testutil::key(KeyCode::Enter));
            },
        },
        ReentryCase {
            name: "a direct field flip",
            w: 200,
            h: 100,
            settled: "←→ pick   ↵ choose   q back",
            enter: |app| {
                if let Some(Modal::EnvCollision(s)) = app.modals.last_mut() {
                    s.reviewing_overwrite = true;
                }
            },
        },
    ];

    for ReentryCase {
        name,
        w,
        h,
        settled,
        enter,
    } in cases
    {
        // Review at 80x24: the long consequence overflows, so frame two names
        // the scroll — the bound is real and published there.
        let mut app = bare_app();
        app.modals.push(form(true));
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
        assert_eq!(
            footer(&term),
            "←→ pick   ↵ choose   ↑↓ scroll   q back",
            "{name}: the review stage overflows at 80x24 on frame two"
        );

        // Back to the list stage, resize, and let the list frames render.
        if let Some(Modal::EnvCollision(s)) = app.modals.last_mut() {
            s.reviewing_overwrite = false;
        }
        term.backend_mut().resize(w, h);
        term.autoresize().unwrap();
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();

        // Re-enter review: the first frame must drop the stale `↑↓ scroll`,
        // and the second settles on the row for the resized body.
        enter(&mut app);
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
        assert_eq!(
            footer(&term),
            "←→ pick   ↵ choose   q back",
            "{name}: the first frame after re-entry shows no stale ↑↓ scroll"
        );
        term.draw(|f| crate::tui::render::draw(f, &app)).unwrap();
        assert_eq!(
            footer(&term),
            settled,
            "{name}: the second frame settles on the row for {w}x{h}"
        );
    }
}

/// Behind a login in flight a long profile name never crowds out the modal's
/// essential groups: the name truncates with a trailing `…` so `esc back` (the
/// name prompt's way out) survives at 45 columns.
#[test]
fn the_login_footer_truncates_a_long_profile_name() {
    use crate::tui::app::{InputState, LoginMethod, LoginStage, NamePromptAction, NamePromptForm};
    let _home = crate::testutil::HomeSandbox::new();
    let mut app = bare_app();
    app.login = Some(LoginSession {
        name: "a-very-long-profile".into(),
        is_new: false,
        generation: 0,
        url: None,
        stage: LoginStage::WaitingBrowser,
        method: LoginMethod::Browser,
        paste: None,
        paste_field: None,
    });
    app.modals.push(Modal::NamePrompt(NamePromptForm {
        input: InputState::new(""),
        action: NamePromptAction::DuplicateProfile("src".into()),
    }));
    let footer = settled_footer(&app, 45, 24);
    assert_eq!(
        footer, "⠋ logging in 'a-very-long-prof…'   esc back",
        "the long name truncates so the name prompt's esc back survives"
    );
}

/// A live footer alert replaces the hint bar in the one footer row, inset one
/// cell then `! `, and the hints return once it clears. This is a
/// characterisation: the behaviour is pre-existing (green on the pre-change
/// code), so it has no red-first run; the returned rows are pinned by equality
/// (the leading inset included).
#[test]
fn the_footer_alert_replaces_the_hints_in_place() {
    let _home = crate::testutil::HomeSandbox::new();
    let mut app = bare_app();
    app.tab = Tab::Usage;
    let at_rest = settled_footer(&app, 80, 24);

    app.footer_alert = Some(FooterAlert::Warn("press q again to quit".into()));
    for (w, h) in [(80u16, 24u16), (45u16, 24u16)] {
        let rows = settled_rows(&app, w, h);
        // The untrimmed row keeps the 1-cell inset before `! `; only the
        // trailing fill trims.
        let footer = rows.last().expect("the footer row").trim_end();
        assert_eq!(
            footer, " ! press q again to quit",
            "the alert replaces the hints with its 1-cell inset at {w}x{h}, got {footer:?}"
        );
    }

    app.footer_alert = None;
    let footer = settled_footer(&app, 80, 24);
    assert_eq!(
        footer, at_rest,
        "the hints return when the alert clears, got {footer:?}"
    );
}

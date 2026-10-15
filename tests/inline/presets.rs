//! Preset store: the shipped table, the on-disk round-trip, and the two
//! refusals that keep a built-in's slot the binary's alone.

use super::{
    Binds, Builtin, RowMove, base_url_matches, builtin_binds_digest, delete_preset,
    digest_of_builtins, group_offers, is_builtin, list_presets, load_preset, preset_exists,
    presets_dir, rebind_offers, save_preset,
};
use crate::profile::{AppConfig, AppState, ModelSettings, Profile};

fn models(default: &str) -> ModelSettings {
    ModelSettings {
        default: Some(default.to_string()),
        ..ModelSettings::default()
    }
}

/// The shipped names in menu order, so a table change lands in one place.
const BUILTIN_NAMES: [&str; 8] = [
    "DeepSeek",
    "Z.ai",
    "OpenRouter",
    "MiniMax",
    "Qwen-TokenPlan-Intl",
    "Qwen-TokenPlan-CN",
    "Qwen-CodingPlan-Intl",
    "Qwen-CodingPlan-CN",
];

fn names(presets: &[super::Preset]) -> Vec<&str> {
    presets.iter().map(|p| p.name.as_str()).collect()
}

/// A built-in's whole model block: `default` is the `opus` alias, so the main
/// model follows the opus bind; `subagent` stays unset, so each subagent's own
/// tier pick lands on that tier's bind.
fn binds(opus: &str, sonnet: &str, haiku: &str, fable: &str) -> ModelSettings {
    ModelSettings {
        default: Some("opus".to_string()),
        opus: Some(opus.to_string()),
        sonnet: Some(sonnet.to_string()),
        haiku: Some(haiku.to_string()),
        fable: Some(fable.to_string()),
        subagent: None,
    }
}

/// With nothing on disk the list is exactly the built-ins, in menu order, each
/// carrying its endpoint and a bind on every tier.
#[test]
fn the_builtins_ship_with_their_endpoint_and_every_tier_bound() {
    let _home = crate::testutil::HomeSandbox::new();

    let listed = list_presets();
    assert_eq!(names(&listed), BUILTIN_NAMES);
    assert!(listed.iter().all(|p| p.builtin));

    for (preset, base_url, models) in [
        (
            &listed[0],
            "https://api.deepseek.com/anthropic",
            binds(
                "deepseek-v4-pro[1m]",
                "deepseek-flash[1m]",
                "deepseek-flash[1m]",
                "deepseek-v4-pro[1m]",
            ),
        ),
        (
            &listed[1],
            "https://api.z.ai/api/anthropic",
            binds(
                "glm-5.3[1m]",
                "glm-5.3-flash[1m]",
                "glm-5.3-flash[1m]",
                "glm-5.3[1m]",
            ),
        ),
        (
            &listed[2],
            "https://openrouter.ai/api",
            binds(
                "~anthropic/claude-opus-latest[1m]",
                "~anthropic/claude-sonnet-latest[1m]",
                "~anthropic/claude-haiku-latest",
                "~anthropic/claude-fable-latest[1m]",
            ),
        ),
        (
            &listed[3],
            "https://api.minimax.io/anthropic",
            binds(
                "MiniMax-M3[1m]",
                "MiniMax-M3.1-Flash-Preview[1m]",
                "MiniMax-M3.1-Flash-Preview[1m]",
                "MiniMax-M3[1m]",
            ),
        ),
    ] {
        assert_eq!(
            preset.base_url.as_deref(),
            Some(base_url),
            "{}",
            preset.name
        );
        assert_eq!(preset.models, models, "{}", preset.name);
    }
}

/// Alibaba's endpoints answer `400 "Model not exist."` for any Claude model id,
/// so the alias an uncovered tier resolves to is a hard failure there rather
/// than a degraded route. Every alibaba preset therefore writes its one model to
/// `default`, each alias AND the subagent row, where the presets above leave the
/// subagent row unset.
#[test]
fn an_alibaba_preset_pins_every_alias_and_the_subagent_row() {
    let _home = crate::testutil::HomeSandbox::new();

    for (name, base_url, model) in [
        (
            "Qwen-TokenPlan-Intl",
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic",
            "qwen3.8-max",
        ),
        (
            "Qwen-TokenPlan-CN",
            "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
            "qwen3.8-max",
        ),
        (
            "Qwen-CodingPlan-Intl",
            "https://coding-intl.dashscope.aliyuncs.com/apps/anthropic",
            "qwen3-coder-plus",
        ),
        (
            "Qwen-CodingPlan-CN",
            "https://coding.dashscope.aliyuncs.com/apps/anthropic",
            "qwen3-coder-plus",
        ),
    ] {
        let p = load_preset(name).unwrap_or_else(|| panic!("{name} ships in the binary"));
        assert_eq!(p.base_url.as_deref(), Some(base_url), "{name} endpoint");
        let m = &p.models;
        for (field, got) in [
            ("default", &m.default),
            ("opus", &m.opus),
            ("sonnet", &m.sonnet),
            ("haiku", &m.haiku),
            ("fable", &m.fable),
            ("subagent", &m.subagent),
        ] {
            assert_eq!(
                got.as_deref(),
                Some(model),
                "{name} leaves `{field}` free to resolve to a Claude id the endpoint rejects",
            );
        }
    }
}

#[test]
fn a_saved_preset_round_trips_and_joins_the_list_after_the_builtins() {
    let _home = crate::testutil::HomeSandbox::new();

    let mut m = models("my-model");
    m.fable = Some("claude-fable-5".to_string());
    save_preset(
        "mine",
        &Some("https://api.example/anthropic".to_string()),
        &m,
    )
    .expect("save");

    let loaded = load_preset("mine").expect("the saved preset loads back");
    assert_eq!(loaded.name, "mine");
    assert_eq!(
        loaded.base_url.as_deref(),
        Some("https://api.example/anthropic")
    );
    assert_eq!(loaded.models, m, "every tier survives the json round-trip");
    assert!(!loaded.builtin);

    assert_eq!(
        names(&list_presets()),
        [BUILTIN_NAMES.as_slice(), ["mine"].as_slice()].concat(),
        "built-ins keep the head of the list",
    );
}

/// The whole preset tree lives under `~/.clauth`, so it is bound by the
/// owner-only invariant — dir 0o700, file 0o600, both
/// born that way rather than repaired later.
#[cfg(unix)]
#[test]
fn preset_file_and_dir_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let _home = crate::testutil::HomeSandbox::new();

    save_preset("mine", &None, &models("m")).expect("save");

    let dir = presets_dir().expect("presets dir");
    let dir_mode = std::fs::metadata(&dir)
        .expect("dir metadata")
        .permissions()
        .mode();
    assert_eq!(dir_mode & 0o777, 0o700, "presets dir is owner-only");

    let file_mode = std::fs::metadata(dir.join("mine.json"))
        .expect("file metadata")
        .permissions()
        .mode();
    assert_eq!(file_mode & 0o777, 0o600, "a preset file is owner-only");
}

/// A built-in's name is the binary's, whatever the spelling: a case-folding
/// filesystem would otherwise let `deepseek.json` take the slot on one host and
/// not another.
#[test]
fn a_builtin_name_refuses_both_writes_whatever_its_case() {
    let _home = crate::testutil::HomeSandbox::new();

    assert!(is_builtin("DeepSeek") && is_builtin("deepseek") && is_builtin("Z.AI"));

    let err = save_preset("deepseek", &None, &models("m")).expect_err("built-in name refuses");
    assert!(
        err.to_string().contains("built-in"),
        "the refusal names why: {err}",
    );
    assert!(
        delete_preset("DeepSeek").is_err(),
        "a built-in cannot be deleted either",
    );
    assert!(!preset_exists("deepseek"), "the refused save wrote no file",);
}

/// The name is the file NAME, so anything that could climb out of the presets
/// dir is refused before it ever reaches a `join`.
#[test]
fn a_name_that_is_not_a_bare_filename_is_refused() {
    let _home = crate::testutil::HomeSandbox::new();

    for bad in ["../escape", "a/b", "", ".hidden"] {
        assert!(
            save_preset(bad, &None, &models("m")).is_err(),
            "'{bad}' must not reach a path join",
        );
    }
}

#[test]
fn delete_drops_the_file_and_then_reports_the_miss() {
    let _home = crate::testutil::HomeSandbox::new();

    save_preset("mine", &None, &models("m")).expect("save");
    assert!(preset_exists("mine"));
    delete_preset("mine").expect("delete");
    assert!(!preset_exists("mine"));
    assert!(load_preset("mine").is_none());
    assert!(
        delete_preset("mine").is_err(),
        "deleting what is already gone is an error, not a silent success",
    );
}

/// One unparseable file must not hide the rest of the store from the picker.
#[test]
fn an_unparseable_preset_is_skipped_not_fatal() {
    let _home = crate::testutil::HomeSandbox::new();

    save_preset("good", &None, &models("m")).expect("save");
    let dir = presets_dir().expect("presets dir");
    std::fs::write(dir.join("broken.json"), "{ not json").expect("write broken preset");

    assert_eq!(
        names(&list_presets()),
        [BUILTIN_NAMES.as_slice(), ["good"].as_slice()].concat(),
    );
}

/// `save_preset` refuses a built-in's name, so a file in that slot can only be
/// hand-written — and it must not shadow the binary's copy. Both surfaces read
/// the built-in: the list shows one `DeepSeek` carrying the shipped endpoint,
/// and a load by that name never reaches the file.
#[test]
fn a_hand_written_file_cannot_shadow_a_builtins_slot() {
    let _home = crate::testutil::HomeSandbox::new();

    save_preset("anchor", &None, &models("m")).expect("seed the dir");
    let dir = presets_dir().expect("presets dir");
    std::fs::write(
        dir.join("DeepSeek.json"),
        r#"{"base_url":"https://evil.test","models":{"default":"pwned"}}"#,
    )
    .expect("hand-write into the built-in's slot");

    let listed = list_presets();
    assert_eq!(
        listed.iter().filter(|p| p.name == "DeepSeek").count(),
        1,
        "the shadow file does not add a second entry",
    );
    let deepseek = listed
        .iter()
        .find(|p| p.name == "DeepSeek")
        .expect("built-in listed");
    assert_eq!(
        deepseek.base_url.as_deref(),
        Some("https://api.deepseek.com/anthropic"),
        "the list reads the binary's copy, not the file",
    );
    assert_eq!(
        load_preset("DeepSeek").expect("loads").base_url.as_deref(),
        Some("https://api.deepseek.com/anthropic"),
        "so does a load by name",
    );
}

// ── Re-bind offer engine ──────────────────────────────────────────────────────

fn config_with(profiles: Vec<Profile>) -> AppConfig {
    AppConfig {
        state: AppState {
            profiles: profiles.iter().map(|p| p.name.clone()).collect(),
            ..AppState::default()
        },
        profiles,
    }
}

fn account(name: &str, base_url: &str, models: ModelSettings) -> Profile {
    let mut p = Profile::new(name.to_string(), Some(base_url.to_string()), None);
    p.models = models;
    p
}

fn move_(row: &'static str, old: Option<&str>, new: Option<&str>) -> RowMove {
    RowMove {
        row,
        old: old.map(str::to_string),
        new: new.map(str::to_string),
    }
}

/// The `(unset)` arm for a row with no stored id: `default` on a matching
/// account becomes the opus alias, and the four empty tier rows take the new
/// tier ids; `subagent` (whose bind is unset) is no change.
#[test]
fn a_former_id_and_every_empty_row_are_offered_the_new_binds() {
    let m = ModelSettings {
        default: Some("deepseek-chat".to_string()),
        ..ModelSettings::default()
    };
    let config = config_with(vec![account(
        "DS1",
        "https://api.deepseek.com/anthropic",
        m,
    )]);

    let offers = rebind_offers(&config);
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0].account, "DS1");
    assert_eq!(
        offers[0].rows,
        [
            move_("default", Some("deepseek-chat"), Some("opus")),
            move_("opus", None, Some("deepseek-v4-pro[1m]")),
            move_("sonnet", None, Some("deepseek-flash[1m]")),
            move_("haiku", None, Some("deepseek-flash[1m]")),
            move_("fable", None, Some("deepseek-v4-pro[1m]")),
        ],
    );
}

/// A fully empty matching account is offered every tier the bind fills, and
/// nothing else.
#[test]
fn a_fully_empty_account_is_offered_every_filled_row() {
    let config = config_with(vec![account(
        "fresh",
        "https://api.z.ai/api/anthropic",
        ModelSettings::default(),
    )]);

    let offers = rebind_offers(&config);
    assert_eq!(offers.len(), 1);
    assert_eq!(
        offers[0].rows,
        [
            move_("default", None, Some("opus")),
            move_("opus", None, Some("glm-5.3[1m]")),
            move_("sonnet", None, Some("glm-5.3-flash[1m]")),
            move_("haiku", None, Some("glm-5.3-flash[1m]")),
            move_("fable", None, Some("glm-5.3[1m]")),
        ],
    );
}

/// A custom id on a row is never touched, whatever it is; only the empty rows
/// are offered.
#[test]
fn a_custom_id_row_is_untouched() {
    let m = ModelSettings {
        default: Some("nvidia/nemotron-3-ultra-550b-a55b:free[1m]".to_string()),
        ..ModelSettings::default()
    };
    let config = config_with(vec![account("openrouter", "https://openrouter.ai/api", m)]);

    let offers = rebind_offers(&config);
    assert_eq!(offers.len(), 1);
    assert_eq!(
        offers[0].rows,
        [
            move_("opus", None, Some("~anthropic/claude-opus-latest[1m]")),
            move_("sonnet", None, Some("~anthropic/claude-sonnet-latest[1m]")),
            move_("haiku", None, Some("~anthropic/claude-haiku-latest")),
            move_("fable", None, Some("~anthropic/claude-fable-latest[1m]")),
        ],
        "the custom default row is not among the moves",
    );
}

/// A row already equal to its new value is no change — nothing to offer.
#[test]
fn a_row_already_on_its_new_value_is_no_change() {
    let m = ModelSettings {
        default: Some("opus".to_string()),
        opus: Some("deepseek-v4-pro[1m]".to_string()),
        sonnet: Some("deepseek-flash[1m]".to_string()),
        haiku: Some("deepseek-flash[1m]".to_string()),
        fable: Some("deepseek-v4-pro[1m]".to_string()),
        ..ModelSettings::default()
    };
    let config = config_with(vec![account(
        "current",
        "https://api.deepseek.com/anthropic",
        m,
    )]);

    assert!(rebind_offers(&config).is_empty());
}

/// A former id on a row the bind leaves unset is offered `(unset)`: the exact
/// string match is per-row, so `deepseek-chat` on the `subagent` row moves.
/// The whole row vec is pinned, so an extra move reds.
#[test]
fn a_former_id_on_a_row_the_bind_leaves_unset_is_offered_unset() {
    let m = ModelSettings {
        subagent: Some("deepseek-chat".to_string()),
        ..ModelSettings::default()
    };
    let config = config_with(vec![account(
        "DS1",
        "https://api.deepseek.com/anthropic",
        m,
    )]);

    let offers = rebind_offers(&config);
    assert_eq!(offers.len(), 1);
    assert_eq!(
        offers[0].rows,
        [
            move_("default", None, Some("opus")),
            move_("opus", None, Some("deepseek-v4-pro[1m]")),
            move_("sonnet", None, Some("deepseek-flash[1m]")),
            move_("haiku", None, Some("deepseek-flash[1m]")),
            move_("fable", None, Some("deepseek-v4-pro[1m]")),
            move_("subagent", Some("deepseek-chat"), None),
        ],
    );
}

/// An endpoint no built-in ships is never offered.
#[test]
fn an_account_on_another_endpoint_is_untouched() {
    let config = config_with(vec![account(
        "anthropic",
        "https://api.anthropic.com",
        ModelSettings::default(),
    )]);
    assert!(rebind_offers(&config).is_empty());
}

/// The endpoint predicate is the shared host matcher: case-insensitive on
/// scheme + host, forgiving a trailing slash, and a different provider's
/// endpoint never matches.
#[test]
fn a_trailing_slash_base_url_matches() {
    assert!(base_url_matches(
        "https://api.deepseek.com/anthropic/",
        "https://api.deepseek.com/anthropic"
    ));
    assert!(
        base_url_matches(
            "https://API.deepseek.com/anthropic",
            "https://api.deepseek.com/anthropic"
        ),
        "a mixed-case host matches"
    );
    assert!(
        !base_url_matches(
            "https://api.anthropic.com",
            "https://api.deepseek.com/anthropic"
        ),
        "another provider never matches"
    );
    assert!(
        !base_url_matches(
            "https://api.deepseek.com",
            "https://api.deepseek.com/anthropic"
        ),
        "a host-only account is a different endpoint"
    );

    let config = config_with(vec![account(
        "DS1",
        "https://api.deepseek.com/anthropic/",
        ModelSettings::default(),
    )]);
    assert_eq!(rebind_offers(&config).len(), 1);
}

/// Accounts whose change sets are byte-identical share a block; a differing
/// one splits into its own, preserving first-seen order.
#[test]
fn identical_change_sets_share_a_block_and_a_differing_one_splits() {
    let ds_a = account(
        "a",
        "https://api.deepseek.com/anthropic",
        ModelSettings::default(),
    );
    let ds_b = account(
        "b",
        "https://api.deepseek.com/anthropic",
        ModelSettings::default(),
    );
    let z = account(
        "z",
        "https://api.z.ai/api/anthropic",
        ModelSettings::default(),
    );
    let blocks = group_offers(rebind_offers(&config_with(vec![ds_a, ds_b, z])));

    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0].accounts, ["a", "b"]);
    assert_eq!(blocks[1].accounts, ["z"]);
    assert_ne!(blocks[0].rows, blocks[1].rows);
}

/// Two literal tables hash differently, and the shipped table's digest is a
/// stable 64-hex pin — never recomputed from the code under test.
#[test]
fn the_digest_changes_when_a_bind_changes() {
    let one = digest_of_builtins(&[Builtin {
        name: "X",
        base_url: "https://a",
        former_ids: &[],
        binds: Binds::Every("m1"),
    }]);
    let two = digest_of_builtins(&[Builtin {
        name: "X",
        base_url: "https://a",
        former_ids: &[],
        binds: Binds::Every("m2"),
    }]);
    assert_ne!(one, two, "a bind change must change the digest");
    assert_eq!(one.len(), 64);
}

/// A release that only adds a former id must reopen the confirm for an
/// account that dismissed it, so `former_ids` feeds the digest too.
#[test]
fn the_digest_changes_when_only_a_former_id_changes() {
    let with = |former_ids: &'static [&'static str]| {
        digest_of_builtins(&[Builtin {
            name: "X",
            base_url: "https://a",
            former_ids,
            binds: Binds::Tiers {
                opus: "big",
                sonnet: "fast",
                haiku: "fast",
                fable: "big",
            },
        }])
    };
    assert_ne!(
        with(&[]),
        with(&["old"]),
        "adding a former id must change the digest"
    );
    assert_ne!(
        with(&["old"]),
        with(&["old", "older"]),
        "a second former id must change the digest"
    );
    assert_ne!(
        with(&["old"]),
        with(&["new"]),
        "replacing a former id must change the digest"
    );
    assert_ne!(
        with(&["ab", "c"]),
        with(&["a", "bc"]),
        "moving an id boundary must change the digest"
    );
}

#[test]
fn the_shipped_binds_digest_is_a_stable_literal() {
    assert_eq!(
        builtin_binds_digest(),
        "a4d733b2c7d56df95eb0b28c09bb944be7504fb274f86357295e26a1d53609c0"
    );
}

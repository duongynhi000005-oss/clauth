//! Cross-profile `.claude.json` synchronizer.
//!
//! Claude Code keeps one large config file — `~/.claude.json` for normal use —
//! holding user-global state (`projects`, `mcpServers`, `tips`, `userID`)
//! alongside an account-specific `oauthAccount` block and a few billing/usage
//! caches. `clauth start <profile>` runs Claude Code against a per-profile
//! runtime tree with its OWN `.claude.json`, because a single shared file leaks
//! one account's identity into another: Claude Code trusts the cached
//! `oauthAccount` and does not re-derive it from the loaded token on a normal
//! startup (its bootstrap merge keeps the cached identity when the server
//! reports a different account).
//!
//! This module keeps every clauth-managed `.claude.json` (the global file plus
//! each profile runtime's copy) in sync EXCEPT for [`PER_PROFILE_FIELDS`], which
//! each file keeps as its own. Sync is "latest write wins" at file granularity:
//! each tick the newest parseable file is the source for the shared fields,
//! overlaid onto every other file while preserving that file's per-profile
//! fields. Atomic writes plus write-only-on-change make it convergent and safe
//! against Claude Code's concurrent in-place writes — a file caught mid-write
//! fails to parse and is simply skipped until the next tick. The merge itself
//! lives in [`crate::jsonsync`], shared with the `settings.json` reconciler.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::SystemTime;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::jsonsync::{KeyPath, KeyRule};
use crate::profile::{AccountId, atomic_write, home_dir};

/// Account-specific keys that must never propagate between profiles.
const PER_PROFILE_FIELDS: &[&str] = &[
    "oauthAccount",
    "overageCreditGrantCache",
    "passesEligibilityCache",
    "passesLastSeenRemaining",
    "cachedExtraUsageDisabledReason",
    // The usage cache stamps `accountUuid` alone, the artifact roster also
    // stamps `organizationUuid`. Two organizations under one account share an
    // `accountUuid`, so syncing `cachedUsageUtilization` made every profile
    // read whichever copy was written last as its own 5h/7d usage.
    "cachedUsageUtilization",
    "cachedArtifactRoster",
    "artifactRosterDenied",
    // Org-scoped config and feature-flag caches Claude Code keeps beside them.
    "groveConfigCache",
    "promoStartupStatusCache",
    "metricsStatusCacheByPrincipal",
    "metricsStatusCache",
    "clientDataCacheSlots",
    "clientDataCache",
    "cachedGrowthBookFeatures",
    "cachedGrowthBookFeaturesAt",
    "cachedExperimentFeatures",
    "cachedExperimentData",
    // Account/org-scoped model caches Claude Code writes into `.claude.json`.
    // Syncing them would bleed one account's model access, org default, and
    // per-model cost/option tables into every other account. Each profile
    // re-fetches its own on first boot, so per-profile is lossless.
    "orgModelDefaultCache",
    "lastSeenOrgDefaultUpdatedAt",
    "modelAccessCache",
    "additionalModelCostsCache",
    "additionalModelOptionsCache",
    "additionalModelOptionsAnsweredAt",
    // The rest of what Claude Code clears on OAuth logout (verified on
    // 2.1.287): the GitHub connection status stamps both uuids, the
    // auto-compact windows come with the bootstrap's model caches, and a shared
    // prefetch stamp would make one profile skip its own startup prefetches.
    "githubWebConnectionStatusCache",
    "autoCompactWindowsCache",
    "startupPrefetchedAt",
    // `/login`-managed API key and the per-key approval ledger. `primaryApiKey`
    // is a RAW KEY: Claude Code writes it into this file (verified on 2.1.215 —
    // the same accessor that reads `numStartups` and `oauthAccount`) and reads
    // it back as an auth source labelled "/login managed key". Propagating it
    // would hand one account's key to every other profile — the `.claude.json`
    // twin of the `apiKeyHelper` leak the settings syncer blocks.
    // `customApiKeyResponses` holds the approved/rejected hashes of those keys,
    // so it is scoped to the same account.
    "primaryApiKey",
    "customApiKeyResponses",
];

/// Newest mtime from the last [`sync_once`] that did work; the key for
/// [`crate::jsonsync::run_with_cache`]'s fast path.
static LAST_SYNCED: Mutex<Option<SystemTime>> = Mutex::new(None);

/// The last good credit-billed family set per `.claude.json` path, so a file
/// CC is caught mid-writing (unparseable) keeps the previous successful parse
/// for that path — the sync engine's own "skip until the next tick" torn-read
/// rule. A `Mutex` leaf held for one map operation, never across the file read.
static CREDIT_BILLED_FAMILIES: LazyLock<Mutex<HashMap<PathBuf, Vec<String>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Every `.claude.json` clauth reconciles: the global file plus each SHARED
/// per-session runtime copy, via [`crate::jsonsync::runtime_files_under`].
fn known_paths() -> Result<Vec<PathBuf>> {
    Ok(crate::jsonsync::runtime_files_under(
        &home_dir()?,
        ".claude.json",
    ))
}

/// Reconcile all known `.claude.json` files once, behind the `LAST_SYNCED`
/// mtime fast path in [`crate::jsonsync::run_with_cache`].
pub(crate) fn sync_once() -> Result<()> {
    let paths = known_paths()?;
    crate::jsonsync::run_with_cache(&LAST_SYNCED, &paths, || {
        sync_paths(&paths)?;
        Ok(true)
    })
}

/// Reconcile the given `.claude.json` copies. Every field is shared except
/// [`PER_PROFILE_FIELDS`], which is a flat top-level set — no `env`-style nested
/// rule, unlike the `settings.json` reconciler.
fn sync_paths(paths: &[PathBuf]) -> Result<()> {
    let operator_file = home_dir().ok().map(|h| h.join(".claude.json"));
    crate::jsonsync::sync_paths(paths, operator_file.as_deref(), |path| match path {
        KeyPath::Top(key) if PER_PROFILE_FIELDS.contains(&key) => KeyRule::PerProfile,
        _ => KeyRule::Shared,
    })
}

/// CC's cached account uuid from `~/.claude.json`'s `oauthAccount.accountUuid`,
/// or `None` when the file, block, or value is absent/blank/unparseable.
/// Read-then-parse with the same discipline as [`strip_home_oauth_account`]: a
/// missing file is never created and a file caught mid-write by CC is left
/// untouched rather than clobbered. CC trusts this cached block and does not
/// re-derive it from a swapped credentials file — so a hit is "CC's last booted
/// identity", not fresh proof of the live token's account.
pub(crate) fn home_oauth_account_uuid() -> Option<AccountId> {
    let path = home_dir().ok()?.join(".claude.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return None; // missing — never create
    };
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(&bytes) else {
        return None; // unparseable (CC mid-write) — never clobber
    };
    obj.get("oauthAccount")
        .and_then(|a| a.get("accountUuid"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(AccountId::from)
}

/// Delete the stale `oauthAccount` identity block from the home
/// `~/.claude.json` on profile switch (issue #17). Claude Code trusts a
/// cached identity block and does not re-derive it from a relinked
/// credentials file on a normal startup; dropping the block instead lets it
/// self-heal — probed on CC 2.1.201: an
/// absent block re-derives the correct identity from the token within
/// seconds, a present-but-wrong one never self-corrects.
///
/// Read-then-parse first: a missing file is left uncreated, and a file that
/// fails to parse (CC mid-write) is left untouched rather than clobbered. A
/// write only happens when the key is actually present — an already-clean
/// file is never touched, because [`sync_once`] picks the newest-mtime member
/// as the sync winner; a pointless touch here would make home win the next
/// tick and stomp a runtime copy's own fields.
pub(crate) fn strip_home_oauth_account() -> Result<()> {
    let path = home_dir()?.join(".claude.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(()); // missing — never create
    };
    let Ok(Value::Object(mut obj)) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(()); // unparseable (CC mid-write) — never clobber
    };
    if obj.remove("oauthAccount").is_none() {
        return Ok(()); // already clean — avoid a pointless mtime bump
    }
    let bytes = serde_json::to_vec_pretty(&Value::Object(obj))
        .context("failed to serialize .claude.json after stripping oauthAccount")?;
    atomic_write(&path, &bytes).with_context(|| format!("failed to write {}", path.display()))
}

/// The model families Claude Code bills against usage credits, derived from
/// the cached `tengu_calm_seal` flag: `fable` always, then every family the
/// flag's array lists, trimmed + lowercased, empty and non-string entries
/// skipped, deduped, order-stable. A non-array flag adds nothing.
pub(crate) fn credit_billed_families_from(flag: Option<&Value>) -> Vec<String> {
    let mut families = vec!["fable".to_string()];
    let Some(Value::Array(items)) = flag else {
        return families;
    };
    for item in items {
        let Some(s) = item.as_str() else {
            continue;
        };
        let s = s.trim().to_lowercase();
        if s.is_empty() || families.contains(&s) {
            continue;
        }
        families.push(s);
    }
    families
}

/// The credit-billed families off `home_dir()/.claude.json`'s
/// `cachedGrowthBookFeatures.tengu_calm_seal`, read with the same discipline
/// as [`home_oauth_account_uuid`]: a missing file is never created and reads
/// no flag. Reads ONLY that key — `oauthAccount` beside it is never logged,
/// printed or copied. A fresh read per call (the file is ~100 KB), so a flag
/// CC just rewrote is seen on the next call; a file caught mid-write
/// (unparseable) answers with this path's last good parse instead of flapping,
/// and a path never parsed yet answers `fable` alone. A read that fails for
/// any reason but absence (a directory, EACCES, ...) is no evidence the flag
/// changed, so it answers like the torn parse; absence itself answers `fable`
/// alone and never creates the file.
pub(crate) fn credit_billed_families() -> Vec<String> {
    let Some(path) = home_dir().ok().map(|h| h.join(".claude.json")) else {
        return credit_billed_families_from(None);
    };
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(obj)) => {
                let flag = obj
                    .get("cachedGrowthBookFeatures")
                    .and_then(|f| f.get("tengu_calm_seal"));
                let families = credit_billed_families_from(flag);
                CREDIT_BILLED_FAMILIES
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(path, families.clone());
                families
            }
            // Unparseable (CC mid-write): keep the last good parse for this
            // path, exactly as the sync engine skips the torn file until the
            // next tick.
            _ => last_good_families(&path),
        },
        // Absence is a real answer: no file, no flag, never create it.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => credit_billed_families_from(None),
        // Any other read failure is no evidence the flag changed, so it answers
        // exactly like the torn parse: the last good set for this path.
        Err(_) => last_good_families(&path),
    }
}

/// The last good credit-billed family set for `path`, else `fable` alone when
/// this path has never parsed yet. One lock, one map lookup — a leaf.
fn last_good_families(path: &Path) -> Vec<String> {
    CREDIT_BILLED_FAMILIES
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(path)
        .cloned()
        .unwrap_or_else(|| credit_billed_families_from(None))
}

#[cfg(test)]
#[path = "../tests/inline/claude_json.rs"]
mod tests;

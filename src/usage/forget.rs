//! Credential-identity reads and the two-layer forget a credential change runs.
//!
//! Each change site compares the STORED identity of each half before and after
//! the change, drops that half's disk caches inside its state-flock hold
//! (before the identity writes), and records the half for `forget_memory`,
//! which a process that holds reading stores (the TUI and the daemon) applies
//! right after its config guard drops (the reading stores rank BELOW `Config`,
//! so the clears cannot run while it is held). A process with no stores — the
//! CLI, the MCP server — records nothing, so its queue cannot grow. The
//! per-process epoch gates the startup seeds so a seed racing a change writes
//! nothing once the epoch moved.
//!
//! Identity is always read from the stored files through one raw-file function
//! per half — never from an in-memory `Profile`, which a caller may hold stale.
//! The "changed" answer is spelled once per half and applied at every site,
//! never a literal compare of two identity values.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use crate::lockorder::{RankedMutex, rank};
use crate::profile::{
    AccountId, ConsoleCredential, ProfileName, stored_access_token, stored_endpoint_identity,
};
use crate::profile_cache::{
    ACCOUNT_ID_CACHE_FILE, KICK_BLOCK_CACHE_FILE, PROFILE_FETCHED_CACHE_FILE,
    THIRD_PARTY_CACHE_FILE, USAGE_CACHE_FILE, load_profile_cache, remove_profile_cache,
};
use crate::providers::Provider;

use super::scheduler::{
    KickBlocks, LastFetchedAt, NextRefreshPerProfile, PollStreaks, StatusStore,
    SuppressedAuthExpiredStore, ThirdPartyBroken, ThirdPartyStatusStore, ThirdPartyStreaks,
    ThirdPartyUsageStore, UsageStore, WeeklyResetKicks, forget_codex_polled, seed_codex_name,
    seed_third_party_name, try_seed_cache,
};
use super::{AutoStartQueueState, FetchLeg, forget_profile_memo, now_ms};

/// One half of a profile's credential identity. `Throughput` has no in-memory
/// store of its own — `forget_memory` skips it — but rides `forget_disk`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Half {
    OAuth,
    ThirdParty,
    Codex,
    Throughput,
}

/// The halves a change site decided to forget.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ForgetPlan {
    pub(crate) oauth: bool,
    pub(crate) third_party: bool,
    pub(crate) codex: bool,
    pub(crate) throughput: bool,
}

impl ForgetPlan {
    /// The memory-relevant halves, in a fixed order (throughput has no memory).
    fn memory_halves(self) -> impl Iterator<Item = Half> {
        [Half::OAuth, Half::ThirdParty, Half::Codex]
            .into_iter()
            .filter(move |h| match h {
                Half::OAuth => self.oauth,
                Half::ThirdParty => self.third_party,
                Half::Codex => self.codex,
                Half::Throughput => false,
            })
    }

    fn all_halves(self) -> impl Iterator<Item = Half> {
        [Half::OAuth, Half::ThirdParty, Half::Codex, Half::Throughput]
            .into_iter()
            .filter(move |h| match h {
                Half::OAuth => self.oauth,
                Half::ThirdParty => self.third_party,
                Half::Codex => self.codex,
                Half::Throughput => self.throughput,
            })
    }
}

/// SHA-256 over length-delimited fields, head-truncated to a u64 — the same
/// explicit-encoding shape the third-party credential fingerprint uses, so no
/// two field splits can collide. A collision costs one profile one extra (or
/// one missed) forget, never a wrong identity on disk.
fn digest_fields(fields: &[&[u8]]) -> u64 {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    for field in fields {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field);
    }
    let digest = hasher.finalize();
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(head)
}

/// The stored-token digest the OAuth identity carries: SHA-256 of the access
/// token alone (the token is the only stored credential a `/profile` read is
/// evidence about).
fn stored_token_digest(access_token: &str) -> u64 {
    digest_fields(&[access_token.as_bytes()])
}

/// The OAuth half's identity: the anchor plus the stored-token digest. The
/// stored login is the dominant fact — a stale anchor file left behind by a
/// log out reads `Unanchored(None)`, never an anchor without a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OAuthIdentity {
    pub(crate) anchor: Option<AccountId>,
    pub(crate) stored: Option<u64>,
}

impl OAuthIdentity {
    /// The identity of the values a change site is about to write. `anchor` is
    /// the anchor the site will leave on disk (already accounting for the
    /// in-hold remove / re-seed); `stored_token` is the access token the new
    /// credentials file will hold.
    pub(crate) fn for_values(anchor: Option<&AccountId>, stored_token: Option<&str>) -> Self {
        match stored_token {
            Some(token) => Self {
                anchor: anchor.filter(|u| !u.trim().is_empty()).cloned(),
                stored: Some(stored_token_digest(token)),
            },
            // No stored login can carry an anchor: a stale anchor left by a log
            // out reads as "unanchored without a login", never as "anchored
            // without one", which the changed predicate has no arm for.
            None => Self {
                anchor: None,
                stored: None,
            },
        }
    }
}

/// Read `name`'s stored OAuth identity from the raw files: the anchor
/// (`account_id.json`) and the stored access token (`credentials.json`, through
/// [`stored_access_token`], which never touches `load_profile`).
pub(crate) fn read_oauth_identity(name: &ProfileName) -> OAuthIdentity {
    let stored = stored_access_token(name).map(|t| stored_token_digest(&t));
    let Some(stored) = stored else {
        return OAuthIdentity {
            anchor: None,
            stored: None,
        };
    };
    let anchor = load_profile_cache::<AccountId>(name, ACCOUNT_ID_CACHE_FILE)
        .filter(|u| !u.trim().is_empty());
    OAuthIdentity {
        anchor,
        stored: Some(stored),
    }
}

/// The OAuth changed predicate, applied to two stored identities.
///
/// Changed: the login appears or disappears; the anchor moves A→B; the anchor
/// is removed while a login stays (an Unproven install); an unanchored
/// profile's stored token changes. NOT changed: an anchor appearing over an
/// unchanged stored token (a backfill, an adopt, a proven install's seed), nor
/// a rotation on an anchored profile.
pub(crate) fn oauth_changed(before: &OAuthIdentity, after: &OAuthIdentity) -> bool {
    match (before.anchor.as_ref(), after.anchor.as_ref()) {
        (Some(a), Some(b)) => a != b,
        (Some(_), None) => true,
        (None, Some(_)) => before.stored != after.stored,
        (None, None) => before.stored != after.stored,
    }
}

/// The provider literal the digest uses, a literal per variant rather than the
/// user-facing display name (which may be reworded and would reset identities).
fn provider_bytes(provider: Provider) -> &'static [u8] {
    match provider {
        Provider::DeepSeek => b"deepseek",
        Provider::Zai => b"zai",
        Provider::Alibaba => b"alibaba",
        Provider::OpenRouter => b"openrouter",
        Provider::MiniMax => b"minimax",
    }
}

/// The third-party usage identity as a digest: the provider plus what its fetch
/// sends. Alibaba usage reads the console session alone (key-less console
/// profiles included, an inference-key edit keeps the console reading); a typed
/// key provider sends the api key; a generic provider sends the api key AND the
/// resolved base_url, so a key edit over the same base_url is a change. `None`
/// when the profile has no third-party credential at all (the same credential
/// test the fetch leg applies).
pub(crate) fn third_party_identity(
    provider: Option<Provider>,
    api_key: Option<&str>,
    console: Option<&ConsoleCredential>,
    base_url: Option<&str>,
) -> Option<u64> {
    // `third_party_credentialed` spelled off the raw fields: Alibaba is
    // fetchable even keyless; every other provider needs a non-empty key.
    let credentialed = match provider {
        Some(Provider::Alibaba) => true,
        _ => api_key.is_some_and(|k| !k.trim().is_empty()),
    };
    if !credentialed {
        return None;
    }
    match provider {
        Some(Provider::Alibaba) => match console {
            Some(c) => Some(digest_fields(&[
                b"alibaba",
                c.token.as_bytes(),
                c.site.as_str().as_bytes(),
                c.region.as_bytes(),
            ])),
            None => Some(digest_fields(&[b"alibaba", b"no-console"])),
        },
        Some(p) => Some(digest_fields(&[
            b"known",
            provider_bytes(p),
            api_key.unwrap_or_default().trim().as_bytes(),
        ])),
        None => Some(digest_fields(&[
            b"generic",
            api_key.unwrap_or_default().trim().as_bytes(),
            base_url.map(str::trim).unwrap_or_default().as_bytes(),
        ])),
    }
}

/// The codex half's identity: the `auth.json` account id, read guard-free.
pub(crate) fn read_codex_identity(name: &ProfileName) -> Option<String> {
    crate::codex_auth::read_store_auth(name.as_str())
        .and_then(|auth| auth.account_id().map(str::to_string))
}

/// The throughput identity: the account uuid for an anchored OAuth spawn, the
/// stored-token digest for an unanchored one, else a digest of (effective
/// base_url, key or env token) — what a delegate spawn actually sends. A hybrid
/// (OAuth + endpoint auth) runs delegates on the key/env, so its OAuth log out
/// keeps the throughput record.
pub(crate) fn throughput_identity(
    oauth: &OAuthIdentity,
    base_url: Option<&str>,
    api_key: Option<&str>,
    env_token: Option<&str>,
) -> Option<u64> {
    let endpoint_auth =
        api_key.is_some_and(|k| !k.trim().is_empty()) || env_token.is_some_and(|t| !t.is_empty());
    if endpoint_auth || base_url.is_some_and(|u| !u.trim().is_empty()) {
        let key_or_env = api_key
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .or_else(|| env_token.map(|t| t.trim()).filter(|t| !t.is_empty()))
            .unwrap_or_default();
        return Some(digest_fields(&[
            b"endpoint",
            base_url.map(str::trim).unwrap_or_default().as_bytes(),
            key_or_env.as_bytes(),
        ]));
    }
    match (&oauth.anchor, oauth.stored) {
        (Some(uuid), _) => Some(digest_fields(&[b"anchor", uuid.trim().as_bytes()])),
        (None, Some(token)) => Some(digest_fields(&[b"oauth-token", &token.to_le_bytes()])),
        (None, None) => None,
    }
}

/// Every stored identity a change site compares, read once before the hold. The
/// endpoint fields ride along so the site computes the AFTER throughput
/// identity (which mixes the OAuth identity with the endpoint the delegates run
/// on) from the same sources the BEFORE identity read.
#[derive(Debug, Clone)]
pub(crate) struct BeforeIdentities {
    pub(crate) oauth: OAuthIdentity,
    pub(crate) third_party: Option<u64>,
    pub(crate) throughput: Option<u64>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key: Option<String>,
    pub(crate) env_token: Option<String>,
    /// Whether `credentials.json` exists, read off the stored file — the
    /// credential-presence bit the throughput identity's effective-base_url
    /// guard reads. The AFTER computation uses this stored fact, never an
    /// in-memory `Profile` a caller may hold stale.
    pub(crate) has_credentials: bool,
}

/// Read `name`'s stored identities from the raw files — OAuth, third-party,
/// throughput, and the endpoint fields the throughput identity consumes.
pub(crate) fn read_before_identities(name: &ProfileName) -> BeforeIdentities {
    let oauth = read_oauth_identity(name);
    let endpoint = stored_endpoint_identity(name);
    let has_credentials = crate::profile::stored_credentials_file_exists(name);
    let (base_url, api_key, env_token) = match &endpoint {
        Some(e) => (
            e.base_url.clone(),
            e.api_key.clone(),
            crate::profile::inference_env_token(&e.env),
        ),
        None => (None, None, None),
    };
    let third_party = endpoint.as_ref().and_then(|e| {
        let provider = e.base_url.as_deref().and_then(Provider::from_base_url);
        third_party_identity(
            provider,
            e.api_key.as_deref(),
            e.console.as_ref(),
            e.base_url.as_deref(),
        )
    });
    let throughput = throughput_identity(
        &oauth,
        base_url.as_deref(),
        api_key.as_deref(),
        env_token.as_deref(),
    );
    BeforeIdentities {
        oauth,
        third_party,
        throughput,
        base_url,
        api_key,
        env_token,
        has_credentials,
    }
}

/// The throughput identity a change site is about to write: the (unchanged)
/// OAuth identity mixed with the endpoint fields being written. `base_url` is
/// the RAW managed field — the effective base_url is derived the same way
/// `load_profile` derives it (the OAuth-bearer leak guard drops a managed url
/// an OAuth profile carries no key or env token for), so the site and the
/// reload agree on what the delegates will actually run on.
pub(crate) fn throughput_for_endpoint(
    oauth: &OAuthIdentity,
    base_url: Option<&str>,
    has_credentials: bool,
    api_key: Option<&str>,
    env: &std::collections::BTreeMap<String, String>,
) -> Option<u64> {
    let effective = crate::profile::effective_base_url(
        base_url.map(str::to_string),
        has_credentials,
        api_key,
        env,
    );
    let env_token = crate::profile::inference_env_token(env);
    throughput_identity(oauth, effective.as_deref(), api_key, env_token.as_deref())
}

/// The per-process credential-forget epoch: monotonic per `(name, half)`, never
/// removed, so a delete-then-recreate cannot reuse a value and a stale in-flight
/// write can never match again. Ranked at [`rank::Epoch`] (160), outermost of
/// every reading store it gates.
/// The epoch map's shape, factored out so the ranked-mutex type stays readable.
type EpochMap = HashMap<(String, Half), u64>;

static EPOCH: LazyLock<RankedMutex<EpochMap, rank::Epoch>> =
    LazyLock::new(|| RankedMutex::new(HashMap::new()));

/// The current epoch for `(name, half)`. A seed reads this before its disk read
/// as its pre-read capture; the write-side compare runs under [`epoch_guard`].
pub(crate) fn epoch_value(name: &ProfileName, half: Half) -> u64 {
    EPOCH
        .lock()
        .ok()
        .and_then(|m| m.get(&(name.to_string(), half)).copied())
        .unwrap_or(0)
}

/// Acquire the epoch guard, held from a seed's compare through its store,
/// status and cadence writes so a forget cannot bump the epoch between the
/// check and the write. The store locks (ranks 200-350) sit above [`rank::Epoch`],
/// so holding it across them stays ascending.
pub(crate) fn epoch_guard() -> crate::lockorder::RankedGuard<'static, EpochMap> {
    EPOCH
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn bump_epoch(name: &ProfileName, half: Half) {
    if let Ok(mut m) = EPOCH.lock() {
        let e = m.entry((name.to_string(), half)).or_insert(0);
        *e = e.wrapping_add(1);
    }
}

/// Test seam: bump `(name, half)`'s epoch directly, posing a change landing
/// between a seed's epoch capture and its store write.
#[cfg(test)]
pub(crate) fn bump_epoch_for_test(name: &ProfileName, half: Half) {
    bump_epoch(name, half);
}

/// Test seam: try to acquire the epoch guard, returning `None` while a seed
/// holds it. Pins that the seed holds the guard across its compare and writes.
#[cfg(test)]
pub(crate) fn try_lock_epoch_for_test() -> Option<crate::lockorder::RankedGuard<'static, EpochMap>>
{
    EPOCH.try_lock().ok()
}

/// Drop one half's disk caches. Called inside the change site's state-flock
/// hold, BEFORE the identity writes, so a reloading process that re-seeds from
/// disk after the writes finds the old cache already gone.
pub(crate) fn forget_disk(name: &ProfileName, half: Half) {
    match half {
        Half::OAuth => {
            remove_profile_cache(name, USAGE_CACHE_FILE);
            remove_profile_cache(name, PROFILE_FETCHED_CACHE_FILE);
            remove_profile_cache(name, KICK_BLOCK_CACHE_FILE);
        }
        Half::ThirdParty => {
            // The balance-marker cache alone. The durable dead-key verdict
            // (`third_party_auth.json`) is fingerprint-bound: an old key's
            // verdict never matches a new key, and a failed save that keeps the
            // still-stored key must keep its dead verdict too.
            remove_profile_cache(name, THIRD_PARTY_CACHE_FILE);
        }
        Half::Codex => {
            // The codex reading shares `usage_cache.json` with the OAuth half
            // (pre-existing). An account change must drop the old account's
            // reading now; a same-name claude profile's reading refills on its
            // next fetch, and a same-name pair is already excluded from the
            // identity checks.
            remove_profile_cache(name, USAGE_CACHE_FILE);
        }
        Half::Throughput => {
            remove_profile_cache(name, crate::throughput::THROUGHPUT_CACHE_FILE);
        }
    }
}

/// A memory forget a change site noted for the TUI to apply once its config
/// guard drops. `Rename` moves entries rather than dropping them — the disk
/// half already moved with the directory.
enum PendingAction {
    Forget(ProfileName, Half),
    Rename(ProfileName, ProfileName),
}

/// Forgets a change site noted for a store-holding process (the TUI, the
/// daemon) to apply to memory. Process-global; a process that never registers
/// stores (the CLI, the MCP server) notes nothing, so its queue cannot grow.
static PENDING_FORGETS: LazyLock<RankedMutex<Vec<PendingAction>, rank::ForgetQueue>> =
    LazyLock::new(|| RankedMutex::new(Vec::new()));

/// Whether THIS process built reading stores and so drains the pending queue.
/// Set once by the TUI and the daemon when they build their stores; never set
/// by the CLI or the MCP server, which hold no stores to forget.
static STORES_REGISTERED: AtomicBool = AtomicBool::new(false);

/// Mark that this process holds reading stores and will drain the pending
/// forgets. A change site's `note_forget` is a no-op until this runs, so a
/// process with no stores records nothing and its queue never grows.
pub(crate) fn register_memory_stores() {
    STORES_REGISTERED.store(true, Ordering::Release);
}

/// Record that `name`'s `half` changed and must be forgotten in memory. Called
/// inside the change site's hold after the identity writes land.
pub(crate) fn note_forget(name: &ProfileName, half: Half) {
    if !STORES_REGISTERED.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut pending) = PENDING_FORGETS.lock() {
        pending.push(PendingAction::Forget(name.clone(), half));
    }
}

/// Record that `old` was renamed to `new` and its memory entries must move.
/// Called inside the rename's hold after the record move lands.
pub(crate) fn note_rename(old: &ProfileName, new: &ProfileName) {
    if !STORES_REGISTERED.load(Ordering::Acquire) {
        return;
    }
    if let Ok(mut pending) = PENDING_FORGETS.lock() {
        pending.push(PendingAction::Rename(old.clone(), new.clone()));
    }
}

/// Test seam: drop every queued forget and the registered-stores flag, so a
/// change site that ran under one sandbox cannot leak a forget into the next
/// test's drain. Called from `HomeSandbox::drop`.
#[cfg(test)]
pub(crate) fn reset_pending_forgets_for_test() {
    if let Ok(mut pending) = PENDING_FORGETS.lock() {
        pending.clear();
    }
    STORES_REGISTERED.store(false, Ordering::Release);
}

/// The stores `forget_memory` clears, cloned by a store-holding process (the
/// TUI's `App` or the daemon; the scheduler-only members ride along so the
/// forget reaches the whole process).
pub(crate) struct MemoryStores {
    pub(crate) usage: UsageStore,
    pub(crate) usage_status: StatusStore,
    pub(crate) last_fetched: LastFetchedAt,
    pub(crate) next_refresh: NextRefreshPerProfile,
    pub(crate) poll_streaks: PollStreaks,
    pub(crate) kick_blocks: KickBlocks,
    pub(crate) weekly_reset_kicks: WeeklyResetKicks,
    pub(crate) auto_start_queue: AutoStartQueueState,
    pub(crate) third_party_usage: ThirdPartyUsageStore,
    pub(crate) third_party_status: ThirdPartyStatusStore,
    pub(crate) third_party_streaks: ThirdPartyStreaks,
    pub(crate) third_party_broken: ThirdPartyBroken,
    pub(crate) suppressed: SuppressedAuthExpiredStore,
    pub(crate) interval_ms: Arc<AtomicU64>,
}

#[cfg(test)]
impl MemoryStores {
    /// Empty stores, for a test that drives a route or drain without a real
    /// scheduler behind it. Does NOT register stores: registration is the
    /// change-site gate, and a test that runs no change site needs none.
    pub(crate) fn empty_for_test() -> Self {
        use std::collections::HashSet;
        MemoryStores {
            usage: Arc::new(RankedMutex::new(HashMap::new())),
            usage_status: Arc::new(RankedMutex::new(HashMap::new())),
            last_fetched: Arc::new(RankedMutex::new(HashMap::new())),
            next_refresh: Arc::new(RankedMutex::new(HashMap::new())),
            poll_streaks: Arc::new(RankedMutex::new(HashMap::new())),
            kick_blocks: Arc::new(RankedMutex::new(HashMap::new())),
            weekly_reset_kicks: Arc::new(RankedMutex::new(HashSet::new())),
            auto_start_queue: super::new_auto_start_queue_state(),
            third_party_usage: Arc::new(RankedMutex::new(HashMap::new())),
            third_party_status: Arc::new(RankedMutex::new(HashMap::new())),
            third_party_streaks: Arc::new(RankedMutex::new(HashMap::new())),
            third_party_broken: Arc::new(RankedMutex::new(HashMap::new())),
            suppressed: Arc::new(RankedMutex::new(HashMap::new())),
            interval_ms: Arc::new(AtomicU64::new(90_000)),
        }
    }
}

/// Forget `name`'s `half` in THIS process's memory: bump the epoch, clear every
/// name-keyed entry the half owns, then re-seed the reading from disk (so a
/// seed another process wrote survives — the re-seed finds nothing when
/// `forget_disk` just removed the cache). Memory only: the durable `/profile`
/// stamp is `forget_disk(OAuth)`'s to remove, never this clear's. Runs with no
/// ranked lock held (the stores rank BELOW `Config`, whose guard the caller
/// must have dropped).
pub(crate) fn forget_memory(stores: &MemoryStores, name: &ProfileName, half: Half) {
    bump_epoch(name, half);
    clear_memory(stores, name, half);
    let interval_ms = stores.interval_ms.load(Ordering::Relaxed);
    let now = now_ms();
    match half {
        Half::OAuth => {
            let _ = try_seed_cache(
                &stores.usage,
                &stores.usage_status,
                &stores.last_fetched,
                name,
                now,
                interval_ms,
            );
        }
        Half::ThirdParty => {
            seed_third_party_name(
                &stores.third_party_usage,
                &stores.third_party_status,
                &stores.last_fetched,
                name,
                false,
                now,
                interval_ms,
            );
        }
        Half::Codex => {
            // The codex reading shares `usage_cache.json` and the `usage` /
            // `usage_status` entries with a same-name claude profile, so
            // re-seed from that cache: a claude reading comes back from its own
            // cache instead of staying blank until its next fetch.
            seed_codex_name(&stores.usage, &stores.usage_status, name, now, interval_ms);
        }
        Half::Throughput => {}
    }
}

fn clear_memory(stores: &MemoryStores, name: &ProfileName, half: Half) {
    match half {
        Half::OAuth => {
            stores
                .usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .usage_status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .poll_streaks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .last_fetched
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&FetchLeg::OAuth.key(name.clone()));
            stores
                .next_refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&FetchLeg::OAuth.key(name.clone()));
            stores
                .kick_blocks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .weekly_reset_kicks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name);
            super::forget_election_failures(&stores.auto_start_queue, name);
            forget_profile_memo(name);
        }
        Half::ThirdParty => {
            stores
                .third_party_usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .third_party_status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .third_party_streaks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .third_party_broken
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .last_fetched
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&FetchLeg::ThirdParty.key(name.clone()));
            stores
                .next_refresh
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&FetchLeg::ThirdParty.key(name.clone()));
            stores
                .suppressed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
        }
        Half::Codex => {
            stores
                .usage
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            stores
                .usage_status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(name.as_str());
            forget_codex_polled(name.as_str());
        }
        Half::Throughput => {}
    }
}

/// Drain the forgets and renames a change site noted and apply each to memory.
/// Called by the TUI after its config guard drops (the change site itself
/// cannot: the stores rank below `Config`).
pub(crate) fn apply_pending_forgets(stores: &MemoryStores) {
    let pending: Vec<PendingAction> = PENDING_FORGETS
        .lock()
        .ok()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default();
    for action in pending {
        match action {
            PendingAction::Forget(name, half) => forget_memory(stores, &name, half),
            PendingAction::Rename(old, new) => forget_memory_rename(stores, &old, &new),
        }
    }
}

/// Move `old`'s in-memory entries to `new` after a rename (its disk caches
/// already moved with the directory), and bump the OLD name's epochs so a
/// stale in-flight landing or seed under the old name writes nothing.
pub(crate) fn forget_memory_rename(stores: &MemoryStores, old: &ProfileName, new: &ProfileName) {
    bump_epoch(old, Half::OAuth);
    bump_epoch(old, Half::ThirdParty);
    bump_epoch(old, Half::Codex);
    move_name_entry(&stores.usage, old, new);
    move_name_entry(&stores.usage_status, old, new);
    move_name_entry(&stores.third_party_usage, old, new);
    move_name_entry(&stores.third_party_status, old, new);
    move_name_entry(&stores.third_party_streaks, old, new);
    move_name_entry(&stores.third_party_broken, old, new);
    move_name_entry(&stores.poll_streaks, old, new);
    move_name_entry(&stores.kick_blocks, old, new);
    move_name_entry(&stores.suppressed, old, new);
    move_leg_entries(&stores.last_fetched, old, new);
    move_leg_entries(&stores.next_refresh, old, new);
    let mut set = stores
        .weekly_reset_kicks
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if set.remove(old) {
        set.insert(new.clone());
    }
    super::move_election_failures(&stores.auto_start_queue, old, new);
    // The durable `/profile` stamp rode the directory to `new`; only the old
    // name's memo is left, so `new` re-reads the moved stamp fresh.
    forget_profile_memo(old);
}

fn move_name_entry<T, R: crate::lockorder::Rank>(
    map: &RankedMutex<HashMap<String, T>, R>,
    old: &ProfileName,
    new: &ProfileName,
) {
    let mut m = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(value) = m.remove(old.as_str()) {
        m.insert(new.to_string(), value);
    }
}

fn move_leg_entries<T, R: crate::lockorder::Rank>(
    map: &RankedMutex<HashMap<super::LegKey, T>, R>,
    old: &ProfileName,
    new: &ProfileName,
) {
    let mut m = map
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for leg in [FetchLeg::OAuth, FetchLeg::ThirdParty] {
        if let Some(value) = m.remove(&leg.key(old.clone())) {
            m.insert(leg.key(new.clone()), value);
        }
    }
}

/// The halves `forget_disk` and `note_forget` must run for, in disk-first order.
/// Split from [`ForgetPlan`] so a change site drives both loops off one value.
fn disk_halves(plan: ForgetPlan) -> impl Iterator<Item = Half> {
    plan.all_halves()
}

/// Forget the changed halves' DISK caches, before the identity writes land.
pub(crate) fn forget_disk_plan(name: &ProfileName, plan: ForgetPlan) {
    for half in disk_halves(plan) {
        forget_disk(name, half);
    }
}

/// Note the changed halves for `forget_memory`, after the identity writes land.
pub(crate) fn note_forget_plan(name: &ProfileName, plan: ForgetPlan) {
    for half in plan.memory_halves() {
        note_forget(name, half);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{OAuthIdentity, oauth_changed, third_party_identity};
    use crate::profile::AccountId;

    fn oa(anchor: Option<&str>, token: Option<&str>) -> OAuthIdentity {
        OAuthIdentity::for_values(anchor.map(AccountId::from).as_ref(), token)
    }

    #[test]
    fn an_anchor_appearing_over_an_unchanged_token_is_not_a_change() {
        let before = oa(None, Some("access-A"));
        let after = oa(Some("uuid-A"), Some("access-A"));
        assert!(
            !oauth_changed(&before, &after),
            "a backfill or adopt over the same stored token is not a change"
        );
    }

    #[test]
    fn an_anchor_removed_while_a_login_stays_is_a_change() {
        let before = oa(Some("uuid-A"), Some("access-A"));
        let after = oa(None, Some("access-A"));
        assert!(
            oauth_changed(&before, &after),
            "an unproven install drops the anchor while the login stays"
        );
    }

    #[test]
    fn an_anchor_move_is_a_change() {
        let before = oa(Some("uuid-A"), Some("access-A"));
        let after = oa(Some("uuid-B"), Some("access-A"));
        assert!(
            oauth_changed(&before, &after),
            "an anchor moving to another account is a change"
        );
    }

    #[test]
    fn an_unanchored_token_change_is_a_change() {
        let before = oa(None, Some("access-A"));
        let after = oa(None, Some("access-B"));
        assert!(
            oauth_changed(&before, &after),
            "a stored-token change on an unanchored profile is a change"
        );
    }

    #[test]
    fn an_anchored_rotation_is_not_a_change() {
        let before = oa(Some("uuid-A"), Some("access-A"));
        let after = oa(Some("uuid-A"), Some("access-B"));
        assert!(
            !oauth_changed(&before, &after),
            "a rotation on an anchored profile keeps the anchor"
        );
    }

    #[test]
    fn for_values_drops_an_anchor_without_a_login() {
        let no_login = OAuthIdentity::for_values(Some(&AccountId::from("uuid-A")), None);
        assert!(
            no_login.anchor.is_none() && no_login.stored.is_none(),
            "no stored login can carry an anchor"
        );
        let before = oa(Some("uuid-A"), Some("access-A"));
        assert!(
            oauth_changed(&before, &no_login),
            "a login disappearing reads as a change even when the anchor name survives"
        );
    }

    #[test]
    fn a_generic_key_edit_changes_the_third_party_identity() {
        let before = third_party_identity(None, Some("key-A"), None, Some("https://gw.example"));
        let after = third_party_identity(None, Some("key-B"), None, Some("https://gw.example"));
        assert_ne!(
            before, after,
            "a generic key edit over the same base_url is a change"
        );
    }

    #[test]
    fn a_generic_base_url_edit_changes_the_third_party_identity() {
        let before = third_party_identity(None, Some("key-A"), None, Some("https://gw-a.example"));
        let after = third_party_identity(None, Some("key-A"), None, Some("https://gw-b.example"));
        assert_ne!(
            before, after,
            "a generic base_url edit over the same key is a change"
        );
    }
}

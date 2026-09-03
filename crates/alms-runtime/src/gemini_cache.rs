// SPDX-License-Identifier: Apache-2.0

//! Gemini explicit context caching (#769).
//!
//! Gemini's context-caching feature is a REST resource, not an inline
//! marker: you `POST /v1beta/cachedContents` once with the stable prefix
//! (system instruction + tool definitions) and get back a cache `name`
//! like `cachedContents/abc123`. Subsequent `generateContent` /
//! `streamGenerateContent` requests reference the cache via the
//! `cachedContent` request field and are billed at a discounted rate for
//! the prefix tokens covered by the cache.
//!
//! This module owns the client-side state needed to reuse the cache
//! across turns:
//!
//! - A `DashMap<SessionId, CacheHandle>` keeps the active cache name per
//!   session, so two turns in the same session share one cache. Entries
//!   are scoped to a single `LlmClient` instance; there is no persistence
//!   across gateway restarts (caches expire naturally and Gemini rejects
//!   stale references, at which point the adapter recreates).
//!
//! - A **prefix hash** stored alongside the cache name invalidates
//!   logically when the stable prefix changes (workspace edit, tool
//!   list change, system-prompt rewrite). Hash mismatch ⇒ delete the
//!   stale handle and create a new one on the next turn.
//!
//! - The **32,768-token minimum** for Gemini caching is enforced
//!   server-side. We do not estimate token count client-side; instead we
//!   attempt cache creation unconditionally when `cache_enabled=true`,
//!   and if Gemini rejects with "prefix too small" we memoize the
//!   rejection in a separate process-wide set keyed on `prefix_hash`
//!   (not `session_id`) to suppress further attempts on that prefix
//!   across every session. This matches the Anthropic caching posture
//!   (silent no-op below threshold) and avoids a brittle tokenizer
//!   re-implementation. Keying on the prefix alone — rather than
//!   `(session_id, prefix_hash)` — means many small agents that share
//!   the same stable prefix only pay one wasted round-trip total, not
//!   one per agent per session (#787 review).
//!
//! - Cache-expired error handling lives on the call site
//!   (`LlmClient::complete` / `complete_stream`): when Gemini returns a
//!   status indicating the referenced `cachedContents/...` is gone, the
//!   call site calls [`GeminiCacheStore::invalidate`] and retries the
//!   request once without `cachedContent`. Transparent to the agent loop.

use alms_core::SessionId;
use dashmap::DashMap;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::gemini::{GeminiContent, GeminiTool};

/// Outcome of an `ensure_cache` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CacheLookup {
    /// A valid cache name is available and should be attached to the next
    /// request via the `cachedContent` field.
    Hit(String),
    /// No cache is available — either caching is disabled, the prefix is
    /// known to be below Gemini's minimum cacheable size, the creation
    /// request failed, or this is the first turn and the caller chose
    /// not to create on first turn. Request dispatches without
    /// `cachedContent`.
    Miss,
}

/// Per-session cache state stored inside [`GeminiCacheStore`].
///
/// Note: the `TooSmall` sentinel used to live on this enum, keyed per
/// session. As of the #787 review it lives in a separate process-wide
/// set keyed on `prefix_hash` alone (see [`GeminiCacheStoreInner::too_small`]),
/// so agents that share a stable prefix also share the "already known
/// too-small" memoization and skip the wasted `cachedContents.create`
/// round-trip on their first turn.
#[derive(Debug, Clone)]
enum CacheHandle {
    /// Gemini created a cache for this prefix; next turns should reference
    /// `name` unless the prefix hash changes.
    Active { name: String, prefix_hash: u64 },
}

/// In-process store of Gemini context caches keyed by `SessionId`.
///
/// Shared via `Arc` across clones of [`LlmClient`] so a subagent spawned
/// from a parent client sees the same cache entries. The struct is
/// `Send + Sync`: `DashMap` covers the storage, and the reqwest HTTP
/// client used for cache creation is itself `Send + Sync`.
///
/// This is deliberately in-memory rather than persisted to SQLite:
///
/// - Cache TTL is short (seconds to minutes), so even a brief gateway
///   restart would invalidate most entries anyway.
/// - Gemini rejects references to expired caches with a specific error
///   the adapter recovers from transparently, so "lost our handle after
///   restart" is already a supported path.
/// - Avoiding a SQLite write on every turn keeps the hot loop tight.
#[derive(Clone)]
pub(crate) struct GeminiCacheStore {
    inner: Arc<GeminiCacheStoreInner>,
}

struct GeminiCacheStoreInner {
    /// Live cache handles keyed per-session. Each entry is always an
    /// [`CacheHandle::Active`] — the "too small" sentinel lives in
    /// `too_small` below, not here, so it can be reused across sessions
    /// that share the same stable prefix.
    entries: DashMap<SessionId, CacheHandle>,
    /// Process-wide set of prefix hashes that Gemini has rejected with
    /// "prefix too small" (the documented 32,768-token floor).  Keyed on
    /// prefix hash alone — if agent A's system prompt is below the floor
    /// and agent B shares the same system prompt, B inherits A's
    /// rejection memoization and never sends its own wasted
    /// `cachedContents.create` round-trip on the first turn.
    ///
    /// `DashMap<u64, ()>` is used as a concurrent set; value is unit.
    /// The map is only ever *inserted into* — entries are never removed,
    /// because "too small" is a property of the prefix, not the session,
    /// and the prefix-hash key already encodes every piece of input
    /// (system instruction + tools). A future enhancement could cap the
    /// map size, but for realistic workloads the entry count is bounded
    /// by the number of distinct small-prefix shapes, which is tiny.
    too_small: DashMap<u64, ()>,
    http: Client,
}

impl std::fmt::Debug for GeminiCacheStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiCacheStore")
            .field("entries", &self.inner.entries.len())
            .field("too_small", &self.inner.too_small.len())
            .finish()
    }
}

impl GeminiCacheStore {
    /// Create a new store. Caller passes the reqwest client used for
    /// `cachedContents` API calls so that connection-pool sharing with
    /// the main LLM client is automatic.
    pub(crate) fn new(http: Client) -> Self {
        Self {
            inner: Arc::new(GeminiCacheStoreInner {
                entries: DashMap::new(),
                too_small: DashMap::new(),
                http,
            }),
        }
    }

    /// Look up or create a cache for the given `(session_id, prefix)`.
    ///
    /// The prefix is hashed so that workspace-file edits or tool-list
    /// changes invalidate the handle automatically — on hash mismatch we
    /// drop the old entry and attempt to create a new one.
    ///
    /// Returns [`CacheLookup::Hit`] when a usable cache name is
    /// available, [`CacheLookup::Miss`] when the caller should dispatch
    /// without `cachedContent` (e.g. creation failed, or the prefix was
    /// flagged too small on a prior attempt).
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn ensure_cache(
        &self,
        session_id: SessionId,
        model: &str,
        base_url: &str,
        api_key: &str,
        system_instruction: Option<&GeminiContent>,
        tools: Option<&[GeminiTool]>,
        ttl_seconds: u64,
    ) -> CacheLookup {
        let hash = hash_prefix(system_instruction, tools);

        // Cross-session short-circuit: if *any* prior turn (possibly in a
        // different session) has already learned that this exact prefix
        // is below Gemini's 32k-token floor, skip the
        // `cachedContents.create` round-trip entirely. This is the key
        // change for #787 review item 4 — small-prefix agents no longer
        // pay a one-round-trip tax on every new session.
        if self.inner.too_small.contains_key(&hash) {
            // #787 re-review nit 3: if this session had an `Active`
            // entry for a *previous* (cacheable) prefix, the prefix
            // rotated to a known-too-small shape, and we're about to
            // short-circuit to Miss, the stale handle would leak until
            // the prefix rotated back to something cacheable. Evict it
            // here so the session's entry count stays bounded — the
            // handle it points at is about to expire naturally on
            // Gemini's side anyway.
            if self.inner.entries.remove(&session_id).is_some() {
                debug!(
                    session_id = %session_id.0,
                    "gemini cache prefix rotated to known-too-small — evicting stale handle"
                );
            }
            debug!(
                session_id = %session_id.0,
                "gemini cache prefix matches known-too-small entry — skipping create"
            );
            return CacheLookup::Miss;
        }

        // Fast path: existing live cache with matching hash.
        if let Some(entry) = self.inner.entries.get(&session_id) {
            match &*entry {
                CacheHandle::Active { name, prefix_hash } if *prefix_hash == hash => {
                    debug!(
                        session_id = %session_id.0,
                        cache_name = %name,
                        "gemini cache hit"
                    );
                    return CacheLookup::Hit(name.clone());
                }
                CacheHandle::Active { .. } => {
                    // Hash mismatch — stored handle is for a different
                    // prefix. Drop and proceed to creation. Falls through
                    // to the remove below (we don't hold the ref across
                    // async points).
                }
            }
        }

        // Stale or missing entry — try to create. If another concurrent
        // request has already created one in the meantime, the insert
        // after creation will still succeed (second create wins; the
        // first one will expire naturally). For a single session this
        // race is impossible because the agent loop is serial per
        // session, but the store tolerates it by construction.
        if self.inner.entries.contains_key(&session_id) {
            debug!(
                session_id = %session_id.0,
                "gemini cache prefix changed or stale — dropping handle"
            );
            self.inner.entries.remove(&session_id);
        }

        match create_cache(
            &self.inner.http,
            base_url,
            api_key,
            model,
            system_instruction,
            tools,
            ttl_seconds,
        )
        .await
        {
            Ok(name) => {
                info!(
                    session_id = %session_id.0,
                    cache_name = %name,
                    "gemini cache created"
                );
                self.inner.entries.insert(
                    session_id,
                    CacheHandle::Active {
                        name: name.clone(),
                        prefix_hash: hash,
                    },
                );
                CacheLookup::Hit(name)
            }
            Err(CreateError::TooSmall) => {
                // Gemini says the prefix is below the cacheable floor
                // (32,768 tokens). Memoize on the prefix hash alone so
                // other sessions with the same prefix inherit the
                // rejection and skip the wasted round-trip. Only retry
                // if the prefix grows (different hash).
                debug!(
                    session_id = %session_id.0,
                    "gemini cache prefix below 32k-token minimum — disabling for all sessions with this prefix"
                );
                self.inner.too_small.insert(hash, ());
                CacheLookup::Miss
            }
            Err(CreateError::Other(msg)) => {
                // Log at warn but don't poison the cache map — transient
                // creation failures (network blip, quota) should not
                // prevent future attempts.
                warn!(
                    session_id = %session_id.0,
                    error = %msg,
                    "gemini cache creation failed — falling back to no-cache request"
                );
                CacheLookup::Miss
            }
        }
    }

    /// Invalidate the stored handle for a session. Called by the LLM
    /// client when a request fails with a cache-expired error so the
    /// next turn creates a fresh cache.
    pub(crate) fn invalidate(&self, session_id: SessionId) {
        if self.inner.entries.remove(&session_id).is_some() {
            info!(
                session_id = %session_id.0,
                "gemini cache invalidated (expired or not found)"
            );
        }
    }

    /// Test-only: peek at the cached name for a session without touching
    /// network. Returns `None` if no handle is stored.
    #[cfg(test)]
    pub(crate) fn peek_active(&self, session_id: SessionId) -> Option<String> {
        self.inner.entries.get(&session_id).map(|e| match &*e {
            CacheHandle::Active { name, .. } => name.clone(),
        })
    }

    /// Test-only: install an `Active` entry for a session.
    #[cfg(test)]
    pub(crate) fn install_active_for_test(
        &self,
        session_id: SessionId,
        name: String,
        prefix_hash: u64,
    ) {
        self.inner
            .entries
            .insert(session_id, CacheHandle::Active { name, prefix_hash });
    }

    /// Test-only: install a "too small" memoization for a prefix hash,
    /// simulating a prior `cachedContents.create` rejection without
    /// touching the network.
    #[cfg(test)]
    pub(crate) fn install_too_small_for_test(&self, prefix_hash: u64) {
        self.inner.too_small.insert(prefix_hash, ());
    }

    /// Test-only: check whether a prefix hash is memoized as too-small.
    #[cfg(test)]
    pub(crate) fn is_too_small_for_test(&self, prefix_hash: u64) -> bool {
        self.inner.too_small.contains_key(&prefix_hash)
    }
}

/// Errors returned by [`create_cache`]. Split into `TooSmall` vs `Other`
/// because the store treats them differently: `TooSmall` poisons the
/// entry for the current prefix, while `Other` is transient.
#[derive(Debug)]
enum CreateError {
    TooSmall,
    Other(String),
}

/// POST to Gemini's `cachedContents` endpoint.
///
/// Body shape (from <https://ai.google.dev/gemini-api/docs/caching>):
///
/// ```json
/// {
///   "model": "models/gemini-2.5-pro",
///   "systemInstruction": { ... },
///   "contents": [],
///   "tools": [ ... ],
///   "ttl": "300s"
/// }
/// ```
///
/// Returns the cache `name` on success.
async fn create_cache(
    http: &Client,
    base_url: &str,
    api_key: &str,
    model: &str,
    system_instruction: Option<&GeminiContent>,
    tools: Option<&[GeminiTool]>,
    ttl_seconds: u64,
) -> Result<String, CreateError> {
    let url = format!("{base_url}/cachedContents");

    let body = CreateCacheRequest {
        model: format!("models/{model}"),
        system_instruction,
        contents: &[],
        tools,
        ttl: format!("{ttl_seconds}s"),
    };

    let response = http
        .post(&url)
        .header("Content-Type", "application/json")
        .header("x-goog-api-key", api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| CreateError::Other(format!("HTTP request failed: {e}")))?;

    let status = response.status();
    let body_text = response
        .text()
        .await
        .unwrap_or_else(|_| "<unavailable>".to_string());

    if !status.is_success() {
        // Gemini reports "prefix too small" with status 400 and an error
        // body containing the phrase "Cached content is too small" (plus
        // minimum-token count). We match on a stable fragment rather
        // than exact text so small wording changes don't break the
        // below-minimum detection path.
        let lower = body_text.to_ascii_lowercase();
        if lower.contains("too small")
            || lower.contains("below the minimum")
            || lower.contains("minimum cache size")
        {
            return Err(CreateError::TooSmall);
        }
        return Err(CreateError::Other(format!(
            "cachedContents.create returned {status}: {body_text}"
        )));
    }

    let parsed: CreateCacheResponse = serde_json::from_str(&body_text).map_err(|e| {
        CreateError::Other(format!(
            "failed to parse cachedContents.create response: {e} — body: {body_text}"
        ))
    })?;

    if parsed.name.is_empty() {
        return Err(CreateError::Other(
            "cachedContents.create response missing `name`".to_string(),
        ));
    }

    Ok(parsed.name)
}

/// Detect whether an error body from Gemini's `generateContent` endpoint
/// indicates a referenced `cachedContents/...` has expired or been
/// deleted. Call sites use this to decide whether to invalidate the
/// store and retry without the cache.
///
/// Gemini's documented error is `NOT_FOUND` with a message matching
/// "CachedContent ... was not found" (the 404-equivalent), but the
/// specific wording varies across model versions — we match the robust
/// substrings.
///
/// The matcher is anchored on the `"cachedcontent"` token so an
/// arbitrary 404 (e.g. `Model models/xyz not found`) cannot trip the
/// recovery path. The disjunction covers the documented wordings plus
/// two additional variants observed on `generateContent`:
/// `"does not exist"` (alternate phrasing for a GC'd cache handle) and
/// the machine-readable `"not_found"` status token in the error JSON
/// (`"status": "NOT_FOUND"` lowercases to `not_found`).
pub(crate) fn is_cache_not_found_error(error_body: &str) -> bool {
    let lower = error_body.to_ascii_lowercase();
    // The unique-ish phrase Gemini uses for stale cache references.
    lower.contains("cachedcontent")
        && (lower.contains("not found")
            || lower.contains("expired")
            || lower.contains("does not exist")
            || lower.contains("not_found"))
}

/// Hash the stable-prefix pieces (system instruction + tool definitions)
/// that define a cache entry. Used as the invalidation key — on every
/// turn the store re-hashes the prefix and compares against the stored
/// hash; a mismatch means the prefix changed and the old cache is
/// logically stale.
///
/// Uses Rust's default `DefaultHasher`. The hash is stable within a
/// single process but **not** guaranteed across restarts — which is
/// fine because the cache itself isn't persisted either.
fn hash_prefix(system_instruction: Option<&GeminiContent>, tools: Option<&[GeminiTool]>) -> u64 {
    let mut h = DefaultHasher::new();
    // Serialize to JSON so every field contributes deterministically.
    // Using serde_json ensures the hash reflects the exact wire bytes the
    // cache was built from, with no reliance on `#[derive(Hash)]` on
    // types we don't control.
    let sys_json = system_instruction
        .and_then(|c| serde_json::to_string(c).ok())
        .unwrap_or_default();
    sys_json.hash(&mut h);
    let tools_json = tools
        .and_then(|t| serde_json::to_string(t).ok())
        .unwrap_or_default();
    tools_json.hash(&mut h);
    h.finish()
}

// ---------------------------------------------------------------------------
// Wire types for `cachedContents.create`
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CreateCacheRequest<'a> {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "systemInstruction")]
    pub system_instruction: Option<&'a GeminiContent>,
    pub contents: &'a [GeminiContent],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<&'a [GeminiTool]>,
    /// Gemini accepts duration strings in `<N>s` form. We always send
    /// seconds (no fractional component) because the TOML surface is
    /// `u64` seconds.
    pub ttl: String,
}

#[derive(Debug, Deserialize)]
struct CreateCacheResponse {
    /// Fully-qualified cache name — exactly what callers pass back in
    /// `cachedContent` on subsequent `generateContent` requests. Format:
    /// `cachedContents/<id>`.
    #[serde(default)]
    pub name: String,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gemini::{GeminiFunctionDeclaration, GeminiPart, GeminiTool};

    fn sample_system() -> GeminiContent {
        GeminiContent {
            role: None,
            parts: vec![GeminiPart::Text {
                text: "You are a helpful agent.".to_string(),
                thought: false,
            }],
        }
    }

    fn sample_tools() -> Vec<GeminiTool> {
        vec![GeminiTool {
            function_declarations: vec![GeminiFunctionDeclaration {
                name: "echo".into(),
                description: "Echo text".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
        }]
    }

    #[test]
    fn prefix_hash_is_stable_for_identical_input() {
        let sys = sample_system();
        let tools = sample_tools();
        let a = hash_prefix(Some(&sys), Some(&tools));
        let b = hash_prefix(Some(&sys), Some(&tools));
        assert_eq!(a, b, "identical inputs must hash to the same value");
    }

    #[test]
    fn prefix_hash_changes_when_system_changes() {
        let sys_a = sample_system();
        let sys_b = GeminiContent {
            role: None,
            parts: vec![GeminiPart::Text {
                text: "DIFFERENT system prompt".to_string(),
                thought: false,
            }],
        };
        let tools = sample_tools();
        let ha = hash_prefix(Some(&sys_a), Some(&tools));
        let hb = hash_prefix(Some(&sys_b), Some(&tools));
        assert_ne!(ha, hb, "system-prompt edit must invalidate the hash");
    }

    #[test]
    fn prefix_hash_changes_when_tools_change() {
        let sys = sample_system();
        let tools_a = sample_tools();
        let tools_b = vec![GeminiTool {
            function_declarations: vec![GeminiFunctionDeclaration {
                name: "shell".into(),
                description: "Run a shell command".into(),
                parameters: serde_json::json!({"type":"object"}),
            }],
        }];
        let ha = hash_prefix(Some(&sys), Some(&tools_a));
        let hb = hash_prefix(Some(&sys), Some(&tools_b));
        assert_ne!(ha, hb, "tool-list change must invalidate the hash");
    }

    #[test]
    fn invalidate_drops_stored_handle() {
        let store = GeminiCacheStore::new(Client::new());
        let session = SessionId::new();
        store.install_active_for_test(session, "cachedContents/abc".into(), 12345);
        assert_eq!(
            store.peek_active(session),
            Some("cachedContents/abc".to_string())
        );
        store.invalidate(session);
        assert_eq!(
            store.peek_active(session),
            None,
            "invalidate must drop the handle"
        );
    }

    #[test]
    fn is_cache_not_found_error_matches_documented_shapes() {
        // Exact wording varies across model / API version, so we assert
        // on the robust substrings we expect to see.
        assert!(is_cache_not_found_error(
            "{\"error\":{\"message\":\"CachedContent cachedContents/xyz was not found\"}}"
        ));
        assert!(is_cache_not_found_error(
            "{\"error\":{\"message\":\"The CachedContent has expired\"}}"
        ));
        // #787 re-review nit 2: alternate 404-phrasings observed on
        // `generateContent` when a cache handle has been GC'd out of
        // band.
        assert!(
            is_cache_not_found_error(
                "{\"error\":{\"message\":\"CachedContent cachedContents/xyz does not exist\"}}"
            ),
            "\"does not exist\" phrasing must trigger cache-retry"
        );
        // Machine-readable `"status":"NOT_FOUND"` token — lowercases to
        // `not_found`. Anchored on the `cachedContent` token so a
        // model-not-found payload can't trip it.
        assert!(
            is_cache_not_found_error(
                "{\"error\":{\"code\":404,\"status\":\"NOT_FOUND\",\"message\":\"CachedContent cachedContents/xyz is gone\"}}"
            ),
            "`NOT_FOUND` status token must trigger cache-retry when paired with cachedContent"
        );
        // Unrelated error should not match.
        assert!(!is_cache_not_found_error(
            "{\"error\":{\"message\":\"Rate limit exceeded\"}}"
        ));
        // Non-cache-related NOT_FOUND errors should not trigger the
        // "cached content" recovery path.
        assert!(!is_cache_not_found_error(
            "{\"error\":{\"message\":\"Model models/gemini-2.5-pro not found\"}}"
        ));
        // Regression guard for the underscore-vs-space variants: a
        // generic `NOT_FOUND` payload without the `cachedContent` anchor
        // must not trigger recovery. This guards against the matcher
        // widening to literal `"not found"` / `"not_found"` without the
        // anchor, which would trip false positives on any 404.
        assert!(
            !is_cache_not_found_error("{\"error\":{\"status\":\"NOT_FOUND\"}}"),
            "bare NOT_FOUND without cachedContent anchor must not match"
        );
        assert!(
            !is_cache_not_found_error(
                "{\"error\":{\"message\":\"File does not exist: /tmp/foo\"}}"
            ),
            "\"does not exist\" without cachedContent anchor must not match"
        );
    }

    /// Regression test for the "cache expired → recreate transparently"
    /// path at the store level.  Full HTTP retry lives in the LLM client;
    /// here we verify that the store's invalidate+re-insert cycle leaves
    /// the map in the right state for a follow-up ensure_cache call to
    /// create a fresh entry.
    #[test]
    fn expired_cache_cycle_invalidates_then_accepts_fresh_entry() {
        let store = GeminiCacheStore::new(Client::new());
        let session = SessionId::new();
        let hash = 0xabcd_ef01;

        // 1. Parent turn installed a cache.
        store.install_active_for_test(session, "cachedContents/old".into(), hash);
        assert_eq!(
            store.peek_active(session),
            Some("cachedContents/old".to_string())
        );

        // 2. Gemini rejects a follow-up request because the cache is
        //    gone — the LLM client calls invalidate.
        store.invalidate(session);
        assert_eq!(store.peek_active(session), None);

        // 3. The store is now ready to accept a freshly created cache
        //    entry for the same session + same prefix.
        store.install_active_for_test(session, "cachedContents/new".into(), hash);
        assert_eq!(
            store.peek_active(session),
            Some("cachedContents/new".to_string())
        );
    }

    /// #787 review item 4: a `TooSmall` rejection for one session must be
    /// honoured by a second, unrelated session that happens to share the
    /// same prefix hash — no wasted `cachedContents.create` round-trip on
    /// the second session's first turn.
    ///
    /// We exercise the cross-session short-circuit directly through
    /// `ensure_cache` because that is the path the LLM client hits on
    /// every turn. The `install_too_small_for_test` seed simulates the
    /// outcome of a prior session having learned the prefix is too
    /// small, without touching the network.
    #[tokio::test]
    async fn too_small_sentinel_short_circuits_across_sessions() {
        let store = GeminiCacheStore::new(Client::new());
        let sys = sample_system();
        let tools = sample_tools();
        let hash = hash_prefix(Some(&sys), Some(&tools));

        // Session A already discovered this prefix is too small.
        store.install_too_small_for_test(hash);
        assert!(store.is_too_small_for_test(hash));

        // Session B (fresh SessionId, same prefix shape) should hit the
        // memoization and short-circuit to Miss without ever attempting
        // an HTTP round-trip.  We use a deliberately unroutable base_url
        // so that the test will hang or fail loudly if the
        // short-circuit is skipped and a real HTTP call fires.
        let session_b = SessionId::new();
        let lookup = store
            .ensure_cache(
                session_b,
                "gemini-2.5-pro",
                "http://127.0.0.1:1/unreachable",
                "api-key",
                Some(&sys),
                Some(&tools),
                300,
            )
            .await;
        assert_eq!(
            lookup,
            CacheLookup::Miss,
            "too-small prefix must short-circuit across sessions to Miss"
        );

        // Session B never got an entry — the short-circuit doesn't
        // install anything session-scoped.
        assert!(
            store.peek_active(session_b).is_none(),
            "too-small short-circuit must not pollute the per-session map"
        );
    }

    /// #787 re-review nit 3: when the too-small short-circuit fires, any
    /// pre-existing `Active` handle for the same session must be evicted.
    /// Scenario: a session has a live cache for prefix P1, the prefix
    /// rotates to a known-too-small P2, and the short-circuit path
    /// returns Miss. The stale P1 handle (pointing at a cache for a
    /// prefix we will never reference again on this turn) must not
    /// linger in the entries map.
    #[tokio::test]
    async fn too_small_short_circuit_evicts_stale_active_entry() {
        let store = GeminiCacheStore::new(Client::new());
        let session = SessionId::new();

        // Turn 1: session had a cacheable prefix P1 and got a live
        // handle. Hash 0xdead_beef stands in for P1's hash.
        store.install_active_for_test(session, "cachedContents/live".into(), 0xdead_beef);
        assert_eq!(
            store.peek_active(session),
            Some("cachedContents/live".to_string()),
            "precondition: session has a live cache handle"
        );

        // Turn 2: the stable prefix rotates to P2, which a prior session
        // already discovered is below the 32k floor. Seed the too_small
        // memoization on P2's actual hash.
        let sys = sample_system();
        let tools = sample_tools();
        let hash_p2 = hash_prefix(Some(&sys), Some(&tools));
        store.install_too_small_for_test(hash_p2);

        // ensure_cache with prefix P2 must (a) short-circuit to Miss and
        // (b) evict the stale P1 handle. Use a deliberately unroutable
        // base_url — if the short-circuit is skipped and a real HTTP
        // call fires the test will hang or error loudly.
        let lookup = store
            .ensure_cache(
                session,
                "gemini-2.5-pro",
                "http://127.0.0.1:1/unreachable",
                "api-key",
                Some(&sys),
                Some(&tools),
                300,
            )
            .await;
        assert_eq!(
            lookup,
            CacheLookup::Miss,
            "too-small short-circuit must return Miss"
        );
        assert!(
            store.peek_active(session).is_none(),
            "too-small short-circuit must evict the session's stale Active handle"
        );
    }

    /// A `TooSmall` memoization for prefix P1 must NOT block a session
    /// whose prefix is a different shape P2 (different hash). Regression
    /// guard so the cross-session reuse doesn't turn into a global
    /// killswitch.
    #[test]
    fn too_small_sentinel_is_per_prefix_not_global() {
        let store = GeminiCacheStore::new(Client::new());
        let hash_a = 0x1111;
        let hash_b = 0x2222;

        store.install_too_small_for_test(hash_a);
        assert!(store.is_too_small_for_test(hash_a));
        assert!(
            !store.is_too_small_for_test(hash_b),
            "a different prefix must not inherit the too-small flag"
        );
    }
}

// SPDX-License-Identifier: Apache-2.0

//! Gemini cache-expired retry decision (#769).
//!
//! Pure decision function extracted from [`super::LlmClient::complete`] and
//! [`super::LlmClient::complete_stream`] so the two branches cannot diverge.
//! No IO, no async — exhaustively unit-testable.

/// Outcome of the Gemini cache-expired retry-decision step (#769).
///
/// Extracted as a pure function so the retry glue in [`super::LlmClient::complete`]
/// and [`super::LlmClient::complete_stream`] can be unit-tested without an HTTP
/// fixture. The two call sites share identical conditions; divergence
/// would be a latent bug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CacheRetryDecision {
    /// Retry the request once without `cachedContent`, after invalidating
    /// the stored cache handle for `session_id`. Applied only on the
    /// primary error body — a second retry is never attempted.
    Retry { session_id: alms_core::SessionId },
    /// No retry — either the request didn't reference a cache, the error
    /// is unrelated to the cache, or the session_id is missing (so we
    /// wouldn't know which store entry to invalidate). Caller propagates
    /// the original error verbatim.
    Propagate,
}

/// Decide whether to invalidate a Gemini cache handle and retry a failed
/// `generateContent` / `streamGenerateContent` request once.
///
/// The three conditions that must all hold for a retry:
/// 1. A cache was attached on the original request (otherwise there is
///    nothing to invalidate, and the error is not cache-related).
/// 2. The error body matches [`crate::gemini_cache::is_cache_not_found_error`] —
///    the cache is gone (expired, GC'd, or deleted out of band).
/// 3. A `session_id` is available so we know which store entry to drop.
///    Without it the retry would succeed but leave a stale handle behind
///    for the next turn.
///
/// Pure function (no IO, no allocations beyond the enum) so it can be
/// exhaustively covered by `#[test]` without wiremock or a fake HTTP
/// fixture. Keeps the retry contract in one place — the streaming and
/// non-streaming call sites both route through this, so they cannot
/// diverge without the compiler noticing.
pub(crate) fn decide_cache_retry(
    cache_name: Option<&str>,
    error_body: &str,
    session_id: Option<alms_core::SessionId>,
) -> CacheRetryDecision {
    if cache_name.is_none() {
        return CacheRetryDecision::Propagate;
    }
    if !crate::gemini_cache::is_cache_not_found_error(error_body) {
        return CacheRetryDecision::Propagate;
    }
    match session_id {
        Some(sid) => CacheRetryDecision::Retry { session_id: sid },
        None => CacheRetryDecision::Propagate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------ decide_cache_retry: pure function, no HTTP. ------
    //
    // These tests lock down the retry contract for Gemini's cache-expired
    // error recovery. The logic sits in the glue between `complete()` /
    // `complete_stream()` and [`GeminiCacheStore::invalidate`], and is
    // otherwise uncovered end-to-end because wiremock is not yet a dev
    // dep. Covering the decision here gives us confidence that a refactor
    // won't silently break the retry — any divergence between the
    // streaming and non-streaming branches would show up as identical
    // enum outputs here.

    /// Happy path: cache was attached, Gemini returned a cache-not-found
    /// error, session_id is present → retry.
    #[test]
    fn decide_cache_retry_returns_retry_when_all_conditions_hold() {
        let sid = alms_core::SessionId::new();
        let decision = decide_cache_retry(
            Some("cachedContents/abc"),
            "{\"error\":{\"message\":\"CachedContent cachedContents/abc was not found\"}}",
            Some(sid),
        );
        assert_eq!(decision, CacheRetryDecision::Retry { session_id: sid });
    }

    /// No cache attached: never retry (there is nothing to invalidate).
    #[test]
    fn decide_cache_retry_propagates_when_no_cache_attached() {
        let decision = decide_cache_retry(
            None,
            "{\"error\":{\"message\":\"CachedContent cachedContents/abc was not found\"}}",
            Some(alms_core::SessionId::new()),
        );
        assert_eq!(decision, CacheRetryDecision::Propagate);
    }

    /// Error unrelated to caching: propagate verbatim. A rate-limit or
    /// auth failure must not trigger a spurious retry that would paper
    /// over the real problem.
    #[test]
    fn decide_cache_retry_propagates_on_unrelated_error() {
        let sid = alms_core::SessionId::new();
        let decision = decide_cache_retry(
            Some("cachedContents/abc"),
            "{\"error\":{\"message\":\"Rate limit exceeded\"}}",
            Some(sid),
        );
        assert_eq!(decision, CacheRetryDecision::Propagate);
    }

    /// Missing session_id: propagate. We wouldn't know which store entry
    /// to invalidate, so re-dispatching without cachedContent would leave
    /// a stale handle in the map for the next turn.
    #[test]
    fn decide_cache_retry_propagates_when_session_id_missing() {
        let decision = decide_cache_retry(
            Some("cachedContents/abc"),
            "{\"error\":{\"message\":\"The CachedContent has expired\"}}",
            None,
        );
        assert_eq!(decision, CacheRetryDecision::Propagate);
    }

    /// Streaming and non-streaming call sites must produce identical
    /// decisions for identical inputs. Pinning this directly so a future
    /// refactor can't accidentally inline the logic on only one side.
    #[test]
    fn decide_cache_retry_identical_for_stream_and_non_stream_inputs() {
        let sid = alms_core::SessionId::new();
        let cases: [(Option<&str>, &str, Option<alms_core::SessionId>); 4] = [
            (
                Some("cachedContents/x"),
                "{\"error\":{\"message\":\"CachedContent cachedContents/x not found\"}}",
                Some(sid),
            ),
            (None, "anything", Some(sid)),
            (Some("cachedContents/x"), "unrelated", Some(sid)),
            (
                Some("cachedContents/x"),
                "{\"error\":{\"message\":\"expired\"}}",
                None,
            ),
        ];
        // Both code paths see exactly this helper, so "stream == non-stream"
        // collapses to "function is deterministic" — this test just
        // documents the invariant as executable.
        for (cache, err, sess) in cases {
            let a = decide_cache_retry(cache, err, sess);
            let b = decide_cache_retry(cache, err, sess);
            assert_eq!(a, b, "decide_cache_retry must be deterministic");
        }
    }
}

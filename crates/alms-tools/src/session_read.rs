//! Shared response contract for the three session-reading tools.
//!
//! `read_messages`, `read_session` and `read_subagent_session` all answer the
//! same question — "give me the tail of this transcript" — and all three feed
//! the same LLM context budget. #1028 fixed the contract for `read_messages`
//! only; #1032 extends it to the siblings, and this module is where it is
//! stated once so the three cannot drift.
//!
//! # What the contract is
//!
//! - **No silent default.** An omitted `last_n` means "everything, bounded by
//!   the caps", not a magic 20. A *malformed* `last_n` is an error rather than
//!   a silent fallback ([`parse_last_n`]).
//! - **The caps are measured in serialized JSON bytes**, not raw UTF-8 — the
//!   #1028 P1 lesson. Content full of escapable characters (`"`, `\`,
//!   newlines, control bytes) costs the model its post-escape size, so that is
//!   what the walk has to measure.
//! - **Truncation is stated, never implied.** Every response carries
//!   `total_count`, `returned_count`, `truncated` and `truncation_reason`, so
//!   an agent can tell "that is all of it" from "there is more above".
//!
//! # Why one helper rather than three migrations
//!
//! Issue #1032 proposed migrating one sibling, then the next, then deciding
//! whether a shared module was justified — on the premise that the tools have
//! "meaningfully different output shapes (DM sender attribution, summary_only
//! branches with fallback, DM-marker filtering)".
//!
//! They do, and every one of those differences sits *outside* the selection.
//! Sender attribution lives in the projection closure; marker filtering
//! decides which slice is handed in; the summary-only fallback is a separate
//! call site with a smaller message cap. What is left once those are removed —
//! walk newest to oldest, measure serialized bytes, stop at the first cap,
//! honour an explicit `last_n` verbatim but flag it — is identical at all
//! three call sites and is a pure function of its arguments. So the boundary
//! was knowable without a trial migration, and the split ships whole.

use alms_sandbox::{SandboxError, error::SandboxResult};
use serde_json::Value;

/// Hard cap on the summed *serialized JSON* size of the entries returned by
/// one session read (~15K tokens).
///
/// Sized to stay well below typical context windows so a single response
/// cannot blow the agent's budget when an LLM forgets to page. The unit is
/// JSON bytes — the `serde_json` rendering of each entry object — so content
/// is measured post-escape, which is what the model actually consumes.
pub const SERIALIZED_BYTE_CAP: usize = 60_000;

/// Soft backstop on the entry count returned by one session read.
///
/// Guards the pathological input the byte cap cannot: a very chatty session
/// where each message is a few bytes, so 60 KB would never fire but ten
/// thousand entries would still arrive.
pub const MESSAGE_CAP: usize = 200;

/// The canonical `truncation_reason` wire values.
///
/// `&'static str` rather than an enum because the value is a wire string the
/// LLM reads and tests assert on; naming them here keeps the three tools from
/// inventing a fourth spelling.
pub mod reason {
    /// The caller's explicit `last_n` cut off older messages.
    pub const EXPLICIT_LAST_N: &str = "explicit_last_n";
    /// [`super::SERIALIZED_BYTE_CAP`] fired.
    pub const BYTE_CAP: &str = "byte_cap";
    /// The message-count backstop fired.
    pub const MESSAGE_CAP: &str = "message_cap";
}

/// Parse the optional `last_n` parameter.
///
/// Returns `Ok(None)` when the key is absent or explicitly `null` — the
/// "return everything, bounded by the caps" path. Returns
/// `Err(InvalidParameters)` for anything that is not a non-negative integer:
/// negative numbers, non-integer floats, strings, booleans, arrays, objects.
///
/// The rejection is the point. The pre-#1028 shape,
/// `params.get("last_n").and_then(|v| v.as_u64()).unwrap_or(20)`, routed every
/// one of those to a silent fallback of 20 — so `{"last_n": -1}` and
/// `{"last_n": "all"}` both returned 20 messages and said nothing about it,
/// and an agent paging deterministically could not tell its request had been
/// discarded. The JSON schema also pins the type, but schema enforcement in
/// the tool-call path is best-effort at the LLM layer, so the runtime
/// validates too.
pub fn parse_last_n(params: &Value) -> SandboxResult<Option<usize>> {
    match params.get("last_n") {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(u) => Ok(Some(u as usize)),
            None => Err(SandboxError::InvalidParameters(format!(
                "'last_n' must be a non-negative integer; got {n}"
            ))),
        },
        Some(other) => Err(SandboxError::InvalidParameters(format!(
            "'last_n' must be a non-negative integer; got {other}"
        ))),
    }
}

/// The outcome of a bounded tail selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The projected entries, in chronological order.
    pub entries: Vec<Value>,
    /// How many items existed to choose from, *after* any caller-side
    /// filtering — so it counts real conversational messages, not markers.
    pub total_count: usize,
    /// Which bound cut the result, or `None` when nothing was omitted.
    pub truncation_reason: Option<&'static str>,
}

impl Selection {
    /// How many entries are in [`Self::entries`].
    #[must_use]
    pub fn returned_count(&self) -> usize {
        self.entries.len()
    }

    /// Whether anything was omitted.
    ///
    /// Coincides with `returned_count() < total_count` by construction —
    /// [`select_recent`] sets a reason exactly when it drops items — so a
    /// consumer may rely on either and they cannot disagree.
    ///
    /// **That coincidence is load-bearing, not incidental.** Reading from the
    /// model's seat rather than the developer's: if a future arm ever dropped
    /// entries without naming a reason, this spelling would report
    /// `truncated: false` with entries missing — which is the #1007 defect
    /// this whole contract exists to remove, an agent believing it has the
    /// whole transcript when it does not. The count-derived spelling would
    /// merely degrade to `truncated: true` with a null reason: the agent
    /// still knows to page, it just does not know why. So the two are not
    /// equally safe to get wrong, and the reason-derived spelling is the
    /// riskier one.
    ///
    /// It is kept because the reason is the *cause* and the flag should not
    /// be able to disagree with it — but an unchecked equivalence expires
    /// silently, so it is asserted rather than assumed:
    /// `truncated_agrees_with_the_counts_on_every_branch`.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.truncation_reason.is_some()
    }

    /// Stamp the four contract fields onto a response object.
    ///
    /// Each tool builds its own response shape — the legacy key names differ
    /// (`message_count` / `showing`, and `fallback_message_count` /
    /// `fallback_showing` on the summary-only path) — but the contract fields
    /// are written from here, so all three spell them identically and none can
    /// ship three of the four.
    ///
    /// Stamping *after* the `json!` literal is safe only because
    /// `serde_json`'s `preserve_order` feature is off, so `Map` is a
    /// `BTreeMap` and the wire order is the keys' sort order regardless of
    /// insertion order. Nothing in this workspace enables it (no feature list
    /// on the `serde_json` dependency, and no `indexmap` in its resolved
    /// dependencies) — but it is a default-off feature that any dependency
    /// could turn on transitively, and no test would catch the resulting
    /// reordering. If it is ever enabled, insert the contract fields into the
    /// `json!` literal instead of after it.
    ///
    /// No-op on a non-object `target`, which cannot happen from the call sites
    /// and is not worth a panic in a tool-response path.
    pub fn write_contract_fields(&self, target: &mut Value) {
        let Some(obj) = target.as_object_mut() else {
            return;
        };
        obj.insert("total_count".into(), Value::from(self.total_count));
        obj.insert("returned_count".into(), Value::from(self.returned_count()));
        obj.insert("truncated".into(), Value::from(self.truncated()));
        obj.insert(
            "truncation_reason".into(),
            match self.truncation_reason {
                Some(r) => Value::from(r),
                None => Value::Null,
            },
        );
    }
}

/// Select the trailing slice of `items` that fits, and project each one.
///
/// Precedence:
///
/// 1. **Explicit `last_n`** — honoured verbatim so agents can page
///    deterministically. When the transcript is longer than `n` the result is
///    still flagged `truncated` with [`reason::EXPLICIT_LAST_N`], so "you
///    asked for 5" and "there are only 5" stay distinguishable.
/// 2. **Otherwise walk newest to oldest**, keeping entries while both caps
///    hold. Whichever fires first names the reason.
///
/// The walk breaks *before* projecting and serializing the rejected tail, so
/// the work is `O(returned_count)` rather than `O(total_count)` — the
/// ten-thousand-message session never materialises a full projection
/// (#1028 P2).
pub fn select_recent<T>(
    items: &[T],
    explicit_last_n: Option<usize>,
    byte_cap: usize,
    message_cap: usize,
    project: impl Fn(&T) -> Value,
) -> Selection {
    let total_count = items.len();

    let (mut acc, truncation_reason): (Vec<Value>, Option<&'static str>) = match explicit_last_n {
        Some(n) => {
            let start = total_count.saturating_sub(n);
            let reason = if start > 0 {
                Some(reason::EXPLICIT_LAST_N)
            } else {
                None
            };
            let mut entries: Vec<Value> = items[start..].iter().map(&project).collect();
            entries.reverse();
            (entries, reason)
        }
        None => {
            let mut entries: Vec<Value> = Vec::new();
            let mut byte_sum: usize = 0;
            let mut reason: Option<&'static str> = None;

            for item in items.iter().rev() {
                if entries.len() >= message_cap {
                    reason = Some(reason::MESSAGE_CAP);
                    break;
                }
                let entry = project(item);
                // Measure the exact wire-bytes of this entry — post-escape,
                // with key wrappers — which is what the model consumes. The
                // serialized string is dropped; only its length is kept, and
                // the response builder re-encodes admitted entries once.
                let entry_bytes = serde_json::to_string(&entry)
                    .map(|s| s.len())
                    .unwrap_or(usize::MAX);
                let next_bytes = byte_sum.saturating_add(entry_bytes);
                if next_bytes > byte_cap {
                    reason = Some(reason::BYTE_CAP);
                    break;
                }
                byte_sum = next_bytes;
                entries.push(entry);
            }
            (entries, reason)
        }
    };

    // Both branches accumulate newest-first; restore chronological order.
    acc.reverse();

    Selection {
        entries: acc,
        total_count,
        truncation_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(n: usize, body: &str) -> Vec<String> {
        (0..n).map(|i| format!("{body}{i}")).collect()
    }

    fn project(s: &String) -> Value {
        serde_json::json!({ "content": s })
    }

    // -- parse_last_n ----------------------------------------------------
    //
    // The claim is "every JSON shape that is not a non-negative integer is
    // rejected", so the rows enumerate the `serde_json::Value` variants
    // rather than the three an author happens to think of. Null is the one
    // non-number that is accepted, and it means "no bound", not "20".

    #[test]
    fn parse_last_n_accepts_absent_and_null_as_no_bound() {
        assert_eq!(parse_last_n(&serde_json::json!({})).unwrap(), None);
        assert_eq!(
            parse_last_n(&serde_json::json!({ "last_n": null })).unwrap(),
            None
        );
    }

    #[test]
    fn parse_last_n_accepts_non_negative_integers_including_zero() {
        assert_eq!(
            parse_last_n(&serde_json::json!({ "last_n": 0 })).unwrap(),
            Some(0)
        );
        assert_eq!(
            parse_last_n(&serde_json::json!({ "last_n": 7 })).unwrap(),
            Some(7)
        );
    }

    #[test]
    fn parse_last_n_rejects_every_other_json_shape() {
        // Derived from the `Value` variants: Bool, String, Array, Object, and
        // the two Number shapes `as_u64` refuses (negative, non-integer).
        // `Null` is covered by the accept row above, so this is the complement
        // of that one and together they range over the whole enum.
        for bad in [
            serde_json::json!(-1),
            serde_json::json!(3.5),
            serde_json::json!("20"),
            serde_json::json!(true),
            serde_json::json!([1]),
            serde_json::json!({ "n": 1 }),
        ] {
            let params = serde_json::json!({ "last_n": bad });
            let err = parse_last_n(&params).expect_err(&format!("{bad} must be rejected"));
            assert!(
                matches!(err, SandboxError::InvalidParameters(_)),
                "{bad} must be InvalidParameters, got {err:?}"
            );
        }
    }

    // -- select_recent ---------------------------------------------------

    #[test]
    fn everything_fits_means_no_truncation() {
        let items = entries(3, "m");
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        assert_eq!(sel.total_count, 3);
        assert_eq!(sel.returned_count(), 3);
        assert!(!sel.truncated());
        assert_eq!(sel.truncation_reason, None);
    }

    #[test]
    fn entries_come_back_in_chronological_order() {
        let items = entries(3, "m");
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        assert_eq!(sel.entries[0]["content"], "m0");
        assert_eq!(sel.entries[2]["content"], "m2");
    }

    #[test]
    fn explicit_last_n_keeps_the_tail_and_flags_it() {
        let items = entries(10, "m");
        let sel = select_recent(&items, Some(3), SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        assert_eq!(sel.total_count, 10);
        assert_eq!(sel.returned_count(), 3);
        assert_eq!(sel.truncation_reason, Some(reason::EXPLICIT_LAST_N));
        assert_eq!(sel.entries[0]["content"], "m7", "the TAIL, not the head");
        assert_eq!(sel.entries[2]["content"], "m9");
    }

    /// The complement that separates "you asked for fewer" from "there are
    /// fewer": an explicit `last_n` at or above the total omits nothing.
    #[test]
    fn explicit_last_n_at_or_above_total_is_not_truncated() {
        let items = entries(3, "m");
        for n in [3, 4, 100] {
            let sel = select_recent(&items, Some(n), SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
            assert_eq!(sel.returned_count(), 3);
            assert_eq!(sel.truncation_reason, None, "last_n={n}");
        }
    }

    #[test]
    fn explicit_zero_returns_nothing_but_says_why() {
        let items = entries(3, "m");
        let sel = select_recent(&items, Some(0), SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        assert_eq!(sel.returned_count(), 0);
        assert_eq!(sel.truncation_reason, Some(reason::EXPLICIT_LAST_N));
    }

    #[test]
    fn message_cap_backstops_a_chatty_session() {
        let items = entries(50, "m");
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, 10, project);
        assert_eq!(sel.total_count, 50);
        assert_eq!(sel.returned_count(), 10);
        assert_eq!(sel.truncation_reason, Some(reason::MESSAGE_CAP));
        assert_eq!(sel.entries[9]["content"], "m49", "the newest are kept");
    }

    #[test]
    fn byte_cap_fires_before_the_message_cap_when_entries_are_large() {
        let items: Vec<String> = (0..10).map(|_| "x".repeat(1000)).collect();
        let sel = select_recent(&items, None, 3_000, MESSAGE_CAP, project);
        assert_eq!(sel.truncation_reason, Some(reason::BYTE_CAP));
        assert!(
            sel.returned_count() < 10 && sel.returned_count() > 0,
            "got {}",
            sel.returned_count()
        );
    }

    /// The #1028 P1 lesson, at the helper level: the cap is measured on the
    /// *serialized* entry, so escapable characters cost what they cost on the
    /// wire. A raw-UTF-8 measurement would admit roughly twice as many of
    /// these, which is the regression this row exists to catch.
    #[test]
    fn byte_cap_counts_json_escape_expansion() {
        // Each `"` serializes to `\"` — one raw byte, two wire bytes.
        let quoted: Vec<String> = (0..10).map(|_| "\"".repeat(500)).collect();
        let plain: Vec<String> = (0..10).map(|_| "x".repeat(500)).collect();

        let quoted_sel = select_recent(&quoted, None, 3_000, MESSAGE_CAP, project);
        let plain_sel = select_recent(&plain, None, 3_000, MESSAGE_CAP, project);

        assert_eq!(quoted_sel.truncation_reason, Some(reason::BYTE_CAP));
        assert!(
            quoted_sel.returned_count() < plain_sel.returned_count(),
            "escaped content must be charged its post-escape size: \
             quoted={} plain={}",
            quoted_sel.returned_count(),
            plain_sel.returned_count()
        );
    }

    /// An explicit `last_n` is honoured verbatim — the caps do not silently
    /// shrink it, or deterministic paging would not be deterministic.
    #[test]
    fn explicit_last_n_is_not_reduced_by_the_caps() {
        let items: Vec<String> = (0..10).map(|_| "x".repeat(1000)).collect();
        let sel = select_recent(&items, Some(10), 100, 1, project);
        assert_eq!(sel.returned_count(), 10);
        assert_eq!(sel.truncation_reason, None);
    }

    #[test]
    fn empty_input_is_not_truncated() {
        let items: Vec<String> = Vec::new();
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        assert_eq!(sel.total_count, 0);
        assert_eq!(sel.returned_count(), 0);
        assert_eq!(sel.truncation_reason, None);
    }

    /// The equivalence [`Selection::truncated`] rests on, asserted rather
    /// than assumed.
    ///
    /// `truncated()` reads the reason; `returned_count() < total_count` reads
    /// the counts. They agree because `select_recent` sets a reason exactly
    /// when it drops items — and the #1032 mutation table declares a mutant
    /// (swapping one spelling for the other) *equivalent on the strength of
    /// that agreement*, so leaving it unchecked would let the declaration
    /// expire silently.
    ///
    /// # Why this is not redundant with the reason-asserting rows
    ///
    /// Seven rows already assert a specific `truncation_reason` for today's
    /// two truncating arms, and they co-kill every mutant this row kills. **The
    /// co-kill is not what earns it** — reading it that way is how this row
    /// gets deleted by a future reader correctly applying the sole-killer
    /// standard.
    ///
    /// What earns it is that its expected value is **computed from the input
    /// rather than stated by the author**. Suppose someone adds a third
    /// truncating arm and forgets to set a reason. They would then write a
    /// reason-asserting row for it saying `truncation_reason: None` — their
    /// honest belief about the code they just wrote — and it would pass,
    /// encoding the bug as the specification. They *cannot* write a passing
    /// case for this row, because there is no expectation here to get wrong:
    /// the expected value derives from `returned_count` and `total_count`,
    /// which the new arm moves whether or not its author understood the
    /// invariant. Same shape as `all_end_reasons` (#1300) and
    /// `expected_registrations` (#1317).
    ///
    /// Stated generally, and worth reusing: **prefer a row whose expected
    /// value is derived from the input over one that restates the author's
    /// belief — the first cannot be satisfied by a wrong author.**
    ///
    /// # Case selection
    ///
    /// Derived from the function's own structure rather than from the
    /// situations that came to mind: both arms of the `explicit_last_n`
    /// match, each cap, and the edges of each — empty input, `n` at zero /
    /// below / equal to / above the total, a cap firing on the very first
    /// item, and an explicit `n` beating both caps.
    ///
    /// Cases that must **fire** a cap pass a literal (`10`, `3_000`, `1`);
    /// cases that must **not** fire one pass the production constants. So
    /// raising `SERIALIZED_BYTE_CAP` later cannot silently un-fire a case that
    /// was here to exercise the truncating branch: the literals hold their
    /// ground while the constants keep tracking the real budget.
    #[test]
    fn truncated_agrees_with_the_counts_on_every_branch() {
        let empty: Vec<String> = Vec::new();
        let few = entries(3, "m");
        let many = entries(50, "m");
        let big: Vec<String> = (0..5).map(|_| "x".repeat(1000)).collect();

        /// (label, items, explicit_last_n, byte_cap, message_cap)
        type Case<'a> = (&'a str, &'a [String], Option<usize>, usize, usize);

        let cases: Vec<Case<'_>> = vec![
            (
                "empty, default",
                &empty,
                None,
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "empty, explicit",
                &empty,
                Some(5),
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "fits, default",
                &few,
                None,
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "explicit zero",
                &few,
                Some(0),
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "explicit below total",
                &few,
                Some(2),
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "explicit equal to total",
                &few,
                Some(3),
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            (
                "explicit above total",
                &few,
                Some(99),
                SERIALIZED_BYTE_CAP,
                MESSAGE_CAP,
            ),
            ("message cap fires", &many, None, SERIALIZED_BYTE_CAP, 10),
            ("message cap of zero", &many, None, SERIALIZED_BYTE_CAP, 0),
            ("byte cap fires", &big, None, 3_000, MESSAGE_CAP),
            (
                "byte cap rejects the first item",
                &big,
                None,
                1,
                MESSAGE_CAP,
            ),
            ("explicit beats both caps", &big, Some(5), 1, 1),
        ];

        for (label, items, explicit, byte_cap, message_cap) in cases {
            let sel = select_recent(items, explicit, byte_cap, message_cap, project);
            assert_eq!(
                sel.truncated(),
                sel.returned_count() < sel.total_count,
                "{label}: the reason-derived flag and the counts must agree \
                 (returned={} total={} reason={:?})",
                sel.returned_count(),
                sel.total_count,
                sel.truncation_reason,
            );
        }
    }

    // -- write_contract_fields -------------------------------------------

    #[test]
    fn contract_fields_are_all_four_and_null_when_untruncated() {
        let items = entries(2, "m");
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, MESSAGE_CAP, project);
        let mut out = serde_json::json!({ "existing": true });
        sel.write_contract_fields(&mut out);

        assert_eq!(out["existing"], true, "existing keys are preserved");
        assert_eq!(out["total_count"], 2);
        assert_eq!(out["returned_count"], 2);
        assert_eq!(out["truncated"], false);
        assert!(
            out["truncation_reason"].is_null(),
            "an untruncated read reports null, not a missing key"
        );
    }

    #[test]
    fn contract_fields_carry_the_reason_when_truncated() {
        let items = entries(50, "m");
        let sel = select_recent(&items, None, SERIALIZED_BYTE_CAP, 10, project);
        let mut out = serde_json::json!({});
        sel.write_contract_fields(&mut out);

        assert_eq!(out["total_count"], 50);
        assert_eq!(out["returned_count"], 10);
        assert_eq!(out["truncated"], true);
        assert_eq!(out["truncation_reason"], "message_cap");
    }
}

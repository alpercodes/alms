// Agent-name normalization for the agent-create form.
//
// The backend `validate_agent_name` accepts ASCII alphanumerics + hyphens,
// with no leading/trailing hyphens (see crates/alms-core/src/registry.rs).
// The form input is lenient — operators can type "Research Bot" and the front
// end normalizes to a name-safe shape on submit, with a live preview shown
// below the input so they know what will actually be created.
//
// **Case is preserved** (#2). Uppercase is admissible on the backend, so an
// operator who types "Atlas" gets an agent named "Atlas". Lowercasing here
// would silently rewrite their choice — and, because the backend enforces
// uniqueness case-insensitively, "Atlas" cannot collide with an existing
// "atlas" either way; it is refused with a 409, not quietly merged.
//
// Normalization rules (kept deliberately simple — no clever camelCase splitting):
//   1. Replace any run of whitespace with a single hyphen.
//   2. Strip every character that isn't [A-Za-z0-9-].
//   3. Collapse runs of consecutive hyphens into a single hyphen.
//   4. Trim leading and trailing hyphens.
//
// Examples:
//   "Atlas"          -> "Atlas"      (case survives — #2)
//   "ResearchBot"    -> "ResearchBot"
//   "Research Bot"   -> "Research-Bot"
//   "  Foo Bar  "    -> "Foo-Bar"
//   "foo--bar"       -> "foo-bar"
//   "-foo-"          -> "foo"
//   "!!!"            -> ""           (caller surfaces "name required")
//   "Foo_Bar.42"     -> "FooBar42"   (underscores + dots are stripped, not converted)
export function normalizeAgentName(raw) {
    if (raw == null) return '';
    return String(raw)
        .replace(/\s+/g, '-')
        .replace(/[^A-Za-z0-9-]/g, '')
        .replace(/-+/g, '-')
        .replace(/^-+|-+$/g, '');
}

// Reserved agent names — must stay in lock-step with `RESERVED_AGENT_NAMES`
// in `crates/alms-core/src/registry.rs`. These collide with API sub-route
// segments and internal prefixes, and the backend rejects them with a 400.
// We mirror the check client-side so typing `Default` / `DM` / `Workspace`
// surfaces an inline error instead of leaking the raw backend error message.
//
// Compared case-insensitively (#2): normalization no longer lowercases, so
// an exact-match check would let `DM` through the client-side mirror and
// leak the backend 400.
const RESERVED_NAMES = ['default', 'dm', 'workspace'];

// UUID shape — backend resolves agents by UUID first, so a UUID-shaped
// name would be unreachable by name. Mirrors the `uuid::Uuid::parse_str`
// check in `validate_agent_name`, which accepts both the hyphenated
// 8-4-4-4-12 form *and* the simple 32-char hex form. Both pass through
// `normalizeAgentName` unchanged, so we have to catch them here.
// (urn:/braced forms contain characters outside the name grammar and don't
// survive normalization, so we don't need to mirror those shapes.)
//
// Case-insensitive (#2): `Uuid::parse_str` accepts uppercase hex, and since
// normalization no longer lowercases, an uppercase-hex UUID now reaches this
// check with its case intact.
const UUID_RE =
    /^([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|[0-9a-f]{32})$/i;

// Maximum name length (matches `validate_agent_name` upper bound).
const MAX_NAME_LEN = 64;

/**
 * Validate a *normalized* agent-name slug against the backend's
 * `validate_agent_name` rules that survive the normalization step.
 *
 * Mirrors the rules in `crates/alms-core/src/registry.rs` that aren't
 * already enforced by `normalizeAgentName`:
 *
 *   - Length: must be 1..=64 chars (empty is the caller's responsibility,
 *     handled separately as the "name required" / "must contain at least
 *     one letter or digit" branches; we only flag the >64 case here).
 *   - Reserved names: `default`, `dm`, `workspace` (case-insensitive).
 *   - UUID-shaped names: `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` hex form
 *     (case-insensitive).
 *
 * Rules that are *already* enforced by normalization and don't need to
 * be re-checked here:
 *   - ASCII alphanumerics + hyphens (the strip step).
 *   - No leading/trailing hyphens (the trim step).
 *
 * Returns `null` when the slug is valid, or `{ code, message }` when it
 * isn't. The caller surfaces `message` in the same inline-error UI as
 * the empty / all-invalid paths.
 *
 * @param {string} slug — output of `normalizeAgentName`
 * @returns {null | { code: string, message: string }}
 */
export function validateNormalizedAgentName(slug) {
    if (typeof slug !== 'string' || slug.length === 0) {
        // The empty / all-invalid paths are handled by the caller before
        // this function is reached. Defensive return so a stray call
        // doesn't surface a confusing length error.
        return null;
    }
    if (slug.length > MAX_NAME_LEN) {
        return {
            code: 'AGENT_NAME_TOO_LONG',
            message: `Agent name is too long (max ${MAX_NAME_LEN} characters after normalization)`,
        };
    }
    if (RESERVED_NAMES.includes(slug.toLowerCase())) {
        return {
            code: 'AGENT_NAME_RESERVED',
            message: `'${slug}' is a reserved name`,
        };
    }
    if (UUID_RE.test(slug)) {
        return {
            code: 'AGENT_NAME_LOOKS_LIKE_UUID',
            message: `'${slug}' looks like a UUID (conflicts with ID-based lookup)`,
        };
    }
    return null;
}

/**
 * Case-insensitive agent-name equality — the JS mirror of the
 * `eq_ignore_ascii_case` comparisons the Rust side uses on agent names.
 *
 * Agent names resolve case-insensitively (#2), so `Atlas` and `atlas` are the
 * same agent, and any UI comparison deciding *identity* has to fold case.
 * Names are `[A-Za-z0-9-]`, so `toLowerCase()` is exact here — no locale or
 * Unicode-folding subtlety applies.
 *
 * Non-string inputs are `false` rather than a throw: callers pass
 * `activeAgent.value?.name`, which is legitimately undefined before boot
 * resolves.
 *
 * @param {unknown} a
 * @param {unknown} b
 * @returns {boolean}
 */
export function agentNamesEqual(a, b) {
    if (typeof a !== 'string' || typeof b !== 'string') return false;
    return a.toLowerCase() === b.toLowerCase();
}

/**
 * Extract the peer agent name from a DM `context_id` — the JS mirror of
 * `alms_core::dm_peer`.
 *
 * DM context IDs have the form `dm:{name1}:{name2}`. The peer is whichever
 * participant is NOT `agentName`, compared case-insensitively.
 *
 * Returns `null` when the string isn't a DM context_id **or when neither
 * participant is `agentName`**. That second case is why this is a function
 * rather than a ternary at the call site. The shape it replaces —
 *
 *     const peer = parts[1] === agentName ? parts[2] : parts[1];
 *
 * — treats "did not match" as "therefore it is the peer", so a name that is
 * not a participant at all silently yields the *first* participant. Once
 * names can differ by case that misfire becomes reachable, and it renders as
 * "Chatting with Atlas" shown to the operator who is themselves Atlas.
 * Returning `null` forces the caller to handle "not my DM" explicitly.
 *
 * The returned spelling is the one stored in the context_id — canonical,
 * because context_ids are minted from registry records.
 *
 * @param {unknown} contextId
 * @param {unknown} agentName
 * @returns {string|null}
 */
export function dmPeerName(contextId, agentName) {
    if (typeof contextId !== 'string' || typeof agentName !== 'string') return null;
    const parts = contextId.split(':');
    if (parts.length < 3 || parts[0] !== 'dm') return null;
    const [, first, second] = parts;
    if (agentNamesEqual(first, agentName)) return second || null;
    if (agentNamesEqual(second, agentName)) return first || null;
    return null;
}

/**
 * Pick the DM peer out of a `participants` array (the envelope-carried
 * counterpart to {@link dmPeerName}, which parses a `context_id`).
 *
 * Same case-folding rule, same explicit-null contract: `null` when
 * `agentName` is not among the participants, rather than the
 * `.find(p => p !== agentName)` shape that returns the active agent's own
 * name the moment the comparison misses.
 *
 * When `agentName` is absent (boot hasn't resolved the active agent yet) the
 * first participant is returned — this preserves the pre-existing fallback,
 * which is a display-only best guess and not an identity decision.
 *
 * @param {unknown} participants
 * @param {unknown} agentName
 * @returns {string|null}
 */
export function dmPeerFromParticipants(participants, agentName) {
    if (!Array.isArray(participants)) return null;
    if (typeof agentName !== 'string' || agentName === '') {
        return participants[0] || null;
    }
    if (!participants.some(p => agentNamesEqual(p, agentName))) return null;
    return participants.find(p => !agentNamesEqual(p, agentName)) || null;
}

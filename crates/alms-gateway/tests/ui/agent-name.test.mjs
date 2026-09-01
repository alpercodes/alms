// Pinned regression for issue #978: the agent-create form normalizes the
// operator's input to a slug-safe shape before POSTing to the backend, so
// "ResearchBot" / "Research Bot" / etc. don't surface a 400 from
// `validate_agent_name`. This test exercises the pure-function normalizer
// in `static/ui/utils/agent-name.js`.
//
// The component-level UX (live preview line, error path on empty
// normalization) is one layer above and isn't covered here — those depend
// on Preact + signals, which aren't vendored into the Node test runtime.
// The normalizer is the load-bearing piece, and the rules it enforces
// must stay in lock-step with `crates/alms-core/src/registry.rs ::
// validate_agent_name` (ASCII alphanumeric + hyphens, no leading or
// trailing hyphens). The cases below were chosen to pin every rule the
// normalizer applies plus the empty-string boundary the caller relies on
// to surface the "name required" inline error.
//
// Issue #2 widened the backend class to admit uppercase, so the normalizer
// no longer lowercases: the operator's capitalization IS part of the name.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import path from 'node:path';
import url from 'node:url';

const __filename = url.fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const NAME_UTIL_PATH = path.resolve(
    __dirname,
    '../../static/ui/utils/agent-name.js'
);

const {
    normalizeAgentName,
    validateNormalizedAgentName,
    agentNamesEqual,
    dmPeerName,
    dmPeerFromParticipants,
} = await import(url.pathToFileURL(NAME_UTIL_PATH).href);

test('#978: normalizer is exported as a function', () => {
    assert.equal(typeof normalizeAgentName, 'function');
});

test('#978: lowercase + alphanumeric input passes through unchanged', () => {
    // The slug rules already match the validator — no normalization needed.
    assert.equal(normalizeAgentName('researchbot'), 'researchbot');
    assert.equal(normalizeAgentName('foo-bar'), 'foo-bar');
    assert.equal(normalizeAgentName('agent-42'), 'agent-42');
});

test('#2: uppercase input survives normalization untouched', () => {
    // The headline case for issue #2: an operator who types "Atlas" must
    // get an agent named "Atlas". Lowercasing here would silently rewrite
    // their choice, and they would never see it happen — the preview line
    // only renders when normalization actually changed something.
    assert.equal(normalizeAgentName('Atlas'), 'Atlas');
    assert.equal(normalizeAgentName('ResearchBot'), 'ResearchBot');
    assert.equal(normalizeAgentName('FOO'), 'FOO');
    assert.equal(normalizeAgentName('FooBar42'), 'FooBar42');
});

test('#978: whitespace runs collapse to a single hyphen', () => {
    // "Research Bot" is the second example in the issue — pick the
    // simplest rule that preserves word boundaries.
    assert.equal(normalizeAgentName('Research Bot'), 'Research-Bot');
    assert.equal(normalizeAgentName('a   b'), 'a-b');
    assert.equal(normalizeAgentName('a\tb'), 'a-b');
    assert.equal(normalizeAgentName('a\nb'), 'a-b');
});

test('#978: leading and trailing whitespace is trimmed', () => {
    // After collapsing whitespace to "-" and stripping non-slug chars,
    // we trim leading/trailing hyphens so the result satisfies the
    // backend's "no leading/trailing hyphen" rule.
    assert.equal(normalizeAgentName('  foo  '), 'foo');
    assert.equal(normalizeAgentName(' Research Bot '), 'Research-Bot');
});

test('#978: non-slug characters are stripped (not converted)', () => {
    // Underscores, dots, slashes, etc. are not part of the slug grammar.
    // Stripping is the simplest predictable rule — the operator sees the
    // result in the live preview before clicking Create.
    assert.equal(normalizeAgentName('foo_bar'), 'foobar');
    assert.equal(normalizeAgentName('foo.bar'), 'foobar');
    assert.equal(normalizeAgentName('foo/bar'), 'foobar');
    assert.equal(normalizeAgentName('foo!bar?'), 'foobar');
    // Unicode letters are out of scope for the backend name grammar — #2
    // widened it to `[A-Za-z0-9-]`, not to "any letter". The normalizer
    // strips them (consistent with the "non-name chars are stripped" rule
    // above) rather than transliterating.
    assert.equal(normalizeAgentName('café'), 'caf');
    assert.equal(normalizeAgentName('Ünicode'), 'nicode');
});

test('#978: consecutive hyphens collapse to one', () => {
    // Both operator-typed "--" and the post-strip residue (e.g. " _ "
    // that becomes "-_-" -> "-" after stripping the underscore) need
    // to collapse so the result satisfies the backend rules.
    assert.equal(normalizeAgentName('foo--bar'), 'foo-bar');
    assert.equal(normalizeAgentName('foo---bar'), 'foo-bar');
    assert.equal(normalizeAgentName('foo - bar'), 'foo-bar');
});

test('#978: leading/trailing hyphens are trimmed', () => {
    // The backend rejects names that start or end with a hyphen — see
    // `validate_agent_name` in registry.rs. The normalizer trims them
    // so the operator never sees that 400 from a normalized input.
    assert.equal(normalizeAgentName('-foo'), 'foo');
    assert.equal(normalizeAgentName('foo-'), 'foo');
    assert.equal(normalizeAgentName('-foo-'), 'foo');
    assert.equal(normalizeAgentName('---foo---'), 'foo');
});

test('#978: empty input returns empty string', () => {
    // The caller treats "" as the trigger for the "name required"
    // inline error path — don't submit garbage to the backend.
    assert.equal(normalizeAgentName(''), '');
    assert.equal(normalizeAgentName('   '), '');
    assert.equal(normalizeAgentName('\t\n'), '');
});

test('#978: all-invalid input returns empty string', () => {
    // "!!!" / "___" / unicode-only strings normalize to "" and the
    // caller surfaces the "name must contain at least one letter or
    // digit" branch rather than POSTing an empty name.
    assert.equal(normalizeAgentName('!!!'), '');
    assert.equal(normalizeAgentName('___'), '');
    assert.equal(normalizeAgentName('...'), '');
    assert.equal(normalizeAgentName('---'), '');
    assert.equal(normalizeAgentName('αβγ'), '');
});

test('#978: null / undefined are handled defensively', () => {
    // The component never passes null today (`useSignal('')` initializes
    // the string), but a defensive guard keeps the helper safe to reuse
    // from contexts that might (e.g. URL-driven prefill, paste events).
    assert.equal(normalizeAgentName(null), '');
    assert.equal(normalizeAgentName(undefined), '');
});

test('#978: digit-only input survives normalization', () => {
    // The backend slug grammar permits digits — "42" is a legal agent
    // name, even if unusual. Don't strip it.
    assert.equal(normalizeAgentName('42'), '42');
    assert.equal(normalizeAgentName('Agent42'), 'Agent42');
});

test('#2: normalization is a no-op for names an operator would actually type', () => {
    // The component hides the preview line when normalize(raw) === raw. If
    // any of these changed, an operator typing a perfectly legal name would
    // see a "will be created as..." nag for no reason — and, worse, would
    // get an agent named something they didn't choose.
    for (const name of ['Atlas', 'ResearchBot', 'Agent-V2', 'ATLAS', 'a1B2c3']) {
        assert.equal(normalizeAgentName(name), name);
        assert.equal(validateNormalizedAgentName(name), null);
    }
});

test('#978: realistic user inputs from the issue body', () => {
    // The two cases the issue body explicitly calls out. Lock them in.
    assert.equal(normalizeAgentName('ResearchBot'), 'ResearchBot');
    assert.equal(normalizeAgentName('Research Bot'), 'Research-Bot');
});

// Below: post-normalization validation tests. These mirror the rules in
// `crates/alms-core/src/registry.rs::validate_agent_name` that survive
// the normalization step (length cap, reserved names, UUID shape) so
// that all rejection paths surface as polished inline errors instead of
// raw backend 400s. Tim's PR #999 review item.

test('#999: validateNormalizedAgentName is exported as a function', () => {
    assert.equal(typeof validateNormalizedAgentName, 'function');
});

test('#999: validator returns null for valid names', () => {
    // Sanity — the happy path that the existing tests already exercise
    // through the normalizer. The validator must not flag any of them.
    assert.equal(validateNormalizedAgentName('researchbot'), null);
    assert.equal(validateNormalizedAgentName('ResearchBot'), null);
    assert.equal(validateNormalizedAgentName('research-bot'), null);
    assert.equal(validateNormalizedAgentName('agent-42'), null);
    assert.equal(validateNormalizedAgentName('a'), null);
    assert.equal(validateNormalizedAgentName('a1b2c3'), null);
    // Max length boundary — exactly 64 chars is OK (mirrors the
    // backend's `test_max_length_ok` fixture).
    assert.equal(validateNormalizedAgentName('a'.repeat(64)), null);
});

test('#999: validator returns null for empty input (caller handles)', () => {
    // The empty / all-invalid paths are handled in the component before
    // the validator is reached. Defensive return so a stray call doesn't
    // surface a misleading length error.
    assert.equal(validateNormalizedAgentName(''), null);
});

test('#999: validator flags names longer than 64 chars', () => {
    // Mirrors `test_invalid_too_long` in registry.rs — a 65-char slug
    // is the boundary case the backend rejects with InvalidConfig.
    const slug65 = 'a'.repeat(65);
    const result = validateNormalizedAgentName(slug65);
    assert.notEqual(result, null);
    assert.equal(result.code, 'AGENT_NAME_TOO_LONG');
    assert.match(result.message, /too long/i);
    assert.match(result.message, /64/);

    // Pasting a 200-char string (Tim's example) — still flagged.
    const slug200 = 'a'.repeat(200);
    const result200 = validateNormalizedAgentName(slug200);
    assert.notEqual(result200, null);
    assert.equal(result200.code, 'AGENT_NAME_TOO_LONG');
});

test('#999: validator flags reserved names (post-normalization)', () => {
    // Mirrors `test_reserved_names` in registry.rs — these collide
    // with API sub-route segments and internal prefixes. Apply the
    // check after normalization so typing `Default` / `DM` / `Workspace`
    // (which all normalize to a reserved value) is still caught.
    for (const reserved of ['default', 'dm', 'workspace']) {
        const result = validateNormalizedAgentName(reserved);
        assert.notEqual(result, null, `expected ${reserved} to be flagged`);
        assert.equal(result.code, 'AGENT_NAME_RESERVED');
        assert.match(result.message, /reserved/i);
        assert.match(result.message, new RegExp(reserved));
    }
});

test('#2: reserved-name check is case-insensitive, not normalization-dependent', () => {
    // Before #2, `DM` was caught only because normalization lowercased it
    // into the reserved list. Normalization no longer lowercases, so the
    // check itself has to fold case — otherwise `DM` sails past the client
    // mirror and the operator gets a raw backend 400 instead of the
    // polished inline error this check exists to give them.
    const cases = [
        ['Default', 'Default'],
        ['DEFAULT', 'DEFAULT'],
        ['DM', 'DM'],
        ['Dm', 'Dm'],
        ['dM', 'dM'],
        ['Workspace', 'Workspace'],
        ['WorkSpace', 'WorkSpace'],
        ['  workspace  ', 'workspace'],
    ];
    for (const [input, expectedName] of cases) {
        const name = normalizeAgentName(input);
        assert.equal(name, expectedName, `normalize(${input})`);
        const result = validateNormalizedAgentName(name);
        assert.notEqual(result, null, `expected ${input} to be flagged`);
        assert.equal(result.code, 'AGENT_NAME_RESERVED');
    }
});

test('#999: validator flags UUID-shaped names', () => {
    // Mirrors `test_uuid_shaped_name_rejected` in registry.rs — the
    // backend resolves agents by UUID first, so a UUID-shaped name
    // would be unreachable by name. Vanishingly unlikely to type by
    // accident, but we mirror the rule for completeness so the raw
    // backend 400 message never leaks.
    const cases = [
        'a1b2c3d4-e5f6-7890-abcd-ef1234567890',
        '550e8400-e29b-41d4-a716-446655440000',
        '00000000-0000-0000-0000-000000000000',
    ];
    for (const uuid of cases) {
        const result = validateNormalizedAgentName(uuid);
        assert.notEqual(result, null, `expected ${uuid} to be flagged`);
        assert.equal(result.code, 'AGENT_NAME_LOOKS_LIKE_UUID');
        assert.match(result.message, /uuid/i);
    }
});

test('#999: validator flags simple-form (32-char hex) UUIDs', () => {
    // `uuid::Uuid::parse_str` accepts the simple 32-char hex form too,
    // not just the hyphenated 8-4-4-4-12 form. A pure-lowercase-hex
    // input (e.g. `a1b2c3d4e5f67890abcdef1234567890`) survives
    // normalization untouched and the backend rejects it — so the
    // client must mirror that rule. Tim's PR #999 re-review item.
    const cases = [
        'a1b2c3d4e5f67890abcdef1234567890',
        '550e8400e29b41d4a716446655440000',
        '00000000000000000000000000000000',
    ];
    for (const uuid of cases) {
        const result = validateNormalizedAgentName(uuid);
        assert.notEqual(result, null, `expected ${uuid} to be flagged`);
        assert.equal(result.code, 'AGENT_NAME_LOOKS_LIKE_UUID');
        assert.match(result.message, /uuid/i);
    }
});

test('#2: uppercase-hex UUIDs reach the validator with their case intact', () => {
    // Before #2 an uppercase-hex UUID was lowercased by normalization into
    // the lowercase-only regex. It now survives untouched, so the regex
    // itself has to be case-insensitive — `uuid::Uuid::parse_str` is, and
    // the backend rejects every casing. Pin that normalization is now the
    // identity here, for both the hyphenated and simple 32-char forms.
    const hyphenName = normalizeAgentName('A1B2C3D4-E5F6-7890-ABCD-EF1234567890');
    assert.equal(hyphenName, 'A1B2C3D4-E5F6-7890-ABCD-EF1234567890');
    const hyphenResult = validateNormalizedAgentName(hyphenName);
    assert.notEqual(hyphenResult, null);
    assert.equal(hyphenResult.code, 'AGENT_NAME_LOOKS_LIKE_UUID');

    const simpleName = normalizeAgentName('A1B2C3D4E5F67890ABCDEF1234567890');
    assert.equal(simpleName, 'A1B2C3D4E5F67890ABCDEF1234567890');
    const simpleResult = validateNormalizedAgentName(simpleName);
    assert.notEqual(simpleResult, null);
    assert.equal(simpleResult.code, 'AGENT_NAME_LOOKS_LIKE_UUID');

    // Mixed case too — the regex must not be anchored to a single casing.
    const mixed = validateNormalizedAgentName('a1B2c3D4-e5F6-7890-AbCd-eF1234567890');
    assert.notEqual(mixed, null);
    assert.equal(mixed.code, 'AGENT_NAME_LOOKS_LIKE_UUID');
});

test('#999: hex-but-not-UUID-shaped names are not flagged', () => {
    // Hex strings that don't match either the 8-4-4-4-12 grouping or
    // the 32-char simple form are fine. Cover the wrong-length cases
    // (31, 33), a non-hex char inside a 32-char string, and partial
    // hyphenated shapes.
    assert.equal(validateNormalizedAgentName('a1b2c3d4e5f67890abcdef123456789'), null); // 31 chars
    assert.equal(validateNormalizedAgentName('a1b2c3d4e5f67890abcdef12345678901'), null); // 33 chars
    assert.equal(validateNormalizedAgentName('a1b2-c3d4-e5f6-7890'), null);
    assert.equal(validateNormalizedAgentName('agent-v2'), null);
});

// ---------------------------------------------------------------------------
// #2: the JS mirrors of the Rust participant-matching rules.
//
// `dm_peer` was made the single participant-matching rule on the Rust side
// precisely so a second copy could not drift. These are the JS copies of the
// same rule; they exist because the UI derives the "Chatting with {peer}"
// label without a round trip, and they are pinned here so the drift the Rust
// consolidation prevents can't reappear on this side of the wire.
// ---------------------------------------------------------------------------

test('#2: agentNamesEqual folds case and rejects non-strings', () => {
    assert.equal(agentNamesEqual('Atlas', 'atlas'), true);
    assert.equal(agentNamesEqual('ATLAS', 'aTlAs'), true);
    assert.equal(agentNamesEqual('Atlas', 'Atlas'), true);
    assert.equal(agentNamesEqual('atlas', 'atlas-2'), false);
    assert.equal(agentNamesEqual('atlas', 'bob'), false);

    // Callers pass `activeAgent.value?.name`, undefined before boot resolves.
    assert.equal(agentNamesEqual(undefined, 'atlas'), false);
    assert.equal(agentNamesEqual('atlas', null), false);
    assert.equal(agentNamesEqual(undefined, undefined), false);
});

test('#2: dmPeerName folds case, mirroring alms_core::dm_peer', () => {
    // Same cases as the Rust doctest on `dm_peer`.
    assert.equal(dmPeerName('dm:alice:bob', 'alice'), 'bob');
    assert.equal(dmPeerName('dm:alice:bob', 'bob'), 'alice');
    assert.equal(dmPeerName('dm:Atlas:bob', 'atlas'), 'bob');
    assert.equal(dmPeerName('dm:atlas:Bob', 'BOB'), 'atlas');

    // The returned spelling is the context_id's (canonical), not the query's.
    assert.equal(dmPeerName('dm:Atlas:Bob', 'atlas'), 'Bob');
});

test('#2: dmPeerName returns null rather than guessing a peer', () => {
    // THE regression this helper exists for. The ternary it replaced --
    //     parts[1] === agentName ? parts[2] : parts[1]
    // -- read "did not match" as "therefore it is the peer", so a
    // non-participant yielded the FIRST participant. With case-varying names
    // that misfires into telling Atlas they are "Chatting with Atlas".
    assert.equal(dmPeerName('dm:alice:bob', 'charlie'), null);
    assert.equal(dmPeerName('dm:Atlas:bob', 'carol'), null);

    // Non-DM / malformed contexts.
    assert.equal(dmPeerName('web-chat-123', 'alice'), null);
    assert.equal(dmPeerName('dm:alice', 'alice'), null);
    assert.equal(dmPeerName('', 'alice'), null);
    assert.equal(dmPeerName(null, 'alice'), null);
    assert.equal(dmPeerName('dm:alice:bob', undefined), null);
});

test('#2: dmPeerFromParticipants folds case and never returns the self name', () => {
    assert.equal(dmPeerFromParticipants(['Atlas', 'bob'], 'atlas'), 'bob');
    assert.equal(dmPeerFromParticipants(['atlas', 'Bob'], 'BOB'), 'atlas');
    assert.equal(dmPeerFromParticipants(['alice', 'bob'], 'alice'), 'bob');

    // Not a participant -> null, NOT participants[0]. This is the
    // `.find(p => p !== agentName)` misfire, which returned the active
    // agent's own name whenever the comparison missed.
    assert.equal(dmPeerFromParticipants(['alice', 'bob'], 'charlie'), null);

    // No active agent yet: the pre-existing display-only fallback is kept.
    assert.equal(dmPeerFromParticipants(['alice', 'bob'], undefined), 'alice');
    assert.equal(dmPeerFromParticipants(['alice', 'bob'], ''), 'alice');

    assert.equal(dmPeerFromParticipants(null, 'alice'), null);
    assert.equal(dmPeerFromParticipants(undefined, 'alice'), null);
});

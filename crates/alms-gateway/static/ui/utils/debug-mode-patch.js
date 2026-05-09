/**
 * Debug-mode PATCH delta helper (#1003).
 *
 * Lifted out of `agents-tab.js` into a standalone pure-JS module so
 * unit tests can import it under Node without dragging in the preact
 * `deps.js` chain. Used by both the `AgentEditModal` and the
 * `SettingsModal` Debug section to produce a `PATCH /agents/{id}`
 * partial body with `debug_mode` included only when the form value
 * differs from the live record.
 *
 * Why a dedicated module instead of inlining the diff: the wire shape
 * has subtle "undefined vs false" semantics (pre-#1003 agent records
 * carry `debug_mode === undefined` because the field was absent from
 * the response; both surfaces must treat that as `false` when
 * comparing against the form state, otherwise opening the modal on a
 * legacy record and pressing Apply would always PATCH). A single
 * well-tested helper means both surfaces can never drift out of sync.
 *
 * @param {boolean|null|undefined} stored — `agent.debug_mode` from
 *   the live record. Pre-#1003 records have this `undefined`;
 *   coerced through `!!` so "undefined vs false" is invisible to the
 *   diff (both legitimately mean "not enabled").
 * @param {boolean} draft — current form state.
 * @returns {{debug_mode?: boolean}} a partial PATCH body — empty
 *   object when the form value matches the stored value.
 */
export function debugModePatchDelta(stored, draft) {
    const cur = !!stored;
    const next = !!draft;
    if (cur === next) return {};
    return { debug_mode: next };
}

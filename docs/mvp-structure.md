# MVP Module vs Crate Structure (Decision)

**Status:** Decided for MVP

## Goal
Reduce integration friction by keeping the crate graph small **while preserving clean internal seams** for later extraction.

## Decision (MVP)
Target **3–4 crates** during MVP:
1) **`alms-core`** — shared types/protocol/errors (stable)
2) **`almsd`** (new crate) — single daemon crate that contains:
   - gateway HTTP server
   - runtime loop
   - session storage
   - scheduler/cron
   - tool registry + sandbox integration
   - audit/event logging
3) **`alms-cli`** — thin wrapper around `almsd`
4) **`alms-channel`** — optional, only if it doesn’t add wiring complexity

**Everything else becomes internal modules** inside `almsd` for MVP.

## Why
- Fewer crates = fewer dependency cycles + simpler wiring.
- Faster iteration during MVP.
- Still preserves clear internal boundaries so extraction later is easy.

## Migration Plan (from current repo)
1) Introduce `almsd` and move gateway/runtime/session/scheduler/tools into modules.
2) Keep `alms-core` as a shared crate.
3) Keep `alms-cli` thin; update entrypoints to call `almsd`.
4) Defer splitting `alms-sandbox` into its own crate until ABI stabilizes.

## Post‑MVP
Once the MVP is stable, split modules into crates as needed:
- `alms-gateway`, `alms-runtime`, `alms-session`, `alms-scheduler`, `alms-sandbox`

---
*Date: 2026-02-11*

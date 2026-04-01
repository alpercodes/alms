# Tech Stack Proposal (Expanded) — ALMS

This proposal expands on `tech-stack.md` with project‑specific constraints, measurable assumptions, and a clearer decision record.

## Executive Summary
Rust remains the primary stack choice for ALMS **if** we commit to:
- predictable latency (no GC pauses in agent loop)
- memory‑safety and sandboxing as core security guarantees
- WASM‑based tool sandbox as a first‑class feature

However, the decision needs explicit constraints and operational tradeoffs to be durable.

---

## Project Constraints (Make These Explicit)
**Latency & Reliability**
- Agent loop should not experience GC‑induced tail latency.
- Target: p95 response time within a defined budget (TBD, e.g., < 1s for tool‑less responses).

**Security**
- Untrusted tool execution must be sandboxed (WASM or equivalent).
- Memory safety is a requirement, not a nice‑to‑have.

**Concurrency**
- Parallel subagent execution is a core feature.
- Must safely isolate failures and resource usage.

**Operational Reality**
- Build speed, CI times, deploy pipelines need to be managed.
- Onboarding new engineers must be tractable.

---

## Rust vs Go — Expanded Analysis

### Rust: Strengths (Project‑Specific)
- **Tail‑latency control** (no GC pauses) during agent loop.
- **Memory safety** for core runtime + sandbox boundary.
- **WASM tool ecosystem** is strongest in Rust (wasmtime is production‑grade).
- **Fine‑grained concurrency** with explicit ownership: less hidden shared‑state bugs.

### Rust: Costs / Risks
- **Steeper learning curve** (slows onboarding, higher review overhead).
- **Longer compile times** (affects CI + dev iteration speed).
- **Complexity debt** if codebase grows without discipline.

### Go: Strengths (Project‑Specific)
- **Developer velocity** for API/services/CLI.
- **Simple concurrency** for straightforward service orchestration.
- **Fast build/test cycles**, easier onboarding.

### Go: Costs / Risks
- **GC latency risk** in performance‑critical loops.
- **Weaker WASM sandbox story** compared to Rust.
- **Memory safety** relies on discipline rather than compiler guarantees.

---

## Decision Logic (What Must Be True)
**Rust is the right primary stack IF:**
1. Agent loop latency targets are strict.
2. Tool sandbox is a flagship capability.
3. Security posture requires memory‑safe core.

**Go is viable IF:**
1. Performance targets are relaxed.
2. Tool execution is not sandbox‑critical.
3. Speed of feature delivery outweighs long‑term safety.

---

## Proposed Hybrid Approach (When It’s Worth It)
- **Rust for core runtime** (gateway, session manager, agent loop, sandbox).
- **Go or Rust for CLI** — choose based on who builds it and time constraints.

**Only adopt hybrid** if:
- We need faster iteration on CLI/SDK than Rust allows.
- The integration surface is stable (avoid multi‑language churn early).

---

## Concrete Performance & Ops Assumptions (TBD)
- p95 response time target: ____ ms
- p99 response time target: ____ ms
- Max concurrent subagents: ____
- Max tool execution time: ____
- CI build time target: ____ minutes

---

## Recommendation (Revised)
**Primary stack: Rust**
- Justified by sandbox + memory safety + latency control.
- Must explicitly manage onboarding and CI/build time cost.

---

## Next Steps
1. Set explicit latency/throughput goals.
2. Decide if WASM sandbox is MVP or Phase 2.
3. Capture this as an ADR once goals are finalized.

---
*Date: 2026-02-10*

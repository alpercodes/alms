# Next Steps — 2026-03-12

Post-P10 review cleanup and next priorities. Work through one by one.

---

## Quick cleanup pass

### 1. ~~Fix fragile duplicate name detection~~ **DONE**
- Replaced `msg.contains("UNIQUE")` with typed `AlmsError::DuplicateName` matching `rusqlite::ErrorCode::ConstraintViolation`.

### 2. ~~Extract and test config merging logic~~ **DONE**
- Extracted `apply_overrides()` pure function with 9 unit tests covering all precedence layers, clamping, and edge cases.

### 3. ~~Deprecate or remove legacy `POST /agent/run` endpoint~~ **DONE**
- Removed both `POST /agent/run` and `POST /agent/run/stream`. All callers use canonical `POST /runs`.

---

## After cleanup — choose direction

### 4. P11: Telegram Adapter Rework (if Telegram is actively used)
- 8 tasks (#56-#63) in TASKS.md
- Critical: stop signal never reaches polling task (#56)
- Start with #56 (shutdown fix), then #57-#63 in order
- Only worth doing if Telegram adapter is needed now

### 5. Multi-turn orchestration (if goal is platform capability)
- The "Not yet real" item in CLAUDE.md
- Coordinator currently spawns single-turn subagents only
- Need: task decomposition, multi-turn subagent loops, result aggregation
- Biggest remaining gap before the system is a real multi-agent platform

# Agent UX Requirements (from Alper)

Lessons learned from OpenClaw frustrations. These are non-negotiable for ALMS.

---

## 1) Configuration must be simple and predictable

**Problem in OpenClaw:** Setting up config/settings is confusing. `rate_limit`, `auth.cooldown`, and similar settings are hard to understand, don't seem to work correctly, and have unclear interactions.

**ALMS requirement:**
- Config should be a single, well-documented file with sane defaults
- Every setting must have a clear description of what it does and what values are valid
- Settings must actually work as documented — no silent failures or ignored values
- Rate limiting and auth should "just work" out of the box with sensible defaults
- Validation on startup: reject invalid config early with clear error messages

---

## 2) Session context management must actually work

**Problem in OpenClaw:** The system that automatically tells the agent to summarize its session, clear it, and start a new one to save on input tokens never really worked. It was frustrating and unreliable.

**ALMS requirement:**
- Context window management is a first-class concern, not an afterthought
- When conversation history grows large, ALMS must compress/summarize it reliably — not just tell the agent to "summarize yourself" and hope for the best
- The compression strategy must be deterministic and testable:
  - Option A: Rolling summary — periodically condense older messages into a summary block
  - Option B: Sliding window — keep last N messages + a persistent summary prefix
  - Option C: Relevance-based pruning — keep messages relevant to the current task
- Token usage must be observable: show how many tokens are being used per turn, how much is history vs new content
- The user should never notice degraded performance from context management — it should be seamless
- Must be tested with real LLM calls, not just unit tests

---

## 3) Agent workspace files — personality, goals, memories

**Problem in OpenClaw:** No structured way for agents to have persistent identity, goals, or memories across sessions.

**ALMS requirement:**
- Each agent has a **workspace directory** containing structured files:
  - `personality.md` — who the agent is, tone, style, constraints
  - `goals.md` — current objectives, priorities
  - `memories.md` — things the agent has learned, user preferences, past decisions
  - `config.toml` (or similar) — agent-specific settings
- These files are read by the agent at the start of each session and inform the system prompt
- The agent can update these files (especially `memories.md` and `goals.md`) as it works

### Bootstrap mechanism
- On first interaction with a new agent, the agent should **interview the user**:
  - "What should I call you?"
  - "What's my primary purpose?"
  - "Any preferences for how I communicate?"
  - etc.
- The agent fills out its own workspace files based on the user's answers
- This happens automatically on the first chat — no manual file editing required
- The user can always edit the files directly to override

### Memory persistence
- Memories should survive across sessions (not just within one conversation)
- The agent should proactively save important learnings to `memories.md`
- Memories should be structured (not just a wall of text) — categories, timestamps, relevance
- Old/irrelevant memories can be pruned by the agent or the user

---

## 4) Token efficiency (cross-cutting)

Related to context management above, but broader:
- System prompts should be as compact as possible
- Agent workspace files should be summarized/compressed before injecting into context
- Token usage per turn should be logged and queryable
- Alert or auto-adjust when token usage is abnormally high
- See also: `docs/architecture.md` § Token Efficiency

---

*Requirements from Alper (2026-02-14). To be incorporated into implementation planning.*

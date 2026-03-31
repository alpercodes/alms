## ALMS UX Drift Analysis

ALMS has a vision document (`ux-principles.md`) that describes a product fundamentally different from what's been built. The vision is sharp, opinionated, and distinctive. The implementation is competent, well-engineered, and generic. The gap between them is the core problem.

---

### What the vision describes

The vision says ALMS should feel like **operating an agent system**, not chatting with an AI. It names 9 principles. The most distinctive ones are:

1. **The core primitive is not chat** — the mental model should be Session → Goals → Runs → Outcomes → Evidence
2. **Artifacts are the currency** — every meaningful output should be a tangible, reviewable object (diffs, test logs, build artifacts, screenshots)
3. **Diff-first by default** — agents propose ChangeSets with rationale, users apply/reject/amend
4. **Cost and time visible by default** — a HUD showing run duration, tool durations, token cost
5. **Team choreography** — each agent's current run, objective, blockers, next action, last artifacts visible at a glance
6. **Spec is law** — specs are enforceable constraints that runs link to, with drift detection

These principles would make ALMS feel like nothing else on the market. They describe a product where you *supervise work*, not *have conversations*.

### What actually exists

A chat interface. You type a message, the agent replies, messages scroll. There's a session sidebar, an agent selector dropdown, workspace panels, and an audit log. The SSE streaming works well. The approval flow works. Token counts show up as badges on messages.

Strip away the labels and it's indistinguishable from any hosted agent chat — Poe, ChatGPT with tools, a Langchain playground with a nice frontend. The infrastructure underneath is significantly better than those products, but the user-facing surface doesn't express that.

Here's where each distinctive principle actually stands:

**Artifacts**: Zero implementation. No `Artifact` type anywhere in the codebase. No artifact storage, linking, viewing, or diffing. When an agent writes a file, there's no record of what changed. The output is text in a chat stream — ephemeral, unreviewable, unauditable. The vision calls artifacts "the currency" of the system; the system has no currency.

**Diff-first**: Zero implementation. `fs_write` overwrites entire files. There's no ChangeSet concept, no diff generation, no propose-before-apply flow. The approval system asks "allow this tool call?" but doesn't show *what will change* — just *that something will run*. Approving a `shell_exec` without seeing the command's effect is the opposite of trust-building.

**Cost/time visibility**: Minimal. Token counts per run exist. No dollar cost estimation, no per-tool timing, no cumulative spending dashboard, no budget enforcement. For a system designed around autonomous agents that spawn subagents, this is flying blind.

**Team choreography**: Zero UX. The multi-agent hierarchy exists in the backend — parent-child relationships, deterministic sessions, workspace isolation. But there's no team dashboard, no "Agent A is on task X, Agent B is blocked, Agent C finished." You can list agents via CLI. That's it.

**Spec is law**: Zero implementation. No spec concept at all.

The principles that ARE partially realized — Run Timeline (SSE events capture agent activity) and Human Decisions (approval workflow pauses execution) — are the least distinctive. Every agent platform has some version of run tracking and approval gating. These don't differentiate ALMS.

---

### Why this happened

It's not a technical failure. The architecture is genuinely good and supports the vision well. The event system could carry artifact events. The run model could attach artifacts. The tool registry could accommodate diff-producing tools. The approval flow could show diffs instead of tool names. None of this requires a rewrite.

What happened is **infrastructure gravity**. Once you have clean REST APIs for agents, sessions, runs, and jobs, the natural next pull is always "add more management features" — more CRUD, more CLI commands, more admin panels, more edge cases handled. Each increment is useful. Each is easy to justify. And each one makes the product wider without making it deeper.

The development pattern over the last 6 weeks has been overwhelmingly horizontal: agent registry, session CLI, run CLI, job CLI, agent selector UI, shell completions, named subagent sessions, config overrides, concurrent subagent guards, Telegram offset persistence. All plumbing. All necessary eventually. None of it moves toward the distinctive product surface.

The vision document and the execution are disconnected. The vision says "artifacts are the currency." The execution says "let's make the agent CRUD more complete." Both are authored by the same team, but they're operating on different timescales — the vision is aspirational, the execution is incremental, and there's no forcing function pulling them together.

---

### Is the product off-course or still building a base?

Still building a base — but the base is substantially complete and the structure hasn't started. The risk isn't that the foundation is wrong. It's that the foundation keeps getting polished while the distinctive surface never gets built.

Consider: the system has 8 crates, 175+ tests, 30+ HTTP endpoints, 6 CLI command groups, two separate web UIs, SQLite persistence with WAL, SSE streaming with replay, multi-agent hierarchy with deterministic sessions, three context strategies, a WASM sandbox, a Telegram adapter, bearer auth, graceful shutdown, and a scheduler. That's a lot of base.

Now consider: zero artifact types, zero diff tooling, zero team visibility, zero cost dashboards, zero spec enforcement. That's zero distinctive product surface.

A focused sprint — `fs_edit` tool that produces diffs, `Artifact` type attached to runs, UI component that renders diffs with approve/reject — would change the character of the product in under two weeks. The infrastructure is ready. Everything needed to support it already exists. The event system, run model, tool registry, and approval flow are all waiting to be composed into the experience the vision describes.

---

### The competitive risk

The window matters. The "chat with tools" agent platform market is getting crowded fast. OpenAI Assistants, Claude Code, Cursor, Windsurf, CrewAI, AutoGen — all shipping variations of the same pattern. If ALMS launches as another entry in that category, the self-hosted angle and clean architecture won't be enough to differentiate. People will see "another chat agent" and move on.

But if ALMS launches with a genuine artifact-and-diff review flow — where you supervise agent work through tangible outputs instead of scrolling through chat — that's a product that doesn't exist yet. The vision document describes that product. The codebase can support it. The only thing missing is the decision to build it before the management plane gets one more round of polish.

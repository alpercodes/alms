# ALMS Product Vision — Core Idea

Written by Alper (2026-03-21). This is the definitive product vision.

---

## The Core Idea

ALMS is a platform where **teams of AI agents collaborate on projects** — like a virtual company.

Each agent has a role: developer, code reviewer, project manager, designer, bug fixer, or whatever the user defines. They work together on a shared project (e.g. a GitHub repository), and they communicate with each other to get work done.

### What agents do together

- **Invoke each other**: an agent asks another for a PR review, assigns a task, notifies about a blocker
- **Chat with each other**: agents have personal 1-on-1 chats, and group chats with multiple or all agents
- **Scheduled rituals**: daily standups, weekly planning, sprint reviews — automated project cadence
- **Shared project work**: PRs, issues, branches, deploys — real development workflow on real repositories

### What the user sees and does

- **Talk to any agent directly** in a private chat — ask for status, give directions, change priorities
- **Observe agent-to-agent conversations** — see what agents are discussing, how they're coordinating
- **Intervene when needed** — approve decisions, resolve disagreements, redirect effort
- **Morning briefing** — open ALMS, see what happened overnight across the team

### The user is the boss, not the operator

The user doesn't micromanage every action. They set goals, assign roles, and let the team run. They step in when something needs human judgment. The system should feel like managing a team, not typing commands.

---

## Beyond Project Teams — Personal Agents

The same platform should also support agents that are NOT part of a team, or teams that aren't working on a software project:

- A personal finance agent that tracks spending and budgets
- A fitness/diet agent that manages meal plans and workout schedules
- A research agent that monitors topics and sends summaries
- A personal assistant that handles scheduling and reminders

These are individual agents (or small teams) working for the user directly, with their own goals, memories, and scheduled tasks. They use the same infrastructure (workspace, tools, persistence, scheduling) but don't need the full team collaboration layer.

---

## Two Modes, One Platform

| | Team Mode | Personal Mode |
|---|---|---|
| **Agents** | Multiple, with defined roles | One or a few, general-purpose |
| **Communication** | Agent-to-agent chats, group chats, invocations | Agent-to-user only |
| **Project** | Shared (GitHub repo, codebase, etc.) | Per-agent goals |
| **Coordination** | Task assignment, reviews, meetings | Scheduled tasks, reminders |
| **User role** | Manager / supervisor | Client / user |

Both modes share the same core: persistent agents with identity, workspace, tools, and scheduling. Team mode adds the collaboration layer on top.

---

## Implementation Layers

The vision breaks down into three layers of increasing difficulty. Each is independently useful.

### Layer 1 — Agents working independently on a shared project

Agents have persistent identity, tools, workspace, and can act on a shared repository. They execute tasks on their own — write code, run tests, open PRs — but don't talk to each other directly.

**Status**: Mostly built. Agent runtime, workspace files, tool execution, scheduling, persistence all exist.

### Layer 2 — Peer-to-peer communication between agents

Agents can send messages to each other asynchronously. Not just "invoke and wait for result" (the current hierarchy model), but actual ongoing conversations. This requires:

- **Message bus**: Agent A sends a message to Agent B's session; B gets notified and processes it
- **Agent-to-agent sessions**: Like user-to-agent sessions, but both sides are agents
- **Group sessions**: Multiple agents in one conversation
- **Always-on listeners**: Agents that are running and waiting for incoming messages, not just invoked per-task

This is the biggest architectural gap. The current design is pure hierarchy (parent spawns child, child returns result, no peer messaging). This layer changes that.

### Layer 3 — Emergent team dynamics

Agents know *when* and *how* to collaborate without being told every step. Scheduled standups happen automatically. A developer agent knows to request a review after opening a PR. A PM agent notices a blocker and reassigns work.

This is less about infrastructure and more about agent behavior — prompt engineering, role definitions, and teaching agents the team's workflow. The infrastructure from Layer 2 makes it possible; Layer 3 makes it feel natural.

### Approach

Build in order: Layer 1 (done) → Layer 2 (peer DMs first, then group chats) → Layer 3 (scheduled rituals, behavioral patterns).

---

*This is the product. Everything else — the Rust binary, the native tool registry, the SSE streaming, the multi-agent hierarchy — is infrastructure in service of this idea.*

ALMS — Communication Layer & Architecture Design Document
1. Project Vision
ALMS (Agent Loop Management System) is an agent platform that simulates software development teams. Unlike existing tools that focus on individual agent task execution or simple job scheduling, ALMS models the full dynamics of a development team — agents with distinct roles (developer, bug fixer, PR reviewer, etc.) that collaborate through conversations, meetings, and review loops.
The core insight driving ALMS is that better agent-to-agent communication produces better development outcomes. Rather than isolated agents executing tasks in sequence, ALMS agents engage in review loops, team discussions, and cross-role feedback — mirroring how effective human teams actually work.
1.1 Initial Use Case
The first target use case is autonomous software development. A team of agents collaborates on a project, producing pull requests, reviewing each other's work, addressing findings, and iterating until code is ready to merge. The user can participate — chatting with agents, giving direction — but the system operates largely autonomously through scheduled jobs and agent-to-agent interaction.
1.2 Future Scope
The architecture is designed to be flexible beyond dev teams. Future team types could include research teams, creative teams, marketing teams, or any collaborative workflow. The team dynamics layer (meetings, chats, roles, review loops) should be generic enough to support this.
1.3 Product Ambition
ALMS is intended as a real desktop product that ships to users. The long-term goal includes enterprise and corporate customers. It is not a research project — it needs to solve real problems better than existing alternatives.

2. Business Model: Open Core
ALMS follows an open core model:
Open source core (Apache 2.0): The agent orchestration engine, basic team dynamics, communication primitives (chats, meetings, review loops), and the core agent runtime. This maximizes community adoption, builds trust, and enables contributions.
Proprietary enterprise layer: Features like SSO, audit logging, advanced permissions, team management dashboards, hosted infrastructure, compliance features, advanced analytics on agent performance, and priority support SLAs.
2.1 Rationale
Fully open source risks a larger company forking and outcompeting on distribution (the AWS/Elasticsearch problem). Fully closed source loses the organic developer adoption that dev tools depend on. Open core provides community trust and adoption while protecting a clear revenue path.

3. Tech Stack

Backend: Rust
Frontend: Preact
Agent sandbox: native Rust tools with per-tool sandboxing (path canonicalization, shell permissions, Landlock on Linux), capability-based permissions
Architecture: Pure tree agent hierarchy with ephemeral and persistent subagents, SSE-based observability

No stack changes are planned. The existing foundation is solid for the target use case and enterprise ambitions.

4. Hybrid Communication Model
ALMS uses a hybrid approach to agent-to-agent communication. Not everything needs to be a natural language LLM call.
4.1 Structured Data (No LLM Required)
Routine, predictable information is passed as structured data (JSON). Examples:

PR metadata (changed files, diff stats, branch info)
Test results (pass/fail, coverage numbers)
Build status
Task status updates (in progress, blocked, done)
Code linting output
Merge readiness signals

These messages are cheap, fast, easy to log, easy to search, and straightforward to render in the UI.
4.2 Natural Language (LLM Calls)
Natural language is reserved for interactions that require reasoning:

Code review discussions (explaining why something is problematic)
Design decision debates
Nuanced feedback during PR reviews
Meeting discussions about priorities and tradeoffs
Situations where an agent needs to explain its reasoning to another agent or to the user

4.3 Why Hybrid
A team of five agents having a meeting entirely in natural language could generate dozens of LLM calls per discussion. The hybrid approach keeps costs manageable and latency low while preserving natural language where it actually adds value. Structured messages are also far easier to build dashboards, audit trails, and search functionality around.

5. Context Management
5.1 Core Principle
When an agent participates in a conversation, it receives the full conversation history for that interaction. No truncation, no partial views — the agent sees everything that has been said in the current conversation.
5.2 Meeting Context
Team meetings receive a curated summary block prepended to the conversation. This block contains relevant grounding context, such as:

Summary of the previous meeting
Current project statistics (open PRs, test pass rates, recent merges)
Relevant documents or specs
Any urgent items flagged since the last meeting

The summary block is not the full history of all prior meetings — it is a curated snapshot of what's relevant now.
5.3 Meeting Lifecycle
Meetings are self-contained units with a clear lifecycle:

Start: Manager agent initiates the meeting. Summary block is prepended as context.
During: Full conversation is sent to all participating agents on each turn.
End: Meeting concludes with a system-generated summary.
Feed-forward: The meeting summary becomes part of the context block for the next meeting.

This creates a recursive loop: meeting → summary → next meeting's context → meeting → summary → and so on. Institutional knowledge accumulates through these summaries without unbounded context growth.
5.4 Meeting Length
When a meeting's conversation grows too long (approaching context window limits or cost thresholds), it ends. The summary captures everything discussed, and a follow-up meeting can be scheduled if agenda items remain unresolved.

6. Message Routing
Three communication channels exist in ALMS.
6.1 Direct Chats
One agent invokes another agent directly. Only those two agents see the conversation. Use cases: a developer asking the reviewer a clarifying question, the manager assigning a task to a specific agent.
6.2 Group Chats
Multiple agents can participate. Invocation is driven by @-mentions:

@agentname — invokes a specific agent
@agentname @agentname2 — invokes multiple specific agents
@everyone — invokes all agents in the team
No tag — no agent is invoked (message is logged but no one responds)

Agents are informed of this tagging system via their system prompt. They understand how to use tags when they want to address specific teammates.
6.3 Team Meetings
All team agents participate. Initiated and facilitated by the manager role agent (see Section 9). Follows the meeting lifecycle described in Section 5.3.

7. Ignore Signal
Agents have the ability to choose not to respond to an invocation. This is a built-in feature, not just prompt engineering.
7.1 How It Works

Agent receives an invocation (e.g., from an @everyone mention in a group chat).
Agent reads the context and evaluates whether it has something meaningful to contribute.
If not, the agent sends a built-in ignore signal to the system, declining to respond.
The system registers the ignore and does not produce a response from that agent.

The system prompt instructs agents on this capability and when it's appropriate to use it.
7.2 Current Implementation
The ignore decision is a full LLM call. The agent reads the message, thinks about whether to respond, and either responds or sends the ignore signal. This is the expensive version.
7.3 Future Optimization
A cheaper pre-filter layer before the full LLM call:

A smaller, faster model that evaluates relevance
Or a rule-based check (e.g., "this message is about frontend CSS and this agent is the database specialist — skip")

This optimization is deferred. Build the expensive correct version first, optimize later.

8. Task Queue
8.1 Core Principle
Each agent has its own task queue. Every invocation — whether from a direct chat, group chat mention, meeting, or scheduled job — gets queued.
8.2 No Parallel Instances
A critical constraint: no two instances of the same agent run in parallel. Tasks are processed sequentially. This prevents:

State conflicts (agent making contradictory decisions in two conversations)
Memory corruption (two instances writing to the same agent memory)
Confusing behavior (agent appearing in two places at once)

8.3 Priority Levels
The queue supports two priority levels:

Normal: First in, first out (FIFO). The default for all invocations.
Urgent: Jumps to the front of the queue. Reserved for critical situations such as blocking production bugs, security incidents, or other time-sensitive invocations.

The system or user can flag an invocation as urgent. The specifics of what qualifies as urgent can be configured per team or per project.

9. Meeting Protocol
9.1 Manager Role
A manager role agent is responsible for:

Initiating team meetings (either on schedule or when circumstances require it)
Driving the meeting agenda
Facilitating discussion (directing questions to specific agents, keeping conversation on track)

9.2 Meeting Termination
Meeting termination strategy (combination approach, to be finalized):

Primary: The manager agent decides when all agenda items are resolved and ends the meeting.
Fallback: A hard cap on the maximum number of rounds prevents infinite loops. If the cap is hit, the meeting ends regardless, and unresolved items carry forward.

The manager can end the meeting early if all topics are covered before the round cap.
9.3 Meeting Output
Every meeting ends with a summary that captures:

What was discussed
Decisions made
Action items and who owns them
Unresolved items to carry forward

This summary becomes persistent context for future meetings (see Section 5.3).

10. Agent Memory
10.1 Per-Agent Persistent Memory
Each agent maintains its own persistent memory, separate from other agents. This is not shared team memory — it is the agent's individual knowledge base.
Examples of what an agent might remember:

The reviewer remembers that a certain module is fragile and needs extra scrutiny
The developer remembers a pattern that worked well in a previous PR
The manager remembers that a certain feature area tends to cause merge conflicts

10.2 Memory Population
Agents decide on their own what to save to memory. The system prompt instructs agents to save information they deem important after interactions. There is no external extraction system — the agent is the sole author of its own memory.
10.3 Future Considerations

Tuning: Agents may save too much noise or miss critical information. Guardrails or guidance around memory quality may be needed.
Memory limits: Currently no defined cap on memory size. May need pruning or relevance decay over time.
Memory search: As memory grows, agents may need a retrieval mechanism rather than having all memory in context.


11. Agent Initiative (Future)
11.1 Current State: Reactive
Agents are currently purely reactive. They act only when:

Another agent invokes them
A user invokes them
A scheduled job triggers them

Their invocation prompt contains the context and clues about what to do.
11.2 Future State: Proactive
Future work may allow agents to take initiative:

A developer agent that finishes a task and picks up the next unassigned issue
A reviewer agent that proactively scans for stale PRs
A bug fixer that monitors error logs and opens issues autonomously

This is explicitly out of scope for now.

12. The PR Review Loop (Core Workflow)
The primary workflow that ALMS is built around:

Developer agent writes code and creates a pull request.
PR reviewer agent reviews the PR, leaving findings and feedback (natural language for reasoning, structured data for metadata).
Developer agent receives the review, addresses findings, and pushes updates.
Back and forth: Reviewer re-reviews, developer addresses, iterating until the reviewer is satisfied.
Merge signal: Reviewer approves with no remaining findings. PR is ready to merge.

This loop is the core value proposition of ALMS. Existing tools require manual orchestration of this loop or only handle pieces of it. ALMS makes it native and automatic, with the team dynamics (meetings, group chats, memory) providing the broader context that makes each iteration smarter.

13. Competitive Landscape
13.1 What Exists

CrewAI / MetaGPT: Multi-agent frameworks with role-based structures. General purpose, not specifically optimized for dev team dynamics.
GitHub Copilot Agents / Kiro / Factory: Create PRs, review code, handle tasks autonomously. Focus on individual agent execution, not team collaboration.
Devin: Autonomous coding agent. Closed source, single agent, not team-based.
Claude Code Review / BugBot: PR review tools. Handle one piece of the loop, not the full team dynamic.

13.2 ALMS Differentiation
No existing product offers the full package ALMS is building:

Agents with distinct, persistent roles on a team
Native agent-to-agent communication (direct chats, group chats, meetings)
Autonomous review loops that iterate without human prompting
Team meetings with summaries that build institutional knowledge
Scheduled jobs and continuous autonomy beyond single-task execution
A system where the user participates in a team, not just dispatches tasks to individual agents

The differentiation is not in any single feature but in the cohesive team simulation as a whole.

14. Current Status & Next Steps
14.1 What's Built

Rust backend and Preact frontend
Agent runtime and basic orchestration
Native tool registry with per-tool sandboxing (path canonicalization, shell permissions, Landlock on Linux) and capability-based permissions
Pure tree agent hierarchy
SSE-based observability

14.2 What's Not Built Yet

Agent-to-agent communication (direct chats, group chats)
Team meetings and meeting protocol
The PR review loop as an automated workflow
Agent memory system
Task queue with priority levels
Ignore signal mechanism
Manager role and meeting facilitation
Scheduled jobs and autonomous operation

14.3 Immediate Next Steps

Fix remaining bugs around subagent invocations and message queuing in the UI
Finalize the base architecture
Begin building the communication layer as described in this document
Target the PR review loop as the first end-to-end workflow to implement

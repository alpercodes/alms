# Multi-Agent Development Workflow with Claude Code

A practical guide to running a 4-agent development team inside Claude Code. This setup enables parallel feature development, bug fixing, and code review — all coordinated by a single human operator.

> **On the issue numbers below.** References such as `#467` and `#497` point at this
> project's pre-migration tracker. The repository moved on 2026-09-01 and issue numbering
> restarted at 1, so they do not resolve against the current tracker — a low number may now
> land on an unrelated issue. They are kept because the worked examples reason about
> specific pieces of work. The same convention is used in
> [`docs/engineering-reviews/README.md`](engineering-reviews/README.md).

## Overview

The system uses Claude Code's Agent tool with git worktree isolation to run multiple specialized agents in parallel. Each agent operates in its own copy of the repository, so they never conflict.

| Agent | Role | Edits Code | Isolation | Co-Authored-By |
|-------|------|-----------|-----------|----------------|
| **Atlas** | Coordinator (main session) | Yes | Main repo | `Atlas <noreply@anthropic.com>` |
| **Heph** | Feature development | Yes | Worktree | `Heph <noreply@anthropic.com>` |
| **Larry** | Bug fixing | Yes | Worktree | `Larry <noreply@anthropic.com>` |
| **Tim** | Code review (read-only) | No | Worktree | N/A |

**Atlas** is the main Claude Code session — the human talks to Atlas, who coordinates the other three. Heph and Larry write code in parallel. Tim reviews their PRs. The human approves merges.

## Prerequisites

- Claude Code CLI or desktop app
- A GitHub repository with `gh` CLI authenticated
- Git worktree support (standard in modern git)

## Setup

### 1. Create the agent definition files

Agent definitions live in `.claude/agents/` at your project root. Each file defines one agent's identity, tools, isolation mode, and instructions.

#### `.claude/agents/heph-dev.md` — Feature Developer

```markdown
---
name: heph-dev
description: "Use this agent for planned development tasks — implementing features, refactors, and enhancements. Heph works in his own worktree, creates a branch, implements the change, runs tests, commits, pushes, creates a PR, and reports back."
model: opus
isolation: worktree
tools: Read, Write, Edit, Glob, Grep, Bash, WebFetch
---

You are Heph, the primary development agent.

## Identity
- Your name is **Heph**
- All commits must end with: `Co-Authored-By: Heph <noreply@anthropic.com>`

## Workflow
1. Read the GitHub issue if one exists
2. Create a branch: `git fetch origin develop && git checkout -b <type>/<name> origin/develop`
3. Read relevant source files, plan the change
4. Implement the change
5. Run your project's CI checks (tests, linter, formatter)
6. **Set per-worktree git identity** (use `--worktree`; plain `git config` writes to the shared `.git/config` and pollutes every other worktree — see Operational Rule #7):
   - `git config --worktree user.name "Heph"`
   - `git config --worktree user.email "noreply@anthropic.com"`
7. Commit with Co-Authored-By trailer
8. Push: `git push -u origin <branch>`
9. Create PR: `gh pr create --base develop --title "..." --body "..."`
10. Report back with PR URL and summary

## Rules
- Branch from `origin/develop`, never commit directly to a protected branch
- Focused changes only — don't refactor surrounding code
- Always run tests before pushing
- Git remote is `origin`
```

#### `.claude/agents/larry-bug-fix.md` — Bug Fixer

```markdown
---
name: larry-bug-fix
description: "Use this agent to fix bugs autonomously. Larry works in his own worktree, creates a branch, fixes the issue, runs tests, commits, pushes, creates a PR, and comments on the GitHub issue."
model: opus
isolation: worktree
tools: Read, Write, Edit, Glob, Grep, Bash, WebFetch
---

You are Larry, an autonomous bug-fix agent.

## Identity
- Your name is **Larry**
- All commits must end with: `Co-Authored-By: Larry <noreply@anthropic.com>`

## Workflow
1. Read the GitHub issue
2. Create a branch: `git fetch origin develop && git checkout -b fix/<name> origin/develop`
3. Investigate and fix the bug — minimal changes only
4. Run CI checks (tests, linter, formatter)
5. **Set per-worktree git identity** (use `--worktree`; plain `git config` writes to the shared `.git/config` and pollutes every other worktree — see Operational Rule #7):
   - `git config --worktree user.name "Larry"`
   - `git config --worktree user.email "noreply@anthropic.com"`
6. Commit, push, create PR with `Fixes #<number>` in body
7. Comment on the issue with what you did

## Rules
- Minimal changes only — fix the bug, nothing else
- Branch from `origin/develop`, never commit directly to a protected branch
- Git remote is `origin`
```

#### `.claude/agents/alms-dev-guardian.md` — Code Reviewer

```markdown
---
name: alms-dev-guardian
description: "Use this agent for code review. Tim reviews PRs, checks documentation, and validates architectural decisions. Read-only — cannot edit code."
model: opus
isolation: worktree
tools: Read, Glob, Grep, Bash, WebFetch
disallowedTools: Write, Edit
---

You are **Tim**, the code review agent.

## Identity
- Your name is **Tim**
- You are **read-only** — you do NOT edit code files
- You post reviews as GitHub PR comments

## Workflow
1. Get the PR diff: `gh pr diff <number>`
2. Read changed files and cross-reference against existing code
3. Post review as a PR comment:
   ```
   gh api repos/OWNER/REPO/pulls/<number>/reviews \
     -f event="COMMENT" \
     -f body="## Review by Tim (automated)\n\n..."
   ```

## Review Format
- **Verdict**: Ready to merge / Needs minor fixes / Needs rework
- **Critical**: Blocking issues
- **Suggestions**: Non-blocking improvements
- **Nits**: Style/naming

## Review Focus
- Error paths and cleanup
- Concurrency and race conditions
- Security boundaries
- Backward compatibility
- Config-to-runtime threading (is the config actually enforced?)
- Test quality (failure modes, not just happy paths)
```

### 2. Configure CLAUDE.md

Add an agent team section to your project's `CLAUDE.md` so the main session knows how to use the agents:

```markdown
## Claude Code Agent Team

| Agent | Role | Launch with |
|-------|------|-------------|
| **Heph** (`heph-dev`) | Feature dev | `subagent_type: "heph-dev"` |
| **Larry** (`larry-bug-fix`) | Bug fixer | `subagent_type: "larry-bug-fix"` |
| **Tim** (`alms-dev-guardian`) | Code reviewer | `subagent_type: "alms-dev-guardian"` |

- All agents use `isolation: "worktree"` — they never interfere with each other
- Never merge PRs without explicit human approval
- After merging, always pull develop
- Never launch two agents targeting the same branch — use SendMessage to continue an existing agent
```

## Directory Structure

After setup, your project should have this structure:

```
.claude/
  agents/                          # Agent definitions (checked into repo)
    heph-dev.md                    # Feature developer
    larry-bug-fix.md               # Bug fixer
    alms-dev-guardian.md            # Code reviewer (Tim)
  agent-memory/                    # Persistent agent memory (gitignored)
    alms-dev-guardian/              # Tim's memory directory
      MEMORY.md                    # Index of Tim's memories
      *.md                         # Individual memory files
  settings.json                    # Claude Code permissions config
  worktrees/                       # Auto-created worktrees (gitignored)
    agent-<id>/                    # One per running/completed agent

# User-level memory (outside repo, persists across projects)
~/.claude/projects/<project-hash>/memory/
  MEMORY.md                        # Atlas's memory index
  feedback_*.md                    # Operational rules learned over time
  project_*.md                     # Project state snapshots
  user_*.md                        # User preferences
```

### What goes where

| File | Purpose | Checked in? |
|------|---------|-------------|
| `.claude/agents/*.md` | Agent definitions | Yes |
| `.claude/agent-memory/` | Per-agent persistent memory (review patterns, findings) | No (gitignore) |
| `.claude/settings.json` | Tool permissions | Optional |
| `.claude/worktrees/` | Agent working copies | No (gitignore) |
| `~/.claude/projects/*/memory/` | Atlas's memory (rules, preferences, project context) | N/A (user-level) |
| `CLAUDE.md` | Project instructions for all agents | Yes |

### `.gitignore` additions

```
.claude/agent-memory/
.claude/worktrees/
.claude/scheduled_tasks.lock
```

### Atlas Memory System

Atlas (the coordinator) has a persistent memory system that survives across sessions. This is how operational rules get learned and enforced:

```
~/.claude/projects/<project>/memory/
  MEMORY.md                           # Index — one-line pointers to each memory file
  feedback_no_merge_without_approval.md  # "Never merge without human saying 'merge it'"
  feedback_always_pull.md              # "After merge, always pull develop"
  feedback_no_parallel_same_branch.md  # "Two agents on same branch = push conflict"
  feedback_git_author_names.md         # "Each agent sets own git user.name"
  project_v012_release.md             # "v0.1.2 tagged 2026-04-03"
```

Each memory file has frontmatter:

```markdown
---
name: Never merge without explicit approval
description: Never merge PRs unless the human explicitly says to
type: feedback
---

Never merge PRs unless the human explicitly says "merge it" or similar.

**Why:** Human wants to maintain control over what goes into `develop`.
**How to apply:** After Tim reviews, report status and wait. Don't merge proactively.
```

Memory types:
- **feedback** — rules learned from human corrections ("don't do X", "always do Y")
- **user** — who the human is, their preferences, expertise level
- **project** — ongoing work, deadlines, decisions
- **reference** — pointers to external systems (issue trackers, dashboards)

Atlas reads these at the start of every session and follows them automatically. When the human corrects behavior, Atlas saves it as a new memory so it sticks.

### Tim's Persistent Memory

Tim (the reviewer) has his own memory at `.claude/agent-memory/alms-dev-guardian/`. This stores:

- Recurring code patterns he's learned
- Security state snapshots
- Lock ordering knowledge
- Architecture decisions
- Dead code tracking
- Documentation drift observations

This means Tim's reviews get better over time — he remembers what he's seen before and doesn't re-discover the same issues.

### Permissions Configuration

`.claude/settings.json` controls which tools agents can use without prompting:

```json
{
  "permissions": {
    "allow": [
      "Bash(*)",
      "Read",
      "Write",
      "Edit",
      "Glob",
      "Grep"
    ]
  }
}
```

This auto-approves common tools so agents don't stall waiting for permission. Adjust based on your trust level.

## How It Works

### The Development Loop

The core loop that makes this efficient:

```
Human: "have heph implement issue #123"
  Atlas → launches Heph in background worktree
  
Human: "have larry fix issue #456"  
  Atlas → launches Larry in background worktree (parallel with Heph)

[Heph completes] → PR #200 created
  Atlas: "Heph's done — PR #200. Have Tim review it."
  Atlas → launches Tim to review PR #200

[Larry completes] → PR #201 created
  Atlas: "Larry's done — PR #201."

[Tim completes review of #200] → "Ready to merge"
  Atlas: "Tim approved #200. Merge?"
  Human: "yes"
  Atlas → merges, pulls develop

  Atlas → launches Tim to review PR #201 (sequential — one Tim at a time)
```

### How Worktree Isolation Works

When an agent launches with `isolation: worktree`, Claude Code:

1. Creates a fresh git worktree at `.claude/worktrees/agent-<id>/`
2. The agent gets a full copy of the repo at that path
3. The agent works entirely in its worktree — reads, edits, builds, tests
4. When done, the agent's result is returned to Atlas
5. The worktree persists until explicitly cleaned up

This means:
- **No branch conflicts** — each agent is on its own branch in its own directory
- **No build interference** — each worktree has its own `target/` (or `node_modules/`, etc.)
- **Atlas keeps working** — the main session's files are never touched
- **Disk cost** — each worktree is a full repo copy. Clean up after agents finish.

### The Role of CLAUDE.md

`CLAUDE.md` at the project root is read by ALL agents (including subagents). Use it to define:

- Project structure and conventions
- Build/test commands
- Agent team table and coordination rules
- Current project state (what's working, what's not)
- Important architectural decisions

Every agent inherits this context, so you don't need to repeat project conventions in each agent definition.

### Launching Agents

Atlas uses the Agent tool with `run_in_background: true` for parallel work:

```
// Launch Heph and Larry in parallel
Agent(subagent_type: "heph-dev", prompt: "...", run_in_background: true)
Agent(subagent_type: "larry-bug-fix", prompt: "...", run_in_background: true)
```

When an agent completes, Atlas gets a notification with the result.

### Writing Good Agent Prompts

Bad prompt (vague):
> "fix the bug"

Good prompt (contextual):
> "Issue #467: SSE reconnect sends UUID in Last-Event-Id header. The backend handles it gracefully but causes a full replay and visual flash. Three things to fix: (1) replace UUID event IDs with ephemeral-N format in sse.rs, (2) add seenEventIds dedup Set in use-session-stream.js, (3) update stale lastSeenEventId doc comment. Branch: fix/sse-reconnect-hardening-467. PR should reference: Closes #467."

Key elements:
- **What** the issue is (with issue number)
- **Where** in the code (file paths)
- **What to change** (specific instructions)
- **Branch naming** convention
- **PR references** (Closes #N)

### The Review Cycle

Tim reviews are iterative:

```
1. Tim reviews PR → finds issues
2. Heph/Larry addresses feedback → pushes to same branch
3. Tim re-reviews → approves
4. Human says "merge"
5. Atlas merges and pulls develop
```

**Critical rule**: Tim reviews sequentially — never run two Tim instances in parallel. This prevents duplicate or conflicting reviews.

**Critical rule**: When Tim finds issues, send the SAME agent (Heph or Larry) back to fix them on the same branch. Never launch a new agent targeting an existing branch — the second push will fail.

## Operational Rules

These rules were learned through experience. Violating them causes real problems.

### 1. Never merge without human approval

Even if Tim approves, wait for the human to say "merge it". The human may want to test first.

### 2. Always pull after merge

After every `gh pr merge`, immediately run:
```bash
git checkout develop && git pull origin develop
```
Don't ask — just do it.

### 3. One Tim at a time

Tim reviews are sequential. Queue them up and send Tim to the next PR only after the current review completes.

### 4. Never launch parallel agents on the same branch

Two agents on the same branch = push conflict. The second agent's work is lost. Use `SendMessage` to continue an existing agent with new instructions.

### 5. Clean up worktrees

Agent worktrees accumulate and eat disk space (each is a full repo copy with build artifacts). After an agent completes:
```bash
git worktree remove .claude/worktrees/agent-<id> --force
git worktree prune
```

Clean up regularly. A session with 10+ agent runs can consume 40+ GB.

### 6. Don't duplicate test runs

Agents should not run tests or linters back-to-back if nothing changed since the last passing run. One pass is enough.

### 7. Git author identity — use `--worktree`, not plain `git config`

Each agent sets its OWN git identity in its worktree, every run — and the `--worktree` scope flag is **load-bearing**, not optional.

**Why `--worktree` matters.** Linked worktrees share `.git/config` with the main repo by default. So `git config user.name "Heph"` (no scope flag — defaults to `--local`) inside Heph's worktree writes to the SHARED config and silently rewrites Atlas's main-repo identity AND every other concurrent worktree's identity. Whichever agent runs the config command last wins, and the others (including Atlas committing in main) inherit the wrong identity. The failure mode is invisible because everyone's commits collapse to a single agent's name, which looks consistent rather than broken.

**One-time setup** (already done for this repo): enable per-worktree config in the main repo:

```bash
git config extensions.worktreeConfig true
```

This makes `git config --worktree key value` write to a separate per-worktree config file (`.git/config.worktree` for the main worktree, `.git/worktrees/<id>/config.worktree` for linked worktrees) that overrides the shared `.git/config` for the current worktree only.

**Per-worktree usage** (every agent, every run, before committing):

```bash
# Heph
git config --worktree user.name "Heph"
git config --worktree user.email "noreply@anthropic.com"

# Larry
git config --worktree user.name "Larry"
git config --worktree user.email "noreply@anthropic.com"

# Atlas (main worktree, also via --worktree)
git config --worktree user.name "Atlas"
git config --worktree user.email "noreply@anthropic.com"
```

Both `user.name` AND `user.email` matter — `user.email` drives GitHub avatar/account-linking on the commit page and shapes the `Co-Authored-By: Name <email>` trailer in squash-merge commits.

Put this BEFORE the commit step in each agent's workflow (not just buried in the Identity section), so the agent can't accidentally skip it. Without `--worktree`, the symptom is everyone's commits authored under one name (whoever set their config last) — which looks consistent rather than broken, so it's easy to miss until you audit.

## Scaling Patterns

### Batch work across agents

When you have multiple independent issues, launch Heph and Larry on different ones simultaneously:

```
Human: "have heph tackle #108 and #109. have larry fix #467."
Atlas → launches Heph (background) on #108+#109
Atlas → launches Larry (background) on #467
```

### Chain reviews efficiently

When multiple PRs land close together:

```
Human: "merge #480. have tim review #481."
Atlas → merges #480, pulls develop
Atlas → launches Tim on #481
[Tim completes]
Human: "merge #481. have tim review #482."
...
```

### Investigation → fix pipeline

For unclear bugs, use Tim to investigate first, then Heph/Larry to fix:

```
Human: "have tim investigate issue #32 and comment on the issue"
[Tim posts findings as GitHub comment]
Human: "have heph fix it based on Tim's findings"
Atlas → launches Heph with Tim's analysis as context
```

### Address review feedback

When Tim flags issues on a PR:

```
Human: "have heph address Tim's criticals and suggestions on #483"
Atlas → reads Tim's review → launches Heph with specific items to fix
[Heph pushes fixes]
Human: "have tim re-review #483"
Atlas → launches Tim for re-review
```

## Typical Session Output

A productive session with this setup routinely produces 10-15 merged PRs:

```
Session: 14 PRs merged
- #480  Canonicalize edge case tests (Larry → Tim reviewed)
- #481  Event log + query-string auth (Heph → Tim reviewed)  
- #482  SSE reconnect hardening (Larry → Tim reviewed)
- #483  Shell sandboxing + Landlock (Heph → Tim reviewed × 2)
- #485  Tool call display redesign (Heph → Tim reviewed)
- #486  Stale docs fix (Larry → Tim reviewed)
- #488  Episodic memory default fix (Heph → Tim reviewed)
- #489  DM run ended display (Larry → Tim reviewed)
- #490  Context building docs (Heph → Tim reviewed)
- #492  Settings persistence fix (Heph → Tim reviewed)
- #493  Subagent session summary (Larry → Tim reviewed × 2)
- #494  Run completed noise fix (Larry → Tim reviewed)
- #496  Notification rerouting (Heph → Tim reviewed × 2)
- #497  Tool calls rendering fix (Larry → Tim reviewed × 2)
```

The key to this throughput: agents run in parallel, Tim reviews sequentially, and the human only needs to approve merges and provide direction.

## Adapting to Your Project

To use this pattern on a different project:

1. **Copy the agent `.md` files** to `.claude/agents/` and customize:
   - Replace CI commands with your project's (e.g., `npm test`, `go test ./...`, `pytest`)
   - Set the correct git remote name
   - Add your project's code conventions and linting rules
   - Adjust the review focus areas for your domain

2. **Update CLAUDE.md** with your agent team table and rules

3. **Start small**: Begin with just Atlas + one dev agent (Heph). Add Tim for reviews once the basic loop works. Add Larry when you need parallel bug fixing.

4. **Tune the prompts**: The more specific your agent prompts, the better the results. Include file paths, line numbers, and clear success criteria.

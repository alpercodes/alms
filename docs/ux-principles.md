# ALMS Product UX Principles (to feel fundamentally different)

ALMS should not feel like “chat with tools”. It should feel like operating an **agent system**.

These principles are the product north star. They are intentionally opinionated.

---

## 0) The core UX primitive is not chat

Chat can exist, but the primary mental model should be:

**Session → Goals → Runs → Outcomes (artifacts) → Evidence (events/audit)**

If the UI/CLI emphasizes this, users will trust and adopt autonomy.

---

## 1) Run Timeline is the primary UI

A user should always be able to answer:
- what is happening?
- why is it happening?
- what did it change?
- what do I need to decide?

The Run Timeline is a live stream of:
- token deltas
- policy decisions (allow/deny/approval_required)
- approvals requested/resolved
- tool start/end
- run finished

If you hide policy/tooling, ALMS feels like a black box and users won’t grant autonomy.

---

## 2) Artifacts are the currency

Outcomes should be tangible:
- diffs/patches
- test logs
- build artifacts
- screenshots
- generated docs
- review results

Everything important should be an artifact or linked to one.

This makes ALMS:
- reviewable
- auditable
- reproducible

---

## 3) Diff-first by default (“Propose → Apply”)

Even in `full_control`, default UX should be:

1) agent proposes a ChangeSet (diff + rationale)
2) user can apply/reject/amend

This is not only safety—it’s trust-building.

---

## 4) Tight feedback loops (Claude Code ergonomics)

Claude Code works because it makes loops fast and visible:
- “what I’m about to do” is explicit
- diffs are visible
- tests/logs are first-class

ALMS should copy that feel:
- short commands that map to big actions
- visible diffs before apply
- one-command iterate loops (diagnose → patch → test → summarize)

---

## 5) Human decisions are first-class states

Approvals should not feel like errors.
They should feel like the system doing the right thing:

- “I can do this, but I need you to confirm.”

Approval UX must be:
- one screen
- exact action shown (argv, cwd, paths, url)
- approve/deny obvious

---

## 6) Cost + time is visible by default

A status HUD changes user behavior immediately:
- run duration
- tool durations
- tokens/cost

Token efficiency should be treated as a product feature, not just an implementation detail.

---

## 7) Team choreography without chaos

For a team, the UI/CLI should make it effortless to:
- assign work
- request reviews
- see blockers
- iterate
- merge safely

The system should surface:
- each agent’s current run
- objective
- blockers
- next scheduled action
- last artifacts

---

## 8) “Spec is law” without bureaucracy

Specs should be enforceable constraints:
- runs/PRs link to spec refs
- spec drift is detectable
- merges can be gated by policy

But specs must be lightweight:
- start as markdown anchors
- evolve into IDs over time

---

## 9) UX success criteria

If ALMS is working, users will say:
- “I can see what it’s doing.”
- “I can review changes safely.”
- “It feels like operating a team, not chatting.”
- “It saves me real work.”

---

*Authored by Mesut (2026-02-14).* 

# ALMS Workflow Layer (spec)

ALMS’s kernel is **runs + events + tools + jobs + audit**.

To feel like a new way to use models (Claude Code ergonomics + team Git automation), ALMS needs a thin layer above the kernel:

- **WorkItems** that span multiple runs
- **ChangeSets** that package code/doc changes as reviewable artifacts
- **PR/Review handshakes** that are structured (not implicit chat)
- **Invariants** that keep multi-agent choreography reliable

This must *not* become a heavy project-management system. The workflow layer is justified only if it reduces friction and increases trust.

**See also:**
- `docs/api.md`
- `docs/events-and-audit.md`
- `docs/security-model.md`
- `docs/approvals-ux.md`
- `docs/artifacts.md`
- `docs/policy-reasons.md`

---

## 0) Design principles (anti-bureaucracy)

1) **Artifacts over metadata**
   - If it’s not tied to a diff, test output, review, or log, it’s probably noise.

2) **Auto-generate by default**
   - WorkItems can be created from a prompt; ChangeSets can be created from diffs; Reviews from PR comments.

3) **Minimal required fields**
   - Require only what makes automation safe: `spec_refs`, `acceptance_criteria`, `repo`, `branch`.

4) **Workflow state derived from events**
   - Don’t store “status” manually; derive it from run/PR/review/check events.

5) **Policy is explicit**
   - Merge rules and safety gates are machine-checkable.

---

## 1) Why a Workflow layer is needed

Runs are a good unit of execution, but the workflows we want are multi-run and multi-agent:

- “implement feature X” → involves design, coding, tests, review, iteration, merge
- “keep docs aligned” → involves reading specs, tracking diffs, opening doc PRs
- “team loop” → author agent ↔ reviewer agents ↔ doc agent, repeated

Without first-class workflow objects, this becomes implicit chat text and drifts.

---

## 2) Core workflow resources (minimal but powerful)

### 2.1 WorkItem
A WorkItem is the unit of work for a team.

Minimal shape:
```json
{
  "work_item_id": "WI-<uuid>",
  "title": "Implement approval flow for guarded posture",
  "repo": {
    "path": "</srv/alms",
    "remote": "<optional git remote url>"
  },
  "spec_refs": ["S-123", "S-124"],
  "acceptance_criteria": [
    "approval_required pauses run",
    "approval_resolved continues",
    "full_control never emits approvals"
  ],
  "owner_agent": "atlas",
  "created_at": "...",
  "links": {
    "session_id": "<uuid>"
  }
}
```

Notes:
- `spec_refs` can point to docs sections/anchors (e.g. `events-and-audit.md#approval_required`) until a formal spec system exists.
- WorkItem should be “small enough to finish”. If not, split.

### 2.2 ChangeSet
A ChangeSet is a package of changes represented as artifacts.

Minimal shape:
```json
{
  "changeset_id": "CS-<uuid>",
  "work_item_id": "WI-...",
  "branch": "feature/wi-...",
  "patch_artifacts": ["A-<uuid>", "A-<uuid>"],
  "files_touched": ["crates/alms-gateway/src/..."],
  "tests": {
    "status": "passed",
    "artifacts": ["A-<uuid>"]
  },
  "risk": {
    "level": "medium",
    "reasons": ["touches auth", "touches tool policy"]
  }
}
```

Key: ChangeSet is not “commit list”. It’s an object that links diffs + evidence.

### 2.3 PullRequest (external object)
ALMS should treat PRs as first-class but keep the model minimal:
```json
{
  "pr_id": "PR-<provider>:<number>",
  "work_item_id": "WI-...",
  "branch": "feature/...",
  "url": "...",
  "checks": {"status": "pending"},
  "review_state": {"blocking": 2, "non_blocking": 3}
}
```

### 2.4 ReviewRequest / ReviewResult
This is the formal handshake.

ReviewRequest:
```json
{
  "review_request_id": "RR-<uuid>",
  "pr_id": "...",
  "reviewers": [
    {"agent": "mustafa", "focus": ["api", "tests"]},
    {"agent": "atlas", "focus": ["security", "correctness"]}
  ],
  "deadline": "<optional>",
  "status": "pending"
}
```

ReviewResult:
```json
{
  "review_request_id": "RR-...",
  "status": "completed",
  "blocking": [
    {"id": "B-1", "title": "Missing invariant: tool_start/tool_end pairing", "suggested_fix": "...", "patch_artifact": "A-<uuid>"}
  ],
  "non_blocking": [
    {"id": "N-1", "title": "Rename var for clarity"}
  ],
  "summary": "..."
}
```

### 2.5 MergeDecision
A structured “can we merge?” object.

```json
{
  "merge_decision_id": "MD-<uuid>",
  "pr_id": "...",
  "status": "blocked",
  "requirements": {
    "checks": "required",
    "blocking_comments": 0,
    "spec_refs": "required",
    "approvals": "required_if_guarded"
  },
  "evidence": {
    "checks": ["A-<uuid>"],
    "review_results": ["RR-<uuid>"]
  }
}
```

---

## 3) Workflow events (what gets emitted)

These events should exist in the same event system (they can be separate streams):

- `work_item_created`
- `changeset_proposed` / `changeset_applied`
- `pr_opened`
- `review_requested`
- `review_received`
- `checks_failed` / `checks_passed`
- `merge_blocked` / `merge_ready` / `merged`
- `spec_drift_detected`

Each event should link to IDs (work_item_id, pr_id, etc.) and to artifacts.

---

## 4) Invariants (to prevent chaos)

1) **No merge with blocking items**
- `blocking == 0` required.

2) **No merge without evidence**
- checks must be passing, or explicitly waived with an audit record.

3) **Spec refs required (by default)**
- WorkItems and PRs must reference spec IDs/anchors unless labeled `spike`.

4) **Approval posture is explicit**
- If posture is `guarded`, approvals must be satisfied.

5) **Everything is auditable**
- “who merged what and why” is in audit.

---

## 5) UX / CLI mapping (Claude Code feel)

### Commands that should exist (conceptual)
- `alms work start "<goal>" --repo <path>`
- `alms work status WI-...`
- `alms changeset propose WI-...`
- `alms changeset apply CS-...` *(diff-first preview, then apply)*
- `alms pr open WI-...`
- `alms pr request-review PR-... --reviewer mustafa --focus security`
- `alms pr iterate PR-...` *(consume review artifacts, fix, re-test)*
- `alms pr merge PR-...` *(only if MergeDecision satisfied)*

### Run Timeline + Artifacts Drawer
- Timeline shows policy decisions and approvals inline.
- Artifacts drawer shows diffs, test logs, review patches, doc diffs.

---

## 6) The Docs/Requirements agent role (thin, enforceable)

This role is your “product nervous system”. Responsibilities:
- convert discussions into spec sections/IDs
- generate WorkItems + acceptance criteria
- detect spec drift: code changed but spec didn’t
- keep tasks aligned with spec_refs

Avoid bureaucracy:
- specs can start as anchors in markdown
- enforcement can be soft at first (warnings), then policy-gated for merges

---

## 7) Minimal MVP slice (to start feeling magical)

To get the Claude Code / team automation feeling fast:

1) WorkItem + ChangeSet + PR open
2) ReviewRequest/ReviewResult handshake
3) Author agent iterates until blocking=0
4) MergeDecision gates merge

Everything else can come later.

---

*Authored by Mesut (2026-02-14).* 

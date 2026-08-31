# Engineering Reviews

Every change to ALMS went through code review before merge. This directory preserves a
selection of those reviews — not as a highlight reel, but as evidence of how the project
was actually built.

The reviews were produced by a four-agent development team coordinated by a single human
operator. That arrangement is documented in
[`docs/multi-agent-development-workflow.md`](../multi-agent-development-workflow.md); what
matters here is the division of labour it created.

| Role | Responsibility | Writes code |
|------|----------------|-------------|
| **Atlas** | Coordinator — the session the human talks to | Yes |
| **Heph** | Feature development, in an isolated worktree | Yes |
| **Larry** | Bug fixing, in an isolated worktree | Yes |
| **Tim** | Code review | **No — read-only by construction** |
| *Human* | Sets direction, approves every merge | — |

Tim holds no write tools. He cannot fix what he finds, only describe it precisely enough
that someone else can. That constraint is why the reviews below read the way they do:
a reviewer who cannot reach for the keyboard has to make the argument instead.

## By the numbers

| | |
|---|---|
| Pull requests | 647 |
| Issues | 671 |
| Review write-ups | 812, across 556 distinct PRs |
| Total review prose | ~4.3 million characters |
| Median review length | ~5,000 characters |
| PRs needing more than one round | 188 |
| Most rounds on a single PR | 7 |
| Period | March – August 2026 |

No change merged on the reviewer's say-so alone. A verdict of *Ready to merge* was a
recommendation to the human operator, never an action.

## Anatomy of a review

Reviews follow a fixed shape, which is what makes 812 of them comparable to each other:

- **Verdict** — `Ready to merge` or `Needs minor fixes`, stated first, with the reason.
- **Critical** — defects that block the merge. Often empty; when populated, specific.
- **Suggestions** — real problems that do not block, including documentation the change
  has silently made untrue.
- **Nits** — small things, explicitly marked as optional.
- **What I verified rather than accepted** — the claims the reviewer actually checked
  against the code, as opposed to those taken on trust from the PR description.

That last section is the load-bearing one. A review that lists what it *did not* verify is
more useful than one that implies it checked everything.

## The threads

Seven threads, chosen for variety rather than length — a security find, a full fix cycle,
a three-round grind, a disagreement the reviewer loses, a design-stage review, and two
pieces of architecture analysis written before any code existed.

| Thread | What makes it worth reading |
|--------|------------------------------|
| [A review that found a cross-agent data-loss bug](1288-subagent-session-ownership.md) | Accepted the change, then traced what else the new key reached — and found that deleting one agent would destroy another's transcripts. |
| [A full review → fix → re-review cycle](1253-fk-degradation-counter.md) | Both rounds on one PR: two Criticals raised, then each finding walked to closure against the fix commit. |
| [Three rounds, each finding what the last missed](1271-live-default-model.md) | Every round re-verifies the previous round's findings before hunting new ones. Round three finds a fourth path that commits half a change. |
| [Where the reviewer is argued down](1245-quarantine-accounting.md) | Opens by conceding the point — *"the argument you added is stronger than mine"* — then narrows the remaining disagreement to four clauses. |
| [Interrogating a design boundary](1267-interrupted-dm-notification.md) | Ignores the diff and asks whether the boundary is drawn in the right place at all, then tests that against every path routing through it. |
| [Reviewing a refactoring plan before any code](0436-refactoring-plan.md) | Module boundaries, dependency direction, migration order, PR granularity, and the risks the plan had not accounted for. |
| [Design analysis with options weighed](0586-context-alternation.md) | Four candidate approaches with their failure modes, a recommendation, an implementation sketch, and an explicit *what I would push back on*. |

## Provenance

These are verbatim reproductions of review comments from the project's original issue
tracker, reformatted as Markdown and otherwise unedited. Cross-references such as `#1246`
point at that tracker; they are retained because the reviews reason about each other, and
stripping them would break arguments that depend on what an earlier review concluded.

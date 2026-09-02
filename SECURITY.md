# Security Policy

## Reporting a vulnerability

Please report security issues privately through GitHub's
[private vulnerability reporting](https://github.com/alpercodes/alms/security/advisories/new)
rather than opening a public issue.

Expect an initial response within **7 days**. ALMS is maintained by one person, so
please allow reasonable time for a fix before public disclosure. Ninety days is a fair
default; if a fix is going to take longer than that, I will say so rather than go quiet.

Please include what you would want to receive yourself: affected version or commit,
configuration involved, reproduction steps, and what an attacker gains.

## Supported versions

Only the latest release on `develop` receives security fixes. ALMS is pre-1.0 and there
are no maintained release branches.

## Threat model — read this before deploying

ALMS runs LLM-directed agents that execute tools on your machine. Several properties
below are **deliberate design decisions, not bugs**, and reports about them will be
closed as working-as-intended. They are listed here because you should know them before
you run this.

### One operator, full trust

ALMS assumes a **single operator with full trust over their own daemon**. There is no
multi-user model, no per-user isolation, and no privilege separation between agents and
the person running them. Multi-tenant deployment is out of scope. Do not expose the
gateway to untrusted users.

### Agents can read the secrets store

The filesystem sandbox root is the **project root**, and the ALMS metadata directory
`.alms/` lives inside it. That means `.alms/secrets.json` and `.alms/alms.db` resolve
inside the sandbox, and an agent can read them with `fs_read`. This was a deliberate
trade for making agents operate on the project the way an operator does. Treat any
secret reachable by an agent as disclosed to the model provider.

### Sandboxing is not equal across platforms

| Platform | Enforcement |
|----------|-------------|
| Linux    | OS-level via Landlock, plus application-layer checks |
| Windows  | Application-layer only — path canonicalization, shell classification, permission gates |
| macOS    | Application-layer only |

On Windows and macOS a sufficiently creative command that defeats the string-level
classifier is not stopped by a second layer. The classifier is defense-in-depth, not a
boundary. See [`docs/security-model.md` § 4.4](docs/security-model.md).

### The escape hatch is real

`[security].allow_full_os_access` disables containment entirely. It exists because some
workloads need it. If you set it, ALMS is a remote code execution service that you have
pointed at your own machine on purpose.

### Prompt injection is unsolved

Tool output and fetched content enter the model's context. A malicious repository, web
page, or file can attempt to steer an agent. ALMS mitigates with output truncation,
classification, and approval gates — none of these are a guarantee. Reports of *novel*
injection vectors that defeat a specific control are in scope; "prompt injection exists"
is not.

## In scope

- Sandbox escapes on Linux (path traversal past the pinned root, Landlock bypass)
- Shell classifier bypasses that reach a destructive command under `block_destructive` or `strict`
- Approval-gate bypasses — a tool executing without required approval
- Secret disclosure beyond the documented `.alms/` posture, e.g. keys in logs, audit
  records, error strings, or SSE frames
- Authentication or authorization flaws in the HTTP/SSE gateway
- SQL injection, or session/run data leaking across session boundaries

## Out of scope

- Anything listed under the threat model above
- Attacks requiring `allow_full_os_access`, an already-compromised operator account, or
  physical access
- Vulnerabilities in dependencies without a demonstrated exploit path through ALMS
- Rate limiting and denial of service against your own daemon

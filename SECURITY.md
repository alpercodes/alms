# Security Policy

## Reporting a vulnerability

**Preferred:** report privately through GitHub's
[private vulnerability reporting](https://github.com/alpercodes/alms/security/advisories/new).

**If that page returns 404**, private vulnerability reporting has not been enabled on this
repository yet. In that case, open a public issue containing **only** a request for a
private channel — a title like `security report — requesting a private channel`, no
technical detail, no reproduction — and a private advisory will be opened for you to
disclose into. Do not put the finding itself in a public issue.

Expect an initial response within **7 days** through either route. ALMS is maintained by one
person, so please allow reasonable time for a fix before public disclosure. Ninety days is a
fair default; if a fix is going to take longer than that, I will say so rather than go quiet.

Please include what you would want to receive yourself: affected version or commit,
configuration involved, reproduction steps, and what an attacker gains.

## Supported versions

Only the latest tagged release on `main` receives security fixes. `main` carries release
merges; `develop` is the integration branch and is not a supported target. ALMS is pre-1.0
and there are no maintained release branches.

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
trade for making agents operate on the project the way an operator does.

The mitigation is encryption at rest: with `ALMS_MASTER_KEY` set, `secrets.json` is stored
AES-256-GCM encrypted, and `ALMS_MASTER_KEY` is on the shell tool's secret-env denylist, so
the daemon's shell children never receive it. An agent that reads the file gets ciphertext.
Without that variable the file is plaintext — treat every secret in it as disclosed to your
model provider.

### `shell` has no filesystem boundary on Windows or macOS

The `fs_*` tools (`fs_read`, `fs_write`, `fs_list`, `fs_edit`, `fs_grep`, `fs_glob`) resolve
every path and refuse anything outside the pinned project root. That check is
platform-independent — it is the same code on Linux, Windows and macOS.

The `shell` tool is different, and this is the weakest point in the system:

| Platform | `fs_*` tools | `shell` tool |
|----------|--------------|--------------|
| Linux 5.13+ | Project root enforced | Landlock LSM confines the child process to the sandbox root |
| Linux < 5.13, or a container blocking the Landlock syscalls | Project root enforced | **No filesystem boundary** — Landlock degrades to unsandboxed |
| Windows, macOS | Project root enforced | **No filesystem boundary** |

`shell` does not inspect paths inside the command on any platform. The last check that did
was removed and never replaced. Where Landlock is not in force, `cd /etc && ls` returns the
listing to the model; the working directory is reverted afterwards and the agent is told,
but the command has already run. What remains is the `[tools.shell_permissions]` regex list
and the destructive-command classifier — both match command *strings* (`rm -rf /`, `mkfs.`,
`dd if=`), so they are not a path boundary, and defeating them is not what gets an agent out
of the project root. Nothing is holding it in.

Two further points for Linux operators: the Landlock fallback is **fail-open**, and it
announces itself only as `[alms] Landlock not supported by kernel, running unsandboxed` on
the child's stderr — not through `tracing`, so it does not reach structured logs.

Where Landlock is not available, run the daemon as a **low-privilege OS user with filesystem
ACLs** limiting it to the project root. Full detail in
[`docs/security-model.md` § 4.4](docs/security-model.md#44-filesystem-sandboxing-implemented).

### The escape hatch is real, and a model can reach it

`[security].allow_full_os_access` is a list of agent names, not a boolean. A listed agent
runs with **no filesystem sandbox** — its `fs_*` and `shell` operate against the real root.
The shell permission list and the destructive-command classifier still apply to it; they are
independent operator policy, not part of the sandbox.

Names are matched case-insensitively against the name a subagent call supplies, and an
`invoke_agent` name that matches no registry entry is passed through as the model wrote it.
So **any** agent can invoke a listed name and get an unsandboxed subagent, whether or not an
agent by that name is registered. Listing a name grants it to the fleet, not to one agent.

### Prompt injection is unsolved

Tool output and fetched content enter the model's context. A malicious repository, web
page, or file can attempt to steer an agent. ALMS mitigates with output truncation,
classification, and approval gates — none of these are a guarantee. Reports of *novel*
injection vectors that defeat a specific control are in scope; "prompt injection exists"
is not.

## In scope

- Filesystem sandbox escapes in the `fs_*` tools on **any** platform — path traversal past
  the pinned project root, including canonicalization-form mismatches on Windows
- Landlock bypasses on Linux — a shell child escaping the kernel ruleset on a kernel that
  supports it
- Shell classifier bypasses that reach a destructive command under `block_destructive` or `strict`
- Approval-gate bypasses — a tool executing without required approval
- Secret disclosure beyond the documented `.alms/` posture, e.g. keys in logs, audit
  records, error strings, or SSE frames
- Authentication or authorization flaws in the HTTP/SSE gateway
- SQL injection, or session/run data leaking across session boundaries

## Out of scope

- Anything listed under the threat model above, including the absence of a `shell`
  filesystem boundary on Windows, macOS, and pre-5.13 Linux
- Attacks that require the operator to have listed the target agent under
  `allow_full_os_access`, an already-compromised operator account, or physical access
- Vulnerabilities in dependencies without a demonstrated exploit path through ALMS
- Rate limiting and denial of service against your own daemon

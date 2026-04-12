# WASM Sandbox vs OpenClaw Skills — Competitive Analysis

A comparison of ALMS's WASM tool sandbox against OpenClaw's skill ecosystem, and the path forward.

---

## OpenClaw's Skill System

OpenClaw skills are natural-language plugin packages. Each skill is a `SKILL.md` file — markdown instructions that the LLM reads and follows at runtime.

### How it works
1. Agent starts, loads a skill manifest from the local filesystem
2. During context assembly, a streamlined manifest is injected into the LLM prompt
3. When the LLM decides a skill is relevant, it reads that skill's full `SKILL.md` on demand
4. The LLM follows the instructions — which can include running shell commands, accessing files, making network requests — with the full privileges of the host process

### Scale
- 13,700+ community skills on ClawHub (public registry)
- 5,400+ curated in community "awesome" lists
- Writing a skill is trivial — it's just a markdown file

### Security record
- **1,184+ confirmed malicious skills** on ClawHub
- Estimates of **12–20% of all skills being malicious** depending on the study
- 21,639 exposed OpenClaw instances found by Censys (21x increase in one week)
- ClawHavoc campaign: 341 skills delivering AMOS malware
- VirusTotal partnership launched (auto-scanning), but maintainers acknowledge it can't catch prompt injection payloads
- The fundamental problem: skills are natural-language instructions with full host access. There is no isolation boundary.

---

## ALMS's WASM Sandbox

Completely different model — tools are compiled binaries, not text instructions.

### Architecture
- **Wasmtime runtime** (JIT/AOT WASM execution)
- **Instance-per-call**: fresh WASM instance created and destroyed per invocation. No state leaks between calls.
- **Defined ABI** (v0): host writes JSON input to WASM memory, calls entry point, reads JSON result back
- **WASI disabled by default**: WASM tools cannot access host filesystem, network, or environment unless explicitly granted

### Resource limits (host-enforced)
- Max memory: 64 MB (configurable per tool)
- Max input: 1 MB
- Max output: 4 MB
- Timeout: 30 seconds
- CPU: fuel metering (10 billion units default)

### Built-in tools (native Rust, still sandbox-enforced)
- `echo`, `math` — safe compute
- `http_get` — network access
- `shell_exec` — argv-only (no shell interpolation), env_clear(), cwd restricted to sandbox root, kill_on_drop
- `fs_read`, `fs_write`, `fs_list`, `fs_edit` — path canonicalization + prefix checking prevents traversal/symlink escapes
- `invoke_agent`, `get_task_result`, `read_subagent_session` — multi-agent coordination

### Current state
- Infrastructure is complete and tested
- **Zero third-party WASM tools shipped** — the ecosystem doesn't exist yet
- No SDK for authoring tools, no registry, no `alms tool new` scaffolding

---

## The Gap — and the Opportunity

### Competing on breadth is impossible
OpenClaw has 13,700 skills because writing markdown is trivial. ALMS won't match that volume.

### The security angle is real
OpenClaw's marketplace is a proven security disaster. ALMS can offer something they architecturally cannot: **runtime-enforced capability isolation**. A WASM tool that declares it needs network access but tries to read the filesystem gets blocked at the boundary — not caught after the fact by a virus scanner.

### What ALMS could build

1. **Secure tool registry** — Tools are WASM binaries with declared capabilities. The host enforces capabilities at runtime. Users see what a tool requests before installing (Android permissions model for AI tools).

2. **Tool authoring SDK** — Rust SDK (and AssemblyScript/Go for reach) that hides the raw ABI. `alms tool new` scaffolding. Local testing before publishing.

3. **Capability declarations as trust signals** — Each tool declares: network domains, filesystem paths, shell access, memory budget. Reviewable, enforceable, auditable.

4. **Hybrid adoption layer** — Wrap OpenClaw-style natural-language skills in a restricted sandbox (limited shell, no network by default, filesystem jail) to bootstrap volume while the native WASM ecosystem grows.

---

## Can WASM tools do everything OpenClaw skills can?

**Yes** — and with stronger guarantees. Every category of OpenClaw skill has a WASM equivalent that's more secure.

Here's what OpenClaw skills typically do, mapped to how ALMS WASM tools would handle it:

| OpenClaw skill category | How it works in OpenClaw | ALMS WASM equivalent |
|---|---|---|
| **Web scraping** (e.g. "scrape HN frontpage") | Skill tells LLM to run `curl` or use `http_get` with full host access | WASM tool compiled with HTTP client, granted specific domain allowlist. Cannot access other domains. |
| **File management** (e.g. "organize downloads") | Skill tells LLM to run shell commands on arbitrary paths | WASM tool with declared fs paths. Sandbox enforces directory boundary — can't escape to `/etc/passwd`. |
| **API integrations** (e.g. "post to Slack") | Skill tells LLM to `curl` an API with a token from env | WASM tool compiled with API client. Receives only declared env vars. Cannot see other secrets. |
| **Code execution** (e.g. "run Python snippet") | Skill tells LLM to pipe code to `python3` | WASM tool running a WASM-compiled interpreter (e.g. wasm-python). Fully sandboxed, no host access. |
| **Browser automation** | Skill tells LLM to run Puppeteer/Playwright commands | WASM tool that emits structured browser commands; host executes them through a capability-gated bridge. |
| **System admin** (e.g. "check disk usage") | Skill tells LLM to run `df`, `top`, etc. | WASM tool with declared shell commands on an allowlist. Only those commands permitted. |
| **Data transformation** (e.g. "convert CSV to JSON") | Skill tells LLM to write a script or use `jq` | Pure WASM — no host access needed at all. Safest category. |

The key difference: in OpenClaw, every skill can do **everything** (because it's just text telling the LLM what to do, and the LLM has full host access). In ALMS, each tool can only do **what it declared** — and the runtime enforces it.

### What a WASM tool can't easily do (today)

- **Interactive/streaming operations** — The ABI is request/response (no streaming). Long-running tools like "monitor this log file" would need ABI v1 extensions.
- **Rich UI rendering** — OpenClaw skills can tell the LLM to emit formatted cards, buttons, etc. WASM tools return JSON. The presentation layer would need to be in the host/UI.
- **LLM access from within the tool** — OpenClaw skills can ask the LLM to reason mid-execution. A WASM tool is pure compute; if it needs LLM calls, that's a host callback (not yet implemented).

These are solvable with ABI extensions, not fundamental limitations.

---

## What creating a WASM tool would look like

### Today (raw ABI v0 — painful)

A developer writing a tool today would need to:

1. Write Rust (or C/AssemblyScript) that exports `alms_tool_call` and `alms_alloc`
2. Parse the JSON input envelope manually
3. Do the work
4. Write the JSON result with a 4-byte length prefix to WASM memory
5. Compile to `wasm32-unknown-unknown`
6. Register the `.wasm` binary with the ALMS host

Example — a "word count" tool in raw Rust/WASM:

```rust
use std::alloc::{alloc, Layout};

#[no_mangle]
pub extern "C" fn alms_alloc(len: i32) -> i32 {
    let layout = Layout::from_size_align(len as usize, 4).unwrap();
    unsafe { alloc(layout) as i32 }
}

#[no_mangle]
pub extern "C" fn alms_tool_call(ptr: i32, len: i32) -> i32 {
    // Read input JSON from memory
    let input = unsafe {
        let slice = std::slice::from_raw_parts(ptr as *const u8, len as usize);
        String::from_utf8_lossy(slice)
    };

    // Parse envelope: {"abi": 0, "tool": "word_count", "params": {"text": "..."}}
    // (in practice you'd use serde_json, but keeping it minimal here)
    let text = /* extract params.text from input */;
    let count = text.split_whitespace().count();

    // Build result
    let result = format!(r#"{{"ok":true,"result":{{"count":{count}}}}}"#);
    let bytes = result.as_bytes();

    // Write length-prefixed result to memory
    let out_len = bytes.len() as u32;
    let layout = Layout::from_size_align(bytes.len() + 4, 4).unwrap();
    let out_ptr = unsafe { alloc(layout) };
    unsafe {
        std::ptr::copy_nonoverlapping(out_len.to_le_bytes().as_ptr(), out_ptr, 4);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out_ptr.add(4), bytes.len());
    }
    out_ptr as i32
}
```

Then compile and register:
```bash
cargo build --target wasm32-unknown-unknown --release
# Register with ALMS host (API or config)
```

This works but is **too much friction** for adoption. Nobody will write this instead of a 10-line SKILL.md.

### Future (with SDK — the goal)

With an `alms-tool-sdk` crate, the same tool becomes:

```rust
use alms_tool_sdk::prelude::*;

#[alms_tool(
    name = "word_count",
    description = "Count words in text",
    capabilities = []  // pure compute, no host access needed
)]
fn word_count(text: String) -> ToolResult {
    let count = text.split_whitespace().count();
    ok(json!({ "count": count }))
}
```

The SDK macro handles:
- ABI boilerplate (alloc, memory layout, length prefix)
- JSON serialization/deserialization
- Parameter schema generation (for LLM tool definitions)
- Capability declarations in the binary metadata

Authoring workflow:
```bash
alms tool new word_count          # scaffolds project with Cargo.toml + template
# edit src/lib.rs
alms tool build                   # compiles to .wasm, validates ABI
alms tool test                    # runs locally against mock host
alms tool publish                 # uploads to registry with capability manifest
```

A tool that needs host access would declare it:

```rust
#[alms_tool(
    name = "check_disk",
    description = "Check disk usage for a path",
    capabilities = [shell_exec(["df", "du"])]  // only these commands allowed
)]
fn check_disk(path: String) -> ToolResult {
    let output = host::shell_exec("df", &["-h", &path])?;
    ok(json!({ "usage": output.stdout }))
}
```

The user installing this tool sees: "check_disk requests: shell access (df, du only)". The runtime enforces it.

### For non-Rust developers

AssemblyScript (TypeScript-like, compiles to WASM) would be the secondary target:

```typescript
import { tool, ok, ToolResult } from "@alms/tool-sdk";

@tool({
  name: "word_count",
  description: "Count words in text",
  capabilities: []
})
export function wordCount(text: string): ToolResult {
  const count = text.split(/\s+/).filter(w => w.length > 0).length;
  return ok({ count });
}
```

---

## Summary

ALMS's WASM sandbox is architecturally superior to OpenClaw's natural-language skills. It provides real isolation that OpenClaw fundamentally cannot offer. But architecture without ecosystem is just potential.

The path to making it real:
1. **Build the SDK** — make authoring a tool as easy as writing a function
2. **Ship 20–30 first-party tools** — cover the common categories (web, files, APIs, data transforms)
3. **Launch a registry** with capability-based trust signals
4. **Optional**: compatibility layer for OpenClaw skills (sandboxed) to bootstrap volume

The security narrative writes itself after the ClawHub disasters. The question is whether ALMS can lower the authoring friction enough to build an ecosystem.

---

*Research by Tesla (2026-03-15).*
*Sources: ALMS codebase (crates/alms-sandbox/), OpenClaw docs, ClawHub registry, web research.*

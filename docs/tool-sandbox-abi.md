# Tool Sandbox ABI (MVP Spec)

**Status:** Design-only. The WASM substrate this ABI targeted was removed from the codebase (see commit history of `crates/alms-sandbox/src/sandbox.rs`). This spec is retained for reference in case WASM tooling is revived. Native builtin tools ship as compiled-in Rust and do not use this ABI.

## Goal
Define a stable ABI for tool execution between ALMS runtime and WASM sandbox.

This ABI is explicitly designed for:
- debuggability (JSON payloads)
- host-enforced safety (timeouts, size caps)
- a clear contract that doesn’t rely on implementation quirks

---

## MVP ABI (v0)

### 1) Entry point
WASM module must export:
- `alms_tool_call(ptr: i32, len: i32) -> i32`

### 2) Input encoding
- `ptr/len` points to UTF‑8 JSON payload.
- The payload **must** include an ABI version field:

```json
{ "abi": 0, "tool": "name", "params": { ... } }
```

### 3) Output encoding
- Return value is a pointer to a buffer whose first 4 bytes are a **u32 little‑endian** length, followed by UTF‑8 JSON.

Success:
```json
{ "ok": true, "result": { ... } }
```

Error (MVP):
```json
{ "ok": false, "error": "message" }
```

Error (recommended shape for post-MVP):
```json
{ "ok": false, "error": { "code": "INVALID_PARAMS", "message": "..." } }
```

### 4) Memory allocation
WASM module must export:
- `alms_alloc(len: i32) -> i32`

Host allocates input via `alms_alloc`, writes JSON bytes, then calls `alms_tool_call`.

#### Allocator contract (must be explicit)
- `alms_alloc(len)` returns a pointer to a region of **at least `len` bytes**.
- The region must live in the module’s exported `memory`.
- Failure behavior: return `0` to signal allocation failure.
- Alignment: pointer must be aligned to at least 4 bytes.

### 5) Payload size limits (host enforced)
To avoid DoS and simplify implementation, the host enforces:
- **max input bytes** (e.g. 1 MiB)
- **max output bytes** (e.g. 4 MiB)

Exact defaults can be configuration-driven, but the existence of limits is part of the contract.

### 6) Timeouts
Host enforces wall‑clock timeout per call; on timeout, the sandbox instance is terminated.

### 7) Memory lifetime (explicit MVP choice)
**MVP guarantee: instance-per-call**
- The host creates a fresh WASM instance per invocation and tears it down after reading the result.
- Memory is reclaimed by instance teardown; therefore an explicit `alms_free` is not required for MVP.

---

## Non‑Goals (MVP)
- Streaming tool results
- Zero‑copy buffers
- Multi‑call sessions

---

## Testing requirements
At minimum:
- Golden test with a sample WASM tool
- Malformed JSON input
- Oversized output
- Timeout termination

**See also:** `docs/testing-strategy.md`

---
*Date: 2026-02-10*

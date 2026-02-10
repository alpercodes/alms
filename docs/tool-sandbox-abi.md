# Tool Sandbox ABI (MVP Spec)

**Status:** MVP draft

## Goal
Define a stable ABI for tool execution between ALMS runtime and WASM sandbox.

## MVP ABI (v0)
### 1) Entry point
WASM module must export:
- `alms_tool_call(ptr: i32, len: i32) -> i32`

### 2) Input encoding
- `ptr/len` points to UTF‑8 JSON payload:
```json
{ "tool": "name", "params": { ... } }
```

### 3) Output encoding
- Return value is a pointer to a buffer whose first 4 bytes are a **u32 little‑endian** length, followed by UTF‑8 JSON:
```json
{ "ok": true, "result": { ... } }
```
On error:
```json
{ "ok": false, "error": "message" }
```

### 4) Memory allocation
WASM module must export:
- `alms_alloc(len: i32) -> i32`
- `alms_free(ptr: i32, len: i32)` (optional for MVP)

Host allocates input via `alms_alloc`, writes JSON, then calls `alms_tool_call`.

### 5) Timeouts
Host enforces wall‑clock timeout per call; on timeout, sandbox instance is terminated.

---

## Non‑Goals (MVP)
- Streaming tool results
- Zero‑copy buffers
- Multi‑call sessions

## Next Steps
- Add ABI versioning field to input JSON.
- Implement allocator contract in `alms-sandbox`.
- Add tests with a sample WASM tool.

---
*Date: 2026-02-10*

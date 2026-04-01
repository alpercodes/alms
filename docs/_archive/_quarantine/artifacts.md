# ALMS Artifacts (spec)

Artifacts are **stored objects** referenced by ID. They exist to prevent:
- huge tool outputs bloating events/audit logs
- secrets accidentally being persisted in transcripts
- losing important outputs when streams reconnect

Artifacts are a bridge between:
- real-time **events** (stream small things)
- **audit** (record what happened, with redacted references)
- **storage** (keep the full payload where appropriate)

**See also:**
- `docs/events-and-audit.md` (event invariants, tool_end)
- `docs/security-model.md` (redaction, secrets)
- `docs/api.md` (planned audit/artifact endpoints)

---

## 0) Goals

- Keep event streams fast and bounded.
- Keep audit logs safe (redacted + minimal) while still accountable.
- Support “replay” and reconnect without dumping megabytes into SSE.
- Provide a clean place to store binary data (images/files) and large text blobs.

Non-goals (MVP):
- external blob storage
- complex lifecycle management
- full content indexing/search

---

## 1) When to use an artifact

Create an artifact when:
- tool output exceeds a size threshold (example: > 64KB)
- output is binary (images, files)
- output may contain secrets and must be stored separately with stricter rules
- output must be re-downloadable for UX (logs, reports)

Do NOT create an artifact when:
- output is small and safe (a few KB)
- output is already redacted and bounded

Rule of thumb:
- **Events**: small, frequent, UI-oriented.
- **Artifacts**: large, durable, potentially sensitive.

---

## 2) Artifact kinds (MVP)

Minimal set:
- `tool_output` — large tool result payload (JSON/text)
- `file_blob` — binary file (image, pdf, etc.)
- `log_chunk` — large logs (shell output, build logs)

Post-MVP:
- `report` — generated documents
- `dataset` — structured data exports

---

## 3) Artifact record shape (conceptual)

```json
{
  "artifact_id": "<uuid>",
  "created_at": "2026-02-11T17:00:00Z",
  "kind": "tool_output",
  "content_type": "application/json",
  "size_bytes": 12345,
  "sha256": "...",
  "storage": {
    "backend": "local",
    "path": "artifacts/<artifact_id>.json"
  },
  "redaction": {
    "applied": true,
    "notes": "..."
  },
  "links": {
    "session_id": "<uuid>",
    "run_id": "<uuid>",
    "tool_invocation_id": "<uuid>"
  }
}
```

Notes:
- `sha256` is for integrity (and optional dedup later).
- `links` should be present so artifacts remain traceable.

---

## 4) Event integration

In `tool_end`, prefer:
- small outputs inline in `result`
- large outputs referenced by `artifact_id`

Example `tool_end`:
```json
{
  "tool_invocation_id": "<uuid>",
  "ok": true,
  "duration_ms": 120,
  "result": {
    "artifact_id": "<uuid>",
    "summary": "Downloaded 2.1MB HTML"
  }
}
```

### Event invariants
- `tool_start`/`tool_end` pairing still holds; the artifact is a detail of `tool_end.result`.
- The event stream must never include raw large payloads beyond configured limits.

---

## 5) Audit integration

Audit records should:
- store redacted/truncated request + result
- include `artifact_id` when applicable

Never store raw secrets in audit. If needed, store:
- a hash/digest
- reference to a protected artifact

Recommended approach:
- audit record contains a **redacted summary** + `artifact_id`
- full payload lives only in artifact storage (with stricter access)

---

## 6) Storage (MVP)

### Backend
MVP backend: **local filesystem**.

Suggested layout:
- `data/artifacts/<artifact_id>.<ext>`

### Naming
- Use `artifact_id` as filename (avoid user-controlled names).
- Extension derived from `content_type` (best effort).

---

## 7) Access control (MVP)

Artifacts are more sensitive than events.

Minimum:
- only accessible to localhost by default
- require auth token if ALMS is bound to non-localhost

Rule:
- access to an artifact must be authorized in the context of its `links.session_id` / principal.

---

## 8) Retention (MVP)

MVP retention policy can be simple:
- keep artifacts for N days, or until explicit deletion

Future:
- per-session retention
- per-artifact sensitivity levels
- user-configurable “keep forever” flags

---

## 9) Security notes

- Prevent path traversal (never accept raw paths from clients).
- Consider encrypting at rest post-MVP.
- Treat artifacts as potential secrets: redact or avoid storing when possible.

---

## 10) Examples (recommended patterns)

### Example A — Large shell output
Scenario: `shell_exec` runs a build and produces 500KB of logs.

Events:
- `tool_start` includes limits (`max_output_bytes`, `timeout_ms`).
- `tool_end` returns an artifact reference:
```json
{
  "tool_invocation_id": "...",
  "ok": true,
  "result": {
    "artifact_id": "<uuid>",
    "summary": "Build finished; logs stored as artifact",
    "truncated": true
  }
}
```

Audit:
- store redacted summary + artifact_id
- do not inline the full logs into audit

Artifact:
- `kind: log_chunk`
- `content_type: text/plain`

### Example B — Image output
Scenario: a tool produces a PNG (diagram, screenshot).

Event:
```json
{
  "tool_invocation_id": "...",
  "ok": true,
  "result": {
    "artifact_id": "<uuid>",
    "summary": "Generated diagram.png"
  }
}
```

Artifact:
- `kind: file_blob`
- `content_type: image/png`

### Example C — HTTP fetch (HTML)
Scenario: `http_get` fetches a webpage; response is 2.1MB HTML.

Event:
```json
{
  "tool_invocation_id": "...",
  "ok": true,
  "result": {
    "artifact_id": "<uuid>",
    "summary": "Fetched https://example.com (2.1MB HTML)",
    "content_type": "text/html"
  }
}
```

Audit:
- record URL, status code, and artifact_id
- consider storing only extracted text post-MVP to reduce injection surface

---

## 11) Planned API surface (post-MVP)

Suggested endpoints:
- `GET /artifacts/{artifact_id}` (download)
- `GET /sessions/{session_id}/artifacts` (list)

The event/audit model should work even before these endpoints exist.

---

*Authored by Mesut (2026-02-11). Updated for clarity and integration.*

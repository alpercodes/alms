# ALMS Developer Onboarding (complete guide)

This is your “first 5 minutes to running ALMS + making a change” guide. It assumes you have a local clone of the canonical repo.

**Canonical repo:** `</srv/alms`

---

## 1) Prerequisites (1 minute)

```bash
# Install Rust if missing
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Verify
rustc --version  # stable 1.75+
cargo --version
```

---

## 2) Clone & build (2 minutes)

```bash
cd </srv/alms
git pull origin main  # sync canonical

cargo build  # ~2min first time
```

---

## 3) Run daemon (30 seconds)

```bash
# Via CLI (recommended)
cargo run --bin alms -- gateway --bind 127.0.0.1:8080

# Health check (new tab)
curl http://127.0.0.1:8080/health
```

**Expected output:**
```json
{"status":"healthy","service":"alms-gateway","version":"0.1.0"}
```

**Configure Telegram (optional)**
```
export TELEGRAM_BOT_TOKEN="your_token"
cargo run --bin alms -- gateway --bind 127.0.0.1:8080
```

---

## 4) Run tests (30 seconds)

```bash
cargo test  # unit + integration
```

**Golden tests** (SSE events, tool sequences):
- Located in `crates/alms-gateway/tests/`
- Update snapshots with `cargo test -- --test-threads=1`

**Test a specific feature:**
```bash
cargo test tool_execution
```

---

## 5) Common workflows

### Add a tool
1. Define in `crates/alms-sandbox/src/builtin.rs` (or WASM ABI)
2. Register in `ToolRegistry`
3. Test with `cargo test`
4. Verify events/audit in SSE stream

**Docs:** `docs/tool-sandbox-abi.md`, `docs/security-model.md`

### Add a channel adapter
1. Implement `Channel` trait in `crates/alms-channel`
2. Wire into `Gateway::initialize_channels`
3. Test message → event flow

### Debug daemon
```
RUST_LOG=debug cargo run --bin alms -- gateway
```

### Policy/approval testing
- Set posture to `guarded`
- Trigger `approval_required` event
- Verify `tool_start` only after `approval_resolved`

---

## 6) Troubleshooting

**Daemon crashes on startup:**
- Check `RUST_LOG=error` output
- Verify `TELEGRAM_BOT_TOKEN` if using Telegram
- Run `cargo check` for compile issues

**Tests fail:**
- `cargo clean && cargo test`
- Check golden file diffs

**No SSE events:**
- Verify `POST /agent/run/stream` request has valid `session_id`
- Check daemon logs for run errors

---

## 7) Workflow (PRs)

See `CONTRIBUTING.md`:
- Feature branch in your workspace
- PR summary in chat
- Merge to canonical only after approval

---

## 8) Key docs (bookmark these)

- `docs/mvp-plan.md` — what we’re building now
- `docs/tech-stack.md` — target architecture
- `docs/api.md` — HTTP + SSE contract
- `docs/events-and-audit.md` — event shapes + invariants
- `docs/security-model.md` — capabilities/approvals

---

*Authored by Mesut (2026-02-11). Updated for MVP completeness.*

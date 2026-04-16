# Plan: Split `main.rs` into submodules

## Problem
`crates/alms-cli/src/main.rs` is 1786 lines — all CLI definitions, command handlers, HTTP helpers, and tests in one file.

## Proposed structure

```
crates/alms-cli/src/
  main.rs          (~120 lines) — Cli struct, Commands enum, main(), shared helpers
  commands/
    mod.rs         — re-exports
    agent.rs       (~250 lines) — AgentCommands enum + agent_create/show/delete/config/list/set_default
    session.rs     (~120 lines) — SessionCommands enum + session_list/show/delete
    run.rs         (~130 lines) — RunCommands enum + run_create/list/show
    job.rs         (~200 lines) — JobCommands enum + job_list/show/create/cancel + parse_schedule + fmt_job_status
  api.rs           (~100 lines) — api_url, api_client, api_get, api_post, api_delete, parse_api_error
```

## What stays in `main.rs`
- `Cli` struct and top-level `Commands` enum (referencing subcommand enums from modules)
- `main()` function with the top-level match
- Shared helpers: `open_db()`, `resolve_agent()`, `fmt_time()`, `short_id()`

## What moves where

| Function(s) | Target file | Lines |
|---|---|---|
| `AgentCommands` enum, `agent_list`, `agent_create`, `agent_show`, `agent_delete`, `agent_set_default`, `agent_config` | `commands/agent.rs` | 112–170, 305–519 |
| `SessionCommands` enum, `session_list`, `session_show`, `session_delete` | `commands/session.rs` | 172–190, 521–621 |
| `RunCommands` enum, `run_create`, `run_list`, `run_show` | `commands/run.rs` | 192–229, 731–841 |
| `JobCommands` enum, `job_list`, `job_show`, `job_create`, `job_cancel`, `parse_schedule`, `fmt_job_status` | `commands/job.rs` | 231–266, 843–1003 |
| `api_url`, `api_client`, `api_get`, `api_post`, `api_delete`, `parse_api_error` | `api.rs` | 623–729 |

## Test placement
Tests follow their code into each module (idiomatic Rust `#[cfg(test)] mod tests`):
- Agent tests → `commands/agent.rs`
- Session tests → `commands/session.rs`
- Schedule parsing tests → `commands/job.rs`
- Job SQLite tests → `commands/job.rs`
- API/HTTP helper tests → `api.rs`
- Completions tests → stay in `main.rs`

## Shared items
`open_db()`, `resolve_agent()`, `fmt_time()`, `short_id()` are used across modules. They stay in `main.rs` and are made `pub(crate)`.

## Steps
1. Create `src/api.rs` — move HTTP helper functions + their tests
2. Create `src/commands/mod.rs` — declare submodules
3. Create `src/commands/agent.rs` — move agent enum + functions + tests
4. Create `src/commands/session.rs` — move session enum + functions + tests
5. Create `src/commands/run.rs` — move run enum + functions + tests
6. Create `src/commands/job.rs` — move job enum + functions + tests
7. Update `main.rs` — keep Cli/Commands/main/shared helpers, wire up imports
8. `cargo fmt && cargo clippy && cargo test -p alms-cli`

## Risk
Pure refactor — no logic changes, no public API changes. All functions keep exact same signatures. If tests pass, it's correct.

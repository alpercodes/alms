# ALMS Configuration

ALMS loads configuration with layered precedence:

1. Compiled defaults
2. `alms.toml` in the current working directory (or `~/.config/alms/config.toml`)
3. `ALMS_*` environment variables (non-secret settings only)

Secrets — API keys, Telegram tokens, etc. — are never read from arbitrary environment variables. Store them with `alms auth set <provider> <key>` or declare a per-provider env var with `api_key_env` (see below).

Most of this document covers the **LLM provider** surface; the section immediately below covers the knobs the web UI's Settings modal exposes. See `docs/architecture.md`, `alms.toml.example` and `crates/alms-core/src/config/mod.rs` for the rest.

---

## Settings modal knobs

The Settings modal in the web UI edits the runtime-mutable slice of this config through `PATCH /settings`. Its hints are deliberately short — one line saying what a field does plus its valid range. This section is where the detail behind them lives.

Two behaviours apply to **every** section of the modal:

- **Propagation.** An accepted PATCH takes effect on the next HTTP-triggered run, with no restart. Telegram-triggered runs read a boot-time snapshot and keep using it until the daemon restarts.
- **Persistence.** The whole mutable surface is written to `{data_dir}/settings.json`, which wins over `alms.toml` on the next boot. To go back to a TOML- or env-driven value, edit or delete `settings.json` before restarting.

See [`docs/api.md`](api.md) § 10.2 for the wire shapes, validation rules and error envelopes.

### Context

| Field | Range / default | Notes |
|---|---|---|
| `strategy` | `truncate` (default) / `compact` / `full` | `truncate` drops the oldest messages to fit; `compact` folds older messages into a summary and keeps a recent verbatim tail. `full` sends everything and is config-file / PATCH only — the modal's dropdown offers the first two. |
| `max_input_tokens` | default `128000` | Per-request LLM budget. Set it to the model's context window. Cross-validated against the provider's published cap — see `ALMS_LLM_BUDGET_VALIDATION` below. |
| `compact_trigger_pct` | `0.50`–`0.95`, default `0.80` | Compaction fires when assembled history exceeds this fraction of the **effective history budget** — `max_input_tokens` minus the system-prompt / user-input / episodic / reserve overhead, i.e. the room actually left for history. |
| `compact_retain_pct` | `0.20`–`0.60`, default `0.40` | Fraction of that same effective history budget kept as recent verbatim messages after compaction. The invariant `compact_retain_pct + 0.10 <= compact_trigger_pct` is enforced on PATCH and on TOML load, so compaction always measurably shrinks the context. |

The modal's **Summary** section (`summary_model` / `summary_provider`) drives both compaction summaries and post-run episodic memory. Set both or neither: a `summary_model` paired with the agent's own provider would be a slug from the wrong namespace, so partial configurations are rejected rather than silently mis-routed. Full notes in `alms.toml.example` under `[context]`.

### Session

`max_messages`, `idle_timeout_secs`, `auto_archive` and `archive_ttl_secs` bound on-disk session storage and retention. `max_context_tokens` is the **storage** limit and must be at least `context.max_input_tokens` — the session has to hold at least as much as one request can consume. Roughly 2× the context window is a comfortable setting.

### Tools

`shell_policy = "sandboxed"` pins the shell tool's working directory to `sandbox_root`; `"unrestricted"` removes that limit. `sandbox_root` is the canonicalization root for the `fs_*` tools, and an empty value disables the restriction. See [`docs/security-model.md`](security-model.md) § 4.4 for what the sandbox does and does not guarantee on each platform. `max_output_bytes` bounds the output of a single tool call.

> **`timeout_secs` is vestigial.** It is validated, exposed on `GET /settings` and accepted by `PATCH /settings`, but nothing in the tool-execution path reads it — setting it has no effect. What bounds a tool instead varies, and for most tools is nothing: `shell` by its own `timeout_ms` argument (120s default, 600s hard cap), `http_get` by a hardcoded 30s, and everything else — the `fs_*` tools, the remaining builtins, and all eight agent tools in `alms-tools` — by no timeout of its own. A batch that hangs on one of those is caught by the run-level `tool_phase_ceiling_secs` backstop (see *Agent-loop hard caps* above) rather than by anything per-tool. The exception is `invoke_agent`: a foreground call is exempt from that ceiling (P3b) and blocks for the subagent's whole run, bounded by the subagent's own loop caps (`max_iterations`, the phase-inactivity budgets, `max_run_duration_secs`) — a different kind of bound from a timeout. Tracked in [#112](https://github.com/alpercodes/alms/issues/112), which will either wire the knob up or remove it.

### Reverting a thinking budget

`llm.anthropic.thinking_budget_tokens` and `llm.gemini.thinking_budget` have **no clear sentinel** on the PATCH wire: `0` means "thinking disabled" and an omitted field means "leave alone", so neither expresses "go back to the config-file default". Once either has been PATCHed, the only way back is to edit `settings.json` and restart.

### Debug mode

The modal's Debug toggle is not a config knob at all — it is the per-agent `debug_mode` field on the agent record, updated via `PUT /agents/{id_or_name}`. It never changes what the LLM receives; it only mirrors the assembled context window to the UI for triage. Documented in [`docs/api.md`](api.md) § 9.4.

---

## LLM providers

Providers are declared in `[llm.providers.<name>]` tables. `llm.provider` selects which one to use.

```toml
[llm]
provider = "openrouter"        # name of a [llm.providers.*] entry
model    = "z-ai/glm-5.2"
```

These two keys are the **server default** and are runtime-mutable: `PATCH /settings` (and the Settings modal) changes them for the next run with no restart, and persists the change to `{data_dir}/settings.json`. That file wins over `alms.toml` on the next boot, so once the pair has been PATCHed, editing it here has no effect until you edit or remove `settings.json`. See `docs/api.md` § 10.2 for the validation rules and the Telegram propagation caveat. The `[llm.providers]` tables below are **not** PATCH-mutable — they are read once at startup.

The sugar names `openai`, `openrouter`, and `anthropic` are auto-populated at config-load time, so classic flat configs (`provider = "openai"` with nothing else) keep working. User-declared entries with the same names override the auto-populated ones.

### Schema

```toml
[llm.providers.<name>]
kind         = "openai_compatible"   # "openai_compatible" | "anthropic"; default = openai_compatible
base_url     = "https://api.x.ai/v1"  # required

# API key (pick one, or use `alms auth set <name> <key>`)
api_key_env  = "XAI_API_KEY"   # read from this env var at gateway startup
# api_key    = "sk-..."        # literal; discouraged

# Optional overrides
model        = "grok-4"         # wins over `llm.model` when this provider is selected
auth_scheme  = { type = "bearer" }   # bearer (default) | header

# Optional request-build quirks (see "Quirks" below)
[llm.providers.<name>.quirks]
tool_gap_fill      = false
drop_empty_content = false
```

API key precedence at gateway startup:

1. `SecretsStore` — the value last written by `alms auth set <name> <key>`.
2. The provider entry's `api_key_env` (read from the named environment variable).
3. The provider entry's literal `api_key`.

When resolution fails, the gateway logs a warning and continues; the first outgoing request will surface an auth error from the upstream.

> Note: the `alms auth set <name> <key>` command currently restricts `<name>` to a fixed list (`openai`, `anthropic`, `gemini`, `openrouter`, `telegram`). For any *other* provider you declare in `[llm.providers.<name>]`, use `api_key_env` instead. Extending `alms auth set` to accept arbitrary provider names is tracked separately.

### Auth schemes

| Scheme | TOML | Wire format |
|---|---|---|
| Bearer (default) | `auth_scheme = { type = "bearer" }` | `Authorization: Bearer <key>` |
| Custom header    | `auth_scheme = { type = "header", name = "x-api-key" }` | `x-api-key: <key>` |

Anthropic's sugar entry defaults to `header` with `name = "x-api-key"`. Additional schemes (query parameters, HMAC-signed requests) will be added the day a concrete provider integration requires them.

### Quirks

Small, deterministic transforms applied to the outgoing request body — cheaper to keep here than to carry through as conditional logic in every caller. Naming and semantics follow a conventional provider-transform middleware set.

| Flag | Default | Behaviour |
|---|---|---|
| `tool_gap_fill` | `false` | Insert an empty `user` turn between two consecutive `tool` messages. Needed for Mistral-family endpoints that reject back-to-back tool results (which happen when the agent runs tools in parallel). |
| `drop_empty_content` | `false` | Drop assistant turns whose `content` is empty and that carry no `tool_calls`. Some OpenAI-compatible endpoints 400 on empty assistant messages. Messages with `role == "tool"` and assistant messages carrying `tool_calls` are always kept. |

---

## Agent-loop hard caps (issues #987, #1150)

A handful of `[llm]` knobs bound a **single agent run** so a run that keeps calling tools without ever producing a final reply terminates instead of hanging forever:

```toml
[llm]
max_iterations         = 500    # default 500;   0 = disabled (no limit)
between_iterations_secs = 180   # default 180;   0 = disabled (P1 inactivity budget)
tool_phase_ceiling_secs = 900   # default 900;   0 = disabled (P3 tool-batch ceiling)
max_run_duration_secs  = 86400  # default 86400 (24h); 0 = disabled (absolute backstop)
```

| Knob | Default | Bounds |
|---|---|---|
| `max_iterations` | `500` | The number of LLM-call iterations a run may take. One iteration is one LLM call plus the tool batch it requests, so this caps the step count. |
| `between_iterations_secs` | `180` | **P1 inactivity budget** — how long a run may go without any progress signal while resting between iterations before it is terminated as *stalled*. Reset on every progress signal. |
| `tool_phase_ceiling_secs` | `900` | **P3 tool-phase ceiling** — a coarse backstop on how long a single tool batch may run. Reset at tool-batch start and evaluated at the next checkpoint. Set *above* the longest single-tool timeout (the `shell` tool's 600s `MAX_TIMEOUT_SECS`), not equal to it — a 600s ceiling would false-stall a `shell` command run to its own 600s cap (the batch completes, then the next checkpoint sees `idle == ceiling`). The 5-minute margin keeps it clear of every per-tool timeout (`http_get` ≈ 30s, background `shell` ≈ 5s; `fs_*` untimed, tracked in #1173). **A batch that blocks on a foreground `invoke_agent` or on human approval is exempt** — bounded instead by the subagent's own inherited phase timer / the human, so this ceiling never applies (see P3b / P3c below). |
| `max_run_duration_secs` | `86400` | **Absolute wall-clock backstop**, in seconds (default 24h). Inactivity (above) is the primary guard now; this only catches a run that pings activity forever (a bug). Checked between iterations alongside the inactivity check. |

### Phase-aware inactivity model (#1150)

Before #1150, `max_run_duration_secs` was a **flat wall-clock cap** (default 4h): a run that made slow-but-real forward progress was clipped the moment total elapsed time hit the cap. #1150 replaces that with a **progress-aware** primary guard. The loop tracks the time since the run last made *progress* — a streamed token / reasoning delta, an LLM response, or a tool start — and at each between-iterations checkpoint terminates the run if that idle time exceeds the budget for the **phase** the run is in:

| Phase | When | Budget |
|---|---|---|
| **P0** awaiting first activity | iteration 1, before the run has produced anything | *derived*: `stream_chunk_timeout_secs + 30s` (≈210s with defaults) — not a knob, so it tracks the LLM idle timeout. A first LLM call that hangs *before* its first byte is bounded by the per-request HTTP guards (`timeout_secs` / `stream_chunk_timeout_secs`), not this check. |
| **P1** between iterations | resting between iterations | `between_iterations_secs` |
| **P2** mid-LLM-call | a streamed response is arriving | *no independent timer* — every token / reasoning delta resets the activity clock, so a long-but-productive stream is never clipped. A stream that **stalls** (no chunk for `stream_chunk_timeout_secs`) is faulted by the per-chunk body-read guard (#1169) instead. |
| **P3** executing a tool batch | a tool batch is running | `tool_phase_ceiling_secs` |
| **P3b** blocking foreground `invoke_agent` | the batch runs a foreground subagent | *unbounded* — the parent blocks on the subagent for its whole runtime with no progress signal reaching the parent's activity clock, so the P3 ceiling would otherwise kill the *parent* the moment a productive long subagent returns. The subagent governs itself via the same in-loop phase timer it inherits (a hung one self-terminates and returns an error to the parent); the parent's absolute `max_run_duration_secs` backstop still applies. A *background* `invoke_agent` returns immediately and stays in P3. |
| **P3c** awaiting human approval | a Guarded-posture batch is blocked on a tool that routes through the human-approval gate | *unbounded* — in **Guarded** posture (the default for user-triggered interactive runs) a tool that is not auto-approved blocks the run until the human approves or denies. A human who takes longer than the P3 ceiling to approve must not be read as a stall, so an approval-gated batch is exempt — the same class of fix as P3b. The absolute `max_run_duration_secs` backstop (24h) still bounds a truly-abandoned approval. A batch of only auto-approved tools, or any FullControl / Autonomous run (no approval gate), stays in P3. |

The net effect: a long but **productive** run (steady tokens, or back-to-back tool calls that each make progress) is **never** terminated by this timer, however long it runs — only a genuinely *stalled* run (no progress for the phase budget) trips. A stalled run surfaces a distinct session-history label, **"Agent stopped after stalling (no activity)"**, separate from the iteration-limit and time-limit labels. This is a minimal, watchdog-free implementation: the check runs only *between* iterations, so the terminating tool/LLM step finishes first and the run is ended at the following checkpoint — there is no mid-step interruption.

**These caps default ON and apply to every run type** — web chat, cron jobs, and subagents (each value is inherited verbatim by subagents) — not just DMs. A subagent is now bounded by this same in-loop phase timer (plus `max_iterations`); the coordinator's old 5-minute (300s) wall-clock subagent kill — which killed legitimately long subagents mid-work — was **removed** in #1150. When any cap trips on a peer-triggered DM run the gateway's DM completion gate converts the failure into an `Errored` conversation end, so the peer is notified rather than stranded.

**Upgrade impact:** after upgrading, a deployment that previously ran unbounded will end any run that exceeds 500 LLM calls, stalls past a phase budget, or runs 24h of wall-clock as `failed`. The headline change versus pre-#1150 is that the run-duration guard is now **inactivity-based** rather than flat wall-clock, and the absolute backstop default was raised from 4h to 24h — so a legitimate long-running scheduled job that makes steady progress is no longer clipped at 4h. A deep autonomous turn exceeding 500 LLM calls must still raise (or disable) `max_iterations`.

Set any knob to **`0` to disable that cap/budget** (no limit) — the escape hatch for workloads that genuinely need unbounded runs (or an unbounded single phase).

All four knobs are **config-file-only**: they are read at gateway startup and are **not mutable via `PATCH /settings`** (and have no `ALMS_*` env-var override). Edit `alms.toml` and restart the gateway to change them.

### Per-step LLM timeouts (issue #1163)

Each individual LLM call is bounded by two complementary `[llm]` timeouts:

```toml
[llm]
timeout_secs             = 600  # total per-request deadline (also bounds time-to-first-byte)
stream_chunk_timeout_secs = 180  # per-chunk BODY-read inactivity timeout
```

| Knob | Default | Bounds |
|---|---|---|
| `timeout_secs` | `600` | **Total** per-request deadline — the whole call (connect + headers + full response body) must complete within this window (reqwest's `.timeout()`). It is the only bound on the **post-connect** header / time-to-first-byte wait, so it is the outer bound for a response that is healthy-but-large *or* healthy-but-slow-to-start. (TCP+TLS connect is bounded tighter by a fixed 30s client connect timeout — a const, not a knob — so a dead/unreachable provider fails in ~30s rather than after this whole window; #1177.) This is a per-*call* deadline, **not** a run cap; raised `120 → 600` in #1177 because heavy reasoning models (`minimax/minimax-m3` on openrouter) legitimately reason past 120s. |
| `stream_chunk_timeout_secs` | `180` | **Per-chunk body-read inactivity** timeout, reset after every successful read, applied **only to the response body** — never to the header/first-byte wait. On the streaming path it is the per-chunk SSE stall timeout; on the **non-streaming (buffered)** path the body is drained as a chunk stream under the same per-chunk guard, so a body that starts arriving then stalls mid-transfer faults within this window too. Raised `60 → 180` in #1177 (also lifts the derived P0 budget `90 → 210s`). |

Before #1163 the buffered (non-streaming) path had no body-read inactivity guard at all, so a slow/stalled **non-streaming** response body (seen with `minimax/minimax-m3` on openrouter, which returned a buffered `application/json` body that never finished arriving) hung for the *entire* `timeout_secs` window before failing with `LLM response decode failed … operation timed out`. The fix drains the buffered body as a chunk stream under the same per-chunk inactivity timeout the streaming path already used, so a stalled body — on **either** path — surfaces a clear error within `stream_chunk_timeout_secs` instead of dead-airing until the total deadline.

The guard is deliberately **body-only** (not a client-level reqwest `.read_timeout()`, which would also cap the header / time-to-first-byte wait). A response that is *slow to send its first byte* — a non-streaming upstream that buffers the whole completion before sending headers, or a slow reasoning model — is therefore governed by `timeout_secs`, not `stream_chunk_timeout_secs`. Raise `stream_chunk_timeout_secs` for upstreams that stream/arrive in slow trickles once they've started; raise `timeout_secs` for upstreams that are simply slow to begin responding (or that return one large buffered body).

The agent loop attempts streaming first and falls back to a buffered `complete()` on a streaming failure. When the streaming attempt blew the **total `timeout_secs`** deadline (a reqwest `operation timed out`), that buffered re-issue is **futile** — it waits out the same deadline and fails identically — so the loop short-circuits and surfaces the diagnostic immediately (#1162 / #1163 / #1177). Every other streaming failure keeps the fallback: a per-chunk **stall** (a token gap past `stream_chunk_timeout_secs`) can still recover, because the buffered path is non-streaming and its header/first-byte wait is bounded by the full `timeout_secs` — a slow-*generating* model's mid-stream silence is absorbed there, then the body arrives in a burst; *decode* faults (connection reset, malformed/truncated JSON, gzip failure) can likewise succeed on a fresh request.

---

## Anthropic extended thinking (issue #767)

Claude 4.x exposes an optional extended-thinking mode where the model streams its internal reasoning as `thinking` content blocks before the final assistant text. ALMS can opt in on a per-server, per-agent, or per-run basis.

```toml
[llm.anthropic]
thinking_budget_tokens = 4096   # 0 = disabled (default), any N > 0 enables thinking
```

When non-zero, every Anthropic request gains a `"thinking": {"type": "enabled", "budget_tokens": N}` field on the wire. The runtime streams the model's reasoning back through a provider-neutral `reasoning_delta` SSE event, and the web UI renders it in a collapsible panel under the assistant message (defaults to collapsed).

Prior thinking blocks are **not** replayed on follow-up tool-use turns — this is standard mode. The Anthropic interleaved-thinking beta (which would require replaying signatures) is out of scope today and will land as a follow-up.

### Per-agent precedence

The budget follows the same two-layer precedence pattern as `model` / `max_tokens`:

1. **Per-agent** (highest) — `thinking_budget_tokens` field on the agent registry entry (set via `POST /agents` or the CLI).
2. **Server default** (lowest) — `[llm.anthropic].thinking_budget_tokens` in `alms.toml`.

`Some(0)` at either layer is an explicit opt-out — e.g. an agent with `thinking_budget_tokens = 0` will never use extended thinking even when the server default enables it. Non-Anthropic providers silently ignore the field.

Per-run config overrides were removed in the #941 pivot; agents are the single per-tenant config surface. Operators set the budget on the agent record before starting the run.

### Usage accounting

Anthropic counts thinking tokens inside `output_tokens` today, so the existing `prompt_tokens` / `completion_tokens` / `total_tokens` surface keeps working unchanged. If Anthropic's API ever exposes a separate `thinking_tokens` field, we can plumb it through `TokenUsage` as an additional slot without breaking existing consumers.

---

## Anthropic prompt caching (issue #766)

Anthropic's [prompt-caching feature](https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching) lets the server cache stable request prefixes (system prompt, workspace files, tool definitions) for 5 minutes, then bill subsequent cache-hit requests at ~10% of the standard input-token rate. ALMS's Anthropic adapter attaches `cache_control: { type: "ephemeral" }` markers on the trailing system content block and the last tool definition whenever caching is enabled.

```toml
[llm.anthropic]
prompt_cache_enabled = true   # default = true
```

Set to `false` to strip all cache markers — useful for diagnosing cache-related failures or if your upstream proxy does not honour Anthropic's `cache_control` shape.

### Why two markers, not four?

Anthropic supports up to four cache breakpoints per request. ALMS uses two:

1. **Last tool definition** — caches the entire tools array.
2. **Trailing system content block** — caches the full prefix through system + workspace + (optional episodic summary), because Anthropic caches up to *and including* each marker.

Workspace files (personality / goals / memories) and episodic summaries are already concatenated into the system string by `agent::context::assemble_system_prompt` and `ContextBuilder::build_with_perspective` before the adapter sees them. Splitting them into separate content blocks for independent breakpoints would require a runtime refactor; the single trailing-system marker gives the full prefix a cache entry today with no per-turn churn.

### Scope

- **Server-level only.** No per-agent or per-run override — caching is a pure optimization with no downside, so there's no need for fine-grained control.
- **Anthropic only for the `prompt_cache_enabled` flag and `cache_creation_input_tokens`.** Other provider adapters ignore `prompt_cache_enabled` entirely, and `cache_creation_input_tokens` stays `None` for them. `cache_read_input_tokens` is **provider-neutral** — it is shared with Gemini (#769), which reports `cachedContentTokenCount` through the same field. See the Gemini section below for details.
- **5-minute TTL.** The 1-hour beta and Bedrock's `cachePoint` are out of scope for this pass.

### Usage metrics

Anthropic responses include two new usage fields when caching is active:

- `cache_creation_input_tokens` — prefix tokens *written* to the cache on this request (billed at ~1.25× standard input rate). **Anthropic-specific**; stays `None` on Gemini (which has no separate creation-side counter).
- `cache_read_input_tokens` — prefix tokens *served from* the cache on this request (billed at ~0.1× on Anthropic). **Provider-neutral field**, historically named after Anthropic's wire shape but reused by Gemini (#769) to surface `cachedContentTokenCount`. When operators see this field populated in a `run_finished` event, disambiguate by cross-referencing `llm.provider` or the run's model name.

Both are plumbed end-to-end:

- Anthropic adapter (`anthropic.rs`) parses them from `AnthropicUsage` and populates the provider-neutral `Usage` struct.
- Agent loop (`loop_impl.rs`) accumulates them into `TokenUsage` across multi-turn runs.
- SSE `run_finished` event surfaces them on the wire (absent when `None`, matching the `skip_serializing_if` contract).
- Subagent completion markers include them under `token_usage.cache_creation_input_tokens` / `cache_read_input_tokens`.
- The web UI's runs tab shows `N cached` alongside input/output counts when `cache_read_input_tokens > 0`.

### Correctness notes

The cache only hits when the request prefix is byte-identical across turns. Two subtle requirements fall out:

- **Tool ordering is deterministic.** The runtime's tool registry is a `DashMap` whose iteration order is non-deterministic, so `ToolRegistry::to_definitions()` sorts by canonical tool name. Adding or removing a tool via `[tools.enabled]` will invalidate the cache on the next turn (one miss, then steady-state hits resume).
- **Non-caching wire parity.** With `prompt_cache_enabled = false`, the adapter emits the pre-#766 wire shape unchanged (plain-string `system`, no `cache_control` on tools). Enabling caching switches `system` to an array-of-blocks shape internally; this is Anthropic's supported alternative wire shape and is transparent to the model.

### Minimum cacheable size

Anthropic's documented minimum for `ephemeral` caching is 1024 input tokens (Sonnet/Haiku; older models require 2048). Below that, Anthropic *silently ignores* the cache marker — no error, no warning — and your request is billed as a normal non-cached request. This is fine; ALMS does not attempt to estimate prefix size before attaching markers, because:

- The measurement would need to mirror Anthropic's tokenizer exactly.
- Short prompts don't benefit from caching anyway, so "no-op below the threshold" is the correct behaviour.

Expect cache hits to stay at zero on very short agent conversations; once you add workspace files or a long system prompt, the prefix crosses the threshold and metrics start flowing.

---

## OpenAI-compat reasoning models (issue #768)

OpenAI o-series (`o1`, `o3`, `o4-mini`), GPT-5, DeepSeek R1, and xAI Grok reasoning variants all produce chain-of-thought output in addition to the final assistant text. ALMS wires them through the same `ReasoningDelta` / collapsible UI panel used for Anthropic extended thinking, with a provider-family-specific knob to request a reasoning budget on the wire.

```toml
[llm.openai]
reasoning_effort = "high"   # "low" | "medium" | "high" | "minimal" (GPT-5 only); default = unset
```

When set, every OpenAI-compat request targeting a **reasoning-capable** model gains a `"reasoning_effort": "<value>"` field on the wire. The adapter only emits the field when the model is actually a reasoning model — for non-reasoning models (gpt-4o, claude-sonnet via proxy, etc.) the field is silently stripped before serialization, because those endpoints return 400 on unknown params. DeepSeek R1 (`deepseek-reasoner`) is also stripped because reasoning is implicit there and the endpoint rejects the param.

The model-detection heuristic covers:
- **OpenAI**: `o1*`, `o3*`, `o4*`, `o5*`, `gpt-5*` (case-insensitive, optional `<provider>/` prefix like `openai/o3-mini`).
- **xAI Grok**: `grok-3-mini`, `grok-3-reasoning`, `grok-4`, `grok-*-reasoning`.
- **Anything else**: field stripped.

Response-side, ALMS parses reasoning content from whichever field the provider uses — `reasoning_content` (DeepSeek / xAI / OpenRouter), `reasoning_summary` (OpenAI user-visible summary), or `reasoning` (OpenAI raw trace) — and routes all three into the existing `RuntimeEvent::ReasoningDelta` stream. When both `reasoning` and `reasoning_summary` are present, the summary wins (OpenAI's documented preference). Streaming deltas (`choices[].delta.*`) use the same priority ordering.

Like Anthropic thinking, reasoning text is **not** replayed back to the model on follow-up turns — it's a display/debug artifact, persisted as `reasoning_blocks` metadata on the assistant message so it survives page reload.

### Per-agent precedence

Same two-layer chain as `model` / `max_tokens` / `thinking_budget_tokens`:

1. **Per-agent** (highest) — `reasoning_effort` field on the agent registry entry (set via `POST /agents`, `PUT /agents/{id}`, or `alms agent create --reasoning-effort <value>`).
2. **Server default** (lowest) — `[llm.openai].reasoning_effort` in `alms.toml`.

Omitting the field at every layer means no `reasoning_effort` is sent — non-reasoning models behave exactly as before. There is no sentinel to clear a per-agent override back to "inherit server default" in a PATCH today (matches the `thinking_budget_tokens` PATCH shape); delete + recreate if you need that.

Per-run config overrides were removed in the #941 pivot; agents are the single per-tenant config surface.

### Usage accounting

OpenAI o-series reports reasoning-token cost separately under `usage.completion_tokens_details.reasoning_tokens`. DeepSeek / xAI may emit a flat `usage.reasoning_tokens` field. ALMS plumbs both shapes through `TokenUsage.reasoning_tokens` — an `Option<u32>` that stays `None` when the provider doesn't split them out (reasoning cost is then implicitly folded into `completion_tokens`). Callers read via `Usage::reasoning_tokens_effective()` which prefers the nested OpenAI shape over the flat DeepSeek/xAI shape when both happen to be present.

### Per-provider notes

- **OpenAI (direct)**: supports `low`, `medium`, `high` on o-series (`o1`/`o3`/`o4`/`o5`). GPT-5 adds `minimal`.
- **OpenRouter**: pass-through — support depends on the underlying model. Works when the model is an OpenAI reasoning SKU routed via the `openai/` prefix (e.g. `openai/o3-mini`) or an xAI reasoning variant. For non-reasoning models routed through OpenRouter the ALMS adapter strips the param before sending (same heuristic as the direct path).
- **xAI Grok**: supports the same four values on `grok-3-mini`, `grok-3-reasoning`, `grok-4`, and `grok-*-reasoning`.
- **DeepSeek R1**: ignores `reasoning_effort`. Reasoning is automatic on `deepseek-reasoner`; the adapter strips the param for any DeepSeek base URL.
- **Non-reasoning OpenAI models** (`gpt-4o`, `gpt-4`, etc.): param stripped automatically — safe to leave `reasoning_effort` in config even when you switch models.

---

## Adding an OpenAI-compatible provider

Copy the block that matches your provider, paste it into `alms.toml`, set `llm.provider` to the entry's name, and run `alms auth set <name> <key>` (or export the `api_key_env` variable before launching the gateway).

### xAI (Grok)

```toml
[llm]
provider = "xai"
model    = "grok-4"

[llm.providers.xai]
base_url    = "https://api.x.ai/v1"
api_key_env = "XAI_API_KEY"
model       = "grok-4"
```

### DeepSeek

```toml
[llm]
provider = "deepseek"
model    = "deepseek-chat"

[llm.providers.deepseek]
base_url    = "https://api.deepseek.com/v1"
api_key_env = "DEEPSEEK_API_KEY"
model       = "deepseek-chat"   # or "deepseek-reasoner" for R1
```

### Groq

```toml
[llm]
provider = "groq"
model    = "llama-3.3-70b-versatile"

[llm.providers.groq]
base_url    = "https://api.groq.com/openai/v1"
api_key_env = "GROQ_API_KEY"
model       = "llama-3.3-70b-versatile"
```

### Mistral

Mistral's hosted API rejects two back-to-back `tool` messages, which happens whenever the agent runs tools in parallel. Enable `tool_gap_fill` to paper over the difference:

```toml
[llm]
provider = "mistral"
model    = "mistral-large-latest"

[llm.providers.mistral]
base_url    = "https://api.mistral.ai/v1"
api_key_env = "MISTRAL_API_KEY"
model       = "mistral-large-latest"

[llm.providers.mistral.quirks]
tool_gap_fill      = true
drop_empty_content = true
```

### Ollama (local)

Ollama's OpenAI-compatible endpoint requires no key. Leave `api_key_env` unset; the gateway will send requests without an auth header:

```toml
[llm]
provider = "ollama"
model    = "llama3.1:8b"

[llm.providers.ollama]
base_url = "http://localhost:11434/v1"
model    = "llama3.1:8b"
```

### LM Studio (local)

LM Studio exposes an OpenAI-compatible server on port 1234 by default. Same story as Ollama — no key needed:

```toml
[llm]
provider = "lmstudio"
model    = "local-model"       # whatever name you gave the loaded model

[llm.providers.lmstudio]
base_url = "http://localhost:1234/v1"
```

### Self-hosted vLLM / Together / Fireworks / anything else

If the endpoint speaks the OpenAI chat-completions protocol, it's one entry away:

```toml
[llm]
provider = "myvllm"

[llm.providers.myvllm]
base_url    = "https://vllm.internal/v1"
api_key_env = "VLLM_API_KEY"
model       = "meta-llama/Llama-3.1-70B-Instruct"
```

If the endpoint authenticates via a non-standard header or a query parameter, set `auth_scheme` accordingly:

```toml
[llm.providers.myproxy]
base_url    = "https://example.com/v1"
api_key_env = "MYPROXY_KEY"
auth_scheme = { type = "header", name = "X-Proxy-Auth" }
```

---

## Gemini

Google Gemini ships a native adapter (`kind = "gemini"`) that speaks the
`generateContent` / `streamGenerateContent` API directly — `systemInstruction`
extraction, `functionCall` / `functionResponse` tool parts, SSE streaming.
The sugar entry is auto-populated, so minimal config is enough:

```toml
[llm]
provider = "gemini"
model    = "gemini-2.5-pro"
```

Store the API key with `alms auth set gemini <key>`. Alternatively, point the
sugar entry at an existing environment variable:

```toml
[llm.providers.gemini]
api_key_env = "GEMINI_API_KEY"   # or "GOOGLE_API_KEY"
```

Only override `base_url` if you need to talk to a proxy or a non-default
region — the default is `https://generativelanguage.googleapis.com/v1beta`.
Gemini authenticates via the `x-goog-api-key` header (preferred over the
`?key=` query parameter because it keeps the secret out of URL logs).

### Gemini context caching + thinking (issue #769)

Gemini's [context-caching feature](https://ai.google.dev/gemini-api/docs/caching) caches the stable request prefix (system instruction + tool definitions) as a REST resource and bills subsequent requests that reference it at a discounted rate. Gemini 2.5+ also supports [extended thinking](https://ai.google.dev/gemini-api/docs/thinking) via `generationConfig.thinkingConfig.thinkingBudget`. ALMS wires both through `[llm.gemini]`:

```toml
[llm.gemini]
cache_enabled      = true    # default = true
cache_ttl_seconds  = 300     # default = 300 (5 minutes)
thinking_budget    = 4096    # default = unset (disabled)
```

#### Context caching

When `cache_enabled = true`, the Gemini adapter creates a `cachedContents` resource on the first turn of each session whose stable prefix crosses Gemini's minimum cacheable size, and references the returned cache name via `cachedContent: "cachedContents/<id>"` on subsequent turns. The cache is reused until the TTL expires or the stable prefix (system instruction + tool definitions) changes.

Set `cache_enabled = false` to skip cache creation entirely — useful for diagnosing cache-related failures, running on a Gemini project without caching enabled, or when the stable prefix churns faster than the TTL.

**Minimum cacheable size.** Gemini caching requires at least **32,768 input tokens** in the cached prefix. Requests below that threshold are rejected by `cachedContents.create` with a "too small" error; ALMS remembers the rejection per-session+prefix (a `TooSmall` sentinel) to avoid creating a cache on every turn. Most ALMS sessions do not hit this floor until workspace files or the system prompt become substantial — operators running small agents will see `cache_read_input_tokens` stay `None`, which is the correct behaviour. This is a much higher floor than Anthropic's 1,024-token minimum.

**TTL.** `cache_ttl_seconds` is sent as the `ttl` field when creating a cache entry. Gemini enforces the TTL server-side; ALMS does not track expiry client-side. When Gemini returns a cache-not-found error on a subsequent `generateContent` / `streamGenerateContent` request (the referenced cache was GC'd or TTL'd), the adapter transparently invalidates the stored handle and retries the same request once without `cachedContent`. Neither the agent loop nor the operator sees the retry — cache reuse is best-effort.

**Prefix-hash invalidation.** Alongside each cache name, the in-process cache store keeps a hash of the exact JSON bytes that went into the cache (system instruction + tool definitions). On every turn the store re-hashes the current prefix; a mismatch (workspace file edited, tool list changed, system prompt rewritten) drops the stored handle and creates a fresh cache on the next turn. There's no operator knob for this — the hash comparison is automatic.

**Server-level only.** Caching is a pure optimization — no per-agent override. Subagents inherit the parent's `cache_enabled` / `cache_ttl_seconds` verbatim so they share cache entries wherever possible.

#### Thinking passthrough

Setting `thinking_budget = N` with `N > 0` emits `generationConfig.thinkingConfig: { thinkingBudget: N, includeThoughts: true }` on every Gemini request. The provider streams parts with `thought: true` alongside the visible text; ALMS routes those into the same provider-neutral `RuntimeEvent::ReasoningDelta` channel used by Anthropic extended thinking (#767) and OpenAI o-series reasoning (#768). The web UI renders them in the same collapsible reasoning panel.

**Two-layer precedence** (#794 / #941) — `[llm.gemini].thinking_budget` in `alms.toml` is the server default, with per-agent overrides layering on top with the same shape as `thinking_budget_tokens` (#767) and `reasoning_effort` (#768):

1. **Per-agent** (highest): stored in the agent registry and set via `alms agent create --gemini-thinking-budget N` or `alms agent config --gemini-thinking-budget N` (also via `POST /agents` and `PUT /agents/{id_or_name}` on the HTTP surface).
2. **Server default** (lowest): `[llm.gemini].thinking_budget` in `alms.toml`.

`Some(0)` at either layer explicitly disables extended thinking for that scope — it is NOT a "use default" sentinel. `None` / omitted at a layer falls through to the next. Non-Gemini providers silently ignore the field regardless of layer. Named subagents get the same two-layer chain with the parent's effective budget acting as the "server default" layer: a per-named-subagent override stored on the registered `AgentRecord` wins, `None` inherits the parent's effective value verbatim. Caching knobs (`cache_enabled`, `cache_ttl_seconds`) remain server-level only.

Per-run config overrides were removed in the #941 pivot — agents are the single per-tenant config surface.

Like Anthropic thinking, reasoning text is **not** replayed back to the model on follow-up turns — it's persisted as `reasoning_blocks` metadata on the assistant message so it survives page reload.

#### Usage metrics

Two new fields flow end-to-end:

- `usageMetadata.thoughtsTokenCount` → `TokenUsage.reasoning_tokens` (same slot used by DeepSeek / xAI via the flat-field path; OpenAI o-series uses the nested `completion_tokens_details.reasoning_tokens` shape — both coalesce through `Usage::reasoning_tokens_effective()`).
- `usageMetadata.cachedContentTokenCount` → `TokenUsage.cache_read_input_tokens` (**shared surface with Anthropic** — both providers' "tokens served from cache" metric flows through the same field; Gemini has no separate creation-side counter, so `cache_creation_input_tokens` stays `None` for Gemini turns).

Both fields surface on the SSE `run_finished` event, `GET /runs/{run_id}`, subagent completion markers, and the runs-tab UI — same plumbing the Anthropic caching metrics use. When caching is disabled or the prefix is below the 32k-token minimum, `cache_read_input_tokens` stays `None` — zero would be ambiguous with "cache miss on a request that had a marker".

#### Out of scope

- **Implicit caching** — Gemini automatically caches prefixes in some tiers. That path is free (no API call to create, no request field to attach); the metrics still flow through `cachedContentTokenCount` as usual, so operators who rely on implicit caching see the savings on `cache_read_input_tokens` without any ALMS config change.
- **Multimodal caching** (cached images, audio, video) — will land when ALMS adds multimodal input.
- **Interleaved thinking replay** — Gemini, like Anthropic, has a beta mode where prior thinking must be replayed on tool-use turns. Out of scope.

---

## Environment-variable overrides

Non-secret `ALMS_*` variables are applied on top of the parsed config file. The LLM-related ones:

| Variable | Equivalent TOML |
|---|---|
| `ALMS_LLM_PROVIDER` | `llm.provider` |
| `ALMS_LLM_BASE_URL` / `LLM_BASE_URL` | `llm.base_url` |
| `ALMS_LLM_MODEL` / `DEFAULT_MODEL` | `llm.model` |
| `ALMS_LLM_MOCK` | `llm.mock` |
| `ALMS_LLM_STREAM_CHUNK_TIMEOUT` | `llm.stream_chunk_timeout_secs` |
| `ALMS_LLM_BUDGET_VALIDATION` | (no TOML equivalent; `strict` default, `warn` opts out) |

`ALMS_LLM_PROVIDER` can reference any entry in `[llm.providers.*]`, including user-declared ones — so you can keep Grok, DeepSeek, Groq, and Mistral all configured in the same file and switch between them with a single env var.

### `ALMS_LLM_BUDGET_VALIDATION` — provider context-window enforcement (#919)

ALMS cross-validates `[context].max_input_tokens + agent.max_tokens` against the published context window for the resolved `(provider, model)` pair on every gateway boot AND on every `POST /runs` / `PATCH /settings`. Default behaviour is **strict**: a configured budget that overshoots the provider cap fails fast — the daemon refuses to boot, the run is rejected with a structured `400 INVALID_TOKEN_BUDGET_FOR_PROVIDER`, and the PATCH is rejected with the same error envelope before any field commits.

Strict mode catches typos (`max_input_tokens = 1_000_000` paired with a model whose cap is 200K) at the earliest possible moment instead of letting a doomed request reach the provider. The published cap table lives in [`crates/alms-core/src/config/budget.rs`](../crates/alms-core/src/config/budget.rs) and is verified against the official model-overview pages each release; rows the table doesn't know about are skipped silently (the validator never false-positives on an unknown pair).

Setting `ALMS_LLM_BUDGET_VALIDATION=warn` downgrades every strict reject to a structured WARN log (`target = "alms.config"`) and lets the boot / run / PATCH proceed. Use this when the table is wrong for a model you know is fine — the operator opts out at their own risk. Any unknown or empty value falls back to **strict** so a typoed opt-out (`ALMS_LLM_BUDGET_VALIDATION=yolo`) doesn't silently disable enforcement. The variable has no TOML equivalent on purpose: a single boot-time env var is the only opt-out surface so cap enforcement cannot be silently flipped via a `PATCH /settings` mutation.

---

## Compatibility with existing configs

The generic `[llm.providers.*]` surface is strictly additive:

- Flat configs (`provider = "openai"` with nothing under `[llm.providers]`) still work — the sugar entries are auto-populated at load time.
- Existing `[llm.openai]` / `[llm.openrouter]` / `[llm.anthropic]` / `[llm.gemini]` sugar blocks (where used) are untouched; they simply don't override the generic `[llm.providers.<name>]` entries.
- `alms auth set <provider> <key>` still works for the fixed list (`openai`, `anthropic`, `gemini`, `openrouter`, `telegram`) as documented above. For any *other* provider declared in `[llm.providers.<name>]`, use `api_key_env` instead — extending `alms auth set` to accept arbitrary provider names is tracked separately.

Native adapters — currently the Anthropic Messages adapter and the Google Gemini `generateContent` adapter — are reserved for providers that cannot be reached through the OpenAI chat-completions protocol. Everything else should land here as a docs entry, not new adapter code.

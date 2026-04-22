# ALMS Configuration

ALMS loads configuration with layered precedence:

1. Compiled defaults
2. `alms.toml` in the current working directory (or `~/.config/alms/config.toml`)
3. `ALMS_*` environment variables (non-secret settings only)

Secrets — API keys, Telegram tokens, etc. — are never read from arbitrary environment variables. Store them with `alms auth set <provider> <key>` or declare a per-provider env var with `api_key_env` (see below).

This document focuses on the **LLM provider** surface. See `docs/architecture.md` and `crates/alms-core/src/config/mod.rs` for the rest.

---

## LLM providers

Providers are declared in `[llm.providers.<name>]` tables. `llm.provider` selects which one to use.

```toml
[llm]
provider = "openrouter"        # name of a [llm.providers.*] entry
model    = "moonshotai/kimi-k2.5"
```

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

## Anthropic extended thinking (issue #767)

Claude 4.x exposes an optional extended-thinking mode where the model streams its internal reasoning as `thinking` content blocks before the final assistant text. ALMS can opt in on a per-server, per-agent, or per-run basis.

```toml
[llm.anthropic]
thinking_budget_tokens = 4096   # 0 = disabled (default), any N > 0 enables thinking
```

When non-zero, every Anthropic request gains a `"thinking": {"type": "enabled", "budget_tokens": N}` field on the wire. The runtime streams the model's reasoning back through a provider-neutral `reasoning_delta` SSE event, and the web UI renders it in a collapsible panel under the assistant message (defaults to collapsed).

Prior thinking blocks are **not** replayed on follow-up tool-use turns — this is standard mode. The Anthropic interleaved-thinking beta (which would require replaying signatures) is out of scope today and will land as a follow-up.

### Per-agent and per-run precedence

The budget follows the same three-layer precedence pattern as `model` / `max_tokens`:

1. **Per-run** (highest) — `thinking_budget_tokens` field on the `POST /runs` body.
2. **Per-agent** — `thinking_budget_tokens` field on the agent registry entry (set via `POST /agents` or the CLI).
3. **Server default** (lowest) — `[llm.anthropic].thinking_budget_tokens` in `alms.toml`.

`Some(0)` at any layer is an explicit opt-out — e.g. an agent with `thinking_budget_tokens = 0` will never use extended thinking even when the server default enables it. Non-Anthropic providers silently ignore the field.

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
- **Anthropic only.** Other provider adapters ignore the `prompt_cache_enabled` flag entirely. `TokenUsage.cache_creation_input_tokens` / `cache_read_input_tokens` stay `None` for them.
- **5-minute TTL.** The 1-hour beta and Bedrock's `cachePoint` are out of scope for this pass.

### Usage metrics

Anthropic responses include two new usage fields when caching is active:

- `cache_creation_input_tokens` — prefix tokens *written* to the cache on this request (billed at ~1.25× standard input rate).
- `cache_read_input_tokens` — prefix tokens *served from* the cache on this request (billed at ~0.1×).

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

### Per-agent and per-run precedence

Same three-layer chain as `model` / `max_tokens` / `thinking_budget_tokens`:

1. **Per-run** (highest) — `reasoning_effort` field on the `POST /runs` body.
2. **Per-agent** — `reasoning_effort` field on the agent registry entry (set via `POST /agents`, `PUT /agents/{id}`, or `alms agent create --reasoning-effort <value>`).
3. **Server default** (lowest) — `[llm.openai].reasoning_effort` in `alms.toml`.

Omitting the field at every layer means no `reasoning_effort` is sent — non-reasoning models behave exactly as before. There is no sentinel to clear a per-agent override back to "inherit server default" in a PATCH today (matches the `thinking_budget_tokens` PATCH shape); delete + recreate if you need that.

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

**Known gaps** (tracked for future work):
- Context caching — Gemini-specific feature, not yet wired.
- Thinking/reasoning token passthrough — not yet surfaced.
- Multimodal input (image / audio / video parts) — not yet supported.

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

`ALMS_LLM_PROVIDER` can reference any entry in `[llm.providers.*]`, including user-declared ones — so you can keep Grok, DeepSeek, Groq, and Mistral all configured in the same file and switch between them with a single env var.

---

## Compatibility with existing configs

The generic `[llm.providers.*]` surface is strictly additive:

- Flat configs (`provider = "openai"` with nothing under `[llm.providers]`) still work — the sugar entries are auto-populated at load time.
- Existing `[llm.openai]` / `[llm.openrouter]` / `[llm.anthropic]` / `[llm.gemini]` sugar blocks (where used) are untouched; they simply don't override the generic `[llm.providers.<name>]` entries.
- `alms auth set <provider> <key>` still works for the fixed list (`openai`, `anthropic`, `gemini`, `openrouter`, `telegram`) as documented above. For any *other* provider declared in `[llm.providers.<name>]`, use `api_key_env` instead — extending `alms auth set` to accept arbitrary provider names is tracked separately.

Native adapters — currently the Anthropic Messages adapter and the Google Gemini `generateContent` adapter — are reserved for providers that cannot be reached through the OpenAI chat-completions protocol. Everything else should land here as a docs entry, not new adapter code.

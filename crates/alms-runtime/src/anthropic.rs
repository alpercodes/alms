//! Anthropic Messages API types and conversion functions.
//!
//! Converts between ALMS internal types (`CompletionRequest`/`CompletionResponse`)
//! and Anthropic's wire format. Used by `LlmClient` when `provider == "anthropic"`.

use crate::llm_client::SseParseResult;
use crate::llm_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    /// Anthropic system prompt — either a plain string or an array of
    /// typed content blocks. We emit the array form whenever prompt
    /// caching (#766) is enabled so the trailing block can carry a
    /// `cache_control` marker; otherwise we keep the plain-string form
    /// for wire parity with pre-#766 requests.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    /// Extended-thinking configuration for Claude 4.x. Omitted when disabled.
    ///
    /// Populated only when the incoming `CompletionRequest` carries a
    /// non-zero `thinking_budget_tokens`. On the wire this serializes to
    /// `"thinking": {"type": "enabled", "budget_tokens": N}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinking>,
}

/// Anthropic's `system` field shape (#766).
///
/// The API accepts either a plain string or an array of typed content
/// blocks — when caching is enabled we need the array form so the last
/// block can carry a `cache_control` marker. The two variants serialise
/// untagged (string-or-array), matching Anthropic's documented shape.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicSystem {
    /// Plain string — used when prompt caching is disabled. Byte-identical
    /// to the pre-#766 request shape.
    Text(String),
    /// Array of content blocks — used when prompt caching is enabled so
    /// the trailing block can carry `cache_control`.
    Blocks(Vec<AnthropicSystemBlock>),
}

/// A single system content block in the array-shaped system field.
///
/// `type == "text"` is the only shape Anthropic accepts inside `system`
/// today. `cache_control` is attached only to the *last* block in the
/// array when caching is enabled.
#[derive(Debug, Serialize)]
pub(crate) struct AnthropicSystemBlock {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

/// Anthropic cache-control marker (#766).
///
/// Currently the only documented `type` is `"ephemeral"` (5-minute TTL).
/// Kept as a tagged struct so a future 1-hour `type` can land as a
/// non-breaking shape evolution.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct CacheControl {
    #[serde(rename = "type")]
    pub kind: &'static str,
}

impl CacheControl {
    const fn ephemeral() -> Self {
        Self { kind: "ephemeral" }
    }
}

impl AnthropicSystem {
    /// Return the raw text of a string-shaped system field, for callers
    /// (chiefly tests and logs) that want to assert on the system prompt
    /// without caring about the array-of-blocks shape. Returns `None` for
    /// the array-shaped variant; those callers should pattern-match on
    /// `Blocks` directly.
    #[cfg(test)]
    pub(crate) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            Self::Blocks(_) => None,
        }
    }
}

/// Anthropic extended-thinking request field.
///
/// Today only the `enabled` variant exists on the Anthropic side; kept as a
/// tagged struct rather than a bare u32 so adding future modes (e.g. a
/// `"redacted"` toggle for the redacted-thinking beta) is a non-breaking
/// shape evolution.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AnthropicThinking {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub budget_tokens: u32,
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicMessage {
    pub role: String,
    pub content: AnthropicContent,
}

/// Anthropic content can be a plain string or an array of content blocks.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum AnthropicContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
    /// Extended-thinking content block — Anthropic's Claude 4.x emits one or
    /// more of these before the final assistant text when
    /// `thinking.type == "enabled"` is passed on the request.
    ///
    /// The `signature` field is a cryptographic signature Anthropic uses to
    /// verify replayed thinking blocks in the interleaved-thinking beta. We
    /// parse it so we don't fail on unexpected fields, but we do NOT replay
    /// thinking blocks back to the model on subsequent turns — in standard
    /// (non-interleaved) mode no signature round-trip is required.
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// Prompt-caching marker (#766). Attached only to the *last* tool in
    /// the array when prompt caching is enabled, and absent otherwise so
    /// non-caching requests stay byte-identical to pre-#766.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<CacheControl>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicResponse {
    pub id: String,
    pub content: Vec<ContentBlock>,
    pub model: String,
    pub stop_reason: Option<String>,
    pub usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AnthropicUsage {
    /// Input (prompt) tokens. Defaults to 0 for streaming `message_delta`
    /// events which only carry `output_tokens`.
    #[serde(default)]
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Prompt-caching metric (#766): tokens *written* to the cache on
    /// this request. Anthropic omits the field when caching is not in
    /// play; `#[serde(default)]` keeps it at `None` so older responses
    /// continue to parse.
    #[serde(default)]
    pub cache_creation_input_tokens: Option<u32>,
    /// Prompt-caching metric (#766): tokens *served from* the cache on
    /// this request. Same defaulting semantics as the creation field.
    #[serde(default)]
    pub cache_read_input_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Streaming types
// ---------------------------------------------------------------------------

/// Anthropic SSE events have an `event:` field and a `data:` JSON payload.
#[derive(Debug, Deserialize)]
pub(crate) struct StreamEvent {
    #[serde(rename = "type")]
    pub _event_type: String,
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub delta: Option<StreamDelta>,
    #[serde(default)]
    pub content_block: Option<ContentBlock>,
    #[serde(default)]
    pub message: Option<AnthropicResponse>,
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct StreamDelta {
    #[serde(rename = "type")]
    pub delta_type: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub partial_json: Option<String>,
    /// Populated on `thinking_delta` events — a text chunk of the model's
    /// extended-thinking trace.
    #[serde(default)]
    pub thinking: Option<String>,
    /// Populated on `signature_delta` events — a cryptographic signature
    /// that Anthropic uses for interleaved-thinking replay. Parsed for
    /// completeness but discarded by the runtime in standard mode.
    ///
    /// `allow(dead_code)` is deliberate: keeping the field on `StreamDelta`
    /// ensures the JSON deserializer doesn't fail on unknown shapes when
    /// Anthropic sends a `signature_delta` event, and makes it trivial to
    /// wire the replay path later if the interleaved-thinking beta is
    /// adopted (follow-up to #767).
    #[serde(default)]
    #[allow(dead_code)]
    pub signature: Option<String>,
    // message_delta fields (deserialized but only usage is read)
    #[serde(default)]
    pub _stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

// ---------------------------------------------------------------------------
// Conversion: internal → Anthropic request
// ---------------------------------------------------------------------------

/// Sanitize a tool call ID so it satisfies Anthropic's strict
/// `^[a-zA-Z0-9_-]+$` regex on `tool_use.id` and `tool_result.tool_use_id`
/// (issue #850).
///
/// Other providers (OpenAI, Gemini) accept a wider character set, so IDs
/// that originated upstream — or that survived in session history from a
/// prior provider switch — can carry characters Anthropic rejects (most
/// commonly `:`, `.`, `/`). Without this sanitizer the Anthropic API
/// returns a 400 on the very first replayed tool_use, blocking any
/// multi-turn run with tool history.
///
/// # Contract
///
/// - Characters in `[a-zA-Z0-9_-]` pass through unchanged.
/// - Any other character is replaced with `_`.
/// - If the input contained no characters in the allowed set
///   (entirely-invalid input like `"!!!"`, `"@@@"`, the empty string,
///   or non-ASCII like `"你好"`), we fall back to `"call_" + hex hash
///   of the original ID`. The hash uses `DefaultHasher` which is
///   process-stable but deliberately not cryptographic — we only need
///   a deterministic, regex-conforming, non-colliding identifier so
///   the matching tool_use / tool_result pair lines up on the wire.
///
///   We test "no valid characters" rather than "result is empty"
///   because dumb char-by-char replacement maps `"!!!"` and `"@@@"`
///   to the same `"___"`, which would silently collide and pair
///   tool_use blocks with the wrong tool_result.
///
/// ## Known corner — standard-path collisions
///
/// The non-fallback path can also collide in principle: `call_a:b` and
/// `call_a/b` both sanitize to `call_a_b`. We accept this because no
/// real-world LLM provider generates IDs that differ only in their
/// forbidden delimiter character — upstream IDs are either UUIDs,
/// `toolu_*` opaque strings, or monotonic counters, none of which
/// exhibit this shape. If a future provider does, the right move is
/// to extend the fallback to also cover `had_collision_after_replace`,
/// not to weaken the regex-conformance guarantee here.
///
/// # Determinism
///
/// The function is a pure mapping with no time, randomness, or external
/// state. This is load-bearing for prompt caching (#766): the same input
/// must produce the same output across turns or the Anthropic cache
/// prefix invalidates and we lose cache hits.
///
/// # Identical application at both ends
///
/// Anthropic pairs `tool_use.id` (assistant) with `tool_result.tool_use_id`
/// (user) by exact string match. The caller MUST run both through this
/// same function or Anthropic will reject the request with a different
/// 400 about an unmatched `tool_use_id`.
///
/// # Scope
///
/// This sanitizer is intentionally Anthropic-adapter-local. We do NOT
/// sanitize at persistence time — stored IDs stay in their original form
/// so OpenAI / Gemini wire shapes remain byte-identical to pre-#850.
fn sanitize_anthropic_tool_id(id: &str) -> String {
    let mut cleaned = String::with_capacity(id.len());
    let mut had_valid_char = false;
    for c in id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            cleaned.push(c);
            had_valid_char = true;
        } else {
            cleaned.push('_');
        }
    }
    if !had_valid_char {
        // Entirely-invalid input (or empty string). A blind char-by-char
        // map would land different inputs on the same all-underscore
        // string and silently collide. Route to a deterministic hash
        // fallback so distinct inputs stay distinct, the result is
        // non-empty, and repeated calls are byte-stable for prompt
        // caching (#766).
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        format!("call_{:016x}", hasher.finish())
    } else {
        cleaned
    }
}

/// Convert an internal `CompletionRequest` to an `AnthropicRequest`.
///
/// The input message list is expected to already satisfy the canonical
/// shape enforced by `ContextBuilder::normalize_for_llm` (see
/// `context.rs` module docs) at the `LlmMessage` layer: system prefix at
/// the front, alternating `user`/`assistant`/`tool` turns, trailing user
/// turn, no empty-content messages, no pending tool_calls.
///
/// The transformations here are the narrowly provider-specific ones:
///
/// - System messages get extracted into the top-level `system` field
///   (Anthropic native format).
/// - Tool calls become `tool_use` content blocks inside assistant messages.
/// - Tool results become `tool_result` content blocks merged into the
///   following user message (Anthropic has no `role: "tool"`).
///
/// # Wire-level alternation post-pass
///
/// Because the `role="tool"` → `role="user"` relabel happens *here*, the
/// canonical builder invariant ("no adjacent same-role messages") does
/// NOT automatically hold on the Anthropic wire. Concretely: a canonical
/// tail `[tool_result, user_text]` becomes two adjacent `user` wire
/// messages, which Anthropic rejects with a 400.
///
/// After the conversion loop we therefore run [`merge_consecutive_roles`]
/// to concatenate adjacent same-role messages' content blocks in order.
/// The post-merge `debug_assert!`s pin the wire-level invariant as a
/// tripwire against future regressions.
pub(crate) fn to_anthropic_request(req: &CompletionRequest) -> AnthropicRequest {
    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<AnthropicMessage> = Vec::new();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(ref text) = msg.content {
                    system_parts.push(text.clone());
                }
            }
            "user" => {
                messages.push(AnthropicMessage {
                    role: "user".to_string(),
                    content: AnthropicContent::Text(msg.content.clone().unwrap_or_default()),
                });
            }
            "assistant" => {
                if let Some(ref tool_calls) = msg.tool_calls {
                    // Assistant message with tool calls → tool_use content blocks
                    let mut blocks: Vec<ContentBlock> = Vec::new();
                    if let Some(ref text) = msg.content
                        && !text.is_empty()
                    {
                        blocks.push(ContentBlock::Text { text: text.clone() });
                    }
                    for tc in tool_calls {
                        // Anthropic's `tool_use.input` is spec'd as an
                        // object and the API rejects any other shape with
                        // `400 invalid_request_error: "Input should be an
                        // object"`. The legacy fallback here was
                        // `Value::String(args)`, which:
                        //
                        //   1. wrote a JSON string into `tool_use.input`
                        //      (Anthropic 400),
                        //   2. and round-tripped a poisoned `params: ""`
                        //      shape through SQLite (so the failure
                        //      persisted across runs and wedged the
                        //      session — see #967).
                        //
                        // Normalize through the shared helper so empty /
                        // malformed / non-object args always serialize as
                        // an object on the wire.
                        let input: Value = normalize_tool_args(&tc.function.arguments);
                        blocks.push(ContentBlock::ToolUse {
                            // Sanitize for Anthropic's `^[a-zA-Z0-9_-]+$`
                            // regex (#850). Must use the same function on
                            // the matching tool_result below.
                            id: sanitize_anthropic_tool_id(&tc.id),
                            name: tc.function.name.clone(),
                            input,
                        });
                    }
                    messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                } else {
                    messages.push(AnthropicMessage {
                        role: "assistant".to_string(),
                        content: AnthropicContent::Text(msg.content.clone().unwrap_or_default()),
                    });
                }
            }
            "tool" => {
                // Tool results → tool_result content block inside a user message.
                // Anthropic requires tool results in user messages.
                let block = ContentBlock::ToolResult {
                    // Sanitize for Anthropic's `^[a-zA-Z0-9_-]+$` regex
                    // (#850). Identical mapping to the `tool_use.id`
                    // emit point above so the pair still matches on the
                    // wire. Empty `tool_call_id` (defensive default)
                    // routes through `sanitize_anthropic_tool_id` and
                    // produces a deterministic synthetic id rather than
                    // an empty string Anthropic would reject.
                    tool_use_id: sanitize_anthropic_tool_id(
                        msg.tool_call_id.as_deref().unwrap_or(""),
                    ),
                    content: msg.content.clone().unwrap_or_default(),
                };
                // If the previous message is a user with blocks, append to it.
                // Otherwise create a new user message with blocks.
                let can_append = matches!(
                    messages.last(),
                    Some(AnthropicMessage { role, content: AnthropicContent::Blocks(_), .. }) if role == "user"
                );
                if can_append {
                    if let Some(AnthropicMessage {
                        content: AnthropicContent::Blocks(blocks),
                        ..
                    }) = messages.last_mut()
                    {
                        blocks.push(block);
                    }
                } else {
                    messages.push(AnthropicMessage {
                        role: "user".to_string(),
                        content: AnthropicContent::Blocks(vec![block]),
                    });
                }
            }
            _ => {}
        }
    }

    // Wire-level alternation pass. The builder enforces alternation at
    // the `LlmMessage` layer where `user` and `tool` are distinct roles,
    // but the `tool` → `user` relabel above can introduce adjacent
    // `user` wire messages (e.g. canonical tail `[tool_result, user_text]`
    // or two back-to-back fresh-user turns after a notification run
    // lands on a tool_result tail). Anthropic rejects adjacent same-role
    // wire messages with a 400, so we concatenate them here. This is
    // provider-specific and cannot live in the shared builder.
    merge_consecutive_roles(&mut messages);

    // Post-conditions from the canonical invariant. Enforced as debug
    // assertions — if they fire, the caller violated the contract and
    // #586's tests missed a case; we want the loudest possible signal
    // in tests and debug builds without paying the cost in release.
    debug_assert!(
        !messages.is_empty(),
        "anthropic adapter received empty messages array after system extraction — \
         context builder must guarantee at least one non-system message"
    );
    debug_assert_eq!(
        messages.last().map(|m| m.role.as_str()),
        Some("user"),
        "anthropic adapter: messages does not end with user role — \
         context builder must guarantee trailing user turn"
    );
    // Wire-level alternation — every adjacent pair must differ in role
    // after `merge_consecutive_roles`. Tripwire for future regressions.
    debug_assert!(
        messages.windows(2).all(|w| w[0].role != w[1].role),
        "anthropic adapter: adjacent same-role messages on the wire — \
         merge_consecutive_roles failed to coalesce (roles={:?})",
        messages.iter().map(|m| m.role.as_str()).collect::<Vec<_>>()
    );

    // Prompt caching (#766): when enabled, emit `cache_control` markers
    // on the trailing system block and the trailing tool so Anthropic
    // caches the full stable prefix (tools + system + workspace +
    // optional episodic summary) for 5 minutes. Opt-in via
    // `CompletionRequest.prompt_cache_enabled` — absent or false keeps
    // the pre-#766 wire shape byte-identical.
    //
    // Why two markers and not four (per issue #766's "up to 4
    // breakpoints" note)?
    // - Anthropic caching is prefix-based: marking the *last* system
    //   block caches the full prefix up to and including that block
    //   (tools + every prior system block). Marking the last tool
    //   caches the tools prefix. Two markers cover the entire stable
    //   prefix; the remaining two breakpoints would only be useful for
    //   independent caching of workspace files or the episodic summary,
    //   which requires threading them through as separate
    //   `LlmMessage::system` blocks — a refactor outside this PR. See
    //   the PR body for the trade-off.
    // - The adapter sees workspace files already concatenated into the
    //   first `LlmMessage::system` by `agent::context::assemble_system_prompt`,
    //   and the episodic summary (when present) added as a second
    //   `LlmMessage::system` by `ContextBuilder::build_with_perspective`.
    //   A single marker on the last of those caches all of it.
    let cache_enabled = req.prompt_cache_enabled.unwrap_or(false);

    let system = if system_parts.is_empty() {
        None
    } else if cache_enabled {
        // Array form — attach `cache_control` to the trailing block so
        // the whole system prefix becomes a cache breakpoint.
        let last_idx = system_parts.len() - 1;
        let blocks: Vec<AnthropicSystemBlock> = system_parts
            .into_iter()
            .enumerate()
            .map(|(i, text)| AnthropicSystemBlock {
                kind: "text",
                text,
                cache_control: if i == last_idx {
                    Some(CacheControl::ephemeral())
                } else {
                    None
                },
            })
            .collect();
        Some(AnthropicSystem::Blocks(blocks))
    } else {
        // Pre-#766 wire shape — plain string. Preserves byte-parity for
        // requests made with caching disabled.
        Some(AnthropicSystem::Text(system_parts.join("\n\n")))
    };

    let tools = req.tools.as_ref().map(|defs| {
        let last_idx = defs.len().saturating_sub(1);
        defs.iter()
            .enumerate()
            .map(|(i, d)| AnthropicTool {
                name: d.function.name.clone(),
                description: d.function.description.clone(),
                input_schema: d.function.parameters.clone(),
                cache_control: if cache_enabled && i == last_idx && !defs.is_empty() {
                    // Anthropic caches the full tools array when any
                    // tool in it carries `cache_control`. Marking the
                    // last one is the recommended placement.
                    Some(CacheControl::ephemeral())
                } else {
                    None
                },
            })
            .collect()
    });

    // Extended thinking: emit the `thinking` field only when the caller
    // supplied a non-zero budget. Zero and `None` both map to "disabled"
    // so we don't serialize an empty `thinking` object on every request.
    let thinking = req
        .thinking_budget_tokens
        .filter(|n| *n > 0)
        .map(|budget_tokens| AnthropicThinking {
            kind: "enabled",
            budget_tokens,
        });

    AnthropicRequest {
        model: req.model.clone(),
        messages,
        system,
        tools,
        max_tokens: req.max_tokens.unwrap_or(32_000),
        stream: req.stream,
        thinking,
    }
}

/// Convert an `AnthropicContent` into a `Vec<ContentBlock>` so that two
/// adjacent same-role messages can be merged by concatenating their blocks.
/// `Text` content becomes a single `ContentBlock::Text` block; `Blocks`
/// content is returned as-is.
fn content_to_blocks(content: AnthropicContent) -> Vec<ContentBlock> {
    match content {
        AnthropicContent::Text(text) => vec![ContentBlock::Text { text }],
        AnthropicContent::Blocks(blocks) => blocks,
    }
}

/// Merge adjacent same-role `AnthropicMessage`s by concatenating their
/// content blocks in order. Needed at the adapter boundary because the
/// `role="tool"` → `role="user"` relabel that happens during
/// [`to_anthropic_request`] can create adjacencies that the canonical
/// `Vec<LlmMessage>` shape forbids (see the module-level doc on
/// [`to_anthropic_request`]).
fn merge_consecutive_roles(messages: &mut Vec<AnthropicMessage>) {
    if messages.len() < 2 {
        return;
    }
    let mut merged: Vec<AnthropicMessage> = Vec::with_capacity(messages.len());
    for msg in messages.drain(..) {
        if let Some(prev) = merged.last_mut()
            && prev.role == msg.role
        {
            let mut prev_blocks =
                match std::mem::replace(&mut prev.content, AnthropicContent::Text(String::new())) {
                    AnthropicContent::Text(text) => vec![ContentBlock::Text { text }],
                    AnthropicContent::Blocks(blocks) => blocks,
                };
            prev_blocks.extend(content_to_blocks(msg.content));
            prev.content = AnthropicContent::Blocks(prev_blocks);
            continue;
        }
        merged.push(msg);
    }
    *messages = merged;
}

// ---------------------------------------------------------------------------
// Conversion: Anthropic response → internal
// ---------------------------------------------------------------------------

/// Convert an `AnthropicResponse` to an internal `CompletionResponse`.
pub(crate) fn from_anthropic_response(resp: AnthropicResponse) -> CompletionResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    // Accumulated extended-thinking text. Surfaced as `reasoning_content` on
    // the returned `LlmMessage` so callers that only use the non-streaming
    // path (buffered fallback, tests) still see the thinking trace — it's
    // the same field that OpenAI-compatible reasoning models populate.
    let mut thinking_parts: Vec<String> = Vec::new();

    for block in &resp.content {
        match block {
            ContentBlock::Text { text } => {
                text_parts.push(text.clone());
            }
            ContentBlock::ToolUse { id, name, input } => {
                tool_calls.push(ToolCall::new(id.clone(), name.clone(), input.to_string()));
            }
            ContentBlock::ToolResult { .. } => {
                // Tool results shouldn't appear in responses
            }
            ContentBlock::Thinking { thinking, .. } => {
                // Signatures are parsed but intentionally discarded — they
                // only matter for the interleaved-thinking beta where prior
                // thinking must be replayed with its signature. In standard
                // mode we don't replay, so the signature is informational.
                thinking_parts.push(thinking.clone());
            }
        }
    }

    let finish_reason = resp.stop_reason.map(|r| match r.as_str() {
        "end_turn" => "stop".to_string(),
        "tool_use" => "tool_calls".to_string(),
        "max_tokens" => "length".to_string(),
        other => other.to_string(),
    });

    CompletionResponse {
        id: resp.id,
        object: "chat.completion".to_string(),
        created: 0,
        model: resp.model,
        choices: vec![Choice {
            index: 0,
            message: LlmMessage {
                role: "assistant".to_string(),
                content: if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join(""))
                },
                reasoning_content: if thinking_parts.is_empty() {
                    None
                } else {
                    Some(thinking_parts.join(""))
                },
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            },
            finish_reason,
        }],
        usage: Some(Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
            // Anthropic surfaces thinking tokens inside `output_tokens`
            // (extended thinking is folded into the completion bucket),
            // so we don't split them out here. See Anthropic's "extended
            // thinking" usage docs.
            reasoning_tokens: None,
            completion_tokens_details: None,
            // Prompt-caching metrics (#766) — flow through unchanged
            // from the Anthropic wire usage into the provider-neutral
            // `Usage` shape. `None` when the response does not include
            // them (non-cached requests, non-Anthropic providers).
            cache_creation_input_tokens: resp.usage.cache_creation_input_tokens,
            cache_read_input_tokens: resp.usage.cache_read_input_tokens,
        }),
    }
}

// ---------------------------------------------------------------------------
// Streaming: parse Anthropic SSE events
// ---------------------------------------------------------------------------

/// Parse an Anthropic SSE event block into an internal `StreamChunk`.
///
/// Anthropic uses typed events (`event: content_block_delta`, etc.) rather than
/// OpenAI's generic `data: {json}` format. We normalize them into `StreamChunk`
/// compatible with the existing streaming accumulator in `agent.rs`.
pub(crate) fn parse_anthropic_sse(event_type: &str, data: &str) -> SseParseResult {
    match event_type {
        "content_block_delta" => {
            let event: Result<StreamEvent, _> = serde_json::from_str(data);
            match event {
                Ok(ev) => {
                    if let Some(delta) = ev.delta {
                        let index = ev.index.unwrap_or(0);
                        match delta.delta_type.as_str() {
                            "text_delta" => SseParseResult::Chunk(StreamChunk {
                                id: String::new(),
                                object: "chat.completion.chunk".to_string(),
                                created: 0,
                                model: String::new(),
                                choices: vec![StreamChoice {
                                    index,
                                    delta: Delta {
                                        role: None,
                                        content: delta.text,
                                        reasoning_content: None,
                                        tool_calls: None,
                                    },
                                    finish_reason: None,
                                }],
                                usage: None,
                            }),
                            "thinking_delta" => {
                                // Route extended-thinking chunks through the
                                // same `reasoning_content` channel that
                                // OpenAI-compatible reasoning models use.
                                // The streaming accumulator in `agent.rs`
                                // recognises this field and forwards it as
                                // `RuntimeEvent::ReasoningDelta`.
                                SseParseResult::Chunk(StreamChunk {
                                    id: String::new(),
                                    object: "chat.completion.chunk".to_string(),
                                    created: 0,
                                    model: String::new(),
                                    choices: vec![StreamChoice {
                                        index,
                                        delta: Delta {
                                            role: None,
                                            content: None,
                                            reasoning_content: delta.thinking,
                                            tool_calls: None,
                                        },
                                        finish_reason: None,
                                    }],
                                    usage: None,
                                })
                            }
                            "signature_delta" => {
                                // Cryptographic signature on the thinking
                                // block. Only relevant for the interleaved-
                                // thinking beta, which replays prior
                                // thinking back to the model on tool-use
                                // follow-ups. Standard mode (this code
                                // path) does not replay, so we skip it.
                                SseParseResult::Skip
                            }
                            "input_json_delta" => {
                                // Partial tool arguments
                                SseParseResult::Chunk(StreamChunk {
                                    id: String::new(),
                                    object: "chat.completion.chunk".to_string(),
                                    created: 0,
                                    model: String::new(),
                                    choices: vec![StreamChoice {
                                        index,
                                        delta: Delta {
                                            role: None,
                                            content: None,
                                            reasoning_content: None,
                                            tool_calls: Some(vec![ToolCallDelta {
                                                index,
                                                id: None,
                                                function: Some(FunctionCallDelta {
                                                    name: None,
                                                    arguments: delta.partial_json,
                                                }),
                                            }]),
                                        },
                                        finish_reason: None,
                                    }],
                                    usage: None,
                                })
                            }
                            _ => SseParseResult::Skip,
                        }
                    } else {
                        SseParseResult::Skip
                    }
                }
                Err(_) => SseParseResult::Skip,
            }
        }
        "content_block_start" => {
            // Emit tool call id and name when a tool_use block starts
            let event: Result<StreamEvent, _> = serde_json::from_str(data);
            match event {
                Ok(ev) => {
                    if let Some(ContentBlock::ToolUse { id, name, .. }) = ev.content_block {
                        let index = ev.index.unwrap_or(0);
                        SseParseResult::Chunk(StreamChunk {
                            id: String::new(),
                            object: "chat.completion.chunk".to_string(),
                            created: 0,
                            model: String::new(),
                            choices: vec![StreamChoice {
                                index,
                                delta: Delta {
                                    role: None,
                                    content: None,
                                    reasoning_content: None,
                                    tool_calls: Some(vec![ToolCallDelta {
                                        index,
                                        id: Some(id),
                                        function: Some(FunctionCallDelta {
                                            name: Some(name),
                                            arguments: None,
                                        }),
                                    }]),
                                },
                                finish_reason: None,
                            }],
                            usage: None,
                        })
                    } else {
                        SseParseResult::Skip
                    }
                }
                Err(_) => SseParseResult::Skip,
            }
        }
        "message_delta" => {
            // Final message with usage and stop_reason
            let event: Result<StreamEvent, _> = serde_json::from_str(data);
            match event {
                Ok(ev) => {
                    let usage = ev.delta.and_then(|d| d.usage).or(ev.usage).map(|u| Usage {
                        prompt_tokens: u.input_tokens,
                        completion_tokens: u.output_tokens,
                        total_tokens: u.input_tokens + u.output_tokens,
                        reasoning_tokens: None,
                        completion_tokens_details: None,
                        // Cache metrics (#766) — `message_delta` carries
                        // authoritative cache counts (repeated from
                        // `message_start` on Anthropic's side). The
                        // stream accumulator in `loop_impl.rs` takes the
                        // max across chunks for the same "report-once"
                        // reason as prompt/completion.
                        cache_creation_input_tokens: u.cache_creation_input_tokens,
                        cache_read_input_tokens: u.cache_read_input_tokens,
                    });
                    if usage.is_some() {
                        SseParseResult::Chunk(StreamChunk {
                            id: String::new(),
                            object: "chat.completion.chunk".to_string(),
                            created: 0,
                            model: String::new(),
                            choices: vec![],
                            usage,
                        })
                    } else {
                        SseParseResult::Skip
                    }
                }
                Err(_) => SseParseResult::Skip,
            }
        }
        "message_start" => {
            // Extract usage from initial message if present
            let event: Result<StreamEvent, _> = serde_json::from_str(data);
            match event {
                Ok(ev) => {
                    if let Some(msg) = ev.message {
                        SseParseResult::Chunk(StreamChunk {
                            id: msg.id,
                            object: "chat.completion.chunk".to_string(),
                            created: 0,
                            model: msg.model,
                            choices: vec![StreamChoice {
                                index: 0,
                                delta: Delta {
                                    role: Some("assistant".to_string()),
                                    content: None,
                                    reasoning_content: None,
                                    tool_calls: None,
                                },
                                finish_reason: None,
                            }],
                            usage: Some(Usage {
                                prompt_tokens: msg.usage.input_tokens,
                                completion_tokens: msg.usage.output_tokens,
                                total_tokens: msg.usage.input_tokens + msg.usage.output_tokens,
                                reasoning_tokens: None,
                                completion_tokens_details: None,
                                // Cache metrics (#766) — `message_start`
                                // carries usage counts for the prompt
                                // side (no output yet). `message_delta`
                                // later carries completion-side counts.
                                cache_creation_input_tokens: msg.usage.cache_creation_input_tokens,
                                cache_read_input_tokens: msg.usage.cache_read_input_tokens,
                            }),
                        })
                    } else {
                        SseParseResult::Skip
                    }
                }
                Err(_) => SseParseResult::Skip,
            }
        }
        "message_stop" => SseParseResult::Done,
        _ => SseParseResult::Skip,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_anthropic_request_basic() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("You are helpful."),
                LlmMessage::user("Hello"),
            ])
            .with_max_tokens(1024);

        let anthropic_req = to_anthropic_request(&req);

        assert_eq!(anthropic_req.model, "claude-sonnet-4-20250514");
        assert_eq!(
            anthropic_req.system.as_ref().and_then(|s| s.as_text()),
            Some("You are helpful.")
        );
        assert_eq!(anthropic_req.messages.len(), 1);
        assert_eq!(anthropic_req.messages[0].role, "user");
        assert_eq!(anthropic_req.max_tokens, 1024);
    }

    #[test]
    fn test_to_anthropic_request_with_tool_calls() {
        let req = CompletionRequest::new("test-model").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("run ls"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new(
                    "call_1",
                    "shell_exec",
                    r#"{"command":"ls"}"#,
                )]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_1", "file1.txt"),
        ]);

        let anthropic_req = to_anthropic_request(&req);

        assert_eq!(anthropic_req.messages.len(), 3); // user, assistant(tool_use), user(tool_result)
        assert_eq!(anthropic_req.messages[0].role, "user");
        assert_eq!(anthropic_req.messages[1].role, "assistant");
        assert_eq!(anthropic_req.messages[2].role, "user"); // tool result in user message
    }

    /// Regression test for #967 — Anthropic 400s when `tool_use.input`
    /// is anything other than an object. The pre-#967 code path
    /// fell back to `Value::String(args)` on parse failure, so a
    /// no-args tool call (Anthropic streaming omits `input_json_delta`
    /// entirely → `arguments == ""`) round-tripped to
    /// `tool_use.input: ""` on the wire and crashed the conversation.
    #[test]
    fn test_to_anthropic_request_no_args_tool_call_emits_object_input() {
        // Empty `arguments` is the no-args case. Must serialize as
        // `tool_use.input: {}` (an object) on the wire — never as a
        // string, null, or any other shape.
        let req = CompletionRequest::new("test-model").with_messages(vec![
            LlmMessage::user("list files"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("call_1", "fs_list", "")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_1", "[]"),
        ]);

        let anthropic_req = to_anthropic_request(&req);
        let serialized = serde_json::to_value(&anthropic_req).unwrap();

        // Find the tool_use block and pin its input shape.
        let assistant_blocks = serialized["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"].as_str() == Some("assistant"))
            .expect("assistant message must exist")["content"]
            .as_array()
            .expect("assistant content must be blocks");

        let tool_use = assistant_blocks
            .iter()
            .find(|b| b["type"].as_str() == Some("tool_use"))
            .expect("tool_use block must be present");

        let input = &tool_use["input"];
        assert!(
            input.is_object(),
            "tool_use.input must be an object on the wire (#967), got: {input}"
        );
        assert_eq!(
            input,
            &serde_json::json!({}),
            "no-args tool call must serialize as tool_use.input: {{}}, got: {input}"
        );
    }

    /// Defense-in-depth: even if a malformed-args string somehow
    /// reaches the adapter (e.g. a poisoned legacy session), the wire
    /// shape must still be an object — never a string. Wrap under
    /// `_raw` instead.
    #[test]
    fn test_to_anthropic_request_malformed_args_wraps_under_raw() {
        let req = CompletionRequest::new("test-model").with_messages(vec![
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                // Not valid JSON at all — pre-#967 this became
                // `Value::String("not json")`.
                tool_calls: Some(vec![ToolCall::new("call_1", "echo", "not json")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_1", "ok"),
        ]);

        let anthropic_req = to_anthropic_request(&req);
        let serialized = serde_json::to_value(&anthropic_req).unwrap();

        let assistant_blocks = serialized["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"].as_str() == Some("assistant"))
            .unwrap()["content"]
            .as_array()
            .unwrap();

        let tool_use = assistant_blocks
            .iter()
            .find(|b| b["type"].as_str() == Some("tool_use"))
            .unwrap();

        let input = &tool_use["input"];
        assert!(
            input.is_object(),
            "tool_use.input must be an object even on malformed args: {input}"
        );
        assert_eq!(input, &serde_json::json!({"_raw": "not json"}));
    }

    #[test]
    fn test_from_anthropic_response_text_only() {
        let resp = AnthropicResponse {
            id: "msg_123".to_string(),
            content: vec![ContentBlock::Text {
                text: "Hello!".to_string(),
            }],
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 5,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };

        let completion = from_anthropic_response(resp);

        assert_eq!(completion.id, "msg_123");
        assert_eq!(completion.choices.len(), 1);
        assert_eq!(completion.choices[0].message.content_str(), "Hello!");
        assert!(completion.choices[0].message.tool_calls.is_none());
        assert_eq!(completion.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, 15);
    }

    #[test]
    fn test_from_anthropic_response_with_tool_use() {
        let resp = AnthropicResponse {
            id: "msg_456".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "toolu_1".to_string(),
                name: "echo".to_string(),
                input: serde_json::json!({"text": "hi"}),
            }],
            model: "test".to_string(),
            stop_reason: Some("tool_use".to_string()),
            usage: AnthropicUsage {
                input_tokens: 20,
                output_tokens: 10,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };

        let completion = from_anthropic_response(resp);

        let msg = &completion.choices[0].message;
        assert!(msg.content.is_none());
        let calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "echo");
        assert_eq!(
            completion.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    #[test]
    fn test_tool_definition_conversion() {
        let req = CompletionRequest::new("test")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_tools(vec![
                ToolDefinition::new("echo", "Echo text").with_parameters(serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                })),
            ]);

        let anthropic_req = to_anthropic_request(&req);

        let tools = anthropic_req.tools.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(tools[0].description, "Echo text");
        assert!(tools[0].input_schema.get("properties").is_some());
    }

    #[test]
    fn test_tool_result_appended_to_preceding_user_blocks() {
        // Two tool_results plus a following user_text should all end up
        // inside the same user-with-blocks message after wire-level
        // alternation merging: the two tool_result blocks first share a
        // user wrapper (via `can_append`), then the follow-up user_text
        // is merged in as a Text block by `merge_consecutive_roles`.
        let req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("first"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![
                    ToolCall::new("c1", "echo", "{}"),
                    ToolCall::new("c2", "echo", "{}"),
                ]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", "result1"),
            LlmMessage::tool_result("c2", "result2"),
            LlmMessage::user("follow up"),
        ]);

        let anthropic_req = to_anthropic_request(&req);

        // Wire messages: user("first"), assistant(tool_use x2),
        // user(tool_result x2 + text("follow up")).
        assert_eq!(anthropic_req.messages.len(), 3);
        assert_eq!(anthropic_req.messages[0].role, "user");
        assert_eq!(anthropic_req.messages[1].role, "assistant");
        assert_eq!(anthropic_req.messages[2].role, "user");
        if let AnthropicContent::Blocks(blocks) = &anthropic_req.messages[2].content {
            assert_eq!(
                blocks.len(),
                3,
                "two tool_results + follow-up text must be packed into one user-blocks message"
            );
            assert!(
                matches!(blocks[0], ContentBlock::ToolResult { .. }),
                "first block should be tool_result c1"
            );
            assert!(
                matches!(blocks[1], ContentBlock::ToolResult { .. }),
                "second block should be tool_result c2"
            );
            match &blocks[2] {
                ContentBlock::Text { text } => assert_eq!(text, "follow up"),
                other => panic!("third block should be text(\"follow up\"), got {other:?}"),
            }
        } else {
            panic!("expected blocks content for merged user message");
        }
    }

    /// Wire-level alternation invariant: after `to_anthropic_request`, no
    /// two adjacent messages share a role, even when the canonical
    /// `Vec<LlmMessage>` tail is `[tool_result, user_text]`. This is the
    /// regression Tim identified in PR #773 — the builder-level invariant
    /// allows `tool` + `user` adjacency, but the adapter's `tool` → `user`
    /// relabel turns that into two adjacent wire-level `user` messages
    /// which Anthropic rejects.
    #[test]
    fn test_wire_alternation_tool_result_then_fresh_user() {
        let req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("c1", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", "ok"),
            LlmMessage::user("fresh input"),
        ]);

        let anthropic_req = to_anthropic_request(&req);

        let roles: Vec<&str> = anthropic_req
            .messages
            .iter()
            .map(|m| m.role.as_str())
            .collect();
        for w in roles.windows(2) {
            assert_ne!(
                w[0], w[1],
                "wire-level adjacent same-role messages: roles={roles:?}"
            );
        }

        // And concretely: user("go"), assistant(tool_use),
        // user(tool_result + text("fresh input")).
        assert_eq!(anthropic_req.messages.len(), 3);
        assert_eq!(anthropic_req.messages[2].role, "user");
        if let AnthropicContent::Blocks(blocks) = &anthropic_req.messages[2].content {
            assert_eq!(blocks.len(), 2);
            assert!(matches!(blocks[0], ContentBlock::ToolResult { .. }));
            match &blocks[1] {
                ContentBlock::Text { text } => assert_eq!(text, "fresh input"),
                other => panic!("expected text block for fresh input, got {other:?}"),
            }
        } else {
            panic!("expected blocks content after merge");
        }
    }

    /// Second scenario from Tim's review: fresh user turn landing on a run
    /// whose persisted history ends mid-tool-loop at `[assistant(tool_use),
    /// tool_result]`. Normalize would not add anything here since the tail
    /// is already `tool` (not `user`), but `ensure_trailing_user` would
    /// synthesise a `Please continue.` placeholder. Either way the adapter
    /// must emit an alternating wire shape.
    #[test]
    fn test_wire_alternation_continue_placeholder_after_tool_result() {
        let req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("do it"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("c1", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", "ok"),
            LlmMessage::user("Please continue."),
        ]);

        let anthropic_req = to_anthropic_request(&req);

        let roles: Vec<&str> = anthropic_req
            .messages
            .iter()
            .map(|m| m.role.as_str())
            .collect();
        for w in roles.windows(2) {
            assert_ne!(w[0], w[1], "adjacent same-role wire messages: {roles:?}");
        }
    }

    #[test]
    fn test_parse_anthropic_sse_text_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#;
        let result = parse_anthropic_sse("content_block_delta", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
            }
            _ => panic!("Expected Chunk"),
        }
    }

    #[test]
    fn test_parse_anthropic_sse_message_stop() {
        let result = parse_anthropic_sse("message_stop", "{}");
        assert!(matches!(result, SseParseResult::Done));
    }

    /// Anthropic `message_delta` events include only `output_tokens` in the
    /// usage field (no `input_tokens`). Verify that `AnthropicUsage`
    /// deserializes correctly with `input_tokens` defaulting to 0.
    #[test]
    fn test_message_delta_usage_missing_input_tokens() {
        let data = r#"{"type":"message_delta","delta":{"type":"message_delta","stop_reason":"end_turn"},"usage":{"output_tokens":42}}"#;
        let result = parse_anthropic_sse("message_delta", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let usage = chunk.usage.expect("usage should be present");
                assert_eq!(usage.completion_tokens, 42);
                // input_tokens defaults to 0 when absent from the JSON
                assert_eq!(usage.prompt_tokens, 0);
            }
            other => panic!("Expected Chunk, got {:?}", std::mem::discriminant(&other)),
        }
    }

    /// Verify that `message_start` usage (with both fields) deserializes correctly.
    #[test]
    fn test_message_start_usage_has_both_fields() {
        let data = r#"{"type":"message_start","message":{"id":"msg_1","content":[],"model":"claude","stop_reason":null,"usage":{"input_tokens":150,"output_tokens":0}}}"#;
        let result = parse_anthropic_sse("message_start", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let usage = chunk.usage.expect("usage should be present");
                assert_eq!(usage.prompt_tokens, 150);
                assert_eq!(usage.completion_tokens, 0);
            }
            other => panic!("Expected Chunk, got {:?}", std::mem::discriminant(&other)),
        }
    }

    // -----------------------------------------------------------------
    // Extended-thinking passthrough (issue #767)
    // -----------------------------------------------------------------

    /// Setting `thinking_budget_tokens` on the `CompletionRequest` produces
    /// the correct `"thinking": {"type": "enabled", "budget_tokens": N}`
    /// field in the serialized Anthropic request body. The wire shape has
    /// to match Anthropic's API exactly, so we go through the full
    /// serialize path rather than asserting on the struct fields.
    #[test]
    fn test_thinking_budget_produces_wire_field() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("be helpful"),
                LlmMessage::user("hi"),
            ])
            .with_max_tokens(1024)
            .with_thinking_budget(4096);

        let anthropic_req = to_anthropic_request(&req);
        let body = serde_json::to_value(&anthropic_req).unwrap();

        let thinking = body
            .get("thinking")
            .expect("thinking field should be present when budget is set");
        assert_eq!(thinking["type"], "enabled");
        assert_eq!(thinking["budget_tokens"], 4096);
    }

    /// A budget of zero disables extended thinking — the serialized body
    /// must NOT include the `thinking` field. (An empty or zero field
    /// would likely be a 400 from Anthropic; absence is the right signal.)
    #[test]
    fn test_thinking_budget_zero_omits_wire_field() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_max_tokens(1024)
            .with_thinking_budget(0);

        let anthropic_req = to_anthropic_request(&req);
        let body = serde_json::to_value(&anthropic_req).unwrap();
        assert!(
            body.get("thinking").is_none(),
            "thinking field must be omitted when budget is 0, got: {body}",
        );
    }

    /// When `thinking_budget_tokens` is `None` (default), the field is
    /// absent on the wire. This is the path taken by non-Anthropic
    /// providers and by agents that haven't opted in.
    #[test]
    fn test_thinking_default_is_none() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_max_tokens(1024);

        let anthropic_req = to_anthropic_request(&req);
        let body = serde_json::to_value(&anthropic_req).unwrap();
        assert!(body.get("thinking").is_none());
    }

    /// `thinking_delta` SSE events route through the `reasoning_content`
    /// channel on the internal `Delta` struct. The `stream_llm_call` path
    /// observes this and emits `RuntimeEvent::ReasoningDelta`.
    #[test]
    fn test_parse_anthropic_sse_thinking_delta() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me think..."}}"#;
        let result = parse_anthropic_sse("content_block_delta", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let delta = &chunk.choices[0].delta;
                assert_eq!(
                    delta.reasoning_content.as_deref(),
                    Some("Let me think..."),
                    "thinking_delta must populate reasoning_content"
                );
                // And must NOT populate visible content — extended thinking
                // is a separate stream.
                assert!(
                    delta.content.is_none(),
                    "thinking_delta must not populate content"
                );
            }
            _ => panic!("Expected Chunk for thinking_delta"),
        }
    }

    /// `signature_delta` is parsed without error but produces `Skip` —
    /// the signature is only relevant for the interleaved-thinking beta
    /// which this feature deliberately does not support.
    #[test]
    fn test_parse_anthropic_sse_signature_delta_skipped() {
        let data = r#"{"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"abc123signature"}}"#;
        let result = parse_anthropic_sse("content_block_delta", data);
        assert!(
            matches!(result, SseParseResult::Skip),
            "signature_delta must be skipped in standard (non-interleaved) mode"
        );
    }

    /// End-to-end: a non-streaming response that carries a mix of
    /// `thinking` and `text` content blocks maps cleanly into the internal
    /// `LlmMessage` shape. Thinking text → `reasoning_content`; visible
    /// text → `content`. Tool use coexists with thinking.
    #[test]
    fn test_from_anthropic_response_with_thinking_and_tool_use() {
        let resp = AnthropicResponse {
            id: "msg_think".to_string(),
            content: vec![
                ContentBlock::Thinking {
                    thinking: "Considering options...".to_string(),
                    signature: Some("sig-opaque".to_string()),
                },
                ContentBlock::Text {
                    text: "I will use the echo tool.".to_string(),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".to_string(),
                    name: "echo".to_string(),
                    input: serde_json::json!({"text": "hi"}),
                },
            ],
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("tool_use".to_string()),
            usage: AnthropicUsage {
                input_tokens: 20,
                output_tokens: 30,
                cache_creation_input_tokens: None,
                cache_read_input_tokens: None,
            },
        };

        let completion = from_anthropic_response(resp);
        let msg = &completion.choices[0].message;
        assert_eq!(
            msg.content.as_deref(),
            Some("I will use the echo tool."),
            "visible text should end up in content"
        );
        assert_eq!(
            msg.reasoning_content.as_deref(),
            Some("Considering options..."),
            "thinking should end up in reasoning_content"
        );
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
        assert_eq!(msg.tool_calls.as_ref().unwrap()[0].function.name, "echo");
    }

    /// Wire invariant check (#773): enabling extended thinking must not
    /// break the `merge_consecutive_roles` pass or the three post-merge
    /// debug_asserts. The adapter already exercises the invariant checks
    /// every time `to_anthropic_request` is called; this test pins that
    /// the invariant still holds when a thinking field is present and the
    /// canonical history has a tool-result tail that needs merging.
    #[test]
    fn test_thinking_preserves_wire_alternation_invariant() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("sys"),
                LlmMessage::user("go"),
                LlmMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall::new("c1", "echo", "{}")]),
                    tool_call_id: None,
                },
                LlmMessage::tool_result("c1", "ok"),
                LlmMessage::user("fresh input"),
            ])
            .with_max_tokens(4096)
            .with_thinking_budget(4096);

        let anthropic_req = to_anthropic_request(&req);

        // The three debug_asserts inside to_anthropic_request pin the
        // post-merge invariant in debug builds. Re-check explicitly here
        // so this test also catches regressions in release builds.
        assert!(!anthropic_req.messages.is_empty());
        assert_eq!(anthropic_req.messages.last().unwrap().role, "user");
        let roles: Vec<&str> = anthropic_req
            .messages
            .iter()
            .map(|m| m.role.as_str())
            .collect();
        for w in roles.windows(2) {
            assert_ne!(w[0], w[1], "adjacent same-role wire messages: {roles:?}");
        }
        // And the thinking field is still present.
        let body = serde_json::to_value(&anthropic_req).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 4096);
    }

    // -----------------------------------------------------------------
    // Prompt caching (#766)
    // -----------------------------------------------------------------

    /// Baseline: when `prompt_cache_enabled` is unset (or `Some(false)`),
    /// the serialized request body must be byte-identical to the
    /// pre-#766 shape — plain string `system`, no `cache_control` on
    /// tools, usage field shapes unchanged.
    #[test]
    fn test_cache_disabled_request_has_no_cache_markers() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("You are helpful."),
                LlmMessage::user("hi"),
            ])
            .with_tools(vec![
                ToolDefinition::new("echo", "Echo text"),
                ToolDefinition::new("shell", "Run a shell command"),
            ])
            .with_max_tokens(1024);

        let areq = to_anthropic_request(&req);
        let body = serde_json::to_value(&areq).unwrap();

        // `system` serializes as a plain string.
        assert!(
            body["system"].is_string(),
            "cache disabled: system must serialize as plain string for byte parity with pre-#766; got {body}"
        );
        assert_eq!(body["system"], "You are helpful.");

        // No tool has `cache_control`.
        let tools = body["tools"].as_array().expect("tools array");
        for (i, t) in tools.iter().enumerate() {
            assert!(
                t.get("cache_control").is_none(),
                "cache disabled: tool[{i}] must not carry cache_control; got {t}"
            );
        }
    }

    /// With caching enabled and a single system message + tools, we emit
    /// exactly two `cache_control` markers: one on the trailing system
    /// block, one on the last tool.
    #[test]
    fn test_cache_enabled_emits_two_markers_with_system_and_tools() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("You are helpful.\n\n## Memories\nLikes Rust."),
                LlmMessage::user("hi"),
            ])
            .with_tools(vec![
                ToolDefinition::new("echo", "Echo"),
                ToolDefinition::new("shell", "Shell"),
                ToolDefinition::new("fs_read", "Read files"),
            ])
            .with_max_tokens(1024)
            .with_prompt_cache_enabled(true);

        let areq = to_anthropic_request(&req);
        let body = serde_json::to_value(&areq).unwrap();

        // System is now an array of blocks with the trailing block
        // carrying cache_control.
        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 1, "one system block for system+workspace");
        assert_eq!(
            system[0]["cache_control"]["type"], "ephemeral",
            "trailing system block must carry ephemeral cache marker"
        );

        // Tools: only the last one has cache_control.
        let tools = body["tools"].as_array().expect("tools");
        assert_eq!(tools.len(), 3);
        assert!(
            tools[0].get("cache_control").is_none(),
            "first tool must not be marked"
        );
        assert!(
            tools[1].get("cache_control").is_none(),
            "middle tool must not be marked"
        );
        assert_eq!(
            tools[2]["cache_control"]["type"], "ephemeral",
            "last tool must be marked ephemeral"
        );
    }

    /// When both a system prompt AND an episodic summary are injected
    /// (two `LlmMessage::system` blocks before any user turn), only the
    /// *last* system block carries a cache marker — the earlier blocks
    /// are still cached by prefix relationship. This models what the
    /// context builder produces when episodic summaries are enabled.
    #[test]
    fn test_cache_enabled_marks_only_last_system_block_of_many() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("You are helpful."),
                LlmMessage::system("## Episodic\nPrevious: discussed Rust."),
                LlmMessage::user("continue"),
            ])
            .with_max_tokens(1024)
            .with_prompt_cache_enabled(true);

        let areq = to_anthropic_request(&req);
        let body = serde_json::to_value(&areq).unwrap();

        let system = body["system"].as_array().expect("system array");
        assert_eq!(system.len(), 2, "two system blocks kept separate");
        assert!(
            system[0].get("cache_control").is_none(),
            "first system block must NOT be marked"
        );
        assert_eq!(
            system[1]["cache_control"]["type"], "ephemeral",
            "last system block must be marked"
        );
    }

    /// No tools configured on the request: the adapter must not emit a
    /// bare `tools: []` with a cache_control on the nonexistent last
    /// tool, and must not panic. (Guards the `saturating_sub(1)` arith
    /// on `defs.len()`.)
    #[test]
    fn test_cache_enabled_no_tools_emits_no_tool_marker() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![
                LlmMessage::system("You are helpful."),
                LlmMessage::user("hi"),
            ])
            .with_max_tokens(1024)
            .with_prompt_cache_enabled(true);

        let areq = to_anthropic_request(&req);
        let body = serde_json::to_value(&areq).unwrap();

        // No `tools` key on the wire at all — matches pre-#766 shape.
        assert!(
            body.get("tools").is_none()
                || body["tools"].as_array().map(Vec::is_empty).unwrap_or(false),
            "no tools must not produce a tools array: {body}"
        );
    }

    /// The adapter must emit cache_control on exactly the tool at index
    /// `len - 1`, regardless of tools vector size. Probes the offset
    /// logic directly.
    #[test]
    fn test_cache_enabled_single_tool_is_marked() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_tools(vec![ToolDefinition::new("only_tool", "only")])
            .with_max_tokens(1024)
            .with_prompt_cache_enabled(true);

        let areq = to_anthropic_request(&req);
        let body = serde_json::to_value(&areq).unwrap();

        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0]["cache_control"]["type"], "ephemeral",
            "single-tool case: that one tool is the trailing one"
        );
    }

    /// Issue #766 correctness invariant: the adapter must emit an
    /// identical request body for two calls with the same input. This
    /// is the property that makes Anthropic's cache-prefix matching
    /// work at all — bitwise drift across turns evicts the cache.
    #[test]
    fn test_cache_enabled_produces_deterministic_wire_shape() {
        let build = || {
            CompletionRequest::new("claude-sonnet-4-20250514")
                .with_messages(vec![
                    LlmMessage::system("stable system prompt"),
                    LlmMessage::user("hi"),
                ])
                .with_tools(vec![
                    ToolDefinition::new("b_tool", "b"),
                    ToolDefinition::new("a_tool", "a"),
                    ToolDefinition::new("c_tool", "c"),
                ])
                .with_max_tokens(1024)
                .with_prompt_cache_enabled(true)
        };

        let a = serde_json::to_string(&to_anthropic_request(&build())).unwrap();
        let b = serde_json::to_string(&to_anthropic_request(&build())).unwrap();
        assert_eq!(
            a, b,
            "two adapter calls with identical input must serialise byte-identically"
        );
    }

    /// Anthropic response with cache metrics populates the
    /// provider-neutral `Usage` fields. `TokenUsage` plumbing then
    /// surfaces the same values at the run level; this test pins the
    /// adapter boundary.
    #[test]
    fn test_anthropic_response_cache_usage_parsed() {
        let resp = AnthropicResponse {
            id: "msg_cache".to_string(),
            content: vec![ContentBlock::Text {
                text: "hi".to_string(),
            }],
            model: "claude-sonnet-4-20250514".to_string(),
            stop_reason: Some("end_turn".to_string()),
            usage: AnthropicUsage {
                input_tokens: 42,
                output_tokens: 7,
                cache_creation_input_tokens: Some(1500),
                cache_read_input_tokens: Some(8200),
            },
        };

        let completion = from_anthropic_response(resp);
        let usage = completion.usage.expect("usage present");
        assert_eq!(usage.cache_creation_input_tokens, Some(1500));
        assert_eq!(usage.cache_read_input_tokens, Some(8200));
    }

    /// Streaming `message_start` and `message_delta` events carry cache
    /// metrics. Verify both paths deserialize into the provider-neutral
    /// `Usage` shape.
    #[test]
    fn test_anthropic_sse_cache_usage_parsed_on_message_start() {
        let data = r#"{
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "content": [],
                "model": "claude-sonnet-4-20250514",
                "stop_reason": null,
                "usage": {
                    "input_tokens": 4,
                    "output_tokens": 1,
                    "cache_creation_input_tokens": 1500,
                    "cache_read_input_tokens": 0
                }
            }
        }"#;
        let result = parse_anthropic_sse("message_start", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let usage = chunk.usage.expect("usage present");
                assert_eq!(usage.cache_creation_input_tokens, Some(1500));
                assert_eq!(usage.cache_read_input_tokens, Some(0));
            }
            other => panic!(
                "expected Chunk with cache usage, got: {:?}",
                std::mem::discriminant(&other)
            ),
        }
    }

    #[test]
    fn test_anthropic_sse_cache_usage_parsed_on_message_delta() {
        let data = r#"{
            "type": "message_delta",
            "delta": {
                "type": "message_delta",
                "stop_reason": "end_turn"
            },
            "usage": {
                "output_tokens": 42,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 9500
            }
        }"#;
        let result = parse_anthropic_sse("message_delta", data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let usage = chunk.usage.expect("usage present");
                assert_eq!(usage.completion_tokens, 42);
                assert_eq!(usage.cache_creation_input_tokens, Some(0));
                assert_eq!(usage.cache_read_input_tokens, Some(9500));
            }
            other => panic!("expected Chunk, got: {:?}", std::mem::discriminant(&other)),
        }
    }

    /// Older Anthropic responses and non-cached requests omit the cache
    /// fields entirely. Verify that `AnthropicUsage` still deserializes
    /// without them and the provider-neutral Usage has `None`.
    #[test]
    fn test_anthropic_response_without_cache_fields_deserializes() {
        let raw = r#"{
            "id": "msg_no_cache",
            "content": [{"type": "text", "text": "hi"}],
            "model": "claude-sonnet-4-20250514",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 2}
        }"#;
        let resp: AnthropicResponse = serde_json::from_str(raw).unwrap();
        let completion = from_anthropic_response(resp);
        let usage = completion.usage.expect("usage present");
        assert!(usage.cache_creation_input_tokens.is_none());
        assert!(usage.cache_read_input_tokens.is_none());
    }

    // -----------------------------------------------------------------
    // tool_use.id sanitization (issue #850)
    // -----------------------------------------------------------------

    /// Anthropic's regex on `tool_use.id` and `tool_result.tool_use_id`
    /// is `^[a-zA-Z0-9_-]+$`. Re-derived as a Rust check so the assertions
    /// below pin the actual contract and not just our paraphrase of it.
    fn matches_anthropic_id_regex(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    }

    /// Pure-alphanumeric / underscore / hyphen IDs pass through unchanged.
    /// This is the common case for Anthropic-native (`toolu_...`) and
    /// well-formed OpenAI (`call_abc123`) IDs — sanitization must not
    /// gratuitously rewrite them, both because rewriting would invalidate
    /// prompt-cache prefixes (#766) and because matching pairs would
    /// silently shift.
    #[test]
    fn test_sanitize_anthropic_tool_id_passthrough_for_valid() {
        for id in [
            "call_abc123",
            "toolu_01ABC",
            "a",
            "Z9_-_test",
            "----",
            "____",
            "0",
        ] {
            assert_eq!(
                sanitize_anthropic_tool_id(id),
                id,
                "valid id `{id}` must pass through unchanged"
            );
            assert!(matches_anthropic_id_regex(&sanitize_anthropic_tool_id(id)));
        }
    }

    /// Each forbidden character in the issue's reproduction set maps to
    /// `_`, and the sanitized result satisfies Anthropic's regex.
    #[test]
    fn test_sanitize_anthropic_tool_id_replaces_forbidden_chars() {
        let cases = [
            ("call_abc:123", "call_abc_123"), // colon — primary suspect
            ("call_abc.def", "call_abc_def"), // period
            ("call_abc/123", "call_abc_123"), // slash
            ("tool@result", "tool_result"),   // at sign
            ("a b c", "a_b_c"),               // space
            ("call_<id>", "call__id_"),       // angle brackets
            ("call_abc!", "call_abc_"),       // bang
            ("café", "caf_"),                 // non-ASCII (single scalar)
        ];
        for (input, expected) in cases {
            let got = sanitize_anthropic_tool_id(input);
            assert_eq!(
                got, expected,
                "input `{input}` should sanitize to `{expected}`"
            );
            assert!(
                matches_anthropic_id_regex(&got),
                "sanitized `{got}` must match Anthropic regex",
            );
        }
    }

    /// All-invalid input falls back to a deterministic synthetic ID. The
    /// fallback must be non-empty, regex-conforming, and identical across
    /// repeated calls (no time, no randomness — caching depends on this).
    #[test]
    fn test_sanitize_anthropic_tool_id_fallback_for_all_invalid() {
        let inputs = ["!!!", "...", "", "@@@", "你好"];
        for input in inputs {
            let a = sanitize_anthropic_tool_id(input);
            let b = sanitize_anthropic_tool_id(input);
            assert_eq!(a, b, "fallback for `{input}` must be deterministic");
            assert!(!a.is_empty(), "fallback for `{input}` must be non-empty");
            assert!(
                matches_anthropic_id_regex(&a),
                "fallback `{a}` must match Anthropic regex",
            );
            assert!(
                a.starts_with("call_"),
                "fallback should be prefixed `call_`, got `{a}`",
            );
        }
    }

    /// Distinct all-invalid inputs should produce distinct fallback IDs
    /// in the common case — otherwise two unrelated calls could collide
    /// and Anthropic would pair the wrong tool_use with the wrong
    /// tool_result. (DefaultHasher is not collision-free, but for these
    /// short ASCII inputs collisions are negligibly unlikely.)
    #[test]
    fn test_sanitize_anthropic_tool_id_fallback_distinguishes_distinct_inputs() {
        let a = sanitize_anthropic_tool_id("!!!");
        let b = sanitize_anthropic_tool_id("@@@");
        assert_ne!(
            a, b,
            "distinct invalid inputs should map to distinct fallbacks"
        );
    }

    /// End-to-end through the request builder: a forbidden character on
    /// the `ToolCall.id` shows up sanitized on the wire `tool_use.id`,
    /// AND the matching `tool_result.tool_use_id` is sanitized identically
    /// — so the pair still matches by string. This is the contract
    /// Anthropic relies on.
    #[test]
    fn test_to_anthropic_request_sanitizes_tool_use_and_tool_result_pair() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("call_abc:123", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_abc:123", "ok"),
            LlmMessage::user("done"),
        ]);

        let areq = to_anthropic_request(&req);

        // Find the tool_use id and the tool_result tool_use_id; they must
        // both pass Anthropic's regex AND be equal so the pair matches.
        let mut tool_use_id: Option<String> = None;
        let mut tool_result_id: Option<String> = None;
        for msg in &areq.messages {
            if let AnthropicContent::Blocks(blocks) = &msg.content {
                for b in blocks {
                    match b {
                        ContentBlock::ToolUse { id, .. } => {
                            tool_use_id = Some(id.clone());
                        }
                        ContentBlock::ToolResult {
                            tool_use_id: id, ..
                        } => {
                            tool_result_id = Some(id.clone());
                        }
                        _ => {}
                    }
                }
            }
        }

        let tu = tool_use_id.expect("tool_use block must be present");
        let tr = tool_result_id.expect("tool_result block must be present");
        assert!(
            matches_anthropic_id_regex(&tu),
            "tool_use.id `{tu}` must match Anthropic regex"
        );
        assert!(
            matches_anthropic_id_regex(&tr),
            "tool_result.tool_use_id `{tr}` must match Anthropic regex"
        );
        assert_eq!(
            tu, tr,
            "sanitized tool_use.id and tool_result.tool_use_id must still pair: tu=`{tu}` tr=`{tr}`"
        );
        assert_eq!(
            tu, "call_abc_123",
            "expected colon → underscore replacement"
        );
    }

    /// Multiple forbidden characters across multiple tool calls all get
    /// sanitized in the same request, and each pair still lines up.
    #[test]
    fn test_to_anthropic_request_sanitizes_multiple_pairs() {
        let req = CompletionRequest::new("test").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![
                    ToolCall::new("call_a:1", "echo", "{}"),
                    ToolCall::new("call_b.2", "echo", "{}"),
                    ToolCall::new("call_c/3", "echo", "{}"),
                ]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("call_a:1", "r1"),
            LlmMessage::tool_result("call_b.2", "r2"),
            LlmMessage::tool_result("call_c/3", "r3"),
            LlmMessage::user("end"),
        ]);

        let areq = to_anthropic_request(&req);

        // Collect IDs in document order.
        let mut tool_use_ids: Vec<String> = Vec::new();
        let mut tool_result_ids: Vec<String> = Vec::new();
        for msg in &areq.messages {
            if let AnthropicContent::Blocks(blocks) = &msg.content {
                for b in blocks {
                    match b {
                        ContentBlock::ToolUse { id, .. } => tool_use_ids.push(id.clone()),
                        ContentBlock::ToolResult { tool_use_id, .. } => {
                            tool_result_ids.push(tool_use_id.clone())
                        }
                        _ => {}
                    }
                }
            }
        }

        assert_eq!(tool_use_ids, vec!["call_a_1", "call_b_2", "call_c_3"]);
        assert_eq!(
            tool_result_ids, tool_use_ids,
            "pairs must align by sanitized id"
        );
        for id in tool_use_ids.iter().chain(tool_result_ids.iter()) {
            assert!(
                matches_anthropic_id_regex(id),
                "id `{id}` must match Anthropic regex",
            );
        }
    }

    /// Two adapter calls with identical forbidden-character input must
    /// produce byte-identical wire bodies. This is the property prompt
    /// caching (#766) depends on — any per-call drift in the sanitizer
    /// would invalidate the cache prefix and silently double Anthropic
    /// spend.
    #[test]
    fn test_sanitization_is_deterministic_across_adapter_calls() {
        let build = || {
            CompletionRequest::new("claude-sonnet-4-20250514").with_messages(vec![
                LlmMessage::system("sys"),
                LlmMessage::user("go"),
                LlmMessage {
                    role: "assistant".to_string(),
                    content: None,
                    reasoning_content: None,
                    tool_calls: Some(vec![ToolCall::new("call_!!!", "echo", "{}")]),
                    tool_call_id: None,
                },
                LlmMessage::tool_result("call_!!!", "ok"),
                LlmMessage::user("end"),
            ])
        };
        let a = serde_json::to_string(&to_anthropic_request(&build())).unwrap();
        let b = serde_json::to_string(&to_anthropic_request(&build())).unwrap();
        assert_eq!(
            a, b,
            "two adapter calls with identical (forbidden-char) input must serialise byte-identically"
        );
    }

    /// Already-valid IDs survive the sanitizer untouched in the wire body —
    /// the pre-#850 wire shape for valid IDs must remain byte-stable so we
    /// don't accidentally regress prompt cache hits for agents that never
    /// switched providers.
    #[test]
    fn test_already_valid_ids_unchanged_on_wire() {
        let req = CompletionRequest::new("claude-sonnet-4-20250514").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("toolu_01ABCdef", "echo", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("toolu_01ABCdef", "ok"),
            LlmMessage::user("end"),
        ]);

        let body = serde_json::to_value(to_anthropic_request(&req)).unwrap();
        let s = serde_json::to_string(&body).unwrap();
        assert!(
            s.contains("\"id\":\"toolu_01ABCdef\""),
            "valid tool_use.id must survive verbatim: {s}"
        );
        assert!(
            s.contains("\"tool_use_id\":\"toolu_01ABCdef\""),
            "valid tool_result.tool_use_id must survive verbatim: {s}"
        );
    }
}

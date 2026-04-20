//! Google Gemini `generateContent` / `streamGenerateContent` API types and
//! conversion functions.
//!
//! Converts between ALMS internal types (`CompletionRequest`/`CompletionResponse`)
//! and Gemini's wire format. Used by `LlmClient` when the resolved provider
//! entry has `kind = "gemini"`.
//!
//! # Wire shape at a glance
//!
//! - Roles: `user` / `model` (Gemini has no `assistant` or `system` role).
//! - System instructions live in a top-level `systemInstruction` field, not
//!   in `contents`.
//! - Messages are `contents[]` entries of the form `{role, parts: [Part, ...]}`.
//! - `Part` is a tagged union: `{text}`, `{functionCall}`, `{functionResponse}`.
//! - Tool use: assistant turns with tool calls become `model` entries whose
//!   parts include `functionCall` parts. Tool results become `user` entries
//!   whose parts include `functionResponse` parts.
//! - Tools are declared once as `tools[].functionDeclarations[]` with a
//!   JSON-Schema-subset `parameters` schema.
//! - Streaming endpoint: `...:streamGenerateContent?alt=sse` produces
//!   OpenAI-style `data: {json}` SSE events (no typed `event:` header,
//!   unlike Anthropic). Each event carries a `candidates[0].content.parts`
//!   snapshot-delta plus optional `usageMetadata`.
//!
//! The adapter mirrors the layout of `anthropic.rs`: types first, then
//! request-building, then response-decoding, then SSE parsing, then tests.

use crate::llm_client::SseParseResult;
use crate::llm_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct GeminiRequest {
    pub contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "systemInstruction")]
    pub system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<GeminiTool>>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "generationConfig")]
    pub generation_config: Option<GenerationConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct GeminiContent {
    /// `"user"` | `"model"`. Omitted for `systemInstruction` where Gemini
    /// doesn't require a role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    pub parts: Vec<GeminiPart>,
}

/// A single part within a `contents[]` entry.
///
/// Serialized as an **untagged** union — Gemini expects `{text: "..."}`,
/// `{functionCall: {...}}`, or `{functionResponse: {...}}`, never a tag
/// field. Deserialization relies on the presence of the discriminating key.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum GeminiPart {
    Text {
        text: String,
    },
    FunctionCall {
        #[serde(rename = "functionCall")]
        function_call: FunctionCallPart,
    },
    FunctionResponse {
        #[serde(rename = "functionResponse")]
        function_response: FunctionResponsePart,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionCallPart {
    pub name: String,
    /// Structured arguments object. Gemini requires an object, never a
    /// string. When the ALMS-internal `ToolCall::function::arguments`
    /// string fails to parse as JSON we fall back to wrapping it in an
    /// object so the request still serializes cleanly.
    ///
    /// `#[serde(default)]`: Gemini omits this field entirely for zero-arg
    /// tool calls (e.g. `list_agents`, `datetime`). Without the default,
    /// serde decode fails with `missing field 'args'` — in the non-stream
    /// path that surfaces as a runtime error, and in the streaming path
    /// `parse_gemini_sse` catches the parse error and returns
    /// `SseParseResult::Skip`, silently dropping the entire tool call.
    /// The default is `Value::Null`; conversion sites treat Null as `{}`.
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FunctionResponsePart {
    pub name: String,
    /// Gemini expects `response` to be an object. We wrap string tool
    /// outputs in `{"result": "..."}` so the on-the-wire shape matches.
    pub response: Value,
}

#[derive(Debug, Serialize)]
pub(crate) struct GeminiTool {
    #[serde(rename = "functionDeclarations")]
    pub function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
pub(crate) struct GeminiFunctionDeclaration {
    pub name: String,
    pub description: String,
    /// JSON Schema subset. Gemini rejects schemas with OpenAI-only keywords
    /// (`additionalProperties`, `$schema`, etc.), but our tool registry
    /// produces plain subsets so no rewriting is needed here.
    pub parameters: Value,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none", rename = "maxOutputTokens")]
    pub max_output_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
}

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct GeminiResponse {
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default, rename = "usageMetadata")]
    pub usage_metadata: Option<UsageMetadata>,
    /// Some versions of the API include a `modelVersion`; we capture it for
    /// the response but it's optional.
    #[serde(default, rename = "modelVersion")]
    pub model_version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Candidate {
    #[serde(default)]
    pub content: Option<GeminiContent>,
    #[serde(default, rename = "finishReason")]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone)]
pub(crate) struct UsageMetadata {
    #[serde(default, rename = "promptTokenCount")]
    pub prompt_token_count: u32,
    #[serde(default, rename = "candidatesTokenCount")]
    pub candidates_token_count: u32,
    #[serde(default, rename = "totalTokenCount")]
    pub total_token_count: u32,
}

// ---------------------------------------------------------------------------
// Conversion: internal → Gemini request
// ---------------------------------------------------------------------------

/// Convert an internal `CompletionRequest` to a `GeminiRequest`.
///
/// The input message list is expected to already satisfy the canonical
/// shape enforced by the context builder at the `LlmMessage` layer:
/// system prefix at the front, alternating `user`/`assistant`/`tool`
/// turns, trailing user turn, no empty-content messages, no pending
/// tool_calls.
///
/// The transformations here are the narrowly provider-specific ones:
///
/// - System messages get extracted into the top-level `systemInstruction`
///   field as a single concatenated text part.
/// - Assistant turns become `role="model"` entries. Tool calls on an
///   assistant turn become `functionCall` parts in that entry.
/// - Tool results become `role="user"` entries with `functionResponse`
///   parts (Gemini has no dedicated `tool` role).
///
/// # Wire-level alternation post-pass
///
/// Because the `role="tool"` → `role="user"` relabel happens *here*, the
/// canonical builder invariant ("no adjacent same-role messages") does
/// NOT automatically hold on the Gemini wire. Concretely: a canonical
/// tail `[tool_result, user_text]` becomes two adjacent `user` wire
/// entries. Gemini is more forgiving than Anthropic — it tolerates some
/// adjacent-role cases — but the invariant that `functionResponse` parts
/// share a `contents[]` entry with any adjacent user text following a
/// `model` turn that emitted `functionCall` parts is still the cleanest
/// shape and what the docs recommend. We therefore run
/// [`merge_consecutive_roles`] to concatenate adjacent same-role entries'
/// parts in order.
///
/// The post-merge `debug_assert!`s pin the wire-level invariant as a
/// tripwire for future regressions — mirrors `anthropic.rs`.
pub(crate) fn to_gemini_request(req: &CompletionRequest) -> GeminiRequest {
    let mut system_parts: Vec<String> = Vec::new();
    let mut contents: Vec<GeminiContent> = Vec::new();

    // Index every assistant tool_call by id → function name BEFORE the
    // main conversion loop. Gemini's `functionResponse` requires `name`
    // (it has no id field), so we need to recover the name from the
    // paired `functionCall` when converting a `tool` role message.
    //
    // Building this up-front (rather than walking back through already-
    // converted `contents`) is required for correctness in heterogeneous
    // parallel tool calls: e.g. an assistant turn emitting `fs_read` +
    // `fs_list` in parallel produces two distinct `functionCall` parts
    // in one `model` content entry. The old approach of walking back
    // through the most recent `model` entry and returning the LAST
    // `functionCall.name` gave both `functionResponse` parts the same
    // name on the wire, which Gemini rejects or mis-pairs. Keying by
    // the real `tool_call_id` (which ALMS carries end-to-end) maps each
    // result to its originating call reliably.
    let tool_name_by_id: HashMap<String, String> = req
        .messages
        .iter()
        .filter(|m| m.role == "assistant")
        .flat_map(|m| m.tool_calls.iter().flatten())
        .map(|tc| (tc.id.clone(), tc.function.name.clone()))
        .collect();

    for msg in &req.messages {
        match msg.role.as_str() {
            "system" => {
                if let Some(ref text) = msg.content {
                    system_parts.push(text.clone());
                }
            }
            "user" => {
                // Drop empty-content user turns instead of emitting a
                // `{text: ""}` part. Gemini's docs suggest non-empty
                // parts; the context builder should never produce this
                // shape, but guarding here prevents a bad-input regression
                // from turning into a 400. Mirrors the assistant arm's
                // `!parts.is_empty()` skip.
                let text = match msg.content.as_deref() {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ => continue,
                };
                contents.push(GeminiContent {
                    role: Some("user".to_string()),
                    parts: vec![GeminiPart::Text { text }],
                });
            }
            "assistant" => {
                let mut parts: Vec<GeminiPart> = Vec::new();
                if let Some(ref text) = msg.content
                    && !text.is_empty()
                {
                    parts.push(GeminiPart::Text { text: text.clone() });
                }
                if let Some(ref tool_calls) = msg.tool_calls {
                    for tc in tool_calls {
                        let args: Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "Malformed tool arguments for {}: {} — wrapping as string",
                                    tc.function.name,
                                    e
                                );
                                // Gemini requires `args` to be an object;
                                // wrap invalid JSON so the request still
                                // serializes and the model can at least
                                // see the raw argument.
                                serde_json::json!({ "_raw": tc.function.arguments })
                            });
                        // Objectify scalar/array JSON since Gemini's `args`
                        // is typed as an object in the schema.
                        let args = if args.is_object() {
                            args
                        } else {
                            serde_json::json!({ "_value": args })
                        };
                        parts.push(GeminiPart::FunctionCall {
                            function_call: FunctionCallPart {
                                name: tc.function.name.clone(),
                                args,
                            },
                        });
                    }
                }
                // Skip entirely empty model turns (no text, no tool calls).
                // These can slip through when the context builder sees a
                // bare `assistant` with None content and no tool_calls —
                // Gemini 400s on empty `parts[]`.
                if !parts.is_empty() {
                    contents.push(GeminiContent {
                        role: Some("model".to_string()),
                        parts,
                    });
                }
            }
            "tool" => {
                // Tool result → functionResponse part inside a user entry.
                // We recover the paired tool name via the `id → name` map
                // built from assistant turns above. Gemini's
                // `functionResponse` requires `name` (it has no id field),
                // so a missing name produces a wire-invalid payload.
                let tool_call_id = msg.tool_call_id.as_deref().unwrap_or_default();
                let name = tool_name_by_id
                    .get(tool_call_id)
                    .cloned()
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            "No matching functionCall found for tool_call_id={:?}; \
                             Gemini requires a name in functionResponse. Using tool_call_id as fallback.",
                            msg.tool_call_id
                        );
                        // Fallback: raw id string. Won't match any entry
                        // in `tools[0].functionDeclarations[]`, so Gemini
                        // will likely reject — but this path is only
                        // reached when the assistant turn that paired
                        // this result has been elided entirely from the
                        // message list (e.g. aggressive summarization).
                        msg.tool_call_id.clone().unwrap_or_default()
                    });

                let raw = msg.content.clone().unwrap_or_default();
                // Wrap the tool output as an object. Prefer parsed JSON
                // when the tool returned structured output; fall back to
                // a `{result: "..."}` object so the wire shape is always
                // an object (Gemini rejects scalars here).
                let response = match serde_json::from_str::<Value>(&raw) {
                    Ok(v) if v.is_object() => v,
                    Ok(v) => serde_json::json!({ "result": v }),
                    Err(_) => serde_json::json!({ "result": raw }),
                };

                let part = GeminiPart::FunctionResponse {
                    function_response: FunctionResponsePart { name, response },
                };
                // If the previous entry is a user, append this part to it
                // (matches the docs' recommendation that `functionResponse`
                // parts share a contents[] entry with adjacent user text).
                // Otherwise create a fresh user entry.
                let can_append = matches!(
                    contents.last(),
                    Some(GeminiContent { role: Some(r), .. }) if r == "user"
                );
                if can_append {
                    if let Some(last) = contents.last_mut() {
                        last.parts.push(part);
                    }
                } else {
                    contents.push(GeminiContent {
                        role: Some("user".to_string()),
                        parts: vec![part],
                    });
                }
            }
            _ => {}
        }
    }

    // Wire-level alternation pass. The builder enforces alternation at
    // the `LlmMessage` layer where `user` and `tool` are distinct roles,
    // but the `tool` → `user` relabel above can introduce adjacent `user`
    // wire entries. We concatenate them here so the emitted wire shape
    // alternates `user`/`model` cleanly — this is both idiomatic Gemini
    // and a safe superset of what the docs explicitly require.
    merge_consecutive_roles(&mut contents);

    // Post-conditions. Tripwires for future regressions — identical in
    // spirit to the Anthropic adapter's assertions.
    debug_assert!(
        !contents.is_empty(),
        "gemini adapter received empty contents array after system extraction — \
         context builder must guarantee at least one non-system message"
    );
    debug_assert_eq!(
        contents.last().and_then(|c| c.role.as_deref()),
        Some("user"),
        "gemini adapter: contents does not end with user role — \
         context builder must guarantee trailing user turn"
    );
    debug_assert!(
        contents
            .windows(2)
            .all(|w| w[0].role.as_deref() != w[1].role.as_deref()),
        "gemini adapter: adjacent same-role entries on the wire — \
         merge_consecutive_roles failed to coalesce (roles={:?})",
        contents
            .iter()
            .map(|c| c.role.as_deref())
            .collect::<Vec<_>>()
    );

    let system_instruction = if system_parts.is_empty() {
        None
    } else {
        Some(GeminiContent {
            role: None,
            parts: vec![GeminiPart::Text {
                text: system_parts.join("\n\n"),
            }],
        })
    };

    let tools = req.tools.as_ref().map(|defs| {
        // Gemini takes a single tools entry with a `functionDeclarations`
        // array, not one entry per function.
        vec![GeminiTool {
            function_declarations: defs
                .iter()
                .map(|d| GeminiFunctionDeclaration {
                    name: d.function.name.clone(),
                    description: d.function.description.clone(),
                    parameters: d.function.parameters.clone(),
                })
                .collect(),
        }]
    });

    let generation_config = if req.max_tokens.is_some() || req.temperature.is_some() {
        Some(GenerationConfig {
            max_output_tokens: req.max_tokens,
            temperature: req.temperature,
        })
    } else {
        None
    };

    GeminiRequest {
        contents,
        system_instruction,
        tools,
        generation_config,
    }
}

/// Merge adjacent same-role `GeminiContent`s by concatenating their parts
/// in order. Needed at the adapter boundary because the `role="tool"` →
/// `role="user"` relabel that happens during [`to_gemini_request`] can
/// create adjacencies that the canonical `Vec<LlmMessage>` shape forbids.
///
/// Mirrors `merge_consecutive_roles` in `anthropic.rs`.
fn merge_consecutive_roles(contents: &mut Vec<GeminiContent>) {
    if contents.len() < 2 {
        return;
    }
    let mut merged: Vec<GeminiContent> = Vec::with_capacity(contents.len());
    for entry in contents.drain(..) {
        if let Some(prev) = merged.last_mut()
            && prev.role == entry.role
        {
            prev.parts.extend(entry.parts);
            continue;
        }
        merged.push(entry);
    }
    *contents = merged;
}

// ---------------------------------------------------------------------------
// Conversion: Gemini response → internal
// ---------------------------------------------------------------------------

/// Convert a `GeminiResponse` to an internal `CompletionResponse`.
pub(crate) fn from_gemini_response(resp: GeminiResponse) -> CompletionResponse {
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let finish_reason_raw = resp
        .candidates
        .first()
        .and_then(|c| c.finish_reason.clone());

    // Gemini doesn't emit tool-call ids; generate stable synthetic ones so
    // our downstream tool loop can pair call → result correctly.
    let mut synthetic_id_counter: usize = 0;

    if let Some(cand) = resp.candidates.into_iter().next()
        && let Some(content) = cand.content
    {
        for part in content.parts {
            match part {
                GeminiPart::Text { text } => {
                    text_parts.push(text);
                }
                GeminiPart::FunctionCall { function_call } => {
                    synthetic_id_counter += 1;
                    let id = format!("gemini_call_{synthetic_id_counter}");
                    // Normalize zero-arg tool calls: Gemini may omit
                    // `args` entirely (decodes to `Value::Null`). Emit an
                    // empty object string so downstream tool execution
                    // sees a valid JSON object, not the literal "null".
                    let args_str = if function_call.args.is_null() {
                        "{}".to_string()
                    } else {
                        function_call.args.to_string()
                    };
                    tool_calls.push(ToolCall::new(id, function_call.name, args_str));
                }
                // Tool results don't appear in responses, but we silently
                // tolerate them in case Gemini ever echoes one back.
                GeminiPart::FunctionResponse { .. } => {}
            }
        }
    }

    let finish_reason = finish_reason_raw.map(|r| match r.as_str() {
        "STOP" => "stop".to_string(),
        "MAX_TOKENS" => "length".to_string(),
        "TOOL_USE" | "FUNCTION_CALL" => "tool_calls".to_string(),
        other => other.to_ascii_lowercase(),
    });

    // If there are tool calls but Gemini returned a plain STOP (it does
    // this) map to `tool_calls` — the agent loop uses this signal.
    let finish_reason = match (finish_reason, tool_calls.is_empty()) {
        (Some(r), false) if r == "stop" => Some("tool_calls".to_string()),
        (r, _) => r,
    };

    let usage = resp.usage_metadata.map(|u| Usage {
        prompt_tokens: u.prompt_token_count,
        completion_tokens: u.candidates_token_count,
        total_tokens: if u.total_token_count > 0 {
            u.total_token_count
        } else {
            u.prompt_token_count + u.candidates_token_count
        },
    });

    CompletionResponse {
        id: String::new(),
        object: "chat.completion".to_string(),
        created: 0,
        model: resp.model_version.unwrap_or_default(),
        choices: vec![Choice {
            index: 0,
            message: LlmMessage {
                role: "assistant".to_string(),
                content: if text_parts.is_empty() {
                    None
                } else {
                    Some(text_parts.join(""))
                },
                reasoning_content: None,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: None,
            },
            finish_reason,
        }],
        usage,
    }
}

// ---------------------------------------------------------------------------
// Streaming: parse Gemini SSE events
// ---------------------------------------------------------------------------

/// Parse a single Gemini SSE `data:` payload into an internal `StreamChunk`.
///
/// Unlike Anthropic, Gemini's `streamGenerateContent?alt=sse` produces
/// OpenAI-style `data: {json}` events (no typed `event:` header). Each
/// event carries a full `GeminiResponse` delta — the `candidates[0].content.parts`
/// field contains *new* parts to append, and `usageMetadata` is updated
/// across events.
///
/// We map text parts to content deltas, functionCall parts to tool_call
/// deltas (emitting id + name + full-JSON args in one shot since Gemini
/// doesn't split functionCall across events), and finishReason to the
/// terminal event signal. The agent loop reassembles the full message
/// from the accumulated chunks.
pub(crate) fn parse_gemini_sse(data: &str) -> SseParseResult {
    let data = data.trim();
    if data.is_empty() {
        return SseParseResult::Skip;
    }
    // Gemini may or may not emit a `[DONE]` sentinel. Tolerate both paths.
    if data == "[DONE]" {
        return SseParseResult::Done;
    }

    let event: GeminiResponse = match serde_json::from_str(data) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!("Failed to parse Gemini SSE chunk: {} — data: {}", e, data);
            return SseParseResult::Skip;
        }
    };

    let mut content_text: Option<String> = None;
    let mut tool_call_deltas: Vec<ToolCallDelta> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut synthetic_id_counter: usize = 0;

    if let Some(cand) = event.candidates.into_iter().next() {
        finish_reason = cand.finish_reason.map(|r| match r.as_str() {
            "STOP" => "stop".to_string(),
            "MAX_TOKENS" => "length".to_string(),
            "TOOL_USE" | "FUNCTION_CALL" => "tool_calls".to_string(),
            other => other.to_ascii_lowercase(),
        });

        if let Some(content) = cand.content {
            let mut text_acc = String::new();
            for part in content.parts {
                match part {
                    GeminiPart::Text { text } => {
                        text_acc.push_str(&text);
                    }
                    GeminiPart::FunctionCall { function_call } => {
                        synthetic_id_counter += 1;
                        let id = format!("gemini_call_{synthetic_id_counter}");
                        // Gemini emits full function calls atomically in a
                        // single event; we can pass id + name + args in
                        // one delta rather than splitting across chunks.
                        // Zero-arg calls arrive with `args` absent
                        // (`Value::Null` via `#[serde(default)]`); emit
                        // `"{}"` so downstream argument parsing succeeds.
                        let args_str = if function_call.args.is_null() {
                            "{}".to_string()
                        } else {
                            function_call.args.to_string()
                        };
                        tool_call_deltas.push(ToolCallDelta {
                            index: (synthetic_id_counter - 1) as u32,
                            id: Some(id),
                            function: Some(FunctionCallDelta {
                                name: Some(function_call.name),
                                arguments: Some(args_str),
                            }),
                        });
                    }
                    GeminiPart::FunctionResponse { .. } => {}
                }
            }
            if !text_acc.is_empty() {
                content_text = Some(text_acc);
            }
        }
    }

    // Map terminal finish_reason from stop → tool_calls when the
    // candidate emitted function calls. We ONLY override when upstream
    // already delivered a finish_reason — i.e. the terminal chunk. On
    // mid-stream chunks we leave `finish_reason = None` so downstream
    // consumers that (today or in the future) trust per-chunk
    // finish_reason can't mistake a mid-stream functionCall delta for
    // an end-of-turn signal. The agent loop's `stream_llm_call` only
    // consumes the accumulator at stream end, so this is semantically
    // neutral today but defensive against future regressions.
    if !tool_call_deltas.is_empty() && matches!(finish_reason.as_deref(), Some("stop")) {
        finish_reason = Some("tool_calls".to_string());
    }

    let usage = event.usage_metadata.map(|u| Usage {
        prompt_tokens: u.prompt_token_count,
        completion_tokens: u.candidates_token_count,
        total_tokens: if u.total_token_count > 0 {
            u.total_token_count
        } else {
            u.prompt_token_count + u.candidates_token_count
        },
    });

    // Nothing to emit (e.g. empty heartbeat event) — skip.
    if content_text.is_none()
        && tool_call_deltas.is_empty()
        && finish_reason.is_none()
        && usage.is_none()
    {
        return SseParseResult::Skip;
    }

    SseParseResult::Chunk(StreamChunk {
        id: String::new(),
        object: "chat.completion.chunk".to_string(),
        created: 0,
        model: String::new(),
        choices: vec![StreamChoice {
            index: 0,
            delta: Delta {
                role: if content_text.is_some() || !tool_call_deltas.is_empty() {
                    Some("assistant".to_string())
                } else {
                    None
                },
                content: content_text,
                reasoning_content: None,
                tool_calls: if tool_call_deltas.is_empty() {
                    None
                } else {
                    Some(tool_call_deltas)
                },
            },
            finish_reason,
        }],
        usage,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_gemini_request_basic() {
        let req = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![
                LlmMessage::system("You are helpful."),
                LlmMessage::user("Hello"),
            ])
            .with_max_tokens(1024);

        let gemini_req = to_gemini_request(&req);

        // system_instruction was extracted
        let sys = gemini_req.system_instruction.as_ref().unwrap();
        assert!(sys.role.is_none());
        match &sys.parts[0] {
            GeminiPart::Text { text } => assert_eq!(text, "You are helpful."),
            other => panic!("expected system text part, got {other:?}"),
        }

        // contents has exactly one user entry
        assert_eq!(gemini_req.contents.len(), 1);
        assert_eq!(gemini_req.contents[0].role.as_deref(), Some("user"));

        // generationConfig respects max_tokens
        let cfg = gemini_req.generation_config.as_ref().unwrap();
        assert_eq!(cfg.max_output_tokens, Some(1024));
    }

    #[test]
    fn test_to_gemini_request_assistant_role_becomes_model() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("hi"),
            LlmMessage::assistant("hello back"),
            LlmMessage::user("follow up"),
        ]);

        let gemini_req = to_gemini_request(&req);
        let roles: Vec<_> = gemini_req
            .contents
            .iter()
            .map(|c| c.role.as_deref().unwrap())
            .collect();
        assert_eq!(roles, vec!["user", "model", "user"]);
    }

    #[test]
    fn test_to_gemini_request_with_tool_calls() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
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

        let gemini_req = to_gemini_request(&req);
        assert_eq!(gemini_req.contents.len(), 3);
        // user (text), model (functionCall), user (functionResponse)
        assert_eq!(gemini_req.contents[0].role.as_deref(), Some("user"));
        assert_eq!(gemini_req.contents[1].role.as_deref(), Some("model"));
        assert_eq!(gemini_req.contents[2].role.as_deref(), Some("user"));

        match &gemini_req.contents[1].parts[0] {
            GeminiPart::FunctionCall { function_call } => {
                assert_eq!(function_call.name, "shell_exec");
                assert_eq!(function_call.args["command"].as_str(), Some("ls"));
            }
            other => panic!("expected FunctionCall, got {other:?}"),
        }

        match &gemini_req.contents[2].parts[0] {
            GeminiPart::FunctionResponse { function_response } => {
                // Name was recovered from the preceding model turn's functionCall
                assert_eq!(function_response.name, "shell_exec");
                // Plain string tool output wrapped as {"result": "..."}
                assert_eq!(
                    function_response.response["result"].as_str(),
                    Some("file1.txt")
                );
            }
            other => panic!("expected FunctionResponse, got {other:?}"),
        }
    }

    #[test]
    fn test_tool_definition_conversion() {
        let req = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![LlmMessage::user("hi")])
            .with_tools(vec![
                ToolDefinition::new("echo", "Echo text").with_parameters(serde_json::json!({
                    "type": "object",
                    "properties": { "text": { "type": "string" } },
                    "required": ["text"]
                })),
            ]);

        let gemini_req = to_gemini_request(&req);
        let tools = gemini_req.tools.unwrap();
        assert_eq!(tools.len(), 1, "gemini uses a single tools entry");
        assert_eq!(tools[0].function_declarations.len(), 1);
        assert_eq!(tools[0].function_declarations[0].name, "echo");
        assert_eq!(tools[0].function_declarations[0].description, "Echo text");
        assert!(
            tools[0].function_declarations[0]
                .parameters
                .get("properties")
                .is_some()
        );
    }

    #[test]
    fn test_tool_result_jsons_parsed_when_object() {
        // A tool result that is already a JSON object should flow through
        // as-is, not get re-wrapped in `{"result": ...}`.
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::user("go"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("c1", "get_weather", "{}")]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", r#"{"temp": 72, "unit": "F"}"#),
        ]);

        let gemini_req = to_gemini_request(&req);
        let tail = gemini_req.contents.last().unwrap();
        match &tail.parts[0] {
            GeminiPart::FunctionResponse { function_response } => {
                assert_eq!(function_response.response["temp"].as_i64(), Some(72));
                assert_eq!(function_response.response["unit"].as_str(), Some("F"));
            }
            other => panic!("expected FunctionResponse, got {other:?}"),
        }
    }

    #[test]
    fn test_from_gemini_response_text_only() {
        let resp = GeminiResponse {
            candidates: vec![Candidate {
                content: Some(GeminiContent {
                    role: Some("model".into()),
                    parts: vec![GeminiPart::Text {
                        text: "Hello!".into(),
                    }],
                }),
                finish_reason: Some("STOP".into()),
            }],
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 10,
                candidates_token_count: 5,
                total_token_count: 15,
            }),
            model_version: Some("gemini-2.5-pro-001".into()),
        };

        let completion = from_gemini_response(resp);
        assert_eq!(completion.choices.len(), 1);
        assert_eq!(completion.choices[0].message.content_str(), "Hello!");
        assert!(completion.choices[0].message.tool_calls.is_none());
        assert_eq!(completion.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, 15);
    }

    #[test]
    fn test_from_gemini_response_with_function_call() {
        let resp = GeminiResponse {
            candidates: vec![Candidate {
                content: Some(GeminiContent {
                    role: Some("model".into()),
                    parts: vec![GeminiPart::FunctionCall {
                        function_call: FunctionCallPart {
                            name: "echo".into(),
                            args: serde_json::json!({"text": "hi"}),
                        },
                    }],
                }),
                // Gemini often reports STOP even for tool-use terminators.
                finish_reason: Some("STOP".into()),
            }],
            usage_metadata: Some(UsageMetadata {
                prompt_token_count: 20,
                candidates_token_count: 10,
                total_token_count: 30,
            }),
            model_version: None,
        };

        let completion = from_gemini_response(resp);
        let msg = &completion.choices[0].message;
        assert!(msg.content.is_none());
        let calls = msg.tool_calls.as_ref().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "echo");
        assert!(calls[0].id.starts_with("gemini_call_"));
        // STOP → tool_calls when tool calls are present
        assert_eq!(
            completion.choices[0].finish_reason.as_deref(),
            Some("tool_calls")
        );
    }

    #[test]
    fn test_parse_gemini_sse_text_delta() {
        // Shape of a Gemini streamGenerateContent?alt=sse event body.
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"Hello"}]},"finishReason":null}]}"#;
        let result = parse_gemini_sse(data);
        match result {
            SseParseResult::Chunk(chunk) => {
                assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
                assert_eq!(chunk.choices[0].delta.role.as_deref(), Some("assistant"));
            }
            _ => panic!("Expected Chunk"),
        }
    }

    #[test]
    fn test_parse_gemini_sse_function_call() {
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"echo","args":{"text":"hi"}}}]},"finishReason":"STOP"}]}"#;
        let result = parse_gemini_sse(data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let tc = chunk.choices[0].delta.tool_calls.as_ref().unwrap();
                assert_eq!(tc.len(), 1);
                assert_eq!(
                    tc[0].function.as_ref().unwrap().name.as_deref(),
                    Some("echo")
                );
                // STOP but with tool_calls → mapped to "tool_calls"
                assert_eq!(
                    chunk.choices[0].finish_reason.as_deref(),
                    Some("tool_calls")
                );
            }
            _ => panic!("Expected Chunk"),
        }
    }

    #[test]
    fn test_parse_gemini_sse_usage_only() {
        // Final event: no content, only usageMetadata + finishReason.
        let data = r#"{"candidates":[{"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":100,"candidatesTokenCount":42,"totalTokenCount":142}}"#;
        let result = parse_gemini_sse(data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let usage = chunk.usage.expect("usage should be present");
                assert_eq!(usage.prompt_tokens, 100);
                assert_eq!(usage.completion_tokens, 42);
                assert_eq!(usage.total_tokens, 142);
                assert_eq!(chunk.choices[0].finish_reason.as_deref(), Some("stop"));
            }
            _ => panic!("Expected Chunk"),
        }
    }

    #[test]
    fn test_parse_gemini_sse_done_sentinel() {
        assert!(matches!(parse_gemini_sse("[DONE]"), SseParseResult::Done));
    }

    #[test]
    fn test_parse_gemini_sse_skips_unparseable() {
        assert!(matches!(
            parse_gemini_sse("not valid json"),
            SseParseResult::Skip
        ));
    }

    /// Wire-level alternation invariant: after `to_gemini_request`, no two
    /// adjacent entries share a role — even when the canonical
    /// `Vec<LlmMessage>` tail is `[tool_result, user_text]`. Parallel of
    /// the Anthropic `test_wire_alternation_tool_result_then_fresh_user`
    /// test.
    #[test]
    fn test_wire_alternation_tool_result_then_fresh_user() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
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

        let gemini_req = to_gemini_request(&req);

        let roles: Vec<Option<&str>> = gemini_req
            .contents
            .iter()
            .map(|c| c.role.as_deref())
            .collect();
        for w in roles.windows(2) {
            assert_ne!(
                w[0], w[1],
                "wire-level adjacent same-role entries: roles={roles:?}"
            );
        }

        // Concrete shape: user("go"), model(functionCall),
        // user(functionResponse + text("fresh input")).
        assert_eq!(gemini_req.contents.len(), 3);
        let tail = &gemini_req.contents[2];
        assert_eq!(tail.role.as_deref(), Some("user"));
        assert_eq!(
            tail.parts.len(),
            2,
            "functionResponse + fresh user text must share a contents[] entry"
        );
        assert!(matches!(tail.parts[0], GeminiPart::FunctionResponse { .. }));
        match &tail.parts[1] {
            GeminiPart::Text { text } => assert_eq!(text, "fresh input"),
            other => panic!("expected text part for fresh input, got {other:?}"),
        }
    }

    /// Parallel of the Anthropic
    /// `test_tool_result_appended_to_preceding_user_blocks` test.
    #[test]
    fn test_multiple_tool_results_merged_into_one_user_entry() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
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

        let gemini_req = to_gemini_request(&req);

        // Wire shape: user("first"), model(2x functionCall),
        // user(2x functionResponse + text("follow up"))
        assert_eq!(gemini_req.contents.len(), 3);
        let tail = &gemini_req.contents[2];
        assert_eq!(tail.role.as_deref(), Some("user"));
        assert_eq!(tail.parts.len(), 3);
        assert!(matches!(tail.parts[0], GeminiPart::FunctionResponse { .. }));
        assert!(matches!(tail.parts[1], GeminiPart::FunctionResponse { .. }));
        match &tail.parts[2] {
            GeminiPart::Text { text } => assert_eq!(text, "follow up"),
            other => panic!("expected text part for follow up, got {other:?}"),
        }
    }

    #[test]
    fn test_gemini_request_serializes_correctly() {
        // Sanity: the serialized JSON should match Gemini's wire format.
        let req = CompletionRequest::new("gemini-2.5-pro")
            .with_messages(vec![
                LlmMessage::system("be helpful"),
                LlmMessage::user("hi"),
            ])
            .with_max_tokens(256);

        let gemini_req = to_gemini_request(&req);
        let body = serde_json::to_value(&gemini_req).unwrap();

        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be helpful");
        assert_eq!(body["contents"][0]["role"], "user");
        assert_eq!(body["contents"][0]["parts"][0]["text"], "hi");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 256);
        // functionCall/functionResponse untagged serialization check.
        assert!(body.get("tools").is_none());
    }

    /// Parallel of Anthropic `test_wire_alternation_continue_placeholder_after_tool_result`.
    #[test]
    fn test_wire_alternation_continue_placeholder_after_tool_result() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
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

        let gemini_req = to_gemini_request(&req);

        let roles: Vec<Option<&str>> = gemini_req
            .contents
            .iter()
            .map(|c| c.role.as_deref())
            .collect();
        for w in roles.windows(2) {
            assert_ne!(
                w[0], w[1],
                "wire-level adjacent same-role entries: roles={roles:?}"
            );
        }
    }

    /// Regression test for Tim's blocker #1: heterogeneous parallel tool
    /// calls. Two assistant tool_calls with DIFFERENT names in one turn,
    /// followed by two tool_results. Each `functionResponse.name` must
    /// match its paired `functionCall.name` — previously the adapter
    /// returned the LAST functionCall name for every tool_result, so
    /// both responses went out labelled the same and Gemini 400'd or
    /// mis-paired results with calls.
    #[test]
    fn test_heterogeneous_parallel_tool_calls_preserve_names() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("read and list"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![
                    ToolCall::new("c1", "fs_read", r#"{"path":"a.txt"}"#),
                    ToolCall::new("c2", "fs_list", r#"{"path":"."}"#),
                ]),
                tool_call_id: None,
            },
            LlmMessage::tool_result("c1", "contents of a.txt"),
            LlmMessage::tool_result("c2", "a.txt\nb.txt"),
        ]);

        let gemini_req = to_gemini_request(&req);

        // Expected wire shape: user("read and list"),
        // model(2x functionCall),
        // user(2x functionResponse).
        assert_eq!(gemini_req.contents.len(), 3);
        let tail = &gemini_req.contents[2];
        assert_eq!(tail.role.as_deref(), Some("user"));
        assert_eq!(tail.parts.len(), 2);

        let names: Vec<&str> = tail
            .parts
            .iter()
            .map(|p| match p {
                GeminiPart::FunctionResponse { function_response } => {
                    function_response.name.as_str()
                }
                other => panic!("expected FunctionResponse, got {other:?}"),
            })
            .collect();
        // Critical invariant: first response is paired with fs_read,
        // second with fs_list — NOT both labelled with the last name.
        assert_eq!(
            names,
            vec!["fs_read", "fs_list"],
            "functionResponse names must match their paired functionCall names; \
             heterogeneous parallel tool calls regressed"
        );
    }

    /// Regression test for Tim's blocker #2: zero-arg function-call
    /// decode. Gemini omits `args` entirely for tools with no parameters
    /// (e.g. `list_agents`, `datetime`). Without `#[serde(default)]` this
    /// fails with "missing field 'args'" — in the streaming path that
    /// surfaces as `SseParseResult::Skip`, silently dropping the entire
    /// tool call.
    #[test]
    fn test_function_call_decodes_with_missing_args_field() {
        // Non-stream response decode path.
        let json = r#"{
            "candidates":[{
                "content":{
                    "role":"model",
                    "parts":[{"functionCall":{"name":"list_agents"}}]
                },
                "finishReason":"STOP"
            }]
        }"#;
        let resp: GeminiResponse =
            serde_json::from_str(json).expect("decode must succeed without args field");
        let completion = from_gemini_response(resp);
        let calls = completion.choices[0]
            .message
            .tool_calls
            .as_ref()
            .expect("tool_calls must be populated");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "list_agents");
        // Zero-arg → normalized to `{}` string rather than `null`, so
        // downstream argument parsing (serde_json::from_str::<Value>)
        // yields an empty object rather than Null.
        assert_eq!(
            calls[0].function.arguments, "{}",
            "zero-arg calls must emit '{{}}' not 'null' so tool handlers see an empty object"
        );
    }

    /// Regression test for blocker #2, SSE path. Zero-arg functionCall
    /// events must parse (not Skip) and must produce a tool_call delta
    /// with `arguments = "{}"` so the agent loop doesn't silently lose
    /// the tool call.
    #[test]
    fn test_parse_gemini_sse_function_call_without_args() {
        let data = r#"{"candidates":[{"content":{"role":"model","parts":[{"functionCall":{"name":"list_agents"}}]},"finishReason":"STOP"}]}"#;
        let result = parse_gemini_sse(data);
        match result {
            SseParseResult::Chunk(chunk) => {
                let tc = chunk.choices[0]
                    .delta
                    .tool_calls
                    .as_ref()
                    .expect("tool_calls must be populated, not Skip");
                assert_eq!(tc.len(), 1);
                let f = tc[0].function.as_ref().unwrap();
                assert_eq!(f.name.as_deref(), Some("list_agents"));
                assert_eq!(
                    f.arguments.as_deref(),
                    Some("{}"),
                    "zero-arg streaming tool calls must emit '{{}}' not 'null'"
                );
                assert_eq!(
                    chunk.choices[0].finish_reason.as_deref(),
                    Some("tool_calls")
                );
            }
            _ => panic!(
                "expected Chunk for zero-arg function call, got Skip/Done — \
                 the missing `#[serde(default)]` regressed"
            ),
        }
    }

    /// Suggestion #4: tool result that is a valid non-object JSON value
    /// (e.g. an array) should be wrapped as `{"result": <value>}` so the
    /// wire-shape stays an object as Gemini requires. Previously this
    /// branch was only exercised implicitly via the raw-string (Err)
    /// path.
    #[test]
    fn test_tool_result_non_object_json_wrapped() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::user("list my ids"),
            LlmMessage {
                role: "assistant".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: Some(vec![ToolCall::new("c1", "get_ids", "{}")]),
                tool_call_id: None,
            },
            // Valid JSON, but an array — not an object.
            LlmMessage::tool_result("c1", "[1,2,3]"),
        ]);

        let gemini_req = to_gemini_request(&req);
        let tail = gemini_req.contents.last().unwrap();
        match &tail.parts[0] {
            GeminiPart::FunctionResponse { function_response } => {
                // Preserved as array under `result`, not stringified.
                let arr = function_response
                    .response
                    .get("result")
                    .and_then(|v| v.as_array())
                    .expect("non-object JSON tool result must be wrapped in `result` as JSON");
                assert_eq!(arr.len(), 3);
                assert_eq!(arr[0].as_i64(), Some(1));
            }
            other => panic!("expected FunctionResponse, got {other:?}"),
        }
    }

    /// Suggestion #5: empty-content user messages must not produce a
    /// `{role:"user", parts:[{text:""}]}` entry on the wire. Gemini's
    /// docs require non-empty parts; the context builder shouldn't
    /// produce this shape but the adapter must defend against it.
    #[test]
    fn test_empty_content_user_message_dropped() {
        let req = CompletionRequest::new("gemini-2.5-pro").with_messages(vec![
            LlmMessage::system("sys"),
            LlmMessage::user("first"),
            // Degenerate: content = None.
            LlmMessage {
                role: "user".to_string(),
                content: None,
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage::assistant("reply"),
            // Degenerate: empty string.
            LlmMessage {
                role: "user".to_string(),
                content: Some(String::new()),
                reasoning_content: None,
                tool_calls: None,
                tool_call_id: None,
            },
            LlmMessage::user("real trailing"),
        ]);

        let gemini_req = to_gemini_request(&req);

        // Expected: user("first"), model("reply"), user("real trailing").
        // The two degenerate user turns are skipped.
        let roles: Vec<Option<&str>> = gemini_req
            .contents
            .iter()
            .map(|c| c.role.as_deref())
            .collect();
        assert_eq!(roles, vec![Some("user"), Some("model"), Some("user")]);

        for entry in &gemini_req.contents {
            for part in &entry.parts {
                if let GeminiPart::Text { text } = part {
                    assert!(!text.is_empty(), "empty text parts must be skipped");
                }
            }
        }
    }
}

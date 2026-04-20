//! Anthropic Messages API types and conversion functions.
//!
//! Converts between ALMS internal types (`CompletionRequest`/`CompletionResponse`)
//! and Anthropic's wire format. Used by `LlmClient` when `provider == "anthropic"`.

use crate::llm_client::SseParseResult;
use crate::llm_types::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicRequest {
    pub model: String,
    pub messages: Vec<AnthropicMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    pub max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
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
}

#[derive(Debug, Serialize)]
pub(crate) struct AnthropicTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
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
    // message_delta fields (deserialized but only usage is read)
    #[serde(default)]
    pub _stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<AnthropicUsage>,
}

// ---------------------------------------------------------------------------
// Conversion: internal → Anthropic request
// ---------------------------------------------------------------------------

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
                        let input: Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|e| {
                                tracing::warn!(
                                    "Malformed tool arguments for {}: {}",
                                    tc.function.name,
                                    e
                                );
                                Value::String(tc.function.arguments.clone())
                            });
                        blocks.push(ContentBlock::ToolUse {
                            id: tc.id.clone(),
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
                    tool_use_id: msg.tool_call_id.clone().unwrap_or_default(),
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

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    let tools = req.tools.as_ref().map(|defs| {
        defs.iter()
            .map(|d| AnthropicTool {
                name: d.function.name.clone(),
                description: d.function.description.clone(),
                input_schema: d.function.parameters.clone(),
            })
            .collect()
    });

    AnthropicRequest {
        model: req.model.clone(),
        messages,
        system,
        tools,
        max_tokens: req.max_tokens.unwrap_or(100_000),
        stream: req.stream,
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
        usage: Some(Usage {
            prompt_tokens: resp.usage.input_tokens,
            completion_tokens: resp.usage.output_tokens,
            total_tokens: resp.usage.input_tokens + resp.usage.output_tokens,
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
        assert_eq!(anthropic_req.system.as_deref(), Some("You are helpful."));
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
}

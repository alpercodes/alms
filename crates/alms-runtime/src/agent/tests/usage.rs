// SPDX-License-Identifier: Apache-2.0

//! Token-usage accumulation across streamed chunks.

use crate::llm_types::*;

/// Verify that streaming usage is accumulated (merged) across chunks rather
/// than overwritten.  Anthropic sends `input_tokens` in `message_start` and
/// `output_tokens` in `message_delta` as separate events; the accumulator
/// must combine them.
#[test]
fn test_streaming_usage_accumulation() {
    // Simulate two chunks: first has prompt_tokens, second has completion_tokens
    let first = Usage {
        prompt_tokens: 150,
        total_tokens: 150,
        ..Usage::default()
    };
    let second = Usage {
        completion_tokens: 75,
        total_tokens: 75,
        ..Usage::default()
    };

    // Replicate the accumulation logic from stream_llm_call
    let mut usage: Option<Usage> = None;

    // Process first chunk
    let chunk_usage = first;
    usage = Some(match usage {
        Some(prev) => Usage {
            prompt_tokens: prev.prompt_tokens.max(chunk_usage.prompt_tokens),
            completion_tokens: prev.completion_tokens.max(chunk_usage.completion_tokens),
            ..Usage::default()
        },
        None => chunk_usage,
    });
    if let Some(ref mut u) = usage {
        u.total_tokens = u.prompt_tokens + u.completion_tokens;
    }

    // Process second chunk
    let chunk_usage = second;
    usage = Some(match usage {
        Some(prev) => Usage {
            prompt_tokens: prev.prompt_tokens.max(chunk_usage.prompt_tokens),
            completion_tokens: prev.completion_tokens.max(chunk_usage.completion_tokens),
            ..Usage::default()
        },
        None => chunk_usage,
    });
    if let Some(ref mut u) = usage {
        u.total_tokens = u.prompt_tokens + u.completion_tokens;
    }

    let u = usage.expect("usage should be set");
    assert_eq!(
        u.prompt_tokens, 150,
        "prompt_tokens should come from first chunk"
    );
    assert_eq!(
        u.completion_tokens, 75,
        "completion_tokens should come from second chunk"
    );
    assert_eq!(u.total_tokens, 225, "total should be sum of both");
}

/// `Usage::reasoning_tokens_effective` prefers the nested OpenAI shape
/// over the flat DeepSeek/xAI shape when both happen to be present;
/// falls back to the flat field when only it is set. (#768)
#[test]
fn test_usage_reasoning_tokens_effective_priority() {
    // Nested wins when set.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        reasoning_tokens: Some(5),
        completion_tokens_details: Some(crate::llm_types::CompletionTokensDetails {
            reasoning_tokens: Some(15),
        }),
        ..Usage::default()
    };
    assert_eq!(u.reasoning_tokens_effective(), Some(15));

    // Flat wins when nested is missing.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        reasoning_tokens: Some(7),
        ..Usage::default()
    };
    assert_eq!(u.reasoning_tokens_effective(), Some(7));

    // None when neither.
    let u = Usage {
        prompt_tokens: 10,
        completion_tokens: 20,
        total_tokens: 30,
        ..Usage::default()
    };
    assert!(u.reasoning_tokens_effective().is_none());
}

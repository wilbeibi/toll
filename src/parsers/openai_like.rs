use crate::record::Usage;
use serde_json::Value;

/// Covers OpenAI Chat Completions, Responses API, and OpenAI-compatible
/// providers: DeepSeek, OpenRouter, Groq, Together, Kimi, MiniMax, GLM, etc.
pub fn parse_openai(body: &Value) -> Usage {
    let Some(u) = body.get("usage").and_then(|v| v.as_object()) else {
        return Usage::default();
    };

    let input_tokens = u
        .get("input_tokens")
        .or_else(|| u.get("prompt_tokens"))
        .and_then(|v| v.as_u64());

    let mut output_tokens = u
        .get("output_tokens")
        .or_else(|| u.get("completion_tokens"))
        .and_then(|v| v.as_u64());

    let in_details = u
        .get("input_tokens_details")
        .or_else(|| u.get("prompt_tokens_details"))
        .and_then(|v| v.as_object());

    let mut cache_read = in_details
        .and_then(|d| d.get("cached_tokens"))
        .and_then(|v| v.as_u64());

    // DeepSeek extension.
    if cache_read.is_none() {
        cache_read = u.get("prompt_cache_hit_tokens").and_then(|v| v.as_u64());
    }
    // Kimi/Moonshot extension seen in official examples.
    if cache_read.is_none() {
        cache_read = u.get("cached_tokens").and_then(|v| v.as_u64());
    }

    // OpenRouter: explicit-caching models (Anthropic, Gemini) report tokens
    // written to cache alongside `cached_tokens`.
    let cache_creation = in_details
        .and_then(|d| d.get("cache_write_tokens"))
        .and_then(|v| v.as_u64());

    let out_details = u
        .get("output_tokens_details")
        .or_else(|| u.get("completion_tokens_details"))
        .and_then(|v| v.as_object());

    let reasoning = out_details
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());

    // xAI's chat API reports reasoning tokens *additively* (prompt +
    // completion + reasoning == total), unlike OpenAI/DeepSeek where
    // reasoning is a subset of completion, and unlike xAI's own Responses
    // endpoint which is subset-style. The total_tokens signature makes the
    // fold exact per response, not a per-provider guess.
    if let (Some(t), Some(i), Some(o), Some(r)) = (
        u.get("total_tokens").and_then(|v| v.as_u64()),
        input_tokens,
        output_tokens,
        reasoning,
    ) {
        if r > 0 && i + o + r == t {
            output_tokens = Some(o + r);
        }
    }

    // OpenRouter reports exact billed USD in `usage.cost`; xAI reports it
    // in `cost_in_usd_ticks` at 1e10 ticks/USD (verified 2026-07-18 against
    // models.dev rates on recorded grok-4.5 calls: /1e9 is 10x off).
    let cost = u.get("cost").and_then(|v| v.as_f64()).or_else(|| {
        u.get("cost_in_usd_ticks")
            .and_then(|v| v.as_u64())
            .map(|t| t as f64 / 1e10)
    });

    Usage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_creation,
        reasoning_output_tokens: reasoning,
        cost,
    }
}

/// OpenAI-compatible SSE: `usage` appears in the final chunk when
/// `stream_options.include_usage=true`. turnpike injects that option automatically.
/// Responses API streams (xAI, OpenAI /v1/responses) instead nest the final
/// usage inside the `response.completed` event's response object.
pub fn merge_openai_sse(_event_type: &str, data: &Value, into: &mut Usage) {
    if data.get("usage").is_some() {
        into.merge(&parse_openai(data));
    }
    if let Some(resp) = data.get("response") {
        if resp.get("usage").is_some() {
            into.merge(&parse_openai(resp));
        }
    }
    // Groq streams report usage in the final chunk under `x_groq.usage`.
    if let Some(xg) = data.get("x_groq") {
        if xg.get("usage").is_some() {
            into.merge(&parse_openai(xg));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_classic_field_names() {
        let u = parse_openai(&json!({
            "usage": { "prompt_tokens": 100, "completion_tokens": 50, "total_tokens": 150 }
        }));
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(50));
    }

    #[test]
    fn parse_cached_tokens_in_details() {
        let u = parse_openai(&json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "input_tokens_details": { "cached_tokens": 30 }
            }
        }));
        assert_eq!(u.cache_read_input_tokens, Some(30));
    }

    #[test]
    fn parse_deepseek_cache_extension() {
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "prompt_cache_hit_tokens": 40,
            }
        }));
        assert_eq!(u.cache_read_input_tokens, Some(40));
    }

    #[test]
    fn parse_kimi_top_level_cached_tokens() {
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "cached_tokens": 25,
            }
        }));
        assert_eq!(u.cache_read_input_tokens, Some(25));
    }

    #[test]
    fn xai_chat_additive_reasoning_folds_into_output() {
        // Verbatim shape from a recorded grok-4.5 chat call: 211+1+11 == 223.
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 211, "completion_tokens": 1, "total_tokens": 223,
                "prompt_tokens_details": {"cached_tokens": 128},
                "completion_tokens_details": {"reasoning_tokens": 11},
                "cost_in_usd_ticks": 2764000_u64
            }
        }));
        assert_eq!(u.output_tokens, Some(12));
        assert_eq!(u.reasoning_output_tokens, Some(11));
        assert!((u.cost.unwrap() - 0.0002764).abs() < 1e-9);
    }

    #[test]
    fn subset_reasoning_is_not_folded() {
        // OpenAI/DeepSeek style: total == prompt + completion; reasoning is
        // a subset of completion. xAI's Responses endpoint matches this too.
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 211, "completion_tokens": 12, "total_tokens": 223,
                "completion_tokens_details": {"reasoning_tokens": 11}
            }
        }));
        assert_eq!(u.output_tokens, Some(12));
    }

    #[test]
    fn sse_groq_x_groq_usage() {
        let mut u = Usage::default();
        merge_openai_sse(
            "",
            &json!({
                "choices": [],
                "x_groq": {"usage": {"prompt_tokens": 40, "completion_tokens": 7}}
            }),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(40));
        assert_eq!(u.output_tokens, Some(7));
    }

    #[test]
    fn sse_responses_api_completed_event() {
        let mut u = Usage::default();
        merge_openai_sse(
            "response.completed",
            &json!({
                "type": "response.completed",
                "response": {
                    "model": "grok-4.5",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 9,
                        "output_tokens_details": {"reasoning_tokens": 3}
                    }
                }
            }),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(9));
        assert_eq!(u.reasoning_output_tokens, Some(3));
    }

    #[test]
    fn parse_openrouter_cost() {
        let u = parse_openai(&json!({
            "usage": { "prompt_tokens": 17, "completion_tokens": 175, "cost": 0.000346775 }
        }));
        assert_eq!(u.cost, Some(0.000346775));
    }

    #[test]
    fn parse_openrouter_cache_write_tokens() {
        // Real shape from OpenRouter's always-on usage accounting.
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 1454,
                "completion_tokens": 3727,
                "cost": 0.01000692,
                "prompt_tokens_details": {
                    "audio_tokens": 0,
                    "cache_write_tokens": 1200,
                    "cached_tokens": 254,
                    "video_tokens": 0
                }
            }
        }));
        assert_eq!(u.cache_read_input_tokens, Some(254));
        assert_eq!(u.cache_creation_input_tokens, Some(1200));
    }

    #[test]
    fn parse_reasoning_tokens() {
        let u = parse_openai(&json!({
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 160,
                "output_tokens_details": { "reasoning_tokens": 100 }
            }
        }));
        assert_eq!(u.reasoning_output_tokens, Some(100));
    }

    #[test]
    fn sse_picks_up_final_usage_chunk() {
        let mut u = Usage::default();
        merge_openai_sse(
            "",
            &json!({"choices": [{"delta": {"content": "hi"}}]}),
            &mut u,
        );
        merge_openai_sse(
            "",
            &json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5}}),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(5));
    }
}

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

    let output_tokens = u
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

    let out_details = u
        .get("output_tokens_details")
        .or_else(|| u.get("completion_tokens_details"))
        .and_then(|v| v.as_object());

    let reasoning = out_details
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(|v| v.as_u64());

    Usage {
        input_tokens,
        output_tokens,
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: None,
        reasoning_output_tokens: reasoning,
        // OpenRouter reports the exact billed cost (USD) in `usage.cost`.
        cost: u.get("cost").and_then(|v| v.as_f64()),
    }
}

/// OpenAI-compatible SSE: `usage` appears in the final chunk when
/// `stream_options.include_usage=true`. toll injects that option automatically.
pub fn merge_openai_sse(event_type: &str, data: &Value, into: &mut Usage) {
    let _ = event_type;
    if data.get("usage").is_some() {
        into.merge(&parse_openai(data));
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
    fn parse_openrouter_cost() {
        let u = parse_openai(&json!({
            "usage": { "prompt_tokens": 17, "completion_tokens": 175, "cost": 0.000346775 }
        }));
        assert_eq!(u.cost, Some(0.000346775));
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

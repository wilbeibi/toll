use crate::record::Usage;
use serde_json::Value;

pub fn parse_gemini(body: &Value) -> Usage {
    let u = body.get("usageMetadata").and_then(|v| v.as_object());
    let candidates = u.and_then(|m| m.get("candidatesTokenCount")?.as_u64());
    // Gemini reports thinking tokens separately from candidatesTokenCount (additive,
    // not a subset). Add them to get the true output total, matching the semantics
    // of every other provider where reasoning_output_tokens ⊆ output_tokens.
    let thoughts = u.and_then(|m| m.get("thoughtsTokenCount")?.as_u64());
    let output_tokens = match (candidates, thoughts) {
        (Some(c), Some(t)) => Some(c + t),
        (c, t) => c.or(t),
    };
    Usage {
        input_tokens: u.and_then(|m| m.get("promptTokenCount")?.as_u64()),
        output_tokens,
        cache_read_input_tokens: u.and_then(|m| m.get("cachedContentTokenCount")?.as_u64()),
        reasoning_output_tokens: thoughts,
        ..Default::default()
    }
}

/// Gemini SSE (`?alt=sse`) emits `usageMetadata` in the final chunk.
pub fn merge_gemini_sse(_event_type: &str, data: &Value, into: &mut Usage) {
    if data.get("usageMetadata").is_some() {
        into.merge(&parse_gemini(data));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_with_cache() {
        let u = parse_gemini(&json!({
            "usageMetadata": {
                "promptTokenCount": 80,
                "candidatesTokenCount": 40,
                "totalTokenCount": 120,
                "cachedContentTokenCount": 20,
            }
        }));
        assert_eq!(u.output_tokens, Some(40));
        assert_eq!(u.cache_read_input_tokens, Some(20));
    }

    #[test]
    fn parse_thoughts_additive_to_output() {
        // Gemini sends candidatesTokenCount (non-thinking) and thoughtsTokenCount
        // as separate additive fields. output_tokens must be their sum so that
        // stats aggregation is correct; reasoning_output_tokens holds the breakdown.
        let u = parse_gemini(&json!({
            "usageMetadata": {
                "promptTokenCount": 100,
                "candidatesTokenCount": 60,
                "thoughtsTokenCount": 40,
            }
        }));
        assert_eq!(u.output_tokens, Some(100)); // 60 + 40
        assert_eq!(u.reasoning_output_tokens, Some(40));
    }

    #[test]
    fn sse_picks_up_final_chunk_with_metadata() {
        let mut u = Usage::default();
        merge_gemini_sse("", &json!({"candidates": []}), &mut u);
        merge_gemini_sse(
            "",
            &json!({
                "usageMetadata": {
                    "promptTokenCount": 10,
                    "candidatesTokenCount": 5,
                }
            }),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, Some(5));
    }
}

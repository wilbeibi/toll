use crate::record::Usage;
use serde_json::Value;

pub fn parse_gemini(body: &Value) -> Usage {
    let u = body.get("usageMetadata").and_then(|v| v.as_object());
    Usage {
        input_tokens: u.and_then(|m| m.get("promptTokenCount")?.as_u64()),
        output_tokens: u.and_then(|m| m.get("candidatesTokenCount")?.as_u64()),
        cache_read_input_tokens: u.and_then(|m| m.get("cachedContentTokenCount")?.as_u64()),
        reasoning_output_tokens: u.and_then(|m| m.get("thoughtsTokenCount")?.as_u64()),
        ..Default::default()
    }
}

/// Gemini SSE (`?alt=sse`) emits `usageMetadata` in the final chunk.
pub fn merge_gemini_sse(event_type: &str, data: &Value, into: &mut Usage) {
    let _ = event_type;
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
        assert_eq!(u.cache_read_input_tokens, Some(20));
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

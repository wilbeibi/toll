use crate::record::Usage;
use serde_json::Value;

pub fn parse_anthropic(body: &Value) -> Usage {
    let u = body.get("usage").and_then(|v| v.as_object());
    Usage {
        input_tokens: u.and_then(|m| m.get("input_tokens")?.as_u64()),
        output_tokens: u.and_then(|m| m.get("output_tokens")?.as_u64()),
        cache_read_input_tokens: u.and_then(|m| m.get("cache_read_input_tokens")?.as_u64()),
        cache_creation_input_tokens: u.and_then(|m| m.get("cache_creation_input_tokens")?.as_u64()),
        ..Default::default()
    }
}

/// Anthropic streams usage across multiple events:
/// - `message_start`:  initial input_tokens + cache fields
/// - `message_delta`:  final output_tokens
pub fn merge_anthropic_sse(event_type: &str, data: &Value, into: &mut Usage) {
    let u = if event_type == "message_start" {
        data.get("message")
            .and_then(|m| m.get("usage"))
            .and_then(|v| v.as_object())
    } else {
        data.get("usage").and_then(|v| v.as_object())
    };
    let Some(u) = u else { return };

    into.merge(&Usage {
        input_tokens: u.get("input_tokens").and_then(|v| v.as_u64()),
        output_tokens: u.get("output_tokens").and_then(|v| v.as_u64()),
        cache_read_input_tokens: u.get("cache_read_input_tokens").and_then(|v| v.as_u64()),
        cache_creation_input_tokens: u
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64()),
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_with_cache() {
        let u = parse_anthropic(&json!({
            "usage": {
                "input_tokens": 80,
                "output_tokens": 50,
                "cache_read_input_tokens": 20,
                "cache_creation_input_tokens": 5,
            }
        }));
        assert_eq!(u.cache_read_input_tokens, Some(20));
        assert_eq!(u.cache_creation_input_tokens, Some(5));
        assert_eq!(u.cache_hit(), Some(true));
    }

    #[test]
    fn sse_message_start_then_delta() {
        let mut u = Usage::default();

        merge_anthropic_sse(
            "message_start",
            &json!({
                "type": "message_start",
                "message": {
                    "usage": {
                        "input_tokens": 100,
                        "cache_read_input_tokens": 20,
                        "cache_creation_input_tokens": 0,
                    }
                }
            }),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, None);
        assert_eq!(u.cache_read_input_tokens, Some(20));

        merge_anthropic_sse(
            "message_delta",
            &json!({ "type": "message_delta", "usage": { "output_tokens": 50 } }),
            &mut u,
        );
        assert_eq!(u.input_tokens, Some(100)); // preserved from message_start
        assert_eq!(u.output_tokens, Some(50));
        assert_eq!(u.cache_read_input_tokens, Some(20)); // preserved
    }
}

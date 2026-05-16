mod anthropic;
mod gemini;
mod openai_like;

pub use anthropic::{merge_anthropic_sse, parse_anthropic};
pub use gemini::{merge_gemini_sse, parse_gemini};
pub use openai_like::{merge_openai_sse, parse_openai};

use serde_json::Value;

pub fn model_from_request_body(body: &[u8]) -> Option<String> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("model")?.as_str().map(String::from))
}

/// Best-effort model name from a provider *response* object (a streaming
/// chunk or full JSON body). Used as a fallback when the request body was
/// too large to inspect (see `should_inspect_body`). Covers OpenAI-like
/// (`model` on every chunk), Anthropic streaming (`message.model` in
/// `message_start`), and Gemini (`modelVersion`).
pub fn model_from_response_value(v: &Value) -> Option<String> {
    v.get("model")
        .or_else(|| v.get("message").and_then(|m| m.get("model")))
        .or_else(|| v.get("modelVersion"))
        .and_then(|m| m.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_model_field_from_json_body() {
        let body = br#"{"model":"claude-3-5-sonnet","messages":[]}"#;
        assert_eq!(
            model_from_request_body(body),
            Some("claude-3-5-sonnet".to_string())
        );
    }

    #[test]
    fn response_model_openai_chunk() {
        let v = serde_json::json!({"object": "chat.completion.chunk", "model": "deepseek-v4-pro"});
        assert_eq!(
            model_from_response_value(&v),
            Some("deepseek-v4-pro".to_string())
        );
    }

    #[test]
    fn response_model_anthropic_message_start() {
        let v = serde_json::json!({"type": "message_start", "message": {"model": "claude-opus-4"}});
        assert_eq!(
            model_from_response_value(&v),
            Some("claude-opus-4".to_string())
        );
    }

    #[test]
    fn response_model_gemini_version() {
        let v = serde_json::json!({"modelVersion": "gemini-2.0-flash", "usageMetadata": {}});
        assert_eq!(
            model_from_response_value(&v),
            Some("gemini-2.0-flash".to_string())
        );
    }

    #[test]
    fn response_model_absent_or_empty() {
        assert_eq!(model_from_response_value(&serde_json::json!({})), None);
        assert_eq!(
            model_from_response_value(&serde_json::json!({"model": ""})),
            None
        );
    }
}

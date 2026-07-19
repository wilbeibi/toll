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

/// The verbatim usage sub-object carried by a response object (streaming chunk
/// or full body), across provider shapes — for audit storage in `raw_usage`,
/// preserving fields the typed parsers normalize away. Mirrors the locations
/// the `merge_*` functions read from: top-level `usage`/`usageMetadata`,
/// Anthropic's `message.usage`, the Responses API's `response.usage`, and
/// Groq's `x_groq.usage`. `None` when the object carries no usage.
pub fn raw_usage_value(data: &Value) -> Option<Value> {
    data.get("usage")
        .or_else(|| data.get("usageMetadata"))
        .or_else(|| data.get("message").and_then(|m| m.get("usage")))
        .or_else(|| data.get("response").and_then(|r| r.get("usage")))
        .or_else(|| data.get("x_groq").and_then(|x| x.get("usage")))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn raw_usage_extracts_verbatim_across_shapes() {
        // Preserves fields the typed parser drops (e.g. `num_sources_used`).
        let openai = serde_json::json!({"usage": {"prompt_tokens": 10, "num_sources_used": 3}});
        assert_eq!(
            raw_usage_value(&openai),
            Some(serde_json::json!({"prompt_tokens": 10, "num_sources_used": 3}))
        );
        // Anthropic message_start nests usage under `message`.
        let start =
            serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 5}}});
        assert_eq!(
            raw_usage_value(&start),
            Some(serde_json::json!({"input_tokens": 5}))
        );
        // Groq's final chunk carries usage under `x_groq`.
        let groq =
            serde_json::json!({"choices": [], "x_groq": {"usage": {"completion_tokens": 7}}});
        assert_eq!(
            raw_usage_value(&groq),
            Some(serde_json::json!({"completion_tokens": 7}))
        );
        // No usage present.
        assert_eq!(raw_usage_value(&serde_json::json!({"choices": []})), None);
    }
}

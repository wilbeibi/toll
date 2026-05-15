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
}

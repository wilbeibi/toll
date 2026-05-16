use crate::parsers::{
    merge_anthropic_sse, merge_gemini_sse, merge_openai_sse, parse_anthropic, parse_gemini,
    parse_openai,
};
use crate::record::{TotalTokenSemantics, Usage};
use serde_json::Value;

pub type ParseJson = fn(&Value) -> Usage;
pub type MergeSse = fn(&str, &Value, &mut Usage);

#[allow(dead_code)]
pub struct Provider {
    pub name: &'static str,
    pub upstream_url: &'static str,
    pub default_port: u16,
    pub host_suffixes: &'static [&'static str],
    pub parse_json: ParseJson,
    /// Top-level non-streaming response field that carries usage accounting.
    pub json_usage_key: &'static str,
    pub merge_sse: MergeSse,
    /// How to derive `total_tokens` when the provider does not report one.
    pub total_token_semantics: TotalTokenSemantics,
    /// If Some, model is extracted from the request path instead of body.
    pub model_from_path: Option<fn(&str) -> Option<String>>,
    /// Shell export template. `{port}` is substituted at print time.
    pub env_template: Option<&'static str>,
    /// Inject `stream_options: {include_usage: true}` into streaming requests so
    /// the final SSE chunk carries token counts. True for OpenAI-compatible APIs;
    /// false for Anthropic (reports via message_start/delta) and Gemini.
    pub inject_stream_options: bool,
}

pub fn gemini_model_from_path(path: &str) -> Option<String> {
    let idx = path.find("/models/")?;
    let rest = &path[idx + "/models/".len()..];
    let end = rest.find([':', '/', '?']).unwrap_or(rest.len());
    if end == 0 {
        None
    } else {
        Some(rest[..end].to_string())
    }
}

pub static PROVIDERS: &[Provider] = &[
    Provider {
        name: "anthropic",
        upstream_url: "https://api.anthropic.com",
        default_port: 4001,
        host_suffixes: &["anthropic.com"],
        parse_json: parse_anthropic,
        json_usage_key: "usage",
        merge_sse: merge_anthropic_sse,
        total_token_semantics: TotalTokenSemantics::CacheAdditiveToInput,
        model_from_path: None,
        env_template: Some("export ANTHROPIC_BASE_URL=http://127.0.0.1:{port}"),
        inject_stream_options: false,
    },
    Provider {
        name: "openai",
        upstream_url: "https://api.openai.com",
        default_port: 4000,
        host_suffixes: &["openai.com"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1"),
        inject_stream_options: true,
    },
    Provider {
        name: "deepseek",
        upstream_url: "https://api.deepseek.com",
        default_port: 4003,
        host_suffixes: &["deepseek.com"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1"),
        inject_stream_options: true,
    },
    Provider {
        name: "openrouter",
        upstream_url: "https://openrouter.ai",
        default_port: 4004,
        host_suffixes: &["openrouter.ai"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/api/v1"),
        inject_stream_options: true,
    },
    Provider {
        name: "gemini",
        upstream_url: "https://generativelanguage.googleapis.com",
        default_port: 4002,
        host_suffixes: &["generativelanguage.googleapis.com"],
        parse_json: parse_gemini,
        json_usage_key: "usageMetadata",
        merge_sse: merge_gemini_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: Some(gemini_model_from_path),
        env_template: None,
        inject_stream_options: false,
    },
    Provider {
        name: "kimi",
        upstream_url: "https://api.moonshot.ai",
        default_port: 4005,
        host_suffixes: &["api.moonshot.ai", "api.moonshot.cn"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1"),
        inject_stream_options: true,
    },
    Provider {
        name: "minimax",
        upstream_url: "https://api.minimaxi.com",
        default_port: 4006,
        host_suffixes: &["api.minimaxi.com", "api.minimax.io"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1"),
        inject_stream_options: true,
    },
    Provider {
        name: "glm",
        upstream_url: "https://open.bigmodel.cn",
        default_port: 4007,
        host_suffixes: &["open.bigmodel.cn"],
        parse_json: parse_openai,
        json_usage_key: "usage",
        merge_sse: merge_openai_sse,
        total_token_semantics: TotalTokenSemantics::CacheIncludedInInput,
        model_from_path: None,
        env_template: Some("export OPENAI_BASE_URL=http://127.0.0.1:{port}/api/paas/v4"),
        inject_stream_options: true,
    },
];

#[allow(dead_code)]
pub fn find_by_name(name: &str) -> Option<&'static Provider> {
    PROVIDERS.iter().find(|p| p.name == name)
}

#[allow(dead_code)]
pub fn find_by_host(host: &str) -> Option<&'static Provider> {
    let h = host.to_lowercase();
    let h = h.split(':').next().unwrap_or(&h); // strip port
    PROVIDERS.iter().find(|p| {
        p.host_suffixes
            .iter()
            .any(|sfx| h == *sfx || h.ends_with(&format!(".{sfx}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_model_standard_path() {
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-1.5-pro:generateContent"),
            Some("gemini-1.5-pro".to_string())
        );
    }

    #[test]
    fn gemini_model_path_with_query() {
        // `?` must also terminate the model name
        assert_eq!(
            gemini_model_from_path("/v1beta/models/gemini-2.0-flash:streamGenerateContent?alt=sse"),
            Some("gemini-2.0-flash".to_string())
        );
    }

    #[test]
    fn gemini_model_path_trailing_slash_returns_none() {
        assert_eq!(gemini_model_from_path("/v1beta/models/"), None);
    }

    #[test]
    fn find_by_host_strips_port() {
        // Port suffix must not prevent matching
        assert_eq!(
            find_by_host("api.openai.com:443").map(|p| p.name),
            Some("openai")
        );
    }
}

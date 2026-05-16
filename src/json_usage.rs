use serde_json::{Map, Value};

const MAX_USAGE_OBJECT_BYTES: usize = 64 * 1024;
const MAX_KEY_BYTES: usize = 128;
const MAX_MODEL_BYTES: usize = 256;

/// Incrementally extracts one top-level provider usage object from a JSON response.
///
/// This is intentionally narrower than a general JSON parser: it only watches
/// root object keys like `usage` or `usageMetadata`, ignores nested content, and
/// stores at most the small matched usage object.
pub struct JsonUsageExtractor {
    target_key: &'static str,
    depth: usize,
    in_string: bool,
    escape: bool,
    capture_top_string: bool,
    key_buf: Vec<u8>,
    last_top_string: Option<String>,
    expecting_value: bool,
    expecting_model_value: bool,
    collecting_model: bool,
    model_escape: bool,
    skipping_model: bool,
    skip_model_escape: bool,
    model_buf: Vec<u8>,
    model: Option<String>,
    collecting: bool,
    collect_depth: usize,
    collect_in_string: bool,
    collect_escape: bool,
    collect_buf: Vec<u8>,
    extracted: Option<Value>,
    disabled: bool,
}

impl JsonUsageExtractor {
    pub fn new(target_key: &'static str) -> Self {
        Self {
            target_key,
            depth: 0,
            in_string: false,
            escape: false,
            capture_top_string: false,
            key_buf: Vec::new(),
            last_top_string: None,
            expecting_value: false,
            expecting_model_value: false,
            collecting_model: false,
            model_escape: false,
            skipping_model: false,
            skip_model_escape: false,
            model_buf: Vec::new(),
            model: None,
            collecting: false,
            collect_depth: 0,
            collect_in_string: false,
            collect_escape: false,
            collect_buf: Vec::new(),
            extracted: None,
            disabled: false,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if self.extracted.is_some() || self.disabled {
            return;
        }

        for &b in bytes {
            if self.collecting {
                self.feed_collect(b);
            } else if self.collecting_model {
                self.feed_model_string(b);
            } else if self.skipping_model {
                self.feed_skip_model_string(b);
            } else if self.expecting_model_value {
                self.feed_expected_model_value(b);
            } else if self.expecting_value {
                self.feed_expected_value(b);
            } else {
                self.feed_scan(b);
            }

            if self.extracted.is_some() || self.disabled {
                return;
            }
        }
    }

    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    pub fn finish_wrapped(self) -> Option<Value> {
        let usage = self.extracted?;
        let mut wrapper = Map::new();
        wrapper.insert(self.target_key.to_string(), usage);
        Some(Value::Object(wrapper))
    }

    fn feed_scan(&mut self, b: u8) {
        if self.in_string {
            self.feed_scan_string(b);
            return;
        }

        match b {
            b'"' => {
                self.in_string = true;
                self.escape = false;
                self.capture_top_string = self.depth == 1;
                self.key_buf.clear();
            }
            b'{' | b'[' => {
                self.depth += 1;
                self.last_top_string = None;
            }
            b'}' | b']' => {
                self.depth = self.depth.saturating_sub(1);
                self.last_top_string = None;
            }
            b':' if self.depth == 1 => {
                match self.last_top_string.as_deref() {
                    Some(key) if key == self.target_key => self.expecting_value = true,
                    Some("model" | "modelVersion") if self.model.is_none() => {
                        self.expecting_model_value = true;
                    }
                    _ => {}
                }
                self.last_top_string = None;
            }
            b',' if self.depth == 1 => {
                self.last_top_string = None;
            }
            _ => {}
        }
    }

    fn feed_scan_string(&mut self, b: u8) {
        if self.escape {
            self.escape = false;
            if self.capture_top_string && self.key_buf.len() < MAX_KEY_BYTES {
                self.key_buf.push(b);
            }
            return;
        }

        match b {
            b'\\' => {
                self.escape = true;
            }
            b'"' => {
                self.in_string = false;
                if self.capture_top_string {
                    self.last_top_string = String::from_utf8(self.key_buf.clone()).ok();
                }
                self.capture_top_string = false;
                self.key_buf.clear();
            }
            _ => {
                if self.capture_top_string && self.key_buf.len() < MAX_KEY_BYTES {
                    self.key_buf.push(b);
                }
            }
        }
    }

    fn feed_expected_value(&mut self, b: u8) {
        if b.is_ascii_whitespace() {
            return;
        }

        self.expecting_value = false;
        if b == b'{' {
            self.collecting = true;
            self.collect_depth = 0;
            self.collect_in_string = false;
            self.collect_escape = false;
            self.collect_buf.clear();
            self.feed_collect(b);
        }
    }

    fn feed_expected_model_value(&mut self, b: u8) {
        if b.is_ascii_whitespace() {
            return;
        }

        self.expecting_model_value = false;
        if b == b'"' {
            self.collecting_model = true;
            self.model_escape = false;
            self.model_buf.clear();
            self.model_buf.push(b);
        } else {
            self.feed_scan(b);
        }
    }

    fn feed_model_string(&mut self, b: u8) {
        if self.model_buf.len() >= MAX_MODEL_BYTES {
            self.collecting_model = false;
            self.skipping_model = true;
            self.skip_model_escape = false;
            self.model_buf.clear();
            self.feed_skip_model_string(b);
            return;
        }
        self.model_buf.push(b);

        if self.model_escape {
            self.model_escape = false;
            return;
        }

        match b {
            b'\\' => self.model_escape = true,
            b'"' => {
                self.collecting_model = false;
                self.model = serde_json::from_slice::<String>(&self.model_buf)
                    .ok()
                    .filter(|s| !s.is_empty());
                self.model_buf.clear();
            }
            _ => {}
        }
    }

    fn feed_skip_model_string(&mut self, b: u8) {
        if self.skip_model_escape {
            self.skip_model_escape = false;
            return;
        }

        match b {
            b'\\' => self.skip_model_escape = true,
            b'"' => self.skipping_model = false,
            _ => {}
        }
    }

    fn feed_collect(&mut self, b: u8) {
        if self.collect_buf.len() >= MAX_USAGE_OBJECT_BYTES {
            self.disabled = true;
            self.collecting = false;
            self.collect_buf.clear();
            return;
        }
        self.collect_buf.push(b);

        if self.collect_in_string {
            if self.collect_escape {
                self.collect_escape = false;
            } else if b == b'\\' {
                self.collect_escape = true;
            } else if b == b'"' {
                self.collect_in_string = false;
            }
            return;
        }

        match b {
            b'"' => self.collect_in_string = true,
            b'{' | b'[' => self.collect_depth += 1,
            b'}' | b']' => {
                self.collect_depth = self.collect_depth.saturating_sub(1);
                if self.collect_depth == 0 {
                    self.collecting = false;
                    self.extracted = serde_json::from_slice::<Value>(&self.collect_buf).ok();
                    self.collect_buf.clear();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_top_level_usage_across_chunks() {
        let mut ex = JsonUsageExtractor::new("usage");
        ex.push(br#"{"id":"x","choices":[{"message":{"content":"hi"}}"#);
        ex.push(br#"],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#);
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usage"]["prompt_tokens"], 10);
        assert_eq!(wrapped["usage"]["completion_tokens"], 5);
    }

    #[test]
    fn ignores_usage_inside_content_string() {
        let mut ex = JsonUsageExtractor::new("usage");
        ex.push(br#"{"choices":[{"message":{"content":"{\"usage\":{\"prompt_tokens\":999}}"}}],"#);
        ex.push(br#""usage":{"prompt_tokens":10}}"#);
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn ignores_nested_usage_objects() {
        let mut ex = JsonUsageExtractor::new("usage");
        ex.push(br#"{"choices":[{"usage":{"prompt_tokens":999}}],"usage":{"prompt_tokens":10}}"#);
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn extracts_gemini_usage_metadata() {
        let mut ex = JsonUsageExtractor::new("usageMetadata");
        ex.push(
            br#"{"candidates":[],"usageMetadata":{"promptTokenCount":10,"totalTokenCount":15}}"#,
        );
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usageMetadata"]["promptTokenCount"], 10);
    }

    #[test]
    fn extracts_top_level_model_across_chunks() {
        let mut ex = JsonUsageExtractor::new("usage");
        ex.push(br#"{"id":"x","model":"gpt-"#);
        ex.push(br#"4o","usage":{"prompt_tokens":10}}"#);
        assert_eq!(ex.model(), Some("gpt-4o"));
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usage"]["prompt_tokens"], 10);
    }

    #[test]
    fn ignores_nested_or_content_model_strings() {
        let mut ex = JsonUsageExtractor::new("usage");
        ex.push(
            br#"{"choices":[{"message":{"content":"{\"model\":\"fake\"}","model":"nested"}}],"usage":{"prompt_tokens":10}}"#,
        );
        assert_eq!(ex.model(), None);
    }

    #[test]
    fn extracts_gemini_model_version() {
        let mut ex = JsonUsageExtractor::new("usageMetadata");
        ex.push(br#"{"modelVersion":"gemini-2.0-flash","usageMetadata":{"totalTokenCount":15}}"#);
        assert_eq!(ex.model(), Some("gemini-2.0-flash"));
    }

    #[test]
    fn oversized_model_does_not_block_usage_extraction() {
        let mut ex = JsonUsageExtractor::new("usage");
        let body = format!(
            r#"{{"model":"{}","usage":{{"prompt_tokens":10}}}}"#,
            "x".repeat(MAX_MODEL_BYTES + 10)
        );
        ex.push(body.as_bytes());
        assert_eq!(ex.model(), None);
        let wrapped = ex.finish_wrapped().unwrap();
        assert_eq!(wrapped["usage"]["prompt_tokens"], 10);
    }
}

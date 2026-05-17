use serde_json::{Map, Value};

const MAX_USAGE_OBJECT_BYTES: usize = 64 * 1024;
const MAX_KEY_BYTES: usize = 128;

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
                if self.last_top_string.as_deref() == Some(self.target_key) {
                    self.expecting_value = true;
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
}

//! One best identity per row, read-time only.
//!
//! turnpike records attribution in two separate provenances (`client` —
//! declared via `x-turnpike-client` or the `User-Agent` fallback — and
//! `peer_exe` — the observed calling process). A unified "tool" view ranks
//! them: declared beats observed, and an observed process beats a bare
//! runtime User-Agent like `node`. Nothing here is inferred from text
//! shape; the ranking uses recorded provenance only.

/// The precedence chain for a row's best tool identity:
///
/// - `client_source == "header"` → `client` (declared)
/// - `client_source == "ua"` → `peer_exe`, else `client` (observed beats
///   a runtime UA)
/// - `client_source == NULL` (legacy rows) → `client`, else `peer_exe`
///   (the historical COALESCE order)
/// - all absent → `None`; callers render "unknown"
///
/// Borrowed from its inputs — a pure selector, no allocation; callers
/// own a copy only where they need one (grouping keys).
pub fn unified_tool<'a>(
    client: Option<&'a str>,
    client_source: Option<&str>,
    peer_exe: Option<&'a str>,
) -> Option<&'a str> {
    match client_source {
        Some("header") => client,
        Some("ua") => peer_exe.or(client),
        _ => client.or(peer_exe),
    }
}

/// Basename for path-shaped keys (`peer_exe` values are absolute paths),
/// unchanged otherwise. Display only — grouping always uses the full key,
/// so two venvs' pythons stay distinct buckets while both read "python".
pub fn display_tool(key: &str) -> String {
    if key.starts_with('/') {
        std::path::Path::new(key)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| key.to_string())
    } else {
        key.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_provenance_picks_declared_client() {
        assert_eq!(
            unified_tool(Some("opencode"), Some("header"), Some("/usr/bin/node")),
            Some("opencode")
        );
    }

    #[test]
    fn ua_provenance_prefers_observed_process() {
        // A bare runtime UA like "node" loses to the resolved executable.
        assert_eq!(
            unified_tool(Some("node"), Some("ua"), Some("/usr/bin/python")),
            Some("/usr/bin/python")
        );
    }

    #[test]
    fn ua_provenance_falls_back_to_client_without_exe() {
        assert_eq!(
            unified_tool(Some("curl/8.5.0"), Some("ua"), None),
            Some("curl/8.5.0")
        );
    }

    #[test]
    fn legacy_rows_prefer_client_over_process() {
        assert_eq!(
            unified_tool(Some("agent-x"), None, Some("/usr/bin/x")),
            Some("agent-x")
        );
    }

    #[test]
    fn legacy_rows_use_process_without_client() {
        assert_eq!(
            unified_tool(None, None, Some("/usr/bin/x")),
            Some("/usr/bin/x")
        );
    }

    #[test]
    fn nothing_recorded_is_none_not_unknown() {
        assert_eq!(unified_tool(None, None, None), None);
    }

    #[test]
    fn display_basenames_paths_only() {
        assert_eq!(display_tool("/usr/bin/node"), "node");
        assert_eq!(display_tool("/home/u/.venv/bin/python"), "python");
        // Non-path keys (headers/UAs) keep their whole value; "opencode/1.0"
        // is a version, not a directory.
        assert_eq!(display_tool("opencode/1.0"), "opencode/1.0");
        assert_eq!(display_tool("/"), "/");
    }
}

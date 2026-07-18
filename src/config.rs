use crate::providers::{Provider, PROVIDERS};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFormat {
    Shell,
    Fish,
    Json,
}

pub fn run(format: ConfigFormat, provider: Option<&str>) -> Result<()> {
    let providers = select_providers(provider)?;
    match format {
        ConfigFormat::Shell => print_exports(&providers, provider.is_some(), shell_line),
        ConfigFormat::Fish => print_exports(&providers, provider.is_some(), fish_line),
        ConfigFormat::Json => print_json(&providers),
    }
    Ok(())
}

fn select_providers(provider: Option<&str>) -> Result<Vec<&'static Provider>> {
    match provider {
        Some(name) => PROVIDERS
            .iter()
            .find(|p| p.name == name)
            .map(|p| vec![p])
            .ok_or_else(|| anyhow::anyhow!("unknown provider {name:?}")),
        None => Ok(PROVIDERS.iter().collect()),
    }
}

fn print_exports(
    providers: &[&Provider],
    single_provider: bool,
    render: fn(&str, u16) -> Option<String>,
) {
    if !single_provider {
        println!("# Multiple providers share OPENAI_BASE_URL.");
        println!("# For pipeable shell output, use: toll config --provider <name>");
    }
    for p in providers {
        if let Some(tmpl) = p.env_template {
            let Some(line) = render(tmpl, p.default_port) else {
                continue;
            };
            if single_provider || p.name == "anthropic" {
                println!("{line}");
            } else {
                println!("# {line}");
            }
        }
    }
}

fn shell_line(template: &str, port: u16) -> Option<String> {
    Some(template.replace("{port}", &port.to_string()))
}

/// Templates are `export NAME=VALUE`; fish spells that `set -gx NAME VALUE`.
fn fish_line(template: &str, port: u16) -> Option<String> {
    let line = template.replace("{port}", &port.to_string());
    let body = line.strip_prefix("export ")?;
    let (name, value) = body.split_once('=')?;
    Some(format!("set -gx {name} {value}"))
}

fn print_json(providers: &[&Provider]) {
    let map: serde_json::Map<String, serde_json::Value> = providers
        .iter()
        .map(|p| {
            let base_url = p
                .env_template
                .map(|t| extract_url(t, p.default_port))
                .unwrap_or_else(|| format!("http://127.0.0.1:{}", p.default_port));
            (
                p.name.to_string(),
                serde_json::json!({"base_url": base_url}),
            )
        })
        .collect();
    let out = serde_json::to_string_pretty(&serde_json::json!({"providers": map}))
        .expect("serializing known-valid JSON structure");
    println!("{out}");
}

fn extract_url(template: &str, port: u16) -> String {
    // Templates look like `export FOO=http://127.0.0.1:{port}/path`
    let value = template
        .split_once('=')
        .map(|x| x.1)
        .unwrap_or(template)
        .trim_matches('"');
    value.replace("{port}", &port.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_line_translates_export() {
        assert_eq!(
            fish_line("export OPENAI_BASE_URL=http://127.0.0.1:{port}/v1", 4008).as_deref(),
            Some("set -gx OPENAI_BASE_URL http://127.0.0.1:4008/v1")
        );
    }

    #[test]
    fn fish_line_rejects_non_export_templates() {
        assert_eq!(fish_line("FOO=bar", 1), None);
    }
}

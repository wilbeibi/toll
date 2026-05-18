use crate::providers::{Provider, PROVIDERS};
use anyhow::Result;

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigFormat {
    Shell,
    Json,
}

pub fn run(format: ConfigFormat, provider: Option<&str>) -> Result<()> {
    let providers = select_providers(provider)?;
    match format {
        ConfigFormat::Shell => print_shell(&providers, provider.is_some()),
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

fn print_shell(providers: &[&Provider], single_provider: bool) {
    if !single_provider {
        println!("# Multiple providers share OPENAI_BASE_URL.");
        println!("# For pipeable shell output, use: toll config --provider <name>");
    }
    for p in providers {
        if let Some(tmpl) = p.env_template {
            let line = tmpl.replace("{port}", &p.default_port.to_string());
            if single_provider || p.name == "anthropic" {
                println!("{line}");
            } else {
                println!("# {line}");
            }
        }
    }
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

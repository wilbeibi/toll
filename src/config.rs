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
    let mut entries = Vec::new();
    for p in providers {
        let base_url = p
            .env_template
            .map(|t| extract_url(t, p.default_port))
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", p.default_port));
        entries.push(format!(
            "    \"{}\": {{\"base_url\": \"{}\"}}",
            p.name, base_url
        ));
    }
    println!("{{");
    println!("  \"providers\": {{");
    println!("{}", entries.join(",\n"));
    println!("  }}");
    println!("}}");
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

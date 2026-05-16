mod cli;
mod config;
mod json_usage;
mod parsers;
mod paths;
mod pricing;
mod providers;
mod proxy;
mod record;
mod sse;
mod stats;
mod tail;

use clap::Parser;
use cli::{Cli, Command};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let cli = Cli::parse();
    match cli.command {
        Command::Start => proxy::run_all().await?,
        Command::Stats { by_model } => stats::run(stats::StatsOpts { by_model })?,
        Command::Tail { n } => tail::run(n)?,
        Command::Config { format, provider } => config::run(
            match format {
                cli::Format::Shell => config::ConfigFormat::Shell,
                cli::Format::Json => config::ConfigFormat::Json,
            },
            provider.as_deref(),
        )?,
        Command::Prices { cmd } => match cmd {
            cli::PricesCmd::Pull => pricing::pull(&paths::prices_json()).await?,
            cli::PricesCmd::Show => pricing::show(&paths::prices_json()),
        },
    }

    Ok(())
}

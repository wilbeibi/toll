use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "toll", version, about = "Personal LLM API usage meter")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the reverse proxy listeners for all providers.
    Start,

    /// Print usage statistics from calls.jsonl.
    Stats {
        /// Group by model instead of provider.
        #[arg(long)]
        by_model: bool,
    },

    /// Pretty-print the last N calls.
    Tail {
        /// Number of records to show.
        #[arg(short, long, default_value = "20")]
        n: usize,
    },

    /// Print configuration snippets for pointing clients at toll.
    Config {
        /// Limit output to one provider.
        #[arg(short, long)]
        provider: Option<String>,

        #[arg(long, value_enum, default_value = "shell")]
        format: Format,
    },
}

#[derive(ValueEnum, Clone)]
pub enum Format {
    Shell,
    Json,
}

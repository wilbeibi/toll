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

    /// Print usage statistics from the call database.
    Stats {
        /// Group by model instead of provider.
        #[arg(long)]
        by_model: bool,

        /// Group by calling client (x-toll-client / User-Agent).
        #[arg(long, conflicts_with = "by_model")]
        by_client: bool,

        /// Group by calendar day (UTC).
        #[arg(long, conflicts_with_all = ["by_model", "by_client"])]
        by_day: bool,

        /// Group by resolved calling process (peer_exe; Linux-only, else every
        /// row is unknown).
        #[arg(long, conflicts_with_all = ["by_model", "by_client", "by_day"])]
        by_exe: bool,

        /// Only include calls at or after this point: 30m, 12h, 7d, today,
        /// a date (2026-07-01), or an RFC-3339 instant.
        #[arg(long)]
        since: Option<String>,

        /// Emit a JSON array (computed costs included) instead of a table.
        #[arg(long)]
        json: bool,
    },

    /// Pretty-print the last N calls.
    Tail {
        /// Number of records to show.
        #[arg(short, long, default_value = "20")]
        n: usize,

        /// Only include calls at or after this point (same forms as stats).
        #[arg(long)]
        since: Option<String>,

        /// Emit one JSON object per line (computed cost + source included).
        #[arg(long)]
        json: bool,
    },

    /// Print configuration snippets for pointing clients at toll.
    Config {
        /// Limit output to one provider.
        #[arg(short, long)]
        provider: Option<String>,

        #[arg(long, value_enum, default_value = "shell")]
        format: Format,
    },

    /// Manage the local pricing table.
    Prices {
        #[command(subcommand)]
        cmd: PricesCmd,
    },
}

#[derive(Subcommand)]
pub enum PricesCmd {
    /// Fetch latest prices from models.dev and save to the local data directory.
    Pull,
    /// Show which price table is active and how many models it covers.
    Show,
}

#[derive(ValueEnum, Clone)]
pub enum Format {
    Shell,
    Fish,
    Json,
    /// Bare base URLs in the memorable `http://<provider>.localhost:4000` form.
    Url,
}

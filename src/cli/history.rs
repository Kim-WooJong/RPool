use clap::{Args, Subcommand};

#[derive(Args, Debug)]
pub(crate) struct HistoryArgs {
    #[command(subcommand)]
    pub(crate) command: HistoryCommands,
}

#[derive(Subcommand, Debug)]
pub(crate) enum HistoryCommands {
    /// Show recent task history.
    List {
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },

    /// Keep only the newest N history records.
    Prune {
        #[arg(long, default_value_t = 500)]
        keep: usize,
    },
}

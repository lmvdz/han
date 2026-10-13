//! `han tui`: the terminal face. It is a thin wrapper; the face itself is the `han-tui` crate.

use std::path::PathBuf;

use clap::Args;

/// Options of `han tui`.
#[derive(Args)]
pub struct TuiArgs {
    /// The space to show (else the one shown last time; `:space <id>` switches inside the TUI).
    space: Option<String>,
    /// The data directory of the running `han node` (names its pipe, and holds the TUI's `tui.json`).
    #[arg(long, default_value = "han-data")]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
}

pub async fn run(args: TuiArgs) -> Result<(), Box<dyn std::error::Error>> {
    han_tui::run(han_tui::Options {
        space: args.space,
        data: args.data,
        endpoint: args.endpoint,
    })
    .await?;
    Ok(())
}

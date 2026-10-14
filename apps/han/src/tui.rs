//! `han tui`: the terminal face. It is a thin wrapper; the face itself is the `han-tui` crate.

use std::path::PathBuf;

use clap::Args;

/// Options of `han tui`.
#[derive(Args)]
pub struct TuiArgs {
    /// The space to show (else the one shown last time; `:space <id>` switches inside the TUI).
    space: Option<String>,
    /// The data directory of the running `han node` (names its pipe, and holds the TUI's `tui.json`).
    #[arg(long, env = "HAN_DATA", default_value = "han-data")]
    data: PathBuf,
    /// The node's endpoint, if it is not the default of `--data`.
    #[arg(long)]
    endpoint: Option<String>,
    /// Print the screen once as text and exit, instead of running interactively: `COLSxROWS`, for example
    /// `120x30`.
    #[arg(long, value_name = "COLSxROWS")]
    print: Option<String>,
    /// With `--print`: seconds to let the node answer before printing.
    #[arg(long, default_value_t = 3)]
    wait: u64,
}

pub async fn run(args: TuiArgs) -> Result<(), Box<dyn std::error::Error>> {
    let opts = han_tui::Options {
        space: args.space,
        data: args.data,
        endpoint: args.endpoint,
    };
    if let Some(size) = args.print {
        let (w, h) = size
            .split_once(['x', 'X'])
            .and_then(|(w, h)| Some((w.parse::<u16>().ok()?, h.parse::<u16>().ok()?)))
            .filter(|(w, h)| *w >= 20 && *h >= 5)
            .ok_or("--print: want COLSxROWS, at least 20x5")?;
        let screen =
            han_tui::print(opts, (w, h), std::time::Duration::from_secs(args.wait)).await?;
        println!("{screen}");
        return Ok(());
    }
    han_tui::run(opts).await?;
    Ok(())
}

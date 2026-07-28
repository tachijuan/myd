use anyhow::Result;
use clap::Parser;
use myd::app::FileBrowser;
use myd::cli::{Cli, Startup};

#[tokio::main]
async fn main() -> Result<()> {
    // Diagnostics, when asked for via MYD_LOG / MYD_TRACE. Must happen here:
    // without it no subscriber is installed and every tracing call in the app is
    // a no-op, so `MYD_LOG=...` silently produced nothing. Output goes to a file
    // (see `trace::trace_path`) because the TUI owns the terminal.
    myd::trace::init();

    let cli = Cli::parse();

    let mut browser = match cli.startup(std::env::current_dir().ok()) {
        // Asked to be shown the picker, so nothing is opened until a destination
        // is chosen. Clap rejects a path alongside the flag, so there is no
        // argument being ignored here.
        Startup::Picker => FileBrowser::new_on_picker(),
        Startup::Local { left, right, dual } => FileBrowser::new(left, right, dual),
        Startup::Remote {
            target,
            panel,
            local,
            dual,
        } => {
            // The local side always occupies the *other* pane, so the remote has
            // somewhere to sit without displacing it.
            let (left, right) = if panel == 0 {
                (None, local)
            } else {
                (local, None)
            };
            let mut b = FileBrowser::new(left, right, dual);
            b.connect_on_start_in_panel(&target, panel);
            b
        }
    };
    browser.run().await
}

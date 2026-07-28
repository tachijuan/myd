use anyhow::Result;
use clap::Parser;
use myd::app::FileBrowser;
use myd::cli::{Cli, Startup};

fn main() -> Result<()> {
    // An explicit runtime rather than `#[tokio::main]`, so the process can stop
    // without waiting on background tasks. An SFTP connection keeps a russh
    // session task running for as long as the backend is registered, and nothing
    // unregisters it — quitting after browsing a remote host therefore hung on a
    // task that never finishes. `shutdown_background` drops the runtime without
    // joining, which is safe here because the UI has already exited and any
    // in-flight transfer is being abandoned deliberately.
    let runtime = tokio::runtime::Runtime::new()?;
    let result = runtime.block_on(run());
    runtime.shutdown_background();
    result
}

async fn run() -> Result<()> {
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
        Startup::Local {
            left,
            right,
            dual,
            shallow,
        } => FileBrowser::new_shallow(left, right, dual, shallow),
        Startup::Remote {
            target,
            panel,
            local,
            dual,
            shallow,
        } => {
            // The local side always occupies the *other* pane, so the remote has
            // somewhere to sit without displacing it.
            let (left, right) = if panel == 0 {
                (None, local)
            } else {
                (local, None)
            };
            // Only the local pane takes the flag; the remote one replaces its
            // panel wholesale on connect and never measures anyway.
            let mut b = FileBrowser::new_shallow(left, right, dual, shallow);
            b.connect_on_start_in_panel(&target, panel);
            b
        }
    };
    browser.run().await
}

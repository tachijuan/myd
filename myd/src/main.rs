use anyhow::Result;
use clap::Parser;
use myd::app::FileBrowser;
use myd::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    // Diagnostics, when asked for via MYD_LOG / MYD_TRACE. Must happen here:
    // without it no subscriber is installed and every tracing call in the app is
    // a no-op, so `MYD_LOG=...` silently produced nothing. Output goes to a file
    // (see `trace::trace_path`) because the TUI owns the terminal.
    myd::trace::init();

    let cli = Cli::parse();

    // A leading `sftp://…` opens a remote panel; the left panel then falls back
    // to the current directory so the two sit side by side for copying.
    let remote = cli
        .path
        .as_deref()
        .filter(|p| myd::cli::is_remote_arg(p))
        .and_then(|p| p.to_str())
        .map(str::to_string);

    let mut browser = if cli.goto {
        // Asked to be shown the picker, so nothing is opened until a destination
        // is chosen. Clap rejects a path alongside the flag, so there is no
        // argument being ignored here.
        FileBrowser::new_on_picker()
    } else if let Some(target) = remote {
        let local = std::env::current_dir().ok();
        let mut b = FileBrowser::new(local, None, false);
        b.connect_on_start(&target);
        b
    } else {
        FileBrowser::new(cli.path, cli.right, cli.dual)
    };
    browser.run().await
}

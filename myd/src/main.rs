use anyhow::Result;
use clap::Parser;
use myd::app::FileBrowser;
use myd::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // A leading `sftp://…` opens a remote panel; the left panel then falls back
    // to the current directory so the two sit side by side for copying.
    let remote = cli
        .path
        .as_deref()
        .filter(|p| myd::cli::is_remote_arg(p))
        .and_then(|p| p.to_str())
        .map(str::to_string);

    let mut browser = if let Some(target) = remote {
        let local = std::env::current_dir().ok();
        let mut b = FileBrowser::new(local, None, false);
        b.connect_on_start(&target);
        b
    } else {
        FileBrowser::new(cli.path, cli.right, cli.dual)
    };
    browser.run().await
}

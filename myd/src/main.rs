use anyhow::Result;
use clap::Parser;
use myd::app::FileBrowser;
use myd::cli::Cli;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let mut browser = FileBrowser::new(cli.path, cli.right, cli.dual);
    browser.run().await
}

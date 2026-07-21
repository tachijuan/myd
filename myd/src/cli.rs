use std::path::PathBuf;

use clap::Parser;

/// A vi-like terminal file browser with disk space visualization.
#[derive(Parser, Debug)]
#[command(name = "myd", version, about, long_about = None)]
pub struct Cli {
    /// Starting directory (defaults to directory picker if omitted)
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub path: Option<PathBuf>,
}

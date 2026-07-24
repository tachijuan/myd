use std::path::PathBuf;

use clap::Parser;

/// A vi-like terminal file browser with disk space visualization.
#[derive(Parser, Debug)]
#[command(name = "myd", version, about, long_about = None)]
pub struct Cli {
    /// Starting directory for the left panel (defaults to directory picker if omitted)
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub path: Option<PathBuf>,

    /// Starting directory for the right panel. Providing it implies dual mode.
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub right: Option<PathBuf>,

    /// Start in dual-panel mode (two views side by side).
    #[arg(long, short = '2')]
    pub dual: bool,
}

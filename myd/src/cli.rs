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

/// Whether a path-like argument is actually a remote target (`sftp://…` or
/// `ssh://…`) rather than a local path.
pub fn is_remote_arg(arg: &std::path::Path) -> bool {
    arg.to_str()
        .map(|s| s.starts_with("sftp://") || s.starts_with("ssh://"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recognises_remote_targets() {
        assert!(is_remote_arg(Path::new("sftp://host/path")));
        assert!(is_remote_arg(Path::new("ssh://user@host")));
    }

    #[test]
    fn treats_local_paths_as_local() {
        assert!(!is_remote_arg(Path::new("/home/user")));
        assert!(!is_remote_arg(Path::new("./relative")));
        assert!(!is_remote_arg(Path::new("sftp-not-a-url")));
    }
}

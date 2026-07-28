use std::path::PathBuf;

use clap::Parser;

/// A vi-like terminal file browser with disk space visualization.
#[derive(Parser, Debug)]
#[command(name = "myd", version, about, long_about = None)]
pub struct Cli {
    /// Starting directory for the left panel (defaults to the current directory)
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub path: Option<PathBuf>,

    /// Starting directory for the right panel. Providing it implies dual mode.
    #[arg(value_hint = clap::ValueHint::DirPath)]
    pub right: Option<PathBuf>,

    /// Start in dual-panel mode (two views side by side).
    #[arg(long, short = '2')]
    pub dual: bool,

    /// Choose from your saved directories and hosts instead of opening a path.
    // Conflicts with a path rather than overriding one: the flag asks to be
    // prompted for a destination, so supplying one too is a contradiction, and
    // reporting it beats silently honouring whichever happened to win.
    #[arg(long, short = 'g', conflicts_with_all = ["path", "right"])]
    pub goto: bool,
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

    #[test]
    fn goto_takes_no_path() {
        use clap::CommandFactory;
        Cli::command().debug_assert();

        let cli = Cli::try_parse_from(["myd", "--goto"]).expect("--goto alone is valid");
        assert!(cli.goto);
        assert!(cli.path.is_none());
        assert!(Cli::try_parse_from(["myd", "-g"]).unwrap().goto);

        // A path alongside it is a contradiction — being asked where to go and
        // told at the same time — so it is reported rather than half-honoured.
        assert!(Cli::try_parse_from(["myd", "--goto", "/tmp"]).is_err());
        assert!(Cli::try_parse_from(["myd", "--goto", "/tmp", "/var"]).is_err());
    }
}

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
    // `-d` to match the `gd` chord that opens the same picker in the app. `-2`
    // already holds the short form for --dual, so the letter was free.
    //
    // Conflicts with a path rather than overriding one: the flag asks to be
    // prompted for a destination, so supplying one too is a contradiction, and
    // reporting it beats silently honouring whichever happened to win.
    #[arg(long, short = 'd', conflicts_with_all = ["path", "right"])]
    pub directory: bool,

    /// Browse without measuring directory sizes (the `S` toggle, from the start).
    // Applies to whichever panels open, single or dual. Remote panels are never
    // measured anyway, so the flag is simply redundant there rather than wrong —
    // no reason to say so.
    #[arg(long, short = 's')]
    pub shallow: bool,
}

/// Whether a path-like argument is actually a remote target (`sftp://…` or
/// `ssh://…`) rather than a local path.
pub fn is_remote_arg(arg: &std::path::Path) -> bool {
    arg.to_str()
        .map(|s| s.starts_with("sftp://") || s.starts_with("ssh://"))
        .unwrap_or(false)
}

/// What the arguments ask the app to open.
///
/// Separated from `main` so the decision can be tested without a terminal: the
/// rule that either positional argument may be a remote target is easy to get
/// wrong, and did go wrong — only the first was ever checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Startup {
    /// Show the picker and open nothing until a destination is chosen.
    Picker,
    /// Open local panels: left, optional right, and whether to split.
    Local {
        left: Option<PathBuf>,
        right: Option<PathBuf>,
        dual: bool,
        /// Open without measuring directory sizes.
        shallow: bool,
    },
    /// Connect to `target`, opening it in panel `panel`. `local` is the other
    /// panel's directory when the layout is split.
    Remote {
        target: String,
        panel: usize,
        local: Option<PathBuf>,
        dual: bool,
        /// Open the *local* pane without measuring. The remote one never
        /// measures regardless, so this says nothing about it.
        shallow: bool,
    },
}

impl Cli {
    /// Decide what to open. `cwd` stands in for the current directory, so the
    /// choice does not depend on where the test process happens to be.
    pub fn startup(&self, cwd: Option<PathBuf>) -> Startup {
        let as_remote = |p: &Option<PathBuf>| -> Option<String> {
            p.as_deref()
                .filter(|p| is_remote_arg(p))
                .and_then(|p| p.to_str())
                .map(str::to_string)
        };

        if self.directory {
            return Startup::Picker;
        }
        // Either positional may be the remote one. Checking only the first left
        // `myd /tmp sftp://host` handing "sftp://host" to a panel as a path,
        // which is not a directory, so it opened the picker instead.
        if let Some(target) = as_remote(&self.right) {
            // The local path keeps the left panel and the remote opens beside
            // it — the pairing dual mode exists for.
            return Startup::Remote {
                target,
                panel: 1,
                local: self.path.clone(),
                dual: true,
                shallow: self.shallow,
            };
        }
        if let Some(target) = as_remote(&self.path) {
            // A second, local path shares the split; otherwise the remote takes
            // the pane on its own and the cwd sits beside it.
            let (local, dual) = match &self.right {
                Some(right) => (Some(right.clone()), true),
                None => (cwd, self.dual),
            };
            return Startup::Remote {
                target,
                panel: 0,
                local,
                dual,
                shallow: self.shallow,
            };
        }
        Startup::Local {
            left: self.path.clone(),
            right: self.right.clone(),
            dual: self.dual,
            shallow: self.shallow,
        }
    }
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

    /// `myd <local> sftp://host` must connect, not open the picker.
    #[test]
    fn a_remote_second_argument_opens_beside_the_local_one() {
        // Reported: `myd /tmp sftp://gb10` split the view but the right pane went
        // to the picker. Only the *first* argument was tested for a remote
        // target, so "sftp://gb10" reached the panel as a path, failed `is_dir`,
        // and fell back to the picker.
        let cli = Cli::try_parse_from(["myd", "/tmp", "sftp://gb10"]).unwrap();
        assert_eq!(
            cli.startup(Some(PathBuf::from("/cwd"))),
            Startup::Remote {
                target: "sftp://gb10".into(),
                panel: 1,
                local: Some(PathBuf::from("/tmp")),
                dual: true,
                shallow: false,
            },
            "the remote belongs in the right pane, beside the local path"
        );
    }

    #[test]
    fn a_leading_remote_still_opens_beside_the_current_directory() {
        // The long-standing form: the remote takes a pane and the cwd sits in the
        // other, so the two are ready to copy between.
        let cli = Cli::try_parse_from(["myd", "sftp://gb10"]).unwrap();
        assert_eq!(
            cli.startup(Some(PathBuf::from("/cwd"))),
            Startup::Remote {
                target: "sftp://gb10".into(),
                panel: 0,
                local: Some(PathBuf::from("/cwd")),
                dual: false,
                shallow: false,
            }
        );

        // With an explicit local path it shares the split rather than being
        // dropped, which is the same pairing read the other way round.
        let cli = Cli::try_parse_from(["myd", "sftp://gb10", "/tmp"]).unwrap();
        assert_eq!(
            cli.startup(Some(PathBuf::from("/cwd"))),
            Startup::Remote {
                target: "sftp://gb10".into(),
                panel: 0,
                local: Some(PathBuf::from("/tmp")),
                dual: true,
                shallow: false,
            }
        );
    }

    #[test]
    fn local_arguments_are_unchanged() {
        let cli = Cli::try_parse_from(["myd"]).unwrap();
        assert_eq!(
            cli.startup(Some(PathBuf::from("/cwd"))),
            Startup::Local { left: None, right: None, dual: false, shallow: false }
        );

        let cli = Cli::try_parse_from(["myd", "/tmp", "/var"]).unwrap();
        assert_eq!(
            cli.startup(None),
            Startup::Local {
                left: Some(PathBuf::from("/tmp")),
                right: Some(PathBuf::from("/var")),
                dual: false,
                shallow: false,
            },
            "two local paths are still the plain dual-panel form"
        );

        // The flag wins over everything else.
        let cli = Cli::try_parse_from(["myd", "--directory"]).unwrap();
        assert_eq!(cli.startup(None), Startup::Picker);
    }

    #[test]
    fn shallow_carries_through_every_layout() {
        // The flag is about how to browse, not about what to open, so it rides
        // along with whichever layout the arguments asked for.
        let cli = Cli::try_parse_from(["myd", "-s", "/tmp"]).unwrap();
        assert!(cli.shallow);
        assert_eq!(
            cli.startup(None),
            Startup::Local {
                left: Some(PathBuf::from("/tmp")),
                right: None,
                dual: false,
                shallow: true,
            }
        );

        // Both panes of a split.
        let cli = Cli::try_parse_from(["myd", "--shallow", "/tmp", "/var"]).unwrap();
        assert_eq!(
            cli.startup(None),
            Startup::Local {
                left: Some(PathBuf::from("/tmp")),
                right: Some(PathBuf::from("/var")),
                dual: false,
                shallow: true,
            }
        );

        // And alongside a remote, where it describes the local pane only.
        let cli = Cli::try_parse_from(["myd", "-s", "/tmp", "sftp://gb10"]).unwrap();
        assert_eq!(
            cli.startup(None),
            Startup::Remote {
                target: "sftp://gb10".into(),
                panel: 1,
                local: Some(PathBuf::from("/tmp")),
                dual: true,
                shallow: true,
            }
        );

        // Off unless asked for.
        assert!(!Cli::try_parse_from(["myd", "/tmp"]).unwrap().shallow);
    }

    #[test]
    fn directory_takes_no_path() {
        use clap::CommandFactory;
        Cli::command().debug_assert();

        let cli =
            Cli::try_parse_from(["myd", "--directory"]).expect("--directory alone is valid");
        assert!(cli.directory);
        assert!(cli.path.is_none());
        // `-d` matches the `gd` chord; `-2` still holds --dual's short form, so
        // the two do not collide.
        assert!(Cli::try_parse_from(["myd", "-d"]).unwrap().directory);
        assert!(Cli::try_parse_from(["myd", "-2"]).unwrap().dual);
        assert!(!Cli::try_parse_from(["myd", "-2"]).unwrap().directory);

        // A path alongside it is a contradiction — being asked where to go and
        // told at the same time — so it is reported rather than half-honoured.
        assert!(Cli::try_parse_from(["myd", "--directory", "/tmp"]).is_err());
        assert!(Cli::try_parse_from(["myd", "--directory", "/tmp", "/var"]).is_err());
    }
}

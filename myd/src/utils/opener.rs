//! Handing a path to the desktop's default application.
//!
//! Every platform has its own launcher and they take the same shape — a command
//! and the path — so the only real work is picking the right one and getting out
//! of the way afterwards.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};

/// The command this platform uses to open a path with its default handler.
///
/// Chosen at compile time rather than by probing at runtime: the target is known
/// when the binary is built, and a runtime check would only be able to guess.
#[cfg(target_os = "macos")]
pub const OPENER: &str = "open";

/// Linux and the BSDs go through the freedesktop helper, which every desktop
/// environment provides.
#[cfg(not(target_os = "macos"))]
pub const OPENER: &str = "xdg-open";

/// Open `path` with the platform's default application.
///
/// Returns as soon as the launcher has been started, not when the application
/// exits — the opened program may well outlive myd, and waiting would freeze the
/// event loop until the user closed it.
///
/// The child's streams are detached. `xdg-open` and some of the handlers it
/// delegates to write to stdout and stderr, and anything written there lands on
/// top of the alternate screen and corrupts the display.
pub fn open_path(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("{} does not exist", path.display());
    }
    Command::new(OPENER)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| {
            format!(
                "could not run {} (is it installed and on your PATH?)",
                OPENER
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_opener_matches_the_platform() {
        if cfg!(target_os = "macos") {
            assert_eq!(OPENER, "open");
        } else {
            assert_eq!(OPENER, "xdg-open");
        }
    }

    #[test]
    fn a_missing_path_is_reported_rather_than_spawned() {
        // Spawning the launcher on a path that is not there produces whatever
        // that launcher decides to say, on a terminal the TUI owns. Checking
        // first keeps the message ours.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-file");
        let err = open_path(&missing).expect_err("a missing path must fail");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {}",
            err
        );
    }
}

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

    // Put the commented-out `editor` setting in prefs.toml if it is not already
    // there. `editor` has no key that toggles it, so without this the only way
    // to find it is the manual; the check inside makes this a no-op on every
    // launch after the first.
    myd::prefs::seed_editor_template();

    let cli = Cli::parse();

    // A path that cannot be opened is reported here rather than swapped for the
    // picker. The panel falls back silently, which looks like the argument was
    // ignored: `myd Dropbox/images` opening the picker gives no hint whether the
    // path was misspelt, is a file, or was never readable — and the one thing
    // the user knows for certain is that they asked for a directory.
    //
    // Checked before the terminal is taken over, so the message lands on a
    // normal screen instead of being wiped by the alternate buffer.
    for path in [&cli.path, &cli.right].into_iter().flatten() {
        if myd::cli::is_remote_arg(path) {
            continue; // Not a local path; the connect path reports its own errors.
        }
        if let Err(e) = check_openable(path) {
            anyhow::bail!("{}: {}", path.display(), e);
        }
    }

    let mut browser = match cli.startup(std::env::current_dir().ok()) {
        // Asked to be shown the picker, so nothing is opened until a destination
        // is chosen. Clap rejects a path alongside the flag, so there is no
        // argument being ignored here.
        Startup::Picker { shallow } => FileBrowser::new_on_picker_shallow(shallow),
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

/// Why a starting path cannot be opened, or `Ok` if it can.
///
/// Separates the cases rather than reporting one "not a directory" for all of
/// them: a typo, a file passed where a directory was meant, and a directory that
/// exists but cannot be read are three different things to do next, and the
/// panel's silent fall-back to the picker distinguished none of them.
///
/// `~` is expanded the same way the panel expands it, so a path that works
/// unquoted is not rejected here for want of shell expansion.
fn check_openable(path: &std::path::Path) -> Result<(), String> {
    let expanded = expand_user(path);
    match std::fs::metadata(&expanded) {
        Ok(m) if m.is_dir() => {
            // Existing but unreadable is its own failure: the tree would open
            // empty, which reads as the directory being empty.
            match std::fs::read_dir(&expanded) {
                Ok(_) => Ok(()),
                Err(e) => Err(format!("cannot read this directory ({e})")),
            }
        }
        Ok(_) => Err("not a directory".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err("no such directory".to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Expand a leading `~`, matching what the panel does with a starting path.
fn expand_user(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return std::path::PathBuf::from(home).join(rest);
        }
    }
    path.to_path_buf()
}

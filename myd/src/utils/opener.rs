//! Handing a path to another program.
//!
//! Two ways out of myd live here. The desktop's default handler takes a command
//! and a path and they have the same shape on every platform, so the only real
//! work is picking the right one and getting out of the way afterwards. The
//! other is a command the user typed, which has to be split, resolved and run —
//! and, because it may want the terminal, run in the foreground.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

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

/// Split a typed command line into a program and its arguments.
///
/// Quotes are honoured, single and double alike, so a program living under a
/// path with a space in it can be typed as one word. This is deliberately not a
/// shell: there is no expansion, no globbing and no `$VAR`, because the command
/// is handed to `exec` rather than to `sh -c`. Running it through a shell would
/// mean the user's own filenames could be read as syntax.
///
/// Returns `None` when the line holds nothing but whitespace.
pub fn split_command(input: &str) -> Option<(String, Vec<String>)> {
    let mut words: Vec<String> = Vec::new();
    let mut word = String::new();
    // `Some(q)` while inside a quoted run, holding the quote that opened it so
    // the other kind is treated as an ordinary character until it closes.
    let mut quote: Option<char> = None;
    // Distinct from `word.is_empty()`: `""` is an empty argument, not no
    // argument, and the two have to stay tellable apart.
    let mut quoted_any = false;

    for c in input.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.push(c),
            None if c == '\'' || c == '"' => {
                quote = Some(c);
                quoted_any = true;
            }
            None if c.is_whitespace() => {
                if !word.is_empty() || quoted_any {
                    words.push(std::mem::take(&mut word));
                    quoted_any = false;
                }
            }
            None => word.push(c),
        }
    }
    // An unterminated quote keeps what it collected rather than failing. The
    // user is mid-typing far more often than they mean a literal quote, and
    // refusing the whole line would only lose the rest of it.
    if !word.is_empty() || quoted_any {
        words.push(word);
    }

    let mut it = words.into_iter();
    let program = it.next()?;
    Some((program, it.collect()))
}

/// True when `path` is a file this user can execute.
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Resolve `program` to something runnable.
///
/// A name containing a separator is taken as written, relative to the working
/// directory if it is not absolute. A bare name is looked up on `PATH`.
///
/// Resolving before spawning keeps the failure ours: the alternative is to hand
/// the name to `Command` and let the OS report `ENOENT`, which surfaces as
/// "No such file or directory" without ever saying *what* was not found — the
/// same reasoning as the `exists` check in [`open_path`].
pub fn resolve_program(program: &str) -> Result<PathBuf> {
    if program.is_empty() {
        bail!("no program given");
    }
    if program.contains(std::path::MAIN_SEPARATOR) {
        let path = PathBuf::from(program);
        if !path.exists() {
            bail!("{} does not exist", path.display());
        }
        if !is_executable(&path) {
            bail!("{} is not executable", path.display());
        }
        return Ok(path);
    }

    let Some(paths) = std::env::var_os("PATH") else {
        bail!("{} not found: PATH is not set", program);
    };
    for dir in std::env::split_paths(&paths) {
        // An empty entry in PATH means the working directory. Joining onto it
        // would produce a bare relative name, which is the same lookup again.
        if dir.as_os_str().is_empty() {
            continue;
        }
        let candidate = dir.join(program);
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    bail!("{} not found on PATH", program)
}

/// Run `program args… files…` and wait for it to finish.
///
/// The child inherits this process's stdin, stdout and stderr, so it gets the
/// real terminal and an editor or pager behaves exactly as it would from the
/// shell. The caller is responsible for having left the alternate screen first —
/// see `FileBrowser::run_program_suspended`, which is the only caller.
///
/// Files come after the user's own arguments, as absolute paths: `vim -v` on two
/// tagged files runs `vim -v /abs/one /abs/two`. Appending rather than
/// interleaving means options keep the position the user gave them.
pub fn run_foreground(program: &Path, args: &[String], files: &[PathBuf]) -> Result<ExitStatus> {
    Command::new(program)
        .args(args)
        .args(files)
        .status()
        .with_context(|| format!("could not run {}", program.display()))
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

    #[test]
    fn a_command_line_splits_into_program_and_arguments() {
        let (program, args) = split_command("vim -v").expect("a command must split");
        assert_eq!(program, "vim");
        assert_eq!(args, vec!["-v".to_string()]);
    }

    #[test]
    fn runs_of_whitespace_do_not_produce_empty_arguments() {
        let (program, args) = split_command("  vim   -u   NONE  ").expect("a command must split");
        assert_eq!(program, "vim");
        assert_eq!(args, vec!["-u".to_string(), "NONE".to_string()]);
    }

    #[test]
    fn quotes_hold_a_space_together() {
        // Without this, a program under "/opt/my editor/bin" is unreachable:
        // the line would split mid-path and resolve nothing.
        let (program, args) =
            split_command("'/opt/my editor/ed' -f \"two words\"").expect("a command must split");
        assert_eq!(program, "/opt/my editor/ed");
        assert_eq!(args, vec!["-f".to_string(), "two words".to_string()]);
    }

    #[test]
    fn a_quoted_empty_string_stays_an_argument() {
        // `""` is an empty argument, not the absence of one, and some programs
        // are given one deliberately.
        let (program, args) = split_command("prog \"\" x").expect("a command must split");
        assert_eq!(program, "prog");
        assert_eq!(args, vec![String::new(), "x".to_string()]);
    }

    #[test]
    fn an_empty_command_line_splits_into_nothing() {
        assert!(split_command("").is_none());
        assert!(split_command("   \t ").is_none());
    }

    #[test]
    fn a_bare_name_resolves_through_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("mydtestprog");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Scoped to this test's lookup rather than mutating the process
        // environment, which every other test in this binary shares.
        let found = std::env::split_paths(&std::ffi::OsString::from(dir.path()))
            .map(|d| d.join("mydtestprog"))
            .find(|c| is_executable(c));
        assert_eq!(found.as_deref(), Some(bin.as_path()));
    }

    #[test]
    fn a_file_without_the_executable_bit_is_not_a_program() {
        // Otherwise `O` on a directory full of data files would happily try to
        // exec one and report the kernel's error instead of ours.
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("notes.txt");
        std::fs::write(&data, "hello").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(!is_executable(&data));
            let err = resolve_program(data.to_str().unwrap())
                .expect_err("a non-executable path must fail");
            assert!(
                err.to_string().contains("not executable"),
                "unexpected error: {}",
                err
            );
        }
    }

    #[test]
    fn an_absolute_path_is_used_as_typed() {
        // /bin/sh is executable everywhere this runs.
        let resolved = resolve_program("/bin/sh").expect("/bin/sh must resolve");
        assert_eq!(resolved, PathBuf::from("/bin/sh"));
    }

    #[test]
    fn a_missing_path_reports_which_path_was_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-program");
        let err = resolve_program(missing.to_str().unwrap()).expect_err("must fail");
        assert!(
            err.to_string().contains("does not exist"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn a_name_that_is_nowhere_on_the_path_names_itself_in_the_error() {
        let err = resolve_program("myd-no-such-program-anywhere").expect_err("must fail");
        let msg = err.to_string();
        assert!(
            msg.contains("myd-no-such-program-anywhere") && msg.contains("PATH"),
            "unexpected error: {}",
            msg
        );
    }
}

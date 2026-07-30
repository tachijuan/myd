//! Drawing images and PDFs by way of `timg` or `chafa`.
//!
//! Both tools can render a picture as coloured block characters. We capture that
//! output and parse it (see [`crate::widget::ansi`]) rather than letting either
//! one write to the terminal, which would land on top of the alternate screen.
//!
//! Neither is a dependency. When both are missing the pane says so and shows what
//! it knows from the file's metadata.
//!
//! # Why the choice is per file, not once at startup
//!
//! `timg` is preferred. It also renders PDFs, when built against poppler, which
//! `chafa` cannot do at all — chafa's loaders cover image formats only and it
//! exits with `Unknown file format` on a PDF. So "which renderer" depends on what
//! is being rendered, and a PDF on a chafa-only machine has no renderer even
//! though an image on that machine does.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use super::graphics::Protocol;

/// How long a renderer may run before it is killed.
///
/// Both tools finish a large photo in about 130ms. This is far enough above that
/// to never trigger in normal use, and short enough that a pathological file
/// cannot leave the pane stuck on "Loading".
const RENDER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Cap on captured output. A 200x200-cell render is roughly 400KB; this leaves
/// generous headroom while bounding what a misbehaving tool can hand back.
const MAX_OUTPUT: usize = 8 * 1024 * 1024;

/// How many times a render is halved trying to fit a multiplexer's limit.
///
/// Each step roughly quarters the payload, so four covers a 16x reduction —
/// more than enough to bring any pane-sized render under the limit.
const MAX_SHRINK_STEPS: usize = 4;

/// Floor for a shrunken render. Below this the image is too small to read and
/// showing nothing would be more honest.
const MIN_GRAPHICS_COLS: u16 = 20;
const MIN_GRAPHICS_ROWS: u16 = 6;

/// A terminal image renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Preferred: better quality, and the only one that renders PDFs.
    Timg,
    Chafa,
}

impl Backend {
    pub fn binary(self) -> &'static str {
        match self {
            Backend::Timg => "timg",
            Backend::Chafa => "chafa",
        }
    }

    /// The arguments that render `path` into a `cols` x `rows` block of text.
    ///
    /// `page` selects a page of a multi-page document, and is ignored by chafa,
    /// which has no notion of one.
    ///
    /// Both invocations pin the output format instead of letting the tool
    /// auto-detect. Left to themselves they may emit sixel or kitty graphics,
    /// which cannot be turned into ratatui spans at all — and this often runs
    /// under tmux, which blocks those protocols anyway.
    fn args(
        self,
        path: &Path,
        cols: u16,
        rows: u16,
        page: usize,
        protocol: Protocol,
    ) -> Vec<std::ffi::OsString> {
        let mut args: Vec<std::ffi::OsString> = Vec::new();
        match self {
            Backend::Timg => {
                // `q` is quarter-block: four pixels per cell, plain SGR colour,
                // parseable but visibly blocky. `k` and `i` hand the terminal a
                // real PNG and are only chosen where it can be shown to
                // understand them.
                args.push("-p".into());
                args.push(protocol.timg_flag().into());
                if protocol.is_graphics() {
                    // Fill the space rather than sitting tiny in the middle of
                    // it. Without this timg never enlarges an image beyond its
                    // own pixel size, so a 260x91 logo stays 260x91 inside a
                    // pane hundreds of pixels across — the escape says "use
                    // these cells" and iTerm2 letterboxes the small bitmap
                    // inside them. Only for graphics: a block render is already
                    // one cell per glyph and upscaling would just blur it.
                    args.push("-U".into());
                }
                // Note there is deliberately no `-W` / `--fit-width` here. It
                // forces the image to the full width and lets the height follow
                // the aspect ratio, which for a graphics protocol means a tall
                // picture runs off the bottom of the pane and paints over whatever
                // is below — nothing clips it, because the terminal draws the image
                // and ratatui knows nothing about it.
                //
                // Left alone, timg fits both dimensions and honours the row budget
                // exactly: measured across a wide photo, a very tall image and
                // several budgets, the result was always `rows * 18` pixels high,
                // and an image smaller than the box is never upscaled. So the
                // pane's own geometry can be handed over unchanged.
                //
                // Geometry is mandatory. With stdout piped, timg cannot query the
                // terminal for a size and exits with "Failed to read size from
                // terminal" — note this is `-g`, as `--grid` does not set it.
                args.push(format!("-g{cols}x{rows}").into());
                // One frame only, so an animated GIF or a multi-page PDF becomes
                // a still instead of a stream of frames.
                args.push("--frames=1".into());
                // timg counts PDF pages as frames, so the page to show is the
                // frame to start at.
                if page > 0 {
                    args.push(format!("--frame-offset={page}").into());
                }
                // Do not blend transparency against a guessed terminal colour;
                // querying for it is pointless when stdout is a pipe.
                args.push("-b".into());
                args.push("none".into());
            }
            Backend::Chafa => {
                args.push("--format=symbols".into());
                // Forced rather than negotiated: piped output happened to stay
                // truecolor across several TERM values, but pinning it means the
                // parser can never meet indexed `38;5;N`.
                args.push("--colors=truecolor".into());
                // Suppresses the cursor hide/show sequences at the source, which
                // is tidier than stripping them afterwards.
                args.push("--polite=on".into());
                args.push(format!("--size={cols}x{rows}").into());
                args.push("--animate=off".into());
            }
        }
        args.push(path.as_os_str().into());
        args
    }
}

/// What the installed tools can do. Probed once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    pub timg: bool,
    pub chafa: bool,
    /// Whether this `timg` was built with PDF rendering. It reports its linked
    /// libraries in `--version`, so this is read rather than guessed — a build
    /// without poppler is a normal thing to find.
    pub timg_pdf: bool,
}

impl Capabilities {
    /// The renderer to use for `path`, if any.
    pub fn backend_for(&self, path: &Path) -> Option<Backend> {
        if crate::utils::filetype::is_pdf(path) {
            // chafa is not a fallback here: it has no PDF loader, so trying it
            // would only produce a confusing error.
            return (self.timg && self.timg_pdf).then_some(Backend::Timg);
        }
        if self.timg {
            Some(Backend::Timg)
        } else if self.chafa {
            Some(Backend::Chafa)
        } else {
            None
        }
    }

    /// Why `path` has no renderer, phrased for the pane.
    pub fn explain_missing(&self, path: &Path) -> String {
        if crate::utils::filetype::is_pdf(path) {
            if self.timg && !self.timg_pdf {
                return "This timg was built without PDF support.".to_string();
            }
            if self.chafa && !self.timg {
                return "PDF preview needs timg (chafa cannot render PDFs).".to_string();
            }
            return "PDF preview needs timg on your PATH.".to_string();
        }
        "Image preview needs timg or chafa on your PATH.".to_string()
    }
}

/// Probe the installed renderers, once per process.
pub fn capabilities() -> Capabilities {
    static CACHE: OnceLock<Capabilities> = OnceLock::new();
    *CACHE.get_or_init(probe)
}

fn probe() -> Capabilities {
    let timg = version_output("timg");
    Capabilities {
        timg: timg.is_some(),
        chafa: version_output("chafa").is_some(),
        // timg lists its linked libraries here, one of which is
        // "PDF rendering with poppler ...".
        timg_pdf: timg
            .as_deref()
            .is_some_and(|v| v.to_ascii_lowercase().contains("pdf rendering")),
    }
}

/// Run `bin --version` and capture stdout, or `None` if it is not runnable.
///
/// The crate had no `which`-style helper; this is the same idea, and it doubles
/// as a way to read what a tool was built with. stderr is captured too because
/// some tools report their version there.
fn version_output(bin: &str) -> Option<String> {
    let out = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some(text)
}

/// The outcome of asking a renderer to draw a file.
pub enum Rendered {
    /// Raw tool output, ready for [`crate::widget::ansi::parse_ansi`].
    Ansi(String),
    /// The tool ran and refused. Carries its own message where there is one:
    /// exit codes differ between the tools (timg uses 1, and 3 for a geometry
    /// problem; chafa uses 2), so any non-zero status is a failure and the
    /// message is more use than the number.
    Failed(String),
}

/// How many pages a document has, if that can be determined.
///
/// Uses `pdfinfo` from poppler-utils, which usually accompanies a `timg` built
/// against poppler. `None` means "unknown", not "one": the caller must then let
/// the user page freely rather than pretending a limit it does not know.
pub fn page_count(path: &Path) -> Option<usize> {
    if !crate::utils::filetype::is_pdf(path) {
        return None;
    }
    let out = Command::new("pdfinfo")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix("Pages:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
}

/// Render `path` at a given cell geometry, optionally a specific page.
///
/// Blocking: call from `spawn_blocking`, never the event loop. Both tools take
/// roughly 80–130ms on real files, which is several frames.
pub fn render(
    backend: Backend,
    path: &Path,
    cols: u16,
    rows: u16,
    page: usize,
    protocol: Protocol,
) -> Rendered {
    // Zero geometry means the pane has not been laid out yet; the tools would
    // either fail or produce nothing useful.
    if cols == 0 || rows == 0 {
        return Rendered::Failed("no room to draw".to_string());
    }

    // Only timg speaks the graphics protocols; chafa always gets symbols.
    let protocol = match backend {
        Backend::Timg => protocol,
        Backend::Chafa => Protocol::Blocks,
    };
    // Sixel is sized by the geometry alone — unlike kitty and iTerm2 it carries
    // no cell-count control that could correct an overflow afterwards, so what
    // timg rasterises is exactly what appears.
    //
    // timg divides the geometry by an assumed 9x18 cell to reach a pixel size.
    // Where the real cell is known, the geometry is scaled so that assumption
    // lands on the pane's true pixel box: a taller real cell means asking for
    // proportionally more rows. Without that the image is drawn for a smaller
    // screen than the one it is on, which is why a forced sixel came out small.
    let rows = if protocol == Protocol::Sixel {
        let budgeted = super::graphics::sixel_row_budget(rows);
        super::graphics::scale_rows_for_cell(budgeted)
    } else {
        rows
    };
    // kitty and iTerm2 are told the cell count in the escape, so the raster can
    // be asked for larger than the pane without occupying more of it — which is
    // what stops the image arriving too few pixels to fill a real cell. Sixel has
    // no such pinning: its raster *is* its size, so it is asked for exactly what
    // it should occupy.
    let (cols, rows) = if protocol.is_graphics() && protocol != Protocol::Sixel {
        super::graphics::oversampled_geometry(cols, rows)
    } else {
        (cols, rows)
    };

    // Inside a multiplexer a payload past the terminal's limit is simply not
    // drawn — no error, just a blank rectangle — so it has to be brought under
    // that limit before it is sent. Halving the geometry roughly quarters the
    // payload, so a few steps cover a very large image, and each step is a real
    // render rather than a guess at what the size would be.
    let in_multiplexer = std::env::var_os("TMUX").is_some();
    let mut attempt = run_with_timeout(backend.binary(), &backend.args(path, cols, rows, page, protocol));

    if in_multiplexer {
        let (mut c, mut r) = (cols, rows);
        for _ in 0..MAX_SHRINK_STEPS {
            match attempt {
                // Judged per escape sequence, not on the total: a chunked
                // protocol carries a large image as many small escapes, and none
                // of them is what a multiplexer refuses.
                Ok((true, ref stdout, _))
                    if !super::graphics::sequences_fit(stdout, true) => {}
                _ => break,
            }
            // Never below a size worth looking at; a tiny thumbnail is no more
            // use than a blank space.
            let (nc, nr) = ((c / 2).max(MIN_GRAPHICS_COLS), (r / 2).max(MIN_GRAPHICS_ROWS));
            if (nc, nr) == (c, r) {
                break;
            }
            c = nc;
            r = nr;
            attempt = run_with_timeout(backend.binary(), &backend.args(path, c, r, page, protocol));
        }
    }
    finish(backend, attempt)
}

/// Turn a finished child process into a render result.
fn finish(
    backend: Backend,
    outcome: Result<(bool, String, String), String>,
) -> Rendered {
    match outcome {
        Ok((status, stdout, stderr)) => {
            if status {
                Rendered::Ansi(stdout)
            } else {
                let msg = stderr
                    .lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty())
                    .unwrap_or("could not render this file")
                    // The tools prefix their own name; the pane already shows it.
                    .trim_start_matches(&format!("{}: ", backend.binary()))
                    .to_string();
                Rendered::Failed(msg)
            }
        }
        Err(e) => Rendered::Failed(e),
    }
}

/// Spawn a renderer and wait for it, killing it if it overruns.
///
/// Returns `(success, stdout, stderr)`.
fn run_with_timeout(
    bin: &str,
    args: &[std::ffi::OsString],
) -> Result<(bool, String, String), String> {
    use std::io::Read;

    let mut child = Command::new(bin)
        .args(args.iter().map(OsStr::new))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {bin}: {e}"))?;

    // Read on this thread while polling for exit. Waiting for the child first
    // could deadlock: a large render can fill the pipe buffer, and the child
    // blocks on write while we block on wait.
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");

    let out = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.by_ref().take(MAX_OUTPUT as u64).read_to_end(&mut buf);
        buf
    });
    let err = std::thread::spawn(move || {
        let mut buf = Vec::new();
        // Errors are one line; the cap is only there to bound a runaway.
        let _ = stderr.by_ref().take(64 * 1024).read_to_end(&mut buf);
        buf
    });

    let deadline = std::time::Instant::now() + RENDER_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return Err(format!("{bin} failed: {e}")),
        }
    };

    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();

    let Some(status) = status else {
        return Err(format!(
            "{bin} took longer than {}s",
            RENDER_TIMEOUT.as_secs()
        ));
    };

    Ok((
        status.success(),
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_missing_binary_probes_as_absent() {
        assert!(version_output("myd-definitely-not-a-real-binary").is_none());
    }

    /// PDFs must never be routed to chafa: it has no PDF loader, so the only
    /// possible outcome is a confusing "Unknown file format".
    #[test]
    fn a_pdf_never_selects_chafa() {
        let caps = Capabilities {
            timg: false,
            chafa: true,
            timg_pdf: false,
        };
        let pdf = PathBuf::from("/tmp/x.pdf");
        assert_eq!(caps.backend_for(&pdf), None);
        assert!(caps.explain_missing(&pdf).contains("cannot render PDFs"));

        // The same machine still renders images perfectly well.
        assert_eq!(
            caps.backend_for(Path::new("/tmp/x.png")),
            Some(Backend::Chafa)
        );
    }

    #[test]
    fn a_pdf_needs_a_poppler_build_of_timg() {
        let no_pdf = Capabilities {
            timg: true,
            chafa: false,
            timg_pdf: false,
        };
        assert_eq!(no_pdf.backend_for(Path::new("/tmp/x.pdf")), None);
        assert!(
            no_pdf
                .explain_missing(Path::new("/tmp/x.pdf"))
                .contains("without PDF support")
        );

        let with_pdf = Capabilities {
            timg: true,
            chafa: false,
            timg_pdf: true,
        };
        assert_eq!(
            with_pdf.backend_for(Path::new("/tmp/x.pdf")),
            Some(Backend::Timg)
        );
    }

    /// timg is preferred wherever both are available.
    #[test]
    fn timg_wins_over_chafa() {
        let both = Capabilities {
            timg: true,
            chafa: true,
            timg_pdf: true,
        };
        assert_eq!(
            both.backend_for(Path::new("/tmp/x.png")),
            Some(Backend::Timg)
        );
    }

    #[test]
    fn nothing_installed_yields_no_backend() {
        let caps = Capabilities::default();
        assert_eq!(caps.backend_for(Path::new("/tmp/x.png")), None);
        assert!(caps.explain_missing(Path::new("/tmp/x.png")).contains("timg"));
    }

    /// Geometry is not optional for timg: without `-g` it tries to query the
    /// terminal, which fails when stdout is a pipe.
    #[test]
    fn timg_is_always_given_an_explicit_geometry() {
        let args = Backend::Timg.args(Path::new("/tmp/x.png"), 60, 24, 0, Protocol::Blocks);
        assert!(args.iter().any(|a| a == "-g60x24"));
        assert!(args.iter().any(|a| a == "-p"));
    }

    /// Both invocations must pin the output format, or the tool may emit sixel or
    /// kitty graphics, which cannot be parsed into spans.
    #[test]
    fn both_backends_pin_a_text_output_format() {
        let timg = Backend::Timg.args(Path::new("/tmp/x.png"), 10, 10, 0, Protocol::Blocks);
        assert!(timg.windows(2).any(|w| w[0] == "-p" && w[1] == "q"));

        let chafa = Backend::Chafa.args(Path::new("/tmp/x.png"), 10, 10, 0, Protocol::Blocks);
        assert!(chafa.iter().any(|a| a == "--format=symbols"));
        assert!(chafa.iter().any(|a| a == "--colors=truecolor"));
        assert!(chafa.iter().any(|a| a == "--polite=on"));
    }

    #[test]
    fn a_zero_sized_pane_does_not_spawn_anything() {
        let r = render(Backend::Timg, Path::new("/tmp/x.png"), 0, 0, 0, Protocol::Blocks);
        assert!(matches!(r, Rendered::Failed(_)));
    }

    /// Runs only where a renderer is installed. Proves the whole path end to
    /// end: spawn, capture, parse, and a real number of lines out.
    #[test]
    fn a_real_render_parses_into_lines() {
        let caps = capabilities();
        let Some(backend) = caps.backend_for(Path::new("x.png")) else {
            return; // no renderer here
        };
        let img = Path::new("/usr/share/pixmaps/ubuntu-logo-text.png");
        if !img.exists() {
            return;
        }
        match render(backend, img, 40, 20, 0, Protocol::Blocks) {
            Rendered::Ansi(text) => {
                let lines = crate::widget::ansi::parse_ansi(&text);
                assert!(!lines.is_empty(), "no lines from {}", backend.binary());
                assert!(crate::widget::ansi::block_width(&lines) > 0);
            }
            Rendered::Failed(m) => panic!("{} failed: {m}", backend.binary()),
        }
    }

    /// A non-image must be reported, not crash — and the exit code differs
    /// between the tools, which is why success is judged by status rather than a
    /// specific number.
    #[test]
    fn a_non_image_is_reported_as_a_failure() {
        let caps = capabilities();
        let Some(backend) = caps.backend_for(Path::new("x.png")) else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let txt = dir.path().join("notanimage.txt");
        std::fs::write(&txt, "hello").unwrap();
        assert!(matches!(
            render(backend, &txt, 20, 10, 0, Protocol::Blocks),
            Rendered::Failed(_)
        ));
    }
}

//! Reading a file and turning it into something the preview pane can draw.
//!
//! Three shapes of content, decided by extension and then by what the bytes
//! actually look like: highlighted text, an image rendered by an external tool,
//! or a note explaining why neither applies.
//!
//! Everything here is blocking or async I/O and belongs on a worker, never the
//! event loop. Reading is done through the [`Vfs`](crate::vfs::Vfs) so a file on
//! an SFTP panel is read from the server — using `std::fs` on a remote path either
//! fails or, worse, silently describes an unrelated local file that happens to
//! share the name.

pub mod graphics;
pub mod image;
pub mod markdown;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::utils::filetype;
use crate::vfs::{VPath, Vfs};
use crate::widget::ansi;

/// How much of a file to read for a text preview.
///
/// Enough for any source file, and a bound on what a multi-gigabyte log can cost.
/// The pane says when it is showing a truncated head.
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

/// Bytes sampled to decide whether a file is text at all.
const SNIFF_BYTES: usize = 8192;

/// Largest file that gets syntax highlighting.
///
/// syntect's cost is linear in the text it is given and not cheap: ~264ms for a
/// 4300-line Rust file, against 6.5ms for the first 200 lines of it. Past this
/// size the pane shows plain text immediately rather than making the user wait,
/// since the content is the point and the colours are a bonus.
///
/// 64KB is roughly 1500 lines of source — comfortably more than anyone reads in
/// a preview, and about 90ms in the worst grammar measured. Markdown does not go
/// through syntect at all (see [`markdown`]), so this only bounds code.
const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;

/// Largest remote file that will be staged to disk for image rendering.
///
/// The renderers take a path, not a stream, so a remote image has to be
/// downloaded first. That is a real transfer, so it is capped.
const MAX_REMOTE_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// What the pane should draw.
pub enum PreviewContent {
    /// Text, already highlighted. `truncated` when only a head was read.
    Text {
        lines: Vec<Line<'static>>,
        truncated: bool,
    },
    /// An image or PDF page, parsed from a renderer's output.
    Image {
        lines: Vec<Line<'static>>,
        /// Which tool drew it, for the footer.
        backend: &'static str,
        /// Zero-based page this is, for a multi-page document.
        page: usize,
        /// Total pages when this is a paged document at all.
        ///
        /// `None` means "not paged" — an ordinary image, where `j`/`k` must
        /// scroll rather than try to turn a page. A paged document whose length
        /// could not be determined reports `Some(0)`, which says "there are pages,
        /// but I do not know how many".
        pages: Option<usize>,
    },
    /// A real image, as escape data the terminal must receive verbatim.
    ///
    /// Kept out of the frame on purpose: kitty and iTerm2 graphics are base64
    /// payloads, not cells, so the widget leaves a blank hole and the escape is
    /// written to stdout after the frame. See [`graphics`].
    Graphics {
        /// The renderer's raw output, ready to be written to the terminal.
        payload: String,
        /// Rows the image occupies, so the pane can size the hole it leaves.
        rows: u16,
        backend: &'static str,
        page: usize,
        pages: Option<usize>,
    },
    /// Nothing to show, with the reason: a binary file, no renderer installed,
    /// a file too large to stage.
    Note { message: String },
}

impl PreviewContent {
    /// Rendered height in lines, for scroll bounds.
    pub fn len(&self) -> usize {
        match self {
            PreviewContent::Text { lines, .. } | PreviewContent::Image { lines, .. } => lines.len(),
            // The image is drawn by the terminal, not scrolled by us.
            PreviewContent::Graphics { rows, .. } => *rows as usize,
            PreviewContent::Note { .. } => 1,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The plain text of each line, for searching. Empty for a rendered image,
    /// where searching has no meaning.
    pub fn search_text(&self) -> Vec<String> {
        match self {
            PreviewContent::Text { lines, .. } => lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// What to load, and the geometry to render an image at.
///
/// The geometry is part of the request because an image has to be re-rendered
/// when the pane resizes; text does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    pub path: VPath,
    /// Display path — a remote file is staged to a temp file, and the name shown
    /// should still be the real one.
    pub label: PathBuf,
    pub cols: u16,
    pub rows: u16,
    /// Which page of a multi-page document to render, zero-based. Ignored for
    /// everything else.
    pub page: usize,
}

/// Load a preview. Blocking on the calling task; run it on a worker.
pub async fn load(fs: Arc<dyn Vfs>, req: PreviewRequest) -> PreviewContent {
    match load_inner(fs, &req).await {
        Ok(content) => content,
        Err(e) => PreviewContent::Note {
            message: format!("Could not read this file: {e}"),
        },
    }
}

async fn load_inner(fs: Arc<dyn Vfs>, req: &PreviewRequest) -> anyhow::Result<PreviewContent> {
    let path = &req.path;
    let label = req.label.as_path();

    // Images and PDFs go to an external renderer, chosen per file: chafa cannot
    // render PDFs, so "which tool" depends on the file and not just on what is
    // installed.
    if filetype::is_image_like(label) || filetype::is_pdf(label) {
        return render_image(fs, req).await;
    }

    let meta = fs.stat(path).await?;
    if meta.is_dir {
        // Directories have their own panel; the preview has nothing to add.
        return Ok(PreviewContent::Note {
            message: "This is a directory.".to_string(),
        });
    }
    if meta.len == 0 {
        return Ok(PreviewContent::Note {
            message: "This file is empty.".to_string(),
        });
    }

    let want = meta.len.min(MAX_TEXT_BYTES);
    let bytes = read_head(fs, path, want).await?;

    if !filetype::looks_like_text(&bytes[..bytes.len().min(SNIFF_BYTES)]) {
        return Ok(PreviewContent::Note {
            message: format!(
                "Binary file ({}). Press o to open it with the default application.",
                crate::utils::sizes::format_size(meta.len)
            ),
        });
    }

    let text = String::from_utf8_lossy(&bytes);
    // A head read almost certainly stops mid-line; dropping the partial line is
    // tidier than showing half of one.
    let truncated = meta.len > want;
    let text = if truncated {
        match text.rfind('\n') {
            Some(i) => &text[..i],
            None => &text,
        }
    } else {
        &text
    };

    // Highlighting is CPU-bound and slower than it looks: syntect's markdown
    // grammar takes ~170ms on a 25KB README in release, and several times that in
    // debug. That must not run on an async worker, where it would block every
    // other task on that thread.
    let owned = text.to_string();
    let label = label.to_path_buf();
    let lines = tokio::task::spawn_blocking(move || highlight(&label, &owned)).await?;

    Ok(PreviewContent::Text { lines, truncated })
}

/// Read up to `len` bytes from the start of a file.
///
/// SFTP backends get a positioned read, which is a single round trip. Note the
/// `Vfs` contract: a short read is legal at any point, not only at EOF, so this
/// loops until it has what it asked for or the file ends.
async fn read_head(fs: Arc<dyn Vfs>, path: &VPath, len: u64) -> anyhow::Result<Vec<u8>> {
    if fs.supports_parallel_read() {
        if let Ok(reader) = fs.open_positioned_read(path).await {
            let mut buf = Vec::with_capacity(len as usize);
            while (buf.len() as u64) < len {
                let want = (len - buf.len() as u64) as usize;
                let chunk = reader.read_at(buf.len() as u64, want).await?;
                if chunk.is_empty() {
                    break; // EOF
                }
                buf.extend_from_slice(&chunk);
            }
            return Ok(buf);
        }
    }

    use tokio::io::AsyncReadExt;
    let reader = fs.open_read(path).await?;
    let mut buf = Vec::with_capacity(len as usize);
    reader.take(len).read_to_end(&mut buf).await?;
    Ok(buf)
}

/// Render an image or PDF through whichever external tool suits it.
async fn render_image(fs: Arc<dyn Vfs>, req: &PreviewRequest) -> anyhow::Result<PreviewContent> {
    let caps = image::capabilities();
    let label = req.label.as_path();

    let Some(backend) = caps.backend_for(label) else {
        return Ok(PreviewContent::Note {
            message: caps.explain_missing(label),
        });
    };

    // The renderers take a path. A remote file has to come down first.
    let staged = if req.path.is_local() {
        None
    } else {
        let meta = fs.stat(&req.path).await?;
        if meta.len > MAX_REMOTE_IMAGE_BYTES {
            return Ok(PreviewContent::Note {
                message: format!(
                    "Remote image is {} — too large to fetch for a preview.",
                    crate::utils::sizes::format_size(meta.len)
                ),
            });
        }
        let bytes = read_head(fs, &req.path, meta.len).await?;
        let ext = label
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("preview");
        let mut tmp = tempfile::Builder::new()
            .prefix("myd-preview-")
            .suffix(&format!(".{ext}"))
            .tempfile()?;
        std::io::Write::write_all(tmp.as_file_mut(), &bytes)?;
        Some(tmp)
    };

    let render_path = staged
        .as_ref()
        .map(|t| t.path().to_path_buf())
        .unwrap_or_else(|| req.path.as_path().to_path_buf());

    let (cols, rows) = (req.cols, req.rows);
    let page = req.page;
    // The tools take 80-130ms on real files. That is several frames, so it goes
    // to a blocking worker even though we are already off the event loop. The
    // page count is asked for on the same worker, since it is another process.
    let probe = render_path.clone();
    // Only a paged format is asked for a page count; an ordinary image reports
    // `None`, which is what tells the pane to scroll rather than page.
    let paged = filetype::is_pdf(label);
    let protocol = graphics::protocol();
    let (rendered, pages) = tokio::task::spawn_blocking(move || {
        (
            image::render(backend, &render_path, cols, rows, page, protocol),
            // `Some(0)` = paged but the length is unknown.
            paged.then(|| image::page_count(&probe).unwrap_or(0)),
        )
    })
    .await?;
    // Keep the staged file alive until the renderer has finished with it.
    drop(staged);

    Ok(match rendered {
        // A graphics protocol's output is opaque escape data; it cannot be parsed
        // into cells and must reach the terminal as-is.
        image::Rendered::Ansi(text) if protocol.is_graphics() => {
            if text.trim().is_empty() {
                PreviewContent::Note {
                    message: format!("{} produced no output.", backend.binary()),
                }
            } else {
                PreviewContent::Graphics {
                    payload: text,
                    rows,
                    backend: backend.binary(),
                    page,
                    pages,
                }
            }
        }
        image::Rendered::Ansi(text) => {
            let lines = ansi::parse_ansi(&text);
            if lines.is_empty() {
                PreviewContent::Note {
                    message: format!("{} produced no output.", backend.binary()),
                }
            } else {
                PreviewContent::Image {
                    lines,
                    backend: backend.binary(),
                    page,
                    pages,
                }
            }
        }
        image::Rendered::Failed(message) => PreviewContent::Note { message },
    })
}

/// Highlight source text, falling back to plain lines.
///
/// syntect's own theme is applied once here rather than per frame: parsing is the
/// expensive part and the result is owned, so the pane just draws it.
fn highlight(path: &Path, text: &str) -> Vec<Line<'static>> {
    use syntect::easy::HighlightLines;
    use syntect::highlighting::{FontStyle, ThemeSet};
    use syntect::parsing::SyntaxSet;
    use syntect::util::LinesWithEndings;

    static ASSETS: std::sync::OnceLock<(SyntaxSet, syntect::highlighting::Theme)> =
        std::sync::OnceLock::new();
    let (syntaxes, theme) = ASSETS.get_or_init(|| {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let mut themes = ThemeSet::load_defaults();
        // A dark theme: the pane is drawn over a terminal background we cannot
        // query, and dark is the safer assumption for a TUI.
        let theme = themes
            .themes
            .remove("base16-ocean.dark")
            .or_else(|| themes.themes.values().next().cloned())
            .expect("syntect ships themes");
        (syntaxes, theme)
    });

    // Markdown gets a purpose-built highlighter: syntect's markdown grammar
    // embeds every other language so fenced blocks can be highlighted in their
    // own syntax, which costs ~170ms on a 30KB README where the hand-rolled pass
    // costs ~37us. A preview has to feel instant.
    if filetype::is_markdown(path) {
        return markdown::highlight(text);
    }

    let syntax = {
        path.extension()
            .and_then(|e| e.to_str())
            .and_then(|e| syntaxes.find_syntax_by_extension(e))
            .or_else(|| {
                text.lines()
                    .next()
                    .and_then(|l| syntaxes.find_syntax_by_first_line(l))
            })
    };

    // No syntax, or too much text to highlight responsively.
    let Some(syntax) = syntax.filter(|_| text.len() <= MAX_HIGHLIGHT_BYTES) else {
        return plain_lines(text);
    };

    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(text) {
        let Ok(ranges) = h.highlight_line(line, syntaxes) else {
            // Give up on highlighting from here rather than losing the file.
            out.extend(plain_lines(line));
            continue;
        };
        let spans = ranges
            .into_iter()
            .map(|(style, piece)| {
                let mut s = Style::default().fg(Color::Rgb(
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                ));
                // Background is deliberately not applied: filling every cell
                // fights the terminal's own theme and looks worse than letting it
                // show through.
                if style.font_style.contains(FontStyle::BOLD) {
                    s = s.add_modifier(Modifier::BOLD);
                }
                if style.font_style.contains(FontStyle::ITALIC) {
                    s = s.add_modifier(Modifier::ITALIC);
                }
                if style.font_style.contains(FontStyle::UNDERLINE) {
                    s = s.add_modifier(Modifier::UNDERLINED);
                }
                Span::styled(strip_eol(piece).to_string(), s)
            })
            .filter(|s| !s.content.is_empty())
            .collect::<Vec<_>>();
        out.push(Line::from(spans));
    }
    out
}

/// Plain, unstyled lines — for text with no syntax, or when highlighting fails.
fn plain_lines(text: &str) -> Vec<Line<'static>> {
    text.lines()
        .map(|l| Line::from(Span::raw(expand_tabs(l))))
        .collect()
}

fn strip_eol(s: &str) -> &str {
    s.trim_end_matches('\n').trim_end_matches('\r')
}

/// Tabs would otherwise be drawn as a single cell, wrecking indentation.
pub(crate) fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + 8);
    for c in line.chars() {
        if c == '\t' {
            let pad = 4 - (out.chars().count() % 4);
            out.extend(std::iter::repeat_n(' ', pad));
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_source_is_highlighted() {
        let lines = highlight(Path::new("a.rs"), "fn main() {}\n");
        assert_eq!(lines.len(), 1);
        // More than one span means the line was actually tokenised.
        assert!(lines[0].spans.len() > 1, "not highlighted: {:?}", lines[0]);
        assert!(lines[0].spans.iter().any(|s| s.style.fg.is_some()));
    }

    #[test]
    fn markdown_is_highlighted() {
        let lines = highlight(Path::new("r.md"), "# Title\n\nsome *text*\n");
        assert!(lines.len() >= 3);
        assert!(lines.iter().any(|l| l.spans.iter().any(|s| s.style.fg.is_some())));
    }

    /// An unknown extension must still show the text.
    #[test]
    fn unknown_extensions_fall_back_to_plain_text() {
        let lines = highlight(Path::new("x.zzzz"), "hello\nworld\n");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].spans[0].content, "world");
    }

    /// A shebang identifies a script that has no extension to go on.
    #[test]
    fn a_shebang_selects_a_syntax() {
        let lines = highlight(Path::new("script"), "#!/bin/bash\necho hi\n");
        assert!(lines[0].spans.iter().any(|s| s.style.fg.is_some()));
    }

    #[test]
    fn highlighted_lines_carry_no_newlines() {
        // A trailing newline inside a span would render as a stray blank cell.
        for l in highlight(Path::new("a.rs"), "fn a() {}\nfn b() {}\n") {
            for s in &l.spans {
                assert!(!s.content.contains('\n'), "newline in span: {:?}", s.content);
            }
        }
    }

    /// A large source file must show up as plain text rather than making the user
    /// wait on syntect, whose cost is linear in the text it is handed.
    #[test]
    fn a_large_file_skips_highlighting() {
        let big = "fn f() { let x = 1; }\n".repeat(20_000);
        assert!(big.len() > MAX_HIGHLIGHT_BYTES);

        let t = std::time::Instant::now();
        let lines = highlight(Path::new("big.rs"), &big);
        let elapsed = t.elapsed();

        assert_eq!(lines.len(), 20_000, "every line must still be shown");
        // Plain text: no colour anywhere.
        assert!(lines[0].spans.iter().all(|s| s.style.fg.is_none()));
        // Generous, since this also runs in debug — highlighting this much text
        // takes seconds.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "skipping highlighting should be fast, took {elapsed:?}"
        );
    }

    /// Markdown does not go through syntect, so the byte cap must not apply to
    /// it — a big README should still be coloured, and still be fast.
    #[test]
    fn a_large_markdown_file_is_still_highlighted_and_fast() {
        let big = "# heading with `code` and *emphasis*\n".repeat(20_000);
        assert!(big.len() > MAX_HIGHLIGHT_BYTES);

        let t = std::time::Instant::now();
        let lines = highlight(Path::new("big.md"), &big);
        let elapsed = t.elapsed();

        assert_eq!(lines.len(), 20_000);
        assert!(
            lines[0].spans.iter().any(|s| s.style.fg.is_some()),
            "markdown should still be highlighted past the syntect cap"
        );
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "markdown highlighting took {elapsed:?}"
        );
    }

    /// A file just under the cap still gets highlighted, so the cap is a ceiling
    /// rather than a switch that turned the feature off.
    #[test]
    fn a_normal_sized_file_is_still_highlighted() {
        let src = "fn main() { let x = 1; }\n".repeat(50);
        assert!(src.len() < MAX_HIGHLIGHT_BYTES);
        let lines = highlight(Path::new("a.rs"), &src);
        assert!(lines[0].spans.len() > 1, "should be tokenised");
    }

    #[test]
    fn tabs_become_spaces() {
        assert_eq!(expand_tabs("\tx"), "    x");
        assert_eq!(expand_tabs("ab\tx"), "ab  x");
        assert_eq!(expand_tabs("no tabs"), "no tabs");
    }

    #[test]
    fn text_detection_separates_source_from_binary() {
        assert!(filetype::looks_like_text(b"fn main() {}\n"));
        assert!(filetype::looks_like_text(b""));
        assert!(!filetype::looks_like_text(b"\x7fELF\x02\x01\x01\x00\x00"));
        // A NUL anywhere is decisive.
        assert!(!filetype::looks_like_text(b"text then \0 nul"));
    }

    /// A multi-byte character split by the read boundary is an artefact of where
    /// the read stopped, not evidence of a binary file.
    #[test]
    fn a_truncated_utf8_sequence_still_reads_as_text() {
        let mut s = "héllo wörld and some more text".as_bytes().to_vec();
        s.push(0xC3); // dangling lead byte
        assert!(filetype::looks_like_text(&s));
    }

    #[test]
    fn search_text_is_the_plain_content() {
        let c = PreviewContent::Text {
            lines: highlight(Path::new("a.rs"), "fn main() {}\nlet x = 1;\n"),
            truncated: false,
        };
        let text = c.search_text();
        assert_eq!(text.len(), 2);
        assert!(text[0].contains("fn main"));
        assert!(text[1].contains("let x"));
    }

    /// Searching a rendered image would only match block characters.
    #[test]
    fn images_are_not_searchable() {
        let c = PreviewContent::Image {
            lines: vec![Line::from("▀▄")],
            backend: "timg",
            page: 0,
            pages: None,
        };
        assert!(c.search_text().is_empty());
    }
}

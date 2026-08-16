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

/// How much of a file gets syntax highlighting.
///
/// syntect's cost is linear in the text it is given and not cheap: ~264ms for a
/// 4300-line Rust file, against 6.5ms for the first 200 lines of it. So this is
/// a budget spent from the top of the file — the head is coloured and the rest
/// is shown plain, which bounds the work without making the colours depend on
/// how long the file happens to be.
///
/// It was once a ceiling on the *whole* file, which had a subtler cost than it
/// looked: the two previews read different amounts (16KB inline, 1MB in the
/// pane), so a 600KB source file came out coloured in one and grey in the
/// other. What is on screen should not change with how much was read behind it.
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

/// Largest file sent to iTerm2 in its own format rather than re-rendered.
///
/// Past this a re-render at the pane's size is smaller than the original, so
/// sending the file whole stops being the cheaper option.
const MAX_NATIVE_IMAGE_BYTES: u64 = 4 * 1024 * 1024;

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
    /// Render into cells rather than a graphics protocol, whatever the terminal
    /// supports.
    ///
    /// The info panel's inline preview sets this. kitty and iTerm2 images are
    /// escape payloads the terminal owns, and the app tracks exactly one such
    /// surface — a second would erase the first on every frame. On kitty it is
    /// worse than that: the delete escape removes every placement at once, so
    /// two surfaces cannot be told apart even in principle without per-image
    /// ids, which nothing here assigns.
    pub cells_only: bool,
    /// Show an archive's members by name alone, without the permission, size
    /// and timestamp columns.
    ///
    /// Set by the info panel's inline preview. Those columns are laid out
    /// *before* the name, so in a box a fraction of a panel wide they are the
    /// only part that survives truncation — the listing ends up showing
    /// everything except what it was opened to show.
    pub compact_listing: bool,
    /// Cap on the text read, overriding [`MAX_TEXT_BYTES`]. `None` for the
    /// default.
    ///
    /// A preview that shows a handful of rows has no use for a megabyte: the
    /// read, the UTF-8 conversion and the syntax highlighting are all paid in
    /// full and then thrown away, and on a remote panel the megabyte crosses
    /// the wire on every cursor move.
    pub max_text_bytes: Option<u64>,
}

/// Load a preview. Blocking on the calling task; run it on a worker.
pub async fn load(fs: Arc<dyn Vfs>, req: PreviewRequest) -> PreviewContent {
    match load_inner(fs, &req).await {
        Ok(content) => content,
        // The whole chain, not just the outer context: a failure wrapped in
        // "could not read x.rar" printed exactly that and dropped the part
        // saying why, which was the only half worth showing.
        Err(e) => PreviewContent::Note {
            message: format!("Could not read this file: {}", crate::app::explain_error(&e)),
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

    // An archive previews as its table of contents. This has to come before the
    // text sniff below, which would stop at "Binary file" — true, and useless:
    // what someone wants to know about an archive is what is in it.
    if let Some(by_name) = crate::vfs::archive::archive_format(label) {
        let name = label
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| label.display().to_string());
        // The extension is a guess; the container's first bytes settle it. A
        // `.cbr` is routinely a zip — the extension means "comic book rar" and
        // is handed out by tools that wrote a zip — and listing it as a rar
        // fails with a signature error that reads as the file being broken.
        //
        // Read through the `Vfs` rather than from disk, so this works on a
        // remote panel and inside another archive, where there is no local
        // path to open.
        let head = read_head(
            fs.clone(),
            path,
            crate::vfs::archive::format::SNIFF_LEN as u64,
        )
        .await
        .unwrap_or_default();
        let format = match crate::vfs::archive::format::sniff_format(&head) {
            Some(by_content) if by_content != by_name => by_content,
            _ => by_name,
        };
        let detail = if req.compact_listing {
            crate::vfs::archive::listing::Detail::NamesOnly
        } else {
            crate::vfs::archive::listing::Detail::Full
        };
        return crate::vfs::archive::listing::preview(fs, path, format, &name, detail).await;
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

    let want = meta.len.min(req.max_text_bytes.unwrap_or(MAX_TEXT_BYTES));

    // Sniff before committing to the full read. Only the first `SNIFF_BYTES`
    // decide whether this is text at all, so reading a megabyte first is a
    // megabyte thrown away for every binary file — and inside an archive that
    // megabyte has to be *decompressed*, which took 758ms for an .mp4 whose
    // preview is one line of text saying it is binary.
    let head = read_head(fs.clone(), path, want.min(SNIFF_BYTES as u64)).await?;
    if !filetype::looks_like_text(&head) {
        return Ok(PreviewContent::Note {
            message: format!(
                "Binary file ({}). Press o to open it with the default application.",
                crate::utils::sizes::format_size(meta.len)
            ),
        });
    }

    // Text, so the rest is worth having. Re-read from the start rather than
    // stitching: a second range read is cheaper than the machinery to join two,
    // and the sniff is at most 8KB of it.
    let bytes = if want > head.len() as u64 {
        read_head(fs, path, want).await?
    } else {
        head
    };

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

/// Send an image's own bytes to iTerm2, skipping the renderer.
///
/// Returns `None` when the file is not worth sending whole — too large to be
/// cheaper than a re-render, or unreadable — so the caller falls back.
async fn native_iterm_image(
    fs: Arc<dyn Vfs>,
    req: &PreviewRequest,
) -> anyhow::Result<Option<PreviewContent>> {
    let meta = fs.stat(&req.path).await?;
    // Past this the file is no longer smaller than what timg would produce, and
    // a re-render at the pane's size is the better trade.
    if meta.len > MAX_NATIVE_IMAGE_BYTES {
        return Ok(None);
    }
    // base64 costs a third on top, and a multiplexer will not carry an
    // arbitrarily large inline image. Over the limit, hand back to the renderer,
    // which can shrink the picture until it fits — sending the original whole
    // would just produce a blank space.
    let encoded = meta.len.saturating_mul(4) / 3;
    if std::env::var_os("TMUX").is_some()
        && !graphics::payload_fits(encoded as usize, true)
    {
        return Ok(None);
    }
    let bytes = read_head(fs, &req.path, meta.len).await?;
    if bytes.len() as u64 != meta.len {
        // A short read would send a truncated image; let the renderer handle it.
        return Ok(None);
    }

    Ok(Some(PreviewContent::Graphics {
        payload: graphics::iterm_escape_for_file(&bytes, req.cols, req.rows),
        rows: req.rows,
        backend: "iterm2",
        page: 0,
        // Not a paged format: the ones that are do not come through here.
        pages: None,
    }))
}

/// Read up to `len` bytes from the start of a file.
///
/// SFTP backends get a positioned read, which is a single round trip. Note the
/// `Vfs` contract: a short read is legal at any point, not only at EOF, so this
/// loops until it has what it asked for or the file ends.
pub(crate) async fn read_head(fs: Arc<dyn Vfs>, path: &VPath, len: u64) -> anyhow::Result<Vec<u8>> {
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

    // Asked for once and used for every decision below, so a request that wants
    // cells cannot take a graphics path by one branch reading the global and
    // another reading the request.
    let protocol = if req.cells_only {
        graphics::Protocol::Blocks
    } else {
        graphics::protocol()
    };

    // iTerm2 decodes the file itself, so for a format it understands the file's
    // own bytes go straight over and the renderer is skipped entirely.
    //
    // This is a size fix, not a shortcut. timg re-encodes everything as PNG, and
    // a photograph is the worst case for PNG: a 537KB JPEG became a 1.7MB PNG,
    // 2.3MB once base64'd. Sending the original costs 716KB, and looks better for
    // not being re-encoded on the way. kitty takes PNG only and sixel is a raster
    // format, so both still go through the renderer.
    //
    // Only when the file is small enough to survive the trip: inside a
    // multiplexer an oversized inline image is silently not drawn, and the
    // renderer can shrink where sending the original cannot.
    if protocol == graphics::Protocol::Iterm2 && graphics::iterm_decodes_natively(label) {
        if let Some(content) = native_iterm_image(fs.clone(), req).await? {
            return Ok(content);
        }
    }

    let Some(backend) = caps.backend_for(label) else {
        return Ok(PreviewContent::Note {
            message: caps.explain_missing(label),
        });
    };

    // The renderers take a path, so anything that is not already a file on this
    // machine has to be written out to one first. That includes an archive
    // member, which is local but lives inside a container the renderer cannot
    // open — it is staged, but none of the *network* costs below apply to it.
    let staged = if req.path.is_local() {
        None
    } else {
        let over_network = fs.is_remote();
        let meta = fs.stat(&req.path).await?;
        // Only a real download is worth refusing. Extracting an 80MB PDF from an
        // archive is a local read, and capping it reported "too large to fetch"
        // about a file there was nothing to fetch.
        if over_network && meta.len > MAX_REMOTE_IMAGE_BYTES {
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
            // The renderer brackets the image with cursor hiding, mode setting
            // and a trailing newline. Inside a TUI that framing scrolls the pane
            // and un-hides the cursor, so only the image data goes through.
            // Pin the image to the cells the pane reserved. timg describes it in
            // pixels and lets the terminal pick a row count, which is how an
            // image ends up taller than its pane — and the overflow lands on rows
            // nothing repaints, so it also outlives the preview.
            let payload = graphics::pin_to_cells(graphics::strip_framing(&text), cols, rows);
            // A payload that hit the output cap is a half-written escape, and
            // unlike a block render it goes straight to the terminal — sending a
            // truncated graphics sequence leaves it waiting for a terminator that
            // never comes, which swallows whatever is printed next.
            if !payload.is_empty() && !graphics::is_complete(&payload) {
                return Ok(PreviewContent::Note {
                    message: "This image is too large to preview at this size."
                        .to_string(),
                });
            }
            if payload.is_empty() {
                PreviewContent::Note {
                    message: format!("{} produced no output.", backend.binary()),
                }
            } else {
                PreviewContent::Graphics {
                    payload,
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

    // No syntax at all — nothing to colour with.
    let Some(syntax) = syntax else {
        return plain_lines(text);
    };

    let mut h = HighlightLines::new(syntax, theme);
    let mut out = Vec::new();
    // Tabs are expanded before highlighting rather than after. A tab that reaches
    // the buffer is one cell as far as ratatui is concerned but advances the real
    // cursor to the next tab stop, so the two disagree about where every
    // subsequent character sits — the pane's border ends up drawn over, and the
    // damage persists as the frame is diffed against a buffer that never matched
    // the screen. Expanding first also keeps the highlighter's byte offsets
    // aligned with what is finally drawn.
    let expanded;
    let text = if text.contains('\t') {
        expanded = expand_text_tabs(text);
        expanded.as_str()
    } else {
        text
    };
    // Highlight the head and leave the rest plain, rather than dropping colour
    // for the whole file once it passes the cap.
    //
    // syntect's cost is linear, so a megabyte of source cannot be highlighted
    // responsively — but the reason to cap it is the *time*, and time is only
    // spent on the lines actually processed. Colouring the first 64KB of a
    // 600KB file costs ~2ms and gives a reader the part they are looking at;
    // refusing outright gave them a wall of grey and, worse, made a file's
    // appearance depend on its total size rather than on anything visible.
    // (That is how this was found: `integration.rs` was coloured in the info
    // panel's 16KB preview and plain in the full pane's 1MB one.)
    let mut budget = MAX_HIGHLIGHT_BYTES;
    for line in LinesWithEndings::from(text) {
        if budget == 0 {
            // One line in, one line out — `plain_lines` would allocate a `Vec`
            // per line, and past the budget there may be tens of thousands.
            out.push(Line::from(Span::raw(expand_tabs(strip_eol(line)))));
            continue;
        }
        budget = budget.saturating_sub(line.len());
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

/// Expand tabs across a whole document, preserving line endings.
///
/// Per line, because a tab stop is measured from the start of its own line.
fn expand_text_tabs(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for line in syntect::util::LinesWithEndings::from(text) {
        let (body, eol) = match line.strip_suffix('\n') {
            Some(b) => (b, "\n"),
            None => (line, ""),
        };
        out.push_str(&expand_tabs(body));
        out.push_str(eol);
    }
    out
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

    /// A large source file is highlighted as far as the budget goes and shown
    /// plain after that — syntect's cost is linear, so a megabyte cannot be
    /// coloured responsively, but the part someone is reading can be.
    ///
    /// It used to refuse outright, which made a file's appearance depend on its
    /// total size rather than on anything on screen: `integration.rs` came out
    /// coloured in the info panel's 16KB preview and grey in the full pane's
    /// 1MB one.
    #[test]
    fn a_large_file_is_highlighted_up_to_the_budget() {
        let big = "fn f() { let x = 1; }\n".repeat(20_000);
        assert!(big.len() > MAX_HIGHLIGHT_BYTES);

        let t = std::time::Instant::now();
        let lines = highlight(Path::new("big.rs"), &big);
        let big_elapsed = t.elapsed();

        assert_eq!(lines.len(), 20_000, "every line must still be shown");
        assert!(
            lines[0].spans.iter().any(|s| s.style.fg.is_some()),
            "the head of a large file should still be coloured"
        );
        // Past the budget it goes plain, which is what keeps this bounded.
        assert!(
            lines[19_999].spans.iter().all(|s| s.style.fg.is_none()),
            "the tail should be plain once the budget is spent"
        );

        // The cost is bounded by the *budget*, not by the file, so a file ten
        // times longer must not take ten times as long. Compared against a
        // budget-sized input rather than a wall-clock limit: syntect is about a
        // thousand times slower in debug than in release, so any absolute
        // number is either meaningless in one profile or flaky in the other.
        let budgeted: String = big.chars().take(MAX_HIGHLIGHT_BYTES).collect();
        let t = std::time::Instant::now();
        let _ = highlight(Path::new("small.rs"), &budgeted);
        let budget_elapsed = t.elapsed();

        assert!(
            big_elapsed < budget_elapsed * 3 + std::time::Duration::from_millis(200),
            "a 20x larger file cost {big_elapsed:?} against {budget_elapsed:?} for one \
             budget's worth — the budget is not bounding the work"
        );
    }

    /// The same file must look the same however much of it was read. The two
    /// previews read different amounts (16KB inline, 1MB in the pane), and a
    /// cap on the *whole* file meant the smaller read was coloured and the
    /// larger one was not.
    #[test]
    fn a_head_and_a_fuller_read_of_one_file_agree() {
        let src = "fn f() { let x = 1; }\n".repeat(20_000);
        let head: String = src.chars().take(16 * 1024).collect();

        let from_head = highlight(Path::new("big.rs"), &head);
        let from_full = highlight(Path::new("big.rs"), &src);

        let coloured = |l: &Line| l.spans.iter().any(|s| s.style.fg.is_some());
        assert!(coloured(&from_head[0]), "the short read lost its colour");
        assert!(
            coloured(&from_full[0]),
            "the same first line is plain when more of the file is read"
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

    /// A tab that survives into the buffer is one cell as far as ratatui is
    /// concerned, but the real cursor jumps to the next tab stop — so every
    /// character after it sits somewhere the buffer does not expect, the pane
    /// border is drawn over, and the damage persists because later frames are
    /// diffed against a buffer that never matched the screen.
    ///
    /// The highlighted path used to pass syntect's pieces through untouched,
    /// which is how tab-indented source (851 of the 1450 lines of Markdown.pl)
    /// broke the pane.
    #[test]
    fn highlighted_code_never_carries_a_raw_tab() {
        // Tab-indented Perl, the shape that broke: a tab, then content, then
        // tabs used to align a trailing comment.
        let src = "sub f {\n\tmy $x = qr{\n\t (?>\t\t\t# comment\n\t)*\n\t}x;\n}\n";
        assert!(src.contains('\t'));

        for name in ["t.pl", "t.rs", "t.py", "t.c", "t.unknown"] {
            for line in highlight(Path::new(name), src) {
                for span in &line.spans {
                    assert!(
                        !span.content.contains('\t'),
                        "{name}: raw tab reached the buffer in {:?}",
                        span.content
                    );
                }
            }
        }
    }

    /// Expanding must not disturb the text itself, only the tabs.
    #[test]
    fn expanding_tabs_preserves_every_line() {
        let src = "a\n\tb\n\t\tc\nd\n";
        let out = expand_text_tabs(src);
        assert_eq!(out.lines().count(), src.lines().count());
        assert_eq!(
            out.lines().map(|l| l.trim().to_string()).collect::<Vec<_>>(),
            src.lines().map(|l| l.trim().to_string()).collect::<Vec<_>>(),
        );
        // Each tab stop is measured from the start of its own line.
        assert_eq!(expand_text_tabs("\tx\n\tx\n"), "    x\n    x\n");
        // A file with no tabs is returned unchanged.
        assert_eq!(expand_text_tabs("plain\n"), "plain\n");
        // A final line with no newline must not gain one.
        assert_eq!(expand_text_tabs("\ta"), "    a");
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

    /// A request for a text file, at whatever read cap.
    fn text_request(path: &Path, max_text_bytes: Option<u64>) -> PreviewRequest {
        PreviewRequest {
            path: crate::vfs::VPath::local(path.to_path_buf()),
            label: path.to_path_buf(),
            cols: 40,
            rows: 10,
            page: 0,
            cells_only: false,
            compact_listing: false,
            max_text_bytes,
        }
    }

    /// The cap has to bound the read, not just the display: the inline preview
    /// sets it so that following the cursor does not read a megabyte per file.
    #[tokio::test]
    async fn max_text_bytes_limits_the_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.txt");
        // 2000 numbered lines, so which ones arrived is visible in the content.
        let body: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &body).unwrap();

        let reg = crate::vfs::BackendRegistry::new();
        let capped = load(reg.local(), text_request(&path, Some(200))).await;
        let full = load(reg.local(), text_request(&path, None)).await;

        let (capped_lines, full_lines) = match (&capped, &full) {
            (
                PreviewContent::Text { lines: a, .. },
                PreviewContent::Text { lines: b, .. },
            ) => (a.len(), b.len()),
            _ => panic!("expected both to load as text"),
        };

        // 200 bytes is ~25 of these lines; the uncapped read gets all 2000.
        assert!(
            capped_lines < 40,
            "the cap was ignored: {capped_lines} lines from a 200-byte read"
        );
        assert_eq!(full_lines, 2000, "the uncapped read lost lines");

        // A capped read is a partial one, and must say so — the pane draws a
        // marker from this, and claiming a truncated file is complete is worse
        // than showing less of it.
        assert!(
            matches!(capped, PreviewContent::Text { truncated: true, .. }),
            "a capped read must report itself truncated"
        );
        assert!(
            matches!(full, PreviewContent::Text { truncated: false, .. }),
            "a complete read must not report itself truncated"
        );
    }

    /// `cells_only` must not disturb text, which has no protocol to choose.
    #[tokio::test]
    async fn cells_only_leaves_text_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hello.txt");
        std::fs::write(&path, "hello\nworld\n").unwrap();

        let reg = crate::vfs::BackendRegistry::new();
        let mut req = text_request(&path, None);
        req.cells_only = true;
        let content = load(reg.local(), req).await;

        match content {
            PreviewContent::Text { lines, .. } => assert_eq!(lines.len(), 2),
            _ => panic!("expected text"),
        }
    }
}

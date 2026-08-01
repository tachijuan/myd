//! An archive's table of contents, rendered for the preview pane.
//!
//! Deliberately standalone: pressing space on a `.zip` in an ordinary local
//! panel must work with nothing registered, so this takes a [`Vfs`] and a path
//! rather than an opened archive backend. Entering the archive is a separate
//! decision the user may never make.
//!
//! The result is [`PreviewContent::Text`] rather than a variant of its own.
//! That is not laziness — `Text` is the only variant the pane can search with
//! `/` and step through with `n`/`p`, and a listing is exactly the kind of
//! thing someone wants to search.

use std::sync::Arc;

use anyhow::Result;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::preview::PreviewContent;
use crate::utils::sizes::format_size;
use crate::vfs::{VPath, Vfs};
use crate::widget::file_info::format_mode;

use super::format::ArchiveFormat;
use super::index::{ArchiveIndex, Rejection};

/// Largest container that will be read to produce a listing.
///
/// The whole file is held in memory to be parsed. A zip only strictly needs its
/// tail, but reading just that means a seeking reader over the `Vfs`, which is
/// worth building only if this limit turns out to bite; 128MB covers every
/// archive anyone browses casually and is a quarter of what the image preview
/// is willing to stage.
const MAX_LISTING_BYTES: u64 = 128 * 1024 * 1024;

/// Largest number of members shown in a preview.
///
/// The pane is scrolled a line at a time, so a listing longer than this is not
/// something anyone reads to the end of — and it is the pane's memory as well
/// as the index's. The header still reports the true total.
const MAX_PREVIEW_MEMBERS: usize = 20_000;

/// Colour for the permissions column, matching the tree's own.
const MODE_COLOUR: Color = Color::Magenta;
/// Directory rows, matching the tree.
const DIR_COLOUR: Color = Color::Rgb(120, 170, 255);
/// Sizes and ratios: present, but not what the eye should land on.
const QUIET: Color = Color::Rgb(130, 140, 160);
/// A member that could not be indexed.
const WARN: Color = Color::Rgb(255, 140, 90);

/// List an archive's contents as preview text.
pub async fn preview(
    fs: Arc<dyn Vfs>,
    path: &VPath,
    format: ArchiveFormat,
    label: &str,
) -> Result<PreviewContent> {
    let meta = fs.stat(path).await?;
    if meta.len == 0 {
        return Ok(PreviewContent::Note {
            message: "This archive is empty.".to_string(),
        });
    }
    if meta.len > MAX_LISTING_BYTES {
        return Ok(PreviewContent::Note {
            message: format!(
                "This archive is {} — too large to list. Open it with Enter to browse it instead.",
                format_size(meta.len)
            ),
        });
    }

    let bytes = crate::preview::read_head(fs, path, meta.len).await?;

    // Parsing is CPU-bound over a buffer that may be a hundred megabytes; on an
    // async worker it would block every other task sharing that thread.
    let label = label.to_string();
    let content = tokio::task::spawn_blocking(move || {
        let index = super::read_index(&bytes, format, MAX_PREVIEW_MEMBERS)?;
        Ok::<_, anyhow::Error>(render(&index, &label, format))
    })
    .await??;

    Ok(content)
}

/// Turn an index into pane lines.
///
/// Pure, so it is testable without an archive on disk, and so the cost of
/// rendering is separable from the cost of parsing.
pub fn render(index: &ArchiveIndex, label: &str, format: ArchiveFormat) -> PreviewContent {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let total = index.total_len();
    let stored = index.total_compressed();
    let ratio = match stored.checked_mul(100).and_then(|s| s.checked_div(total)) {
        Some(pct) => format!(", {}% of size", pct.min(100)),
        // An archive of nothing but empty files and directories has no ratio to
        // report, and neither does one whose sizes overflow a u64 multiply.
        None => String::new(),
    };
    lines.push(Line::from(Span::styled(
        format!(
            "{label} — {}, {} members, {} uncompressed ({} stored{ratio})",
            format.label(),
            index.declared_members,
            format_size(total),
            format_size(stored),
        ),
        Style::default().add_modifier(Modifier::BOLD),
    )));

    if index.truncated {
        lines.push(warning(format!(
            "Listing truncated at {} of {} members. Open it with Enter to browse all of it.",
            index.member_count(),
            index.declared_members,
        )));
    }
    let unsafe_names = index
        .rejected
        .iter()
        .filter(|(_, r)| *r == Rejection::Unsafe)
        .count();
    if unsafe_names > 0 {
        lines.push(warning(format!(
            "{unsafe_names} entries skipped: names that escape the archive root."
        )));
    }
    let implausible = index.rejected.len() - unsafe_names;
    if implausible > 0 {
        lines.push(warning(format!(
            "{implausible} entries skipped: declared sizes far larger than the archive."
        )));
    }
    lines.push(Line::from(""));

    // Sorted by virtual path so a directory's contents follow it, which is how
    // the tree will show them; the stored name is what is *displayed*, but
    // ordering by it would interleave `./a` and `a` arbitrarily.
    let mut entries: Vec<_> = index.entries().collect();
    entries.sort_by(|a, b| a.path.cmp(&b.path));

    for node in entries {
        let size = if node.is_dir {
            format!("{:>6}", "—")
        } else {
            format_size(node.len)
        };
        let ratio = match (node.is_dir, node.len) {
            (false, len) if len > 0 => {
                format!("{:>4}%", (node.compressed_len * 100 / len).min(999))
            }
            _ => format!("{:>5}", "—"),
        };
        // The stored name is untrusted text from the container. A raw tab or
        // escape would corrupt the pane exactly as one in a source file would —
        // the same hazard `expand_tabs` exists for on the text path.
        let name = sanitise(if node.stored_path.is_empty() {
            // A synthesised directory has no stored name, because the archive
            // never wrote one. Show the path it stands for, marked as ours.
            format!("{}/", node.path.display())
        } else {
            node.stored_path.clone()
        });

        lines.push(Line::from(vec![
            Span::styled(
                format_mode(node.mode, node.is_dir, node.is_symlink),
                Style::default().fg(MODE_COLOUR),
            ),
            Span::raw("  "),
            Span::styled(size, Style::default().fg(QUIET)),
            Span::raw(" "),
            Span::styled(ratio, Style::default().fg(QUIET)),
            Span::raw("  "),
            Span::styled(
                crate::widget::file_info::format_time_fixed(node.mtime),
                Style::default().fg(QUIET),
            ),
            Span::raw("  "),
            Span::styled(
                name,
                if node.is_dir {
                    Style::default().fg(DIR_COLOUR).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                },
            ),
        ]));
    }

    PreviewContent::Text {
        lines,
        truncated: index.truncated,
    }
}

fn warning(message: String) -> Line<'static> {
    Line::from(Span::styled(
        format!("!! {message}"),
        Style::default().fg(WARN),
    ))
}

/// Make a stored name safe to draw.
///
/// Tabs become spaces on the same eight-column stops the text path uses, and
/// every other control character becomes `·`. A raw escape in a `Line` is not a
/// cosmetic problem: it is interpreted by the terminal, and one in a crafted
/// archive could repaint the screen.
fn sanitise(name: String) -> String {
    let expanded = crate::preview::expand_tabs(&name);
    if !expanded.chars().any(|c| c.is_control()) {
        return expanded;
    }
    expanded
        .chars()
        .map(|c| if c.is_control() { '·' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::archive::zip_reader::index_zip;

    fn text_of(content: &PreviewContent) -> Vec<String> {
        match content {
            PreviewContent::Text { lines, .. } => lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect(),
            _ => panic!("expected a Text preview"),
        }
    }

    fn fixture_listing() -> PreviewContent {
        let bytes = crate::vfs::archive::zip_reader::tests::fixture();
        let index = index_zip(&bytes, 10_000).unwrap();
        render(&index, "fixture.zip", ArchiveFormat::Zip)
    }

    #[test]
    fn the_listing_shows_the_stored_path_verbatim() {
        // What the archive says, not what we normalised it to. A member stored
        // as "./docs/x.md" is shown that way — that is what "the full path as
        // stored in the archive" means.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut w = zip::ZipWriter::new(&mut buf);
            let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("./docs/x.md", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let index = index_zip(&buf.into_inner(), 10_000).unwrap();
        let text = text_of(&render(&index, "a.zip", ArchiveFormat::Zip));
        assert!(
            text.iter().any(|l| l.ends_with("./docs/x.md")),
            "stored name should appear verbatim, got {text:?}"
        );
    }

    #[test]
    fn permissions_come_from_the_archive_when_it_has_them() {
        let text = text_of(&fixture_listing());
        let row = text
            .iter()
            .find(|l| l.ends_with("run.sh"))
            .expect("run.sh row");
        assert!(
            row.starts_with("-rwxr-xr-x"),
            "executable member should show its mode, got {row:?}"
        );
    }

    #[test]
    fn an_entry_with_no_recorded_mode_shows_the_placeholder() {
        // Guessing 0644 would invent a fact the archive never stated. A zip
        // whose entries carry no external attributes (some writers, and every
        // directory this app synthesises) must say so instead.
        //
        // Exercised through a synthesised directory, which is the case that
        // reliably has no mode: the zip *writer* always sets one, so a fixture
        // cannot produce a mode-less member, but a real archive can and
        // `unix_mode()` returns `None` for it.
        let text = text_of(&fixture_listing());
        let row = text
            .iter()
            .find(|l| l.ends_with("/docs/deep/"))
            .expect("the synthesised directory");
        assert!(
            row.starts_with("?---------"),
            "a directory the archive never declared has no mode to show, got {row:?}"
        );
    }

    #[test]
    fn the_listing_is_searchable() {
        // Guards the decision to reuse `Text`: any other variant returns an
        // empty search set, so `/` in the pane would silently match nothing.
        let content = fixture_listing();
        let haystack = content.search_text();
        assert!(!haystack.is_empty());
        assert!(haystack.iter().any(|l| l.contains("run.sh")));
    }

    #[test]
    fn a_directory_is_marked_and_has_no_size() {
        let text = text_of(&fixture_listing());
        let row = text
            .iter()
            .find(|l| l.contains("docs/") && l.starts_with('d'))
            .expect("a directory row");
        assert!(row.contains('—'), "a directory has no size of its own");
    }

    #[test]
    fn the_header_reports_the_real_member_count() {
        let text = text_of(&fixture_listing());
        assert!(text[0].contains("fixture.zip"));
        assert!(text[0].contains("zip"));
        assert!(text[0].contains("members"));
    }

    #[test]
    fn skipped_names_are_reported_rather_than_vanishing() {
        // The fixture contains one Zip Slip name. A member that is neither
        // shown nor explained makes the archive look smaller than it is.
        let text = text_of(&fixture_listing());
        assert!(
            text.iter().any(|l| l.starts_with("!!") && l.contains("escape")),
            "expected a warning row, got {text:?}"
        );
    }

    #[test]
    fn a_stored_name_never_carries_a_control_character_into_the_pane() {
        // A crafted archive can name a member with an escape sequence, which
        // the terminal would act on. Same hazard as a raw tab in source text.
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut w = zip::ZipWriter::new(&mut buf);
            let o: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            w.start_file("evil\u{1b}[31mred\ttab.txt", o).unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let index = index_zip(&buf.into_inner(), 10_000).unwrap();
        let text = text_of(&render(&index, "a.zip", ArchiveFormat::Zip));
        for line in &text {
            assert!(
                !line.chars().any(|c| c.is_control()),
                "control character reached the pane in {line:?}"
            );
        }
    }

    #[test]
    fn a_synthesised_directory_is_shown_as_the_path_it_stands_for() {
        // It has no stored name, because the archive never wrote one — but
        // leaving the column blank would look like a rendering fault.
        let text = text_of(&fixture_listing());
        assert!(
            text.iter().any(|l| l.ends_with("/docs/deep/")),
            "expected the synthesised directory to be named, got {text:?}"
        );
    }
}

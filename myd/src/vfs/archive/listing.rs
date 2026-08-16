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

/// Largest *remote* container that will be downloaded to produce a listing.
///
/// A local container is memory-mapped, so its size does not matter and there is
/// no ceiling on listing one. A remote container has nothing to map: listing it
/// means pulling it across the wire into memory first, and doing that silently
/// for a multi-gigabyte file is a surprise rather than a feature. Opening it
/// with `Enter` is refused for the same reason and says so.
const MAX_REMOTE_LISTING_BYTES: u64 = 128 * 1024 * 1024;

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
    detail: Detail,
) -> Result<PreviewContent> {
    // The bsdtar-backed formats hand the file to a child process, so they need a
    // real path a separate process can open. `is_local` is the right question
    // here and not `is_remote`: an archive nested inside another archive is on
    // this machine but has no path of its own, so bsdtar cannot reach it either.
    if format.needs_bsdtar() && !path.is_local() {
        return Ok(PreviewContent::Note {
            message: format!(
                "Listing a {} archive needs it on this machine. Copy it here first (c).",
                format.label()
            ),
        });
    }
    if format.needs_bsdtar() && !super::libarchive_reader::available() {
        return Ok(PreviewContent::Note {
            message: super::libarchive_reader::explain_missing(format),
        });
    }
    let meta = fs.stat(path).await?;
    if meta.len == 0 {
        return Ok(PreviewContent::Note {
            message: "This archive is empty.".to_string(),
        });
    }
    // A remote container has to come across the wire before it can be parsed,
    // and a large one is a download nobody asked for. A local one is mapped
    // below and has no such cost, so no limit applies to it.
    //
    // `is_remote`, not `!is_local`: a nested archive is read from a container on
    // this machine, so it is read into memory rather than mapped but never
    // crosses a network — refusing it would report a download that is not
    // happening.
    if fs.is_remote() && !format.needs_bsdtar() && meta.len > MAX_REMOTE_LISTING_BYTES {
        return Ok(PreviewContent::Note {
            message: format!(
                "This archive is {} and is on a remote panel — listing it would download \
                 it whole. Copy it here (c) first.",
                format_size(meta.len)
            ),
        });
    }

    // The bsdtar formats read the file themselves; only the in-process readers
    // need the bytes, and reading a DVD-sized .iso to list it would be absurd
    // when the tool can seek within it. A local container is mapped, so nothing
    // is read here either — only a remote one is actually pulled across.
    let container = if format.needs_bsdtar() {
        super::Container::empty()
    } else if path.is_local() {
        super::Container::map(path.as_path())?
    } else {
        super::Container::owned(crate::preview::read_head(fs, path, meta.len).await?)
    };

    // Parsing is CPU-bound, and over a mapping it also faults pages in; on an
    // async worker either would block every other task sharing that thread.
    let label = label.to_string();
    let container_path = path.as_path().to_path_buf();
    let content = tokio::task::spawn_blocking(move || {
        let index = super::read_index(&container, &container_path, format, MAX_PREVIEW_MEMBERS)?;
        Ok::<_, anyhow::Error>(render_with_detail(&index, &label, format, detail))
    })
    .await??;

    Ok(content)
}

/// How much of each member to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    /// Permissions, size, ratio, timestamp, then the name — for the full pane,
    /// which has the width for all of it.
    Full,
    /// The name alone.
    ///
    /// For the info panel's sub-panel, which is a fraction of a panel wide. The
    /// columns are laid out before the name, so in a narrow box they are the
    /// only thing that survives truncation and the listing shows everything
    /// *except* what someone opened it to see.
    NamesOnly,
}

/// Turn an index into pane lines.
///
/// Pure, so it is testable without an archive on disk, and so the cost of
/// rendering is separable from the cost of parsing.
pub fn render(index: &ArchiveIndex, label: &str, format: ArchiveFormat) -> PreviewContent {
    render_with_detail(index, label, format, Detail::Full)
}

/// As [`render`], showing as much of each member as `detail` asks for.
pub fn render_with_detail(
    index: &ArchiveIndex,
    label: &str,
    format: ArchiveFormat,
    detail: Detail,
) -> PreviewContent {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let total = index.total_len();
    let n = index.declared_members;
    // Only a zip compresses its members individually, so only a zip can say
    // what each one cost. A tar's members are stored whole and the compression,
    // where there is any, is applied to the container afterwards — quoting a
    // per-member total there would report the uncompressed size twice and call
    // the second one "stored".
    let stored = if format == ArchiveFormat::Zip {
        let compressed = index.total_compressed();
        match compressed.checked_mul(100).and_then(|c| c.checked_div(total)) {
            Some(pct) => format!(", {} stored, {pct}%", format_size(compressed)),
            // Nothing but empty files and directories: no ratio to report.
            None => String::new(),
        }
    } else {
        String::new()
    };
    // The narrow box gets a header it can actually fit: the full one runs to
    // sixty-odd columns and would be truncated mid-word, taking the member
    // count with it.
    let header = match detail {
        Detail::Full => format!(
            "{label} — {}, {n} member{}, {} uncompressed{stored}",
            format.label(),
            if n == 1 { "" } else { "s" },
            format_size(total),
        ),
        Detail::NamesOnly => format!(
            "{n} member{}, {}",
            if n == 1 { "" } else { "s" },
            format_size(total),
        ),
    };
    lines.push(Line::from(Span::styled(
        header,
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

    let per_member_ratios = format == ArchiveFormat::Zip;

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
        // Per-member only where the format stores one. A tar's rows would all
        // read 100%, which looks like a measurement rather than the absence of
        // one.
        let ratio = match (per_member_ratios, node.is_dir, node.len) {
            (true, false, len) if len > 0 => {
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

        let name_style = if node.is_dir {
            Style::default().fg(DIR_COLOUR).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(match detail {
            Detail::NamesOnly => Line::from(Span::styled(name, name_style)),
            Detail::Full => Line::from(vec![
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
                Span::styled(name, name_style),
            ]),
        });
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
    fn a_lone_member_is_not_pluralised() {
        use crate::vfs::archive::format::Compression;
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(b"hello\n").unwrap();
        let gz = enc.finish().unwrap();

        let index =
            crate::vfs::archive::tar_reader::index_single(&gz, "note.txt.gz", Compression::Gzip)
                .unwrap();
        let text = text_of(&render(
            &index,
            "note.txt.gz",
            ArchiveFormat::Single(Compression::Gzip),
        ));
        assert!(text[0].contains("1 member,"), "got {:?}", text[0]);
    }

    #[test]
    fn only_a_zip_claims_per_member_compression() {
        // A tar stores its members whole; the compression, where there is any,
        // wraps the container. Quoting a per-member ratio there would print
        // 100% on every row and read as a measurement rather than its absence.
        let tar = crate::vfs::archive::tar_reader::tests::fixture_tar();
        let index =
            crate::vfs::archive::tar_reader::index_tar(&tar, ArchiveFormat::Tar, 10_000).unwrap();
        let text = text_of(&render(&index, "a.tar", ArchiveFormat::Tar));

        assert!(
            !text[0].contains("stored"),
            "a tar header must not quote a stored total, got {:?}",
            text[0]
        );
        let row = text.iter().find(|l| l.ends_with("run.sh")).unwrap();
        assert!(!row.contains('%'), "got {row:?}");

        // The zip case still does, which is the point of distinguishing them.
        let zip = text_of(&fixture_listing());
        assert!(zip[0].contains("stored"));
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

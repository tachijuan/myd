use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};
use std::path::Path;

use crate::utils::sizes::format_size;

/// Width of the label column, including its trailing space.
///
/// Labels are abbreviated to fit it — the panel can be narrowed to 20 columns,
/// and a label column wider than this leaves nothing for the value.
const LABEL_WIDTH: usize = 7;

/// A field the info panel can edit, when it has focus.
///
/// Only three of the displayed fields are writable; the rest (size, times,
/// path) are facts about the file rather than settings on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InfoField {
    #[default]
    Perms,
    Owner,
    Group,
}

impl InfoField {
    /// The fields in the order they are drawn, which is the order `j`/`k` walk.
    pub const ORDER: [InfoField; 3] = [InfoField::Perms, InfoField::Owner, InfoField::Group];

    /// The label this field is drawn with, and what the edit dialog is titled.
    pub fn label(&self) -> &'static str {
        match self {
            InfoField::Perms => "Perms",
            InfoField::Owner => "Owner",
            InfoField::Group => "Group",
        }
    }

    /// The next field down, stopping at the end rather than wrapping.
    ///
    /// Deliberately not a cycle: this is a three-item list drawn in place, and
    /// `j` running off the bottom back to the top reads as the cursor having
    /// jumped rather than moved.
    pub fn next(&self) -> Self {
        match self {
            InfoField::Perms => InfoField::Owner,
            InfoField::Owner | InfoField::Group => InfoField::Group,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            InfoField::Group => InfoField::Owner,
            InfoField::Owner | InfoField::Perms => InfoField::Perms,
        }
    }
}

/// One `label  value` row.
///
/// The label sits in a fixed dim column so the values line up down the panel,
/// and the whole field fits one row rather than the three (label, value, blank)
/// the panel used to spend — which is what leaves room for the preview beneath.
fn field(label: &str, value: String, style: Style) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!(" {:<width$}", label, width = LABEL_WIDTH),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(value, style),
    ])
}

/// One `label  value` row with a cursor marker, for a field the panel can edit.
///
/// The marker replaces the leading space rather than being inserted before it,
/// so a focused row occupies exactly the columns an unfocused one does and the
/// values do not shift sideways as the cursor moves.
fn editable_field(
    label: &str,
    value: String,
    style: Style,
    marked: bool,
) -> Line<'static> {
    if !marked {
        return field(label, value, style);
    }
    Line::from(vec![
        Span::styled(
            format!("▸{:<width$}", label, width = LABEL_WIDTH),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(value, style.add_modifier(Modifier::BOLD)),
    ])
}

/// Info for an entry on a remote backend.
///
/// A remote path must not be inspected with `std::fs`: it names a file on the
/// server, so local metadata is either missing ("Cannot access") or — for a
/// path like `/var/log` that exists on both machines — silently describes an
/// unrelated local file. Everything here comes from the directory listing the
/// tree already holds.
///
/// Fields the listing doesn't carry (owner, group, creation time) are reported
/// as unavailable rather than guessed at. Permissions *are* carried, so they
/// are shown: the listing already holds the mode bits for the tree's own
/// permissions column, and reading them here costs no round trip.
#[allow(clippy::too_many_arguments)]
pub fn render_remote_info_owned(
    path: &Path,
    is_dir: bool,
    is_symlink: bool,
    size: u64,
    mode: Option<u32>,
    mtime: Option<std::time::SystemTime>,
    atime: Option<std::time::SystemTime>,
    focus: Option<InfoField>,
) -> Text<'static> {
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let type_str = if is_symlink {
        "Symlink"
    } else if is_dir {
        "Directory"
    } else {
        "File"
    };
    let type_color = if is_dir { Color::Blue } else { Color::Cyan };
    let dim = Style::default().fg(Color::DarkGray);
    let marked = |f: InfoField| focus == Some(f);

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            format!(" {}", name),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        field("Type", String::from(type_str), Style::default().fg(type_color)),
        field("Size", format_size(size), Style::default().fg(Color::Yellow)),
        editable_field(
            "Perms",
            format_mode(mode, is_dir, is_symlink),
            Style::default().fg(Color::Magenta),
            marked(InfoField::Perms),
        ),
        // The listing carries no uid or gid, so these are unavailable rather
        // than unknown — but they are still shown, so the rows the cursor can
        // land on are the same set on every backend.
        editable_field("Owner", String::from("—"), dim, marked(InfoField::Owner)),
        editable_field("Group", String::from("—"), dim, marked(InfoField::Group)),
        field("Mod", format_time(mtime), dim),
        field("Acc", format_time(atime), dim),
        field("Path", path.to_string_lossy().to_string(), dim),
    ];

    if is_dir {
        lines.push(Line::from(Span::styled(
            String::from(" Remote sizes are shallow"),
            dim,
        )));
    }

    Text::from(lines)
}

/// Info for an entry on the local filesystem, as fully owned `Text<'static>`.
///
/// `focus` marks the field the panel's cursor is on, or `None` when the panel
/// does not have focus — an unfocused panel draws no cursor at all.
pub fn render_info_owned(
    path: &Path,
    size_cache: &crate::utils::sizes::SizeCache,
    focus: Option<InfoField>,
) -> Text<'static> {
    let metadata = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => {
            return Text::from(vec![Line::from(Span::styled(
                format!("Cannot access '{}'", path.display()),
                Style::default().fg(Color::Red),
            ))]);
        }
    };

    let is_dir = path.is_dir();
    let type_str = if is_dir { "Directory" } else { "File" };
    let type_color = if is_dir { Color::Blue } else { Color::Cyan };

    let size = if is_dir {
        let resolved = path.canonicalize().unwrap_or(path.to_path_buf());
        size_cache.get(&resolved).unwrap_or(0)
    } else {
        metadata.len()
    };

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string_lossy().to_string());

    let perms = format_permissions(&metadata);
    let (owner, group) = get_owner_group(&metadata);
    let dim = Style::default().fg(Color::DarkGray);
    let marked = |f: InfoField| focus == Some(f);

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(
            format!(" {}", name),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        field("Type", String::from(type_str), Style::default().fg(type_color)),
        field("Size", format_size(size), Style::default().fg(Color::Yellow)),
    ];

    if is_dir {
        let (files, dirs) = count_items(path);
        lines.push(field(
            "Items",
            format!("{} files, {} dirs", files, dirs),
            Style::default().fg(Color::Cyan),
        ));
    }

    lines.extend([
        editable_field(
            "Perms",
            perms,
            Style::default().fg(Color::Magenta),
            marked(InfoField::Perms),
        ),
        editable_field(
            "Owner",
            owner,
            Style::default().fg(Color::Cyan),
            marked(InfoField::Owner),
        ),
        editable_field(
            "Group",
            group,
            Style::default().fg(Color::Cyan),
            marked(InfoField::Group),
        ),
        field("Made", format_time(metadata.created().ok()), dim),
        field("Mod", format_time(metadata.modified().ok()), dim),
        field("Acc", format_time(metadata.accessed().ok()), dim),
        field("Path", path.to_string_lossy().to_string(), dim),
    ]);

    Text::from(lines)
}

/// Width of an `ls -l` permission string — the tree's permissions column.
pub const MODE_COL_WIDTH: usize = 10;

/// An `ls -l` permission string built from raw mode bits.
///
/// Takes the bits rather than a `std::fs::Metadata`, so it serves both a local
/// stat and a remote listing (which has no `Metadata` to offer), and is testable
/// on any host. Always [`MODE_COL_WIDTH`] characters: an unknown mode renders as
/// `?---------`, distinguishable from the genuine "no permissions" `----------`
/// by its type character.
pub fn format_mode(mode: Option<u32>, is_dir: bool, is_symlink: bool) -> String {
    let Some(mode) = mode else {
        return "?---------".to_string();
    };
    let file_type = if is_symlink {
        'l'
    } else if is_dir {
        'd'
    } else {
        '-'
    };
    let mut out = String::with_capacity(MODE_COL_WIDTH);
    out.push(file_type);
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        out.push(if bits & 4 != 0 { 'r' } else { '-' });
        out.push(if bits & 2 != 0 { 'w' } else { '-' });
        out.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    out
}

/// Parse a permissions entry: octal (`644`, `0644`) or symbolic (`rw-r--r--`).
///
/// Both forms are accepted because the panel *displays* the symbolic one, and
/// typing back what is on screen has to work. A leading type character is
/// tolerated for the same reason — `-rw-r--r--` is what the panel shows — but
/// it is ignored rather than stored: the file type is not the user's to set.
///
/// Anything else is rejected rather than guessed at. A silently misparsed mode
/// is a permission bug, and the one thing worse than refusing `rwx` is quietly
/// reading it as `0`.
pub fn parse_mode(input: &str) -> Option<u32> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }

    // Octal. `0644` and `644` mean the same thing; `0o644` is accepted too
    // since that is how the same number is written in the source here.
    let octal = s.strip_prefix("0o").unwrap_or(s);
    if octal.chars().all(|c| c.is_ascii_digit()) {
        if octal.chars().any(|c| !('0'..='7').contains(&c)) {
            // A digit outside 0-7 means this was never an octal mode — `999` is
            // a typo, not a permission.
            return None;
        }
        if octal.len() > 4 {
            return None;
        }
        return u32::from_str_radix(octal, 8).ok();
    }

    // Symbolic, with or without the leading type character.
    let body = if s.len() == MODE_COL_WIDTH {
        &s[1..]
    } else {
        s
    };
    if body.len() != 9 {
        return None;
    }

    let mut mode = 0u32;
    for (i, chunk) in body.as_bytes().chunks(3).enumerate() {
        let shift = 6 - i * 3;
        // Each triple is exactly r, w, then x — in that order. A `-` is the
        // absence of that one bit, and anything else is not a mode string.
        let expected = [b'r', b'w', b'x'];
        for (j, (&c, &want)) in chunk.iter().zip(expected.iter()).enumerate() {
            let bit = 4 >> j;
            if c == want {
                mode |= bit << shift;
            } else if c != b'-' {
                // setuid/setgid/sticky are shown in the x column as s/S/t/T.
                // Recognised so a displayed mode round-trips, rather than being
                // rejected as malformed.
                let special = match (i, c) {
                    (0, b's') | (0, b'S') => Some((0o4000, c == b's')),
                    (1, b's') | (1, b'S') => Some((0o2000, c == b's')),
                    (2, b't') | (2, b'T') => Some((0o1000, c == b't')),
                    _ => None,
                };
                match special {
                    // The lowercase forms mean the execute bit is set as well;
                    // the uppercase ones mean it is not.
                    Some((bit_value, also_x)) if j == 2 => {
                        mode |= bit_value;
                        if also_x {
                            mode |= 1 << shift;
                        }
                    }
                    _ => return None,
                }
            }
        }
    }
    Some(mode)
}

/// Look up a user by name, or accept a numeric id outright.
///
/// The inverse of [`uid_to_name`], for a dialog that has to turn what was typed
/// back into the id the filesystem wants. A bare number is taken as an id so an
/// account with no passwd entry can still be named.
#[cfg(unix)]
pub fn name_to_uid(name: &str) -> Option<u32> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Ok(id) = name.parse::<u32>() {
        return Some(id);
    }
    let c_name = std::ffi::CString::new(name).ok()?;
    unsafe {
        let mut result = std::mem::zeroed::<libc::passwd>();
        let mut buffer = [0 as libc::c_char; 1024];
        let mut entry: *mut libc::passwd = std::ptr::null_mut();
        if libc::getpwnam_r(
            c_name.as_ptr(),
            &mut result,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut entry,
        ) == 0
            && !entry.is_null()
        {
            Some((*entry).pw_uid)
        } else {
            None
        }
    }
}

/// Look up a group by name, or accept a numeric id outright.
#[cfg(unix)]
pub fn name_to_gid(name: &str) -> Option<u32> {
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    if let Ok(id) = name.parse::<u32>() {
        return Some(id);
    }
    let c_name = std::ffi::CString::new(name).ok()?;
    unsafe {
        let mut result = std::mem::zeroed::<libc::group>();
        let mut buffer = [0 as libc::c_char; 1024];
        let mut entry: *mut libc::group = std::ptr::null_mut();
        if libc::getgrnam_r(
            c_name.as_ptr(),
            &mut result,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut entry,
        ) == 0
            && !entry.is_null()
        {
            Some((*entry).gr_gid)
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
pub fn name_to_uid(name: &str) -> Option<u32> {
    name.trim().parse().ok()
}

#[cfg(not(unix))]
pub fn name_to_gid(name: &str) -> Option<u32> {
    name.trim().parse().ok()
}

fn format_permissions(metadata: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        format_mode(
            Some(metadata.permissions().mode()),
            metadata.is_dir(),
            metadata.file_type().is_symlink(),
        )
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        "N/A".to_string()
    }
}

fn get_owner_group(metadata: &std::fs::Metadata) -> (String, String) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = metadata.uid();
        let gid = metadata.gid();

        let owner = uid_to_name(uid).unwrap_or_else(|| uid.to_string());
        let group = gid_to_name(gid).unwrap_or_else(|| gid.to_string());
        (owner, group)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        ("N/A".to_string(), "N/A".to_string())
    }
}

#[cfg(unix)]
pub fn uid_to_name(uid: u32) -> Option<String> {
    unsafe {
        let mut result = std::mem::zeroed::<libc::passwd>();
        let mut buffer = [0 as libc::c_char; 1024];
        let mut entry: *mut libc::passwd = std::ptr::null_mut();
        if libc::getpwuid_r(uid, &mut result, buffer.as_mut_ptr(), buffer.len(), &mut entry) == 0
            && !entry.is_null()
        {
            let name = std::ffi::CStr::from_ptr((*entry).pw_name);
            Some(name.to_string_lossy().into_owned())
        } else {
            None
        }
    }
}

#[cfg(unix)]
pub fn gid_to_name(gid: u32) -> Option<String> {
    unsafe {
        let mut result = std::mem::zeroed::<libc::group>();
        let mut buffer = [0 as libc::c_char; 1024];
        let mut entry: *mut libc::group = std::ptr::null_mut();
        if libc::getgrgid_r(gid, &mut result, buffer.as_mut_ptr(), buffer.len(), &mut entry) == 0
            && !entry.is_null()
        {
            let name = std::ffi::CStr::from_ptr((*entry).gr_name);
            Some(name.to_string_lossy().into_owned())
        } else {
            None
        }
    }
}

fn format_time(time: Option<std::time::SystemTime>) -> String {
    use chrono::DateTime;
    match time {
        Some(t) => {
            let dt: DateTime<chrono::Local> = DateTime::from(t);
            dt.format("%Y-%m-%d %H:%M").to_string()
        }
        None => "N/A".to_string(),
    }
}

/// Width of a `%Y-%m-%d %H:%M` timestamp — the tree's time column.
pub const TIME_COL_WIDTH: usize = 16;

/// A timestamp in a fixed-width field, for the tree's aligned column.
///
/// Always [`TIME_COL_WIDTH`] characters. [`format_time`]'s `"N/A"` is three, and
/// in a column it would shift every name on the row.
pub fn format_time_fixed(time: Option<std::time::SystemTime>) -> String {
    match time {
        Some(_) => format_time(time),
        None => format!("{:>width$}", "—", width = TIME_COL_WIDTH),
    }
}

fn count_items(path: &Path) -> (usize, usize) {
    let mut files = 0usize;
    let mut dirs = 0usize;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    dirs += 1;
                } else {
                    files += 1;
                }
            }
        }
    }
    (files, dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_mode_matches_ls() {
        // Takes raw bits, so this is host-independent — no real files involved.
        assert_eq!(format_mode(Some(0o755), true, false), "drwxr-xr-x");
        assert_eq!(format_mode(Some(0o644), false, false), "-rw-r--r--");
        assert_eq!(format_mode(Some(0o777), false, true), "lrwxrwxrwx");
        assert_eq!(format_mode(Some(0o000), false, false), "----------");
        // A symlink's own type wins over the target's directory-ness, as in ls -l.
        assert_eq!(format_mode(Some(0o777), true, true), "lrwxrwxrwx");
        // Unknown is distinguishable from "no permissions" by the type character.
        assert_eq!(format_mode(None, true, false), "?---------");
    }

    #[test]
    fn format_mode_is_always_column_width() {
        // The tree draws this in a fixed column; a short string would shift every
        // name on the row.
        for m in [None, Some(0o000), Some(0o644), Some(0o7777)] {
            assert_eq!(
                format_mode(m, false, false).chars().count(),
                MODE_COL_WIDTH,
                "mode {:?} is not {} chars",
                m,
                MODE_COL_WIDTH
            );
        }
    }

    /// The plain text of a rendered line, for asserting on layout.
    fn text_of(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Which *column* `needle` starts at — not which byte.
    ///
    /// The cursor marker is a 3-byte character, so byte offsets differ between
    /// a marked and an unmarked row that line up perfectly on screen. Columns
    /// are what the panel is aligned in.
    fn column_of(haystack: &str, needle: &str) -> Option<usize> {
        let byte = haystack.find(needle)?;
        Some(haystack[..byte].chars().count())
    }

    /// One row per field is what frees the space the preview needs. The old
    /// layout spent three rows on each (label, value, blank).
    #[test]
    fn a_field_is_one_row_with_the_value_beside_the_label() {
        let line = field("Perms", "-rw-r--r--".to_string(), Style::default());
        let text = text_of(&line);
        assert!(text.contains("Perms"), "no label in {text:?}");
        assert!(text.contains("-rw-r--r--"), "no value in {text:?}");
    }

    /// Values must line up down the panel, so every label occupies the same
    /// columns whatever its length.
    #[test]
    fn labels_share_one_column_width() {
        let short = text_of(&field("Mod", "x".to_string(), Style::default()));
        let long = text_of(&field("Owner", "x".to_string(), Style::default()));
        assert_eq!(
            column_of(&short, "x"),
            column_of(&long, "x"),
            "values start at different columns: {short:?} vs {long:?}"
        );
    }

    /// The cursor marker must not shift the value, or the column jumps sideways
    /// as the cursor moves down it.
    #[test]
    fn the_cursor_marker_does_not_move_the_value() {
        let plain = text_of(&editable_field(
            "Perms",
            "644".to_string(),
            Style::default(),
            false,
        ));
        let marked = text_of(&editable_field(
            "Perms",
            "644".to_string(),
            Style::default(),
            true,
        ));
        assert_eq!(
            column_of(&plain, "644"),
            column_of(&marked, "644"),
            "the marker moved the value: {plain:?} vs {marked:?}"
        );
        assert!(marked.starts_with('▸'), "no cursor marker in {marked:?}");
        assert!(!plain.starts_with('▸'), "unfocused row drew a marker");
    }

    /// An unfocused panel draws no cursor anywhere — it looks exactly as it did
    /// before the panel became focusable.
    #[test]
    fn no_field_is_marked_without_focus() {
        let text = render_remote_info_owned(
            Path::new("/srv/data.txt"),
            false,
            false,
            10,
            Some(0o644),
            None,
            None,
            None,
        );
        assert!(
            !text.lines.iter().any(|l| text_of(l).contains('▸')),
            "an unfocused panel drew a cursor"
        );
    }

    /// The focused field, and only that one, carries the marker.
    #[test]
    fn exactly_one_field_is_marked_when_focused() {
        let text = render_remote_info_owned(
            Path::new("/srv/data.txt"),
            false,
            false,
            10,
            Some(0o644),
            None,
            None,
            Some(InfoField::Owner),
        );
        let marked: Vec<String> = text
            .lines
            .iter()
            .map(|l| text_of(l))
            .filter(|t| t.contains('▸'))
            .collect();
        assert_eq!(marked.len(), 1, "expected one marked row, got {marked:?}");
        assert!(marked[0].contains("Owner"), "wrong row marked: {marked:?}");
    }

    /// The listing already carries mode bits for the tree's permissions column,
    /// so the remote panel can show them without a round trip. It used to omit
    /// the field entirely.
    #[test]
    fn a_remote_entry_shows_its_permissions() {
        let text = render_remote_info_owned(
            Path::new("/srv/data.txt"),
            false,
            false,
            10,
            Some(0o640),
            None,
            None,
            None,
        );
        let body: String = text.lines.iter().map(|l| text_of(l)).collect();
        assert!(
            body.contains("-rw-r-----"),
            "no permissions in the remote panel: {body:?}"
        );
    }

    /// A listing without mode bits must say so rather than invent them.
    #[test]
    fn a_remote_entry_without_a_mode_says_unknown() {
        let text = render_remote_info_owned(
            Path::new("/srv/data.txt"),
            false,
            false,
            10,
            None,
            None,
            None,
            None,
        );
        let body: String = text.lines.iter().map(|l| text_of(l)).collect();
        assert!(body.contains("?---------"), "expected unknown mode: {body:?}");
    }

    /// Every field the cursor can reach must be drawn on both backends, or
    /// `j`/`k` would land on a row that is not there.
    #[test]
    fn both_backends_draw_every_editable_field() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        let cache = crate::utils::sizes::SizeCache::default();

        let local: String = render_info_owned(&file, &cache, None)
            .lines
            .iter()
            .map(|l| text_of(l))
            .collect();
        let remote: String = render_remote_info_owned(
            Path::new("/srv/f.txt"),
            false,
            false,
            1,
            Some(0o644),
            None,
            None,
            None,
        )
        .lines
        .iter()
        .map(|l| text_of(l))
        .collect();

        for f in InfoField::ORDER {
            assert!(local.contains(f.label()), "local panel lacks {}", f.label());
            assert!(
                remote.contains(f.label()),
                "remote panel lacks {}",
                f.label()
            );
        }
    }

    /// `j` and `k` stop at the ends rather than wrapping, so the cursor never
    /// appears to jump across the panel.
    #[test]
    fn the_field_cursor_stops_at_both_ends() {
        assert_eq!(InfoField::Perms.prev(), InfoField::Perms);
        assert_eq!(InfoField::Group.next(), InfoField::Group);
        assert_eq!(InfoField::Perms.next(), InfoField::Owner);
        assert_eq!(InfoField::Group.prev(), InfoField::Owner);
    }

    /// The panel displays the symbolic form, so typing back what is on screen
    /// has to work — including the leading type character it draws.
    #[test]
    fn parse_mode_round_trips_what_the_panel_displays() {
        for (mode, is_dir) in [
            (0o644, false),
            (0o755, true),
            (0o600, false),
            (0o777, false),
            (0o000, false),
        ] {
            let shown = format_mode(Some(mode), is_dir, false);
            assert_eq!(
                parse_mode(&shown),
                Some(mode),
                "{shown} did not parse back to {mode:o}"
            );
        }
    }

    #[test]
    fn parse_mode_takes_octal_in_the_usual_spellings() {
        assert_eq!(parse_mode("644"), Some(0o644));
        assert_eq!(parse_mode("0644"), Some(0o644));
        assert_eq!(parse_mode("0o644"), Some(0o644));
        assert_eq!(parse_mode(" 755 "), Some(0o755));
        assert_eq!(parse_mode("7"), Some(0o7));
        // setuid, setgid and sticky are four digits.
        assert_eq!(parse_mode("4755"), Some(0o4755));
    }

    #[test]
    fn parse_mode_takes_symbolic_with_or_without_the_type_character() {
        assert_eq!(parse_mode("rw-r--r--"), Some(0o644));
        assert_eq!(parse_mode("-rw-r--r--"), Some(0o644));
        assert_eq!(parse_mode("drwxr-xr-x"), Some(0o755));
        assert_eq!(parse_mode("rwxrwxrwx"), Some(0o777));
        assert_eq!(parse_mode("---------"), Some(0));
    }

    /// The special bits are displayed in the x column, so they have to parse
    /// from there too or a displayed mode would not round-trip.
    #[test]
    fn parse_mode_understands_the_special_bits() {
        // Lowercase means the execute bit is set as well; uppercase means it is
        // not — which is exactly how ls writes them.
        assert_eq!(parse_mode("rwsr-xr-x"), Some(0o4755));
        assert_eq!(parse_mode("rwSr--r--"), Some(0o4644));
        assert_eq!(parse_mode("rwxr-sr-x"), Some(0o2755));
        assert_eq!(parse_mode("rwxr-xr-t"), Some(0o1755));
    }

    /// A misparsed mode is a permission bug, so anything ambiguous is refused
    /// rather than guessed at.
    #[test]
    fn parse_mode_rejects_what_it_cannot_read() {
        for bad in [
            "",
            "   ",
            "qqq",
            "999",       // not octal
            "888",       // not octal
            "rw-r--r",   // too short
            "rw-r--r---", // too long
            "xw-r--r--", // r/w/x out of order
            "rw-r--rw",  // eight characters
            "12345",     // too many digits
            "-1",
            "0o999",
        ] {
            assert_eq!(parse_mode(bad), None, "{bad:?} should not parse");
        }
    }

    /// A bare number is an id, so an account with no passwd entry can still be
    /// named.
    #[test]
    fn a_numeric_id_is_accepted_as_itself() {
        assert_eq!(name_to_uid("0"), Some(0));
        assert_eq!(name_to_gid("0"), Some(0));
        assert_eq!(name_to_uid("4242"), Some(4242));
    }

    #[test]
    fn an_unknown_name_does_not_resolve() {
        assert_eq!(name_to_uid("definitely-no-such-user-xyzzy"), None);
        assert_eq!(name_to_gid("definitely-no-such-group-xyzzy"), None);
        assert_eq!(name_to_uid(""), None);
        assert_eq!(name_to_gid("   "), None);
    }

    /// The lookups have to be inverses, or the dialog would resolve a name to
    /// an id the panel then displays as a different name.
    #[cfg(unix)]
    #[test]
    fn the_name_and_id_lookups_are_inverses() {
        // root exists on every unix this runs on.
        let uid = name_to_uid("root").expect("root should resolve");
        assert_eq!(uid, 0);
        assert_eq!(uid_to_name(0).as_deref(), Some("root"));

        // Whatever this process is, its own name must round-trip.
        let me = unsafe { libc::getuid() };
        if let Some(name) = uid_to_name(me) {
            assert_eq!(name_to_uid(&name), Some(me), "{name} did not round-trip");
        }
    }

    #[test]
    fn format_time_fixed_is_always_column_width() {
        // `format_time` returns "N/A" for None, which is 3 chars and would break
        // the column — the whole reason format_time_fixed exists.
        assert_eq!(
            format_time_fixed(None).chars().count(),
            TIME_COL_WIDTH,
            "a missing time must still fill the column"
        );
        assert_eq!(
            format_time_fixed(Some(std::time::SystemTime::UNIX_EPOCH))
                .chars()
                .count(),
            TIME_COL_WIDTH
        );
    }
}

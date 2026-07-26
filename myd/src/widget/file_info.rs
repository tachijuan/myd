use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
    layout::Rect,
    Frame,
};
use std::path::Path;

use crate::utils::sizes::format_size;

pub fn render_info<'a>(path: &'a Path, size_cache: &'a crate::utils::sizes::SizeCache) -> Text<'a> {
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

    let created = format_time(metadata.created().ok());
    let modified = format_time(metadata.modified().ok());
    let accessed = format_time(metadata.accessed().ok());

    let mut lines: Vec<Line<'a>> = vec![
        Line::from(Span::styled(
            format!("  {}", name),
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Type:"),
        Line::from(Span::styled(type_str.to_string(), Style::default().fg(type_color))),
        Line::from(""),
        Line::from("Size:"),
        Line::from(Span::styled(
            format_size(size),
            Style::default().fg(Color::Yellow),
        )),
    ];

    if is_dir {
        let (files, dirs) = count_items(path);
        lines.extend([
            Line::from(""),
            Line::from("Contents:"),
            Line::from(Span::styled(
                format!("{} files, {} directories", files, dirs),
                Style::default().fg(Color::Cyan),
            )),
        ]);
    }

    lines.extend([
        Line::from(""),
        Line::from("Permissions:"),
        Line::from(Span::styled(perms, Style::default().fg(Color::Magenta))),
        Line::from(""),
        Line::from("Owner:"),
        Line::from(Span::styled(owner, Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from("Group:"),
        Line::from(Span::styled(group, Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from("Created:"),
        Line::from(Span::styled(created, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Modified:"),
        Line::from(Span::styled(modified, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Accessed:"),
        Line::from(Span::styled(accessed, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Path:"),
        Line::from(Span::styled(
            path.to_string_lossy().to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ]);

    Text::from(lines)
}

pub fn render_info_panel(frame: &mut Frame, area: Rect, path: &Path, size_cache: &crate::utils::sizes::SizeCache) {
    let text = render_info(path, size_cache);
    let paragraph = Paragraph::new(text).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Plain)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Info "),
    );
    frame.render_widget(paragraph, area);
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
/// as unavailable rather than guessed at.
pub fn render_remote_info_owned(
    path: &Path,
    is_dir: bool,
    is_symlink: bool,
    size: u64,
    mtime: Option<std::time::SystemTime>,
    atime: Option<std::time::SystemTime>,
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

    let unknown = || Span::styled(String::from("—"), Style::default().fg(Color::DarkGray));

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::raw(format!("  {}", name))),
        Line::from(""),
        Line::from("Type:"),
        Line::from(Span::styled(
            String::from(type_str),
            Style::default().fg(type_color),
        )),
        Line::from(""),
        Line::from("Size:"),
        Line::from(Span::styled(
            format_size(size),
            Style::default().fg(Color::Yellow),
        )),
        Line::from(""),
        Line::from("Modified:"),
        Line::from(Span::styled(
            format_time(mtime),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from("Accessed:"),
        Line::from(Span::styled(
            format_time(atime),
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(""),
        Line::from("Owner:"),
        Line::from(unknown()),
        Line::from(""),
        Line::from("Path:"),
        Line::from(Span::styled(
            path.to_string_lossy().to_string(),
            Style::default().fg(Color::DarkGray),
        )),
    ];

    if is_dir {
        lines.extend([
            Line::from(""),
            Line::from(Span::styled(
                String::from("Remote directory sizes are shallow"),
                Style::default().fg(Color::DarkGray),
            )),
        ]);
    }

    Text::from(lines)
}

/// Like `render_info` but returns fully owned `Text<'static>` with no borrows from `path`.
pub fn render_info_owned(path: &Path, size_cache: &crate::utils::sizes::SizeCache) -> Text<'static> {
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

    let created = format_time(metadata.created().ok());
    let modified = format_time(metadata.modified().ok());
    let accessed = format_time(metadata.accessed().ok());

    let mut lines: Vec<Line<'static>> = vec![
        Line::from(Span::raw(format!("  {}", name))),
        Line::from(""),
        Line::from("Type:"),
        Line::from(Span::styled(String::from(type_str), Style::default().fg(type_color))),
        Line::from(""),
        Line::from("Size:"),
        Line::from(Span::styled(
            format_size(size),
            Style::default().fg(Color::Yellow),
        )),
    ];

    if is_dir {
        let (files, dirs) = count_items(path);
        lines.extend([
            Line::from(""),
            Line::from("Contents:"),
            Line::from(Span::styled(
                format!("{} files, {} directories", files, dirs),
                Style::default().fg(Color::Cyan),
            )),
        ]);
    }

    lines.extend([
        Line::from(""),
        Line::from("Permissions:"),
        Line::from(Span::styled(perms, Style::default().fg(Color::Magenta))),
        Line::from(""),
        Line::from("Owner:"),
        Line::from(Span::styled(owner, Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from("Group:"),
        Line::from(Span::styled(group, Style::default().fg(Color::Cyan))),
        Line::from(""),
        Line::from("Created:"),
        Line::from(Span::styled(created, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Modified:"),
        Line::from(Span::styled(modified, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Accessed:"),
        Line::from(Span::styled(accessed, Style::default().fg(Color::DarkGray))),
        Line::from(""),
        Line::from("Path:"),
        Line::from(Span::styled(
            path.to_string_lossy().to_string(),
            Style::default().fg(Color::DarkGray),
        )),
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
fn uid_to_name(uid: u32) -> Option<String> {
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
fn gid_to_name(gid: u32) -> Option<String> {
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

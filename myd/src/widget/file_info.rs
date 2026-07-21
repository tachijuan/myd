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

fn format_permissions(metadata: &std::fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        let file_type = if metadata.is_dir() { 'd' }
            else if metadata.file_type().is_symlink() { 'l' }
            else { '-' };

        let mut perms = String::new();
        for shift in [6, 3, 0] {
            let bits = (mode >> shift) & 7;
            perms.push(if bits & 4 != 0 { 'r' } else { '-' });
            perms.push(if bits & 2 != 0 { 'w' } else { '-' });
            perms.push(if bits & 1 != 0 { 'x' } else { '-' });
        }
        format!("{}{}", file_type, perms)
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
        let mut buffer = [0u8; 1024];
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
        let mut buffer = [0u8; 1024];
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

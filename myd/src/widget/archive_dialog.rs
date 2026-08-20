//! Naming a new archive and choosing its format.
//!
//! A text field plus a radio group plus buttons, following
//! [`crate::widget::attr_dialog`] — which is the open dialog's field and the
//! confirm dialog's buttons put together, with one extra control between them.
//! Copying it keeps `Tab`, `Enter`, `Esc` and clicks meaning what they already
//! mean everywhere else here.
//!
//! The radio group is **one** focus stop, not one per format. Four formats as
//! four stops would put seven stops in a form with three controls, and cross a
//! group that answers a single question in four Tabs. Inside it the arrows
//! move, which is what an arrow does on a list.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use std::path::PathBuf;

use crate::vfs::archive::writer::{with_extension_for, WriteFormat};

/// What the dialog decided this keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveDialogOutcome {
    /// Still editing. The dialog stays up.
    Continue,
    Cancelled,
    /// Create this archive from the captured sources.
    Create { name: String, format: WriteFormat },
}

/// Which part of the dialog has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArchiveFocus {
    Field,
    Format,
    Buttons,
}

/// The buttons, in the order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveButton {
    Create,
    Cancel,
}

pub struct ArchiveDialog {
    name: String,
    /// Char index within `name`, converted to bytes by [`Self::byte_index`].
    cursor: usize,
    focus: ArchiveFocus,
    /// Index into [`WriteFormat::ALL`].
    format: usize,
    /// Index into [`Self::buttons`].
    button: usize,
    /// Whether the name has been edited by hand.
    ///
    /// Until it has, changing the format rewrites the extension to match. After
    /// it has, it does not: someone who typed `backup.tgz` and then tabbed
    /// through the formats to check their choice must not watch the name they
    /// just wrote get rewritten under them.
    name_touched: bool,
    /// The paths this will archive, captured when the dialog opened.
    ///
    /// Held here for the reason the attribute dialog holds its targets: the
    /// dialog said what it was going to act on, and that promise holds even if
    /// the cursor moves behind it.
    sources: Vec<PathBuf>,
    /// The directory the archive will be created in, captured likewise.
    dest_dir: PathBuf,
    /// Rebuilt on every render, so the drawn boxes and their click targets
    /// cannot describe different rectangles.
    button_areas: Vec<Rect>,
    /// Where each format row was drawn, for the same reason.
    format_areas: Vec<Rect>,
}

impl ArchiveDialog {
    /// A dialog creating an archive of `sources` inside `dest_dir`.
    ///
    /// `default_format` comes from the preferences. The name starts as the
    /// destination directory's own name plus that format's extension, so the
    /// common case is one keystroke — the same bargain `AttrDialog::with_value`
    /// makes by pre-filling the current mode.
    pub fn new(sources: Vec<PathBuf>, dest_dir: PathBuf, default_format: WriteFormat) -> Self {
        let format = WriteFormat::ALL
            .iter()
            .position(|f| *f == default_format)
            .unwrap_or(0);
        let stem = sources
            .first()
            .filter(|_| sources.len() == 1)
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .or_else(|| {
                dest_dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "archive".to_string());
        let name = with_extension_for(&stem, default_format);
        let cursor = name.chars().count();
        Self {
            name,
            cursor,
            focus: ArchiveFocus::Field,
            format,
            button: 0,
            name_touched: false,
            sources,
            dest_dir,
            button_areas: Vec::new(),
            format_areas: Vec::new(),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn format(&self) -> WriteFormat {
        WriteFormat::ALL
            .get(self.format)
            .copied()
            .unwrap_or_default()
    }

    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    pub fn dest_dir(&self) -> &PathBuf {
        &self.dest_dir
    }

    /// The buttons this dialog has, in draw order.
    pub fn buttons(&self) -> Vec<ArchiveButton> {
        vec![ArchiveButton::Create, ArchiveButton::Cancel]
    }

    fn label(button: ArchiveButton) -> &'static str {
        match button {
            ArchiveButton::Create => " [ Create ] ",
            ArchiveButton::Cancel => " [ Cancel ] ",
        }
    }

    /// A name with something in it. An empty field has nothing to create, and
    /// Enter on it keeps the dialog rather than closing it having silently done
    /// nothing.
    fn is_creatable(&self) -> bool {
        !self.name.trim().is_empty()
    }

    fn create_outcome(&self) -> ArchiveDialogOutcome {
        if self.is_creatable() {
            ArchiveDialogOutcome::Create {
                name: self.name.trim().to_string(),
                format: self.format(),
            }
        } else {
            ArchiveDialogOutcome::Continue
        }
    }

    /// What pressing `button` decides.
    fn press(&self, button: ArchiveButton) -> ArchiveDialogOutcome {
        match button {
            ArchiveButton::Create => self.create_outcome(),
            ArchiveButton::Cancel => ArchiveDialogOutcome::Cancelled,
        }
    }

    /// Move the format selection, carrying the extension with it.
    fn select_format(&mut self, index: usize) {
        if index >= WriteFormat::ALL.len() {
            return;
        }
        self.format = index;
        if !self.name_touched {
            self.name = with_extension_for(&self.name, self.format());
            self.cursor = self.name.chars().count();
        }
    }

    fn step_format(&mut self, backwards: bool) {
        let n = WriteFormat::ALL.len();
        let next = if backwards {
            (self.format + n - 1) % n
        } else {
            (self.format + 1) % n
        };
        self.select_format(next);
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> ArchiveDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => ArchiveDialogOutcome::Cancelled,

            // Tab moves the focus and never creates anything — the house rule.
            // Reaching for a button must not be read as consent to write a file.
            KeyCode::Tab | KeyCode::BackTab => {
                self.cycle(matches!(key.code, KeyCode::BackTab));
                ArchiveDialogOutcome::Continue
            }

            KeyCode::Enter => match self.focus {
                // Enter in the field creates, as in the attribute dialog: this
                // is a one-value form and requiring a Tab first would be
                // ceremony. The Tab rule still holds, because Tab stays inert.
                ArchiveFocus::Field => self.create_outcome(),
                // On the radio group Enter does nothing. The group's whole
                // purpose is "which one", and the honest answer to Enter there
                // is the selection it already shows. Creating from here would
                // make the group a hidden accept — and writing a file is
                // exactly what must not happen from a stray Enter while
                // looking through the options.
                ArchiveFocus::Format => ArchiveDialogOutcome::Continue,
                ArchiveFocus::Buttons => match self.buttons().get(self.button).copied() {
                    Some(button) => self.press(button),
                    None => ArchiveDialogOutcome::Continue,
                },
            },

            // A digit picks a format outright, but does *not* accept. The sort
            // menu closes on a digit because a sort order is instantly
            // reversible; an archive write is not, so here the digit is a
            // shortcut for the radio rather than for the form.
            //
            // Guarded on the focus so a digit typed into the name is a
            // character: `report-2026.zip` has to be typable.
            KeyCode::Char(c @ '1'..='9')
                if self.focus == ArchiveFocus::Format
                    && (c as usize - '0' as usize) <= WriteFormat::ALL.len() =>
            {
                self.select_format(c as usize - '0' as usize - 1);
                ArchiveDialogOutcome::Continue
            }

            // Space picks the focused format, which is what a radio answers to.
            // Everywhere else it is an ordinary character in the name.
            KeyCode::Char(' ') if self.focus == ArchiveFocus::Format => {
                ArchiveDialogOutcome::Continue
            }

            // Left and right move within whichever control is laid out along
            // them; the field needs them for its own cursor.
            KeyCode::Left if self.focus == ArchiveFocus::Buttons => {
                self.cycle(true);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Right if self.focus == ArchiveFocus::Buttons => {
                self.cycle(false);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Left if self.focus == ArchiveFocus::Format => {
                self.step_format(true);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Right if self.focus == ArchiveFocus::Format => {
                self.step_format(false);
                ArchiveDialogOutcome::Continue
            }
            // Up and down move within the format list, which is drawn
            // vertically — an arrow along a list must move along the list
            // rather than leave it.
            KeyCode::Up if self.focus == ArchiveFocus::Format => {
                self.step_format(true);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Down if self.focus == ArchiveFocus::Format => {
                self.step_format(false);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Up => {
                self.cycle(true);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Down => {
                self.cycle(false);
                ArchiveDialogOutcome::Continue
            }

            // Everything below edits the name. Typing while something else has
            // focus returns to the field and inserts, so a user who tabbed too
            // far and kept typing does not lose the characters.
            KeyCode::Char(c) => {
                self.focus = ArchiveFocus::Field;
                self.name_touched = true;
                let at = self.byte_index();
                self.name.insert(at, c);
                self.cursor += 1;
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Backspace => {
                self.focus = ArchiveFocus::Field;
                self.name_touched = true;
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let at = self.byte_index();
                    self.name.remove(at);
                }
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Delete => {
                self.focus = ArchiveFocus::Field;
                self.name_touched = true;
                let at = self.byte_index();
                if at < self.name.len() {
                    self.name.remove(at);
                }
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Right => {
                if self.cursor < self.name.chars().count() {
                    self.cursor += 1;
                }
                ArchiveDialogOutcome::Continue
            }
            KeyCode::Home => {
                self.cursor = 0;
                ArchiveDialogOutcome::Continue
            }
            KeyCode::End => {
                self.cursor = self.name.chars().count();
                ArchiveDialogOutcome::Continue
            }
            _ => ArchiveDialogOutcome::Continue,
        }
    }

    /// The focus stops, in order: the field, the format group, then each
    /// button.
    ///
    /// The whole group is one stop; see the module note for why.
    fn stops(&self) -> usize {
        2 + self.buttons().len()
    }

    fn current_stop(&self) -> usize {
        match self.focus {
            ArchiveFocus::Field => 0,
            ArchiveFocus::Format => 1,
            ArchiveFocus::Buttons => 2 + self.button.min(self.buttons().len().saturating_sub(1)),
        }
    }

    /// Move the focus one stop, wrapping.
    fn cycle(&mut self, backwards: bool) {
        let stops = self.stops();
        let current = self.current_stop();
        let next = if backwards {
            (current + stops - 1) % stops
        } else {
            (current + 1) % stops
        };
        match next {
            0 => self.focus = ArchiveFocus::Field,
            1 => self.focus = ArchiveFocus::Format,
            n => {
                self.focus = ArchiveFocus::Buttons;
                self.button = n - 2;
            }
        }
    }

    /// The cursor's byte offset in the name.
    ///
    /// A file name can carry multibyte characters; indexing by the char cursor
    /// directly would panic mid-codepoint.
    fn byte_index(&self) -> usize {
        self.name
            .char_indices()
            .nth(self.cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.name.len())
    }

    /// Answer a click at `(x, y)`.
    ///
    /// A click on a button is that button's decision. A click on a format row
    /// selects it and decides nothing else: this dialog writes a file, and a
    /// stray click must not.
    pub fn click_at(&mut self, x: u16, y: u16) -> ArchiveDialogOutcome {
        let inside = |r: &Rect| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height;

        if let Some(i) = self.format_areas.iter().position(inside) {
            self.focus = ArchiveFocus::Format;
            self.select_format(i);
            return ArchiveDialogOutcome::Continue;
        }

        if let Some(i) = self.button_areas.iter().position(inside) {
            if let Some(button) = self.buttons().get(i).copied() {
                self.focus = ArchiveFocus::Buttons;
                self.button = i;
                return self.press(button);
            }
        }
        ArchiveDialogOutcome::Continue
    }

    /// The line describing what this will act on and where it will land.
    fn summary(&self) -> String {
        let where_to = self
            .dest_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.dest_dir.display().to_string());
        match self.sources.len() {
            0 => " Nothing selected".to_string(),
            1 => {
                let name = self.sources[0]
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.sources[0].display().to_string());
                format!(" {name} → {where_to}")
            }
            n => format!(" {n} tagged entries → {where_to}"),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 60.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // summary + blank + label + field + blank + "Format" + one row per
        // format + blank + buttons, plus the two border rows. Derived from the
        // list rather than written out, so a fifth format cannot silently clip.
        let height = (11 + WriteFormat::ALL.len() as u16).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        // Too small for a border plus a row of content. Checked against the
        // borders rather than against zero: a 1x1 box is not empty, but every
        // cell of it is border, so anything "drawn" inside would be recorded at
        // a coordinate outside the box — click rectangles pointing at cells no
        // one can see.
        if center.width <= 2 || center.height <= 2 {
            self.button_areas.clear();
            self.format_areas.clear();
            return;
        }
        frame.render_widget(Clear, center);

        let dim = Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg);
        let normal = Style::default().fg(Color::Rgb(235, 235, 245)).bg(bg);
        let accent = Color::Rgb(120, 220, 255);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            truncate(&self.summary(), inner),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        let field_focused = self.focus == ArchiveFocus::Field;
        lines.push(Line::from(Span::styled(
            " Name".to_string(),
            if field_focused {
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        )));
        let shown = if field_focused {
            let cut = self.byte_index();
            format!("  {}█{}", &self.name[..cut], &self.name[cut..])
        } else {
            format!("  {}", self.name)
        };
        lines.push(Line::from(Span::styled(
            pad_to(&shown, inner),
            if field_focused {
                normal.add_modifier(Modifier::BOLD)
            } else {
                normal
            },
        )));

        lines.push(Line::from(Span::styled(String::new(), normal)));
        let group_focused = self.focus == ArchiveFocus::Format;
        lines.push(Line::from(Span::styled(
            " Format".to_string(),
            if group_focused {
                Style::default()
                    .fg(accent)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        )));

        // The format rows and their click targets are built together, in one
        // pass, for the same reason the buttons below are: two passes can
        // disagree. The first row is the seventh content line (index 6).
        self.format_areas.clear();
        let first_row = center.y + 1 + 6;
        let last_content = center.y + center.height.saturating_sub(1);
        for (i, format) in WriteFormat::ALL.iter().enumerate() {
            // `●` marks the selection and the reversed style marks the cursor.
            // They are different questions — which one is chosen, and which one
            // the keyboard is on — so they get different signals.
            let mark = if i == self.format { "●" } else { "○" };
            let text = format!(
                "  {} {}. {:<6} {}",
                mark,
                i + 1,
                format.label(),
                format.description()
            );
            let focused = group_focused && i == self.format;
            lines.push(Line::from(Span::styled(
                pad_to(&text, inner),
                if focused {
                    Style::default()
                        .fg(accent)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                } else {
                    normal
                },
            )));
            let row = first_row + i as u16;
            if row < last_content {
                self.format_areas.push(Rect::new(
                    center.x + 1,
                    row,
                    center.width.saturating_sub(2),
                    1,
                ));
            }
        }

        lines.push(Line::from(Span::styled(String::new(), normal)));

        // Buttons and their click targets, likewise in one pass.
        let buttons = self.buttons();
        let sep = "  ";
        let mut x = center.x + 1;
        let button_y = center.y + center.height.saturating_sub(2);
        let mut spans: Vec<Span> = Vec::new();
        self.button_areas.clear();
        for (i, button) in buttons.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(sep, normal));
                x += sep.chars().count() as u16;
            }
            let label = Self::label(*button);
            let w = label.chars().count() as u16;
            let focused = self.focus == ArchiveFocus::Buttons && i == self.button;
            let style = if focused {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Yellow).bg(bg)
            };
            spans.push(Span::styled(label, style));
            // Clamped to what is actually inside the box: a narrow terminal
            // truncates the drawn label, and a click rectangle wider than the
            // cells it was drawn on would catch clicks on whatever is beside
            // the dialog.
            let right_edge = center.x + center.width.saturating_sub(1);
            if x < right_edge {
                self.button_areas
                    .push(Rect::new(x, button_y, w.min(right_edge - x), 1));
            }
            x += w;
        }

        // The buttons sit on the last content row. Pad the lines before them so
        // the box is filled to that row whatever the terminal height allowed.
        let content_rows = center.height.saturating_sub(2) as usize;
        while lines.len() + 1 < content_rows {
            lines.push(Line::from(Span::styled(String::new(), normal)));
        }
        lines.truncate(content_rows.saturating_sub(1));
        lines.push(Line::from(spans));

        let paragraph = Paragraph::new(Text::from(lines))
            .style(Style::default().bg(bg))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Create archive ",
                        Style::default().fg(accent).add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(accent))
                    .style(Style::default().bg(bg)),
            );
        frame.render_widget(paragraph, center);
    }
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        return s.to_string();
    }
    let keep = width.saturating_sub(1);
    let mut out: String = s.chars().take(keep).collect();
    out.push('…');
    out
}

fn pad_to(s: &str, width: usize) -> String {
    let len = s.chars().count();
    if len >= width {
        return s.chars().take(width).collect();
    }
    let mut out = s.to_string();
    out.extend(std::iter::repeat_n(' ', width - len));
    out
}

fn centered(inner: Rect, outer: Rect) -> Rect {
    let x = outer.x + outer.width.saturating_sub(inner.width) / 2;
    let y = outer.y + outer.height.saturating_sub(inner.height) / 2;
    Rect::new(
        x,
        y,
        inner.width.min(outer.width),
        inner.height.min(outer.height),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    fn type_str(d: &mut ArchiveDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    fn dialog() -> ArchiveDialog {
        ArchiveDialog::new(
            vec![PathBuf::from("/tmp/work/notes.txt")],
            PathBuf::from("/tmp/work"),
            WriteFormat::Zip,
        )
    }

    /// The house rule: Tab reaches for a control, it does not answer the form.
    /// Writing a file must never be a side effect of looking around.
    #[test]
    fn tab_never_creates() {
        let mut d = dialog();
        for _ in 0..(d.stops() * 2) {
            assert_eq!(
                d.handle_key(code(KeyCode::Tab)),
                ArchiveDialogOutcome::Continue,
                "Tab decided something"
            );
        }
        for _ in 0..(d.stops() * 2) {
            assert_eq!(
                d.handle_key(code(KeyCode::BackTab)),
                ArchiveDialogOutcome::Continue,
                "BackTab decided something"
            );
        }
    }

    /// Four formats are one stop, not four: field, formats, Create, Cancel.
    #[test]
    fn the_format_group_is_one_stop() {
        let mut d = dialog();
        assert_eq!(d.stops(), 4, "the group should not add a stop per format");
        assert_eq!(d.focus, ArchiveFocus::Field);
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus, ArchiveFocus::Format);
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(
            d.focus,
            ArchiveFocus::Buttons,
            "a second Tab should leave the group, not move inside it"
        );
    }

    #[test]
    fn arrows_move_within_the_format_group() {
        let mut d = dialog();
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.format(), WriteFormat::Zip);

        d.handle_key(code(KeyCode::Down));
        assert_eq!(d.format(), WriteFormat::SevenZ);
        assert_eq!(d.focus, ArchiveFocus::Format, "the arrow left the group");

        d.handle_key(code(KeyCode::Up));
        assert_eq!(d.format(), WriteFormat::Zip);

        // Wrapping at both ends.
        d.handle_key(code(KeyCode::Up));
        assert_eq!(d.format(), *WriteFormat::ALL.last().unwrap());
        d.handle_key(code(KeyCode::Down));
        assert_eq!(d.format(), WriteFormat::Zip);
    }

    /// Enter on the group answers the group, which has already been answered.
    /// It must not be a hidden accept.
    #[test]
    fn enter_on_the_format_group_does_not_create() {
        let mut d = dialog();
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus, ArchiveFocus::Format);
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            ArchiveDialogOutcome::Continue,
            "Enter on the radio group wrote a file"
        );
    }

    /// A digit is a shortcut for the radio, not for the form — unlike the sort
    /// menu, where a digit closes the menu.
    #[test]
    fn a_digit_picks_a_format_without_creating() {
        let mut d = dialog();
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(
            d.handle_key(key('3')),
            ArchiveDialogOutcome::Continue,
            "a digit created the archive"
        );
        assert_eq!(d.format(), WriteFormat::Tar);
    }

    /// The other half of that guard: in the name field a digit is a character.
    /// `report-2026.zip` has to be typable.
    #[test]
    fn digits_typed_into_the_name_field_are_text() {
        let mut d = dialog();
        // Focus starts on the field. Clear the pre-filled name first.
        for _ in 0..d.name().chars().count() {
            d.handle_key(code(KeyCode::Backspace));
        }
        type_str(&mut d, "report-2026");
        assert_eq!(
            d.name(),
            "report-2026",
            "the digits did not reach the name"
        );
        assert_eq!(d.format(), WriteFormat::Zip, "a typed digit moved the radio");
    }

    #[test]
    fn a_digit_past_the_last_format_is_inert() {
        let mut d = dialog();
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(key('9'));
        assert_eq!(d.format(), WriteFormat::Zip, "a digit past the end selected");
    }

    /// The two-part case is the one that goes wrong.
    #[test]
    fn changing_the_format_rewrites_the_extension() {
        let mut d = ArchiveDialog::new(
            vec![PathBuf::from("/tmp/work")],
            PathBuf::from("/tmp/work"),
            WriteFormat::TarGz,
        );
        assert_eq!(d.name(), "work.tgz");

        d.handle_key(code(KeyCode::Tab));
        d.select_format(0);
        assert_eq!(d.name(), "work.zip", "the stem should survive");
    }

    #[test]
    fn a_hand_edited_name_is_not_rewritten() {
        let mut d = dialog();
        // Type a name by hand, extension and all.
        for _ in 0..d.name().chars().count() {
            d.handle_key(code(KeyCode::Backspace));
        }
        type_str(&mut d, "backup.tgz");
        assert_eq!(d.name(), "backup.tgz");

        d.handle_key(code(KeyCode::Tab));
        d.handle_key(code(KeyCode::Down));
        assert_eq!(
            d.name(),
            "backup.tgz",
            "the name the user typed was rewritten under them"
        );
    }

    /// A single file keeps its own extension and gains the archive's:
    /// `notes.txt` becomes `notes.txt.zip`. Stripping `.txt` would lose which
    /// file it was — `notes.zip` could have come from `notes.txt` or
    /// `notes.md`. Only a recognised *archive* extension is replaced.
    #[test]
    fn enter_in_the_field_creates() {
        let mut d = dialog();
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            ArchiveDialogOutcome::Create {
                name: "notes.txt.zip".to_string(),
                format: WriteFormat::Zip,
            }
        );
    }

    #[test]
    fn esc_cancels_and_an_empty_name_does_not_create() {
        let mut d = dialog();
        assert_eq!(
            d.handle_key(code(KeyCode::Esc)),
            ArchiveDialogOutcome::Cancelled
        );

        let mut d = dialog();
        for _ in 0..d.name().chars().count() {
            d.handle_key(code(KeyCode::Backspace));
        }
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            ArchiveDialogOutcome::Continue,
            "an empty name should keep the dialog up"
        );
    }

    /// A name can carry multibyte characters, and the cursor is a char index.
    #[test]
    fn a_multibyte_name_is_editable() {
        let mut d = dialog();
        for _ in 0..d.name().chars().count() {
            d.handle_key(code(KeyCode::Backspace));
        }
        type_str(&mut d, "José.zip");
        assert_eq!(d.name(), "José.zip");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Right));
        d.handle_key(code(KeyCode::Right));
        d.handle_key(code(KeyCode::Delete));
        assert_eq!(d.name(), "Joé.zip");
    }

    #[test]
    fn clicking_a_format_selects_it_without_creating() {
        let mut d = dialog();
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();

        let row = d.format_areas[2];
        assert_eq!(
            d.click_at(row.x + 1, row.y),
            ArchiveDialogOutcome::Continue,
            "a click on a format wrote a file"
        );
        assert_eq!(d.format(), WriteFormat::Tar);
    }

    #[test]
    fn clicking_create_creates_and_clicking_nothing_does_nothing() {
        let mut d = dialog();
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();

        assert_eq!(
            d.click_at(0, 0),
            ArchiveDialogOutcome::Continue,
            "a click outside decided something"
        );

        let create = d.button_areas[0];
        assert!(matches!(
            d.click_at(create.x + 1, create.y),
            ArchiveDialogOutcome::Create { .. }
        ));
    }

    #[test]
    fn the_dialog_renders_its_field_formats_and_buttons() {
        let mut d = dialog();
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 30)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();
        let text: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(text.contains("Create archive"), "no title:\n{text}");
        assert!(text.contains("notes.txt.zip"), "no name:\n{text}");
        for f in WriteFormat::ALL {
            assert!(
                text.contains(f.label()),
                "format {} is not offered:\n{text}",
                f.label()
            );
        }
        assert!(text.contains("Create"), "no create button:\n{text}");
        assert!(text.contains("Cancel"), "no cancel button:\n{text}");
    }

    /// A terminal too small to draw the controls must not leave click
    /// rectangles pointing at cells no one can see.
    #[test]
    fn the_dialog_survives_a_tiny_terminal() {
        for w in 1..40u16 {
            for h in 1..20u16 {
                let mut d = dialog();
                let mut term =
                    ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
                term.draw(|f| d.render(f, f.area())).unwrap();
                for area in d.format_areas.iter().chain(d.button_areas.iter()) {
                    assert!(
                        area.y < h && area.x < w,
                        "a click target at {area:?} is off a {w}x{h} screen"
                    );
                    assert!(
                        area.x + area.width <= w,
                        "a click target at {area:?} runs off the right of a {w}x{h} screen"
                    );
                }
            }
        }
    }
}

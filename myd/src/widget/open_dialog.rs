//! Running a program of the user's choosing over the selection.
//!
//! A text field for the command line and three buttons. The field mechanics
//! follow [`crate::widget::rename_dialog`] and the buttons follow
//! [`crate::widget::confirm_dialog`] — this dialog is the two halves together,
//! and copying both keeps `Tab`, `Enter` and clicks meaning what they already
//! mean everywhere else here.

use crate::widget::text_field::TextField;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};
use std::path::PathBuf;

/// What the dialog decided this keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenDialogOutcome {
    /// Still editing. The dialog stays up.
    Continue,
    Cancelled,
    /// Run this command line over the captured targets.
    Run { command: String },
    /// Save the field's command as a preset under `label`.
    ///
    /// The app owns the catalog, so the dialog reports the intent rather than
    /// writing the file — the same split `gd` uses for its saved hosts.
    Save { label: String, command: String },
    /// Forget the preset with this label.
    Forget { label: String },
}

/// Which half of the dialog has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenFocus {
    Field,
    /// The saved-app list. One stop, whatever its length — the list is walked
    /// with the arrows once it has the keyboard, as `gd`'s is.
    List,
    Actions,
    Buttons,
}

/// The buttons, in the order they are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenButton {
    Ok,
    Cancel,
}

/// What the actions panel offers for the highlighted entry.
///
/// Modelled on `gd`'s panel: each action carries the letter that runs it, and
/// the letter is drawn highlighted inside the label so the shortcut is visible
/// without a legend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAction {
    /// Save what is in the field as a new preset.
    Save,
    /// Replace the highlighted preset's command with the field's.
    Update,
    /// Delete the highlighted preset.
    Forget,
}

impl OpenAction {
    pub fn label(self) -> &'static str {
        match self {
            OpenAction::Save => "Save",
            OpenAction::Update => "Update",
            OpenAction::Forget => "Forget",
        }
    }

    /// The key that runs it. Upper case in the label, matched case-insensitively.
    pub fn shortcut(self) -> char {
        match self {
            OpenAction::Save => 'S',
            OpenAction::Update => 'U',
            OpenAction::Forget => 'F',
        }
    }
}

pub struct OpenDialog {
    command: TextField,
    focus: OpenFocus,
    /// Index into [`Self::buttons`].
    button: usize,
    /// The paths this will act on, captured when the dialog opened. Held so the
    /// summary line can say what is about to happen; the app re-reads the
    /// selection when it actually runs.
    targets: Vec<PathBuf>,
    /// Rebuilt on every render, so the drawn buttons and their click targets
    /// cannot describe different boxes.
    button_areas: Vec<Rect>,
    /// The saved presets, already ordered for this selection: entries claiming
    /// the target first. A snapshot taken when the dialog opened, so the list
    /// cannot reorder under the cursor while it is being read.
    presets: Vec<PresetRow>,
    /// Index into `presets`.
    selected: usize,
    /// Index into `actions()`.
    action: usize,
    /// Click targets for the list rows and the action rows, rebuilt with them.
    row_areas: Vec<Rect>,
    action_areas: Vec<Rect>,
}

/// One row of the saved-app list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetRow {
    pub label: String,
    pub command: String,
    /// Whether this entry claims the selection, for the marker in the list.
    pub matches_target: bool,
}

impl OpenDialog {
    pub fn new(targets: Vec<PathBuf>) -> Self {
        Self {
            command: TextField::new(),
            focus: OpenFocus::Field,
            button: 0,
            targets,
            button_areas: Vec::new(),
            presets: Vec::new(),
            selected: 0,
            action: 0,
            row_areas: Vec::new(),
            action_areas: Vec::new(),
        }
    }

    /// Attach the saved presets, in the order they should be shown.
    pub fn with_presets(mut self, presets: Vec<PresetRow>) -> Self {
        self.presets = presets;
        self
    }

    /// The presets this dialog is showing.
    pub fn presets(&self) -> &[PresetRow] {
        &self.presets
    }

    /// Replace the list after the catalog changed under it.
    ///
    /// The highlight is kept on the same label where it still exists, so
    /// saving an entry does not move the selection out from under the user;
    /// otherwise it clamps into range.
    pub fn set_presets(&mut self, presets: Vec<PresetRow>) {
        let was = self.selected_preset().map(|p| p.label.clone());
        self.presets = presets;
        self.selected = was
            .and_then(|l| self.presets.iter().position(|p| p.label == l))
            .unwrap_or(0);
        if self.selected >= self.presets.len() {
            self.selected = self.presets.len().saturating_sub(1);
        }
        if self.action >= self.actions().len() {
            self.action = 0;
        }
        // A focus stop that no longer exists must not keep the keyboard.
        if (self.focus == OpenFocus::List && self.presets.is_empty())
            || (self.focus == OpenFocus::Actions && self.actions().is_empty())
        {
            self.focus = OpenFocus::Field;
        }
    }

    /// The highlighted preset, if the list has any.
    pub fn selected_preset(&self) -> Option<&PresetRow> {
        self.presets.get(self.selected)
    }

    /// Which actions apply right now.
    ///
    /// `Save` needs something in the field; `Update` and `Forget` need a
    /// highlighted entry. An action that cannot act is left out rather than
    /// drawn dead — the panel says what is possible, as `gd`'s does.
    pub fn actions(&self) -> Vec<OpenAction> {
        let mut out = Vec::new();
        if self.is_runnable() {
            out.push(OpenAction::Save);
        }
        if self.selected_preset().is_some() {
            if self.is_runnable() {
                out.push(OpenAction::Update);
            }
            out.push(OpenAction::Forget);
        }
        out
    }

    /// Whether the list section is drawn at all.
    ///
    /// Either half is enough: entries to list, or an action to offer. Gating
    /// this on the presets alone made the feature unreachable from a fresh
    /// install — with nothing saved there was no section, so no Save action,
    /// and Save was the only way to get a first entry. The section still
    /// disappears entirely when there is nothing to put in it, which is an
    /// empty field and an empty catalogue.
    ///
    /// The panel must never be a focus stop while the section is hidden: Tab
    /// would land somewhere nobody can see, and the next letter would fire an
    /// action instead of typing a character.
    fn has_list(&self) -> bool {
        !self.presets.is_empty() || !self.actions().is_empty()
    }

    /// Start with the field pre-filled, for a remembered command.
    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = TextField::with_value(command);
        // The field keeps its own cursor.
        self
    }

    pub fn command(&self) -> &str {
        self.command.value()
    }

    pub fn targets(&self) -> &[PathBuf] {
        &self.targets
    }

    /// The buttons this dialog has, in draw order.
    ///
    /// One source for the labels, the click rectangles and the Tab cycle, so
    /// the three can never describe different boxes.
    pub fn buttons(&self) -> Vec<OpenButton> {
        vec![OpenButton::Ok, OpenButton::Cancel]
    }

    fn label(button: OpenButton) -> &'static str {
        match button {
            OpenButton::Ok => " [ OK ] ",
            OpenButton::Cancel => " [ Cancel ] ",
        }
    }

    /// The button with the keyboard, if the buttons have it at all.
    pub fn focused_button(&self) -> Option<OpenButton> {
        if self.focus != OpenFocus::Buttons {
            return None;
        }
        self.buttons().get(self.button).copied()
    }

    /// A command line with something in it. An empty field has nothing to run,
    /// and Enter on it should keep the dialog rather than close it having done
    /// nothing visible.
    fn is_runnable(&self) -> bool {
        !self.command.value().trim().is_empty()
    }

    /// What pressing `button` decides.
    fn press(&self, button: OpenButton) -> OpenDialogOutcome {
        match button {
            OpenButton::Ok => self.run_outcome(),
            OpenButton::Cancel => OpenDialogOutcome::Cancelled,
        }
    }

    /// What an action decides.
    ///
    /// `Save` and `Update` differ only in the label they attach: a new preset
    /// is named after the command's first word, an update keeps the highlighted
    /// entry's name. Both go back to the app as an intent — the catalog is not
    /// the dialog's to write.
    fn run_action(&self, action: OpenAction) -> OpenDialogOutcome {
        let command = self.command.value().trim().to_string();
        match action {
            OpenAction::Save if !command.is_empty() => OpenDialogOutcome::Save {
                label: default_label(&command),
                command,
            },
            OpenAction::Update if !command.is_empty() => match self.selected_preset() {
                Some(row) => OpenDialogOutcome::Save {
                    label: row.label.clone(),
                    command,
                },
                None => OpenDialogOutcome::Continue,
            },
            OpenAction::Forget => match self.selected_preset() {
                Some(row) => OpenDialogOutcome::Forget {
                    label: row.label.clone(),
                },
                None => OpenDialogOutcome::Continue,
            },
            _ => OpenDialogOutcome::Continue,
        }
    }

    fn run_outcome(&self) -> OpenDialogOutcome {
        if self.is_runnable() {
            OpenDialogOutcome::Run {
                command: self.command.value().trim().to_string(),
            }
        } else {
            OpenDialogOutcome::Continue
        }
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> OpenDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => OpenDialogOutcome::Cancelled,

            // Tab moves the focus and never runs anything — the same contract as
            // every other dialog here. Reaching for the buttons must not be read
            // as consent to execute a program.
            KeyCode::Tab | KeyCode::BackTab => {
                self.cycle(matches!(key.code, KeyCode::BackTab));
                OpenDialogOutcome::Continue
            }

            KeyCode::Enter => match self.focus {
                // Enter in the field runs it. This is a command prompt, and
                // making the user Tab to OK before it would take would be a
                // nuisance — the Tab rule still holds, because Tab itself
                // remains inert.
                OpenFocus::Field => self.run_outcome(),
                // Enter on a preset runs it, which is the whole point of the
                // list. It fills the field first so the command is visible
                // before it goes — running something the user cannot see would
                // be a poor way to launch a program.
                OpenFocus::List => match self.selected_preset() {
                    Some(row) => {
                        let command = row.command.clone();
                        self.command = TextField::with_value(command.clone());
                        OpenDialogOutcome::Run { command }
                    }
                    None => OpenDialogOutcome::Continue,
                },
                OpenFocus::Actions => match self.actions().get(self.action).copied() {
                    Some(action) => self.run_action(action),
                    None => OpenDialogOutcome::Continue,
                },
                OpenFocus::Buttons => match self.buttons().get(self.button).copied() {
                    Some(button) => self.press(button),
                    None => OpenDialogOutcome::Continue,
                },
            },

            // The buttons take the arrows they are laid out along; the field
            // needs left and right for its own cursor.
            KeyCode::Left if self.focus == OpenFocus::Buttons => {
                self.cycle(true);
                OpenDialogOutcome::Continue
            }
            KeyCode::Right if self.focus == OpenFocus::Buttons => {
                self.cycle(false);
                OpenDialogOutcome::Continue
            }
            // Up and down walk whichever list has the keyboard. With no list
            // showing they step between the field and the buttons, which is
            // what they did before there was one.
            KeyCode::Up | KeyCode::Down => {
                let down = key.code == KeyCode::Down;
                match self.focus {
                    OpenFocus::List if !self.presets.is_empty() => {
                        let last = self.presets.len() - 1;
                        self.selected = step(self.selected, last, down);
                        // Show what is highlighted. OK and the actions all act
                        // on the field, so walking the list has to put the
                        // command there or the panel would be offering to save
                        // and update something the user cannot see. A click and
                        // a search both do this; the arrows must agree.
                        let command = self.presets[self.selected].command.clone();
                        self.command = TextField::with_value(command);
                    }
                    OpenFocus::Actions if !self.actions().is_empty() => {
                        let last = self.actions().len() - 1;
                        self.action = step(self.action, last, down);
                    }
                    _ => {
                        self.focus = match self.focus {
                            OpenFocus::Field => OpenFocus::Buttons,
                            _ => OpenFocus::Field,
                        };
                    }
                }
                OpenDialogOutcome::Continue
            }

            // A letter in the actions panel runs that action outright, the way
            // `gd`'s panel works. Guarded on the focus: the same letters are
            // ordinary text in the command field, which is the trap the archive
            // dialog's digit shortcut documents.
            KeyCode::Char(c) if self.focus == OpenFocus::Actions => {
                match self
                    .actions()
                    .into_iter()
                    .find(|a| a.shortcut().eq_ignore_ascii_case(&c))
                {
                    Some(action) => self.run_action(action),
                    None => OpenDialogOutcome::Continue,
                }
            }

            // Typing in the list jumps to the first entry whose label starts
            // with that letter, as the `gd` picker's list does.
            //
            // A letter that matches nothing falls through to the field instead
            // of being swallowed. The picker can afford to eat every keystroke
            // because searching is all its list is for; here the field is the
            // primary control, and a dialog where typing silently does nothing
            // is worse than one where an unmatched letter starts editing.
            KeyCode::Char(c)
                if self.focus == OpenFocus::List
                    && self
                        .presets
                        .iter()
                        .any(|p| p.label.to_ascii_lowercase().starts_with(c.to_ascii_lowercase())) =>
            {
                let c = c.to_ascii_lowercase();
                if let Some(i) = self
                    .presets
                    .iter()
                    .position(|p| p.label.to_ascii_lowercase().starts_with(c))
                {
                    self.selected = i;
                    // Show what the search landed on, so OK and the actions act
                    // on something visible — the same reason a click does.
                    let command = self.presets[i].command.clone();
                    self.command = TextField::with_value(command);
                }
                OpenDialogOutcome::Continue
            }

            // Everything below edits the field. Typing while the buttons have
            // focus returns to the field and inserts, so a user who tabbed too
            // far and kept typing does not lose the characters.
            // Everything below edits the field. A key that types or deletes
            // returns focus to the field first, so a user who tabbed too far
            // and kept typing does not lose the characters; a bare motion does
            // not steal focus back.
            _ => {
                let edits = !matches!(
                    key.code,
                    KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
                );
                if self.command.handle_key(key) && edits {
                    self.focus = OpenFocus::Field;
                }
                OpenDialogOutcome::Continue
            }
        }
    }

    /// Move the focus one stop, wrapping.
    ///
    /// The stops are: the field, the list (one stop however long it is), the
    /// actions panel, then each button. A section with nothing in it is skipped
    /// rather than being a stop that shows nothing — Tab past an empty list
    /// would look like the key had been dropped.
    fn cycle(&mut self, backwards: bool) {
        // Each button is its own stop, as it was before the list existed: Tab
        // walks OK then Cancel rather than treating the row as one place. The
        // list and the actions panel are single stops instead — they are walked
        // with the arrows once they have the keyboard, which is how the `gd`
        // picker's list behaves and what makes a long list bearable.
        let buttons = self.buttons().len();
        let mut stops: Vec<(OpenFocus, usize)> = vec![(OpenFocus::Field, 0)];
        if self.has_list() {
            // The list is only a stop when it has rows; the panel only when it
            // has actions. With nothing saved yet there is a panel offering
            // Save and no list to walk, and Tab must skip the empty half.
            if !self.presets.is_empty() {
                stops.push((OpenFocus::List, 0));
            }
            if !self.actions().is_empty() {
                stops.push((OpenFocus::Actions, self.action.min(self.actions().len() - 1)));
            }
        }
        for i in 0..buttons {
            stops.push((OpenFocus::Buttons, i));
        }
        if stops.is_empty() {
            return;
        }
        let here = stops
            .iter()
            .position(|(f, i)| {
                *f == self.focus && (*f != OpenFocus::Buttons || *i == self.button)
            })
            .unwrap_or(0);
        let next = if backwards {
            (here + stops.len() - 1) % stops.len()
        } else {
            (here + 1) % stops.len()
        };
        let (focus, index) = stops[next];
        self.focus = focus;
        match focus {
            OpenFocus::Buttons => self.button = index,
            OpenFocus::Actions if self.action >= self.actions().len() => self.action = 0,
            _ => {}
        }
    }


    /// Answer a click at `(x, y)`.
    ///
    /// A click on a button is that button's decision, the same one Enter makes
    /// on the focused one. Clicks elsewhere only move the focus to the field:
    /// this dialog launches a program, and a stray click must not do that.
    pub fn click_at(&mut self, x: u16, y: u16) -> OpenDialogOutcome {
        let hit = self
            .button_areas
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height);
        if let Some(i) = hit {
            if let Some(button) = self.buttons().get(i).copied() {
                self.focus = OpenFocus::Buttons;
                self.button = i;
                return self.press(button);
            }
        }
        // A click on a preset highlights it and does not run it. Running a
        // program is a decision, and this dialog makes the user say so with
        // Enter or the OK button — the same reason a click on a format row in
        // the archive dialog only selects.
        if let Some(i) = self
            .row_areas
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
        {
            if i < self.presets.len() {
                self.focus = OpenFocus::List;
                self.selected = i;
                // Show what was clicked, so OK and the actions act on something
                // the user can see.
                let command = self.presets[i].command.clone();
                self.command = TextField::with_value(command);
            }
            return OpenDialogOutcome::Continue;
        }
        // An action is a button in all but shape, so a click runs it.
        if let Some(i) = self
            .action_areas
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)
        {
            if let Some(action) = self.actions().get(i).copied() {
                self.focus = OpenFocus::Actions;
                self.action = i;
                return self.run_action(action);
            }
        }
        OpenDialogOutcome::Continue
    }

    /// The line describing what this will act on.
    fn summary(&self) -> String {
        match self.targets.len() {
            0 => " Nothing selected".to_string(),
            1 => format!(
                " Opening {}",
                self.targets[0]
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| self.targets[0].display().to_string())
            ),
            n => format!(" Opening {} tagged files", n),
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 72.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // title + blank + label + field + blank + hint + blank + buttons, plus
        // the two border rows — and the list section when there is one, which
        // is a header row plus a row per preset. Derived rather than a constant,
        // so a catalog of six does not clip the buttons off the bottom.
        let rows = if self.has_list() {
            self.presets.len().max(self.actions().len())
        } else {
            0
        };
        let list_rows = if rows == 0 { 0 } else { rows as u16 + 2 };
        let height = (12 + list_rows).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        // Not just zero: a 1x1 box is entirely border, with no interior for a
        // row to land in, and the rows below would still record rectangles for
        // cells nobody can click. The archive dialog carries the same guard for
        // the same reason.
        if center.width <= 2 || center.height <= 2 {
            // Nothing survives the clamp on a terminal this small. Drawing the
            // buttons anyway would leave click rectangles pointing at cells no
            // one can see.
            self.button_areas.clear();
            self.row_areas.clear();
            self.action_areas.clear();
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

        let field_focused = self.focus == OpenFocus::Field;
        lines.push(Line::from(Span::styled(
            " Command".to_string(),
            if field_focused {
                Style::default().fg(accent).bg(bg).add_modifier(Modifier::BOLD)
            } else {
                dim
            },
        )));
        // Two columns of indent, then the field. The cursor is a styled cell,
        // not a character spliced into the text — see `TextField`.
        let field_style = if field_focused {
            normal.add_modifier(Modifier::BOLD)
        } else {
            normal
        };
        let mut field_spans = vec![Span::styled("  ".to_string(), field_style)];
        field_spans.extend(self.command.spans(inner.saturating_sub(2), field_style, field_focused));
        lines.push(Line::from(field_spans));

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            truncate(
                "  The selected files are appended, e.g.  vim -v  runs  vim -v <files>",
                inner,
            ),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        // The saved presets and the actions panel, side by side. Both are built
        // in the same pass that records their click rectangles — two passes can
        // disagree about which cell is which.
        self.row_areas.clear();
        self.action_areas.clear();
        let actions = if self.has_list() {
            self.actions()
        } else {
            Vec::new()
        };
        if self.has_list() {
            let list_focused = self.focus == OpenFocus::List;
            let actions_focused = self.focus == OpenFocus::Actions;
            // The list takes the left two thirds, the panel the rest.
            let col = (inner * 2 / 3).max(12);
            lines.push(Line::from(vec![
                Span::styled(
                    pad_to(" Saved apps", col),
                    if list_focused {
                        Style::default().fg(accent).bg(bg).add_modifier(Modifier::BOLD)
                    } else {
                        dim
                    },
                ),
                Span::styled(
                    " Actions".to_string(),
                    if actions_focused {
                        Style::default().fg(accent).bg(bg).add_modifier(Modifier::BOLD)
                    } else {
                        dim
                    },
                ),
            ]));
            // The first list row is this many content lines down: the header
            // just pushed, plus everything before it, plus the top border.
            let first_row = center.y + lines.len() as u16;
            let rows = self.presets.len().max(actions.len());
            for i in 0..rows {
                let mut row: Vec<Span> = Vec::new();
                let left = match self.presets.get(i) {
                    Some(p) => {
                        let selected = i == self.selected;
                        let marker = if selected { " ▸ " } else { "   " };
                        // A dot marks an entry that claims this file, so the
                        // promotion at the top of the list has a visible reason
                        // rather than looking like an arbitrary order.
                        let claim = if p.matches_target { "• " } else { "  " };
                        // The command, not the label. The label is only the
                        // command's first word, so showing it alone hid every
                        // argument — a saved `open -a VLC.app` read as plain
                        // `open`, which is a different program entirely.
                        //
                        // A label the user renamed by hand is worth showing
                        // too, since it is then not derivable from the command;
                        // an auto-derived one would only repeat the first word.
                        let derived = default_label(&p.command);
                        let shown = if p.label == derived {
                            p.command.clone()
                        } else {
                            format!("{}  ({})", p.command, p.label)
                        };
                        let text = pad_to(&format!("{marker}{claim}{shown}"), col);
                        let style = if selected && list_focused {
                            normal.add_modifier(Modifier::REVERSED)
                        } else if selected {
                            normal.add_modifier(Modifier::BOLD)
                        } else {
                            normal
                        };
                        self.row_areas.push(Rect::new(
                            center.x + 1,
                            first_row + i as u16,
                            col as u16,
                            1,
                        ));
                        Span::styled(truncate(&text, col), style)
                    }
                    None => Span::styled(pad_to("", col), normal),
                };
                row.push(left);
                if let Some(action) = actions.get(i) {
                    let focused = actions_focused && i == self.action;
                    let style = if focused {
                        Style::default()
                            .fg(Color::Yellow)
                            .bg(bg)
                            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
                    } else {
                        normal
                    };
                    let label = format!(" [{}]{}", action.shortcut(), &action.label()[1..]);
                    self.action_areas.push(Rect::new(
                        center.x + 1 + col as u16,
                        first_row + i as u16,
                        label.chars().count() as u16,
                        1,
                    ));
                    row.push(Span::styled(label, style));
                }
                lines.push(Line::from(row));
            }
            lines.push(Line::from(Span::styled(String::new(), normal)));
        }

        // Buttons and their click targets are built together, in one pass, for
        // the same reason the confirm dialog does it: two passes can disagree.
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
            let focused = self.focus == OpenFocus::Buttons && i == self.button;
            let style = if focused {
                Style::default()
                    .fg(Color::Yellow)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Yellow).bg(bg)
            };
            spans.push(Span::styled(label, style));
            self.button_areas.push(Rect::new(x, button_y, w, 1));
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
                        " Open with ",
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


/// Right-pad to `width` display columns.
///
/// Measured with the same `display_width` the text field uses, so a CJK label
/// does not push the actions panel out of alignment.
fn pad_to(s: &str, width: usize) -> String {
    let w: usize = s.chars().map(crate::widget::text_field::display_width).sum();
    let mut out = s.to_string();
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// Move an index one place, wrapping at both ends.
fn step(current: usize, last: usize, down: bool) -> usize {
    if down {
        if current >= last { 0 } else { current + 1 }
    } else if current == 0 {
        last
    } else {
        current - 1
    }
}

/// A name for a newly saved command: its first word, without any path.
///
/// `code -w --reuse` becomes `code`, `/usr/bin/gimp` becomes `gimp`. The label
/// is what the list shows and the key edits and deletes use, so it wants to be
/// the short recognisable part rather than the whole line. A command whose
/// first word is empty falls back to the line itself, which cannot be empty
/// here — `Save` is only offered when the field has something in it.
pub fn default_label(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .and_then(|w| w.rsplit('/').next())
        .filter(|w| !w.is_empty())
        .unwrap_or(command)
        .to_string()
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

    fn type_str(d: &mut OpenDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    fn one_file() -> Vec<PathBuf> {
        vec![PathBuf::from("/tmp/a.txt")]
    }

    fn two_files() -> Vec<PathBuf> {
        vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
    }

    /// Render the dialog and return the screen as text.
    fn render_to(d: &mut OpenDialog, w: u16, h: u16) -> String {
        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn row(label: &str, command: &str) -> PresetRow {
        PresetRow {
            label: label.to_string(),
            command: command.to_string(),
            matches_target: false,
        }
    }

    fn with_saved() -> OpenDialog {
        OpenDialog::new(one_file()).with_presets(vec![
            row("vim", "vim"),
            row("code", "code -w"),
            row("gimp", "gimp"),
        ])
    }

    /// With nothing saved but something typed, Save is reachable.
    ///
    /// This is how the first entry ever gets made, and gating the section on
    /// the presets alone made that impossible: no entries meant no section,
    /// which meant no Save action, which meant no way to get an entry. The
    /// feature was unreachable from a fresh install.
    #[test]
    fn the_first_app_can_be_saved_with_an_empty_catalog() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        assert!(d.presets().is_empty(), "nothing saved yet");
        assert!(
            d.actions().contains(&OpenAction::Save),
            "Save must be offered, or the first entry can never be made"
        );

        // Tab reaches the panel — skipping the list, which has no rows.
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(
            d.handle_key(key('s')),
            OpenDialogOutcome::Save {
                label: "vim".to_string(),
                command: "vim".to_string()
            }
        );
    }

    /// And the section is absent entirely when there is nothing to put in it:
    /// no entries and an empty field. Then the dialog is what it always was.
    #[test]
    fn an_empty_catalog_and_an_empty_field_leave_the_dialog_as_it_was() {
        let mut d = OpenDialog::new(one_file());
        assert!(d.presets().is_empty());
        assert!(d.actions().is_empty(), "nothing to save, nothing to forget");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Cancel));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), None, "and back to the field");
    }

    /// A letter must still be text in the field when a panel exists beside it.
    #[test]
    fn a_letter_in_the_field_is_text_even_when_save_is_offered() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vims");
        assert_eq!(d.command(), "vims", "the trailing s is a character");
        assert!(d.presets().is_empty(), "and saved nothing");
    }

    /// Tab reaches the list and the panel, and never runs anything on the way.
    #[test]
    fn tab_reaches_the_list_and_the_actions_and_never_decides() {
        let mut d = with_saved();
        type_str(&mut d, "vim");
        // field → list → actions → OK → Cancel → field
        for _ in 0..12 {
            assert_eq!(
                d.handle_key(code(KeyCode::Tab)),
                OpenDialogOutcome::Continue,
                "Tab must never decide anything"
            );
        }
    }

    /// The list is one stop however long it is; the arrows walk it.
    #[test]
    fn the_list_is_one_stop_and_the_arrows_walk_it() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("vim"));
        d.handle_key(code(KeyCode::Down));
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("code"));
        d.handle_key(code(KeyCode::Up));
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("vim"));
        // And it wraps, rather than sticking at the ends.
        d.handle_key(code(KeyCode::Up));
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("gimp"));
    }

    /// Enter on a preset runs it, and fills the field first so the command is
    /// visible before it goes.
    #[test]
    fn enter_on_a_preset_runs_it() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        d.handle_key(code(KeyCode::Down)); // code
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "code -w".to_string()
            }
        );
        assert_eq!(d.command(), "code -w", "the field shows what ran");
    }

    /// Typing in the list searches it, as the `gd` picker's list does.
    #[test]
    fn typing_in_the_list_searches_it() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        d.handle_key(key('g'));
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("gimp"));
        assert_eq!(
            d.command(),
            "gimp",
            "the field shows what the search landed on"
        );
    }

    /// A letter matching no entry edits the field instead of vanishing.
    ///
    /// The `gd` picker eats every keystroke because searching is all its list
    /// does. Here the field is the primary control, and a dialog where typing
    /// silently does nothing would be worse than one where an unmatched letter
    /// starts an edit.
    #[test]
    fn an_unmatched_letter_in_the_list_reaches_the_field() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        d.handle_key(key('z'));
        assert_eq!(d.command(), "z", "no entry starts with z, so it is text");
    }

    /// A letter in the actions panel runs that action. The same letters are
    /// ordinary text in the field, which is what the guard is for.
    #[test]
    fn an_action_letter_runs_it_only_from_the_panel() {
        let mut d = with_saved();
        type_str(&mut d, "mpv");
        // In the field, `f` is a character.
        d.handle_key(key('f'));
        assert_eq!(d.command(), "mpvf");
        assert_eq!(d.presets().len(), 3, "and forgot nothing");

        // In the panel, it forgets the highlighted entry.
        let mut d = with_saved();
        type_str(&mut d, "mpv");
        d.handle_key(code(KeyCode::Tab)); // list
        d.handle_key(code(KeyCode::Tab)); // actions
        assert_eq!(
            d.handle_key(key('f')),
            OpenDialogOutcome::Forget {
                label: "vim".to_string()
            }
        );
    }

    #[test]
    fn save_names_the_entry_after_the_command() {
        let mut d = with_saved();
        type_str(&mut d, "/usr/bin/gimp -n");
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(code(KeyCode::Tab)); // actions
        assert_eq!(
            d.handle_key(key('s')),
            OpenDialogOutcome::Save {
                label: "gimp".to_string(),
                command: "/usr/bin/gimp -n".to_string()
            },
            "the label is the program, without its path or arguments"
        );
    }

    /// Update keeps the highlighted entry's name, so editing a command does not
    /// create a second entry under a different label.
    #[test]
    fn update_keeps_the_highlighted_label() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        d.handle_key(code(KeyCode::Down)); // code
        // Walking the list filled the field with `code -w`; clear it and type
        // the replacement, which is what editing a preset actually looks like.
        d.handle_key(code(KeyCode::BackTab));
        d.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        type_str(&mut d, "codium -w");
        // The highlight stayed on `code` through the edit — that is the point.
        // Walk field → list → actions.
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(
            d.handle_key(key('u')),
            OpenDialogOutcome::Save {
                label: "code".to_string(),
                command: "codium -w".to_string()
            }
        );
    }

    /// An action that cannot act is not offered: Save with an empty field would
    /// save nothing, and the panel says what is possible.
    #[test]
    fn an_empty_field_offers_no_save() {
        let d = with_saved();
        assert!(!d.actions().contains(&OpenAction::Save));
        assert!(d.actions().contains(&OpenAction::Forget), "but forget applies");
    }

    /// A click on a preset selects it and does not run it. Running a program is
    /// a decision, and a stray click must not make it.
    #[test]
    fn clicking_a_preset_selects_without_running() {
        let mut d = with_saved();
        let text = render_to(&mut d, 90, 24);
        assert!(text.contains("Saved apps"), "the list should be drawn:\n{text}");
        // The second row's rectangle, from the render that just happened.
        let r = d.row_areas[1];
        assert_eq!(
            d.click_at(r.x + 1, r.y),
            OpenDialogOutcome::Continue,
            "a click must not launch anything"
        );
        assert_eq!(d.selected_preset().map(|p| p.label.as_str()), Some("code"));
        assert_eq!(d.command(), "code -w", "and shows what was clicked");
    }

    /// The list shows the command, arguments and all.
    ///
    /// It used to show only the label, which is the command's *first word* —
    /// so a saved `open -a VLC.app` was listed as plain `open`, a different
    /// program. The entry ran correctly; it just described itself wrongly,
    /// which is worse than not listing it.
    #[test]
    fn the_list_shows_the_whole_command_not_just_its_first_word() {
        let mut d = OpenDialog::new(one_file())
            .with_presets(vec![row("open", "open -a VLC.app")]);
        let text = render_to(&mut d, 90, 24);
        assert!(
            text.contains("open -a VLC.app"),
            "the arguments must be visible:\n{text}"
        );
    }

    /// A label the user renamed by hand is shown beside the command, since it
    /// is then not derivable from it. An auto-derived label is not, because it
    /// would only repeat the command's first word.
    #[test]
    fn a_hand_named_label_is_shown_and_a_derived_one_is_not() {
        let mut d = OpenDialog::new(one_file())
            .with_presets(vec![row("player", "open -a VLC.app")]);
        let text = render_to(&mut d, 90, 24);
        assert!(text.contains("open -a VLC.app"), "{text}");
        assert!(text.contains("(player)"), "the chosen name is worth showing:\n{text}");

        let mut d = OpenDialog::new(one_file()).with_presets(vec![row("vim", "vim")]);
        let text = render_to(&mut d, 90, 24);
        assert!(!text.contains("(vim)"), "a derived label is just noise:\n{text}");
    }

    #[test]
    fn the_list_and_the_actions_are_drawn() {
        let mut d = with_saved();
        type_str(&mut d, "vim");
        let text = render_to(&mut d, 90, 26);
        for want in ["Saved apps", "Actions", "vim", "code", "gimp", "[S]ave"] {
            assert!(text.contains(want), "missing {want:?} in:\n{text}");
        }
    }

    /// The box grows with the list rather than clipping the buttons off.
    #[test]
    fn a_long_list_does_not_push_the_buttons_out_of_the_box() {
        let mut d = OpenDialog::new(one_file()).with_presets(
            (0..8).map(|i| row(&format!("app{i}"), "run")).collect(),
        );
        let text = render_to(&mut d, 90, 40);
        assert!(text.contains("[ OK ]"), "the buttons must survive:\n{text}");
        assert!(text.contains("app7"), "and so must the last entry:\n{text}");
    }

    /// A terminal too small for the box leaves no click targets behind.
    #[test]
    fn a_tiny_terminal_clears_every_click_target() {
        let mut d = with_saved();
        let _ = render_to(&mut d, 1, 1);
        assert!(d.row_areas.is_empty());
        assert!(d.action_areas.is_empty());
        assert!(d.button_areas.is_empty());
    }

    /// Replacing the list keeps the highlight on the same entry, so saving does
    /// not move the selection out from under the user.
    #[test]
    fn set_presets_keeps_the_highlight_where_it_can() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab));
        d.handle_key(code(KeyCode::Down)); // code
        d.set_presets(vec![row("gimp", "gimp"), row("code", "code -w")]);
        assert_eq!(
            d.selected_preset().map(|p| p.label.as_str()),
            Some("code"),
            "the highlight follows the label, not the index"
        );
    }

    /// And when the list empties, the keyboard leaves with it rather than
    /// staying on a stop that is no longer drawn.
    #[test]
    fn emptying_the_list_returns_the_focus_to_the_field() {
        let mut d = with_saved();
        d.handle_key(code(KeyCode::Tab)); // list
        d.set_presets(Vec::new());
        d.handle_key(key('x'));
        assert_eq!(d.command(), "x", "typing goes to the field again");
    }

    #[test]
    fn enter_in_the_field_runs_the_typed_command() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim -v");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim -v".to_string()
            }
        );
    }

    #[test]
    fn tab_moves_the_focus_and_never_runs_anything() {
        // The house rule: Tab is for reaching the buttons, not for pressing
        // them. A dialog that launches a program is the last place to bend it.
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "rm -rf");
        for _ in 0..8 {
            assert_eq!(
                d.handle_key(code(KeyCode::Tab)),
                OpenDialogOutcome::Continue,
                "Tab must never decide anything"
            );
        }
        for _ in 0..8 {
            assert_eq!(
                d.handle_key(code(KeyCode::BackTab)),
                OpenDialogOutcome::Continue,
                "Shift-Tab must never decide anything either"
            );
        }
    }

    #[test]
    fn tab_walks_the_field_then_every_button_and_wraps() {
        let mut d = OpenDialog::new(one_file());
        assert_eq!(d.focused_button(), None, "the field starts with focus");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Cancel));
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), None, "and back to the field");
    }

    #[test]
    fn the_same_two_buttons_whatever_is_selected() {
        // The selection changes what the command runs over, not what the dialog
        // offers to do with it.
        assert_eq!(
            OpenDialog::new(one_file()).buttons(),
            vec![OpenButton::Ok, OpenButton::Cancel]
        );
        assert_eq!(
            OpenDialog::new(two_files()).buttons(),
            vec![OpenButton::Ok, OpenButton::Cancel]
        );
    }

    #[test]
    fn esc_cancels_distinctly_from_an_empty_enter() {
        // The plain input dialog cannot tell these apart — it returns an empty
        // string for both — and several call sites work around it. This one
        // reports them separately.
        let mut d = OpenDialog::new(one_file());
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Continue,
            "an empty command has nothing to run"
        );
        assert_eq!(d.handle_key(code(KeyCode::Esc)), OpenDialogOutcome::Cancelled);
    }

    #[test]
    fn a_blank_command_is_not_runnable() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "   ");
        assert_eq!(d.handle_key(code(KeyCode::Enter)), OpenDialogOutcome::Continue);
    }

    #[test]
    fn the_command_is_trimmed_before_it_runs() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "  vim  ");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            }
        );
    }

    #[test]
    fn each_button_decides_its_own_thing() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");

        // Something in the field means a Save action, so Tab reaches the panel
        // before the buttons: field → actions → OK → Cancel.
        d.handle_key(code(KeyCode::Tab)); // actions
        d.handle_key(code(KeyCode::Tab)); // OK
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            }
        );

        d.handle_key(code(KeyCode::Tab)); // Cancel
        assert_eq!(d.focused_button(), Some(OpenButton::Cancel));
        assert_eq!(d.handle_key(code(KeyCode::Enter)), OpenDialogOutcome::Cancelled);
    }

    #[test]
    fn typing_after_tabbing_returns_to_the_field() {
        // Otherwise the characters vanish: the user tabbed one stop too far,
        // kept typing, and watched nothing appear.
        let mut d = OpenDialog::new(one_file());
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focused_button(), Some(OpenButton::Ok));
        type_str(&mut d, "less");
        assert_eq!(d.command(), "less");
        assert_eq!(d.focused_button(), None);
    }

    #[test]
    fn editing_keys_move_within_the_command() {
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        d.handle_key(code(KeyCode::Home));
        type_str(&mut d, "g");
        assert_eq!(d.command(), "gvim");
        d.handle_key(code(KeyCode::End));
        type_str(&mut d, "!");
        assert_eq!(d.command(), "gvim!");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.command(), "gvim");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Delete));
        assert_eq!(d.command(), "vim");
    }

    #[test]
    fn a_multibyte_command_does_not_panic_on_the_cursor() {
        // The cursor counts characters and the string indexes bytes; mixing the
        // two panics mid-codepoint.
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "é→ç");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Right));
        type_str(&mut d, "ü");
        assert_eq!(d.command(), "éü→ç");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.command(), "é→ç");
    }

    #[test]
    fn clicking_a_button_presses_it() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.render(f, f.area())).unwrap();

        let areas = d.button_areas.clone();
        assert_eq!(areas.len(), 2, "two buttons were drawn");
        assert_eq!(
            d.click_at(areas[1].x, areas[1].y),
            OpenDialogOutcome::Cancelled,
            "clicking Cancel backs out"
        );
        assert_eq!(
            d.click_at(areas[0].x, areas[0].y),
            OpenDialogOutcome::Run {
                command: "vim".to_string()
            },
            "clicking OK runs the command"
        );
    }

    #[test]
    fn clicking_away_from_the_buttons_does_nothing() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut d = OpenDialog::new(one_file());
        type_str(&mut d, "vim");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| d.render(f, f.area())).unwrap();
        assert_eq!(d.click_at(0, 0), OpenDialogOutcome::Continue);
    }

    #[test]
    fn the_summary_says_what_is_being_opened() {
        assert!(OpenDialog::new(one_file()).summary().contains("a.txt"));
        assert!(OpenDialog::new(two_files()).summary().contains("2 tagged"));
    }

    #[test]
    fn it_renders_at_tiny_terminal_sizes_without_panicking() {
        use ratatui::{backend::TestBackend, Terminal};
        for (w, h) in [(1u16, 1u16), (2, 3), (10, 4), (40, 2), (80, 24)] {
            let mut d = OpenDialog::new(one_file());
            type_str(&mut d, "vim -v");
            let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| d.render(f, f.area()))
                .unwrap_or_else(|e| panic!("open dialog panicked at {}x{}: {}", w, h, e));
        }
    }
}

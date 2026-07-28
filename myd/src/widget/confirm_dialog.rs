use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// What a choice dialog answered with.
///
/// A plain yes/no confirmation reports `Yes`/`No`; a multi-choice dialog reports
/// the letter that was pressed, so the caller can tell "skip" from "cancel the
/// whole batch" — two answers that both mean "don't do this one".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// One of the dialog's declared choice letters.
    Choice(char),
}

pub struct ConfirmDialog {
    pub title: &'static str,
    pub message: String,
    cursor: usize,
    /// Letters this dialog accepts instead of y/n. Empty for a yes/no dialog.
    choices: Vec<char>,
    /// A statement rather than a question: one OK button, no choice to make.
    ///
    /// Most of this dialog's uses report something ("could not save…", "not a
    /// directory") rather than asking anything, and offering Yes/No there
    /// invites the reader to wonder what "No" would decline.
    notice: bool,
    /// Where each button was last drawn, for click hit-testing. Rebuilt every
    /// render, so a resize can never leave a button answering from stale coords.
    button_areas: Vec<Rect>,
}

impl ConfirmDialog {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            title: "Confirm",
            message: message.into(),
            cursor: 0,
            choices: Vec::new(),
            notice: false,
            button_areas: Vec::new(),
        }
    }

    /// Accept these letters rather than y/n.
    ///
    /// Used where "no" isn't a single thing — a move collision can be skipped or
    /// can abandon the rest of the batch, and silently picking one of those
    /// would be a poor guess.
    /// Present this as a notice: a single OK button, dismissed by Enter or Esc.
    pub fn notice(message: impl Into<String>) -> Self {
        Self {
            title: "Notice",
            message: message.into(),
            cursor: 0,
            choices: Vec::new(),
            notice: true,
            button_areas: Vec::new(),
        }
    }

    pub fn with_choices(mut self, choices: &[char]) -> Self {
        self.choices = choices.to_vec();
        self
    }

    /// Handle a key; `None` while the dialog is still waiting for a valid one.
    ///
    /// `\t` moves the focus between buttons and `\u{1}` moves it back; neither
    /// ever answers. Tab used to arrive here as `' '`, along with every other
    /// unhandled key, and `' '` meant accept — so reaching for the other button
    /// pressed the one already focused. Only Enter, a mouse click, or a button's
    /// own letter decides anything.
    pub fn handle_key_answer(&mut self, key: char) -> Option<Answer> {
        // Focus movement first, so it can never be read as an answer.
        if key == '\t' || key == '\u{1}' {
            let count = self.button_count();
            if count > 1 {
                self.cursor = if key == '\t' {
                    (self.cursor + 1) % count
                } else {
                    (self.cursor + count - 1) % count
                };
            }
            return None;
        }

        // A notice has nothing to decide: any of the usual dismissals closes it,
        // and the caller reads that as "acknowledged" rather than as consent.
        // Space is safe here precisely because there is only one button — on a
        // question it would be a blind answer, so it is not accepted there.
        if self.notice {
            return match key {
                '\n' | ' ' | 'y' | 'o' => Some(Answer::Yes),
                _ => None,
            };
        }
        if !self.choices.is_empty() {
            let lowered = key.to_ascii_lowercase();
            if self.choices.contains(&lowered) {
                return Some(Answer::Choice(lowered));
            }
            // Enter picks whichever choice has the focus; Esc is handled by the
            // caller, which maps it to the safest option.
            if key == '\n' {
                return self.choices.get(self.cursor).copied().map(Answer::Choice);
            }
            return None;
        }
        match key {
            'y' => Some(Answer::Yes),
            'n' => Some(Answer::No),
            '\n' => Some(if self.cursor == 0 { Answer::Yes } else { Answer::No }),
            _ => None,
        }
    }

    /// How many focusable buttons this dialog draws.
    fn button_count(&self) -> usize {
        if self.notice {
            1
        } else if self.choices.is_empty() {
            2
        } else {
            self.choices.len()
        }
    }

    /// Which button currently has the focus, for the renderer and for tests.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The label each button draws, in order. One source for the rendered text
    /// and the click rectangles, so the two can never describe different boxes.
    fn button_labels(&self) -> Vec<String> {
        if self.notice {
            vec![" [ OK ] ".to_string()]
        } else if self.choices.is_empty() {
            vec![" [ Yes ] ".to_string(), " [  No  ] ".to_string()]
        } else {
            self.choices.iter().map(|c| format!("[{}]", c)).collect()
        }
    }

    /// Answer a click at `(x, y)`, or `None` if it missed every button.
    ///
    /// A click on a button is that button's answer — the same decision Enter
    /// makes on the focused one. Clicks elsewhere in the dialog do nothing
    /// rather than dismissing: this is a question, and a stray click landing on
    /// "yes" is exactly the accident the Tab fix is about.
    pub fn click_at(&mut self, x: u16, y: u16) -> Option<Answer> {
        let hit = self
            .button_areas
            .iter()
            .position(|r| x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height)?;
        self.cursor = hit;
        if self.notice {
            return Some(Answer::Yes);
        }
        if !self.choices.is_empty() {
            return self.choices.get(hit).copied().map(Answer::Choice);
        }
        Some(if hit == 0 { Answer::Yes } else { Answer::No })
    }

    /// Yes/no view of [`handle_key_answer`], for the many plain confirmations.
    pub fn handle_key(&mut self, key: char) -> Option<bool> {
        match self.handle_key_answer(key)? {
            Answer::Yes => Some(true),
            Answer::No => Some(false),
            // A multi-choice dialog has no meaningful boolean reading.
            Answer::Choice(_) => None,
        }
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        // Sized to its content: a fixed box clipped long messages (a full path
        // in a collision prompt is easily wider than 50 columns) and left the
        // user answering a question they could only half read.
        let width = 60.min(area.width.max(1));
        let inner_width = width.saturating_sub(2).max(1) as usize;
        let message_lines = wrap_text(&self.message, inner_width);
        // title + blank + message(n) + blank + buttons, plus 2 border rows.
        let height = (message_lines.len() as u16 + 6).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        // Buttons and their click targets are laid out together. The focused one
        // is reversed so Tab visibly moves something — without that the key
        // looked inert, which is half of why it read as "accept".
        let labels = self.button_labels();
        let sep = "  ";
        // Text starts inside the left border, and the buttons are the last line
        // before the bottom one.
        let mut x = center.x + 1;
        let button_y = center.y + center.height.saturating_sub(2);
        let mut spans: Vec<Span> = Vec::new();
        self.button_areas.clear();
        for (i, label) in labels.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(sep));
                x += sep.chars().count() as u16;
            }
            let w = label.chars().count() as u16;
            let style = if i == self.cursor {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(Color::Yellow)
            };
            spans.push(Span::styled(label.clone(), style));
            self.button_areas.push(Rect::new(x, button_y, w, 1));
            x += w;
        }
        let buttons = Line::from(spans);

        let mut lines = vec![
            Line::from(Span::styled(
                self.title,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
        ];
        lines.extend(message_lines.into_iter().map(|l| Line::from(Span::raw(l))));
        lines.push(Line::from(""));
        lines.push(buttons);

        let paragraph = Paragraph::new(ratatui::text::Text::from(lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(Color::Cyan)),
        );
        frame.render_widget(paragraph, center);
    }
}

/// Wrap `text` to `width` columns, counting chars so multibyte paths wrap at the
/// right column. A word longer than the line is hard-split rather than truncated.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > width {
            if current_len > 0 {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                current = chunk;
                current_len = current.chars().count();
            }
            continue;
        }
        let need = if current_len == 0 {
            word_len
        } else {
            current_len + 1 + word_len
        };
        if need > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
            current_len = word_len;
        } else {
            if current_len > 0 {
                current.push(' ');
            }
            current.push_str(word);
            current_len = need;
        }
    }
    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Center `r` inside `area`, clamped to fit. A dialog larger than the terminal
/// would otherwise land outside the buffer and panic on render.
fn centered(r: Rect, area: Rect) -> Rect {
    let w = r.width.min(area.width);
    let h = r.height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(w) / 2,
        area.y + area.height.saturating_sub(h) / 2,
        w,
        h,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_moves_the_focus_and_never_answers() {
        // Reported: the overwrite prompt read Tab as "OK". Every key the app did
        // not recognise arrived here as `' '`, and `' '` meant accept — so
        // reaching for the other button pressed the one already focused.
        let mut d = ConfirmDialog::new("'x' exists. Overwrite?");
        assert_eq!(d.cursor(), 0, "Yes starts focused");

        assert_eq!(d.handle_key_answer('\t'), None, "Tab must not answer");
        assert_eq!(d.cursor(), 1, "it moves to No");
        assert_eq!(d.handle_key_answer('\t'), None);
        assert_eq!(d.cursor(), 0, "and wraps");

        // Shift-Tab steps back.
        assert_eq!(d.handle_key_answer('\u{1}'), None);
        assert_eq!(d.cursor(), 1);

        // Enter takes whatever has the focus.
        assert_eq!(d.handle_key_answer('\n'), Some(Answer::No));
    }

    #[test]
    fn unhandled_keys_do_not_answer_a_question() {
        // `\0` is what the app maps every unrecognised key to. Space goes with
        // them on a question: two buttons means it would be a blind answer.
        let mut d = ConfirmDialog::new("Delete 3 items?");
        for k in ['\0', ' ', 'q', '\u{7}'] {
            assert_eq!(
                d.handle_key_answer(k),
                None,
                "{:?} must not answer a question",
                k
            );
        }
    }

    #[test]
    fn tab_cycles_a_multi_choice_dialog_and_enter_takes_the_focused_one() {
        // A move collision offers overwrite / skip / cancel. Enter used to take
        // the first choice whatever the focus, so Tab could not reach the others.
        let mut d = ConfirmDialog::new("'x' exists.").with_choices(&['o', 's', 'c']);
        assert_eq!(d.handle_key_answer('\t'), None);
        assert_eq!(d.cursor(), 1);
        assert_eq!(d.handle_key_answer('\n'), Some(Answer::Choice('s')));

        // A letter still answers directly, wherever the focus is.
        let mut d = ConfirmDialog::new("'x' exists.").with_choices(&['o', 's', 'c']);
        assert_eq!(d.handle_key_answer('c'), Some(Answer::Choice('c')));
    }

    #[test]
    fn clicking_a_button_answers_it() {
        use ratatui::{backend::TestBackend, Terminal};

        // The click targets come from the same labels the renderer draws, so
        // they cannot describe boxes that are not on screen.
        let mut d = ConfirmDialog::new("'x' exists. Overwrite?");
        let mut term = Terminal::new(TestBackend::new(70, 12)).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();

        let areas = d.button_areas.clone();
        assert_eq!(areas.len(), 2, "a question draws Yes and No");

        // A click on "No" answers No, not whatever had the focus.
        let no = areas[1];
        assert_eq!(
            d.click_at(no.x + 1, no.y),
            Some(Answer::No),
            "clicking No must answer No"
        );

        // A click that misses every button decides nothing.
        let mut d = ConfirmDialog::new("'x' exists. Overwrite?");
        term.draw(|f| d.render(f, f.area())).unwrap();
        let y = d.button_areas[0].y;
        assert_eq!(
            d.click_at(0, y.saturating_sub(2)),
            None,
            "a stray click must not answer"
        );
    }

    #[test]
    fn a_notice_is_dismissed_but_asks_nothing() {
        // Errors and status messages are statements. Offering Yes/No invites the
        // reader to wonder what declining would mean.
        let mut d = ConfirmDialog::notice("'/no/such/place' is not a directory.");
        assert_eq!(d.title, "Notice");
        // The usual dismissals all close it.
        for k in ['\n', ' ', 'y'] {
            assert_eq!(
                ConfirmDialog::notice("x").handle_key_answer(k),
                Some(Answer::Yes),
                "{:?} should dismiss a notice",
                k
            );
        }
        // "No" is not a meaningful reply to a statement, so it is ignored rather
        // than reported as a decision the caller has to interpret.
        assert_eq!(d.handle_key_answer('n'), None);
    }

    #[test]
    fn a_question_still_answers_yes_and_no() {
        let mut d = ConfirmDialog::new("Delete 3 items?");
        assert_eq!(d.title, "Confirm");
        assert_eq!(d.handle_key_answer('y'), Some(Answer::Yes));
        assert_eq!(d.handle_key_answer('n'), Some(Answer::No));
    }

    #[test]
    fn a_notice_renders_one_ok_button() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(70, 12)).unwrap();
        let mut d = ConfirmDialog::notice("something went wrong");
        term.draw(|f| d.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..12)
            .map(|y| (0..70).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect();
        assert!(text.contains("[ OK ]"), "notice needs an OK button: {}", text);
        assert!(!text.contains("Yes"), "and no Yes/No: {}", text);
    }
}

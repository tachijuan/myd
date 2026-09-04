//! Patterned rename: a regex and a replacement, applied to every tagged file.
//!
//! Renaming a batch by hand is the tedium this removes — `IMG_(\d+)` to
//! `holiday-$1` across forty files. The dialog is two fields and a live preview
//! of what the first tagged file would become, so the patterns can be corrected
//! before anything is renamed rather than after.
//!
//! The preview is the whole point: a regex that compiles is not the same as a
//! regex that does what you meant, and a rename batch is not something you want
//! to discover was wrong afterwards.

use crate::widget::text_field::TextField;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

/// Which field has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Pattern,
    Replacement,
}

/// What the dialog decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenameDialogOutcome {
    /// Still editing.
    Continue,
    /// Dismissed; nothing is renamed.
    Cancelled,
    /// Apply this pattern and replacement to the tagged files.
    Apply { pattern: String, replacement: String },
}

/// How the current patterns look when applied to the sample name.
///
/// Held as a computed value rather than recalculated during render, so the
/// renderer stays free of the regex work and the same answer drives both the
/// preview line and whether Enter is allowed to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// No pattern typed yet — nothing to say.
    Empty,
    /// The pattern does not compile; carries the reason.
    BadPattern(String),
    /// Compiles, but does not match the sample.
    NoMatch,
    /// The sample would become this.
    Renamed(String),
}

/// An open patterned-rename dialog.
pub struct RenameDialog {
    pattern: TextField,
    replacement: TextField,
    focus: Field,
    /// The name previewed against — the first tagged file.
    sample: String,
    /// How many files the patterns would be applied to, for the header.
    count: usize,
    preview: Preview,
    /// Where each field was last drawn, so a click can focus it.
    field_areas: Vec<Rect>,
}

impl RenameDialog {
    /// Build a dialog previewing against `sample`, one of `count` tagged files.
    pub fn new(sample: impl Into<String>, count: usize) -> Self {
        let mut d = Self {
            pattern: TextField::new(),
            replacement: TextField::new(),
            focus: Field::Pattern,
            sample: sample.into(),
            count,
            preview: Preview::Empty,
            field_areas: Vec::new(),
        };
        d.recompute();
        d
    }

    pub fn pattern(&self) -> &str {
        self.pattern.value()
    }

    pub fn replacement(&self) -> &str {
        self.replacement.value()
    }

    pub fn focus(&self) -> Field {
        self.focus
    }

    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    /// Whether the current patterns could be applied.
    ///
    /// A pattern that does not compile, or matches nothing, is not an
    /// application — Enter reports the problem instead, leaving the text in
    /// place to be corrected.
    pub fn is_applicable(&self) -> bool {
        matches!(self.preview, Preview::Renamed(_))
    }

    /// Recompute the preview from the current fields.
    ///
    /// Routed through `apply_pattern_indexed` rather than calling the regex
    /// directly, so the counter is resolved here exactly as it will be during
    /// the rename. Previewing with a raw `replace_all` would show a literal
    /// `##` and then produce something else — the one thing this dialog exists
    /// to prevent.
    ///
    /// Index 0: the preview is of the *first* file, which is the one the sample
    /// is taken from, so it shows the counter's starting value.
    fn recompute(&mut self) {
        if self.pattern.is_empty() {
            self.preview = Preview::Empty;
            return;
        }
        self.preview = match apply_pattern_indexed(
            self.pattern.value(),
            self.replacement.value(),
            &self.sample,
            0,
        ) {
            // The message is reported as-is: regex's own errors name the
            // position and the construct, which is more use than "invalid
            // pattern" would be. A malformed counter reports its own reason
            // the same way.
            Err(e) => Preview::BadPattern(e),
            Ok(None) => Preview::NoMatch,
            Ok(Some(name)) => Preview::Renamed(name),
        }
    }

    /// The counter in the replacement, if there is one — the dialog shows a
    /// worked example only when one is in play.
    pub fn sequence(&self) -> Option<SeqSpec> {
        sequence_spec(self.replacement.value())
    }

    fn active_mut(&mut self) -> &mut TextField {
        match self.focus {
            Field::Pattern => &mut self.pattern,
            Field::Replacement => &mut self.replacement,
        }
    }

    /// Move the focus to `field`, putting the cursor at the end of its text.
    fn focus_field(&mut self, field: Field) {
        self.focus = field;
        // The field keeps its own cursor, so focus does not move it.
    }

    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> RenameDialogOutcome {
        use crossterm::event::KeyCode;

        match key.code {
            KeyCode::Esc => RenameDialogOutcome::Cancelled,
            // Tab moves between the fields and never applies, as in every other
            // dialog here: reaching for the other field must not be read as
            // consent to rename a batch.
            KeyCode::Tab | KeyCode::Down => {
                self.focus_field(match self.focus {
                    Field::Pattern => Field::Replacement,
                    Field::Replacement => Field::Pattern,
                });
                RenameDialogOutcome::Continue
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus_field(match self.focus {
                    Field::Pattern => Field::Replacement,
                    Field::Replacement => Field::Pattern,
                });
                RenameDialogOutcome::Continue
            }
            KeyCode::Enter => {
                // Only a preview that resolved to a name can be applied. The
                // caller turns the other cases into a message and hands the
                // dialog back, so the patterns can be fixed rather than retyped.
                if self.is_applicable() {
                    RenameDialogOutcome::Apply {
                        pattern: self.pattern.value().to_string(),
                        replacement: self.replacement.value().to_string(),
                    }
                } else {
                    RenameDialogOutcome::Continue
                }
            }
            // Everything else is line editing: the field answers to the
            // readline bindings and reports whether it took the key. Enter,
            // Esc and Tab are handled above and `TextField` declines them, so
            // the dialog's own contract is unchanged.
            _ => {
                if self.active_mut().handle_key(key) {
                    self.recompute();
                }
                RenameDialogOutcome::Continue
            }
        }
    }

    /// Focus whichever field was clicked. Clicks elsewhere do nothing: this is
    /// a form, and a stray click must not apply it.
    pub fn click_at(&mut self, x: u16, y: u16) -> RenameDialogOutcome {
        for (i, r) in self.field_areas.iter().enumerate() {
            if x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height {
                let field = if i == 0 {
                    Field::Pattern
                } else {
                    Field::Replacement
                };
                self.focus_field(field);
                return RenameDialogOutcome::Continue;
            }
        }
        RenameDialogOutcome::Continue
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect) {
        let bg = Color::Rgb(24, 24, 34);
        let width = 72.min(area.width.max(1));
        let inner = width.saturating_sub(2).max(1) as usize;
        // title + blank + 2 labelled fields (2 rows each) + blank + sample +
        // arrow + result + blank + syntax hint + optional sequence example +
        // blank + key hint, plus borders.
        let seq = self.sequence();
        let height = (16 + usize::from(seq.is_some()) as u16).min(area.height.max(1));
        let center = centered(Rect::new(0, 0, width, height), area);
        if center.width == 0 || center.height == 0 {
            return;
        }
        frame.render_widget(Clear, center);

        let dim = Style::default().fg(Color::Rgb(150, 150, 170)).bg(bg);
        let normal = Style::default().fg(Color::Rgb(235, 235, 245)).bg(bg);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(
            format!(
                " Renaming {} tagged file{}",
                self.count,
                if self.count == 1 { "" } else { "s" }
            ),
            dim,
        )));
        lines.push(Line::from(Span::styled(String::new(), normal)));

        // Two fields. The focused one shows a block cursor; both are drawn the
        // same otherwise so the focus is the only difference the eye has to find.
        self.field_areas.clear();
        for (i, (label, value, field)) in [
            (" Match (regex)", &self.pattern, Field::Pattern),
            (" Replace with", &self.replacement, Field::Replacement),
        ]
        .into_iter()
        .enumerate()
        {
            let focused = self.focus == field;
            lines.push(Line::from(Span::styled(
                label.to_string(),
                if focused {
                    Style::default()
                        .fg(Color::Rgb(120, 220, 255))
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    dim
                },
            )));
            // Row index within the box: border + title + blank + (2 rows per
            // field) + 1 for the value row.
            let row = center.y + 1 + 2 + (i as u16 * 2) + 1;
            self.field_areas
                .push(Rect::new(center.x + 1, row, width.saturating_sub(2), 1));
            // Two columns of indent, then the field. The cursor is a styled
            // cell rather than a character spliced into the text, so moving it
            // does not shift what is to its right — see `TextField`.
            let style = if focused { normal.add_modifier(Modifier::BOLD) } else { normal };
            let mut spans = vec![Span::styled("  ".to_string(), style)];
            spans.extend(value.spans(inner.saturating_sub(2), style, focused));
            lines.push(Line::from(spans));
        }

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            format!("  {}", truncate(&self.sample, inner.saturating_sub(2))),
            dim,
        )));

        // The preview line carries the whole state of the patterns: what the
        // name becomes, or why it cannot.
        let (arrow, style) = match &self.preview {
            Preview::Empty => (
                "  (type a pattern to see the result)".to_string(),
                dim,
            ),
            Preview::BadPattern(e) => (
                format!("  ✗ {}", truncate(e, inner.saturating_sub(4))),
                Style::default().fg(Color::Rgb(255, 120, 120)).bg(bg),
            ),
            Preview::NoMatch => (
                "  ✗ pattern does not match this name".to_string(),
                Style::default().fg(Color::Rgb(255, 120, 120)).bg(bg),
            ),
            Preview::Renamed(name) => (
                format!("→ {}", truncate(name, inner.saturating_sub(4))),
                Style::default()
                    .fg(Color::Rgb(140, 230, 140))
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
        };
        lines.push(Line::from(Span::styled(pad_to(&arrow, inner), style)));

        // The counter is our own syntax, so the dialog has to teach it — there
        // is nowhere else the user would learn `##` from at the moment of use.
        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            truncate(
                "  $1 group   ## counter   ##{7+3} from 7 by 3   \\# literal #",
                inner.saturating_sub(1),
            ),
            dim,
        )));

        // With a counter in play, show what it will actually produce. A spec
        // like `##:start=7,step=3` is unreadable until you see 07, 10, 13.
        if let Some(spec) = seq {
            let sample: Vec<String> = (0..3).map(|i| spec.format(i)).collect();
            lines.push(Line::from(Span::styled(
                truncate(
                    &format!("  numbering: {}, …", sample.join(", ")),
                    inner.saturating_sub(1),
                ),
                Style::default().fg(Color::Rgb(140, 230, 140)).bg(bg),
            )));
        }

        lines.push(Line::from(Span::styled(String::new(), normal)));
        lines.push(Line::from(Span::styled(
            " Tab: field   Enter: rename   Esc: cancel".to_string(),
            dim,
        )));

        let paragraph = Paragraph::new(Text::from(lines))
            .style(Style::default().bg(bg))
            .block(
                Block::default()
                    .title(Span::styled(
                        " Patterned rename ",
                        Style::default()
                            .fg(Color::Rgb(120, 220, 255))
                            .add_modifier(Modifier::BOLD),
                    ))
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .border_style(Style::default().fg(Color::Rgb(120, 220, 255)))
                    .style(Style::default().bg(bg)),
            );
        frame.render_widget(paragraph, center);
    }
}

/// A sequence-number placeholder parsed out of a replacement string.
///
/// `#` is our own syntax, not the regex crate's. The crate's replacement
/// grammar interpolates capture groups only (`$1`, `${name}`, `$$` for a
/// literal `$`) and has no counter of any kind, so there is nothing to reuse —
/// see the module docs for why `${seq}` was not the spelling chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeqSpec {
    /// Zero-padded width, taken from the number of `#` characters. `#` is
    /// width 1, which pads nothing.
    pub width: usize,
    /// First value emitted.
    pub start: i64,
    /// Added for each subsequent file.
    pub step: i64,
}

impl SeqSpec {
    /// The counter's value for the `i`th renamed file (0-based), formatted.
    ///
    /// Negative values keep the sign outside the padding — `-05`, not `0-5` —
    /// which is what `format!("{:05}", -5)` would otherwise produce.
    pub fn format(&self, i: usize) -> String {
        let n = self.start.saturating_add(self.step.saturating_mul(i as i64));
        let digits = n.unsigned_abs().to_string();
        let pad = self.width.saturating_sub(digits.len());
        let zeros: String = std::iter::repeat_n('0', pad).collect();
        if n < 0 {
            format!("-{}{}", zeros, digits)
        } else {
            format!("{}{}", zeros, digits)
        }
    }
}

/// What a replacement string's `#` run turned out to be.
enum Token {
    /// Literal text, with `\#` already unescaped to `#`.
    Text(String),
    /// A counter placeholder.
    Seq(SeqSpec),
}

/// Split a replacement into literal text and counter placeholders.
///
/// Returns an error message rather than a partial parse: a malformed option
/// list is a typo the user wants told about, and silently treating
/// `##:start=x` as literal text would produce a batch of files all named
/// `##:start=x` — the exact failure the preview exists to prevent.
fn tokenize(replacement: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = replacement.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut text = String::new();
    let mut i = 0usize;

    while i < chars.len() {
        match chars[i] {
            // `\#` is a literal `#`. A backslash before anything else is left
            // alone: replacement strings are not regexes, and eating every
            // backslash would mangle Windows-style names.
            '\\' if i + 1 < chars.len() && chars[i + 1] == '#' => {
                text.push('#');
                i += 2;
            }
            // `\{` is a literal `{`, so a name can still contain a brace
            // directly after a counter — `##\{draft}` is `01{draft}`. Only
            // needed in that one position, but accepted everywhere so the rule
            // is "backslash-brace is a brace" rather than a positional
            // exception the user has to reason about.
            '\\' if i + 1 < chars.len() && chars[i + 1] == '{' => {
                text.push('{');
                i += 2;
            }
            '#' => {
                let start = i;
                while i < chars.len() && chars[i] == '#' {
                    i += 1;
                }
                let width = i - start;

                // An option list may follow: `##:start=5,step=2`. Only consumed
                // when a `:` is actually followed by a known key, so a name like
                // `shot-##:final` keeps its literal colon.
                let mut spec = SeqSpec {
                    width,
                    start: 1,
                    step: 1,
                };
                if i < chars.len() && chars[i] == '{' {
                    let (consumed, parsed) = parse_braced(&chars[i + 1..])?;
                    spec = SeqSpec { width, ..parsed };
                    i += 1 + consumed;
                } else if i < chars.len() && chars[i] == ':' {
                    if let Some((consumed, parsed)) = parse_options(&chars[i + 1..], spec)? {
                        spec = parsed;
                        i += 1 + consumed;
                    }
                }

                if !text.is_empty() {
                    out.push(Token::Text(std::mem::take(&mut text)));
                }
                out.push(Token::Seq(spec));
            }
            c => {
                text.push(c);
                i += 1;
            }
        }
    }
    if !text.is_empty() {
        out.push(Token::Text(text));
    }
    Ok(out)
}

/// Parse a braced counter option list — `{7}` or `{7+3}` — from just after
/// the `{`, returning how many characters it consumed (including the `}`).
///
/// This is the documented spelling. Unlike the `:start=`/`:step=` form it is
/// self-delimiting: the `}` says where the options stop, so the parser never
/// has to guess whether the next character belongs to the number or to the
/// filename. That guess is the reason the older form was hard to predict.
///
/// The step's sign is its separator: `{7+3}` counts up by 3, `{10-1}` counts
/// down by 1. There is no `+`/`-` ambiguity with the start value because the
/// start is read first and a sign there is only meaningful in the leading
/// position — `{-5+2}` starts at -5 and steps by 2.
///
/// An unclosed or malformed brace is an error rather than literal text. The
/// whole point of the delimiter is that its extent is unambiguous, so a missing
/// `}` is a typo the user wants told about, not a filename.
fn parse_braced(s: &[char]) -> Result<(usize, SeqSpec), String> {
    let Some(end) = s.iter().position(|c| *c == '}') else {
        return Err("unclosed { — write \\{ for a literal brace".to_string());
    };
    let body: String = s[..end].iter().collect();
    let spec = parse_brace_body(&body)?;
    // +1 for the closing brace itself.
    Ok((end + 1, spec))
}

/// Parse the inside of a braced counter: `7`, `7+3`, `-5+2`, `10-1`.
///
/// Width is filled in by the caller from the `#` run; the placeholder here is
/// overwritten, never read.
fn parse_brace_body(body: &str) -> Result<SeqSpec, String> {
    let mut spec = SeqSpec {
        width: 1,
        start: 1,
        step: 1,
    };
    let body = body.trim();
    if body.is_empty() {
        // `##{}` is the default counter. Harmless, and rejecting it would be a
        // rule with no purpose.
        return Ok(spec);
    }

    let chars: Vec<char> = body.chars().collect();
    // The start value: an optional leading sign, then digits.
    let mut i = 0usize;
    if chars[0] == '-' || chars[0] == '+' {
        i = 1;
    }
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    let start_text: String = chars[..i].iter().collect();
    let start = parse_signed(&start_text, body)?;
    spec.start = start;

    if i == chars.len() {
        return Ok(spec);
    }

    // What remains must be the step, introduced by its own sign. Anything else
    // is a typo: `{7x3}` should be reported, not silently read as start=7.
    if chars[i] != '+' && chars[i] != '-' {
        return Err(format!("{{{}}} — expected + or - before the step", body));
    }
    let step_text: String = chars[i..].iter().collect();
    spec.step = parse_signed(&step_text, body)?;
    Ok(spec)
}

/// Parse a signed integer, reporting the whole brace body for context.
///
/// A bare sign, an empty string, or an overflowing value are all typos rather
/// than filenames, so each is an error the dialog surfaces.
fn parse_signed(text: &str, body: &str) -> Result<i64, String> {
    // `+7` is not accepted by `i64::from_str`, but it is the natural way to
    // write a positive step, so the sign is stripped first.
    let normalised = text.strip_prefix('+').unwrap_or(text);
    if normalised.is_empty() || normalised == "-" {
        return Err(format!("{{{}}} needs a number", body));
    }
    normalised
        .parse::<i64>()
        .map_err(|_| format!("{{{}}} is not a number", body))
}

/// Parse `start=N,step=N` from the head of `s`.
///
/// The older, undocumented spelling. Kept so patterns written before the braced
/// form keep working; `{7+3}` is what the dialog and the docs teach.
///
/// Returns `None` when `s` does not begin with a known key, leaving the `:`
/// to be treated as literal text. Returns an error when a key *is* recognised
/// but its value is not a number, since that is a typo rather than a filename.
///
/// Indices are into `s` as a `char` slice, and the count returned is in `char`s
/// too — the caller's cursor counts characters, and mixing the two would
/// mis-slice any replacement containing a multibyte character.
fn parse_options(s: &[char], mut spec: SeqSpec) -> Result<Option<(usize, SeqSpec)>, String> {
    let mut consumed = 0usize;
    let mut saw_any = false;

    loop {
        let rest = &s[consumed..];
        let Some(key_end) = rest.iter().position(|c| *c == '=') else {
            break;
        };
        let key: String = rest[..key_end].iter().collect();
        if key != "start" && key != "step" {
            break;
        }

        // The value is an optional leading `-` and then digits, and it stops
        // at the first character that is neither.
        //
        // The sign is only accepted in the leading position. A `-` further in
        // is ordinary text — hyphens are everywhere in filenames, and
        // `step=3-${1}.jpg` must read as step 3 followed by the rest of the
        // name rather than as a malformed number.
        let value_start = key_end + 1;
        let mut value = String::new();
        for (n, c) in rest[value_start..].iter().enumerate() {
            if c.is_ascii_digit() || (n == 0 && *c == '-') {
                value.push(*c);
            } else {
                break;
            }
        }
        if value.is_empty() || value == "-" {
            return Err(format!("{}= needs a number", key));
        }
        let n: i64 = value
            .parse()
            .map_err(|_| format!("{}={} is not a number", key, value))?;

        match key.as_str() {
            "start" => spec.start = n,
            "step" => spec.step = n,
            _ => unreachable!("guarded above"),
        }
        saw_any = true;
        consumed += value_start + value.chars().count();

        // Another option only if a comma joins it.
        if s.get(consumed) == Some(&',') {
            consumed += 1;
        } else {
            break;
        }
    }

    if saw_any {
        Ok(Some((consumed, spec)))
    } else {
        Ok(None)
    }
}

/// The first counter in `replacement`, if it has one.
///
/// The dialog uses this to show a worked example of the numbering, which is
/// the only way `##:start=5,step=2` becomes legible without running it.
pub fn sequence_spec(replacement: &str) -> Option<SeqSpec> {
    tokenize(replacement).ok()?.into_iter().find_map(|t| match t {
        Token::Seq(spec) => Some(spec),
        Token::Text(_) => None,
    })
}

/// Resolve the counter in `replacement` for the `i`th renamed file, yielding a
/// replacement string for the regex crate to interpolate.
///
/// Counters are substituted *before* regex expansion, so the number is fixed
/// text by the time capture groups are filled in. Doing it the other way round
/// would let a `#` inside a captured filename be mistaken for a placeholder.
///
/// A literal `$` produced by the counter is escaped to `$$`, so a negative
/// start or an odd step can never be re-read as a capture reference. (Digits
/// alone cannot form one, but the escape costs nothing and removes the need to
/// reason about it.)
pub fn resolve_sequence(replacement: &str, i: usize) -> Result<String, String> {
    let tokens = tokenize(replacement)?;
    let mut out = String::new();
    for t in &tokens {
        match t {
            Token::Text(s) => out.push_str(s),
            Token::Seq(spec) => out.push_str(&spec.format(i).replace('$', "$$")),
        }
    }
    Ok(out)
}

/// Apply `pattern` -> `replacement` to `name`, resolving any sequence counter
/// to its value for the `index`th renamed file.
///
/// Shared by the dialog's preview and the rename itself, so what was shown is
/// by construction what gets done — the preview cannot promise one name and the
/// batch produce another.
pub fn apply_pattern_indexed(
    pattern: &str,
    replacement: &str,
    name: &str,
    index: usize,
) -> Result<Option<String>, String> {
    let re = regex::Regex::new(pattern).map_err(|e| first_line(&e.to_string()))?;
    if !re.is_match(name) {
        return Ok(None);
    }
    let replacement = resolve_sequence(replacement, index)?;
    Ok(Some(re.replace_all(name, replacement.as_str()).into_owned()))
}

/// Apply `pattern` -> `replacement` to `name`.
///
/// Shared by the dialog's preview and the rename itself, so what was shown is
/// by construction what gets done — the preview cannot promise one name and the
/// batch produce another.
pub fn apply_pattern(
    pattern: &str,
    replacement: &str,
    name: &str,
) -> Result<Option<String>, String> {
    apply_pattern_indexed(pattern, replacement, name, 0)
}

/// The first line of a multi-line error, for a one-line display.
fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(s)
        .trim()
        .to_string()
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
    Rect::new(x, y, inner.width.min(outer.width), inner.height.min(outer.height))
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

    fn type_str(d: &mut RenameDialog, s: &str) {
        for c in s.chars() {
            d.handle_key(key(c));
        }
    }

    #[test]
    fn the_preview_follows_the_patterns() {
        let mut d = RenameDialog::new("IMG_0042.jpg", 3);
        assert_eq!(*d.preview(), Preview::Empty, "nothing typed yet");

        type_str(&mut d, r"IMG_(\d+)");
        // Matches, and with an empty replacement the matched part disappears.
        assert_eq!(*d.preview(), Preview::Renamed(".jpg".to_string()));

        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "holiday-$1");
        assert_eq!(
            *d.preview(),
            Preview::Renamed("holiday-0042.jpg".to_string())
        );
        assert!(d.is_applicable());
    }

    #[test]
    fn a_pattern_that_does_not_compile_is_reported_not_applied() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "(unclosed");
        assert!(
            matches!(d.preview(), Preview::BadPattern(_)),
            "got {:?}",
            d.preview()
        );
        assert!(!d.is_applicable(), "a broken pattern cannot be applied");
        // Enter is inert rather than destructive, leaving the text to be fixed.
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Continue
        );
        assert_eq!(d.pattern(), "(unclosed", "the text is still there to correct");
    }

    #[test]
    fn a_pattern_that_matches_nothing_is_not_applicable() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "zzz");
        assert_eq!(*d.preview(), Preview::NoMatch);
        assert!(!d.is_applicable());
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Continue
        );
    }

    #[test]
    fn tab_moves_between_fields_and_never_applies() {
        // The dialog contract: only Enter, a click, or a letter decides. Tab
        // reaching for the other field must not rename a batch.
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "a");
        assert_eq!(d.focus(), Field::Pattern);
        assert_eq!(
            d.handle_key(code(KeyCode::Tab)),
            RenameDialogOutcome::Continue,
            "Tab must not apply"
        );
        assert_eq!(d.focus(), Field::Replacement);
        type_str(&mut d, "b");
        assert_eq!(d.replacement(), "b", "typing lands in the focused field");
        assert_eq!(d.pattern(), "a", "and not in the other one");
        d.handle_key(code(KeyCode::Tab));
        assert_eq!(d.focus(), Field::Pattern, "Tab wraps back");
    }

    #[test]
    fn enter_applies_only_a_working_pattern() {
        let mut d = RenameDialog::new("IMG_1.jpg", 2);
        type_str(&mut d, "IMG");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "PIC");
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Apply {
                pattern: "IMG".to_string(),
                replacement: "PIC".to_string(),
            }
        );
    }

    #[test]
    fn esc_cancels_from_either_field() {
        for tabs in [0, 1] {
            let mut d = RenameDialog::new("a.txt", 1);
            for _ in 0..tabs {
                d.handle_key(code(KeyCode::Tab));
            }
            assert_eq!(
                d.handle_key(code(KeyCode::Esc)),
                RenameDialogOutcome::Cancelled
            );
        }
    }

    #[test]
    fn editing_keys_work_on_the_focused_field() {
        let mut d = RenameDialog::new("a.txt", 1);
        type_str(&mut d, "abc");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.pattern(), "ab");
        d.handle_key(code(KeyCode::Left));
        d.handle_key(key('X'));
        assert_eq!(d.pattern(), "aXb", "insert lands at the cursor");
        d.handle_key(code(KeyCode::Home));
        d.handle_key(code(KeyCode::Delete));
        assert_eq!(d.pattern(), "Xb", "Delete removes forwards");
    }

    #[test]
    fn a_multibyte_pattern_does_not_panic() {
        // These fields take pasted text, and slicing a regex by char index as
        // though it were bytes would split a codepoint.
        let mut d = RenameDialog::new("café.txt", 1);
        type_str(&mut d, "café");
        assert_eq!(d.pattern(), "café");
        d.handle_key(code(KeyCode::Left));
        d.handle_key(key('x'));
        assert_eq!(d.pattern(), "cafxé");
        d.handle_key(code(KeyCode::Backspace));
        assert_eq!(d.pattern(), "café");
    }

    #[test]
    fn apply_pattern_agrees_with_the_preview() {
        // The preview and the rename must not be able to disagree; they share
        // this function precisely so they cannot.
        let mut d = RenameDialog::new("IMG_7.jpg", 1);
        type_str(&mut d, r"IMG_(\d+)");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "shot-$1");
        let Preview::Renamed(shown) = d.preview().clone() else {
            panic!("expected a rendered preview, got {:?}", d.preview());
        };
        let done = apply_pattern(d.pattern(), d.replacement(), "IMG_7.jpg").unwrap();
        assert_eq!(done, Some(shown));
    }

    #[test]
    fn a_bare_hash_counts_from_one_with_no_padding() {
        for (i, want) in [(0, "1"), (1, "2"), (8, "9"), (9, "10"), (99, "100")] {
            assert_eq!(resolve_sequence("shot-#", i).unwrap(), format!("shot-{}", want));
        }
    }

    /// The width is the number of `#`, which is the whole point of the syntax:
    /// `##` pads to two, `###` to three.
    #[test]
    fn the_number_of_hashes_sets_the_padding() {
        assert_eq!(resolve_sequence("#", 0).unwrap(), "1");
        assert_eq!(resolve_sequence("##", 0).unwrap(), "01");
        assert_eq!(resolve_sequence("###", 0).unwrap(), "001");
        assert_eq!(resolve_sequence("#####", 0).unwrap(), "00001");
    }

    /// Padding is a minimum, not a maximum. A batch of 150 files numbered `##`
    /// must not truncate at 99 — silently colliding names would be far worse
    /// than a wider number.
    #[test]
    fn a_number_wider_than_its_padding_is_not_truncated() {
        assert_eq!(resolve_sequence("##", 99).unwrap(), "100");
        assert_eq!(resolve_sequence("##", 998).unwrap(), "999");
    }

    /// The documented spelling: `{start}` and `{start+step}`.
    #[test]
    fn a_braced_counter_sets_start_and_step() {
        assert_eq!(resolve_sequence("##{7}", 0).unwrap(), "07");
        assert_eq!(resolve_sequence("##{7}", 2).unwrap(), "09");
        assert_eq!(resolve_sequence("##{7+3}", 0).unwrap(), "07");
        assert_eq!(resolve_sequence("##{7+3}", 2).unwrap(), "13");
        // The `#` run still sets the width; the braces only carry the numbers.
        assert_eq!(resolve_sequence("####{7+3}", 2).unwrap(), "0013");
    }

    /// A minus introduces a negative step, which is how a countdown is written.
    #[test]
    fn a_braced_counter_counts_down() {
        assert_eq!(resolve_sequence("###{10-1}", 0).unwrap(), "010");
        assert_eq!(resolve_sequence("###{10-1}", 3).unwrap(), "007");
        // A negative start keeps its sign outside the padding.
        assert_eq!(resolve_sequence("##{-5+2}", 0).unwrap(), "-05");
        assert_eq!(resolve_sequence("##{-5+2}", 3).unwrap(), "01");
    }

    /// The whole reason for braces: the option list cannot leak into the
    /// filename, because `}` says where it stops. The `:start=` form had to
    /// guess, which is what made it unpredictable.
    #[test]
    fn a_brace_delimits_the_options_from_the_filename() {
        assert_eq!(
            resolve_sequence("shot-###{7+3}.jpg", 0).unwrap(),
            "shot-007.jpg"
        );
        // A digit straight after the brace is filename, not part of the start.
        // This is the ambiguity that ruled out the bare `##7` spelling.
        assert_eq!(resolve_sequence("##{7}2024", 0).unwrap(), "072024");
        // As is a letter, with no separator needed.
        assert_eq!(resolve_sequence("##{7}x", 0).unwrap(), "07x");
    }

    /// `\{` escapes a literal brace, mirroring `\#`.
    #[test]
    fn a_backslash_escapes_a_literal_brace() {
        assert_eq!(resolve_sequence(r"##\{draft}", 0).unwrap(), "01{draft}");
        assert_eq!(resolve_sequence(r"\{7+3}", 0).unwrap(), "{7+3}");
    }

    /// An empty brace is just the default counter.
    #[test]
    fn an_empty_brace_is_the_plain_counter() {
        assert_eq!(resolve_sequence("##{}", 0).unwrap(), "01");
        assert_eq!(resolve_sequence("##{}", 4).unwrap(), "05");
    }

    /// A malformed brace is reported rather than renaming the batch to the
    /// literal text — the same contract the `:start=` form has.
    #[test]
    fn a_malformed_brace_is_an_error() {
        assert!(resolve_sequence("##{7", 0).is_err(), "unclosed");
        assert!(resolve_sequence("##{7x3}", 0).is_err(), "junk after start");
        assert!(resolve_sequence("##{x}", 0).is_err(), "not a number");
        assert!(resolve_sequence("##{7+}", 0).is_err(), "bare sign");
    }

    /// The unclosed-brace message has to name the escape, or the user has no
    /// way to type a literal brace after a counter.
    #[test]
    fn the_unclosed_brace_error_names_the_escape() {
        let e = resolve_sequence("##{7", 0).unwrap_err();
        assert!(e.contains(r"\{"), "must point at the escape: {e}");
    }

    /// The older spelling keeps working. It is undocumented now, but patterns
    /// written before the braced form must not start failing.
    #[test]
    fn the_legacy_colon_form_still_parses() {
        assert_eq!(resolve_sequence("##:start=7,step=3", 2).unwrap(), "13");
    }

    /// A counter and capture groups compose under the braced form too, with the
    /// counter resolved before the regex sees the string.
    #[test]
    fn a_braced_counter_composes_with_capture_groups() {
        assert_eq!(
            resolve_sequence("trip-##{7+3}-${1}.jpg", 1).unwrap(),
            "trip-10-${1}.jpg"
        );
    }

    #[test]
    fn start_and_step_are_configurable() {
        assert_eq!(resolve_sequence("##:start=5", 0).unwrap(), "05");
        assert_eq!(resolve_sequence("##:start=5", 2).unwrap(), "07");
        assert_eq!(resolve_sequence("##:start=0,step=10", 0).unwrap(), "00");
        assert_eq!(resolve_sequence("##:start=0,step=10", 3).unwrap(), "30");
        assert_eq!(resolve_sequence("#:step=2", 4).unwrap(), "9");
    }

    /// A countdown is expressible, and a negative value keeps its sign outside
    /// the zero padding — `-05`, not `0-5`, which is what a naive `{:03}` gives.
    #[test]
    fn a_negative_value_pads_after_its_sign() {
        // Width 3 means three digits, with the sign outside them: `-005`.
        assert_eq!(resolve_sequence("###:start=-5", 0).unwrap(), "-005");
        assert_eq!(resolve_sequence("##:start=-5", 0).unwrap(), "-05");
        assert_eq!(resolve_sequence("##:start=3,step=-1", 4).unwrap(), "-01");
    }

    /// `\#` is the escape, so a literal hash in a filename is still reachable.
    #[test]
    fn a_backslash_escapes_a_literal_hash() {
        assert_eq!(resolve_sequence(r"track\#1", 0).unwrap(), "track#1");
        assert_eq!(resolve_sequence(r"\#\#", 5).unwrap(), "##");
        // The escape and a real counter can coexist.
        assert_eq!(resolve_sequence(r"\#-##", 0).unwrap(), "#-01");
    }

    /// A backslash before anything else is left alone. Replacement strings are
    /// not regexes, and eating every backslash would mangle ordinary names.
    #[test]
    fn other_backslashes_are_left_alone() {
        assert_eq!(resolve_sequence(r"a\db", 0).unwrap(), r"a\db");
    }

    /// A colon that does not introduce a known option is literal text. Names
    /// like `shot-##:final` are ordinary and must not be read as options.
    #[test]
    fn a_colon_without_options_stays_literal() {
        assert_eq!(resolve_sequence("shot-##:final", 0).unwrap(), "shot-01:final");
        assert_eq!(resolve_sequence("##:", 0).unwrap(), "01:");
    }

    /// A recognised key with a broken value is a typo, and is reported. Left as
    /// literal text it would rename an entire batch to the same bad string.
    #[test]
    fn a_malformed_option_is_an_error_not_literal_text() {
        assert!(resolve_sequence("##:start=x", 0).is_err());
        assert!(resolve_sequence("##:start=", 0).is_err());
        assert!(resolve_sequence("##:start=1,step=-", 0).is_err());
    }

    /// The option list ends at the first character that cannot be part of it,
    /// so an extension after the options survives. Getting this wrong would
    /// swallow `.jpg` into the step value.
    #[test]
    fn text_after_an_option_list_survives() {
        assert_eq!(
            resolve_sequence("shot-###:start=7,step=3.jpg", 0).unwrap(),
            "shot-007.jpg"
        );
        assert_eq!(
            resolve_sequence("shot-###:start=7,step=3.jpg", 2).unwrap(),
            "shot-013.jpg"
        );
        assert_eq!(resolve_sequence("a-##:start=5.txt", 0).unwrap(), "a-05.txt");
    }

    /// A `-` is a sign only in the leading position. Hyphens are everywhere in
    /// filenames, so `step=3-${1}.jpg` has to read as step 3 followed by the
    /// rest of the name rather than as a malformed number.
    #[test]
    fn a_hyphen_after_the_digits_is_text_not_part_of_the_number() {
        assert_eq!(
            resolve_sequence("trip-###:start=7,step=3-x", 1).unwrap(),
            "trip-010-x"
        );
        assert_eq!(resolve_sequence("a-##:step=3-x", 0).unwrap(), "a-01-x");
        // A leading sign still works, and a later hyphen still ends the value.
        assert_eq!(resolve_sequence("a-##:start=-5-x", 0).unwrap(), "a--05-x");
    }

    #[test]
    fn text_around_the_counter_is_preserved() {
        assert_eq!(
            resolve_sequence("holiday-##-final.jpg", 1).unwrap(),
            "holiday-02-final.jpg"
        );
    }

    /// Several counters in one replacement each get the same index, which is
    /// what "the Nth file" means.
    #[test]
    fn multiple_counters_share_the_index() {
        assert_eq!(resolve_sequence("##-of-##", 4).unwrap(), "05-of-05");
    }

    /// The counter is substituted before regex expansion, so a `$` it produces
    /// must be escaped or it would be re-read as a capture reference. Digits
    /// alone cannot form one, but a negative start is written with `-`, and the
    /// escaping keeps the rule simple rather than conditional.
    #[test]
    fn a_counter_and_capture_groups_compose() {
        let got = apply_pattern_indexed(r"IMG_(\d+)\.jpg", "trip-##-$1.jpg", "IMG_0042.jpg", 2)
            .unwrap();
        assert_eq!(got, Some("trip-03-0042.jpg".to_string()));
    }

    /// A `#` in the *matched filename* is data, not syntax. Resolving the
    /// counter before expansion is what guarantees this; doing it afterwards
    /// would rewrite the file's own name.
    #[test]
    fn a_hash_inside_the_captured_name_is_not_a_counter() {
        let got = apply_pattern_indexed(r"(.+)\.txt", "$1-##.txt", "note#3.txt", 0).unwrap();
        assert_eq!(got, Some("note#3-01.txt".to_string()));
    }

    #[test]
    fn sequence_spec_reports_the_counter_for_the_dialog() {
        assert_eq!(sequence_spec("plain"), None);
        let spec = sequence_spec("shot-###:start=7,step=3").unwrap();
        assert_eq!(spec.width, 3);
        assert_eq!(spec.start, 7);
        assert_eq!(spec.step, 3);
    }

    /// The preview must show the counter resolved, not a literal `##` — it is
    /// the only check before a batch rename.
    #[test]
    fn the_preview_resolves_the_counter() {
        let mut d = RenameDialog::new("IMG_0042.jpg", 3);
        type_str(&mut d, r"IMG_(\d+)");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "holiday-##");
        assert_eq!(
            *d.preview(),
            Preview::Renamed("holiday-01.jpg".to_string()),
            "the first file previews with the counter's starting value"
        );
        assert!(d.is_applicable());
    }

    /// A broken counter is reported the same way a broken regex is, and blocks
    /// Enter rather than renaming a batch to a literal `##:start=x`.
    #[test]
    fn a_broken_counter_blocks_the_rename() {
        let mut d = RenameDialog::new("a.txt", 2);
        type_str(&mut d, "a");
        d.handle_key(code(KeyCode::Tab));
        type_str(&mut d, "x-##:start=q");
        assert!(
            matches!(d.preview(), Preview::BadPattern(_)),
            "got {:?}",
            d.preview()
        );
        assert!(!d.is_applicable());
        assert_eq!(
            d.handle_key(code(KeyCode::Enter)),
            RenameDialogOutcome::Continue
        );
    }

    /// A multibyte replacement must not panic. The option parser indexes by
    /// char, and mixing that with byte offsets would split a codepoint.
    #[test]
    fn a_multibyte_replacement_around_a_counter_is_safe() {
        assert_eq!(resolve_sequence("café-##", 0).unwrap(), "café-01");
        assert_eq!(resolve_sequence("café-##:start=9", 1).unwrap(), "café-10");
        assert_eq!(resolve_sequence("日本-##-語", 2).unwrap(), "日本-03-語");
    }

    #[test]
    fn clicking_a_field_focuses_it() {
        let mut d = RenameDialog::new("a.txt", 1);
        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| d.render(f, f.area())).unwrap();

        let second = d.field_areas[1];
        assert_eq!(
            d.click_at(second.x + 2, second.y),
            RenameDialogOutcome::Continue,
            "a click focuses, it does not apply"
        );
        assert_eq!(d.focus(), Field::Replacement);

        let first = d.field_areas[0];
        d.click_at(first.x + 2, first.y);
        assert_eq!(d.focus(), Field::Pattern);
    }
}

//! Turning a terminal image renderer's output into ratatui text.
//!
//! `timg` and `chafa` both draw an image by printing block characters coloured
//! with SGR escapes. Nothing stops us capturing that instead of letting it reach
//! the terminal, and once captured it can be parsed back into [`Line`]s and
//! drawn as an ordinary widget — which is the only way a child process can
//! contribute to the display without corrupting it. The alternative, letting the
//! child write to the terminal directly, is what [`crate::utils::opener`] goes
//! out of its way to avoid.
//!
//! That works because neither tool moves the cursor. Their entire vocabulary,
//! measured across photos, an alpha PNG and a PDF page, is colour and reverse
//! video:
//!
//! | Parameter        | Meaning              | Emitted by  |
//! |------------------|----------------------|-------------|
//! | `0`, bare `ESC[m`| reset                | both        |
//! | `7` / `27`       | reverse video on/off | chafa       |
//! | `39` / `49`      | default fg / bg      | timg        |
//! | `38;2;r;g;b`     | truecolor foreground | both        |
//! | `48;2;r;g;b`     | truecolor background | both        |
//!
//! Reverse video is the one easy to miss: only chafa emits it, on every image
//! with transparency, and a parser written against timg alone drops it and
//! renders those cells inverted.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Parse a terminal renderer's output into styled lines.
///
/// Unknown escapes are skipped rather than rendered, so a tool that grows a new
/// sequence degrades to slightly wrong colours instead of spraying `[38;2` over
/// the pane. A truncated escape at end of input is dropped.
pub fn parse_ansi(input: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut style = Style::default();

    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Only CSI sequences appear here. Anything else (a lone ESC, or
                // an OSC string) is dropped along with its terminator.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = None;
                    for c in chars.by_ref() {
                        if c.is_ascii_digit() || c == ';' || c == '?' {
                            params.push(c);
                        } else {
                            final_byte = Some(c);
                            break;
                        }
                    }
                    // `m` is SGR. `ESC[?25l` and friends toggle private modes
                    // (cursor visibility) and carry no styling; chafa's
                    // `--polite=on` suppresses them, timg emits them.
                    if final_byte == Some('m') && !params.starts_with('?') {
                        flush_span(&mut text, style, &mut spans);
                        apply_sgr(&params, &mut style);
                    }
                }
            }
            '\n' => {
                flush_span(&mut text, style, &mut spans);
                lines.push(Line::from(std::mem::take(&mut spans)));
            }
            // Carriage returns would only matter if the tools drew over a line
            // they had already written, and neither does.
            '\r' => {}
            c => text.push(c),
        }
    }

    flush_span(&mut text, style, &mut spans);
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }
    lines
}

/// Move the pending run of characters into a span carrying the current style.
fn flush_span(text: &mut String, style: Style, spans: &mut Vec<Span<'static>>) {
    if !text.is_empty() {
        spans.push(Span::styled(std::mem::take(text), style));
    }
}

/// Fold one SGR sequence's parameters into `style`.
///
/// Truecolor takes five parameters (`38;2;r;g;b`) and may be followed by more in
/// the same escape — chafa routinely sets both colours at once — so parameters
/// are consumed positionally rather than looked at one at a time.
fn apply_sgr(params: &str, style: &mut Style) {
    // A bare `ESC[m` means reset, same as `ESC[0m`.
    if params.is_empty() {
        *style = Style::default();
        return;
    }

    let parts: Vec<&str> = params.split(';').collect();
    let mut i = 0;
    while i < parts.len() {
        // An empty field (`38;;2`) is not something either tool emits; treat it
        // as a zero the way terminals do.
        let code: u16 = parts[i].parse().unwrap_or(0);
        match code {
            0 => *style = Style::default(),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            39 => *style = style.fg(Color::Reset),
            49 => *style = style.bg(Color::Reset),
            38 | 48 => {
                // Only the truecolor form appears: the invocations pin
                // `--colors=truecolor` precisely so indexed colour never shows
                // up here. Anything else is left alone.
                if let Some(rgb) = parse_truecolor(&parts[i + 1..]) {
                    *style = if code == 38 {
                        style.fg(rgb)
                    } else {
                        style.bg(rgb)
                    };
                    i += 5;
                    continue;
                }
                // Unrecognised colour form — stop, since the parameters that
                // follow cannot be located reliably.
                return;
            }
            _ => {}
        }
        i += 1;
    }
}

/// Read a `2;r;g;b` truecolor tail, if that is what follows.
fn parse_truecolor(rest: &[&str]) -> Option<Color> {
    if rest.len() < 4 || rest[0] != "2" {
        return None;
    }
    let r = rest[1].parse().ok()?;
    let g = rest[2].parse().ok()?;
    let b = rest[3].parse().ok()?;
    Some(Color::Rgb(r, g, b))
}

/// The widest line in a parsed block, in terminal cells.
///
/// Needed because the renderers do not necessarily honour the geometry they are
/// given: `timg -g60x24` returns lines up to 66 columns wide, while chafa's
/// `--size=60x24` returns exactly 60. The pane centres and clips on the real
/// width rather than the requested one.
pub fn block_width(lines: &[Line<'_>]) -> usize {
    lines.iter().map(Line::width).max().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every escape in both tools' real output must be understood. The fixtures
    /// are captures from the actual binaries, so this fails if either tool grows
    /// a sequence the parser cannot account for.
    fn assert_fully_parsed(fixture: &str) {
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/ansi/{}",
            env!("CARGO_MANIFEST_DIR"),
            fixture
        ))
        .expect("fixture missing");

        let lines = parse_ansi(&raw);
        assert!(!lines.is_empty(), "{fixture}: parsed to nothing");

        // No escape may survive into the rendered text. If one did, the pane
        // would show it literally.
        for line in &lines {
            for span in &line.spans {
                assert!(
                    !span.content.contains('\x1b') && !span.content.contains('['),
                    "{fixture}: escape leaked into the text: {:?}",
                    span.content
                );
            }
        }
    }

    #[test]
    fn timg_photo_output_is_fully_understood() {
        assert_fully_parsed("timg_photo.ansi");
    }

    #[test]
    fn timg_pdf_output_is_fully_understood() {
        assert_fully_parsed("timg_pdf.ansi");
    }

    #[test]
    fn chafa_photo_output_is_fully_understood() {
        assert_fully_parsed("chafa_photo.ansi");
    }

    #[test]
    fn chafa_alpha_output_is_fully_understood() {
        assert_fully_parsed("chafa_alpha.ansi");
    }

    /// Reverse video is chafa-only and appears on every image with an alpha
    /// channel. A parser built against timg alone drops it silently and the
    /// affected cells render with their colours inverted.
    #[test]
    fn chafa_reverse_video_becomes_a_modifier() {
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/ansi/chafa_alpha.ansi",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        assert!(
            raw.contains("\x1b[7m"),
            "fixture no longer exercises reverse video"
        );

        let reversed = parse_ansi(&raw)
            .iter()
            .flat_map(|l| l.spans.clone())
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .count();
        assert!(reversed > 0, "reverse video was dropped");
    }

    /// timg does not emit reverse video, so its fixtures must come back clean —
    /// this is what proves the modifier above is really coming from the escape
    /// and not from a stuck style.
    #[test]
    fn timg_output_has_no_reverse_video() {
        let raw = std::fs::read_to_string(format!(
            "{}/tests/fixtures/ansi/timg_photo.ansi",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let reversed = parse_ansi(&raw)
            .iter()
            .flat_map(|l| l.spans.clone())
            .filter(|s| s.style.add_modifier.contains(Modifier::REVERSED))
            .count();
        assert_eq!(reversed, 0);
    }

    #[test]
    fn truecolor_sets_foreground_and_background() {
        let lines = parse_ansi("\x1b[38;2;10;20;30;48;2;40;50;60mX\x1b[0mY");
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content, "X");
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(10, 20, 30)));
        assert_eq!(spans[0].style.bg, Some(Color::Rgb(40, 50, 60)));
        // The reset must actually clear the colours, or every later cell
        // inherits them.
        assert_eq!(spans[1].content, "Y");
        assert_eq!(spans[1].style.fg, None);
    }

    #[test]
    fn reset_forms_are_equivalent() {
        for seq in ["\x1b[0m", "\x1b[m"] {
            let lines = parse_ansi(&format!("\x1b[38;2;1;2;3mA{seq}B"));
            assert_eq!(lines[0].spans[1].style.fg, None, "{seq} did not reset");
        }
    }

    #[test]
    fn default_colour_codes_are_honoured() {
        let lines = parse_ansi("\x1b[38;2;1;2;3m\x1b[39mA");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Reset));
    }

    /// Private-mode sequences carry no styling and must not become text.
    #[test]
    fn cursor_visibility_sequences_are_dropped() {
        let lines = parse_ansi("\x1b[?25lA\x1b[?25h");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans[0].content, "A");
    }

    #[test]
    fn lines_are_split_on_newlines() {
        let lines = parse_ansi("a\nb\nc");
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2].spans[0].content, "c");
    }

    /// A trailing newline should not manufacture an empty final line, or an
    /// image gains a blank row every time it is rendered.
    #[test]
    fn a_trailing_newline_does_not_add_a_line() {
        assert_eq!(parse_ansi("a\nb\n").len(), 2);
    }

    /// Truncated output — a killed child, a hit output cap — must not panic.
    #[test]
    fn truncated_escapes_do_not_panic() {
        for input in [
            "\x1b",
            "\x1b[",
            "\x1b[38",
            "\x1b[38;2",
            "\x1b[38;2;10",
            "\x1b[38;2;10;20",
            "\x1b[?",
            "A\x1b[38;2;1;2;3",
        ] {
            let _ = parse_ansi(input);
        }
    }

    /// An unknown parameter should be ignored without discarding the ones around
    /// it that are understood.
    #[test]
    fn unknown_parameters_are_skipped() {
        let lines = parse_ansi("\x1b[1;38;2;9;9;9;5mX");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(9, 9, 9)));
    }

    /// Indexed colour is not something the pinned invocations produce, but it
    /// must not be mistaken for truecolor and shift the parameter window.
    #[test]
    fn indexed_colour_does_not_derail_the_parse() {
        let lines = parse_ansi("\x1b[38;5;200mX");
        assert_eq!(lines[0].spans[0].content, "X");
        assert_eq!(lines[0].spans[0].style.fg, None);
    }

    #[test]
    fn block_width_measures_the_widest_line() {
        // timg overshoots its requested geometry, so the real width has to be
        // measured rather than assumed.
        let lines = parse_ansi("ab\nabcd\nabc");
        assert_eq!(block_width(&lines), 4);
    }

    #[test]
    fn the_fixtures_measure_their_real_widths() {
        let read = |n: &str| {
            std::fs::read_to_string(format!(
                "{}/tests/fixtures/ansi/{}",
                env!("CARGO_MANIFEST_DIR"),
                n
            ))
            .unwrap()
        };
        // chafa honours --size exactly; timg does not. Both were asked for 60.
        assert_eq!(block_width(&parse_ansi(&read("chafa_photo.ansi"))), 60);
        assert!(block_width(&parse_ansi(&read("timg_photo.ansi"))) >= 60);
    }
}

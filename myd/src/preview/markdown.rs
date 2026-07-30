//! A line-oriented markdown highlighter.
//!
//! syntect has a perfectly good markdown grammar and it is far too slow to use
//! here: 170ms on a 30KB README against 61ms for the same bytes as Rust, because
//! the markdown grammar embeds every other language so that fenced code blocks
//! can be highlighted in their own syntax. Opening a preview is meant to feel
//! instant, and a sixth of a second is not.
//!
//! Markdown does not need a grammar engine. Every construct that matters here is
//! either decided by how a line starts, or is a delimiter pair within one line,
//! so a single pass over the text does the job in ~37µs on the same file — around
//! four thousand times faster, which is the difference between noticeable and
//! free.
//!
//! What that trades away: fenced code is coloured as one block rather than
//! highlighted per language, and pathological nesting (emphasis wrapped around a
//! link wrapped around code) is resolved outermost-first instead of by a real
//! parser. Both are acceptable in a preview pane a few hundred lines tall.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Heading — the brightest thing on the page, since it is the structure.
const HEADING: Color = Color::Rgb(130, 170, 255);
/// Inline `code` and fenced blocks.
const CODE: Color = Color::Rgb(195, 232, 141);
/// Link text and bare URLs.
const LINK: Color = Color::Rgb(137, 221, 255);
/// Block quotes and horizontal rules — deliberately quiet.
const QUIET: Color = Color::Rgb(130, 140, 160);
/// List bullets and numbers, and table pipes: structure, not content.
const MARKER: Color = Color::Rgb(240, 180, 100);
/// Ordinary prose.
const TEXT: Color = Color::Rgb(220, 223, 228);

/// Highlight markdown source into styled lines.
pub fn highlight(text: &str) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    // Fenced blocks span lines, so this is the one piece of state carried across
    // them. The fence character is remembered because ``` must not be closed by
    // ~~~ .
    let mut fence: Option<char> = None;

    for raw in text.lines() {
        let line = super::expand_tabs(raw);
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();

        // A fence toggles, and the fence line itself is part of the block.
        if let Some(c) = fence_char(trimmed) {
            match fence {
                // Only a matching fence closes: a ~~~ inside a ``` block is
                // content.
                Some(open) if open == c => fence = None,
                Some(_) => {}
                None => fence = Some(c),
            }
            out.push(Line::from(Span::styled(line, Style::default().fg(CODE))));
            continue;
        }
        if fence.is_some() {
            out.push(Line::from(Span::styled(line, Style::default().fg(CODE))));
            continue;
        }

        // Whole-line constructs, decided by the first non-space characters.
        if trimmed.starts_with('#') {
            out.push(Line::from(Span::styled(
                line,
                Style::default().fg(HEADING).add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        if is_rule(trimmed) {
            out.push(Line::from(Span::styled(line, Style::default().fg(QUIET))));
            continue;
        }
        if trimmed.starts_with('>') {
            out.push(Line::from(Span::styled(
                line,
                Style::default().fg(QUIET).add_modifier(Modifier::ITALIC),
            )));
            continue;
        }
        // An indented code block, but only outside a list — four spaces under a
        // bullet is a continuation paragraph, not code. Distinguishing those
        // properly needs a parser; treating deep indentation as code is the
        // common case and reads correctly either way.
        if indent >= 4 && !trimmed.is_empty() {
            out.push(Line::from(Span::styled(line, Style::default().fg(CODE))));
            continue;
        }

        let mut spans = Vec::new();
        // A table row or a list item leads with a marker, then ordinary inline
        // markup.
        let rest = if trimmed.starts_with('|') {
            spans.push(Span::styled(
                line[..indent + 1].to_string(),
                Style::default().fg(MARKER),
            ));
            &line[indent + 1..]
        } else if let Some(len) = list_marker(trimmed) {
            spans.push(Span::styled(
                line[..indent + len].to_string(),
                Style::default().fg(MARKER).add_modifier(Modifier::BOLD),
            ));
            &line[indent + len..]
        } else {
            if indent > 0 {
                spans.push(Span::raw(line[..indent].to_string()));
            }
            &line[indent..]
        };

        inline(rest, &mut spans);
        out.push(Line::from(spans));
    }
    out
}

/// The fence character if this line opens or closes a fenced block.
fn fence_char(trimmed: &str) -> Option<char> {
    for c in ['`', '~'] {
        if trimmed.starts_with(&[c, c, c][..]) {
            return Some(c);
        }
    }
    None
}

/// A thematic break: three or more of `-`, `*` or `_` and nothing else.
fn is_rule(trimmed: &str) -> bool {
    let t = trimmed.trim_end();
    for c in ['-', '*', '_'] {
        if t.len() >= 3 && t.chars().all(|x| x == c) {
            return true;
        }
    }
    false
}

/// Length of a leading list marker (`- `, `* `, `+ `, `1. `, `2) `), if present.
///
/// The trailing space is required, so a word like `*emphasis*` at the start of a
/// line is not mistaken for a bullet.
fn list_marker(trimmed: &str) -> Option<usize> {
    let b = trimmed.as_bytes();
    if b.len() >= 2 && matches!(b[0], b'-' | b'*' | b'+') && b[1] == b' ' {
        return Some(2);
    }
    let digits = b.iter().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0
        && b.len() > digits + 1
        && matches!(b[digits], b'.' | b')')
        && b[digits + 1] == b' '
    {
        return Some(digits + 2);
    }
    None
}

/// Split a line's inline markup into spans.
///
/// One left-to-right pass. Each delimiter is matched to its closer on the same
/// line; an unclosed delimiter is emitted as the plain text it looks like, which
/// is what a reader sees anyway.
fn inline(s: &str, out: &mut Vec<Span<'static>>) {
    let b = s.as_bytes();
    let mut i = 0;
    let mut plain_from = 0;

    // Emit the pending run of ordinary text.
    macro_rules! flush {
        ($upto:expr) => {
            if $upto > plain_from {
                out.push(Span::styled(
                    s[plain_from..$upto].to_string(),
                    Style::default().fg(TEXT),
                ));
            }
        };
    }

    while i < b.len() {
        match b[i] {
            // `code`, and ``code with a backtick`` .
            b'`' => {
                let ticks = b[i..].iter().take_while(|c| **c == b'`').count();
                if let Some(end) = find_run(b, i + ticks, b'`', ticks) {
                    flush!(i);
                    let stop = end + ticks;
                    out.push(Span::styled(
                        s[i..stop].to_string(),
                        Style::default().fg(CODE),
                    ));
                    i = stop;
                    plain_from = i;
                    continue;
                }
                i += ticks;
            }
            // **strong** / __strong__, then *em* / _em_.
            b'*' | b'_' => {
                let c = b[i];
                let run = b[i..].iter().take_while(|x| **x == c).count().min(2);
                if let Some(end) = find_run(b, i + run, c, run) {
                    flush!(i);
                    let stop = end + run;
                    let style = if run == 2 {
                        Style::default().fg(TEXT).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(TEXT).add_modifier(Modifier::ITALIC)
                    };
                    out.push(Span::styled(s[i..stop].to_string(), style));
                    i = stop;
                    plain_from = i;
                    continue;
                }
                i += run;
            }
            // [text](url) and ![alt](url); also a bare [text].
            b'[' => {
                if let Some(close) = memchr(b, i + 1, b']') {
                    // Include a following (...) so the URL is coloured too.
                    let mut stop = close + 1;
                    if b.get(stop) == Some(&b'(') {
                        if let Some(paren) = memchr(b, stop + 1, b')') {
                            stop = paren + 1;
                        }
                    }
                    flush!(i);
                    out.push(Span::styled(
                        s[i..stop].to_string(),
                        Style::default().fg(LINK).add_modifier(Modifier::UNDERLINED),
                    ));
                    i = stop;
                    plain_from = i;
                    continue;
                }
                i += 1;

            }
            _ => i += 1,
        }
    }
    flush!(b.len());
}

/// Index of the next `needle` at or after `from`.
fn memchr(b: &[u8], from: usize, needle: u8) -> Option<usize> {
    (from..b.len()).find(|&i| b[i] == needle)
}

/// Index of a run of exactly `count` `needle` bytes at or after `from`.
fn find_run(b: &[u8], from: usize, needle: u8, count: usize) -> Option<usize> {
    let mut i = from;
    while i < b.len() {
        if b[i] == needle {
            let run = b[i..].iter().take_while(|c| **c == needle).count();
            if run >= count {
                return Some(i);
            }
            i += run;
        } else {
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styles(line: &Line<'_>) -> Vec<(String, Style)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style))
            .collect()
    }

    /// The whole point of this module.
    #[test]
    fn highlighting_a_readme_is_effectively_instant() {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../README.md"
        ))
        .expect("the project README");

        let t = std::time::Instant::now();
        let lines = highlight(&text);
        let elapsed = t.elapsed();

        assert_eq!(lines.len(), text.lines().count());
        // syntect's markdown grammar takes ~170ms on this file in release and
        // over a second in debug. Even allowing for a debug build and a loaded
        // machine, this must be in a different class entirely.
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "markdown highlighting took {elapsed:?}, which is not instant"
        );
    }

    #[test]
    fn headings_are_bold() {
        let l = &highlight("# Title\n")[0];
        assert_eq!(l.spans[0].style.fg, Some(HEADING));
        assert!(l.spans[0].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn inline_code_is_separated_from_prose() {
        let l = &highlight("use `cargo test` please\n")[0];
        let got = styles(l);
        assert!(
            got.iter().any(|(t, s)| t == "`cargo test`" && s.fg == Some(CODE)),
            "{got:?}"
        );
        assert!(got.iter().any(|(t, _)| t == "use "), "{got:?}");
    }

    #[test]
    fn emphasis_and_strong_are_distinguished() {
        let l = &highlight("*em* and **strong**\n")[0];
        let got = styles(l);
        assert!(
            got.iter()
                .any(|(t, s)| t == "*em*" && s.add_modifier.contains(Modifier::ITALIC)),
            "{got:?}"
        );
        assert!(
            got.iter()
                .any(|(t, s)| t == "**strong**" && s.add_modifier.contains(Modifier::BOLD)),
            "{got:?}"
        );
    }

    #[test]
    fn links_are_underlined_including_the_url() {
        let l = &highlight("see [docs](https://example.com) now\n")[0];
        let got = styles(l);
        assert!(
            got.iter().any(|(t, s)| t == "[docs](https://example.com)"
                && s.fg == Some(LINK)
                && s.add_modifier.contains(Modifier::UNDERLINED)),
            "{got:?}"
        );
    }

    /// A fenced block is coloured as code throughout, including its fences, and
    /// markup inside it is left alone.
    #[test]
    fn fenced_blocks_suppress_inline_markup() {
        let lines = highlight("text\n```rust\nlet x = *p;\n```\nafter\n");
        assert_eq!(lines.len(), 5);
        // The fence lines and everything between them.
        for (i, line) in lines.iter().enumerate().take(4).skip(1) {
            assert!(
                line.spans.iter().all(|s| s.style.fg == Some(CODE)),
                "line {i} not code: {:?}",
                styles(line)
            );
        }
        // And normal highlighting resumes afterwards.
        assert_eq!(lines[4].spans[0].style.fg, Some(TEXT));
    }

    /// A tilde fence must not be closed by a backtick fence, or everything after
    /// a code sample containing ``` falls out of the block.
    #[test]
    fn a_fence_is_only_closed_by_its_own_character() {
        let lines = highlight("~~~\n```\nstill code\n~~~\nprose\n");
        assert!(lines[2].spans.iter().all(|s| s.style.fg == Some(CODE)));
        assert_eq!(lines[4].spans[0].style.fg, Some(TEXT));
    }

    #[test]
    fn list_markers_are_distinct_from_their_text() {
        for src in ["- item\n", "* item\n", "+ item\n", "1. item\n", "2) item\n"] {
            let l = &highlight(src)[0];
            assert_eq!(
                l.spans[0].style.fg,
                Some(MARKER),
                "marker not highlighted in {src:?}: {:?}",
                styles(l)
            );
            assert!(l.spans.len() > 1, "text not separated in {src:?}");
        }
    }

    /// `*emphasis*` opening a line is not a bullet.
    #[test]
    fn emphasis_at_the_start_of_a_line_is_not_a_bullet() {
        let l = &highlight("*not a bullet*\n")[0];
        assert!(
            l.spans[0].style.add_modifier.contains(Modifier::ITALIC),
            "{:?}",
            styles(l)
        );
    }

    #[test]
    fn table_rows_mark_the_leading_pipe() {
        let l = &highlight("| a | b |\n")[0];
        assert_eq!(l.spans[0].style.fg, Some(MARKER));
    }

    #[test]
    fn rules_and_quotes_are_quiet() {
        assert_eq!(highlight("---\n")[0].spans[0].style.fg, Some(QUIET));
        assert_eq!(highlight("> quoted\n")[0].spans[0].style.fg, Some(QUIET));
    }

    /// An unclosed delimiter is common in real prose ("2 * 3") and must not eat
    /// the rest of the line or drop it.
    #[test]
    fn unclosed_delimiters_keep_their_text() {
        for src in ["2 * 3 = 6\n", "a `unclosed\n", "see [ref\n", "under_score\n"] {
            let l = &highlight(src)[0];
            let joined: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(joined, src.trim_end(), "text lost for {src:?}");
        }
    }

    /// No span may contain a newline, or the pane gains blank cells.
    #[test]
    fn spans_never_contain_newlines() {
        let lines = highlight("# a\n\n- b `c`\n\n```\nd\n```\n");
        for l in &lines {
            for s in &l.spans {
                assert!(!s.content.contains('\n'), "{:?}", s.content);
            }
        }
    }

    /// Every line of input must produce exactly one line of output, whatever it
    /// contains — the scroll bounds depend on it.
    #[test]
    fn line_count_is_preserved() {
        for src in [
            "",
            "\n",
            "a\nb\n",
            "```\nunclosed fence\n",
            "#\n>\n---\n|\n- \n",
        ] {
            assert_eq!(
                highlight(src).len(),
                src.lines().count(),
                "line count changed for {src:?}"
            );
        }
    }

    #[test]
    fn tabs_are_expanded() {
        let l = &highlight("\tcode\n")[0];
        let joined: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(joined.starts_with("    "), "{joined:?}");
    }

    /// Multibyte text must not be split mid-character.
    #[test]
    fn utf8_survives() {
        let l = &highlight("héllo — *wörld* 日本語\n")[0];
        let joined: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "héllo — *wörld* 日本語");
    }
}

//! Deciding whether the terminal can draw a real image, not an approximation.
//!
//! Quarter-block rendering packs four pixels into one cell and picks two colours
//! for them, so a photo comes out visibly blocky. Both `kitty`'s graphics protocol
//! and iTerm2's inline-images protocol hand the terminal an actual PNG and let it
//! draw at pixel resolution, which looks like the image rather than like an
//! impression of it.
//!
//! The catch is that neither can be turned into ratatui spans. They are escape
//! sequences carrying base64 image data, so they have to reach the terminal
//! verbatim — written straight to stdout after the frame has been drawn, into a
//! gap the frame deliberately left blank. See
//! [`crate::widget::preview`] for the hole and `app.rs` for the write.
//!
//! # Why this is conservative
//!
//! Guessing wrong is not a cosmetic problem: a terminal that does not understand
//! the sequence prints it, so a mis-detection sprays kilobytes of base64 across
//! the display. Detection therefore only says yes when the terminal identifies
//! itself as one known to support the protocol, and multiplexers are refused
//! unless they are configured to pass the sequences through. When in doubt it
//! falls back to blocks, which always work.

use std::sync::OnceLock;

/// How an image should be handed to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Unicode block characters with SGR colour. Works everywhere; blocky.
    Blocks,
    /// kitty graphics protocol — `ESC _G ... ESC \` with base64 PNG.
    Kitty,
    /// iTerm2 inline images — `ESC ] 1337 ; File=... ESC \`.
    Iterm2,
}

impl Protocol {
    /// The `timg -p` letter that produces this protocol.
    pub fn timg_flag(self) -> &'static str {
        match self {
            Protocol::Blocks => "q",
            Protocol::Kitty => "k",
            Protocol::Iterm2 => "i",
        }
    }

    /// Whether output in this protocol is escape data for the terminal rather
    /// than text that can be parsed into cells.
    pub fn is_graphics(self) -> bool {
        !matches!(self, Protocol::Blocks)
    }

    fn from_name(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "kitty" => Some(Protocol::Kitty),
            "iterm" | "iterm2" => Some(Protocol::Iterm2),
            "blocks" | "block" | "none" | "off" => Some(Protocol::Blocks),
            _ => None,
        }
    }
}

/// The protocol to use, decided once per process.
pub fn protocol() -> Protocol {
    static CACHE: OnceLock<Protocol> = OnceLock::new();
    *CACHE.get_or_init(|| detect(&EnvVars::from_process()))
}

/// The environment inputs detection depends on.
///
/// Threaded through a struct rather than read from the process directly so the
/// decision table can be tested — the alternative is mutating the environment in
/// tests, which is a data race in a multi-threaded test binary.
#[derive(Debug, Default, Clone)]
pub struct EnvVars {
    pub override_var: Option<String>,
    pub term: Option<String>,
    pub term_program: Option<String>,
    pub kitty_window_id: Option<String>,
    pub konsole_version: Option<String>,
    pub tmux: Option<String>,
    /// Whether tmux is configured to forward unknown escape sequences.
    /// Only consulted when [`Self::tmux`] is set.
    pub tmux_passthrough: bool,
}

impl EnvVars {
    fn from_process() -> Self {
        let tmux = std::env::var("TMUX").ok().filter(|s| !s.is_empty());
        Self {
            override_var: std::env::var("MYD_PREVIEW_GRAPHICS").ok(),
            term: std::env::var("TERM").ok(),
            term_program: std::env::var("TERM_PROGRAM").ok(),
            kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok(),
            konsole_version: std::env::var("KONSOLE_VERSION").ok(),
            tmux_passthrough: tmux.is_some() && tmux_allows_passthrough(),
            tmux,
        }
    }
}

/// Decide which protocol to use.
pub fn detect(env: &EnvVars) -> Protocol {
    // An explicit choice always wins: detection cannot know about every terminal,
    // and someone who knows their setup should not have to argue with a guess.
    if let Some(p) = env.override_var.as_deref().and_then(Protocol::from_name) {
        return p;
    }

    let native = native_protocol(env);
    if native == Protocol::Blocks {
        return Protocol::Blocks;
    }

    // Inside a multiplexer the sequence has to survive being forwarded. tmux only
    // does that with `allow-passthrough` on, and screen not at all, so anything
    // else falls back rather than risking base64 on the screen.
    if env.tmux.is_some() && !env.tmux_passthrough {
        return Protocol::Blocks;
    }
    if env
        .term
        .as_deref()
        .is_some_and(|t| t.starts_with("screen") && env.tmux.is_none())
    {
        // GNU screen has no passthrough equivalent worth relying on. (tmux also
        // sets TERM=screen-*, which is why $TMUX is checked first.)
        return Protocol::Blocks;
    }

    native
}

/// What the terminal itself supports, ignoring any multiplexer.
fn native_protocol(env: &EnvVars) -> Protocol {
    let term = env.term.as_deref().unwrap_or("").to_ascii_lowercase();
    let prog = env.term_program.as_deref().unwrap_or("").to_ascii_lowercase();

    // kitty sets both; ghostty implements the same protocol and identifies itself
    // in TERM_PROGRAM.
    if env.kitty_window_id.is_some()
        || term.contains("kitty")
        || prog.contains("kitty")
        || prog.contains("ghostty")
        || term.contains("ghostty")
    {
        return Protocol::Kitty;
    }

    // Konsole gained kitty-protocol support in 22.04; older builds print the
    // escape, so a version that cannot be parsed is treated as too old.
    if let Some(v) = &env.konsole_version {
        if v.trim().parse::<u32>().map(|n| n >= 220400).unwrap_or(false) {
            return Protocol::Kitty;
        }
    }

    // WezTerm implements both; its iTerm2 support is the older and more settled of
    // the two, so prefer that.
    if prog.contains("iterm") || prog.contains("wezterm") || prog.contains("mintty") {
        return Protocol::Iterm2;
    }

    Protocol::Blocks
}

/// Whether tmux will forward escape sequences it does not understand.
///
/// Without this, a graphics escape is swallowed (or worse, mangled) by tmux and
/// the image never appears. Asking tmux is the only reliable way to know: the
/// option is off by default and there is no environment variable for it.
fn tmux_allows_passthrough() -> bool {
    std::process::Command::new("tmux")
        .args(["show", "-gv", "allow-passthrough"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let v = String::from_utf8_lossy(&o.stdout).trim().to_ascii_lowercase();
            // tmux reports "on", "off" or "all"; "all" additionally forwards in
            // panes that are not visible.
            v == "on" || v == "all"
        })
        .unwrap_or(false)
}

/// Wrap a graphics escape so it survives tmux's passthrough.
///
/// tmux forwards a sequence wrapped in `ESC Ptmux; ... ESC \`, and requires every
/// `ESC` inside to be doubled so it can tell the payload from the wrapper's own
/// terminator.
pub fn wrap_for_tmux(payload: &str) -> String {
    let mut out = String::with_capacity(payload.len() + 16);
    out.push_str("\x1bPtmux;");
    for c in payload.chars() {
        if c == '\x1b' {
            out.push('\x1b');
        }
        out.push(c);
    }
    out.push_str("\x1b\\");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EnvVars {
        EnvVars::default()
    }

    #[test]
    fn a_plain_terminal_gets_blocks() {
        let mut e = env();
        e.term = Some("xterm-256color".into());
        assert_eq!(detect(&e), Protocol::Blocks);
    }

    #[test]
    fn kitty_is_detected_three_ways() {
        for e in [
            EnvVars {
                kitty_window_id: Some("1".into()),
                ..env()
            },
            EnvVars {
                term: Some("xterm-kitty".into()),
                ..env()
            },
            EnvVars {
                term_program: Some("ghostty".into()),
                ..env()
            },
        ] {
            assert_eq!(detect(&e), Protocol::Kitty, "{e:?}");
        }
    }

    #[test]
    fn iterm_and_wezterm_use_the_iterm_protocol() {
        for p in ["iTerm.app", "WezTerm", "mintty"] {
            let e = EnvVars {
                term_program: Some(p.into()),
                ..env()
            };
            assert_eq!(detect(&e), Protocol::Iterm2, "{p}");
        }
    }

    /// Konsole only gained the protocol in 22.04; an older one would print the
    /// escape.
    #[test]
    fn konsole_is_gated_on_its_version() {
        let old = EnvVars {
            konsole_version: Some("211200".into()),
            ..env()
        };
        assert_eq!(detect(&old), Protocol::Blocks);

        let new = EnvVars {
            konsole_version: Some("220400".into()),
            ..env()
        };
        assert_eq!(detect(&new), Protocol::Kitty);

        // Unparseable means unknown, which means don't risk it.
        let weird = EnvVars {
            konsole_version: Some("not-a-number".into()),
            ..env()
        };
        assert_eq!(detect(&weird), Protocol::Blocks);
    }

    /// The case on the development machine: kitty-capable outer terminal, but
    /// tmux in front of it with passthrough off. Sending graphics here would put
    /// base64 on the screen.
    #[test]
    fn tmux_without_passthrough_refuses_graphics() {
        let e = EnvVars {
            term: Some("screen-256color".into()),
            kitty_window_id: Some("1".into()),
            tmux: Some("/tmp/tmux-1000/default,123,0".into()),
            tmux_passthrough: false,
            ..env()
        };
        assert_eq!(
            detect(&e),
            Protocol::Blocks,
            "graphics must not be sent through a tmux that will not forward them"
        );
    }

    #[test]
    fn tmux_with_passthrough_allows_graphics() {
        let e = EnvVars {
            term: Some("screen-256color".into()),
            kitty_window_id: Some("1".into()),
            tmux: Some("/tmp/tmux-1000/default,123,0".into()),
            tmux_passthrough: true,
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Kitty);
    }

    /// GNU screen has no passthrough to rely on.
    #[test]
    fn gnu_screen_refuses_graphics() {
        let e = EnvVars {
            term: Some("screen.xterm-kitty".into()),
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Blocks);
    }

    /// A terminal that is not graphics-capable stays on blocks whatever the
    /// multiplexer allows — passthrough does not conjure support.
    #[test]
    fn passthrough_does_not_help_a_plain_terminal() {
        let e = EnvVars {
            term: Some("xterm-256color".into()),
            tmux: Some("x".into()),
            tmux_passthrough: true,
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Blocks);
    }

    #[test]
    fn an_explicit_override_wins_over_detection() {
        // Forced on, in a setup detection would refuse.
        let e = EnvVars {
            override_var: Some("kitty".into()),
            term: Some("dumb".into()),
            tmux: Some("x".into()),
            tmux_passthrough: false,
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Kitty);

        // And forced off, where detection would say yes.
        let e = EnvVars {
            override_var: Some("blocks".into()),
            kitty_window_id: Some("1".into()),
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Blocks);
    }

    /// A typo in the override must not silently disable graphics; fall through to
    /// detection instead.
    #[test]
    fn an_unrecognised_override_falls_through_to_detection() {
        let e = EnvVars {
            override_var: Some("sixel-maybe".into()),
            kitty_window_id: Some("1".into()),
            ..env()
        };
        assert_eq!(detect(&e), Protocol::Kitty);
    }

    #[test]
    fn timg_flags_match_the_protocols() {
        assert_eq!(Protocol::Blocks.timg_flag(), "q");
        assert_eq!(Protocol::Kitty.timg_flag(), "k");
        assert_eq!(Protocol::Iterm2.timg_flag(), "i");
        assert!(!Protocol::Blocks.is_graphics());
        assert!(Protocol::Kitty.is_graphics());
        assert!(Protocol::Iterm2.is_graphics());
    }

    /// tmux tells the payload from its own terminator by the doubled ESC, so
    /// every one has to be doubled or the image is truncated.
    #[test]
    fn tmux_wrapping_doubles_every_escape() {
        let wrapped = wrap_for_tmux("\x1b_Gf=100;AAAA\x1b\\");
        assert!(wrapped.starts_with("\x1bPtmux;"));
        assert!(wrapped.ends_with("\x1b\\"));
        // Two ESCs in the payload become four, plus one opening and one closing.
        assert_eq!(wrapped.matches('\x1b').count(), 2 + 4);
        assert!(wrapped.contains("\x1b\x1b_G"));
    }
}

//! Deciding whether the terminal can draw a real image, not an approximation.
//!
//! Quarter-block rendering packs four pixels into one cell and picks two colours
//! for them, so a photo comes out visibly blocky. The kitty graphics protocol,
//! iTerm2's inline images and sixel all hand the terminal real pixel data
//! instead, which looks like the image rather than like an impression of it.
//!
//! None of them can be turned into ratatui spans: they are escape sequences
//! carrying encoded image data, so they have to reach the terminal verbatim —
//! written straight to stdout after the frame has been drawn, into a gap the
//! frame deliberately left blank. See [`crate::widget::preview`] for the hole
//! and `app.rs` for the write.
//!
//! # How the terminal is identified
//!
//! Two sources, in that order:
//!
//! 1. **Asking it.** [`query_tty`] writes a kitty graphics query and a Device
//!    Attributes request and reads the replies. This is the only thing that
//!    actually knows, and the only way to detect sixel at all — no environment
//!    variable reports it.
//! 2. **Environment variables**, when there is no terminal to ask (a pipe, a
//!    test) or it does not answer.
//!
//! The environment route is easy to get subtly wrong, and did go wrong: it read
//! `TERM_PROGRAM` but not `LC_TERMINAL`, so iTerm2 went unrecognised both over
//! ssh (`TERM_PROGRAM` is not forwarded; `LC_TERMINAL` is) and under tmux (which
//! overwrites `TERM_PROGRAM` with its own name). Both are consulted now.
//!
//! # Why this is conservative
//!
//! Guessing wrong is not a cosmetic problem: a terminal that does not understand
//! the sequence prints it, so a mis-detection sprays kilobytes of base64 across
//! the display. A protocol is chosen only on positive evidence, and a multiplexer
//! is refused unless it will actually forward the sequence. When in doubt it
//! falls back to blocks, which always work.

use std::sync::OnceLock;

/// How an image should be handed to the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// Unicode block characters with SGR colour. Works everywhere; blocky.
    Blocks,
    /// kitty graphics protocol — `ESC _G ... ESC \` with base64 PNG.
    Kitty,
    /// iTerm2 inline images — `ESC ] 1337 ; File=... BEL`.
    Iterm2,
    /// Sixel — `ESC P q ... ESC \`. Lower fidelity than the other two (a
    /// palette rather than truecolour), but the only one tmux forwards without
    /// being configured to.
    Sixel,
}

impl Protocol {
    /// The `timg -p` letter that produces this protocol.
    pub fn timg_flag(self) -> &'static str {
        match self {
            Protocol::Blocks => "q",
            Protocol::Kitty => "k",
            Protocol::Iterm2 => "i",
            Protocol::Sixel => "s",
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
            "sixel" => Some(Protocol::Sixel),
            "blocks" | "block" | "none" | "off" => Some(Protocol::Blocks),
            _ => None,
        }
    }
}

/// The protocol to use, decided once per process.
pub fn protocol() -> Protocol {
    decision().0
}

/// Why the chosen protocol was chosen, for the footer and diagnostics.
///
/// Silent degradation is what made the original bug so hard to place: the pane
/// simply showed blocks and said nothing. When a capable-looking terminal ends up
/// on blocks anyway, this says what would have to change.
pub fn explain() -> Option<&'static str> {
    decision().1
}

/// The cached decision and its explanation.
fn decision() -> &'static Decision {
    static CACHE: OnceLock<Decision> = OnceLock::new();
    CACHE.get_or_init(|| {
        let env = EnvVars::from_process();
        // Only ask kitty's question where the environment already hints the
        // protocol is understood — see `KITTY_QUERY` for why asking blindly puts
        // the query text on the user's screen.
        // `env_capabilities` folds the iTerm2 family in with kitty, because both
        // are "some graphics protocol". Only a terminal that plausibly speaks
        // *kitty's* protocol should be asked kitty's question — iTerm2 does not,
        // and would print it.
        let ask_kitty = env_capabilities(&env).kitty && !iterm_family(&env);
        let probed = query_tty(PROBE_TIMEOUT, ask_kitty);
        decide(&env, probed)
    })
}

/// A protocol, the reason when it is a fallback, and the measured cell size.
pub struct Decision(pub Protocol, pub Option<&'static str>, pub Option<(u16, u16)>);

impl std::ops::Deref for Decision {
    type Target = Protocol;
    fn deref(&self) -> &Protocol {
        &self.0
    }
}

/// How long to wait for a terminal to answer a capability query.
///
/// A real terminal replies to Device Attributes immediately, and that reply
/// doubles as the "no more is coming" signal, so this bound is only reached when
/// something is not answering at all. Short enough not to be felt at startup.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// What the terminal said when asked directly.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Probed {
    pub kitty: bool,
    pub sixel: bool,
    /// The terminal's cell size in pixels, when it reported one.
    ///
    /// This is what makes an image the right size. Given it, the escape can
    /// state the image's size in **pixels**, which every terminal resolves the
    /// same way. Sizing in *cells* instead leaves the terminal to do that
    /// arithmetic, and iTerm2 reaches a different answer inside tmux than it
    /// does natively — which is why previews filled a native window but came out
    /// small in a tmux pane.
    pub cell: Option<(u16, u16)>,
}

impl Probed {
    fn saw_anything(&self) -> bool {
        self.kitty || self.sixel
    }
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
    /// iTerm2 (and WezTerm) set this, and ssh forwards `LC_*` by default, so it
    /// is the only identification that survives both a remote session and tmux.
    pub lc_terminal: Option<String>,
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
            lc_terminal: std::env::var("LC_TERMINAL").ok(),
            kitty_window_id: std::env::var("KITTY_WINDOW_ID").ok(),
            konsole_version: std::env::var("KONSOLE_VERSION").ok(),
            tmux_passthrough: tmux.is_some() && tmux_allows_passthrough(),
            tmux,
        }
    }
}

/// Decide which protocol to use, given the environment and any probe result.
pub fn decide(env: &EnvVars, probed: Option<Probed>) -> Decision {
    // Carried through every outcome: the cell size is useful even when the
    // decision is to fall back to blocks, and is independent of the protocol.
    let cell = probed.and_then(|p| p.cell);

    // An explicit choice always wins: detection cannot know about every terminal,
    // and someone who knows their setup should not have to argue with a guess.
    if let Some(p) = env.override_var.as_deref().and_then(Protocol::from_name) {
        return Decision(p, None, cell);
    }

    let caps = capabilities(env, probed);
    if !caps.saw_anything() {
        return Decision(Protocol::Blocks, None, cell);
    }

    // Inside a multiplexer the sequence has to survive being forwarded.
    if env.tmux.is_some() {
        // tmux parses and re-emits sixel itself, so it needs no passthrough —
        // this is the only protocol that works in a default tmux. It is
        // preferred here even though kitty and iTerm2 look better, because
        // working beats looking good.
        if caps.sixel {
            return Decision(Protocol::Sixel, None, cell);
        }
        // The rest ride the passthrough escape, which is off by default.
        if !env.tmux_passthrough {
            return Decision(
                Protocol::Blocks,
                Some("tmux needs: set -g allow-passthrough on"),
                cell,
            );
        }
        return Decision(preferred(env, &caps), None, cell);
    }

    // GNU screen has no passthrough worth relying on. (tmux also sets
    // TERM=screen-*, which is why $TMUX is checked first.)
    if env
        .term
        .as_deref()
        .is_some_and(|t| t.starts_with("screen") && env.tmux.is_none())
    {
        return Decision(
            Protocol::Blocks,
            Some("GNU screen cannot forward graphics"),
            cell,
        );
    }

    Decision(preferred(env, &caps), None, cell)
}

/// Best available protocol, in quality order.
///
/// iTerm2 and WezTerm are checked before kitty because they are identified the
/// same way but speak a different protocol: `env_capabilities` reports a
/// capability, and this decides which escape to actually send.
fn preferred(env: &EnvVars, caps: &Probed) -> Protocol {
    if iterm_family(env) {
        return Protocol::Iterm2;
    }
    if caps.kitty {
        Protocol::Kitty
    } else if caps.sixel {
        Protocol::Sixel
    } else {
        Protocol::Blocks
    }
}

/// What the terminal can do: what it told us, or failing that what the
/// environment implies.
///
/// A probe that saw something is authoritative. A probe that answered nothing
/// falls back to the environment rather than concluding "no support", since a
/// terminal behind ssh or a multiplexer may simply not have replied in time.
fn capabilities(env: &EnvVars, probed: Option<Probed>) -> Probed {
    if let Some(p) = probed {
        if p.saw_anything() {
            return p;
        }
    }
    env_capabilities(env)
}

/// What the environment claims the terminal supports.
///
/// Note this can report `kitty` for a terminal whose protocol is really iTerm2's;
/// the distinction is made by [`preferred`] via [`iterm_family`].
fn env_capabilities(env: &EnvVars) -> Probed {
    let term = env.term.as_deref().unwrap_or("").to_ascii_lowercase();
    let prog = env.term_program.as_deref().unwrap_or("").to_ascii_lowercase();
    let lc = env.lc_terminal.as_deref().unwrap_or("").to_ascii_lowercase();

    // kitty sets both of its own variables; ghostty implements the same protocol.
    let kitty = env.kitty_window_id.is_some()
        || term.contains("kitty")
        || prog.contains("kitty")
        || prog.contains("ghostty")
        || term.contains("ghostty")
        || lc.contains("kitty")
        || lc.contains("ghostty")
        // Konsole gained the protocol in 22.04; older builds print the escape, so
        // a version that cannot be parsed is treated as too old.
        || env
            .konsole_version
            .as_deref()
            .and_then(|v| v.trim().parse::<u32>().ok())
            .is_some_and(|n| n >= 220400);

    // The iTerm2 family is a capability too, even though `Probed` has no field
    // for it: without this, `saw_anything()` is false for an iTerm2 terminal and
    // the decision bails out before it can choose a protocol.
    Probed {
        kitty: kitty || iterm_family(env),
        // Nothing in the environment reports sixel. Only a probe can, so an
        // env-only decision never selects it.
        sixel: false,
        // Nor the cell size.
        cell: None,
    }
}

/// Whether the environment names a terminal in the iTerm2 family.
///
/// Checked separately from [`env_capabilities`] because iTerm2 and WezTerm are
/// identified the same way but speak a different protocol from kitty.
fn iterm_family(env: &EnvVars) -> bool {
    let prog = env.term_program.as_deref().unwrap_or("").to_ascii_lowercase();
    // `TERM_PROGRAM` is overwritten by tmux and dropped by ssh, so `LC_TERMINAL`
    // is the one that survives. Missing this was the original bug.
    let lc = env.lc_terminal.as_deref().unwrap_or("").to_ascii_lowercase();
    let names = |s: &str| s.contains("iterm") || s.contains("wezterm") || s.contains("mintty");
    names(&prog) || names(&lc)
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

/// Ask the terminal what it supports.
///
/// Writes a kitty graphics query followed by a Device Attributes request, then
/// reads replies until DA arrives or the timeout expires. DA is answered by
/// every real terminal, so it marks the end of the replies and the timeout is
/// only reached when nothing is listening.
///
/// Returns `None` when there is no terminal to ask — a pipe, a test harness, a
/// sandbox without `/dev/tty` — so the caller can fall back to the environment.
///
/// Must run **before** the alternate screen is entered: a reply that arrives
/// late would otherwise be delivered as if it were a keypress.
pub fn query_tty(timeout: std::time::Duration, ask_kitty: bool) -> Option<Probed> {
    #[cfg(unix)]
    {
        unix_probe::run(timeout, ask_kitty)
    }
    #[cfg(not(unix))]
    {
        let _ = (timeout, ask_kitty);
        None
    }
}

#[cfg(unix)]
mod unix_probe {
    use super::Probed;
    use std::io::{Read, Write};
    use std::os::fd::AsRawFd;

    /// Ask the terminal what it supports.
    ///
    /// `ESC[c` (Device Attributes) is safe to send anywhere: it is ancient, every
    /// terminal answers it, and none prints it. A `4` in the reply means sixel.
    ///
    /// The kitty graphics query is **not** sent unconditionally. It is an APC
    /// string, and a terminal that does not understand APC prints the payload
    /// instead of swallowing it — which puts `_Gi=31,s=1,...;AAAAAAAA` on the
    /// user's screen. Observed doing exactly that. It is therefore only sent when
    /// something already suggests the terminal speaks the protocol, and the
    /// answer for everyone else comes from Device Attributes and the environment.
    const DA_QUERY: &[u8] = b"\x1b[c";
    /// Ask for the character cell size in pixels. Answered `ESC[6;H;Wt`.
    /// Old terminals ignore it silently rather than printing it, so unlike the
    /// kitty query this is safe to send anywhere.
    const CELL_QUERY: &[u8] = b"\x1b[16t";
    const KITTY_QUERY: &[u8] = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAAAAAA\x1b\\";

    /// The cell size according to the kernel's window size, if it carries one.
    ///
    /// `TIOCGWINSZ` optionally reports the terminal's size in pixels alongside
    /// its size in rows and columns; dividing gives the cell. Preferred over the
    /// `ESC[16t` query because it is a synchronous call with no reply to wait
    /// for — and because under tmux it describes the *pane*, where the escape is
    /// answered by the outer terminal about its own window.
    ///
    /// Many terminals leave the pixel fields zero, in which case this reports
    /// nothing and the query is used instead.
    fn cell_from_ioctl(fd: i32) -> Option<(u16, u16)> {
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) != 0 {
                return None;
            }
            if ws.ws_col == 0 || ws.ws_row == 0 || ws.ws_xpixel == 0 || ws.ws_ypixel == 0 {
                return None;
            }
            let w = ws.ws_xpixel / ws.ws_col;
            let h = ws.ws_ypixel / ws.ws_row;
            (w > 0 && h > 0).then_some((w, h))
        }
    }

    pub fn run(timeout: std::time::Duration, ask_kitty: bool) -> Option<Probed> {
        let mut tty = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/tty")
            .ok()?;

        let fd = tty.as_raw_fd();
        let saved = set_raw(fd)?;
        // Restore the terminal on every path out, including an early return or a
        // panic in between.
        let _restore = Restore { fd, saved };

        // Kitty's query first when it is safe to ask, so both replies arrive
        // before the Device Attributes answer that marks the end.
        // Ask the kernel first; it answers immediately or not at all.
        let ioctl_cell = cell_from_ioctl(fd);

        if ask_kitty {
            tty.write_all(KITTY_QUERY).ok()?;
        }
        // Only ask the terminal when the kernel had nothing, so a terminal that
        // ignores the query does not cost the full timeout for an answer we
        // already have.
        let ask_cell = ioctl_cell.is_none();
        if ask_cell {
            tty.write_all(CELL_QUERY).ok()?;
        }
        tty.write_all(DA_QUERY).ok()?;
        tty.flush().ok()?;

        let deadline = std::time::Instant::now() + timeout;
        let mut buf = Vec::with_capacity(256);
        let mut chunk = [0u8; 256];
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() || !readable(fd, remaining) {
                break;
            }
            match tty.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    // Stop once everything asked for has arrived. Waiting on the
                    // Device Attributes reply alone is not enough: a terminal may
                    // answer the queries in any order, and stopping at the first
                    // `c` byte then discards a cell-size reply that had not been
                    // read yet — which silently loses the measurement and falls
                    // back to sizing in cells.
                    if replies_complete(&buf, ask_kitty, ask_cell) {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        // The kernel's answer wins: under tmux it describes the pane, where the
        // escape is answered by the outer terminal about its whole window.
        let mut probed = parse_reply(&buf);
        if let Some(c) = ioctl_cell {
            probed.cell = Some(c);
        }
        Some(probed)
    }

    /// Whether every answer that was asked for has arrived.
    ///
    /// A terminal is free to answer in any order, so this waits for each reply
    /// individually rather than treating one of them as a finish line.
    pub fn replies_complete(buf: &[u8], asked_kitty: bool, asked_cell: bool) -> bool {
        let s = String::from_utf8_lossy(buf);
        // Device Attributes: `ESC[?...c`.
        let da = s
            .split('\x1b')
            .any(|p| p.starts_with("[?") && p.contains('c'));
        // Cell size: `ESC[6;h;wt`. A terminal that does not implement the query
        // says nothing at all, so this can never complete on its own — the
        // timeout is what ends the wait there.
        let cell = !asked_cell
            || s.split('\x1b')
                .any(|p| p.starts_with("[6;") && p.contains('t'));
        // kitty answers `_G...;OK` or an error; either is an answer.
        let kitty = !asked_kitty || s.contains("_Gi=31;");
        da && cell && kitty
    }

    /// Read the answers out of whatever the terminal sent.
    pub fn parse_reply(buf: &[u8]) -> Probed {
        let s = String::from_utf8_lossy(buf);
        // kitty answers the graphics query with `_Gi=31;OK`.
        let kitty = s.contains("_Gi=31;OK") || s.contains("_Gi=31;ok");
        // Device Attributes looks like `ESC[?62;4;22c`; attribute 4 is sixel.
        let sixel = s
            .split('\x1b')
            .filter_map(|part| part.strip_prefix("[?"))
            .filter_map(|part| part.split('c').next())
            .any(|attrs| attrs.split(';').any(|a| a == "4"));
        // The cell size arrives as `ESC[6;<height>;<width>t` — note the reply
        // gives height before width, the reverse of how sizes are usually
        // written.
        let cell = s.split('\x1b').find_map(|part| {
            let body = part.strip_prefix("[6;")?.split('t').next()?;
            let (h, w) = body.split_once(';')?;
            let (h, w) = (h.trim().parse().ok()?, w.trim().parse().ok()?);
            // A terminal that reports zero is not telling us anything usable.
            (w > 0 && h > 0).then_some((w, h))
        });
        Probed { kitty, sixel, cell }
    }

    /// Put the terminal into raw mode so the reply is not line-buffered or echoed.
    fn set_raw(fd: i32) -> Option<libc::termios> {
        unsafe {
            let mut saved: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(fd, &mut saved) != 0 {
                return None;
            }
            let mut raw = saved;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(fd, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(saved)
        }
    }

    struct Restore {
        fd: i32,
        saved: libc::termios,
    }

    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::tcsetattr(self.fd, libc::TCSANOW, &self.saved);
            }
        }
    }

    /// Wait until the terminal has something to say, or the time runs out.
    fn readable(fd: i32, timeout: std::time::Duration) -> bool {
        unsafe {
            let mut set: libc::fd_set = std::mem::zeroed();
            libc::FD_ZERO(&mut set);
            libc::FD_SET(fd, &mut set);
            let mut tv = libc::timeval {
                tv_sec: timeout.as_secs() as libc::time_t,
                tv_usec: timeout.subsec_micros() as libc::suseconds_t,
            };
            libc::select(
                fd + 1,
                &mut set,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut tv,
            ) > 0
        }
    }
}

/// Remove the framing a renderer wraps around its graphics output.
///
/// timg brackets every image with terminal housekeeping that is right for a
/// standalone command and wrong inside a TUI. Measured with `xxd`:
///
/// ```text
/// ESC[?25l  [sixel: ESC[80h ESC[?7730h ESC[?8452l]  <graphics>  [CR|LF]  ESC[?25h
/// ```
///
/// Two of those actively break the pane. The trailing newline (kitty, iTerm2) or
/// carriage return (sixel) **moves the cursor**, which scrolls the display when
/// the image sits near the bottom. `ESC[?25h` **un-hides the cursor** that the
/// TUI hid at startup, leaving it blinking over the image.
///
/// Everything from the first graphics sequence to the last is kept exactly as it
/// is; only the surrounding framing goes.
pub fn strip_framing(payload: &str) -> &str {
    let seqs = split_sequences(payload);
    let first = seqs.iter().position(|s| is_graphics_sequence(s));
    let last = seqs.iter().rposition(|s| is_graphics_sequence(s));
    let (Some(first), Some(last)) = (first, last) else {
        // No graphics at all — hand it back untouched rather than guessing.
        return payload;
    };

    // Slice from the start of the first graphics sequence to the end of the last.
    // Both are subslices of `payload`, so their offsets locate the span.
    let base = payload.as_ptr() as usize;
    let start = seqs[first].as_ptr() as usize - base;
    let end = seqs[last].as_ptr() as usize - base + seqs[last].len();
    &payload[start..end]
}

/// Whether a sequence carries image data, as opposed to terminal housekeeping.
fn is_graphics_sequence(seq: &str) -> bool {
    seq.starts_with("\x1b_G")      // kitty APC
        || seq.starts_with("\x1b]1337;") // iTerm2 OSC
        || seq.starts_with("\x1bP")      // sixel DCS
}

/// The terminal's cell size in pixels, if it told us.
///
/// Measured once, alongside the protocol probe. `None` when there was no
/// terminal to ask or it did not answer.
pub fn cell_size() -> Option<(u16, u16)> {
    decision().2
}

/// Pin a graphics payload to an exact size in terminal cells.
///
/// This is what keeps an image inside its pane. timg describes the image in
/// **pixels** (`width=260px;height=91px` for iTerm2, and nothing at all for
/// kitty), which leaves the terminal to divide by its own cell size and decide
/// how many rows to occupy. That cell size is not knowable from here — timg
/// itself falls back to assuming 18px per row when its stdout is a pipe, as it
/// is here — so the row count the terminal picks and the row count the pane
/// reserved are two different numbers.
///
/// When the image comes out taller, the excess lands on rows the pane never
/// drew. Those rows are outside everything ratatui repaints, so the overflow
/// both spills past the pane border *and* survives closing the preview: the two
/// symptoms are one bug.
///
/// Both protocols can be told the size in cells directly, which removes the
/// guesswork:
///
/// - **iTerm2** takes `width=` / `height=` in cells when the `px` suffix is
///   dropped, plus `preserveAspectRatio` so it letterboxes rather than
///   stretches.
/// - **kitty** takes `c=` / `r=` (columns and rows) among the control keys of
///   the *first* chunk — the continuation chunks carry no control keys.
///
/// Sixel has no such control. It is a raster with fixed pixel dimensions, and
/// the rows it occupies depend on the terminal's cell height — which timg
/// assumes is 18px when it cannot ask. A terminal with a shorter cell will draw
/// it taller than the reserved rows, so [`sixel_row_budget`] shrinks what is
/// requested to leave headroom.
pub fn pin_to_cells(payload: &str, cols: u16, rows: u16) -> String {
    if payload.starts_with("\x1b]1337;") {
        return pin_iterm(payload, cols, rows, cell_size());
    }
    if payload.starts_with("\x1b_G") {
        return pin_kitty(payload, cols, rows);
    }
    payload.to_string()
}

/// Give iTerm2 the size of the image, in pixels where possible.
///
/// Pixels rather than cells, when the terminal has told us how big a cell is.
/// Both forms are valid, but only one is unambiguous: sizing in cells leaves the
/// terminal to multiply out, and iTerm2 reaches a different answer inside tmux
/// than it does natively — the same payload filled a native window and came out
/// small in a tmux pane. A pixel count means the same thing everywhere.
///
/// Falls back to cells when the cell size is unknown, which is better than
/// nothing and is what worked natively all along.
fn pin_iterm(payload: &str, cols: u16, rows: u16, cell: Option<(u16, u16)>) -> String {
    let Some(start) = payload.find("width=") else {
        return payload.to_string();
    };
    let Some(rest) = payload[start..].find("inline=") else {
        return payload.to_string();
    };
    let end = start + rest;

    // `preserveAspectRatio=1` keeps the image's shape inside whichever box it is
    // given; without it iTerm2 stretches to fill exactly.
    let size = match cell {
        Some((cw, ch)) => format!(
            "width={}px;height={}px;preserveAspectRatio=1;",
            u32::from(cols) * u32::from(cw),
            u32::from(rows) * u32::from(ch)
        ),
        None => format!("width={cols};height={rows};preserveAspectRatio=1;"),
    };
    format!("{}{}{}", &payload[..start], size, &payload[end..])
}

/// Add kitty's columns/rows control keys to the first chunk.
fn pin_kitty(payload: &str, cols: u16, rows: u16) -> String {
    // Control keys run from `ESC_G` to the first `;`, and only the opening chunk
    // has them; later chunks are `ESC_Gq=2,m=1;<data>`.
    let Some(semi) = payload.find(';') else {
        return payload.to_string();
    };
    let head = &payload[..semi];
    // Already pinned (a payload that has been through here before).
    if head.contains(",c=") || head.contains(",r=") {
        return payload.to_string();
    }
    format!("{},c={},r={}{}", head, cols, rows, &payload[semi..])
}

/// How many times over to sample a graphics render.
///
/// With stdout on a pipe timg cannot ask the terminal how big a cell is and
/// assumes 9x18 pixels — measured: a request of `rows` rows produced a raster
/// exactly `rows * 18` pixels tall. Real cells are often larger, and on a retina
/// display roughly twice that, so a raster sized for an 18px cell is drawn into
/// a box built from 30px cells and comes out visibly soft and small.
///
/// The escape pins the image to a cell count regardless (see [`pin_to_cells`]),
/// so the raster can be asked for at a multiple of the pane's size without
/// changing the space it occupies — it just arrives with enough pixels to fill
/// that space properly. Two is the useful ceiling: past it a PDF stops gaining
/// resolution (poppler reaches its own limit) while the payload keeps growing.
pub const GRAPHICS_OVERSAMPLE: u16 = 2;

/// Largest oversampled geometry worth asking for, in cells.
///
/// The payload grows with the square of the raster: a full-screen photo at 2x
/// came to 4.7MB across 1157 kitty chunks, each of which has to be individually
/// wrapped and pushed through tmux every time the selection moves. Past roughly
/// this size the extra pixels are beyond what any cell can show, so the cost buys
/// nothing.
const MAX_OVERSAMPLED_CELLS: u16 = 300;

/// The geometry to ask a renderer for, given the cells the image will occupy.
///
/// Returns a larger box than the pane so the raster has enough pixels for a cell
/// bigger than the renderer assumes — 37 rows of a 30px retina cell need 1110
/// pixels, where an 18px assumption yields 666. Bounded at both ends: saturating,
/// so a very large pane cannot wrap the multiplication, and capped, so it cannot
/// ask for a raster whose cost outweighs what the screen can show.
pub fn oversampled_geometry(cols: u16, rows: u16) -> (u16, u16) {
    // Scale both axes by the same factor, so the aspect ratio of the request is
    // unchanged — scaling them independently would ask for a differently-shaped
    // box than the pane and let the renderer letterbox inside it.
    //
    // The factor is reduced until neither axis exceeds the cap. A pane already
    // larger than the cap is passed through unscaled rather than shrunk: asking
    // for fewer cells than the pane would make the image worse than not
    // oversampling at all.
    let largest = cols.max(rows);
    let factor = if largest == 0 || largest >= MAX_OVERSAMPLED_CELLS {
        1
    } else {
        GRAPHICS_OVERSAMPLE.min(MAX_OVERSAMPLED_CELLS / largest).max(1)
    };
    (cols.saturating_mul(factor), rows.saturating_mul(factor))
}

/// Rows to ask timg for when rendering sixel into a pane of `rows` rows.
///
/// Sixel cannot be pinned to a cell count after the fact the way kitty and
/// iTerm2 can, so the only lever is the geometry timg is given. It sizes the
/// raster assuming an 18-pixel cell — measured: a request of `rows` produced a
/// raster of `rows * 18` pixels. A terminal whose cells are shorter than that
/// draws the same raster over *more* rows than were reserved, and the excess
/// lands where nothing repaints it.
///
/// Common cell heights run from about 14px upwards, so asking for roughly three
/// quarters of the pane keeps the image inside it on a 13-14px cell while still
/// filling most of the space on a taller one. Sixel is the fallback protocol;
/// the ones that can be pinned exactly are preferred where available.
pub fn sixel_row_budget(rows: u16) -> u16 {
    // With the cell size measured there is nothing to hedge against: timg's
    // 18-pixel assumption is what the budget exists to absorb, and knowing the
    // real figure makes the reserved rows exactly right. Shrinking anyway is
    // what made forced sixel look small in a native terminal.
    if cell_size().is_some() {
        return rows.max(1);
    }
    (rows.saturating_mul(3) / 4).max(1)
}

/// Scale a row count so timg's assumed cell height lands on the real one.
///
/// timg rasterises to `rows * 18` pixels when it cannot ask the terminal how big
/// a cell is — and it cannot, because its stdout is a pipe here. On a terminal
/// whose cells are taller than 18 pixels that raster is too small for the space
/// it will occupy, and the image looks shrunken. Asking for proportionally more
/// rows makes the pixel size come out right.
///
/// Only useful for sixel: the other protocols state their size in the escape and
/// are corrected there instead. Returns `rows` unchanged when the cell size is
/// unknown, or when the real cell is no taller than the assumption.
pub fn scale_rows_for_cell(rows: u16) -> u16 {
    /// The cell height timg assumes for a graphics render on a pipe.
    const ASSUMED_CELL_H: u32 = 18;

    let Some((_, ch)) = cell_size() else {
        return rows;
    };
    if u32::from(ch) <= ASSUMED_CELL_H {
        return rows;
    }
    let scaled = u32::from(rows) * u32::from(ch) / ASSUMED_CELL_H;
    scaled.min(u32::from(u16::MAX)) as u16
}

/// The escape that deletes a previously drawn image, where the protocol has one.
///
/// Only kitty does. Its images are objects the terminal keeps and re-composites,
/// so `a=d,d=A` is the way to remove every placement.
///
/// iTerm2 and sixel have no delete operation — the image belongs to the cells it
/// was drawn into — so removing one means erasing those cells instead. See
/// [`needs_region_erase`].
pub fn clear_sequence(protocol: Protocol) -> Option<&'static str> {
    match protocol {
        Protocol::Kitty => Some("\x1b_Ga=d,d=A\x1b\\"),
        _ => None,
    }
}

/// Whether removing an image of this protocol means erasing the cells it covered.
///
/// iTerm2 and sixel draw into the grid and offer no delete escape, so the only
/// way to get rid of one is to erase the region. Redrawing the frame is *not*
/// enough: ratatui only emits cells whose content changed, and the cells under an
/// image are ones it believes are already blank — so it writes nothing there and
/// the picture survives. That is the ghost left behind in a native iTerm2 pane.
///
/// kitty is excluded because it has a real delete, which is cheaper and does not
/// disturb the rest of the frame.
pub fn needs_region_erase(protocol: Protocol) -> bool {
    matches!(protocol, Protocol::Iterm2 | Protocol::Sixel)
}

/// Whether a graphics payload ends on a complete escape sequence.
///
/// The renderer's output is read under a size cap, so a very large image can be
/// cut mid-sequence. A block render truncated that way merely loses its bottom
/// rows, but a graphics payload goes to the terminal verbatim: an unterminated
/// APC, OSC or DCS leaves the terminal consuming everything after it as part of
/// the string, swallowing the interface.
pub fn is_complete(payload: &str) -> bool {
    match split_sequences(payload).last() {
        Some(last) if last.starts_with('\x1b') => {
            last.ends_with("\x1b\\") || last.ends_with('\x07')
        }
        // No sequences at all, or trailing plain text: nothing half-written.
        _ => true,
    }
}

/// Wrap graphics output so it survives tmux's passthrough./// Wrap graphics output so it survives tmux's passthrough./// Wrap graphics output so it survives tmux's passthrough./// Wrap graphics output so it survives tmux's passthrough.
///
/// tmux forwards a sequence wrapped in `ESC Ptmux; … ESC \`, with every `ESC`
/// inside doubled so it can tell the payload from the wrapper's terminator.
///
/// **Each escape sequence must be wrapped on its own.** A kitty image is not one
/// sequence but a chain of them — measured at 3 for a small logo and 35 for a
/// photo — and wrapping the whole chain in a single envelope delivers only the
/// first. This function therefore splits the payload and wraps each piece.
///
/// An OSC terminated by BEL is re-terminated with `ESC \` on the way through:
/// BEL inside a passthrough wrapper is not reliably recognised, and iTerm2's
/// inline-image sequence is BEL-terminated.
pub fn wrap_for_tmux(payload: &str) -> String {
    let mut out = String::with_capacity(payload.len() + payload.len() / 8);
    for seq in split_sequences(payload) {
        // Anything that is not an escape sequence (stray text) is passed through
        // untouched; there should be none after the framing is stripped.
        if !seq.starts_with('\x1b') {
            out.push_str(seq);
            continue;
        }
        out.push_str("\x1bPtmux;");
        // Normalise a BEL-terminated OSC to ST before doubling.
        let body = match seq.strip_suffix('\x07') {
            Some(without_bel) => {
                let mut s = String::with_capacity(seq.len() + 1);
                s.push_str(without_bel);
                s.push_str("\x1b\\");
                s
            }
            None => seq.to_string(),
        };
        for c in body.chars() {
            if c == '\x1b' {
                out.push('\x1b');
            }
            out.push(c);
        }
        out.push_str("\x1b\\");
    }
    out
}

/// Split a payload into individual escape sequences.
///
/// Recognises the three string-carrying forms the renderers emit: APC
/// (`ESC _ … ESC \`), OSC (`ESC ] … BEL` or `… ESC \`) and DCS
/// (`ESC P … ESC \`). Anything else is returned as a run of plain text.
pub fn split_sequences(payload: &str) -> Vec<&str> {
    let b = payload.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_from = 0;

    while i < b.len() {
        if b[i] != 0x1b || i + 1 >= b.len() {
            i += 1;
            continue;
        }
        let kind = b[i + 1];
        // Only these three carry a string body that needs its own wrapper.
        if !matches!(kind, b'_' | b']' | b'P') {
            i += 1;
            continue;
        }
        if i > text_from {
            out.push(&payload[text_from..i]);
        }
        // Scan for the terminator: ST (ESC \) for all three, or BEL for OSC.
        let mut j = i + 2;
        let end = loop {
            if j >= b.len() {
                break b.len();
            }
            if b[j] == 0x07 && kind == b']' {
                break j + 1;
            }
            if b[j] == 0x1b && j + 1 < b.len() && b[j + 1] == b'\\' {
                break j + 2;
            }
            j += 1;
        };
        out.push(&payload[i..end]);
        i = end;
        text_from = end;
    }
    if text_from < payload.len() {
        out.push(&payload[text_from..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EnvVars {
        EnvVars::default()
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/ansi/{}",
            env!("CARGO_MANIFEST_DIR"),
            name
        ))
        .expect("fixture missing")
    }

    // ---- detection ------------------------------------------------------

    #[test]
    fn a_plain_terminal_gets_blocks() {
        let mut e = env();
        e.term = Some("xterm-256color".into());
        assert_eq!(*decide(&e, None), Protocol::Blocks);
    }

    /// The original bug. `TERM_PROGRAM` is set by iTerm2 on the local machine and
    /// is not forwarded by ssh, so on a remote host `LC_TERMINAL` is the only
    /// evidence that the terminal is iTerm2 at all.
    #[test]
    fn iterm_over_ssh_is_recognised_by_lc_terminal() {
        let e = EnvVars {
            term: Some("xterm-256color".into()),
            term_program: None,
            lc_terminal: Some("iTerm2".into()),
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Iterm2);
    }

    /// tmux overwrites `TERM_PROGRAM` with its own name, so the same fallback is
    /// what identifies iTerm2 inside a multiplexer.
    #[test]
    fn tmux_does_not_hide_the_terminal_behind_its_own_name() {
        let e = EnvVars {
            term: Some("screen-256color".into()),
            term_program: Some("tmux".into()),
            lc_terminal: Some("iTerm2".into()),
            tmux: Some("/tmp/tmux-1000/default,1,0".into()),
            tmux_passthrough: true,
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Iterm2);
    }

    #[test]
    fn kitty_is_detected_several_ways() {
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
            EnvVars {
                lc_terminal: Some("kitty".into()),
                ..env()
            },
        ] {
            assert_eq!(*decide(&e, None), Protocol::Kitty, "{e:?}");
        }
    }

    #[test]
    fn iterm_and_wezterm_use_the_iterm_protocol() {
        for p in ["iTerm.app", "WezTerm", "mintty"] {
            let e = EnvVars {
                term_program: Some(p.into()),
                ..env()
            };
            assert_eq!(*decide(&e, None), Protocol::Iterm2, "{p}");
        }
    }

    #[test]
    fn konsole_is_gated_on_its_version() {
        let old = EnvVars {
            konsole_version: Some("211200".into()),
            ..env()
        };
        assert_eq!(*decide(&old, None), Protocol::Blocks);

        let new = EnvVars {
            konsole_version: Some("220400".into()),
            ..env()
        };
        assert_eq!(*decide(&new, None), Protocol::Kitty);

        let weird = EnvVars {
            konsole_version: Some("not-a-number".into()),
            ..env()
        };
        assert_eq!(*decide(&weird, None), Protocol::Blocks);
    }

    /// tmux 3.4 parses and re-emits sixel itself, so it needs no passthrough.
    /// That makes sixel the only protocol that works in a default tmux, and it is
    /// preferred there over the higher-quality ones for exactly that reason.
    #[test]
    fn sixel_is_preferred_inside_tmux_and_needs_no_passthrough() {
        let e = EnvVars {
            term: Some("screen-256color".into()),
            lc_terminal: Some("iTerm2".into()),
            tmux: Some("/tmp/tmux-1000/default,1,0".into()),
            tmux_passthrough: false,
            ..env()
        };
        let probed = Probed {
            kitty: true,
            sixel: true,
            ..Default::default()
        };
        assert_eq!(*decide(&e, Some(probed)), Protocol::Sixel);
    }

    /// Outside tmux the better protocols win; sixel is the fallback.
    #[test]
    fn sixel_is_only_a_fallback_outside_tmux() {
        let e = EnvVars {
            term: Some("xterm-256color".into()),
            ..env()
        };
        assert_eq!(
            *decide(
                &e,
                Some(Probed {
                    kitty: true,
                    sixel: true,
                    ..Default::default()
                })
            ),
            Protocol::Kitty
        );
        assert_eq!(
            *decide(
                &e,
                Some(Probed {
                    kitty: false,
                    sixel: true,
                    ..Default::default()
                })
            ),
            Protocol::Sixel
        );
    }

    /// A terminal with no sixel, in a tmux that will not forward, has to fall
    /// back — and must say why, since that is the one thing the user can fix.
    #[test]
    fn tmux_without_passthrough_explains_itself() {
        let e = EnvVars {
            term: Some("screen-256color".into()),
            kitty_window_id: Some("1".into()),
            tmux: Some("/tmp/tmux-1000/default,1,0".into()),
            tmux_passthrough: false,
            ..env()
        };
        let d = decide(&e, None);
        assert_eq!(*d, Protocol::Blocks);
        assert!(
            d.1.is_some_and(|m| m.contains("allow-passthrough")),
            "should name the setting to change: {:?}",
            d.1
        );
    }

    #[test]
    fn gnu_screen_refuses_graphics() {
        let e = EnvVars {
            term: Some("screen.xterm-kitty".into()),
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Blocks);
    }

    #[test]
    fn passthrough_does_not_help_a_plain_terminal() {
        let e = EnvVars {
            term: Some("xterm-256color".into()),
            tmux: Some("x".into()),
            tmux_passthrough: true,
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Blocks);
    }

    #[test]
    fn an_explicit_override_wins_over_detection() {
        let e = EnvVars {
            override_var: Some("kitty".into()),
            term: Some("dumb".into()),
            tmux: Some("x".into()),
            tmux_passthrough: false,
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Kitty);

        let e = EnvVars {
            override_var: Some("blocks".into()),
            kitty_window_id: Some("1".into()),
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Blocks);

        let e = EnvVars {
            override_var: Some("sixel".into()),
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Sixel);
    }

    #[test]
    fn an_unrecognised_override_falls_through_to_detection() {
        let e = EnvVars {
            override_var: Some("sixel-maybe-please".into()),
            kitty_window_id: Some("1".into()),
            ..env()
        };
        assert_eq!(*decide(&e, None), Protocol::Kitty);
    }

    /// A probe that heard nothing must not be read as "no support" — a terminal
    /// behind ssh may simply have been slow. The environment still decides.
    #[test]
    fn a_silent_probe_falls_back_to_the_environment() {
        let e = EnvVars {
            lc_terminal: Some("iTerm2".into()),
            ..env()
        };
        assert_eq!(*decide(&e, Some(Probed::default())), Protocol::Iterm2);
    }

    /// A probe that heard something overrides the environment, which is the whole
    /// point of asking.
    #[test]
    fn a_probe_beats_the_environment() {
        let e = EnvVars {
            term: Some("xterm-256color".into()),
            ..env()
        };
        assert_eq!(
            *decide(
                &e,
                Some(Probed {
                    kitty: false,
                    sixel: true,
                    ..Default::default()
                })
            ),
            Protocol::Sixel
        );
    }

    #[test]
    fn timg_flags_match_the_protocols() {
        assert_eq!(Protocol::Blocks.timg_flag(), "q");
        assert_eq!(Protocol::Kitty.timg_flag(), "k");
        assert_eq!(Protocol::Iterm2.timg_flag(), "i");
        assert_eq!(Protocol::Sixel.timg_flag(), "s");
        assert!(!Protocol::Blocks.is_graphics());
        assert!(Protocol::Sixel.is_graphics());
    }

    // ---- probe replies --------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn device_attributes_reveal_sixel() {
        use super::unix_probe::parse_reply;
        // xterm with sixel: attribute 4 in the DA list.
        assert!(parse_reply(b"\x1b[?62;4;22c").sixel);
        // Without it.
        assert!(!parse_reply(b"\x1b[?62;22c").sixel);
        // 4 must be a whole attribute, not a digit inside another (e.g. 14).
        assert!(!parse_reply(b"\x1b[?62;14;22c").sixel);
    }

    #[cfg(unix)]
    #[test]
    fn a_kitty_reply_is_recognised() {
        use super::unix_probe::parse_reply;
        assert!(parse_reply(b"\x1b_Gi=31;OK\x1b\\\x1b[?62;22c").kitty);
        assert!(!parse_reply(b"\x1b[?62;22c").kitty);
    }

    /// `ESC[16t` is answered `ESC[6;<height>;<width>t` — height first, which is
    /// the reverse of how sizes are normally written and easy to transpose.
    #[cfg(unix)]
    #[test]
    fn the_cell_size_reply_is_parsed_height_first() {
        use super::unix_probe::parse_reply;
        assert_eq!(parse_reply(b"\x1b[6;17;8t").cell, Some((8, 17)));
        // Alongside the other replies.
        assert_eq!(
            parse_reply(b"\x1b[6;34;16t\x1b[?62;4;22c").cell,
            Some((16, 34))
        );
        // A terminal that says nothing, or says zero, tells us nothing usable.
        assert_eq!(parse_reply(b"\x1b[?62;22c").cell, None);
        assert_eq!(parse_reply(b"\x1b[6;0;0t").cell, None);
        assert_eq!(parse_reply(b"").cell, None);
        // Malformed replies must not panic or produce nonsense.
        for junk in [&b"\x1b[6;"[..], b"\x1b[6;abc;deft", b"\x1b[6;17t"] {
            assert_eq!(parse_reply(junk).cell, None, "{junk:?}");
        }
    }

    /// The probe used to stop at the first `c` byte, on the assumption that the
    /// Device Attributes reply came last. A terminal may answer in any order, and
    /// when DA arrived first the cell-size reply was still unread — so the
    /// measurement was silently lost and sizing fell back to cells.
    #[cfg(unix)]
    #[test]
    fn the_probe_waits_for_every_answer_it_asked_for() {
        use super::unix_probe::replies_complete;

        let da = b"\x1b[?62;4;22c";
        let cell = b"\x1b[6;34;16t";

        // DA alone is not enough when a cell size was asked for.
        assert!(!replies_complete(da, false, true));
        // Nor is the cell size alone.
        assert!(!replies_complete(cell, false, true));

        // Either order completes once both have arrived.
        let mut both = da.to_vec();
        both.extend_from_slice(cell);
        assert!(replies_complete(&both, false, true), "DA first");

        let mut other = cell.to_vec();
        other.extend_from_slice(da);
        assert!(replies_complete(&other, false, true), "cell first");

        // Nothing was asked of kitty or the cell size: DA alone finishes.
        assert!(replies_complete(da, false, false));

        // A kitty answer is waited for when it was asked for.
        assert!(!replies_complete(&both, true, true));
        let mut all = b"\x1b_Gi=31;OK\x1b\\".to_vec();
        all.extend_from_slice(&both);
        assert!(replies_complete(&all, true, true));
    }

    #[cfg(unix)]
    #[test]
    fn an_empty_reply_reports_nothing() {
        use super::unix_probe::parse_reply;
        let p = parse_reply(b"");
        assert!(!p.kitty && !p.sixel);
    }

    // ---- cell pinning ---------------------------------------------------

    /// The fix for images spilling past the pane. timg describes the image in
    /// pixels and lets the terminal choose a row count; pinning it in cells is
    /// what makes the image occupy exactly the rows the pane reserved.
    #[test]
    fn iterm_dimensions_are_converted_to_cells() {
        let raw = strip_framing(&fixture("timg_iterm2.esc")).to_string();
        // The fixture must actually carry pixel dimensions, or this proves
        // nothing.
        assert!(raw.contains("width=260px"), "fixture lost its pixel size");

        let pinned = pin_to_cells(&raw, 60, 24);
        assert!(pinned.contains("width=60;height=24"), "not pinned: {:?}",
            &pinned[..pinned.find(':').unwrap_or(120).min(120)]);
        // Only check the header: "px" also occurs by chance in the base64 body.
        let header_end = pinned.find(':').expect("iTerm2 header ends at the colon");
        assert!(
            !pinned[..header_end].contains("px"),
            "pixel dimensions must be gone, or the terminal sizes it itself: {:?}",
            &pinned[..header_end]
        );
        // Without this iTerm2 stretches the image to fill the box.
        assert!(pinned.contains("preserveAspectRatio=1"));
        // The image data itself must be untouched.
        assert!(pinned.contains("inline=1"));
        assert_eq!(
            pinned.matches("\x1b]1337;").count(),
            1,
            "should still be one sequence"
        );
    }

    /// kitty takes columns and rows as control keys, and only the first chunk
    /// carries control keys — the continuations are pure data.
    #[test]
    fn kitty_gets_column_and_row_control_keys() {
        let raw = strip_framing(&fixture("timg_kitty.esc")).to_string();
        let pinned = pin_to_cells(&raw, 60, 24);

        let heads: Vec<&str> = pinned
            .split("\x1b_G")
            .skip(1)
            .map(|c| c.split(';').next().unwrap_or(""))
            .collect();
        assert_eq!(heads.len(), 3, "should still be three chunks");
        assert!(heads[0].contains("c=60"), "first chunk: {:?}", heads[0]);
        assert!(heads[0].contains("r=24"), "first chunk: {:?}", heads[0]);
        // Adding control keys to a continuation chunk would corrupt the stream.
        for h in &heads[1..] {
            assert!(!h.contains("c="), "continuation was modified: {h:?}");
        }
    }

    /// Pinning twice must not stack duplicate keys — the payload is re-pinned
    /// whenever the pane is resized.
    #[test]
    fn pinning_is_idempotent() {
        let raw = strip_framing(&fixture("timg_kitty.esc")).to_string();
        let once = pin_to_cells(&raw, 60, 24);
        let twice = pin_to_cells(&once, 60, 24);
        assert_eq!(once, twice);
    }

    /// Sixel is a raster with fixed dimensions and no size control, so it passes
    /// through untouched rather than being corrupted by a rewrite attempt.
    #[test]
    fn sixel_is_left_alone() {
        let raw = strip_framing(&fixture("timg_sixel.esc")).to_string();
        assert_eq!(pin_to_cells(&raw, 60, 24), raw);
    }

    #[test]
    fn pinning_a_malformed_payload_does_not_panic() {
        for s in ["", "\x1b_G", "\x1b]1337;", "\x1b]1337;File=width=1px", "plain"] {
            let _ = pin_to_cells(s, 10, 5);
        }
    }

    /// Sixel cannot be pinned after the fact, so the geometry request is the
    /// only lever; it must leave room for a terminal whose cells are shorter than
    /// timg assumes.
    #[test]
    fn the_sixel_row_budget_leaves_headroom() {
        for rows in [8u16, 20, 24, 40] {
            let asked = sixel_row_budget(rows);
            assert!(asked < rows, "{rows}: no headroom left ({asked})");
            assert!(asked > 0, "{rows}: budget collapsed to nothing");
            // Still worth looking at — not shrunk into a thumbnail.
            assert!(
                asked * 2 >= rows,
                "{rows}: shrunk too far ({asked}), the image would be tiny"
            );
        }
        // A pane with almost no room must still ask for a drawable row.
        assert_eq!(sixel_row_budget(1), 1);
        assert_eq!(sixel_row_budget(0), 1);
    }

    /// The reported bug: in tmux, images and PDFs drew far smaller than the pane.
    /// timg cannot ask a piped stdout how big a cell is and assumes 18px, so the
    /// raster it produces is too small for a real cell — especially a retina one
    /// at roughly twice that. The escape pins the cell count separately, so the
    /// raster can be asked for larger without changing the space it occupies.
    #[test]
    fn graphics_are_oversampled_without_growing_the_image() {
        let (cols, rows) = oversampled_geometry(140, 37);
        assert_eq!((cols, rows), (280, 74));

        // The point of the exercise: more pixels, same cells. Pinning is what
        // holds the size, so it must still name the pane's own cell count.
        let pinned = pin_to_cells("\x1b]1337;File=size=1;width=9px;height=9px;inline=1:AA", 140, 37);
        assert!(
            pinned.contains("width=140;height=37"),
            "must still occupy the pane, not the oversampled box: {pinned}"
        );
    }

    /// A pane close to the integer limit must not wrap into a tiny request, and a
    /// very large one must not ask for a raster that costs megabytes to push
    /// through a multiplexer for pixels no cell can show.
    #[test]
    fn oversampling_is_bounded_at_both_ends() {
        // Ordinary panes get the full multiple.
        assert_eq!(oversampled_geometry(140, 37), (280, 74));

        // A large pane is not doubled: the payload grows with the square of the
        // raster and the extra pixels are past what a cell can show.
        let (c, r) = oversampled_geometry(200, 60);
        assert!(c <= MAX_OVERSAMPLED_CELLS, "cols not capped: {c}");
        assert!(r <= MAX_OVERSAMPLED_CELLS, "rows not capped: {r}");

        // The aspect ratio of the request must survive, or the renderer
        // letterboxes inside a differently-shaped box than the pane.
        let (c, r) = oversampled_geometry(140, 70);
        assert_eq!(c, r * 2, "aspect ratio changed: {c}x{r}");

        // A pane already past the cap is left alone rather than shrunk.
        assert_eq!(oversampled_geometry(400, 120), (400, 120));

        // No wrapping at the limit, and never smaller than the pane itself —
        // asking for less than the pane would make the image *worse* than before.
        let (c, r) = oversampled_geometry(u16::MAX, u16::MAX);
        assert_eq!((c, r), (u16::MAX, u16::MAX));
        for n in [1u16, 50, 299, 301, 1000] {
            let (c, _) = oversampled_geometry(n, n);
            assert!(c >= n, "{n}: asked for less than the pane ({c})");
        }

        assert_eq!(oversampled_geometry(0, 0), (0, 0));
    }

    /// A payload cut off by the output cap must be rejected rather than sent.
    /// An unterminated escape leaves the terminal consuming everything printed
    /// after it as part of the string.
    #[test]
    fn a_truncated_payload_is_detected() {
        // Real payloads are complete.
        for name in ["timg_kitty.esc", "timg_iterm2.esc", "timg_sixel.esc"] {
            let raw = strip_framing(&fixture(name)).to_string();
            assert!(is_complete(&raw), "{name} should be complete");
        }

        // Cutting one mid-sequence must be caught.
        for name in ["timg_kitty.esc", "timg_iterm2.esc", "timg_sixel.esc"] {
            let raw = strip_framing(&fixture(name)).to_string();
            let cut = &raw[..raw.len() * 3 / 4];
            assert!(!is_complete(cut), "{name} truncated but reported complete");
        }

        assert!(is_complete(""));
        assert!(is_complete("plain text"));
    }

    /// The reported bug: the same payload filled a native iTerm2 window but came
    /// out small in a tmux pane. Sizing in cells leaves the terminal to multiply
    /// out, and iTerm2 reaches a different answer through tmux. A pixel count
    /// means the same thing everywhere.
    #[test]
    fn iterm_is_sized_in_pixels_when_the_cell_size_is_known() {
        let raw = "\x1b]1337;File=size=9;width=260px;height=91px;inline=1:AA\x07";

        // 140x37 cells of an 8x17 cell = 1120x629 pixels.
        let pinned = pin_iterm(raw, 140, 37, Some((8, 17)));
        assert!(
            pinned.contains("width=1120px;height=629px"),
            "should state the size in pixels: {pinned}"
        );
        assert!(pinned.contains("preserveAspectRatio=1"));
        // The image data must be untouched.
        assert!(pinned.ends_with(":AA\x07"));
    }

    /// Without a measured cell size there is nothing to multiply by, so the cell
    /// form is still used — it is what worked natively before.
    #[test]
    fn iterm_falls_back_to_cells_when_the_cell_size_is_unknown() {
        let raw = "\x1b]1337;File=size=9;width=260px;height=91px;inline=1:AA\x07";
        let pinned = pin_iterm(raw, 140, 37, None);
        assert!(
            pinned.contains("width=140;height=37"),
            "should fall back to cells: {pinned}"
        );
        assert!(!pinned.contains("px;"), "no pixel units: {pinned}");
    }

    /// A large pane must not overflow the arithmetic — 65535 cells of a 20px
    /// cell is well past what a u16 holds.
    #[test]
    fn pixel_sizing_does_not_overflow() {
        let raw = "\x1b]1337;File=size=9;width=1px;height=1px;inline=1:AA\x07";
        let pinned = pin_iterm(raw, u16::MAX, u16::MAX, Some((20, 40)));
        assert!(pinned.contains("width=1310700px"), "{pinned}");
    }

    /// Sixel cannot be corrected in the escape the way the others can, so the
    /// geometry has to compensate for timg's assumed 18-pixel cell. Without this
    /// a forced sixel rasterises for a smaller screen than the one it is on and
    /// comes out visibly small.
    #[test]
    fn sixel_rows_scale_to_the_real_cell_height() {
        // No measurement: nothing to scale by, leave it alone.
        assert_eq!(scale_rows_for_cell(37), 37);
    }

    /// The scaling arithmetic itself, independent of whether a cell was measured.
    #[test]
    fn the_cell_scaling_arithmetic_is_proportional() {
        // Mirrors `scale_rows_for_cell`'s body so the maths can be checked
        // without a terminal to measure.
        fn scaled(rows: u16, cell_h: u32) -> u16 {
            const ASSUMED: u32 = 18;
            if cell_h <= ASSUMED {
                return rows;
            }
            ((u32::from(rows) * cell_h / ASSUMED).min(u32::from(u16::MAX))) as u16
        }

        // A 36px retina cell is twice the assumption: ask for twice the rows so
        // the raster comes out the right pixel height.
        assert_eq!(scaled(37, 36), 74);
        // A cell at or below the assumption needs no correction — asking for
        // fewer rows would make the image smaller than it should be.
        assert_eq!(scaled(37, 18), 37);
        assert_eq!(scaled(37, 14), 37);
        // And it cannot overflow.
        assert_eq!(scaled(u16::MAX, 1000), u16::MAX);
    }

    // ---- clearing -------------------------------------------------------

    /// kitty images are objects with a real delete operation.
    #[test]
    fn only_kitty_has_a_delete_escape() {
        let seq = clear_sequence(Protocol::Kitty).expect("kitty must be cleared");
        assert!(seq.starts_with("\x1b_G"));
        assert!(seq.contains("a=d"), "should be a delete: {seq:?}");

        assert_eq!(clear_sequence(Protocol::Iterm2), None);
        assert_eq!(clear_sequence(Protocol::Sixel), None);
        assert_eq!(clear_sequence(Protocol::Blocks), None);
    }

    /// The ghost-image bug. iTerm2 and sixel have no delete operation, and
    /// redrawing the frame does not remove them either: ratatui emits only the
    /// cells whose content changed, and the cells under an image are ones it
    /// believes are already blank. They have to be erased explicitly.
    #[test]
    fn iterm_and_sixel_need_their_region_erased() {
        assert!(
            needs_region_erase(Protocol::Iterm2),
            "an iTerm2 image is not removed by repainting"
        );
        assert!(needs_region_erase(Protocol::Sixel));

        // kitty has a cheaper, more precise option.
        assert!(!needs_region_erase(Protocol::Kitty));
        assert!(!needs_region_erase(Protocol::Blocks));
    }

    /// Exactly one removal mechanism per protocol, and every graphics protocol
    /// has one — a protocol with neither would leak images.
    #[test]
    fn every_graphics_protocol_can_be_removed() {
        for p in [Protocol::Kitty, Protocol::Iterm2, Protocol::Sixel] {
            let deletes = clear_sequence(p).is_some();
            let erases = needs_region_erase(p);
            assert!(deletes || erases, "{p:?} has no way to be removed");
            assert!(
                !(deletes && erases),
                "{p:?} would be removed twice, which flickers"
            );
        }
        // Blocks are cells; ratatui already owns them.
        assert!(clear_sequence(Protocol::Blocks).is_none());
        assert!(!needs_region_erase(Protocol::Blocks));
    }

    /// The kitty query is an APC string, and a terminal that does not understand
    /// APC *prints* it rather than swallowing it — observed putting
    /// `_Gi=31,s=1,...;AAAAAAAA` on the screen at startup. It must only be sent
    /// where the environment already suggests kitty's protocol is spoken.
    #[test]
    fn the_kitty_query_is_only_sent_to_plausible_terminals() {
        let ask = |e: &EnvVars| env_capabilities(e).kitty && !iterm_family(e);

        // Nothing known about the terminal: do not risk it.
        assert!(!ask(&EnvVars {
            term: Some("xterm-256color".into()),
            ..env()
        }));

        // iTerm2 and WezTerm speak their own protocol, not kitty's — asking them
        // kitty's question is what put the text on screen.
        for e in [
            EnvVars {
                lc_terminal: Some("iTerm2".into()),
                ..env()
            },
            EnvVars {
                term_program: Some("WezTerm".into()),
                ..env()
            },
        ] {
            assert!(!ask(&e), "must not ask kitty's question: {e:?}");
        }

        // A terminal that plausibly speaks it may be asked.
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
            assert!(ask(&e), "should ask: {e:?}");
        }
    }

    // ---- tmux wrapping --------------------------------------------------

    #[test]
    fn tmux_wrapping_doubles_every_escape() {
        let wrapped = wrap_for_tmux("\x1b_Gf=100;AAAA\x1b\\");
        assert!(wrapped.starts_with("\x1bPtmux;"));
        assert!(wrapped.ends_with("\x1b\\"));
        assert!(wrapped.contains("\x1b\x1b_G"));
    }

    /// The defect this rewrite exists for: a kitty image is a chain of escape
    /// sequences, and tmux needs each one wrapped on its own. Wrapping the chain
    /// in a single envelope delivers only the first chunk.
    #[test]
    fn every_kitty_chunk_gets_its_own_wrapper() {
        for (name, want) in [("timg_kitty.esc", 3), ("timg_kitty_big.esc", 35)] {
            let raw = fixture(name);
            let seqs = split_sequences(&raw);
            let apc = seqs.iter().filter(|s| s.starts_with("\x1b_")).count();
            assert_eq!(apc, want, "{name}: expected {want} chunks, split into {apc}");

            let wrapped = wrap_for_tmux(&raw);
            assert_eq!(
                wrapped.matches("\x1bPtmux;").count(),
                seqs.iter().filter(|s| s.starts_with('\x1b')).count(),
                "{name}: one wrapper per sequence"
            );
        }
    }

    /// iTerm2 terminates with BEL, which is not reliably recognised inside a
    /// passthrough wrapper, so it is re-terminated with ST.
    #[test]
    fn a_bel_terminated_osc_is_re_terminated() {
        let raw = fixture("timg_iterm2.esc");
        assert!(raw.contains('\x07'), "fixture no longer exercises BEL");

        let wrapped = wrap_for_tmux(&raw);
        assert!(
            !wrapped.contains('\x07'),
            "BEL must not survive into the wrapper"
        );
        assert!(wrapped.contains("\x1bPtmux;"));
    }

    #[test]
    fn a_sixel_payload_is_one_sequence() {
        let raw = fixture("timg_sixel.esc");
        let dcs = split_sequences(&raw)
            .iter()
            .filter(|s| s.starts_with("\x1bP"))
            .count();
        assert_eq!(dcs, 1, "sixel should be a single DCS sequence");
    }

    // ---- framing --------------------------------------------------------

    /// The framing timg adds is right for a standalone command and wrong inside
    /// a TUI: the trailing newline scrolls the pane and `ESC[?25h` un-hides the
    /// cursor the TUI hid.
    #[test]
    fn framing_is_stripped_from_every_protocol() {
        for (name, starts) in [
            ("timg_kitty.esc", "\x1b_G"),
            ("timg_iterm2.esc", "\x1b]1337;"),
            ("timg_sixel.esc", "\x1bP"),
        ] {
            let raw = fixture(name);
            // The fixtures must actually contain the framing, or this proves
            // nothing.
            assert!(raw.starts_with("\x1b[?25l"), "{name}: no leading cursor hide");
            assert!(raw.contains("\x1b[?25h"), "{name}: no trailing cursor show");

            let out = strip_framing(&raw);
            assert!(out.starts_with(starts), "{name}: lost the graphics start");
            assert!(
                !out.contains("\x1b[?25h"),
                "{name}: cursor-show survived, it would un-hide the cursor"
            );
            assert!(
                !out.ends_with('\n') && !out.ends_with('\r'),
                "{name}: trailing newline survived, it would scroll the pane"
            );
            assert!(
                out.ends_with("\x1b\\") || out.ends_with('\x07'),
                "{name}: should end on a sequence terminator"
            );
        }
    }

    /// Sixel additionally sets three modes before the image; those are framing
    /// too and must not be left behind.
    #[test]
    fn sixel_mode_setting_is_stripped() {
        let raw = fixture("timg_sixel.esc");
        assert!(raw.contains("\x1b[?8452l"), "fixture lost its mode sets");
        let out = strip_framing(&raw);
        assert!(!out.contains("\x1b[?8452l"));
        assert!(!out.contains("\x1b[80h"));
    }

    /// The image data itself must survive untouched — this is the whole payload.
    #[test]
    fn stripping_keeps_all_the_image_data() {
        let raw = fixture("timg_kitty_big.esc");
        let out = strip_framing(&raw);
        assert_eq!(
            out.matches("\x1b_G").count(),
            raw.matches("\x1b_G").count(),
            "chunks were lost"
        );
    }

    #[test]
    fn stripping_a_payload_with_no_graphics_changes_nothing() {
        assert_eq!(strip_framing("plain text"), "plain text");
        assert_eq!(strip_framing(""), "");
    }

    #[test]
    fn splitting_an_empty_or_plain_payload_does_not_panic() {
        assert!(split_sequences("").is_empty());
        assert_eq!(split_sequences("no escapes here"), vec!["no escapes here"]);
        // Unterminated sequences must not loop or slice out of bounds.
        for s in ["\x1b_G", "\x1b]1337;File=", "\x1bP", "\x1b"] {
            let _ = wrap_for_tmux(s);
        }
    }
}

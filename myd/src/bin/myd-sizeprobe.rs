//! Which kitty sizing keys does this terminal honour?
//!
//! Inside a multiplexer myd sends images with the kitty graphics protocol,
//! because it chunks a picture into ~4KB escapes that tmux will actually
//! deliver. The size the image should occupy is stated with the `c=` and `r=`
//! control keys. If previews arrive far too small, the likely reason is that
//! those keys are ignored and the terminal sizes by the raster's own pixels
//! instead — they are the keys most often left out of a partial implementation.
//!
//! This draws one image four ways, one per screen, waiting for a keypress
//! between each. It is a binary rather than a shell script because the tmux
//! passthrough framing has to be exact: every escape wrapped in its own
//! envelope with each inner `ESC` doubled. Reproducing that in shell went wrong
//! twice, so this calls the same code the app itself uses.
//!
//!     myd-sizeprobe [image] [cols] [rows]

use std::io::{Read, Write};

/// One variant to draw.
struct Variant {
    label: &'static str,
    /// Whether to state the size in cells with `c=`/`r=`.
    sized: bool,
    /// Multiplier on the raster asked of `timg`.
    raster: u16,
}

const VARIANTS: [Variant; 4] = [
    Variant {
        label: "A: c=/r= cells, pane-sized raster  (what myd sends)",
        sized: true,
        raster: 1,
    },
    Variant {
        label: "B: c=/r= cells, doubled raster",
        sized: true,
        raster: 2,
    },
    Variant {
        label: "C: no sizing keys, pane-sized raster",
        sized: false,
        raster: 1,
    },
    Variant {
        label: "D: no sizing keys, doubled raster",
        sized: false,
        raster: 2,
    },
];

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let image = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/usr/share/pixmaps/ubuntu-logo-text.png".to_string());
    let cols: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let rows: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);

    if !std::path::Path::new(&image).is_file() {
        eprintln!("no such image: {image}");
        eprintln!("usage: myd-sizeprobe [image] [cols] [rows]");
        std::process::exit(2);
    }

    let in_tmux = std::env::var_os("TMUX").is_some();
    clear();
    println!("  kitty sizing probe");
    println!();
    println!("  image:  {image}");
    println!("  target: {cols}x{rows} cells");
    println!("  tmux:   {}", if in_tmux { "yes" } else { "no" });
    println!("  cell:   {:?}", myd::preview::graphics::cell_size());
    println!();
    println!("  Four ways of asking for the same size, one per screen.");
    println!("  Note which ones fill the pane.");
    print!("\n  [any key to start]");
    let _ = std::io::stdout().flush();
    wait_key();

    for v in &VARIANTS {
        clear();
        println!("  {}", v.label);
        println!(
            "  raster: {}x{} cells{}",
            cols * v.raster,
            rows * v.raster,
            if v.sized { "   sizing: c=/r=" } else { "   sizing: none" }
        );
        println!();
        let _ = std::io::stdout().flush();

        match render(&image, cols * v.raster, rows * v.raster, v.sized, cols, rows) {
            Ok(escape) => {
                let body = if in_tmux {
                    myd::preview::graphics::wrap_for_tmux(&escape)
                } else {
                    escape
                };
                let mut out = std::io::stdout();
                out.write_all(body.as_bytes())?;
                out.flush()?;
            }
            Err(e) => println!("  could not render: {e}"),
        }

        print!("\n\n  Does this fill the {cols}x{rows} pane?  [any key for next]");
        let _ = std::io::stdout().flush();
        wait_key();
    }

    clear();
    println!("  Done. Which filled the pane?");
    println!();
    println!("    A / B  -> c= and r= are honoured; sizing is not the fault.");
    println!("    C / D  -> the keys are ignored and the terminal sizes by the");
    println!("              raster's pixels, so the raster must be built at the");
    println!("              pane's real pixel size instead.");
    println!("    none   -> something else is scaling the image down.");
    println!();
    Ok(())
}

/// Render `image` through timg and return a kitty escape, optionally carrying
/// the cell-size control keys.
fn render(
    image: &str,
    raster_cols: u16,
    raster_rows: u16,
    sized: bool,
    cols: u16,
    rows: u16,
) -> anyhow::Result<String> {
    let out = std::process::Command::new("timg")
        .args([
            "-pk",
            &format!("-g{raster_cols}x{raster_rows}"),
            "-U",
            "--frames=1",
            image,
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    if !out.status.success() {
        anyhow::bail!("timg exited with {}", out.status);
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    // Drop the cursor hiding and trailing newline timg brackets its output with;
    // inside a TUI-shaped test they only move the cursor around.
    let stripped = myd::preview::graphics::strip_framing(&text).to_string();
    Ok(if sized {
        myd::preview::graphics::pin_to_cells(&stripped, cols, rows)
    } else {
        stripped
    })
}

fn clear() {
    print!("\x1b[2J\x1b[H");
    let _ = std::io::stdout().flush();
}

/// Wait for a single keypress, without needing Enter.
fn wait_key() {
    // Best effort: on a terminal, put it in raw mode so one byte is enough.
    // Anywhere else (a pipe, a test) fall through immediately.
    #[cfg(unix)]
    let restore = raw_mode();
    let mut byte = [0u8; 1];
    let _ = std::io::stdin().read(&mut byte);
    #[cfg(unix)]
    if let Some(saved) = restore {
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &saved);
        }
    }
}

#[cfg(unix)]
fn raw_mode() -> Option<libc::termios> {
    unsafe {
        let mut saved: libc::termios = std::mem::zeroed();
        if libc::tcgetattr(0, &mut saved) != 0 {
            return None;
        }
        let mut raw = saved;
        raw.c_lflag &= !(libc::ICANON | libc::ECHO);
        raw.c_cc[libc::VMIN] = 1;
        raw.c_cc[libc::VTIME] = 0;
        if libc::tcsetattr(0, libc::TCSANOW, &raw) != 0 {
            return None;
        }
        Some(saved)
    }
}

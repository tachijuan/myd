//! Dump the exact escape sequence the preview pane would send for one file.
//!
//! Diagnostic. The preview writes graphics escapes straight to the terminal,
//! outside anything a test can observe, so when an image does not appear there
//! is no way to tell whether the bytes were wrong or the terminal refused them.
//! This writes those same bytes to stdout, where they can be replayed, measured
//! or piped into a file.
//!
//!     myd-escape <file> [cols] [rows] > /tmp/esc.bin
//!     cat /tmp/esc.bin     # in the pane where previews fail
use std::io::Write;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: myd-escape <file> [cols] [rows]");
        std::process::exit(2);
    };
    let cols: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(140);
    let rows: u16 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(37);

    let reg = myd::vfs::BackendRegistry::new();
    let path = std::path::PathBuf::from(path);
    let content = myd::preview::load(
        reg.local(),
        myd::preview::PreviewRequest {
            path: myd::vfs::VPath::local(path.clone()),
            label: path,
            cols,
            rows,
            page: 0,
            cells_only: false,
            compact_listing: false,
            max_text_bytes: None,
        },
    )
    .await;

    match content {
        myd::preview::PreviewContent::Graphics { payload, .. } => {
            eprintln!(
                "protocol={:?} cell={:?} payload={} bytes",
                myd::preview::graphics::protocol(),
                myd::preview::graphics::cell_size(),
                payload.len()
            );
            // Wrapped exactly as the app wraps it, so what is dumped is what the
            // terminal would have received.
            let body = if std::env::var_os("TMUX").is_some() {
                myd::preview::graphics::wrap_for_tmux(&payload)
            } else {
                payload
            };
            std::io::stdout().write_all(body.as_bytes())?;
            std::io::stdout().flush()?;
        }
        myd::preview::PreviewContent::Note { message } => {
            eprintln!("no image: {message}");
            std::process::exit(1);
        }
        _ => {
            eprintln!("this file does not preview as an image");
            std::process::exit(1);
        }
    }
    Ok(())
}

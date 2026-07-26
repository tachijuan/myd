//! Headless transfer benchmark — the same engine the TUI uses, with no TUI.
//!
//! Exists so transfer timings can be measured without the render loop in the
//! way, scripted for comparison against the `sftp` binary, and run under
//! `perf`/`strace` at a real high-latency site.
//!
//! ```text
//! myd-transfer sftp://user@host/remote/big.bin /tmp/big.bin
//! myd-transfer /tmp/big.bin sftp://user@host/remote/big.bin
//! MYD_LOG=myd=debug myd-transfer --repeat 3 sftp://host/f /tmp/f
//! ```
//!
//! Either side may be remote. Passwords are read from the terminal only if the
//! key ladder fails, matching the TUI's behaviour.

use anyhow::{bail, Context, Result};
use clap::Parser;
use std::sync::Arc;
use std::time::Instant;

use myd::transfer::{
    run_transfer, TransferConfig, TransferId, TransferJob, TransferOutcome, TransferProgress,
};
use myd::utils::sizes::CancelToken;
use myd::vfs::sftp::{ConnectOutcome, Credentials, SftpTarget};
use myd::vfs::{BackendId, LocalFs, SftpFs, VPath, Vfs};

#[derive(Parser, Debug)]
#[command(
    name = "myd-transfer",
    about = "Transfer a file or directory using myd's engine, with timings"
)]
struct Cli {
    /// Source: a local path or sftp://[user@]host[:port]/path
    source: String,
    /// Destination: a local path or sftp://[user@]host[:port]/path
    dest: String,

    /// Run the transfer this many times and report each run.
    #[arg(long, default_value_t = 1)]
    repeat: usize,

    /// Concurrent transfers / files per directory level.
    #[arg(long)]
    max_parallel: Option<usize>,

    /// Bytes per chunk.
    #[arg(long)]
    chunk_size: Option<usize>,

    /// Concurrent chunk reads within one large file.
    #[arg(long)]
    chunks_in_flight: Option<usize>,

    /// Delete the destination between repeats (default: on).
    #[arg(long, default_value_t = true)]
    clean: bool,
}

/// A parsed endpoint: which backend to build, and the path on it.
enum Endpoint {
    Local(std::path::PathBuf),
    Remote(SftpTarget, std::path::PathBuf),
}

fn parse_endpoint(s: &str) -> Result<Endpoint> {
    // Only treat it as remote if it carries an explicit scheme; a bare path with
    // a colon is far more likely to be a local file than an scp-style target.
    if s.starts_with("sftp://") || s.starts_with("ssh://") {
        let target = SftpTarget::parse(s)?;
        let path = target
            .path
            .clone()
            .ok_or_else(|| anyhow::anyhow!("remote endpoint '{}' needs a path", s))?;
        Ok(Endpoint::Remote(target, path))
    } else {
        Ok(Endpoint::Local(std::path::PathBuf::from(s)))
    }
}

/// Connect, prompting on the terminal for a passphrase or password if needed.
async fn connect_remote(target: &SftpTarget) -> Result<SftpFs> {
    let mut creds = Credentials::default();
    // Bounded so a server that keeps asking cannot spin forever.
    for _ in 0..3 {
        match SftpFs::connect(target, &creds, true).await? {
            ConnectOutcome::Connected(fs) => return Ok(fs),
            ConnectOutcome::NeedsCredential(need) => match need {
                myd::vfs::sftp::AuthNeed::Passphrase { key_path } => {
                    let p = rpassword_prompt(&format!(
                        "Passphrase for {}: ",
                        key_path.display()
                    ))?;
                    creds.passphrase = Some(p);
                }
                myd::vfs::sftp::AuthNeed::Password { user, host, .. } => {
                    let p = rpassword_prompt(&format!("Password for {}@{}: ", user, host))?;
                    creds.password = Some(p);
                }
            },
        }
    }
    bail!("authentication failed after 3 attempts")
}

/// Read a secret from the terminal.
///
/// No `rpassword` dependency: this binary is a diagnostic tool, and echoing is
/// disabled with `stty` where available rather than pulling in a crate for it.
fn rpassword_prompt(prompt: &str) -> Result<String> {
    use std::io::{BufRead, Write};
    eprint!("{}", prompt);
    std::io::stderr().flush().ok();

    let echo_off = std::process::Command::new("stty")
        .arg("-echo")
        .stdin(std::process::Stdio::inherit())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;

    if echo_off {
        std::process::Command::new("stty")
            .arg("echo")
            .stdin(std::process::Stdio::inherit())
            .status()
            .ok();
        eprintln!();
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    myd::trace::init();

    let mut config = TransferConfig::default();
    if let Some(v) = cli.max_parallel {
        config.max_parallel = v;
    }
    if let Some(v) = cli.chunk_size {
        config.chunk_size = v;
    }
    if let Some(v) = cli.chunks_in_flight {
        config.chunks_in_flight = v;
    }

    let src_ep = parse_endpoint(&cli.source)?;
    let dest_ep = parse_endpoint(&cli.dest)?;

    let local: Arc<dyn Vfs> = Arc::new(LocalFs::new());

    // Backend ids only have to be distinct within this process; the registry the
    // TUI uses isn't needed for a single transfer.
    let connect_started = Instant::now();
    let (src_fs, src_path) = match &src_ep {
        Endpoint::Local(p) => (local.clone(), VPath::local(p)),
        Endpoint::Remote(t, p) => {
            let fs = connect_remote(t).await.context("source connect failed")?;
            let fs: Arc<dyn Vfs> = Arc::new(fs);
            (fs, VPath::new(BackendId(1), p))
        }
    };
    let (dest_fs, dest_path) = match &dest_ep {
        Endpoint::Local(p) => (local.clone(), VPath::local(p)),
        Endpoint::Remote(t, p) => {
            let fs = connect_remote(t).await.context("dest connect failed")?;
            let fs: Arc<dyn Vfs> = Arc::new(fs);
            (fs, VPath::new(BackendId(2), p))
        }
    };
    let connect_elapsed = connect_started.elapsed();

    println!(
        "connect: {:.2}s   config: max_parallel={} chunk_size={} chunks_in_flight={} (window={})",
        connect_elapsed.as_secs_f64(),
        config.max_parallel,
        config.chunk_size,
        config.chunks_in_flight,
        myd::transfer::large_file_chunks_in_flight(&config),
    );

    for run in 1..=cli.repeat {
        if cli.clean && run > 1 {
            dest_fs.remove_file(&dest_path).await.ok();
        }

        let progress = Arc::new(TransferProgress::new(0));
        let started = Instant::now();
        let outcome = run_transfer(TransferJob {
            id: TransferId(run as u64),
            src_fs: src_fs.clone(),
            dest_fs: dest_fs.clone(),
            src: src_path.clone(),
            dest: dest_path.clone(),
            progress: progress.clone(),
            cancel: CancelToken::new(),
            config,
        })
        .await?;

        let elapsed = started.elapsed();
        let bytes = progress.bytes_done();
        let secs = elapsed.as_secs_f64();
        let mibs = if secs > 0.0 {
            (bytes as f64 / (1024.0 * 1024.0)) / secs
        } else {
            0.0
        };
        match outcome {
            TransferOutcome::Done => println!(
                "run {}/{}: {:.3}s  {} bytes  {:.2} MiB/s",
                run, cli.repeat, secs, bytes, mibs
            ),
            TransferOutcome::Cancelled => println!("run {}/{}: cancelled", run, cli.repeat),
        }
    }

    Ok(())
}

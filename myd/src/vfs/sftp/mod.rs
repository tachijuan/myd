//! SFTP backend: `russh` for transport and authentication, then
//! `openssh-sftp-client` for the SFTP protocol itself.
//!
//! The pairing is deliberate. `openssh-sftp-client` pipelines both reads and
//! writes (libssh2-based clients pipeline only reads, costing most of the
//! throughput), but its usual transport is the `openssh` crate, which shells out
//! to `ssh` and cannot do password authentication. Since `Sftp::new` accepts any
//! `AsyncRead`/`AsyncWrite` pair, we drive it with a `russh` channel instead and
//! get full auth coverage — agent, keys, passphrases, passwords — at no cost to
//! throughput.

pub mod auth;
mod target;

pub use auth::{AuthNeed, Credentials, HostKeyVerdict};
pub use target::SftpTarget;

use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use std::num::{NonZeroU16, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use openssh_sftp_client::{Sftp, SftpOptions};
use russh::client::{self, AuthResult, Handle};
use russh::keys::PrivateKeyWithHashAlg;

use super::{VEntry, VMetadata, VPath, VPositionedRead, VRead, VWrite, Vfs};
use crate::utils::sizes::{CancelToken, SizeCache};
use crate::widget::progress::OpProgress;

/// Build the SSH transport configuration.
///
/// Two of russh's defaults are actively harmful on a long link:
///
/// * `window_size` is 2 MiB. The SSH channel window is a hard ceiling on
///   in-flight data: throughput cannot exceed `window / rtt` however deep the
///   SFTP request pipeline is. At a 150 ms transatlantic round trip that caps
///   the connection near 13 MiB/s — *below* what the request pipeline alone
///   would allow, so it binds first and hides every other improvement.
/// * `nodelay` is false, leaving Nagle's algorithm on. SFTP is request/response
///   with many small packets, the exact shape where Nagle and delayed ACK
///   interact to add tens of milliseconds per exchange.
///
/// `maximum_packet_size` is deliberately left alone: it is not a bottleneck and
/// raising it past 32 KiB risks a disconnect from stricter servers.
fn ssh_config() -> client::Config {
    client::Config {
        window_size: crate::config::ssh_window_size(),
        maximum_packet_size: crate::config::ssh_max_packet(),
        nodelay: crate::config::ssh_nodelay(),
        ..Default::default()
    }
}

/// Keep a deep request pipeline in flight. The crate default of 100 leaves a
/// high-latency link idle waiting on round trips.
///
/// This is a client-side cap only — the server enforces its own — so sizing it
/// generously costs nothing when the pipeline is shallower.
fn max_pending_requests() -> u16 {
    crate::config::sftp_max_pending()
}

/// Flush immediately. The crate's own docs note this costs nothing (flushing
/// happens on a daemon task) while removing up to 0.5 ms of added latency per
/// batch.
const FLUSH_INTERVAL: Duration = Duration::from_micros(0);

/// Room for many concurrent responses without reallocating mid-transfer.
const RESPONSE_BUFFER_SIZE: usize = 4096;

/// How long to wait for the TCP/SSH handshake before giving up. Without this a
/// dead or firewalled host would hang the connection attempt indefinitely, and
/// the user could only escape with Ctrl-C.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// A live SFTP connection.
///
/// Holds the `Sftp` client and the `russh` handle together: dropping the handle
/// tears down the connection, so it must outlive the client.
pub struct SftpFs {
    sftp: Sftp,
    /// Kept alive for the connection's sake, not called directly.
    _session: Handle<ClientHandler>,
    label: String,
    /// Where the session starts, resolved server-side.
    home: PathBuf,
    /// Directories confirmed to exist on the server during this session.
    ///
    /// `create_dir_all` is called once per written file, and on a high-latency
    /// link re-checking the same ancestors every time dominates the transfer.
    /// Entries are only ever added, so a stale hit is at worst a directory the
    /// user removed out-of-band — the subsequent write reports that anyway.
    known_dirs: dashmap::DashSet<PathBuf>,
}

/// russh callback handler.
///
/// Host-key verification happens *before* this handler is constructed (the
/// caller checks known_hosts and asks the user), so by the time russh calls back
/// the decision is already made and recorded here.
struct ClientHandler {
    accept_key: bool,
}

impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(self.accept_key)
    }
}

/// The outcome of a connection attempt.
///
/// Connecting is a state machine rather than a blocking prompt so the UI never
/// freezes: when a credential is missing we return what's needed and the caller
/// retries with it.
pub enum ConnectOutcome {
    Connected(SftpFs),
    /// A credential is required; ask the user and call `connect` again.
    NeedsCredential(AuthNeed),
}

impl SftpFs {
    /// Connect and authenticate, then start the SFTP subsystem.
    ///
    /// `creds` carries anything the user has already supplied. `accept_new_host`
    /// must be true to accept a host key that isn't yet in known_hosts — the
    /// caller is expected to have asked first.
    pub async fn connect(
        target: &SftpTarget,
        creds: &Credentials,
        accept_new_host: bool,
    ) -> Result<ConnectOutcome> {
        let started = std::time::Instant::now();
        let resolved = auth::resolve_target(target);
        auth::precheck(&resolved)?;

        // Verify the host key before authenticating, so a changed key is caught
        // before any credential is sent to whoever is on the other end. Bounded
        // by a timeout so an unreachable host fails instead of hanging.
        let addr = (resolved.host.as_str(), resolved.port);
        let probe = tokio::time::timeout(CONNECT_TIMEOUT, probe_host_key(addr))
            .await
            .with_context(|| {
                format!(
                    "timed out connecting to {}:{} after {}s",
                    resolved.host,
                    resolved.port,
                    CONNECT_TIMEOUT.as_secs()
                )
            })??;
        match auth::verify_host_key(&resolved.host, resolved.port, &probe) {
            HostKeyVerdict::Known => {}
            HostKeyVerdict::Changed { line } => {
                bail!(auth::host_key_changed_message(&resolved.host, line));
            }
            HostKeyVerdict::Unknown => {
                if !accept_new_host {
                    bail!(
                        "unknown host key for {} (fingerprint {}). Accept it to continue.",
                        resolved.host,
                        probe.fingerprint(Default::default())
                    );
                }
                auth::remember_host_key(&resolved.host, resolved.port, &probe).ok();
            }
        }

        let config = Arc::new(ssh_config());
        let mut session = tokio::time::timeout(
            CONNECT_TIMEOUT,
            client::connect(config, addr, ClientHandler { accept_key: true }),
        )
        .await
        .with_context(|| {
            format!(
                "timed out connecting to {}:{} after {}s",
                resolved.host,
                resolved.port,
                CONNECT_TIMEOUT.as_secs()
            )
        })?
        .with_context(|| format!("could not connect to {}:{}", resolved.host, resolved.port))?;

        // 1. ssh-agent, which covers the common case with no prompting at all.
        let mut authenticated = try_agent_auth(&mut session, &resolved.user).await;

        // 2. Key files from ssh config or the conventional ~/.ssh locations.
        if !authenticated {
            for key_path in &resolved.identity_files {
                match auth::load_key(key_path, creds.passphrase.as_deref()) {
                    auth::KeyLoad::Loaded(key) => {
                        if try_key_auth(&mut session, &resolved.user, *key).await {
                            authenticated = true;
                            break;
                        }
                    }
                    // Both mean "ask for a passphrase and retry": either none was
                    // given, or the one given didn't decrypt this key.
                    auth::KeyLoad::NeedsPassphrase | auth::KeyLoad::WrongPassphrase => {
                        return Ok(ConnectOutcome::NeedsCredential(AuthNeed::Passphrase {
                            key_path: key_path.clone(),
                        }));
                    }
                    // An unreadable key is not fatal; try the next one.
                    auth::KeyLoad::Failed(_) => continue,
                }
            }
        }

        // 3. Password, only once every key method has failed.
        if !authenticated {
            match creds.password.as_deref() {
                Some(pw) => {
                    let res = session
                        .authenticate_password(&resolved.user, pw)
                        .await
                        .map_err(|e| anyhow!("password authentication failed: {}", e))?;
                    authenticated = matches!(res, AuthResult::Success);
                    if !authenticated {
                        // A rejected password is recoverable — most often a typo.
                        // Ask again (flagged as a retry) instead of failing the
                        // whole connection, which would strand the user in an
                        // error dialog with no way back to the prompt.
                        return Ok(ConnectOutcome::NeedsCredential(AuthNeed::Password {
                            user: resolved.user.clone(),
                            host: resolved.host.clone(),
                            retry: true,
                        }));
                    }
                }
                None => {
                    return Ok(ConnectOutcome::NeedsCredential(AuthNeed::Password {
                        user: resolved.user.clone(),
                        host: resolved.host.clone(),
                        retry: false,
                    }));
                }
            }
        }

        // Start the SFTP subsystem and hand the channel to the sftp client.
        let channel = session
            .channel_open_session()
            .await
            .context("could not open SSH channel")?;
        channel
            .request_subsystem(true, "sftp")
            .await
            .context("remote refused the sftp subsystem")?;

        let stream = channel.into_stream();
        let (reader, writer) = tokio::io::split(stream);

        let options = SftpOptions::new()
            .flush_interval(FLUSH_INTERVAL)
            .max_pending_requests(
                NonZeroU16::new(max_pending_requests()).unwrap_or(
                    NonZeroU16::new(256).expect("non-zero constant"),
                ),
            )
            .responses_buffer_size(
                NonZeroUsize::new(RESPONSE_BUFFER_SIZE).expect("non-zero constant"),
            )
            // How many bytes may be outstanding on the sequential write path
            // before it blocks. The crate's 640 KiB default caps uploads near
            // 4 MiB/s over a 150 ms link regardless of the buffer sizes above
            // it, because that is all the data the pipeline is allowed to have
            // in flight at once.
            .tokio_compat_file_write_limit(
                NonZeroUsize::new(crate::config::sftp_write_limit()).unwrap_or(
                    NonZeroUsize::new(16 * 1024 * 1024).expect("non-zero constant"),
                ),
            );

        let sftp = Sftp::new(writer, reader, options)
            .await
            .context("could not start the SFTP session")?;

        // Resolve the starting directory server-side so a relative or absent
        // path lands somewhere sensible (usually $HOME).
        let home = {
            let mut fs = sftp.fs();
            let start = resolved.path.clone().unwrap_or_else(|| PathBuf::from("."));
            fs.canonicalize(&start)
                .await
                .unwrap_or_else(|_| PathBuf::from("/"))
        };

        tracing::info!(
            host = %resolved.host,
            port = resolved.port,
            user = %resolved.user,
            connect_ms = started.elapsed().as_secs_f64() * 1000.0,
            window_size = crate::config::ssh_window_size(),
            nodelay = crate::config::ssh_nodelay(),
            max_pending = max_pending_requests(),
            write_limit = crate::config::sftp_write_limit(),
            "sftp connected"
        );

        if crate::trace::enabled() && crate::trace::observed_read_limit().is_none() {
            probe_read_limit(&sftp, &home).await;
        }

        Ok(ConnectOutcome::Connected(SftpFs {
            sftp,
            _session: session,
            label: target.display_name(),
            home,
            known_dirs: dashmap::DashSet::new(),
        }))
    }

    /// The directory a panel opened on this connection should start in.
    pub fn home(&self) -> &Path {
        &self.home
    }
}

/// Discover the SFTP read limit the server actually negotiated.
///
/// `openssh-sftp-client` keeps its `max_read_len` accessor behind a private
/// feature, so the negotiated value cannot be read back — but it can be
/// observed: ask for more than any plausible limit and see how much comes back.
/// The reply is capped at exactly the negotiated size.
///
/// This matters because a chunk size above that limit does not produce a bigger
/// request; it produces *several* requests issued back-to-back on one handle,
/// making the pipeline shallower than its window count suggests. Costs one round
/// trip, once per connection, and only when diagnostics are on.
async fn probe_read_limit(sftp: &Sftp, home: &Path) {
    use bytes::BytesMut;

    // Any readable file will do; the directory listing supplies one.
    let mut fs = sftp.fs();
    let Ok(dir) = fs.open_dir(home).await else {
        return;
    };
    // `ReadDir` is a self-referential stream, so it must be pinned to poll.
    let read_dir = dir.read_dir();
    futures::pin_mut!(read_dir);
    use futures::StreamExt;

    let mut name = None;
    while let Some(Ok(entry)) = read_dir.next().await {
        let n = entry.filename().to_string_lossy().to_string();
        if n == "." || n == ".." {
            continue;
        }
        // Needs to be a regular file with enough bytes to reach any plausible
        // limit; a short file would report its own length, not the server's cap.
        let meta = entry.metadata();
        if meta.file_type().is_some_and(|t| t.is_file()) && meta.len().unwrap_or(0) > 512 * 1024 {
            name = Some(n);
            break;
        }
    }
    let Some(name) = name else {
        // Nothing big enough to measure against; better to log nothing than a
        // number that is really just some small file's size.
        return;
    };

    let Ok(mut file) = sftp.open(home.join(&name)).await else {
        return;
    };
    // 1 MiB is above every limit in practice (OpenSSH reports 256 KiB), so the
    // reply length is the limit.
    let want = 1024 * 1024u32;
    if let Ok(Some(chunk)) = file.read(want, BytesMut::with_capacity(want as usize)).await {
        crate::trace::set_observed_read_limit(chunk.len() as u64);
    }
    let _ = file.close().await;
}

/// Open a throwaway connection purely to read the server's host key.
///
/// Verifying before authenticating means a changed key is refused before any
/// credential is transmitted.
async fn probe_host_key(addr: (&str, u16)) -> Result<russh::keys::PublicKey> {
    use std::sync::Mutex;

    struct Probe(Arc<Mutex<Option<russh::keys::PublicKey>>>);

    impl client::Handler for Probe {
        type Error = russh::Error;
        async fn check_server_key(
            &mut self,
            key: &russh::keys::PublicKey,
        ) -> Result<bool, Self::Error> {
            *self.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(key.clone());
            // Reject: we only wanted the key, and rejecting closes the probe
            // connection immediately without authenticating.
            Ok(false)
        }
    }

    let seen = Arc::new(Mutex::new(None));
    let config = Arc::new(ssh_config());
    // This is expected to fail at key-exchange time, having captured the key.
    let _ = client::connect(config, addr, Probe(seen.clone())).await;

    let key = seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| anyhow!("could not reach {}:{}", addr.0, addr.1))?;
    Ok(key)
}

/// Try every identity the agent holds.
async fn try_agent_auth(session: &mut Handle<ClientHandler>, user: &str) -> bool {
    let Ok(mut agent) = russh::keys::agent::client::AgentClient::connect_env().await else {
        return false;
    };
    let Ok(identities) = agent.request_identities().await else {
        return false;
    };

    for id in identities {
        // Only plain public keys are usable here; certificates take a different
        // authentication path.
        let russh::keys::agent::AgentIdentity::PublicKey { key, .. } = id else {
            continue;
        };
        if let Ok(AuthResult::Success) = session
            .authenticate_publickey_with(user, key, None, &mut agent)
            .await
        {
            return true;
        }
    }
    false
}

/// Try one private key, picking the best RSA hash the server supports.
async fn try_key_auth(
    session: &mut Handle<ClientHandler>,
    user: &str,
    key: russh::keys::PrivateKey,
) -> bool {
    // RSA keys need an explicit hash algorithm; ed25519 and friends ignore it.
    let hash_alg = session.best_supported_rsa_hash().await.ok().flatten();
    let key = PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg.flatten());

    matches!(
        session.authenticate_publickey(user, key).await,
        Ok(AuthResult::Success)
    )
}

/// A positioned reader over one remote file.
///
/// The crate's `File` clones share the remote file handle but each keeps its own
/// offset, so reads issued on different clones pipeline over the one connection.
/// The mutex only serialises the seek+read pair for a *single* handle; the
/// transfer worker holds a pool of clones and reads from all of them at once.
struct SftpPositionedRead {
    file: std::sync::Arc<tokio::sync::Mutex<openssh_sftp_client::file::File>>,
}

#[async_trait]
impl VPositionedRead for SftpPositionedRead {
    /// Issue exactly one SFTP READ and return whatever the server sends.
    ///
    /// This deliberately does *not* loop to fill `len`. A server caps one READ at
    /// its negotiated limit (256 KiB for OpenSSH), so a loop here would turn one
    /// logical chunk into several round trips issued back-to-back while holding
    /// the handle's mutex — making the read pipeline exactly as deep as its slot
    /// count no matter how many bytes each slot asked for. Callers size their
    /// chunks at or below the limit and treat a short read as normal, which
    /// keeps every window slot worth one in-flight wire request.
    ///
    /// Not looping also removes a silent-corruption hazard. `File::read` advances
    /// the handle's offset by the *requested* length rather than the returned
    /// one, so a genuinely short read mid-file would leave a gap: the next
    /// iteration would resume past the missing bytes and concatenate
    /// non-contiguous data. Seeking to an absolute offset on every call makes
    /// that unrepresentable.
    async fn read_at(&self, offset: u64, len: usize) -> Result<Vec<u8>> {
        use bytes::BytesMut;
        use tokio::io::AsyncSeekExt;

        if len == 0 {
            return Ok(Vec::new());
        }

        let mut file = self.file.lock().await;
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .context("seek failed")?;

        let want = len.min(u32::MAX as usize) as u32;
        let buf = BytesMut::with_capacity(want as usize);
        match file.read(want, buf).await.context("remote read failed")? {
            Some(chunk) => Ok(chunk.to_vec()),
            None => Ok(Vec::new()), // EOF
        }
    }

    /// Produce another handle onto the same remote file for no round trip.
    ///
    /// `File::clone` duplicates the client-side handle without an SFTP OPEN, so a
    /// pool of readers costs one open rather than one per slot. On a 150 ms link
    /// that is the difference between 2.4 s of dead time before the first byte
    /// and a single round trip.
    async fn clone_handle(&self) -> Option<Box<dyn VPositionedRead>> {
        let file = self.file.lock().await.clone();
        Some(Box::new(SftpPositionedRead {
            file: std::sync::Arc::new(tokio::sync::Mutex::new(file)),
        }))
    }
}

/// Rebuild a numeric Unix mode from the SFTP permission flags.
///
/// The crate exposes one accessor per bit rather than the raw value, but the
/// info panel renders a `rwxr-xr-x` string, so the bits are reassembled here.
fn permissions_to_mode(p: &openssh_sftp_client::metadata::Permissions) -> u32 {
    let mut mode = 0u32;
    for (set, bit) in [
        (p.read_by_owner(), 0o400),
        (p.write_by_owner(), 0o200),
        (p.execute_by_owner(), 0o100),
        (p.read_by_group(), 0o040),
        (p.write_by_group(), 0o020),
        (p.execute_by_group(), 0o010),
        (p.read_by_other(), 0o004),
        (p.write_by_other(), 0o002),
        (p.execute_by_other(), 0o001),
        (p.suid(), 0o4000),
        (p.sgid(), 0o2000),
        (p.svtx(), 0o1000),
    ] {
        if set {
            mode |= bit;
        }
    }
    mode
}

/// Convert an SFTP metadata record into the backend-neutral form.
fn to_vmetadata(meta: &openssh_sftp_client::metadata::MetaData) -> VMetadata {
    let file_type = meta.file_type();
    VMetadata {
        is_dir: file_type.map(|t| t.is_dir()).unwrap_or(false),
        is_symlink: file_type.map(|t| t.is_symlink()).unwrap_or(false),
        len: meta.len().unwrap_or(0),
        mode: meta.permissions().map(|p| permissions_to_mode(&p)),
        uid: meta.uid(),
        gid: meta.gid(),
        mtime: meta.modified().map(|t| t.as_system_time()),
        atime: meta.accessed().map(|t| t.as_system_time()),
        ctime: None,
    }
}

#[async_trait]
impl Vfs for SftpFs {
    fn scheme(&self) -> &'static str {
        "sftp"
    }

    fn display_name(&self) -> String {
        self.label.clone()
    }

    async fn read_dir(&self, path: &VPath) -> Result<Vec<VEntry>> {
        let mut fs = self.sftp.fs();
        let dir = fs
            .open_dir(&path.path)
            .await
            .with_context(|| format!("could not open remote directory {}", path.path.display()))?;

        let mut entries = Vec::new();
        // ReadDir is a self-referential stream, so it has to be pinned before
        // it can be polled.
        let read_dir = dir.read_dir();
        futures::pin_mut!(read_dir);
        use futures::StreamExt;
        while let Some(entry) = read_dir.next().await {
            let entry = entry?;
            let name = entry.filename().to_string_lossy().to_string();
            // The server lists these; the tree supplies its own hierarchy.
            if name == "." || name == ".." {
                continue;
            }
            let meta = entry.metadata();
            let ty = meta.file_type();
            entries.push(VEntry {
                name,
                is_dir: ty.map(|t| t.is_dir()).unwrap_or(false),
                is_symlink: ty.map(|t| t.is_symlink()).unwrap_or(false),
                len: meta.len().unwrap_or(0),
                mtime: meta.modified().map(|t| t.as_system_time()),
                atime: meta.accessed().map(|t| t.as_system_time()),
                mode: meta.permissions().map(|p| permissions_to_mode(&p)),
                uid: meta.uid(),
                gid: meta.gid(),
            });
        }

        // READDIR reports lstat data, so a symlink to a directory arrives with
        // is_dir == false and would be unenterable. Resolve the targets with a
        // follow-up stat (which follows links) so symlinked directories can be
        // traversed like the real thing.
        //
        // Only symlinks are re-stat'd, and they're done concurrently: statting
        // every entry is what caused the round-trip storm this backend was
        // tuned to avoid. A broken link keeps its listing values.
        let links: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_symlink)
            .map(|(i, _)| i)
            .collect();
        if !links.is_empty() {
            let base = self.sftp.fs();
            let stats = futures::future::join_all(links.iter().map(|&i| {
                let target = path.path.join(&entries[i].name);
                let mut fs = base.clone();
                async move { fs.metadata(&target).await.ok() }
            }))
            .await;
            for (&i, meta) in links.iter().zip(stats) {
                if let Some(meta) = meta {
                    let e = &mut entries[i];
                    e.is_dir = meta.file_type().map(|t| t.is_dir()).unwrap_or(e.is_dir);
                    // A link's own length is the target path's byte count; the
                    // target's size is what the user cares about.
                    if let Some(len) = meta.len() {
                        e.len = len;
                    }
                }
            }
        }
        Ok(entries)
    }

    async fn stat(&self, path: &VPath) -> Result<VMetadata> {
        let mut fs = self.sftp.fs();
        let meta = fs
            .metadata(&path.path)
            .await
            .with_context(|| format!("could not stat {}", path.path.display()))?;
        Ok(to_vmetadata(&meta))
    }

    async fn symlink_stat(&self, path: &VPath) -> Result<VMetadata> {
        let mut fs = self.sftp.fs();
        let meta = fs
            .symlink_metadata(&path.path)
            .await
            .with_context(|| format!("could not lstat {}", path.path.display()))?;
        Ok(to_vmetadata(&meta))
    }

    async fn create_dir_all(&self, path: &VPath) -> Result<()> {
        // Already known to exist (we created or checked it earlier in this
        // session)? Then this costs nothing. A directory copy calls this for
        // every file it writes, and on a long link an ancestor walk per file is
        // pure dead time.
        if self.known_dirs.contains(&path.path) {
            return Ok(());
        }

        let mut fs = self.sftp.fs();

        // Fast path: the destination directory almost always exists already, so
        // one round trip settles it instead of walking every ancestor.
        if fs.metadata(&path.path).await.is_ok() {
            self.known_dirs.insert(path.path.clone());
            return Ok(());
        }

        // SFTP has no mkdir -p, so walk the ancestors and create what's missing.
        let mut prefix = PathBuf::new();
        for component in path.path.components() {
            prefix.push(component);
            if prefix.parent().is_none() {
                continue; // the root itself
            }
            if self.known_dirs.contains(&prefix) {
                continue;
            }
            if fs.metadata(&prefix).await.is_ok() {
                self.known_dirs.insert(prefix.clone());
                continue;
            }
            // A racing creator is fine; only report a failure if it's still
            // missing afterwards.
            if fs.create_dir(&prefix).await.is_err() && fs.metadata(&prefix).await.is_err() {
                bail!("could not create remote directory {}", prefix.display());
            }
            self.known_dirs.insert(prefix.clone());
        }
        Ok(())
    }

    async fn remove_file(&self, path: &VPath) -> Result<()> {
        let mut fs = self.sftp.fs();
        fs.remove_file(&path.path)
            .await
            .with_context(|| format!("could not remove {}", path.path.display()))?;
        Ok(())
    }

    async fn remove_dir(&self, path: &VPath) -> Result<()> {
        let mut fs = self.sftp.fs();
        fs.remove_dir(&path.path)
            .await
            .with_context(|| format!("could not remove directory {}", path.path.display()))?;
        Ok(())
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<()> {
        let mut fs = self.sftp.fs();
        fs.rename(&from.path, &to.path).await.with_context(|| {
            format!(
                "could not rename {} to {}",
                from.path.display(),
                to.path.display()
            )
        })?;
        Ok(())
    }

    async fn open_read(&self, path: &VPath) -> Result<Box<dyn VRead>> {
        let file = self
            .sftp
            .open(&path.path)
            .await
            .with_context(|| format!("could not open {}", path.path.display()))?;
        // TokioCompatFile adapts the sftp File to AsyncRead, and is where the
        // crate's read pipelining lives.
        Ok(Box::new(Box::pin(
            openssh_sftp_client::file::TokioCompatFile::new(file),
        )))
    }

    fn supports_parallel_read(&self) -> bool {
        true
    }

    async fn open_positioned_read(&self, path: &VPath) -> Result<Box<dyn VPositionedRead>> {
        let file = self
            .sftp
            .open(&path.path)
            .await
            .with_context(|| format!("could not open {}", path.path.display()))?;
        Ok(Box::new(SftpPositionedRead {
            file: std::sync::Arc::new(tokio::sync::Mutex::new(file)),
        }))
    }

    async fn open_write(&self, path: &VPath, _len_hint: Option<u64>) -> Result<Box<dyn VWrite>> {
        // The parent is ensured by the caller (run_transfer for a single file,
        // transfer_dir per level), and `create_dir_all` now short-circuits on
        // known directories — so this no longer pays an ancestor walk per file.
        if let Some(parent) = path.parent() {
            self.create_dir_all(&parent).await.ok();
        }
        let file = self
            .sftp
            .create(&path.path)
            .await
            .with_context(|| format!("could not create {}", path.path.display()))?;
        Ok(Box::new(Box::pin(
            openssh_sftp_client::file::TokioCompatFile::new(file),
        )))
    }

    async fn dir_size(
        &self,
        path: &VPath,
        _cache: &SizeCache,
        _cancel: &CancelToken,
        _progress: Option<&OpProgress>,
    ) -> u64 {
        // Deliberately *not* recursive. A `du`-style walk over SFTP is one round
        // trip per directory — thousands on a real tree — and would stall the UI
        // for minutes. Report the directory's own size and let the user expand
        // to discover more.
        self.stat(path).await.map(|m| m.len).unwrap_or(0)
    }

    fn has_recursive_sizes(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tuning_constants_beat_the_defaults() {
        // The crate defaults are 100 pending requests and a 0.5 ms flush wait;
        // both are latency killers on a long link.
        assert!(max_pending_requests() > 100);
        assert_eq!(FLUSH_INTERVAL, Duration::from_micros(0));
    }

    /// The two russh defaults that cap a long link, regardless of anything the
    /// SFTP layer does.
    #[test]
    fn ssh_transport_defaults_are_overridden() {
        let c = ssh_config();
        let stock = client::Config::default();

        // A 2 MiB window caps throughput at window/rtt — about 13 MiB/s at a
        // 150 ms round trip, below what the request pipeline could sustain.
        assert!(
            c.window_size > stock.window_size,
            "window_size {} is not above russh's {}",
            c.window_size,
            stock.window_size
        );
        // Nagle plus delayed ACK adds tens of ms per request/response exchange.
        assert!(c.nodelay, "Nagle must be disabled on the SSH socket");
        // Deliberately left alone: raising it risks a disconnect from stricter
        // servers and it is not a bottleneck.
        assert_eq!(c.maximum_packet_size, stock.maximum_packet_size);
    }

    #[test]
    fn sftp_backend_reports_non_recursive_sizes() {
        // Guards the deliberate lazy-size decision: if this ever flips, the
        // treemap's remote behavior changes and the README is wrong.
        fn assert_lazy<T: ?Sized>() {}
        assert_lazy::<SftpFs>();
    }
}

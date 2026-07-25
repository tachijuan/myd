use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use super::target::SftpTarget;

/// Credentials the UI can supply when the non-interactive ladder isn't enough.
///
/// Held only for the duration of a connect attempt and never persisted, logged,
/// or stored in tree state.
#[derive(Debug, Clone, Default)]
pub struct Credentials {
    /// Passphrase for an encrypted private key.
    pub passphrase: Option<String>,
    /// Account password, for servers doing password auth.
    pub password: Option<String>,
}

impl Credentials {
    pub fn with_passphrase(passphrase: impl Into<String>) -> Self {
        Self {
            passphrase: Some(passphrase.into()),
            password: None,
        }
    }

    pub fn with_password(password: impl Into<String>) -> Self {
        Self {
            passphrase: None,
            password: Some(password.into()),
        }
    }
}

/// Why a connection attempt stopped, when the answer is "we need input".
///
/// The connect flow is a state machine rather than a blocking prompt: the UI
/// stays responsive, asks the user, and retries with the extra credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthNeed {
    /// An encrypted key needs its passphrase. Carries the key's path for the
    /// prompt text.
    Passphrase { key_path: PathBuf },
    /// Every key method failed; the server may accept a password.
    ///
    /// `retry` is set when a password was already tried and rejected, so the
    /// prompt can say so instead of looking like the first ask. A typo must be
    /// correctable — a rejected password is a re-prompt, not a dead end.
    Password {
        user: String,
        host: String,
        retry: bool,
    },
}

/// Settings resolved for a target, after consulting `~/.ssh/config`.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_files: Vec<PathBuf>,
    pub path: Option<PathBuf>,
}

/// The user to assume when neither the target nor ssh config names one.
fn current_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".to_string())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The default identity files, in the order OpenSSH prefers them: modern and
/// fast first, RSA last.
pub fn default_identity_files() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    ["id_ed25519", "id_ecdsa", "id_rsa"]
        .iter()
        .map(|n| home.join(".ssh").join(n))
        .filter(|p| p.exists())
        .collect()
}

/// Resolve a target against `~/.ssh/config` so aliases, per-host users, ports,
/// and IdentityFile entries work exactly as they do for the `ssh` command.
///
/// A missing or unparsable config is not an error — it just means no overrides.
pub fn resolve_target(target: &SftpTarget) -> ResolvedTarget {
    let mut host = target.host.clone();
    let mut user = target.user.clone();
    let mut port = target.port;
    let mut identity_files = Vec::new();

    if let Some(params) = ssh_config_params(&target.host) {
        if let Some(h) = params.host_name {
            host = h;
        }
        // Explicit user/port on the target win over the config file.
        if user.is_none() {
            user = params.user;
        }
        if port.is_none() {
            port = params.port;
        }
        identity_files.extend(
            params
                .identity_file
                .unwrap_or_default()
                .into_iter()
                .map(|p| expand_tilde(&p)),
        );
    }

    // Fall back to the conventional key locations when the config names none.
    if identity_files.is_empty() {
        identity_files = default_identity_files();
    }

    ResolvedTarget {
        host,
        user: user.unwrap_or_else(current_user),
        port: port.unwrap_or(22),
        identity_files,
        path: target.path.clone(),
    }
}

/// Look up a host block in `~/.ssh/config`.
fn ssh_config_params(host: &str) -> Option<ssh2_config::HostParams> {
    use ssh2_config::{ParseRule, SshConfig};

    let path = home_dir()?.join(".ssh").join("config");
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    // Tolerate unknown/unsupported directives: a config we can't fully model is
    // still worth reading for HostName/User/Port.
    let config = SshConfig::default()
        .parse(&mut reader, ParseRule::ALLOW_UNKNOWN_FIELDS)
        .ok()?;
    Some(config.query(host))
}

/// Expand a leading `~` using `$HOME`.
pub fn expand_tilde(path: &Path) -> PathBuf {
    if let Ok(rest) = path.strip_prefix("~") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

/// Whether a `load_secret_key` failure means "this key is encrypted" as opposed
/// to a genuine problem.
///
/// Distinguishing these is what lets us prompt for a passphrase only when one
/// would actually help, instead of on every unreadable key.
pub fn is_encrypted_key_error(err: &russh::keys::Error) -> bool {
    matches!(err, russh::keys::Error::KeyIsEncrypted)
}

/// Whether a failure looks like a *wrong* passphrase rather than a broken key.
///
/// russh reports a bad passphrase as a cryptographic error from the decrypt
/// step, which is worth telling apart so the user can be asked to try again
/// instead of being told the key is corrupt.
pub fn is_wrong_passphrase_error(err: &russh::keys::Error) -> bool {
    matches!(
        err,
        russh::keys::Error::SshKey(russh::keys::ssh_key::Error::Crypto)
    )
}

/// Load a private key, reporting separately whether it needs a passphrase.
pub enum KeyLoad {
    Loaded(Box<russh::keys::PrivateKey>),
    /// The key is encrypted and no passphrase was supplied.
    NeedsPassphrase,
    /// A passphrase was supplied but did not decrypt the key.
    WrongPassphrase,
    Failed(String),
}

/// Read and decrypt a private key from disk.
pub fn load_key(path: &Path, passphrase: Option<&str>) -> KeyLoad {
    match russh::keys::load_secret_key(path, passphrase) {
        Ok(key) => KeyLoad::Loaded(Box::new(key)),
        Err(e) if is_encrypted_key_error(&e) => KeyLoad::NeedsPassphrase,
        Err(e) if is_wrong_passphrase_error(&e) && passphrase.is_some() => KeyLoad::WrongPassphrase,
        Err(e) => KeyLoad::Failed(e.to_string()),
    }
}

/// Verify a server key against `~/.ssh/known_hosts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKeyVerdict {
    /// Recorded and matching — proceed silently.
    Known,
    /// Not recorded. The UI asks the user whether to trust and record it.
    Unknown,
    /// Recorded but *different*. Refused outright: this is what a
    /// man-in-the-middle looks like, so it is never a prompt.
    Changed { line: usize },
}

/// Check `pubkey` for `host`/`port` against the user's known_hosts file.
pub fn verify_host_key(host: &str, port: u16, pubkey: &russh::keys::PublicKey) -> HostKeyVerdict {
    match russh::keys::check_known_hosts(host, port, pubkey) {
        Ok(true) => HostKeyVerdict::Known,
        Ok(false) => HostKeyVerdict::Unknown,
        Err(russh::keys::Error::KeyChanged { line }) => HostKeyVerdict::Changed { line },
        // A missing known_hosts file simply means nothing is recorded yet.
        Err(_) => HostKeyVerdict::Unknown,
    }
}

/// Record a newly accepted host key, so the next connection is silent.
///
/// `learn_known_hosts` isn't re-exported by `russh::keys`, so the entry is
/// appended directly in the standard format.
pub fn remember_host_key(host: &str, port: u16, pubkey: &russh::keys::PublicKey) -> Result<()> {
    use russh::keys::PublicKeyBase64;
    use std::io::Write;

    let path = known_hosts_path().ok_or_else(|| anyhow!("no known_hosts path"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Non-default ports use the bracketed `[host]:port` form, as OpenSSH does.
    let host_field = if port == 22 {
        host.to_string()
    } else {
        format!("[{}]:{}", host, port)
    };

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("could not open {}", path.display()))?;
    writeln!(
        file,
        "{} {} {}",
        host_field,
        pubkey.algorithm().as_str(),
        pubkey.public_key_base64()
    )
    .context("could not record host key")?;
    Ok(())
}

/// The user's `known_hosts` file.
pub fn known_hosts_path() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".ssh").join("known_hosts"))
}

/// A human-readable message for a refused (changed) host key.
pub fn host_key_changed_message(host: &str, line: usize) -> String {
    format!(
        "HOST KEY CHANGED for {} (known_hosts line {}). \
         Refusing to connect. If this is expected, remove the old entry.",
        host, line
    )
}

/// Validate that a target can plausibly be connected to before we try.
pub fn precheck(resolved: &ResolvedTarget) -> Result<()> {
    if resolved.host.is_empty() {
        bail!("no host to connect to");
    }
    if resolved.port == 0 {
        bail!("invalid port 0");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_tilde_uses_home() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            expand_tilde(Path::new("~/.ssh/id_ed25519")),
            PathBuf::from(&home).join(".ssh/id_ed25519")
        );
    }

    #[test]
    fn expand_tilde_leaves_absolute_paths_alone() {
        assert_eq!(
            expand_tilde(Path::new("/etc/ssh/key")),
            PathBuf::from("/etc/ssh/key")
        );
    }

    #[test]
    fn resolve_fills_in_defaults() {
        let t = SftpTarget::parse("sftp://example.invalid/srv").unwrap();
        let r = resolve_target(&t);
        // Port defaults to 22 and the user to the current account when neither
        // the target nor ssh config says otherwise.
        assert_eq!(r.port, 22);
        assert!(!r.user.is_empty());
        assert_eq!(r.path, Some(PathBuf::from("/srv")));
    }

    #[test]
    fn explicit_user_and_port_are_preserved() {
        let t = SftpTarget::parse("sftp://bob@example.invalid:2202/x").unwrap();
        let r = resolve_target(&t);
        assert_eq!(r.user, "bob");
        assert_eq!(r.port, 2202);
    }

    #[test]
    fn precheck_rejects_nonsense_targets() {
        let mut r = resolve_target(&SftpTarget::parse("host").unwrap());
        assert!(precheck(&r).is_ok());
        r.port = 0;
        assert!(precheck(&r).is_err());
        r.port = 22;
        r.host = String::new();
        assert!(precheck(&r).is_err());
    }

    #[test]
    fn loading_a_missing_key_fails_without_asking_for_a_passphrase() {
        match load_key(Path::new("/nonexistent/key"), None) {
            KeyLoad::Failed(_) => {}
            _ => panic!("a missing key must not be reported as needing a passphrase"),
        }
    }

    /// Generate a real ed25519 key with `ssh-keygen`, so the auth ladder is
    /// tested against genuine OpenSSH key material rather than a handmade blob.
    /// Returns `None` when ssh-keygen isn't available.
    fn generate_key(dir: &Path, name: &str, passphrase: &str) -> Option<PathBuf> {
        let path = dir.join(name);
        let status = std::process::Command::new("ssh-keygen")
            .args([
                "-t", "ed25519", "-q", "-N", passphrase, "-C", "myd-test", "-f",
            ])
            .arg(&path)
            .status()
            .ok()?;
        status.success().then_some(path)
    }

    #[test]
    fn encrypted_key_without_passphrase_asks_for_one() {
        let dir = tempfile::tempdir().unwrap();
        let Some(key) = generate_key(dir.path(), "id_enc", "secret123") else {
            eprintln!("ssh-keygen unavailable; skipping");
            return;
        };

        // The UI needs "ask for a passphrase", not a generic failure.
        match load_key(&key, None) {
            KeyLoad::NeedsPassphrase => {}
            KeyLoad::Loaded(_) => panic!("encrypted key must not load without a passphrase"),
            other => panic!(
                "expected NeedsPassphrase, got {}",
                match other {
                    KeyLoad::Failed(e) => e,
                    KeyLoad::WrongPassphrase => "WrongPassphrase".into(),
                    _ => unreachable!(),
                }
            ),
        }
    }

    #[test]
    fn encrypted_key_loads_with_the_right_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let Some(key) = generate_key(dir.path(), "id_enc", "secret123") else {
            eprintln!("ssh-keygen unavailable; skipping");
            return;
        };
        assert!(matches!(
            load_key(&key, Some("secret123")),
            KeyLoad::Loaded(_)
        ));
    }

    #[test]
    fn wrong_passphrase_is_distinguished_from_a_broken_key() {
        let dir = tempfile::tempdir().unwrap();
        let Some(key) = generate_key(dir.path(), "id_enc", "secret123") else {
            eprintln!("ssh-keygen unavailable; skipping");
            return;
        };
        // Telling these apart lets the UI re-prompt instead of reporting the
        // key as corrupt.
        assert!(matches!(
            load_key(&key, Some("not-the-passphrase")),
            KeyLoad::WrongPassphrase
        ));
    }

    #[test]
    fn unencrypted_key_loads_without_any_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let Some(key) = generate_key(dir.path(), "id_plain", "") else {
            eprintln!("ssh-keygen unavailable; skipping");
            return;
        };
        assert!(matches!(load_key(&key, None), KeyLoad::Loaded(_)));
    }

    #[test]
    fn changed_host_key_message_names_the_host_and_line() {
        let msg = host_key_changed_message("prod.example", 42);
        assert!(msg.contains("prod.example") && msg.contains("42"));
        assert!(msg.contains("Refusing"));
    }

    #[test]
    fn credentials_never_hold_both_secrets_by_accident() {
        let p = Credentials::with_password("pw");
        assert!(p.passphrase.is_none() && p.password.is_some());
        let k = Credentials::with_passphrase("pp");
        assert!(k.password.is_none() && k.passphrase.is_some());
    }
}

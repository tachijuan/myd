use anyhow::{bail, Result};
use std::path::PathBuf;

/// A parsed connection target: where to connect, as whom, and where to start.
///
/// Accepts `sftp://[user@]host[:port][/path]` as well as the bare
/// `[user@]host[:/path]` forms people type from habit. Unspecified fields are
/// filled in later from `~/.ssh/config` and the environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SftpTarget {
    /// Host as typed — may be an ssh config alias rather than a real hostname.
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
    /// Starting directory. `None` means the server's default (usually `$HOME`).
    pub path: Option<PathBuf>,
}

impl SftpTarget {
    /// Parse a user-supplied connection string.
    pub fn parse(input: &str) -> Result<Self> {
        let s = input.trim();
        if s.is_empty() {
            bail!("empty connection target");
        }

        // Strip a scheme if present. Only sftp:// and ssh:// make sense here;
        // anything else is a typo worth reporting rather than guessing at.
        let rest = if let Some((scheme, rest)) = s.split_once("://") {
            match scheme {
                "sftp" | "ssh" => rest,
                other => bail!("unsupported scheme '{}': expected sftp://", other),
            }
        } else {
            s
        };

        // Split userinfo from the host. rsplit so a password-ish '@' in the
        // user part doesn't confuse the host lookup.
        let (user, hostpart) = match rest.rsplit_once('@') {
            Some((u, h)) if !u.is_empty() => (Some(u.to_string()), h),
            _ => (None, rest),
        };

        // Split the path off the host. The first '/' starts the path; a ':'
        // before it introduces either a port or (scp-style) a path.
        let (hostport, path) = match hostpart.find('/') {
            Some(i) => (&hostpart[..i], Some(PathBuf::from(&hostpart[i..]))),
            None => (hostpart, None),
        };

        let (host, port, path) = match hostport.rsplit_once(':') {
            Some((h, tail)) if !tail.is_empty() => {
                match tail.parse::<u16>() {
                    Ok(p) => (h.to_string(), Some(p), path),
                    // `host:/some/path` and `host:path` are scp-style, not a port.
                    Err(_) if path.is_none() => (h.to_string(), None, Some(PathBuf::from(tail))),
                    Err(_) => bail!("invalid port '{}'", tail),
                }
            }
            // Trailing ':' with a path already split off, e.g. "host:/tmp".
            Some((h, _)) => (h.to_string(), None, path),
            None => (hostport.to_string(), None, path),
        };

        if host.is_empty() {
            bail!("no host in '{}'", input);
        }

        Ok(Self {
            host,
            user,
            port,
            path,
        })
    }

    /// The label shown in the panel title, e.g. `user@host`.
    pub fn display_name(&self) -> String {
        match &self.user {
            Some(u) => format!("{}@{}", u, self.host),
            None => self.host.clone(),
        }
    }

    /// The canonical URL for this target — the inverse of [`parse`](Self::parse).
    ///
    /// Saved hosts round-trip through here, so anything the catalog stores can be
    /// handed straight back to the existing connect path as a string.
    pub fn to_url(&self) -> String {
        let mut s = String::from("sftp://");
        if let Some(u) = &self.user {
            s.push_str(u);
            s.push('@');
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        if let Some(path) = &self.path {
            let p = path.to_string_lossy();
            if !p.starts_with('/') {
                s.push('/');
            }
            s.push_str(&p);
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_url() {
        let t = SftpTarget::parse("sftp://alice@example.com:2222/srv/data").unwrap();
        assert_eq!(t.host, "example.com");
        assert_eq!(t.user.as_deref(), Some("alice"));
        assert_eq!(t.port, Some(2222));
        assert_eq!(t.path, Some(PathBuf::from("/srv/data")));
    }

    #[test]
    fn parses_a_bare_host() {
        let t = SftpTarget::parse("prod").unwrap();
        assert_eq!(t.host, "prod");
        assert!(t.user.is_none() && t.port.is_none() && t.path.is_none());
        assert_eq!(t.display_name(), "prod");
    }

    #[test]
    fn parses_user_and_host_without_scheme() {
        let t = SftpTarget::parse("bob@server.local").unwrap();
        assert_eq!(
            (t.host.as_str(), t.user.as_deref()),
            ("server.local", Some("bob"))
        );
        assert_eq!(t.display_name(), "bob@server.local");
    }

    #[test]
    fn parses_scheme_with_path_but_no_user() {
        let t = SftpTarget::parse("sftp://example.com/var/log").unwrap();
        assert_eq!(t.host, "example.com");
        assert!(t.user.is_none());
        assert_eq!(t.path, Some(PathBuf::from("/var/log")));
    }

    #[test]
    fn treats_scp_style_colon_path_as_a_path_not_a_port() {
        let t = SftpTarget::parse("host:/var/tmp").unwrap();
        assert_eq!(t.host, "host");
        assert_eq!(t.port, None);
        assert_eq!(t.path, Some(PathBuf::from("/var/tmp")));
    }

    #[test]
    fn ssh_scheme_is_accepted() {
        assert_eq!(SftpTarget::parse("ssh://h").unwrap().host, "h");
    }

    #[test]
    fn rejects_unsupported_scheme_rather_than_guessing() {
        let err = SftpTarget::parse("ftp://host/x").unwrap_err().to_string();
        assert!(err.contains("unsupported scheme"), "{}", err);
    }

    #[test]
    fn rejects_empty_and_hostless_input() {
        assert!(SftpTarget::parse("").is_err());
        assert!(SftpTarget::parse("   ").is_err());
        assert!(SftpTarget::parse("sftp://").is_err());
    }

    #[test]
    fn rejects_a_bad_port() {
        assert!(SftpTarget::parse("sftp://host:99999/x").is_err());
        assert!(SftpTarget::parse("sftp://host:abc/x").is_err());
    }

    #[test]
    fn port_and_path_together() {
        let t = SftpTarget::parse("sftp://h:22/").unwrap();
        assert_eq!(t.port, Some(22));
        assert_eq!(t.path, Some(PathBuf::from("/")));
    }
}

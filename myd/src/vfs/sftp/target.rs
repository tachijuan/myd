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

        // Split host, port and path. The host runs up to the first ':' or '/',
        // whichever comes first — splitting on the last ':' mis-set the host to
        // "host:2222" for `host:2222:dir`, since the port and an scp-style path
        // can both be present.
        let host_end = hostpart.find([':', '/']).unwrap_or(hostpart.len());
        let host = &hostpart[..host_end];
        let mut rest = &hostpart[host_end..];

        // An optional ':port'. Only digits count: `host:dir` is scp-style, so a
        // non-numeric segment here is a path, not a bad port. A numeric one is
        // unambiguously the port, and any path follows it.
        let mut port = None;
        if let Some(after) = rest.strip_prefix(':') {
            let seg_end = after.find([':', '/']).unwrap_or(after.len());
            let seg = &after[..seg_end];
            if !seg.is_empty() && seg.chars().all(|c| c.is_ascii_digit()) {
                port = Some(
                    seg.parse::<u16>()
                        .map_err(|_| anyhow::anyhow!("invalid port '{}'", seg))?,
                );
                rest = &after[seg_end..];
            }
        }

        // What remains is the path, in one of two notations:
        //
        //   ':path'  scp-style, relative to the login directory (`~/path`)
        //   '/path'  absolute, as given
        //
        // A ':' followed by '/' is absolute too — `host:/some/path` names the
        // real root, matching scp. Relative paths are kept relative here and
        // resolved server-side against $HOME on connect.
        //
        // A lone trailing '/' is dropped rather than read as the root. Typing
        // `sftp://host/` is how a URL is habitually ended, and taking it as "/"
        // opened the server's root instead of the home directory the same
        // address without the slash would have given — a surprise, and a slow
        // one on a large filesystem. `sftp://host//` still asks for the root,
        // which is the only way left to say it explicitly.
        let path = if let Some(after) = rest.strip_prefix(':') {
            // Everything after the colon, absolute or relative as written.
            match after {
                "" => None,
                "/" => Some(PathBuf::from("/")),
                other => Some(PathBuf::from(other)),
            }
        } else {
            // `//` is normalised to `/` rather than passed on literally: it is a
            // way of spelling the root here, not part of the name, and the
            // server should be addressed with the path it expects.
            match rest {
                "" | "/" => None,
                "//" => Some(PathBuf::from("/")),
                other => Some(PathBuf::from(other)),
            }
        };

        let host = host.to_string();

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
            if p.starts_with('/') {
                // The root has to be written as '//', because a single trailing
                // slash now parses as "no path given". Saved hosts round-trip
                // through here, so emitting "sftp://host/" for a host pinned to
                // the root would quietly move it to the home directory the next
                // time it was opened.
                if p == "/" {
                    s.push('/');
                }
                s.push_str(&p);
            } else {
                // A relative path keeps the scp-style colon. Writing it as
                // '/path' made it absolute, so saving `host:dir` as a favourite
                // and reopening it went to /dir instead of ~/dir.
                s.push(':');
                s.push_str(&p);
            }
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
        // Out of range for a port, and all digits, so it cannot be a path either.
        assert!(SftpTarget::parse("sftp://host:99999/x").is_err());
        // `host:abc/x` is not a bad port — it is an scp-style relative path.
        let t = SftpTarget::parse("sftp://host:abc/x").unwrap();
        assert_eq!(t.port, None);
        assert_eq!(t.path, Some(PathBuf::from("abc/x")));
    }

    /// `host:dir` means `~/dir`; only a leading '/' asks for the real root.
    ///
    /// The colon form is how scp addresses a path relative to the login
    /// directory. A relative path is kept relative here and canonicalized
    /// server-side against $HOME on connect.
    #[test]
    fn a_colon_path_is_relative_to_home_unless_absolute() {
        // Relative: no leading slash, so it resolves under $HOME.
        for s in ["sftp://user@remote.com:dir", "user@remote.com:dir"] {
            let t = SftpTarget::parse(s).unwrap();
            assert_eq!(t.host, "remote.com");
            assert_eq!(t.user.as_deref(), Some("user"));
            assert_eq!(t.port, None);
            assert_eq!(t.path, Some(PathBuf::from("dir")), "{} should be ~/dir", s);
            assert!(!t.path.unwrap().is_absolute(), "{} must stay relative", s);
        }

        // Absolute: the colon introduces a full path, which is used as given.
        for s in ["sftp://user@remote.com:/some/path", "user@remote.com:/some/path"] {
            let t = SftpTarget::parse(s).unwrap();
            assert_eq!(t.host, "remote.com");
            assert_eq!(
                t.path,
                Some(PathBuf::from("/some/path")),
                "{} should be an absolute path",
                s
            );
        }

        // The slash form is absolute, as in any URL.
        let t = SftpTarget::parse("sftp://user@remote.com/dir").unwrap();
        assert_eq!(t.path, Some(PathBuf::from("/dir")));
    }

    /// A multi-segment relative path is a path, not a malformed port.
    ///
    /// The host was split on the *last* ':' and the path on the first '/', so
    /// `host:dir/sub` left "dir" sitting where the port belonged and was
    /// rejected outright as "invalid port 'dir'".
    #[test]
    fn a_relative_colon_path_may_have_several_segments() {
        let t = SftpTarget::parse("sftp://user@remote.com:dir/sub").unwrap();
        assert_eq!(t.host, "remote.com");
        assert_eq!(t.port, None);
        assert_eq!(t.path, Some(PathBuf::from("dir/sub")));
    }

    /// A port and an scp-style relative path can be given together.
    ///
    /// `rsplit_once(':')` took the last colon, so the host came back as
    /// "remote.com:2222" — a host that does not resolve.
    #[test]
    fn a_port_and_a_relative_path_coexist() {
        let t = SftpTarget::parse("sftp://user@remote.com:2222:dir").unwrap();
        assert_eq!(t.host, "remote.com");
        assert_eq!(t.port, Some(2222));
        assert_eq!(t.path, Some(PathBuf::from("dir")));
    }

    /// A relative path must survive being saved and reopened.
    ///
    /// `to_url` wrote every path with a leading '/', so a favourite saved as
    /// `host:dir` came back as `sftp://host/dir` and opened /dir instead of
    /// ~/dir — the starting directory silently moved.
    #[test]
    fn a_relative_path_round_trips_through_a_url() {
        for s in [
            "sftp://user@remote.com:dir",
            "sftp://user@remote.com:dir/sub",
            "sftp://user@remote.com:2222:dir",
        ] {
            let t = SftpTarget::parse(s).unwrap();
            let url = t.to_url();
            let back = SftpTarget::parse(&url).unwrap();
            assert_eq!(back, t, "{} became {} and re-parsed differently", s, url);
            assert!(
                !back.path.as_ref().unwrap().is_absolute(),
                "{} became absolute as {}",
                s,
                url
            );
        }
    }

    #[test]
    fn port_and_path_together() {
        // The trailing slash is dropped here as everywhere else, so this is a
        // port with no path rather than a port plus the root.
        let t = SftpTarget::parse("sftp://h:22/").unwrap();
        assert_eq!(t.port, Some(22));
        assert_eq!(t.path, None);
        // Spelled out, the port survives alongside the root.
        let t = SftpTarget::parse("sftp://h:22//").unwrap();
        assert_eq!(t.port, Some(22));
        assert_eq!(t.path, Some(PathBuf::from("/")));
    }

    #[test]
    fn a_trailing_slash_is_not_the_root() {
        // Ending a URL with '/' is habit, not a request for the server's root —
        // it means the same as leaving it off, which is the default directory.
        for s in ["sftp://gb10/", "gb10/", "sftp://me@gb10/"] {
            let t = SftpTarget::parse(s).unwrap();
            assert_eq!(t.host, "gb10");
            assert_eq!(t.path, None, "{} should not select the root", s);
        }
    }

    #[test]
    fn a_double_slash_asks_for_the_root() {
        // The explicit way to say "the root", now that one slash does not.
        // Normalised to a single '/', since '//' is this app's notation rather
        // than a path the server should be sent.
        for s in ["sftp://gb10//", "gb10//", "sftp://me@gb10//"] {
            let t = SftpTarget::parse(s).unwrap();
            assert_eq!(t.host, "gb10");
            assert_eq!(t.path, Some(PathBuf::from("/")), "{} should be the root", s);
        }
    }

    #[test]
    fn a_real_path_keeps_its_trailing_slash_behaviour() {
        // Only a *lone* slash is dropped. A path that happens to end in one is
        // still that path — the rule is about the empty case, not about
        // stripping separators generally.
        let t = SftpTarget::parse("sftp://gb10/srv/data/").unwrap();
        assert_eq!(t.path, Some(PathBuf::from("/srv/data/")));
        let t = SftpTarget::parse("sftp://gb10/srv").unwrap();
        assert_eq!(t.path, Some(PathBuf::from("/srv")));
    }

    #[test]
    fn the_root_round_trips_through_a_url() {
        // Saved hosts are stored as URLs and re-parsed on use, so a saved root
        // has to come back as the root. to_url emits "sftp://h/" for it, which
        // now parses as the default directory — so it must emit the double
        // slash instead.
        let t = SftpTarget::parse("sftp://gb10//").unwrap();
        let round = SftpTarget::parse(&t.to_url()).unwrap();
        assert_eq!(round.path, Some(PathBuf::from("/")), "url was {}", t.to_url());
        assert_eq!(round, t);
    }
}

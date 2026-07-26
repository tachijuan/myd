//! The dialing directory: saved remote locations.
//!
//! Connecting used to mean typing a full `sftp://user@host:port/path` every
//! time. This is a small persistent catalog of the places you actually go,
//! ordered most-recently-connected first so the last few are one keystroke away.
//!
//! **No passwords are stored, ever.** An entry records where to connect and as
//! whom; authentication still goes through the usual ladder (ssh-agent, then
//! `~/.ssh` keys, then a password prompt). Writing secrets to a config file
//! would be a meaningful downgrade from what ssh already does well.
//!
//! Stored as TOML at `~/.config/myd/hosts.toml` (honouring `$XDG_CONFIG_HOME`),
//! hand-editable on purpose:
//!
//! ```toml
//! [[host]]
//! label = "prod-fr"
//! user = "juan"
//! host = "prod.example.com"
//! port = 2222
//! path = "/srv/app"
//! uses = 47
//! last_used = "2026-07-26T10:14:00Z"
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::vfs::sftp::SftpTarget;

/// One saved location.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedHost {
    /// What the user calls this place. Unique within the catalog, and the key
    /// for edits and deletes.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Hostname or IP.
    pub host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Directory to open on arrival. `None` means the server's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Times connected. Shown in the picker; does not affect ordering.
    #[serde(default)]
    pub uses: u64,
    /// RFC 3339 timestamp of the last connection. This is what the quick list is
    /// ordered by — newest first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

impl SavedHost {
    pub fn new(label: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            host: host.into(),
            ..Default::default()
        }
    }

    /// The connection string, for [`SftpTarget::parse`].
    pub fn to_url(&self) -> String {
        let mut s = String::from("sftp://");
        if let Some(u) = &self.user {
            if !u.is_empty() {
                s.push_str(u);
                s.push('@');
            }
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        if let Some(path) = &self.path {
            if !path.is_empty() {
                if !path.starts_with('/') {
                    s.push('/');
                }
                s.push_str(path);
            }
        }
        s
    }

    /// `user@host:port` for the picker's second column.
    pub fn target_display(&self) -> String {
        let mut s = String::new();
        if let Some(u) = &self.user {
            if !u.is_empty() {
                s.push_str(u);
                s.push('@');
            }
        }
        s.push_str(&self.host);
        if let Some(p) = self.port {
            s.push(':');
            s.push_str(&p.to_string());
        }
        if let Some(path) = &self.path {
            if !path.is_empty() {
                s.push(' ');
                s.push_str(path);
            }
        }
        s
    }

    /// Build an entry from anything [`SftpTarget::parse`] accepts.
    pub fn from_url(label: impl Into<String>, url: &str) -> Result<Self> {
        let t = SftpTarget::parse(url)?;
        Ok(Self {
            label: label.into(),
            user: t.user,
            host: t.host,
            port: t.port,
            path: t.path.map(|p| p.to_string_lossy().to_string()),
            uses: 0,
            last_used: None,
        })
    }

    /// Whether `needle` appears in any user-visible field. Case-insensitive, so
    /// picker filtering matches what is on screen.
    pub fn matches(&self, needle: &str) -> bool {
        let n = needle.to_ascii_lowercase();
        self.label.to_ascii_lowercase().contains(&n)
            || self.host.to_ascii_lowercase().contains(&n)
            || self
                .user
                .as_deref()
                .is_some_and(|u| u.to_ascii_lowercase().contains(&n))
            || self
                .path
                .as_deref()
                .is_some_and(|p| p.to_ascii_lowercase().contains(&n))
    }
}

/// The on-disk shape. A wrapper struct because TOML needs a named array.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CatalogFile {
    #[serde(default, rename = "host")]
    hosts: Vec<SavedHost>,
}

/// The saved-host list, backed by a TOML file.
#[derive(Debug, Default, Clone)]
pub struct HostCatalog {
    hosts: Vec<SavedHost>,
    path: Option<PathBuf>,
}

/// Where the catalog lives.
pub fn default_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("myd").join("hosts.toml"))
}

impl HostCatalog {
    /// Load the catalog, or an empty one.
    ///
    /// A missing, unreadable, or malformed file yields an empty catalog rather
    /// than an error: the dialing directory is a convenience, and failing to
    /// start over a stray character in a config file would be a poor trade. A
    /// malformed file is left alone rather than overwritten, so a typo can be
    /// fixed by hand instead of silently costing every saved host.
    pub fn load() -> Self {
        let Some(path) = default_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let hosts = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| match toml::from_str::<CatalogFile>(&s) {
                Ok(f) => Some(f.hosts),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring malformed host catalog");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            hosts,
            path: Some(path.to_path_buf()),
        }
    }

    /// An in-memory catalog that never persists. For tests.
    pub fn in_memory(hosts: Vec<SavedHost>) -> Self {
        Self { hosts, path: None }
    }

    /// Write the catalog out.
    ///
    /// Writes to a temporary file and renames, so an interrupted save cannot
    /// leave a half-written catalog where the real one was.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(&CatalogFile {
            hosts: self.hosts.clone(),
        })
        .context("could not serialise the host catalog")?;

        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    pub fn hosts(&self) -> &[SavedHost] {
        &self.hosts
    }

    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    /// The `n` most recently connected hosts, newest first.
    ///
    /// This is what `gr` shows without asking for the full list — the whole
    /// point of the catalog is that the usual destinations need no searching.
    ///
    /// Strictly least-recently-used. Ranking by *frequency* first (as this once
    /// did) meant a host connected fifty times last month outranked one used an
    /// hour ago, and the order barely moved as you worked. Recency changes
    /// visibly with use, so the list stays predictable: whatever you just
    /// connected to is at the top.
    pub fn recent(&self, n: usize) -> Vec<&SavedHost> {
        let mut refs: Vec<&SavedHost> = self.hosts.iter().collect();
        refs.sort_by(|a, b| {
            // RFC 3339 with a fixed offset sorts correctly as a string. A host
            // that has never been connected to has no timestamp; treating that
            // as an empty string sinks it below every dated entry, whereas
            // comparing the `Option`s directly would sort `None` *first* and put
            // unused hosts at the top of a "recent" list.
            let (ak, bk) = (
                a.last_used.as_deref().unwrap_or(""),
                b.last_used.as_deref().unwrap_or(""),
            );
            bk.cmp(ak).then_with(|| a.label.cmp(&b.label))
        });
        refs.into_iter().take(n).collect()
    }

    pub fn find(&self, label: &str) -> Option<&SavedHost> {
        self.hosts.iter().find(|h| h.label == label)
    }

    /// Add a host, or replace one with the same label.
    pub fn upsert(&mut self, host: SavedHost) {
        match self.hosts.iter_mut().find(|h| h.label == host.label) {
            Some(existing) => {
                // Editing must not reset the ranking.
                let uses = existing.uses;
                let last = existing.last_used.clone();
                *existing = host;
                existing.uses = uses;
                existing.last_used = last;
            }
            None => self.hosts.push(host),
        }
    }

    pub fn remove(&mut self, label: &str) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| h.label != label);
        self.hosts.len() != before
    }

    /// Note a successful connection, promoting the host in the ranking.
    pub fn record_use(&mut self, label: &str) {
        if let Some(h) = self.hosts.iter_mut().find(|h| h.label == label) {
            h.uses += 1;
            h.last_used = Some(chrono::Utc::now().to_rfc3339());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<SavedHost> {
        vec![
            SavedHost {
                label: "prod".into(),
                user: Some("juan".into()),
                host: "prod.example.com".into(),
                port: Some(2222),
                path: Some("/srv/app".into()),
                uses: 10,
                last_used: Some("2026-07-20T00:00:00Z".into()),
            },
            SavedHost {
                label: "backup".into(),
                user: Some("root".into()),
                host: "10.0.0.5".into(),
                port: None,
                path: None,
                uses: 3,
                last_used: None,
            },
            SavedHost::new("scratch", "dev.local"),
        ]
    }

    #[test]
    fn url_round_trips_through_the_parser() {
        for h in sample() {
            let url = h.to_url();
            let parsed = SftpTarget::parse(&url)
                .unwrap_or_else(|e| panic!("{} did not parse: {}", url, e));
            assert_eq!(parsed.host, h.host, "host lost in {}", url);
            assert_eq!(parsed.user, h.user, "user lost in {}", url);
            assert_eq!(parsed.port, h.port, "port lost in {}", url);
        }
    }

    /// Recency beats frequency: whatever was connected to last is at the top.
    #[test]
    fn recent_ranks_by_recency_not_frequency() {
        let hosts = vec![
            SavedHost {
                label: "workhorse".into(),
                host: "a.example.com".into(),
                uses: 50,
                last_used: Some("2026-06-01T00:00:00Z".into()),
                ..Default::default()
            },
            SavedHost {
                label: "yesterday".into(),
                host: "b.example.com".into(),
                uses: 1,
                last_used: Some("2026-07-25T00:00:00Z".into()),
                ..Default::default()
            },
        ];
        let c = HostCatalog::in_memory(hosts);
        assert_eq!(
            c.recent(1)[0].label,
            "yesterday",
            "a host used once yesterday must outrank one used 50 times last month"
        );
    }

    /// A host that has never been connected to sorts below every dated one —
    /// `Option` ordering would otherwise put `None` first.
    #[test]
    fn never_connected_hosts_sort_last() {
        let hosts = vec![
            SavedHost::new("never", "new.example.com"),
            SavedHost {
                label: "used".into(),
                host: "old.example.com".into(),
                uses: 1,
                last_used: Some("2026-01-01T00:00:00Z".into()),
                ..Default::default()
            },
        ];
        let c = HostCatalog::in_memory(hosts);
        let order: Vec<&str> = c.recent(10).iter().map(|h| h.label.as_str()).collect();
        assert_eq!(order, vec!["used", "never"]);
    }

    #[test]
    fn recent_returns_at_most_n_and_is_stable() {
        let c = HostCatalog::in_memory(sample());
        assert_eq!(c.recent(2).len(), 2);
        assert_eq!(c.recent(10).len(), 3);
        // Two hosts with no timestamp fall back to label order, so the list does
        // not shuffle between renders.
        let a = c.recent(10);
        let b = c.recent(10);
        let names_a: Vec<&str> = a.iter().map(|h| h.label.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|h| h.label.as_str()).collect();
        assert_eq!(names_a, names_b);
    }

    #[test]
    fn record_use_promotes_a_host() {
        let mut c = HostCatalog::in_memory(sample());
        for _ in 0..20 {
            c.record_use("scratch");
        }
        assert_eq!(c.recent(1)[0].label, "scratch");
        assert!(c.find("scratch").unwrap().last_used.is_some());
    }

    #[test]
    fn upsert_replaces_but_keeps_the_ranking() {
        let mut c = HostCatalog::in_memory(sample());
        let mut edited = SavedHost::new("prod", "new.example.com");
        edited.uses = 0;
        c.upsert(edited);

        let h = c.find("prod").unwrap();
        assert_eq!(h.host, "new.example.com");
        // Editing an entry must not demote it to the bottom of the list.
        assert_eq!(h.uses, 10, "edit reset the use count");
        assert_eq!(c.len(), 3, "upsert should replace, not append");
    }

    #[test]
    fn round_trips_through_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut c = HostCatalog::load_from(&path);
        assert!(c.is_empty(), "a missing file should load as empty");
        for h in sample() {
            c.upsert(h);
        }
        c.save().unwrap();

        let reloaded = HostCatalog::load_from(&path);
        assert_eq!(reloaded.len(), 3);
        assert_eq!(reloaded.find("prod").unwrap().port, Some(2222));
        assert_eq!(reloaded.find("scratch").unwrap().host, "dev.local");
    }

    #[test]
    fn a_malformed_file_loads_empty_and_is_not_destroyed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(&path, "this is not valid toml {{{").unwrap();

        let c = HostCatalog::load_from(&path);
        assert!(c.is_empty(), "malformed file must not panic or half-load");

        // The bad file is left for the user to fix rather than overwritten.
        assert!(std::fs::read_to_string(&path).unwrap().contains("not valid"));
    }

    #[test]
    fn no_password_field_is_ever_serialised() {
        let c = HostCatalog::in_memory(sample());
        let body = toml::to_string_pretty(&CatalogFile {
            hosts: c.hosts().to_vec(),
        })
        .unwrap();
        let lower = body.to_lowercase();
        assert!(!lower.contains("password"), "catalog must never hold secrets");
        assert!(!lower.contains("passphrase"));
    }

    #[test]
    fn matches_searches_every_visible_field() {
        let h = &sample()[0];
        assert!(h.matches("PROD"));
        assert!(h.matches("example"));
        assert!(h.matches("juan"));
        assert!(h.matches("srv"));
        assert!(!h.matches("nonsense"));
    }

    #[test]
    fn remove_reports_whether_anything_went() {
        let mut c = HostCatalog::in_memory(sample());
        assert!(c.remove("backup"));
        assert!(!c.remove("backup"));
        assert_eq!(c.len(), 2);
    }
}

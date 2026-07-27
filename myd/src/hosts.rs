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
    // Empty arrays are omitted rather than written as `host = []`, which is
    // noise in a file the user is invited to edit by hand.
    #[serde(default, rename = "host", skip_serializing_if = "Vec::is_empty")]
    hosts: Vec<SavedHost>,
    /// Saved local directories for the `gd` picker. A separate array from
    /// `host`, so the two kinds of entry stay legible in a hand-edited file.
    #[serde(default, rename = "favorite", skip_serializing_if = "Vec::is_empty")]
    favorites: Vec<SavedDir>,
}

/// One saved local directory, for the `gd` picker's shortcut list.
///
/// Deliberately not a [`SavedHost`] with an empty `host`: a place on this machine
/// and a remote to dial are different things, and a reader of the config file
/// should not have to infer which one an entry is from a missing field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedDir {
    /// Absolute path to the directory. The identity of the entry.
    pub path: String,
    /// Optional display name; the path is shown when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Times opened. Shown in the picker; does not affect ordering.
    #[serde(default)]
    pub uses: u64,
    /// RFC 3339 timestamp of the last visit — what the list is ordered by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
    /// Whether the user asked for this entry (`a`) rather than it being recorded
    /// automatically from a typed path.
    ///
    /// History is convenience — it should not accumulate forever, and it is
    /// trimmed to the most recent [`MAX_HISTORY`]. An explicitly saved favourite
    /// is a decision and is never trimmed, so the two have to be told apart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
}

/// How many automatically-recorded directories to keep.
///
/// Enough that somewhere visited a few sessions ago is still one keystroke away,
/// small enough that the list stays scannable and the config file stays short.
pub const MAX_HISTORY: usize = 20;

impl SavedDir {
    /// An automatically recorded visit.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            ..Default::default()
        }
    }

    /// A directory the user explicitly asked to keep.
    pub fn pinned(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            pinned: true,
            ..Default::default()
        }
    }

    /// What the picker shows for this entry.
    pub fn display(&self) -> &str {
        match &self.label {
            Some(l) if !l.is_empty() => l,
            _ => &self.path,
        }
    }
}

/// The saved-host list, backed by a TOML file.
#[derive(Debug, Default, Clone)]
pub struct HostCatalog {
    hosts: Vec<SavedHost>,
    favorites: Vec<SavedDir>,
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
        let file = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| match toml::from_str::<CatalogFile>(&s) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring malformed host catalog");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            hosts: file.hosts,
            favorites: file.favorites,
            path: Some(path.to_path_buf()),
        }
    }

    /// An in-memory catalog that never persists. For tests.
    pub fn in_memory(hosts: Vec<SavedHost>) -> Self {
        Self {
            hosts,
            favorites: Vec::new(),
            path: None,
        }
    }

    /// An in-memory catalog of saved directories. For tests.
    pub fn in_memory_dirs(favorites: Vec<SavedDir>) -> Self {
        Self {
            hosts: Vec::new(),
            favorites,
            path: None,
        }
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
        // Both arrays are written every time. Serialising only one of them would
        // silently drop the other from the file on the next save.
        let body = toml::to_string_pretty(&CatalogFile {
            hosts: self.hosts.clone(),
            favorites: self.favorites.clone(),
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

    /// Every saved directory, in file order.
    pub fn favorites(&self) -> &[SavedDir] {
        &self.favorites
    }

    /// Saved directories, most recently visited first.
    ///
    /// Ordered exactly as [`Self::recent`] orders hosts, including sinking
    /// never-visited entries below dated ones rather than letting `None` sort
    /// first and head a "recent" list.
    pub fn favorites_by_recency(&self) -> Vec<&SavedDir> {
        let mut refs: Vec<&SavedDir> = self.favorites.iter().collect();
        refs.sort_by(|a, b| {
            let (ak, bk) = (
                a.last_used.as_deref().unwrap_or(""),
                b.last_used.as_deref().unwrap_or(""),
            );
            bk.cmp(ak).then_with(|| a.path.cmp(&b.path))
        });
        refs
    }

    /// Whether `path` is already saved.
    pub fn is_favorite(&self, path: &str) -> bool {
        self.favorites.iter().any(|f| f.path == path)
    }

    /// Save a directory. Returns false when it was already there, so the caller
    /// can say so rather than silently doing nothing.
    ///
    /// Pinning an entry that history already recorded is not a duplicate — it
    /// promotes that entry, keeping its visit count.
    pub fn add_favorite(&mut self, dir: SavedDir) -> bool {
        // Matched canonically: history records the path the picker resolved, so
        // pinning the same place typed a different way (a symlinked route, or
        // macOS's `/tmp` vs `/private/tmp`) must promote that entry rather than
        // add a second one for the same directory.
        let pin = dir.pinned;
        if let Some(existing) = self.find_dir_mut(&dir.path) {
            if pin && !existing.pinned {
                existing.pinned = true;
                return true;
            }
            return false;
        }
        self.favorites.push(dir);
        true
    }

    /// Record a directory the user opened by typing its path.
    ///
    /// Creates the entry when it is new, so the places you actually go
    /// accumulate without having to be saved by hand, and promotes it when it is
    /// not. Automatically recorded entries are then trimmed to [`MAX_HISTORY`];
    /// entries the user pinned with `a` are never trimmed.
    pub fn record_visit(&mut self, path: &str) {
        if let Some(f) = self.find_dir_mut(path) {
            f.uses += 1;
            f.last_used = Some(chrono::Utc::now().to_rfc3339());
        } else {
            self.favorites.push(SavedDir {
                path: path.to_string(),
                uses: 1,
                last_used: Some(chrono::Utc::now().to_rfc3339()),
                ..Default::default()
            });
        }
        self.trim_history();
    }

    /// Drop the oldest automatically-recorded entries beyond [`MAX_HISTORY`].
    fn trim_history(&mut self) {
        let mut unpinned: Vec<(String, String)> = self
            .favorites
            .iter()
            .filter(|f| !f.pinned)
            .map(|f| {
                (
                    f.path.clone(),
                    f.last_used.clone().unwrap_or_default(),
                )
            })
            .collect();
        if unpinned.len() <= MAX_HISTORY {
            return;
        }
        // Newest first, then drop everything past the cap.
        unpinned.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let doomed: std::collections::HashSet<String> = unpinned
            .into_iter()
            .skip(MAX_HISTORY)
            .map(|(p, _)| p)
            .collect();
        self.favorites
            .retain(|f| f.pinned || !doomed.contains(&f.path));
    }

    /// The entry for `path`, matching canonical forms as well as literal ones.
    fn find_dir_mut(&mut self, path: &str) -> Option<&mut SavedDir> {
        let canonical = std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        self.favorites.iter_mut().find(|f| {
            f.path == path
                || canonical.as_deref() == Some(f.path.as_str())
                || std::fs::canonicalize(&f.path)
                    .ok()
                    .map(|p| p.to_string_lossy().to_string())
                    .as_deref()
                    == Some(path)
        })
    }

    /// Forget a saved directory. Returns whether anything was removed.
    pub fn remove_favorite(&mut self, path: &str) -> bool {
        let before = self.favorites.len();
        self.favorites.retain(|f| f.path != path);
        self.favorites.len() != before
    }

    /// Note a visit, promoting the directory in the ranking.
    ///
    /// Only touches paths that are already saved: visiting an arbitrary
    /// directory should not silently add it to the favourites list.
    ///
    /// Matches on the canonical form as well as the literal one. The picker
    /// resolves what the user typed before opening it, so a favourite saved as
    /// `/tmp/work` would otherwise never match the `/private/tmp/work` that
    /// comes back on macOS — or the resolved target of any symlinked path.
    pub fn record_dir_use(&mut self, path: &str) {
        if let Some(f) = self.find_dir_mut(path) {
            f.uses += 1;
            f.last_used = Some(chrono::Utc::now().to_rfc3339());
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
            favorites: Vec::new(),
        })
        .unwrap();
        let lower = body.to_lowercase();
        assert!(!lower.contains("password"), "catalog must never hold secrets");
        assert!(!lower.contains("passphrase"));
    }

    #[test]
    fn favorites_and_hosts_share_one_file_without_clobbering_each_other() {
        // Both arrays are serialised on every save. Writing only one of them
        // would drop the other from the file the next time anything changed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");

        let mut c = HostCatalog::load_from(&path);
        c.upsert(SavedHost::new("prod", "prod.example.com"));
        assert!(c.add_favorite(SavedDir::new("/home/juan/code")));
        c.save().unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[[host]]"), "hosts section missing: {}", body);
        assert!(body.contains("[[favorite]]"), "favourites section missing: {}", body);

        // Round-trip, change only the host side, and save again.
        let mut back = HostCatalog::load_from(&path);
        assert_eq!(back.hosts().len(), 1);
        assert_eq!(back.favorites().len(), 1);
        back.record_use("prod");
        back.save().unwrap();

        let after = HostCatalog::load_from(&path);
        assert_eq!(
            after.favorites().len(),
            1,
            "saving a host change must not drop favourites"
        );
        assert_eq!(after.favorites()[0].path, "/home/juan/code");
    }

    #[test]
    fn a_host_only_file_still_loads() {
        // Existing configs have no [[favorite]] array at all.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        std::fs::write(
            &path,
            "[[host]]\nlabel = \"prod\"\nhost = \"prod.example.com\"\n",
        )
        .unwrap();
        let c = HostCatalog::load_from(&path);
        assert_eq!(c.hosts().len(), 1);
        assert!(c.favorites().is_empty());
    }

    #[test]
    fn favorites_rank_by_recency_not_frequency() {
        let c = HostCatalog::in_memory_dirs(vec![
            SavedDir {
                path: "/often".into(),
                uses: 50,
                last_used: Some("2026-06-01T00:00:00Z".into()),
                ..Default::default()
            },
            SavedDir {
                path: "/yesterday".into(),
                uses: 1,
                last_used: Some("2026-07-26T00:00:00Z".into()),
                ..Default::default()
            },
            SavedDir {
                path: "/never".into(),
                ..Default::default()
            },
        ]);
        let order: Vec<&str> = c
            .favorites_by_recency()
            .iter()
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(
            order,
            vec!["/yesterday", "/often", "/never"],
            "recency wins over use count, and never-visited sinks last"
        );
    }

    #[test]
    fn pinning_an_existing_history_entry_promotes_it() {
        // `a` on somewhere history already recorded should promote that entry,
        // keeping its visit count, rather than adding a second one.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_string_lossy().to_string();
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        c.record_visit(&p);
        assert_eq!(c.favorites().len(), 1);
        assert!(!c.favorites()[0].pinned, "a visit is history, not a favourite");

        assert!(c.add_favorite(SavedDir::pinned(p)), "pinning reports a change");
        assert_eq!(c.favorites().len(), 1, "and does not duplicate");
        assert!(c.favorites()[0].pinned);
        assert_eq!(c.favorites()[0].uses, 1, "history is kept");
    }

    #[test]
    fn history_is_capped_but_pinned_entries_are_never_trimmed() {
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        c.add_favorite(SavedDir::pinned("/pinned"));
        // More automatic entries than the cap allows.
        for i in 0..(MAX_HISTORY + 8) {
            c.record_visit(&format!("/auto/{:03}", i));
        }
        let unpinned = c.favorites().iter().filter(|f| !f.pinned).count();
        assert_eq!(unpinned, MAX_HISTORY, "history is trimmed to the cap");
        assert!(
            c.favorites().iter().any(|f| f.path == "/pinned"),
            "an explicitly saved entry survives trimming"
        );
        // The oldest automatic entries are the ones dropped.
        assert!(
            !c.favorites().iter().any(|f| f.path == "/auto/000"),
            "the oldest history entry should have been trimmed"
        );
    }

    #[test]
    fn adding_a_duplicate_favorite_is_refused() {
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        assert!(c.add_favorite(SavedDir::new("/a")));
        assert!(!c.add_favorite(SavedDir::new("/a")), "no duplicates");
        assert_eq!(c.favorites().len(), 1);
        assert!(c.remove_favorite("/a"));
        assert!(!c.remove_favorite("/a"), "removing twice reports nothing done");
    }

    #[test]
    fn recording_a_visit_only_touches_saved_directories() {
        let mut c = HostCatalog::in_memory_dirs(vec![SavedDir::new("/saved")]);
        c.record_dir_use("/not-saved");
        assert_eq!(c.favorites().len(), 1, "visiting must not add entries");
        c.record_dir_use("/saved");
        assert_eq!(c.favorites()[0].uses, 1);
        assert!(c.favorites()[0].last_used.is_some());
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

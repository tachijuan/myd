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
                if path.starts_with('/') {
                    // The root is written '//', since a lone trailing slash
                    // parses as "no path given" and would move a host pinned to
                    // the root into the home directory on the next open.
                    if path == "/" {
                        s.push('/');
                    }
                    s.push_str(path);
                } else {
                    // A relative path keeps the scp-style colon: it means "under
                    // the login directory", and writing it as '/path' made it
                    // absolute — so a host saved as `host:c` came back as
                    // `sftp://host/c` and opened /c instead of ~/c.
                    s.push(':');
                    s.push_str(path);
                }
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
    /// trimmed to the most recent [`MAX_HISTORY`]. A saved entry is a decision
    /// and is never trimmed, so the two have to be told apart.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub saved: bool,
    /// Browse this directory without measuring its subdirectories.
    ///
    /// Remembered per directory: somewhere you have decided is not worth walking
    /// — a huge archive, a network mount — stays that way next time you open it,
    /// rather than making you toggle again on arrival.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub shallow: bool,
    /// Position in the pinned block, which sits above everything else in an
    /// order the user controls rather than one recency decides.
    ///
    /// `None` for the great majority of entries. Stored as an explicit rank so
    /// the order survives a reload; the ranks are renumbered on every change, so
    /// they stay dense and a hand-edited file with gaps or duplicates still
    /// produces a sensible order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin_rank: Option<u32>,
}

impl SavedDir {
    /// Whether this entry sits in the pinned block.
    pub fn is_pinned(&self) -> bool {
        self.pin_rank.is_some()
    }

    /// Which tier this entry belongs to, for display and ordering.
    pub fn tier(&self) -> DirTier {
        if self.is_pinned() {
            DirTier::Pinned
        } else if self.saved {
            DirTier::Saved
        } else {
            DirTier::Recent
        }
    }
}

/// The three groups the picker shows, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DirTier {
    /// Pinned to the top, in the user's own order.
    Pinned,
    /// Explicitly saved, never trimmed, ordered by recency.
    Saved,
    /// Automatically remembered, trimmed to [`MAX_HISTORY`].
    Recent,
}

/// The directories a fresh catalog starts with.
///
/// Seeded into the file rather than merged in at render time, so they are
/// ordinary entries: pinnable, reorderable, and deletable like anything else.
/// They used to be a separate hardcoded list, which meant `p` and `d` silently
/// did nothing on them — the list looked uniform and did not behave that way.
///
/// The working directory is deliberately absent: it differs per launch, so an
/// entry naming one particular directory would be wrong most of the time, and
/// the app already starts there.
pub fn seed_dirs() -> Vec<SavedDir> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    let mut push = |path: PathBuf, label: &str| {
        if path.is_dir() {
            out.push(SavedDir {
                path: path.to_string_lossy().to_string(),
                label: Some(label.to_string()),
                saved: true,
                ..Default::default()
            });
        }
    };
    if let Some(home) = home {
        push(home.clone(), "~ (Home)");
        for name in ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Videos"] {
            push(home.join(name), name);
        }
    }
    push(PathBuf::from("/"), "/ (Root)");
    push(PathBuf::from("/tmp"), "/tmp");
    out
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
    pub fn saved(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            saved: true,
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
        let raw = std::fs::read_to_string(path).ok();
        // A file that exists but does not parse is left strictly alone: the user
        // has something in there worth fixing, and seeding over it would destroy
        // the very content the warning is telling them to repair.
        let mut malformed = false;
        let file = raw
            .and_then(|s| match toml::from_str::<CatalogFile>(&s) {
                Ok(f) => Some(f),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "ignoring malformed host catalog");
                    malformed = true;
                    None
                }
            })
            .unwrap_or_default();
        let seeded = !malformed && file.favorites.is_empty();
        let favorites = if seeded {
            // A catalog with no directories yet — a first run, or a file that
            // only ever held hosts. Seed the standard locations so every row in
            // the picker is a real entry the user can pin, reorder or delete.
            seed_dirs()
        } else {
            file.favorites
        };
        let catalog = Self {
            hosts: file.hosts,
            favorites,
            path: Some(path.to_path_buf()),
        };
        if seeded && !catalog.favorites.is_empty() {
            // Write them out now, so the file the user edits matches the list
            // they see rather than materialising only after some later change.
            if let Err(e) = catalog.save() {
                tracing::warn!(path = %path.display(), error = %e, "could not seed the catalog");
            }
        }
        catalog
    }

    /// An in-memory catalog that never persists. For tests.
    pub fn in_memory(hosts: Vec<SavedHost>) -> Self {
        Self {
            hosts,
            favorites: Vec::new(),
            path: None,
        }
    }

    /// Load without seeding the standard locations.
    ///
    /// For tests that assert on the exact contents of a catalog and would
    /// otherwise have to account for a dozen entries they did not put there.
    pub fn load_from_unseeded(path: &std::path::Path) -> Self {
        // Read the file directly: `load_from` would seed an absent or
        // directory-less one, which is the behaviour this bypasses.
        let file = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| toml::from_str::<CatalogFile>(&s).ok())
            .unwrap_or_default();
        Self {
            hosts: file.hosts,
            favorites: file.favorites,
            path: Some(path.to_path_buf()),
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
    /// The picker lists these after the directories — the whole point of the
    /// catalog is that the usual destinations need no searching.
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
        let want_saved = dir.saved;
        if let Some(existing) = self.find_dir_mut(&dir.path) {
            if want_saved && !existing.saved {
                existing.saved = true;
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

    /// Entries in the pinned block, in the user's chosen order.
    pub fn pinned_dirs(&self) -> Vec<&SavedDir> {
        let mut v: Vec<&SavedDir> = self.favorites.iter().filter(|f| f.is_pinned()).collect();
        v.sort_by_key(|f| f.pin_rank.unwrap_or(u32::MAX));
        v
    }

    /// Pin `path` to the bottom of the pinned block.
    ///
    /// A new pin goes below the existing ones rather than on top: the block is
    /// an order the user arranged, and inserting into the middle of it uninvited
    /// would disturb that. Returns false when it is already pinned or unknown.
    pub fn pin_dir(&mut self, path: &str) -> bool {
        let next = self
            .favorites
            .iter()
            .filter_map(|f| f.pin_rank)
            .max()
            .map(|r| r + 1)
            .unwrap_or(0);
        let Some(entry) = self.find_dir_mut(path) else {
            return false;
        };
        if entry.is_pinned() {
            return false;
        }
        entry.pin_rank = Some(next);
        // Pinning is at least as deliberate as saving, so it implies it — an
        // entry the user arranged by hand must not be trimmed as history.
        entry.saved = true;
        self.renumber_pins();
        true
    }

    /// Remove `path` from the pinned block, leaving it as a saved entry.
    pub fn unpin_dir(&mut self, path: &str) -> bool {
        let Some(entry) = self.find_dir_mut(path) else {
            return false;
        };
        if !entry.is_pinned() {
            return false;
        }
        entry.pin_rank = None;
        self.renumber_pins();
        true
    }

    /// Move a pinned entry to `index` within the pinned block.
    ///
    /// Returns false when the path is not pinned. An index past the end lands it
    /// last rather than failing, so a caller can clamp loosely.
    pub fn move_pin_to(&mut self, path: &str, index: usize) -> bool {
        let mut order: Vec<String> = self.pinned_dirs().iter().map(|f| f.path.clone()).collect();
        let Some(from) = order.iter().position(|p| p == path) else {
            return false;
        };
        let entry = order.remove(from);
        let to = index.min(order.len());
        order.insert(to, entry);
        self.apply_pin_order(&order);
        true
    }

    /// Set the pinned block's order from a list of paths.
    pub fn apply_pin_order(&mut self, order: &[String]) {
        for (rank, path) in order.iter().enumerate() {
            if let Some(f) = self.favorites.iter_mut().find(|f| &f.path == path) {
                f.pin_rank = Some(rank as u32);
            }
        }
        self.renumber_pins();
    }

    /// Renumber pin ranks to 0..n in their current order.
    ///
    /// Keeps the ranks dense after any change, so a hand-edited file with gaps
    /// or duplicate ranks still yields a stable, sensible order.
    fn renumber_pins(&mut self) {
        let order: Vec<String> = self.pinned_dirs().iter().map(|f| f.path.clone()).collect();
        for (rank, path) in order.iter().enumerate() {
            if let Some(f) = self.favorites.iter_mut().find(|f| &f.path == path) {
                f.pin_rank = Some(rank as u32);
            }
        }
    }

    /// Drop the oldest automatically-recorded entries beyond [`MAX_HISTORY`].
    fn trim_history(&mut self) {
        let mut unpinned: Vec<(String, String)> = self
            .favorites
            .iter()
            .filter(|f| !f.saved && !f.is_pinned())
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
            .retain(|f| f.saved || f.is_pinned() || !doomed.contains(&f.path));
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

    /// Remember whether this directory should be browsed without measuring.
    ///
    /// Creates the entry when it is new, so choosing shallow somewhere you have
    /// not saved still sticks — the same reasoning as recording a visit.
    pub fn set_dir_shallow(&mut self, path: &str, shallow: bool) {
        match self.find_dir_mut(path) {
            Some(entry) => entry.shallow = shallow,
            None => self.favorites.push(SavedDir {
                path: path.to_string(),
                shallow,
                ..Default::default()
            }),
        }
    }

    /// Whether this directory was last browsed without measuring.
    pub fn dir_is_shallow(&self, path: &str) -> bool {
        self.dir_shallow_pref(path).unwrap_or(false)
    }

    /// This directory's remembered traversal mode, or `None` when it has never
    /// been recorded.
    ///
    /// The distinction matters on arrival: a directory with no entry should
    /// inherit the mode the session is already browsing in, whereas one recorded
    /// as measured should stay measured even inside a shallow session. Folding
    /// both into `false` is what made `-s` evaporate on the first `Enter`.
    pub fn dir_shallow_pref(&self, path: &str) -> Option<bool> {
        let canonical = std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().to_string());
        self.favorites
            .iter()
            .find(|f| f.path == path || canonical.as_deref() == Some(f.path.as_str()))
            .map(|f| f.shallow)
    }

    /// Change a saved directory's path, keeping everything else about it.
    ///
    /// The visit count, timestamp and position in the pinned block are the
    /// entry's history; a correction to the path should not throw them away, as
    /// deleting and re-adding would.
    ///
    /// Returns an error message when the new path is already saved — merging the
    /// two entries silently would lose whichever history the user cared about.
    pub fn rename_favorite(&mut self, from: &str, to: &str) -> Option<String> {
        if from == to {
            return None;
        }
        if self.favorites.iter().any(|f| f.path == to) {
            return Some(format!("'{}' is already in the list.", to));
        }
        match self.favorites.iter_mut().find(|f| f.path == from) {
            Some(entry) => {
                entry.path = to.to_string();
                // A label naming the old path would now be wrong; one the user
                // chose is theirs to keep.
                if entry.label.as_deref() == Some(from) {
                    entry.label = None;
                }
                None
            }
            None => Some(format!("'{}' is no longer in the list.", from)),
        }
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

    /// `sample()` plus an scp-style relative path.
    ///
    /// Kept out of `sample()` itself because several tests there assert on its
    /// length, and a URL round trip is not what those are about.
    fn sample_with_relative_path() -> Vec<SavedHost> {
        let mut v = sample();
        v.push(SavedHost {
            label: "home-rel".into(),
            user: Some("juan".into()),
            host: "rel.example.com".into(),
            port: None,
            path: Some("work/notes".into()),
            uses: 1,
            last_used: None,
        });
        v
    }

    #[test]
    fn url_round_trips_through_the_parser() {
        for h in sample_with_relative_path() {
            let url = h.to_url();
            let parsed = SftpTarget::parse(&url)
                .unwrap_or_else(|e| panic!("{} did not parse: {}", url, e));
            assert_eq!(parsed.host, h.host, "host lost in {}", url);
            assert_eq!(parsed.user, h.user, "user lost in {}", url);
            assert_eq!(parsed.port, h.port, "port lost in {}", url);
            // The path was omitted here once, which is how a saved relative path
            // could be rewritten to an absolute one unnoticed.
            assert_eq!(
                parsed.path.map(|p| p.to_string_lossy().to_string()),
                h.path,
                "path lost in {}",
                url
            );
        }
    }

    /// A saved relative path must not be rewritten as absolute.
    ///
    /// `to_url` wrote every path with a leading '/', so a host saved as
    /// `host:c` was re-displayed and reopened as `sftp://host/c` — the starting
    /// directory silently moved from ~/c to /c. This is a second copy of the
    /// conversion in `SftpTarget::to_url`; fixing that one alone left saved
    /// hosts, which round-trip through here, still broken.
    #[test]
    fn a_relative_path_keeps_its_colon() {
        for (url, want_path) in [
            ("sftp://user@remote.com:c", "c"),
            ("sftp://user@remote.com:work/notes", "work/notes"),
            ("sftp://user@remote.com:2222:c", "c"),
        ] {
            let h = SavedHost::from_url("lab", url).unwrap();
            assert_eq!(h.path.as_deref(), Some(want_path), "{} stored wrongly", url);

            // Re-emitting it must not turn the path absolute.
            let out = h.to_url();
            assert!(
                !out.contains(&format!("/{}", want_path)),
                "{} became absolute as {}",
                url,
                out
            );

            // And it must survive a full save/reopen cycle unchanged.
            let back = SavedHost::from_url("lab", &out).unwrap();
            assert_eq!(back, h, "{} did not round-trip (via {})", url, out);

            // The connect path reads the same URL, so it must agree.
            let t = SftpTarget::parse(&out).unwrap();
            assert_eq!(
                t.path.map(|p| p.to_string_lossy().to_string()).as_deref(),
                Some(want_path),
                "connecting to {} would use the wrong directory",
                out
            );
        }
    }

    /// An absolute saved path stays absolute, including the root.
    #[test]
    fn an_absolute_path_stays_absolute() {
        for (url, want_path) in [
            ("sftp://user@remote.com:/abs/c", "/abs/c"),
            ("sftp://user@remote.com/abs/c", "/abs/c"),
            ("sftp://h//", "/"),
        ] {
            let h = SavedHost::from_url("lab", url).unwrap();
            assert_eq!(h.path.as_deref(), Some(want_path), "{} stored wrongly", url);
            let back = SavedHost::from_url("lab", &h.to_url()).unwrap();
            assert_eq!(back, h, "{} did not round-trip (via {})", url, h.to_url());
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
        // A fresh catalog is seeded with the standard locations; this test is
        // about the two sections coexisting, so measure against that baseline
        // rather than assuming an empty list.
        let seeded = c.favorites().len();
        c.upsert(SavedHost::new("prod", "prod.example.com"));
        assert!(c.add_favorite(SavedDir::new("/home/juan/code")));
        c.save().unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[[host]]"), "hosts section missing: {}", body);
        assert!(body.contains("[[favorite]]"), "favourites section missing: {}", body);

        // Round-trip, change only the host side, and save again.
        let mut back = HostCatalog::load_from(&path);
        assert_eq!(back.hosts().len(), 1);
        assert_eq!(
            back.favorites().len(),
            seeded + 1,
            "a file that already has directories is not re-seeded"
        );
        back.record_use("prod");
        back.save().unwrap();

        let after = HostCatalog::load_from(&path);
        assert_eq!(
            after.favorites().len(),
            seeded + 1,
            "saving a host change must not drop favourites"
        );
        assert!(
            after.favorites().iter().any(|f| f.path == "/home/juan/code"),
            "the added directory survives"
        );
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
        // A file with no directory entries gets the standard locations seeded,
        // so the picker's rows are all real entries the user can act on.
        assert!(
            !c.favorites().is_empty(),
            "a host-only file should be seeded with the standard directories"
        );
        assert!(
            c.favorites().iter().all(|f| f.saved),
            "seeded entries are saved, so history trimming never removes them"
        );
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
    fn a_new_pin_goes_below_the_existing_ones() {
        // The block is an order the user arranged; a new pin must not barge into
        // the middle of it.
        let mut c = HostCatalog::in_memory_dirs(vec![
            SavedDir::saved("/a"),
            SavedDir::saved("/b"),
            SavedDir::saved("/c"),
        ]);
        assert!(c.pin_dir("/a"));
        assert!(c.pin_dir("/b"));
        assert!(c.pin_dir("/c"));
        let order: Vec<&str> = c.pinned_dirs().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, vec!["/a", "/b", "/c"]);
        assert!(!c.pin_dir("/a"), "pinning twice reports nothing done");
    }

    #[test]
    fn pinning_implies_saving_so_it_is_never_trimmed() {
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        c.record_visit("/history");
        assert!(!c.favorites()[0].saved);
        assert!(c.pin_dir("/history"));
        assert!(
            c.favorites()[0].saved,
            "an entry arranged by hand must survive history trimming"
        );
    }

    #[test]
    fn moving_a_pin_reorders_within_the_block() {
        let mut c = HostCatalog::in_memory_dirs(vec![
            SavedDir::saved("/a"),
            SavedDir::saved("/b"),
            SavedDir::saved("/c"),
        ]);
        for p in ["/a", "/b", "/c"] {
            c.pin_dir(p);
        }
        // Send the first entry to the end.
        assert!(c.move_pin_to("/a", 2));
        let order: Vec<&str> = c.pinned_dirs().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, vec!["/b", "/c", "/a"]);

        // And back to the top.
        assert!(c.move_pin_to("/a", 0));
        let order: Vec<&str> = c.pinned_dirs().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, vec!["/a", "/b", "/c"]);

        // An index past the end clamps rather than failing.
        assert!(c.move_pin_to("/a", 99));
        assert_eq!(c.pinned_dirs().last().unwrap().path, "/a");
        assert!(!c.move_pin_to("/not-pinned", 0));
    }

    #[test]
    fn unpinning_keeps_the_entry_as_a_saved_one() {
        let mut c = HostCatalog::in_memory_dirs(vec![SavedDir::saved("/a")]);
        c.pin_dir("/a");
        assert!(c.unpin_dir("/a"));
        assert!(c.pinned_dirs().is_empty());
        assert_eq!(c.favorites().len(), 1, "the entry itself survives");
        assert!(c.favorites()[0].saved, "and stays saved, not demoted to history");
        assert!(!c.unpin_dir("/a"), "unpinning twice reports nothing done");
    }

    #[test]
    fn pin_ranks_stay_dense_and_survive_a_round_trip() {
        // A hand-edited file can have gaps or duplicate ranks; the order must
        // still be stable and the ranks renumbered on the next change.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hosts.toml");
        let mut c = HostCatalog::load_from(&path);
        for p in ["/a", "/b", "/c"] {
            c.add_favorite(SavedDir::saved(p));
            c.pin_dir(p);
        }
        c.move_pin_to("/c", 0);
        c.save().unwrap();

        let back = HostCatalog::load_from(&path);
        let order: Vec<&str> = back.pinned_dirs().iter().map(|f| f.path.as_str()).collect();
        assert_eq!(order, vec!["/c", "/a", "/b"], "order survives a reload");
        let ranks: Vec<u32> = back.pinned_dirs().iter().filter_map(|f| f.pin_rank).collect();
        assert_eq!(ranks, vec![0, 1, 2], "ranks are dense");
    }

    #[test]
    fn saving_an_existing_history_entry_promotes_it() {
        // `a` on somewhere history already recorded should promote that entry,
        // keeping its visit count, rather than adding a second one.
        let d = tempfile::tempdir().unwrap();
        let p = d.path().to_string_lossy().to_string();
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        c.record_visit(&p);
        assert_eq!(c.favorites().len(), 1);
        assert!(!c.favorites()[0].saved, "a visit is history, not a favourite");

        assert!(c.add_favorite(SavedDir::saved(p)), "saving reports a change");
        assert_eq!(c.favorites().len(), 1, "and does not duplicate");
        assert!(c.favorites()[0].saved);
        assert_eq!(c.favorites()[0].uses, 1, "history is kept");
    }

    #[test]
    fn history_is_capped_but_saved_entries_are_never_trimmed() {
        let mut c = HostCatalog::in_memory_dirs(vec![]);
        c.add_favorite(SavedDir::saved("/kept"));
        // More automatic entries than the cap allows.
        for i in 0..(MAX_HISTORY + 8) {
            c.record_visit(&format!("/auto/{:03}", i));
        }
        let unpinned = c.favorites().iter().filter(|f| !f.saved).count();
        assert_eq!(unpinned, MAX_HISTORY, "history is trimmed to the cap");
        assert!(
            c.favorites().iter().any(|f| f.path == "/kept"),
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

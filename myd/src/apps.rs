//! Saved applications for the `O` (open-with) dialog.
//!
//! `O` runs a program of your choosing over the selection. Before this, the
//! only memory it had was the last command typed, held in RAM and lost on exit
//! — so the handful of programs anyone actually opens files with had to be
//! retyped every session.
//!
//! This is the same idea as [`crate::hosts`] and deliberately the same shape:
//! a small hand-editable TOML catalog, ordered most-recently-used first, with
//! the entries you reach for at the top of the list.
//!
//! Stored at `~/.config/myd/apps.toml` (honouring `$XDG_CONFIG_HOME`, and
//! `$MYD_APPS` ahead of both):
//!
//! ```toml
//! [[app]]
//! label = "gimp"
//! command = "gimp"
//! match = ["*.png", "*.jpg", "*.xcf"]
//! uses = 4
//! last_used = "2026-08-27T09:14:00Z"
//! ```
//!
//! `match` is optional. When the selection is a file the entry matches, it
//! sorts above the rest — so pressing `O` on a photograph offers the image
//! editor first without hiding anything else.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One saved program.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedApp {
    /// What the user calls it. Unique within the catalog, and the key for
    /// edits and deletes — the same contract [`crate::hosts::SavedHost`] has.
    pub label: String,
    /// The command line, run exactly as `O` would run it if typed.
    pub command: String,
    /// Extension globs this program is for, e.g. `*.png`. Empty means "any
    /// file", which is the right answer for an editor or a pager.
    #[serde(default, rename = "match", skip_serializing_if = "Vec::is_empty")]
    pub matches: Vec<String>,
    /// Times run from the picker. Shown in the list; does not affect ordering.
    #[serde(default)]
    pub uses: u64,
    /// RFC 3339 timestamp of the last run. This is what the list is ordered by
    /// — newest first, as the dialing directory is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used: Option<String>,
}

impl SavedApp {
    pub fn new(label: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            command: command.into(),
            ..Default::default()
        }
    }

    /// Attach extension globs.
    pub fn matching(mut self, globs: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.matches = globs.into_iter().map(Into::into).collect();
        self
    }

    /// Whether this entry claims `path`.
    ///
    /// Only `*.ext` patterns and bare extensions are honoured, matched on the
    /// file name's suffix and case-insensitively — `*.PNG` and `*.png` are the
    /// same claim, and a camera that writes `.JPG` should not need its own
    /// entry. Anything else in the list is ignored rather than treated as a
    /// full glob: this is a hand-edited file, and a pattern that silently
    /// matches nothing is friendlier than one that errors on load.
    pub fn claims(&self, path: &Path) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_ascii_lowercase(),
            None => return false,
        };
        self.matches.iter().any(|pat| {
            let pat = pat.trim().to_ascii_lowercase();
            let ext = pat.strip_prefix("*.").or_else(|| pat.strip_prefix('.'));
            match ext {
                Some(e) if !e.is_empty() => name.ends_with(&format!(".{e}")),
                // A bare word is taken as an extension too, so `png` works as
                // well as `*.png` — the file is hand-edited and both readings
                // are obviously intended.
                None if !pat.is_empty() && !pat.contains('*') => {
                    name.ends_with(&format!(".{pat}"))
                }
                _ => false,
            }
        })
    }
}

/// The saved-app list, backed by a TOML file.
#[derive(Debug, Default, Clone)]
pub struct AppCatalog {
    apps: Vec<SavedApp>,
    path: Option<PathBuf>,
}

/// Where the catalog lives.
///
/// `$MYD_APPS` names the file outright, ahead of the usual lookup, so a test
/// can redirect it and never touch the real one. Every config path in myd has
/// such an override for that reason.
pub fn default_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MYD_APPS") {
        return Some(PathBuf::from(explicit));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("myd").join("apps.toml"))
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CatalogFile {
    #[serde(default, rename = "app")]
    apps: Vec<SavedApp>,
}

impl AppCatalog {
    /// Load the catalog, or an empty one.
    ///
    /// A missing, unreadable or malformed file yields an empty catalog rather
    /// than an error, and the file is left alone rather than overwritten — the
    /// reasoning is [`crate::hosts::AppCatalog::load`]'s: this is a
    /// convenience, and a stray character in it should cost a typo's worth of
    /// fixing rather than every saved entry.
    pub fn load() -> Self {
        match default_path() {
            Some(path) => Self::load_from(&path),
            None => Self::default(),
        }
    }

    pub fn load_from(path: &Path) -> Self {
        let apps = std::fs::read_to_string(path)
            .ok()
            .and_then(|body| toml::from_str::<CatalogFile>(&body).ok())
            .map(|f| f.apps)
            .unwrap_or_default();
        Self {
            apps,
            path: Some(path.to_path_buf()),
        }
    }

    /// A catalog that is never written, for tests.
    pub fn in_memory(apps: Vec<SavedApp>) -> Self {
        Self { apps, path: None }
    }

    /// Write the catalog back.
    ///
    /// Through a temporary file and a rename, so an interrupted write cannot
    /// leave a half-serialised catalog where the whole one was.
    pub fn save(&self) -> Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(&CatalogFile {
            apps: self.apps.clone(),
        })
        .context("could not serialise the app catalog")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    pub fn apps(&self) -> &[SavedApp] {
        &self.apps
    }

    pub fn is_empty(&self) -> bool {
        self.apps.is_empty()
    }

    pub fn len(&self) -> usize {
        self.apps.len()
    }

    pub fn find(&self, label: &str) -> Option<&SavedApp> {
        self.apps.iter().find(|a| a.label == label)
    }

    /// Add or replace by label, keeping the counters an existing entry had.
    ///
    /// Editing a command must not reset the usage that decides where it sits in
    /// the list — otherwise fixing a typo sends a favourite to the bottom.
    pub fn upsert(&mut self, app: SavedApp) {
        match self.apps.iter_mut().find(|a| a.label == app.label) {
            Some(existing) => {
                existing.command = app.command;
                existing.matches = app.matches;
                if app.uses > 0 {
                    existing.uses = app.uses;
                }
                if app.last_used.is_some() {
                    existing.last_used = app.last_used;
                }
            }
            None => self.apps.push(app),
        }
    }

    pub fn remove(&mut self, label: &str) -> bool {
        let before = self.apps.len();
        self.apps.retain(|a| a.label != label);
        before != self.apps.len()
    }

    /// Note that `label` was just run.
    pub fn record_use(&mut self, label: &str) {
        if let Some(app) = self.apps.iter_mut().find(|a| a.label == label) {
            app.uses = app.uses.saturating_add(1);
            app.last_used = Some(now_rfc3339());
        }
    }

    /// The list as the dialog shows it: entries claiming `target` first, then
    /// the rest, each group most-recently-used first.
    ///
    /// Matching entries are promoted rather than filtered. Pressing `O` on a
    /// `.png` should offer the image editor first, but the pager still has to
    /// be reachable — hiding the non-matches would make the list lie about what
    /// is saved.
    pub fn for_target(&self, target: Option<&Path>) -> Vec<&SavedApp> {
        let mut refs: Vec<&SavedApp> = self.apps.iter().collect();
        refs.sort_by(|a, b| {
            let am = target.is_some_and(|p| a.claims(p));
            let bm = target.is_some_and(|p| b.claims(p));
            bm.cmp(&am)
                .then_with(|| b.last_used.cmp(&a.last_used))
                .then_with(|| a.label.cmp(&b.label))
        });
        refs
    }
}

/// Now, as an RFC 3339 timestamp.
///
/// Formatted by hand from the Unix epoch rather than pulling in a date library
/// for one line. Only ever compared with other strings from this function, and
/// the format sorts lexicographically in time order, which is all the ordering
/// above needs.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let tod = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60
    )
}

/// Days since the Unix epoch to a civil date. Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat() -> AppCatalog {
        AppCatalog::in_memory(vec![
            SavedApp::new("vim", "vim"),
            SavedApp::new("gimp", "gimp").matching(["*.png", "*.jpg"]),
            SavedApp::new("mpv", "mpv --loop").matching(["*.mp4"]),
        ])
    }

    #[test]
    fn a_glob_claims_only_its_own_extension() {
        let gimp = SavedApp::new("gimp", "gimp").matching(["*.png", "*.jpg"]);
        assert!(gimp.claims(Path::new("/a/photo.png")));
        assert!(gimp.claims(Path::new("/a/photo.jpg")));
        assert!(!gimp.claims(Path::new("/a/notes.txt")));
        assert!(!gimp.claims(Path::new("/a/pngfile")), "a suffix is not an extension");
    }

    /// A camera writes `.JPG`; the entry says `*.jpg`. One should not need two
    /// entries for that.
    #[test]
    fn matching_ignores_case_on_both_sides() {
        let gimp = SavedApp::new("gimp", "gimp").matching(["*.JPG"]);
        assert!(gimp.claims(Path::new("/a/photo.jpg")));
        assert!(gimp.claims(Path::new("/a/photo.JPG")));
    }

    /// The file is hand-edited, so a bare extension reads as one too.
    #[test]
    fn a_bare_extension_works_like_a_glob() {
        for pat in ["png", ".png", "*.png"] {
            let a = SavedApp::new("x", "x").matching([pat]);
            assert!(a.claims(Path::new("/a/b.png")), "{pat} should claim a png");
        }
    }

    /// An unsupported pattern matches nothing rather than failing the load.
    #[test]
    fn an_unsupported_pattern_is_inert() {
        let a = SavedApp::new("x", "x").matching(["src/**/*.rs"]);
        assert!(!a.claims(Path::new("/a/b.rs")));
        assert!(!a.claims(Path::new("/a/b.txt")));
    }

    #[test]
    fn an_entry_with_no_globs_claims_nothing_but_still_lists() {
        let vim = SavedApp::new("vim", "vim");
        assert!(!vim.claims(Path::new("/a/b.txt")));
        let c = cat();
        let listed = c.for_target(Some(Path::new("/a/b.png")));
        assert!(
            listed.iter().any(|a| a.label == "vim"),
            "a general-purpose entry must stay reachable"
        );
    }

    /// Matching entries are promoted, not filtered: the list must not lie about
    /// what is saved.
    #[test]
    fn matching_apps_sort_first_without_hiding_the_rest() {
        let c = cat();
        let listed = c.for_target(Some(Path::new("/a/photo.png")));
        assert_eq!(listed[0].label, "gimp", "the matching entry comes first");
        assert_eq!(listed.len(), 3, "and nothing is hidden");
    }

    #[test]
    fn without_a_target_the_order_is_by_recency() {
        let mut c = cat();
        c.record_use("mpv");
        let listed = c.for_target(None);
        assert_eq!(listed[0].label, "mpv", "the last one run comes first");
    }

    #[test]
    fn upsert_replaces_by_label_and_keeps_the_counters() {
        let mut c = cat();
        c.record_use("vim");
        let before = c.find("vim").unwrap().uses;
        assert_eq!(before, 1);
        c.upsert(SavedApp::new("vim", "nvim"));
        let after = c.find("vim").unwrap();
        assert_eq!(after.command, "nvim", "the command is replaced");
        assert_eq!(after.uses, 1, "fixing a typo must not reset the usage");
        assert_eq!(c.len(), 3, "and must not add a second entry");
    }

    #[test]
    fn remove_reports_whether_it_removed_anything() {
        let mut c = cat();
        assert!(c.remove("vim"));
        assert!(!c.remove("vim"), "removing it twice is not a removal");
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn a_catalog_round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.toml");
        let mut c = AppCatalog::load_from(&path);
        c.upsert(SavedApp::new("gimp", "gimp").matching(["*.png"]));
        c.record_use("gimp");
        c.save().unwrap();

        let back = AppCatalog::load_from(&path);
        let gimp = back.find("gimp").expect("the entry should survive");
        assert_eq!(gimp.command, "gimp");
        assert_eq!(gimp.matches, vec!["*.png"]);
        assert_eq!(gimp.uses, 1);
        assert!(gimp.last_used.is_some());
    }

    /// The file is meant to be edited by hand, so its shape is part of the
    /// contract — `[[app]]` with a `match` key, not `matches`.
    #[test]
    fn the_written_file_reads_the_way_the_docs_say() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.toml");
        let mut c = AppCatalog::load_from(&path);
        c.upsert(SavedApp::new("gimp", "gimp").matching(["*.png"]));
        c.save().unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("[[app]]"), "{body}");
        assert!(body.contains("match = "), "{body}");
        assert!(!body.contains("matches"), "{body}");
    }

    /// A malformed file costs nothing but itself, and is left alone so the typo
    /// can be fixed by hand.
    #[test]
    fn a_malformed_file_yields_an_empty_catalog_and_is_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("apps.toml");
        std::fs::write(&path, "this is not toml {{{").unwrap();
        let c = AppCatalog::load_from(&path);
        assert!(c.is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "this is not toml {{{",
            "the file must be left for the user to fix"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let c = AppCatalog::load_from(&dir.path().join("nope.toml"));
        assert!(c.is_empty());
    }

    /// `$MYD_APPS` must win, or a test would write the real user's catalog.
    #[test]
    fn the_env_override_names_the_file_outright() {
        let dir = tempfile::tempdir().unwrap();
        let want = dir.path().join("elsewhere.toml");
        // SAFETY: single-threaded test, restored immediately.
        unsafe { std::env::set_var("MYD_APPS", &want) };
        let got = default_path();
        unsafe { std::env::remove_var("MYD_APPS") };
        assert_eq!(got, Some(want));
    }

    #[test]
    fn the_timestamp_is_rfc_3339_and_sorts_in_time_order() {
        let s = now_rfc3339();
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'), "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        // The year is this century, so the epoch arithmetic is not off by a
        // century or a leap-year rule.
        let year: i64 = s[..4].parse().unwrap();
        assert!((2020..2100).contains(&year), "{s}");
    }
}

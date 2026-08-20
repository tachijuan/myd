//! User preferences that outlive a session.
//!
//! Distinct from [`config`](crate::config), which is environment-backed
//! tunables for the transport engine: those are set by whoever is diagnosing a
//! slow link, these are set by using the app and are expected to still be there
//! next time.
//!
//! Distinct too from [`ViewPrefs`](crate::panel::ViewPrefs), which is the live
//! per-panel state. That is where a preference is *read* from during a session;
//! this is where it is kept between them.
//!
//! Stored as TOML at `~/.config/myd/prefs.toml` (honouring `$XDG_CONFIG_HOME`),
//! beside the host catalog and hand-editable for the same reason:
//!
//! ```toml
//! info_panel_pct = 35
//! default_archive_format = "zip"
//! ```

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Narrowest the info panel may be set, as a percentage of the panel.
///
/// Below this the values are truncated mid-word and the panel says less than
/// the tree's own permissions column already does.
pub const MIN_INFO_PCT: u16 = 15;

/// Widest the info panel may be set.
///
/// The file listing is what the app is for; past this the panel it is meant to
/// annotate has less room than the annotation.
pub const MAX_INFO_PCT: u16 = 60;

/// Default info panel width, as a percentage of the panel.
pub const DEFAULT_INFO_PCT: u16 = 30;

/// Largest shift `+` and `-` can apply to the info panel's metadata share.
///
/// Mirrors the screen's own limit; the layout clamps to the real panel height
/// as well, so neither half can be squeezed out however this is set by hand.
pub const MAX_META_BIAS: i16 = 40;

/// Preferences as they sit on disk.
///
/// Every field is `#[serde(default)]` so a file written by an older version —
/// or edited by hand down to a single line — still loads, gaining the defaults
/// for anything it does not mention.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prefs {
    /// Info panel width, as a percentage of the panel it sits in.
    #[serde(default = "default_info_pct")]
    pub info_panel_pct: u16,
    /// Rows added to the info panel's metadata share, shifting where the
    /// preview beneath it starts. Negative gives the preview more.
    #[serde(default)]
    pub info_meta_bias: i16,
    /// The format `gz` offers first when creating an archive.
    ///
    /// Read when the dialog opens; the format chosen there applies to that
    /// archive only and is not written back. Every other preference here is
    /// set by an explicit toggle, and inferring a lasting preference from one
    /// use of a one-shot action would be a different bargain.
    #[serde(default, deserialize_with = "lenient_archive_format")]
    pub default_archive_format: crate::vfs::archive::WriteFormat,
}

fn default_info_pct() -> u16 {
    DEFAULT_INFO_PCT
}

/// Read an archive format, falling back to the default rather than failing.
///
/// Without this a hand-written `default_archive_format = "rar"` fails the whole
/// `toml::from_str`, and [`Prefs::load_from`] then discards the entire file —
/// so one typo in the format would silently reset the info panel width as well.
/// The promise this file makes is that a stray character costs *the setting*,
/// and that has to hold per setting to mean anything.
fn lenient_archive_format<'de, D>(d: D) -> Result<crate::vfs::archive::WriteFormat, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use crate::vfs::archive::WriteFormat;
    let raw = Option::<String>::deserialize(d).unwrap_or_default();
    let Some(raw) = raw else {
        return Ok(WriteFormat::default());
    };
    Ok(WriteFormat::from_label(&raw).unwrap_or_else(|| {
        tracing::warn!(value = %raw, "ignoring an unknown default_archive_format");
        WriteFormat::default()
    }))
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            info_panel_pct: DEFAULT_INFO_PCT,
            info_meta_bias: 0,
            default_archive_format: crate::vfs::archive::WriteFormat::default(),
        }
    }
}

/// The preferences as they were at startup.
///
/// Read once, on first use. Panels are built in five places and each wants the
/// saved width; threading it through every one of them would mean a new panel
/// added later silently gets the default instead — the same trap `ViewPrefs`
/// documents for the sort order, where the preference held only where a caller
/// happened to pass it.
///
/// Deliberately a snapshot rather than a live view: a change made during the
/// session is written to disk *and* carried on the panel's own `ViewPrefs`, so
/// nothing needs to re-read this, and a half-written file cannot change a
/// running layout.
pub fn startup() -> Prefs {
    static CACHE: std::sync::OnceLock<Prefs> = std::sync::OnceLock::new();
    *CACHE.get_or_init(Prefs::load)
}

/// Where the preferences live.
///
/// `$MYD_PREFS` names the file outright, ahead of the usual lookup. That is
/// what lets a test drive the real save-and-load path without writing to the
/// config of whoever is running it — and, incidentally, lets a user keep
/// preferences somewhere other than the default.
pub fn default_path() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("MYD_PREFS") {
        return Some(PathBuf::from(explicit));
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("myd").join("prefs.toml"))
}

impl Prefs {
    /// Load the preferences, or the defaults.
    ///
    /// A missing, unreadable, or malformed file yields defaults rather than an
    /// error. Refusing to start because a preferences file has a stray
    /// character in it would trade the whole app for a cosmetic setting, and a
    /// malformed file is left alone rather than overwritten so a typo can be
    /// fixed by hand instead of silently costing whatever else was in there.
    pub fn load() -> Self {
        let Some(path) = default_path() else {
            return Self::default();
        };
        Self::load_from(&path)
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        match toml::from_str::<Prefs>(&raw) {
            Ok(p) => p.clamped(),
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "ignoring malformed preferences"
                );
                Self::default()
            }
        }
    }

    /// The same preferences with every value forced into range.
    ///
    /// Applied on load as well as on change, because the file is meant to be
    /// hand-editable and a hand-written `info_panel_pct = 900` must not lay out
    /// a panel wider than the terminal.
    pub fn clamped(mut self) -> Self {
        self.info_panel_pct = self.info_panel_pct.clamp(MIN_INFO_PCT, MAX_INFO_PCT);
        self.info_meta_bias = self
            .info_meta_bias
            .clamp(-MAX_META_BIAS, MAX_META_BIAS);
        // `default_archive_format` has no arm: an enum is in range by
        // construction, and an unrecognised name never becomes one — see
        // `lenient_archive_format`. Mentioned so its absence reads as a
        // decision rather than an omission.
        self
    }

    /// Write the preferences to their default location.
    ///
    /// Writes to a temporary file and renames, as the host catalog does, so an
    /// interrupted save cannot leave a half-written file where the real one was.
    pub fn save(&self) -> Result<()> {
        let Some(path) = default_path() else {
            return Ok(());
        };
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
        let body = toml::to_string_pretty(self).context("could not serialise the preferences")?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, body).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let prefs = Prefs::load_from(&dir.path().join("nope.toml"));
        assert_eq!(prefs, Prefs::default());
    }

    /// A stray character must cost the setting, not the app.
    #[test]
    fn a_malformed_file_gives_the_defaults_and_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(&path, "info_panel_pct = = 40\n").unwrap();

        assert_eq!(Prefs::load_from(&path), Prefs::default());
        // The file the warning is telling them to fix must still be there to fix.
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "info_panel_pct = = 40\n"
        );
    }

    #[test]
    fn a_saved_width_comes_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        Prefs {
            info_panel_pct: 45,
            ..Prefs::default()
        }
        .save_to(&path)
        .unwrap();
        assert_eq!(Prefs::load_from(&path).info_panel_pct, 45);
    }

    #[test]
    fn a_saved_archive_format_comes_back() {
        use crate::vfs::archive::WriteFormat;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        Prefs {
            default_archive_format: WriteFormat::TarGz,
            ..Prefs::default()
        }
        .save_to(&path)
        .unwrap();
        assert_eq!(
            Prefs::load_from(&path).default_archive_format,
            WriteFormat::TarGz
        );
    }

    /// The file is hand-editable, so it is written in the same words the dialog
    /// shows rather than in Rust's spelling of the variant.
    #[test]
    fn the_archive_format_is_written_as_its_label() {
        use crate::vfs::archive::WriteFormat;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        Prefs {
            default_archive_format: WriteFormat::SevenZ,
            ..Prefs::default()
        }
        .save_to(&path)
        .unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("default_archive_format = \"7z\""),
            "expected the label, got:\n{body}"
        );
    }

    /// A typo in one setting must not cost the others. Plain serde fails the
    /// whole parse on an unknown variant, which would take the width down with
    /// it — the reason `lenient_archive_format` exists.
    #[test]
    fn an_unknown_archive_format_costs_only_that_setting() {
        use crate::vfs::archive::WriteFormat;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(
            &path,
            "info_panel_pct = 45\ndefault_archive_format = \"rar\"\n",
        )
        .unwrap();

        let prefs = Prefs::load_from(&path);
        assert_eq!(
            prefs.default_archive_format,
            WriteFormat::Zip,
            "an unwritable format should fall back to the default"
        );
        assert_eq!(
            prefs.info_panel_pct, 45,
            "the unrelated setting was thrown away with it"
        );
    }

    /// The file is meant to be hand-edited, so a value out of range has to be
    /// brought back in rather than laying out a panel wider than the terminal.
    #[test]
    fn a_hand_written_value_is_clamped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");

        std::fs::write(&path, "info_panel_pct = 900\n").unwrap();
        assert_eq!(Prefs::load_from(&path).info_panel_pct, MAX_INFO_PCT);

        std::fs::write(&path, "info_panel_pct = 1\n").unwrap();
        assert_eq!(Prefs::load_from(&path).info_panel_pct, MIN_INFO_PCT);
    }

    /// A file from a version that did not know a field must still load.
    #[test]
    fn an_empty_file_loads_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(&path, "").unwrap();
        assert_eq!(Prefs::load_from(&path), Prefs::default());
    }

    /// An interrupted save must not leave the temporary file behind as if it
    /// were the real one.
    #[test]
    fn saving_leaves_no_temporary_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        Prefs::default().save_to(&path).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("toml.tmp").exists());
    }
}

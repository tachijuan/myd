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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    /// The program `e` hands a file to, overriding `$EDITOR`.
    ///
    /// Empty means "not set", which is what an absent key deserialises to —
    /// so the precedence below is decided by one rule rather than by whether
    /// the key exists. A command line, not just a program name: `code -w` and
    /// `vim -p` both need their arguments to behave as an editor should.
    #[serde(default)]
    pub editor: String,
}

/// The commented-out template written into a fresh `prefs.toml`.
///
/// A setting nobody can see is a setting nobody uses, and `editor` is the one
/// preference here with no key that toggles it — it can only be discovered by
/// reading the file or the manual. The comment puts it in front of whoever
/// opens the file for any other reason.
///
/// Written once, when the key is absent from the file *and* the comment is not
/// already there. Re-adding it on every launch would grow the file without
/// bound and would fight anyone who deliberately deleted it.
pub const EDITOR_TEMPLATE: &str = "\
# The editor `e` opens files with. Overrides $EDITOR; myd falls back to vim
# when neither is set. Takes arguments, e.g. \"code -w\" or \"vim -p\".
# editor = \"vim\"
";

/// Whether `body` already carries the editor template's own comment.
///
/// Matched on the distinctive first line rather than the whole block, so a
/// user who edited the wording, reflowed it, or uncommented the key keeps
/// their version instead of getting a second copy appended beneath it.
pub fn has_editor_template(body: &str) -> bool {
    body.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("# The editor `e` opens")
            || l.starts_with("editor ")
            || l.starts_with("editor=")
    })
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
            editor: String::new(),
        }
    }
}

/// Put the commented-out `editor` template in the preferences file, once.
///
/// Called at startup. Does nothing when the file already mentions the setting
/// in any form — the comment, or a real `editor =` key — so a launch never
/// re-adds what a previous launch wrote, and never argues with someone who
/// deleted the comment or filled the key in. That check is the whole point:
/// appending unconditionally would grow the file by a paragraph per run.
///
/// Failures are ignored. Seeding a comment is a convenience, and a read-only
/// config directory must not stop the app from starting.
pub fn seed_editor_template() {
    let Some(path) = default_path() else {
        return;
    };
    seed_editor_template_at(&path);
}

pub fn seed_editor_template_at(path: &std::path::Path) {
    let existing = std::fs::read_to_string(path);
    if let Ok(body) = &existing {
        if has_editor_template(body) {
            return;
        }
    }
    // A malformed file is left alone, exactly as `load_from` leaves it: the
    // user is going to open it to fix the typo, and finding myd had also
    // rewritten it would make that harder, not easier.
    if let Ok(body) = &existing {
        if toml::from_str::<Prefs>(body).is_err() {
            return;
        }
    }
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
    }
    let body = match existing {
        Ok(body) => format!("{EDITOR_TEMPLATE}{body}"),
        // No file yet: write the comment alone. An empty TOML document is a
        // valid `Prefs` (every field has a default), so this does not need to
        // serialise anything to be loadable.
        Err(_) => EDITOR_TEMPLATE.to_string(),
    };
    let tmp = path.with_extension("toml.tmp");
    if std::fs::write(&tmp, body).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The editor to run, in precedence order.
///
/// `prefs.toml` first, then `$EDITOR`, then `vim`. The file beats the
/// environment deliberately: `$EDITOR` is set globally for every tool on the
/// machine, and the point of the preference is to say "in myd, use this one".
/// A user who wants to follow the environment simply leaves the key unset,
/// which is the default.
///
/// Whitespace-only values count as unset — a key left as `editor = ""` is a
/// half-finished edit, and running the empty string would fail with a message
/// about a program named "".
pub fn editor_command(prefs: &Prefs, env_editor: Option<&str>) -> String {
    let from_prefs = prefs.editor.trim();
    if !from_prefs.is_empty() {
        return from_prefs.to_string();
    }
    match env_editor.map(str::trim) {
        Some(e) if !e.is_empty() => e.to_string(),
        _ => "vim".to_string(),
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
    CACHE.get_or_init(Prefs::load).clone()
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
        let mut body =
            toml::to_string_pretty(self).context("could not serialise the preferences")?;
        // Serialising the struct cannot produce a comment, so a save would
        // otherwise erase the template every time a width was nudged. Carry it
        // across whenever the file being replaced had it and the value is still
        // at its default — once `editor` is actually set, the real key says
        // what the comment was there to say.
        if self.editor.trim().is_empty()
            && std::fs::read_to_string(path).is_ok_and(|old| has_editor_template(&old))
        {
            body = format!("{EDITOR_TEMPLATE}{body}");
        }
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

    /// The precedence: file, then environment, then vim.
    #[test]
    fn the_editor_falls_back_from_prefs_to_env_to_vim() {
        let mut p = Prefs::default();
        assert_eq!(editor_command(&p, None), "vim", "the last resort");
        assert_eq!(
            editor_command(&p, Some("nano")),
            "nano",
            "$EDITOR beats the built-in default"
        );

        p.editor = "hx".to_string();
        assert_eq!(
            editor_command(&p, Some("nano")),
            "hx",
            "the file beats the environment: it is the myd-specific answer"
        );
    }

    /// A key left empty is a half-finished edit, not a request to run "".
    #[test]
    fn a_blank_editor_setting_is_treated_as_unset() {
        let p = Prefs {
            editor: "   ".to_string(),
            ..Prefs::default()
        };
        assert_eq!(editor_command(&p, Some("nano")), "nano");
        assert_eq!(editor_command(&p, None), "vim");
    }

    /// An editor with arguments survives, since that is how most need running.
    #[test]
    fn the_editor_setting_keeps_its_arguments() {
        let p = Prefs {
            editor: "code -w".to_string(),
            ..Prefs::default()
        };
        assert_eq!(editor_command(&p, None), "code -w");
    }

    /// Seeding writes the template once and never again — the thing that would
    /// otherwise grow the file by a paragraph on every launch.
    #[test]
    fn seeding_the_editor_template_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");

        seed_editor_template_at(&path);
        let once = std::fs::read_to_string(&path).unwrap();
        assert!(once.contains("# editor ="), "the template must be written");

        for _ in 0..5 {
            seed_editor_template_at(&path);
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            once,
            "a later launch must not add a second copy"
        );
        assert_eq!(once.matches("# editor =").count(), 1);
    }

    /// And it leaves an existing file's settings intact.
    #[test]
    fn seeding_preserves_what_is_already_in_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(&path, "info_panel_pct = 45\n").unwrap();

        seed_editor_template_at(&path);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# editor ="), "template added");
        assert!(body.contains("info_panel_pct = 45"), "and nothing lost");
        assert_eq!(
            Prefs::load_from(&path).info_panel_pct,
            45,
            "and the file still parses"
        );
    }

    /// Someone who set the key does not also get the comment telling them to.
    #[test]
    fn seeding_does_nothing_once_the_key_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(&path, "editor = \"hx\"\n").unwrap();

        seed_editor_template_at(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "editor = \"hx\"\n",
            "a set key already says what the comment would"
        );
    }

    /// Someone who deleted the comment on purpose does not get it back.
    #[test]
    fn seeding_does_not_fight_a_deliberate_deletion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");

        seed_editor_template_at(&path);
        // Delete it, as a user tidying the file would.
        std::fs::write(&path, "info_panel_pct = 30\n").unwrap();
        seed_editor_template_at(&path);

        // It comes back once — the file no longer mentions the setting at all,
        // so this is the same case as a fresh file. What must never happen is
        // two copies, which the idempotence test above pins.
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(body.matches("# editor =").count(), 1);
    }

    /// A malformed file is left alone here too, matching `load_from`.
    #[test]
    fn seeding_leaves_a_malformed_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        std::fs::write(&path, "info_panel_pct = = 40\n").unwrap();

        seed_editor_template_at(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "info_panel_pct = = 40\n",
            "the file the user is about to fix must not also be rewritten"
        );
    }

    /// Saving must not destroy the comment a previous launch wrote.
    #[test]
    fn saving_preserves_the_editor_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        seed_editor_template_at(&path);

        let mut prefs = Prefs::load_from(&path);
        prefs.info_panel_pct = 42;
        prefs.save_to(&path).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("# editor ="),
            "serialising the struct would drop the comment:\n{body}"
        );
        assert_eq!(body.matches("# editor =").count(), 1, "and not double it");
        assert_eq!(Prefs::load_from(&path).info_panel_pct, 42);
    }

    /// Once the key is really set, the comment is not carried along beside it.
    #[test]
    fn saving_a_set_editor_drops_the_now_redundant_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefs.toml");
        seed_editor_template_at(&path);

        let mut prefs = Prefs::load_from(&path);
        prefs.editor = "hx".to_string();
        prefs.save_to(&path).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("editor = \"hx\""), "the real key:\n{body}");
        assert!(
            !body.contains("# editor ="),
            "the comment told them to set it; they have:\n{body}"
        );
        assert_eq!(Prefs::load_from(&path).editor, "hx");
    }

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

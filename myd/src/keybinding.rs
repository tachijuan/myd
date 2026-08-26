use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::screen::SortMode;

/// Actions that keybindings resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Confirm,
    CursorDown,
    CursorUp,
    Collapse,
    Expand,
    ToTop,
    ToBottom,
    /// A full viewport down / up (vim `Ctrl-F` / `Ctrl-B`, and the PageDown /
    /// PageUp keys).
    PageDown,
    PageUp,
    /// Half a viewport down / up (vim `Ctrl-D` / `Ctrl-U`).
    HalfPageDown,
    HalfPageUp,
    GoParent,
    ChangeRoot,
    Delete,
    Refresh,
    Rename,
    ToggleSort,
    /// Open the sort picker, to choose an order directly rather than cycling to
    /// it. Bound to the `gs` chord and to clicking the "Sort:" indicator.
    OpenSortMenu,
    /// Sort by the nth mode in `SortMode::ALL`, zero-based.
    ///
    /// The digit keys are the menu's own numbering without the menu: `5` does
    /// what `gs5` does. The menu is still there to be discovered through and to
    /// show what is currently set — this is the shortcut for once you know.
    SetSort(usize),
    /// Rename every tagged file through a regex and a replacement (`gr`).
    PatternRename,
    /// Create an archive of the tagged set, or of the cursor's entry.
    CreateArchive,
    ToggleHidden,
    ToggleBar,
    CollapseAll,
    ExpandAll,
    Search,
    ToggleInfoPanel,
    Help,
    PopScreen,
    GoDirPicker,
    ToggleView,
    /// Switch focus to the other panel (dual mode).
    SwitchPanel,
    /// Toggle between single and dual panel layouts.
    ToggleSplit,
    /// Copy the active panel's selection into the other panel's directory.
    Copy,
    /// Move the active panel's selection into the other panel's directory.
    /// Within one backend this is a rename; across backends a copy then delete.
    Move,
    /// Toggle a tag on the file under the cursor.
    ToggleTag,
    /// Toggle visual (range-tag) mode.
    VisualMode,
    /// Remove all tags.
    UntagAll,
    /// Filter the cursor's directory by a regex pattern.
    Filter,
    /// Create a new directory in the active pane's current directory.
    CreateDir,
    /// Jump to the next match of the last search (down the tree).
    SearchNext,
    /// Jump to the previous match of the last search (up the tree).
    SearchPrev,
    /// Show or hide the transfer sidebar.
    ToggleTransferPanel,
    /// Cancel every queued and in-flight transfer.
    CancelTransfers,
    /// Release or re-grab the mouse, for terminal text selection.
    ToggleMouse,
    /// Hand the selected entry to the desktop's default application.
    OpenWithDefaultApp,
    /// Ask which program to run, then run it over the selection.
    OpenWith,
    /// Browse without measuring directory sizes, and back again.
    ToggleShallow,
    /// Show or hide the tree's `ls -l` permissions column.
    TogglePerms,
    /// Show or hide the tree's modification-time column.
    ToggleTimes,
    /// Show or hide the file preview pane.
    TogglePreview,
    /// Redraw the whole screen from scratch.
    Redraw,
    /// Ring the terminal bell: the keys typed were not a binding.
    ///
    /// Only a chord that went nowhere produces this. A single unbound key stays
    /// silent, as it always has — you have not started anything, so there is
    /// nothing to report failing.
    Bell,
}

/// Tracks whether a chord prefix has been pressed and is awaiting the second key.
///
/// There is no deadline. `g` waits for the next key however long that takes, and
/// the sequence is resolved by what is typed rather than by how fast it was
/// typed. A timeout made the same two keystrokes mean different things depending
/// on the machine's load — the chord tests here had to retry because a busy test
/// run could expire the window between two programmatic presses, which is the
/// same race a user hits on a loaded machine or over a slow link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordState {
    Idle,
    Waiting { key: char },
}

/// Handles key events and resolves them to actions via vi-like bindings + chord detection.
pub struct KeyBindingHandler {
    chord: ChordState,
}

impl Default for KeyBindingHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyBindingHandler {
    pub fn new() -> Self {
        Self {
            chord: ChordState::Idle,
        }
    }

    /// Whether a chord prefix is pending — the UI shows it in the status bar, so
    /// a waiting `g` is visible rather than the app seeming to ignore a key.
    pub fn pending_chord(&self) -> Option<char> {
        match self.chord {
            ChordState::Waiting { key } => Some(key),
            ChordState::Idle => None,
        }
    }

    /// Handle a key event and return the resolved action (if any).
    /// Returns `None` while waiting for the second key in a chord.
    pub fn handle(&mut self, key: KeyEvent) -> Option<Action> {
        let ch = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Enter => '\n',
            _ => {
                // Non-character keys don't participate in chords. Pressing one
                // mid-chord abandons it: Esc is the obvious way out, and an
                // arrow key is not the second half of anything.
                if self.chord != ChordState::Idle {
                    self.chord = ChordState::Idle;
                    // Esc means "never mind", so it is not an error worth a
                    // beep; anything else here is a sequence that went nowhere.
                    if key.code == KeyCode::Esc {
                        return None;
                    }
                    return Some(Action::Bell);
                }
                return self.resolve_single(key);
            }
        };

        if let ChordState::Waiting { key: first } = self.chord {
            let combined = format!("{}{}", first, ch);
            self.chord = ChordState::Idle;
            if let Some(a) = self.resolve_chord(&combined) {
                return Some(a);
            }
            // `gg` is a chord in its own right, so a second `g` cannot be a new
            // prefix — it is handled above. Anything else that does not complete
            // a chord is a mistake, and says so rather than silently running
            // whatever the second key means on its own. That fallback was the
            // real hazard of the old design: `gr` timing out ran `r` (refresh),
            // so a slow `g`+`r` did something other than what was typed.
            Some(Action::Bell)
        } else if ch == 'g' {
            // Only `g` starts a chord.
            self.chord = ChordState::Waiting { key: ch };
            None // Wait for the second key, however long it takes.
        } else {
            self.resolve_single(key)
        }
    }

    /// Test hook for the plain-key table.
    pub fn resolve_single_for_test(&self, key: KeyEvent) -> Option<Action> {
        self.resolve_single(key)
    }

    /// Every second key that completes a `g` chord.
    ///
    /// The footer's pending-chord hint is built from this, so a chord added to
    /// `resolve_chord` without a hint is a test failure rather than a key the
    /// user has no way to discover.
    pub const G_CHORD_KEYS: &'static [char] = &['g', 'u', 'd', 's', 'r', 't', 'x', 'z'];

    /// Resolve a chord (two-character sequence, only g-prefix).
    fn resolve_chord(&self, combined: &str) -> Option<Action> {
        match combined {
            "gg" => Some(Action::ToTop),
            "gu" => Some(Action::GoParent),
            // One picker for every destination. `gr` (connect) and `gs` (saved
            // hosts) used to sit alongside this: `gd` now lists directories and
            // hosts together, `/` searches the lot, and the path field takes an
            // sftp:// URL directly, which left both chords doing nothing `gd`
            // could not. Both have since been reused — `gs` for the sort picker,
            // `gr` for the patterned rename.
            "gd" => Some(Action::GoDirPicker),
            // Opens the numbered picker, so `gs5` is "sort by newest" as one
            // gesture. `s` still cycles, which is quicker once the order is
            // known; this is for going straight there, and for discovering what
            // the orders are.
            "gs" => Some(Action::OpenSortMenu),
            // Lower case, matching the other chords. Plain `R` renames one file;
            // this is the same operation over the tagged set.
            "gr" => Some(Action::PatternRename),
            // The same action as `U`, reachable the way the other set-wide
            // operations are. `U` came first and still works; this is here
            // because the chord footer lists what `g` completes to, so an
            // operation on the tagged set is findable while browsing rather
            // than only by opening the help. `t` for tag, matching the `t` that
            // made the tags in the first place.
            "gt" => Some(Action::UntagAll),
            "gx" => Some(Action::CancelTransfers),
            // `z` for zip, the format most people mean by "archive" and the
            // default the dialog opens on. Lower case, matching the other
            // chords; `c` was taken by copy, which is also how an archive is
            // *extracted*, so the two directions stay distinct keys.
            "gz" => Some(Action::CreateArchive),
            _ => None,
        }
    }

    /// Resolve a single key event.
    fn resolve_single(&self, key: KeyEvent) -> Option<Action> {
        // Handle Ctrl combinations first.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return match key.code {
                KeyCode::Char('c') => Some(Action::Quit), // Ctrl-C to quit.
                KeyCode::Char('r') => Some(Action::Refresh),
                KeyCode::Char('p') => Some(Action::ToggleInfoPanel),
                // vim paging: Ctrl-F / Ctrl-B move a whole screen, Ctrl-D /
                // Ctrl-U half of one. All four measure the real viewport.
                KeyCode::Char('f') => Some(Action::PageDown),
                KeyCode::Char('b') => Some(Action::PageUp),
                KeyCode::Char('d') => Some(Action::HalfPageDown),
                KeyCode::Char('u') => Some(Action::HalfPageUp),
                KeyCode::Char('o') => Some(Action::PopScreen),
                // Pairs with Ctrl-p for the info panel: both are sidebars.
                KeyCode::Char('t') => Some(Action::ToggleTransferPanel),
                // Releases the mouse so the terminal's own text selection works.
                KeyCode::Char('n') => Some(Action::ToggleMouse),
                // Redraw everything, as in a shell or vim. The escape hatch for
                // a screen left corrupted by something outside our control.
                KeyCode::Char('l') => Some(Action::Redraw),
                _ => None,
            };
        }

        match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('j') => Some(Action::CursorDown),
            KeyCode::Char('k') => Some(Action::CursorUp),
            KeyCode::Char('h') => Some(Action::Collapse),
            KeyCode::Char('l') => Some(Action::Expand),
            KeyCode::Char('G') => Some(Action::ToBottom),
            KeyCode::Char('r') => Some(Action::Refresh),
            KeyCode::Char('R') => Some(Action::Rename),
            KeyCode::Char('D') => Some(Action::Delete),
            KeyCode::Char('s') => Some(Action::ToggleSort),
            KeyCode::Char('H') => Some(Action::ToggleHidden),
            KeyCode::Char('B') => Some(Action::ToggleBar),
            KeyCode::Char('o') => Some(Action::OpenWithDefaultApp),
            KeyCode::Char('O') => Some(Action::OpenWith),
            KeyCode::Char('S') => Some(Action::ToggleShallow),
            KeyCode::Char('P') => Some(Action::TogglePerms),
            KeyCode::Char('T') => Some(Action::ToggleTimes),
            KeyCode::Char('v') => Some(Action::ToggleView),
            KeyCode::Char('c') => Some(Action::Copy),
            KeyCode::Char('m') => Some(Action::Move),
            KeyCode::Char('|') => Some(Action::ToggleSplit),
            KeyCode::Tab => Some(Action::SwitchPanel),
            KeyCode::Char('t') => Some(Action::ToggleTag),
            KeyCode::Char('V') => Some(Action::VisualMode),
            KeyCode::Char('U') => Some(Action::UntagAll),
            KeyCode::Char('f') => Some(Action::Filter),
            KeyCode::Char('N') => Some(Action::CreateDir),
            KeyCode::Char('n') => Some(Action::SearchNext),
            KeyCode::Char('p') => Some(Action::SearchPrev),
            // Space opens and closes the preview pane. It was the one unbound
            // key that reads as "show me this", and it is not an accept key in
            // any dialog, so a modal can never mistake it for one.
            KeyCode::Char(' ') => Some(Action::TogglePreview),
            KeyCode::Char('0') => Some(Action::CollapseAll),
            // The sort menu's numbers, usable without opening the menu: `5` and
            // `gs5` set the same order. Derived from `SortMode::ALL` so the two
            // numberings cannot drift apart, and so a digit past the last mode
            // stays unbound rather than silently sorting by something else. `0`
            // is collapse-all and predates this; the menu starts at 1 anyway.
            KeyCode::Char(c @ '1'..='9') if (c as usize - '0' as usize) <= SortMode::ALL.len() => {
                Some(Action::SetSort(c as usize - '1' as usize))
            }
            KeyCode::Char('*') => Some(Action::ExpandAll),
            KeyCode::Char('/') => Some(Action::Search),
            KeyCode::Char('?') => Some(Action::Help),
            KeyCode::Char('\n') => Some(Action::Confirm),
            KeyCode::Enter => Some(Action::Confirm),
            KeyCode::Esc => Some(Action::Quit),
            KeyCode::F(1) => Some(Action::Help),
            KeyCode::Down => Some(Action::CursorDown),
            KeyCode::Up => Some(Action::CursorUp),
            KeyCode::Char('u') => Some(Action::GoParent),
            KeyCode::Left => Some(Action::Collapse),
            KeyCode::Right => Some(Action::Expand),
            KeyCode::Home => Some(Action::ToTop),
            KeyCode::End => Some(Action::ToBottom),
            KeyCode::PageDown => Some(Action::PageDown),
            KeyCode::PageUp => Some(Action::PageUp),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `G_CHORD_KEYS` must list exactly the keys that complete a `g` chord.
    ///
    /// The footer hint is built from it, so a chord added to `resolve_chord`
    /// without updating the list would be undiscoverable, and a key listed but
    /// unbound would send the user to the bell.
    #[test]
    fn g_chord_keys_match_the_resolver() {
        let h = KeyBindingHandler::new();
        // Every advertised key resolves.
        for c in KeyBindingHandler::G_CHORD_KEYS {
            assert!(
                h.resolve_chord(&format!("g{}", c)).is_some(),
                "g{} is advertised in the footer but resolves to nothing",
                c
            );
        }
        // And nothing else does: sweep the printable ASCII range so a new chord
        // cannot be added without appearing here.
        for c in (b' '..=b'~').map(char::from) {
            let bound = h.resolve_chord(&format!("g{}", c)).is_some();
            let advertised = KeyBindingHandler::G_CHORD_KEYS.contains(&c);
            assert_eq!(
                bound, advertised,
                "g{} is {} but {} in G_CHORD_KEYS",
                c,
                if bound { "bound" } else { "unbound" },
                if advertised { "listed" } else { "missing" },
            );
        }
    }
}

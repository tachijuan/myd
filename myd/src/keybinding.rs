use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

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
    /// Browse without measuring directory sizes, and back again.
    ToggleShallow,
    /// Show or hide the tree's `ls -l` permissions column.
    TogglePerms,
    /// Show or hide the tree's modification-time column.
    ToggleTimes,
    /// Show or hide the file preview pane.
    TogglePreview,
}

/// Tracks whether a chord prefix has been pressed and is awaiting the second key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChordState {
    Idle,
    Waiting { key: char, deadline: Instant },
}

/// Handles key events and resolves them to actions via vi-like bindings + chord detection.
pub struct KeyBindingHandler {
    chord: ChordState,
    chord_timeout: Duration,
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
            chord_timeout: Duration::from_millis(500),
        }
    }

    /// Handle a key event and return the resolved action (if any).
    /// Returns `None` while waiting for the second key in a chord.
    pub fn handle(&mut self, key: KeyEvent) -> Option<Action> {
        let ch = match key.code {
            KeyCode::Char(c) => c,
            KeyCode::Enter => '\n',
            _ => {
                // Non-character keys don't participate in chords.
                self.chord = ChordState::Idle;
                return self.resolve_single(key);
            }
        };

        // Check if the chord is still alive (only g-prefix chords).
        if let ChordState::Waiting {
            key: first,
            deadline,
        } = self.chord
        {
            if Instant::now() > deadline {
                // Timeout — fall back to single-key handling for the new key.
                self.chord = ChordState::Idle;
                return self.resolve_single(key);
            } else {
                // Chord completed (gX).
                let combined = format!("{}{}", first, ch);
                self.chord = ChordState::Idle;
                if let Some(a) = self.resolve_chord(&combined) {
                    return Some(a);
                }
                // Chord didn't match — fall back to second key.
                if ch == 'g' {
                    self.chord = ChordState::Waiting {
                        key: ch,
                        deadline: Instant::now() + self.chord_timeout,
                    };
                    return None;
                }
                return self.resolve_single_char(ch);
            }
        }

        // Only g starts a chord (no d-chord to avoid delay).
        if ch == 'g' {
            self.chord = ChordState::Waiting {
                key: ch,
                deadline: Instant::now() + self.chord_timeout,
            };
            return None; // Wait for second key.
        }

        self.chord = ChordState::Idle;
        self.resolve_single(key)
    }

    /// Test hooks for the two plain-key tables, so a test can assert they agree.
    /// The chord-fallback table is only reachable through a timed-out `g`, which
    /// makes a disagreement between them very easy to miss in practice.
    pub fn resolve_single_for_test(&self, key: KeyEvent) -> Option<Action> {
        self.resolve_single(key)
    }

    pub fn resolve_single_char_for_test(&self, c: char) -> Option<Action> {
        self.resolve_single_char(c)
    }

    /// Resolve a single character to an action (used by chord fallback).
    fn resolve_single_char(&self, c: char) -> Option<Action> {
        match c {
            'q' => Some(Action::Quit),
            'j' => Some(Action::CursorDown),
            'k' => Some(Action::CursorUp),
            'h' => Some(Action::Collapse),
            'l' => Some(Action::Expand),
            'G' => Some(Action::ToBottom),
            'r' => Some(Action::Refresh),
            'R' => Some(Action::Rename),
            's' => Some(Action::ToggleSort),
            'H' => Some(Action::ToggleHidden),
            'b' => Some(Action::ToggleBar),
            'o' => Some(Action::OpenWithDefaultApp),
            'S' => Some(Action::ToggleShallow),
            'P' => Some(Action::TogglePerms),
            'T' => Some(Action::ToggleTimes),
            '0' => Some(Action::CollapseAll),
            '*' => Some(Action::ExpandAll),
            '/' => Some(Action::Search),
            '?' => Some(Action::Help),
            'u' => Some(Action::GoParent),
            'v' => Some(Action::ToggleView),
            'c' => Some(Action::Copy),
            'm' => Some(Action::Move),
            '|' => Some(Action::ToggleSplit),
            't' => Some(Action::ToggleTag),
            'V' => Some(Action::VisualMode),
            'U' => Some(Action::UntagAll),
            'f' => Some(Action::Filter),
            'N' => Some(Action::CreateDir),
            'n' => Some(Action::SearchNext),
            'p' => Some(Action::SearchPrev),
            ' ' => Some(Action::TogglePreview),
            _ => None,
        }
    }

    /// Resolve a chord (two-character sequence, only g-prefix).
    fn resolve_chord(&self, combined: &str) -> Option<Action> {
        match combined {
            "gg" => Some(Action::ToTop),
            "gu" => Some(Action::GoParent),
            // One picker for every destination. `gr` (connect) and `gs` (saved
            // hosts) used to sit alongside this: `gd` now lists directories and
            // hosts together, `/` searches the lot, and the path field takes an
            // sftp:// URL directly, which left both chords doing nothing `gd`
            // could not.
            "gd" => Some(Action::GoDirPicker),
            "gx" => Some(Action::CancelTransfers),
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
            KeyCode::Char('b') => Some(Action::ToggleBar),
            KeyCode::Char('o') => Some(Action::OpenWithDefaultApp),
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

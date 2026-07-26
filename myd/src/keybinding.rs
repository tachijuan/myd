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
    PageDown,
    PageUp,
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
    /// Prompt for a remote host to connect to.
    Connect,
    /// Open the dialing directory on the full saved-host list.
    HostDirectory,
    /// Release or re-grab the mouse, for terminal text selection.
    ToggleMouse,
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
            _ => None,
        }
    }

    /// Resolve a chord (two-character sequence, only g-prefix).
    fn resolve_chord(&self, combined: &str) -> Option<Action> {
        match combined {
            "gg" => Some(Action::ToTop),
            "gu" => Some(Action::GoParent),
            "gd" => Some(Action::GoDirPicker),
            // Single letters are effectively exhausted, so remote connect and
            // transfer-cancel take g-chords.
            "gr" => Some(Action::Connect),
            // The full saved-host list, skipping the recent-three view.
            "gs" => Some(Action::HostDirectory),
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
                KeyCode::Char('b') => Some(Action::ToggleInfoPanel),
                KeyCode::Char('d') => Some(Action::PageDown),
                KeyCode::Char('u') => Some(Action::PageUp),
                KeyCode::Char('o') => Some(Action::PopScreen),
                // Pairs with Ctrl-b for the info panel: both are sidebars.
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

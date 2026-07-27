use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::path::{Path, PathBuf};

/// Which half of the picker has the keyboard.
///
/// The screen is a path field *and* a list, so a bare `j` is ambiguous. Rather
/// than guess, focus is explicit and `Tab` moves it, matching how `Tab` already
/// switches panels in the main view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerFocus {
    /// Typing edits the path; `j`/`k` are ordinary characters.
    Field,
    /// `j`/`k` walk the list; typing a printable character jumps to the field so
    /// starting to type a path never silently does nothing.
    List,
}

/// State for the directory picker startup screen.
pub struct DirPickerState {
    options: Vec<PickerOption>,
    cursor: usize,
    /// Current input value (typed path).
    input: String,
    /// Input cursor position. Must always be a valid char boundary within
    /// `input` and never past its end — see `set_input`.
    input_cursor: usize,
    /// Whether `input` was filled from the highlighted option rather than typed.
    ///
    /// Browsing the list mirrors the option into the field so the user can see
    /// what Enter would open, but that text is a *suggestion*. The first typed
    /// character replaces it rather than extending it: appending produced a
    /// nonsense concatenation of the option and the typed path, which then
    /// resolved to whichever half happened to exist.
    input_is_suggestion: bool,
    /// Which half of the screen the keyboard drives.
    focus: PickerFocus,
    /// Which kinds of row this picker lists.
    scope: PickerScope,
    /// Whether `/` has been pressed and typing is narrowing the list.
    searching: bool,
    /// Incremental search over the list, entered with `/`.
    ///
    /// Empty means "show everything". Kept separate from the path field: that
    /// one is a destination you are composing, this one narrows what is on
    /// screen, and conflating them made `/` ambiguous.
    query: String,
    /// Indices into `options` currently on screen, in display order.
    ///
    /// Filtering rewrites this rather than the list itself, so the cursor can
    /// always be mapped back to the right row — acting on the wrong entry after
    /// a search would be the obvious bug here.
    visible: Vec<usize>,
    /// An in-progress `m` reorder: the path being moved and where it started, so
    /// Esc can put it back.
    ///
    /// Held here rather than committed per keystroke because a cancelled move
    /// has to restore the original position exactly, and the catalog would
    /// otherwise have already been renumbered several times.
    moving: Option<MoveState>,
    /// A favourite the user asked to add or remove, awaiting the app.
    ///
    /// The picker cannot persist anything itself — the catalog and its file live
    /// on the app — so an edit is recorded here and drained on the next key
    /// dispatch, in the same spirit as the loading screens' pending results.
    pending_edit: Option<FavoriteEdit>,
}

/// What confirming the picker asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    /// Open this directory.
    Open(PathBuf),
    /// Connect to this remote target.
    Connect(String),
    /// The typed path is not a directory, so nothing was opened.
    NotADirectory(PathBuf),
    /// Nothing typed and nothing to highlight.
    Nothing,
}

/// An in-progress reorder of the pinned block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveState {
    /// The entry being moved.
    pub path: PathBuf,
    /// The pinned order as it stood when the move began, for Esc.
    pub original: Vec<String>,
    /// Whether the entry has been slid out of the pinned block entirely, which
    /// unpins it on confirm.
    pub unpinning: bool,
}

/// A requested change to the saved directory list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FavoriteEdit {
    /// Ask the user which directory to save. `a` is "add a favourite", not
    /// "bookmark whatever the cursor happens to be on" — the point is to save a
    /// place you know about, which is usually not one already on the list.
    PromptAdd,
    /// Forget this path.
    Remove(PathBuf),
    /// Open the form for this saved host.
    EditHost(String),
    /// Edit this saved directory's path.
    EditDir(PathBuf),
    /// Ask before forgetting this saved host.
    DeleteHost(String),
    /// Pin this path to the bottom of the pinned block.
    Pin(PathBuf),
    /// Pin this path, then immediately begin moving it — `m` on an entry that
    /// is not yet in the pinned block.
    PinAndMove(PathBuf),
    /// Remove this path from the pinned block, keeping it saved.
    Unpin(PathBuf),
    /// Commit a reorder: the pinned block's new order, and any path that was
    /// slid out of it.
    Reorder {
        order: Vec<String>,
        unpin: Option<PathBuf>,
    },
}

/// One row of the picker's shortcut list.
#[derive(Debug, Clone)]
pub struct PickerOption {
    pub path: PathBuf,
    pub label: String,
    /// Whether this row came from the saved favourites rather than the built-in
    /// locations. Only a favourite can be removed, and only a non-favourite can
    /// be added.
    pub is_favorite: bool,
    /// Which group this row belongs to: pinned, saved, or recent history. A
    /// built-in location is `Recent`.
    pub tier: crate::hosts::DirTier,
    /// Times visited, for the trailing count. Zero for an unvisited built-in.
    pub uses: u64,
    /// RFC 3339 last visit, or `None`. Drives the ordering.
    pub last_used: Option<String>,
    /// A saved remote host, when this row is one.
    ///
    /// The picker lists local directories and saved hosts together, since both
    /// answer "where do you want to go" and keeping two near-identical screens
    /// meant two places to fix every bug. The kinds stay visually separate and a
    /// few keys only apply to one of them, so the row has to know which it is.
    pub host: Option<crate::hosts::SavedHost>,
}

impl PickerOption {
    /// Whether this row is a saved remote host rather than a local directory.
    pub fn is_host(&self) -> bool {
        self.host.is_some()
    }

    /// Which section this row is drawn under.
    pub fn section(&self) -> PickerSection {
        if self.is_host() {
            PickerSection::Hosts
        } else {
            PickerSection::Directories
        }
    }

    /// The text a search matches against: everything the row shows.
    pub fn search_text(&self) -> String {
        match &self.host {
            Some(h) => format!("{} {} {}", h.label, h.to_url(), self.label),
            None => format!("{} {}", self.label, self.path.display()),
        }
    }
}

/// The two groups the combined picker draws, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PickerSection {
    Directories,
    Hosts,
}

/// Which kinds of row the picker is showing.
///
/// `gd` opens on everything; `gs` opens filtered to hosts, so someone with many
/// of both can still get straight to the remote list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PickerScope {
    #[default]
    All,
    HostsOnly,
}

impl Default for DirPickerState {
    fn default() -> Self {
        Self::new()
    }
}

impl DirPickerState {
    pub fn new() -> Self {
        Self::with_favorites(&[])
    }

    /// Build the picker over `favorites` plus the built-in locations.
    ///
    /// The two are merged into one list ordered by recency, so a directory you
    /// actually use rises to the top whether or not it is one of the built-ins.
    /// A favourite that duplicates a built-in path replaces it rather than
    /// appearing twice.
    /// Build the picker over an entire catalog: directories and saved hosts.
    ///
    /// `scope` decides which kinds are listed — `gd` shows everything, `gs`
    /// shows only the hosts.
    pub fn with_catalog(catalog: &crate::hosts::HostCatalog, scope: PickerScope) -> Self {
        let mut picker = if scope == PickerScope::HostsOnly {
            Self::with_favorites(&[])
        } else {
            Self::with_favorites(catalog.favorites())
        };
        if scope == PickerScope::HostsOnly {
            // The synthesised working-directory row is a directory, so it has no
            // place in a hosts-only view.
            picker.options.clear();
        }
        picker.scope = scope;

        // Hosts follow the directories, most recently connected first — the same
        // ordering rule the directory tiers use.
        for h in catalog.recent(usize::MAX) {
            picker.options.push(PickerOption {
                // A host's "path" is its URL, so every row still has one thing
                // that identifies it and Enter has something to act on.
                path: PathBuf::from(h.to_url()),
                label: h.label.clone(),
                is_favorite: true,
                tier: crate::hosts::DirTier::Saved,
                uses: h.uses,
                last_used: h.last_used.clone(),
                host: Some(h.clone()),
            });
        }
        picker.cursor = 0;
        picker.recompute_visible();
        picker
    }

    pub fn with_favorites(favorites: &[crate::hosts::SavedDir]) -> Self {
        // Every row comes from the catalog. The standard locations used to be a
        // separate hardcoded list merged in here, which made them look like
        // ordinary entries while `p`, `m` and `d` silently ignored them; they
        // are seeded into the catalog now instead (see `hosts::seed_dirs`).
        //
        // One exception is still synthesised: the working directory, which
        // differs per launch and so cannot sensibly be stored.
        let cwd = std::env::current_dir().unwrap_or(PathBuf::from("."));

        let mut options: Vec<PickerOption> = Vec::new();
        for f in favorites {
            options.push(PickerOption {
                path: PathBuf::from(&f.path),
                label: f.display().to_string(),
                is_favorite: true,
                tier: f.tier(),
                uses: f.uses,
                last_used: f.last_used.clone(),
                host: None,
            });
        }
        if cwd.is_dir() && !options.iter().any(|o| o.path == cwd) {
            options.push(PickerOption {
                path: cwd.clone(),
                label: format!(". (Current: {})", cwd.display()),
                // Not a catalog entry, so it cannot be pinned or removed — but
                // `a` saves it, which turns it into one.
                is_favorite: false,
                tier: crate::hosts::DirTier::Recent,
                uses: 0,
                last_used: None,
                host: None,
            });
        }

        // Grouped by tier, then ordered within each. The pinned block keeps the
        // order the user arranged; the other two are most-recently-visited
        // first. Built-ins have no timestamp and so settle below anything
        // actually used, in their original order rather than alphabetically —
        // "Home" before "/tmp" reads better than the reverse.
        let declared: std::collections::HashMap<&Path, usize> = options
            .iter()
            .enumerate()
            .map(|(i, o)| (o.path.as_path(), i))
            .collect();
        // Rank within the pinned block, taken from the catalog's order.
        let pin_rank: std::collections::HashMap<&str, usize> = favorites
            .iter()
            .filter(|f| f.is_pinned())
            .map(|f| (f.path.as_str(), f.pin_rank.unwrap_or(u32::MAX) as usize))
            .collect();

        let mut indexed: Vec<(usize, PickerOption)> = options
            .iter()
            .map(|o| (declared[o.path.as_path()], o.clone()))
            .collect();
        indexed.sort_by(|(ai, a), (bi, b)| {
            a.tier
                .cmp(&b.tier)
                .then_with(|| {
                    if a.tier == crate::hosts::DirTier::Pinned {
                        let ar = pin_rank
                            .get(a.path.to_string_lossy().as_ref())
                            .copied()
                            .unwrap_or(usize::MAX);
                        let br = pin_rank
                            .get(b.path.to_string_lossy().as_ref())
                            .copied()
                            .unwrap_or(usize::MAX);
                        ar.cmp(&br)
                    } else {
                        let (ak, bk) = (
                            a.last_used.as_deref().unwrap_or(""),
                            b.last_used.as_deref().unwrap_or(""),
                        );
                        bk.cmp(ak)
                    }
                })
                .then_with(|| ai.cmp(bi))
        });
        let options: Vec<PickerOption> = indexed.into_iter().map(|(_, o)| o).collect();

        let mut picker = Self {
            options,
            cursor: 0,
            input: String::new(),
            input_cursor: 0,
            input_is_suggestion: false,
            // The field starts focused: the picker exists to accept a path, and
            // the common directories are the shortcut, not the main event.
            focus: PickerFocus::Field,
            scope: PickerScope::All,
            searching: false,
            query: String::new(),
            visible: Vec::new(),
            moving: None,
            pending_edit: None,
        };
        // `visible` is what the cursor indexes and what render walks, so it has
        // to be populated before the picker is handed out — an empty one leaves
        // a list with rows that cannot be selected or drawn.
        picker.recompute_visible();
        picker
    }

    /// Rebuild the visible index list from the scope and the search query.
    fn recompute_visible(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, o)| match self.scope {
                PickerScope::All => true,
                PickerScope::HostsOnly => o.is_host(),
            })
            .filter(|(_, o)| {
                query.is_empty() || o.search_text().to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
    }

    /// Which kinds of row this picker is listing.
    pub fn scope(&self) -> PickerScope {
        self.scope
    }

    /// The active search query, for the title bar.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Rows currently on screen, in display order.
    pub fn visible_options(&self) -> Vec<&PickerOption> {
        self.visible.iter().filter_map(|&i| self.options.get(i)).collect()
    }

    /// How many rows the filter is showing.
    pub fn visible_count(&self) -> usize {
        self.visible.len()
    }

    /// The reorder in progress, if any.
    pub fn moving(&self) -> Option<&MoveState> {
        self.moving.as_ref()
    }

    /// Take the pending favourite edit, if the user asked for one.
    pub fn take_favorite_edit(&mut self) -> Option<FavoriteEdit> {
        self.pending_edit.take()
    }

    /// The highlighted option, if any.
    pub fn selected(&self) -> Option<&PickerOption> {
        // `cursor` indexes the *visible* rows, so a search cannot leave it
        // pointing at a row that is filtered out.
        self.visible
            .get(self.cursor)
            .and_then(|&i| self.options.get(i))
    }


    /// Carry the keyboard state across a rebuild, so adding or removing a
    /// favourite does not dump the user back into the path field.
    pub fn adopt_focus_from(&mut self, other: &Self) {
        self.focus = other.focus;
        self.input.clone_from(&other.input);
        self.input_cursor = other.input_cursor;
        self.input_is_suggestion = other.input_is_suggestion;
    }

    /// Highlight the row for `path`, if it is still listed. Leaves the cursor
    /// alone otherwise, which is what happens to the row just removed.
    pub fn select_path(&mut self, path: &Path) {
        if let Some(i) = self
            .visible
            .iter()
            .position(|&i| self.options[i].path == path)
        {
            self.cursor = i;
        } else {
            self.cursor = self.cursor.min(self.visible.len().saturating_sub(1));
        }
    }

    /// Show `value` in the path field as a *suggestion* from the option list.
    ///
    /// The text cursor goes to the end, and the content is marked so the next
    /// typed character replaces it. Assigning `input` directly left `input_cursor`
    /// at 0, so typing inserted in front of the suggestion and the field became
    /// `<typed><option>` — which resolved to whichever half existed, and looked
    /// exactly like a typed path being ignored.
    fn set_suggestion(&mut self, value: String) {
        self.input_cursor = value.chars().count();
        self.input = value;
        self.input_is_suggestion = true;
    }

    /// Clear a suggestion before the first real edit, so typing replaces the
    /// list's proposal instead of appending to it.
    fn take_over_suggestion(&mut self) {
        if self.input_is_suggestion {
            self.input.clear();
            self.input_cursor = 0;
            self.input_is_suggestion = false;
        }
    }

    /// Which half of the picker currently has the keyboard.
    pub fn focus(&self) -> PickerFocus {
        self.focus
    }

    /// Move the keyboard between the path field and the list.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PickerFocus::Field => PickerFocus::List,
            PickerFocus::List => PickerFocus::Field,
        };
    }

    /// The current contents of the path field. Test hook.
    pub fn input_for_test(&self) -> &str {
        &self.input
    }

    /// The shortcut list, in display order. Test hook.
    pub fn options_for_test(&self) -> &[PickerOption] {
        &self.options
    }

    /// The index of the highlighted option. Test hook.
    pub fn cursor_for_test(&self) -> usize {
        self.cursor
    }

    pub fn resolve_path(&self, path_str: &str) -> Option<PathBuf> {
        let path = expand_tilde(path_str);
        let path = path.canonicalize().unwrap_or(path);
        if path.is_dir() {
            Some(path)
        } else {
            None
        }
    }

    /// Byte offset of the text cursor.
    ///
    /// `input_cursor` counts *characters* (the renderer indexes it with
    /// `char_indices`), so every operation on the string has to convert. Mixing
    /// the two would panic on a path containing any non-ASCII character.
    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.input_cursor)
            .map(|(i, _)| i)
            .unwrap_or(self.input.len())
    }

    /// Number of characters in the field.
    fn input_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Handle typed character in the input field.
    pub fn input_char(&mut self, c: char) {
        self.take_over_suggestion();
        let at = self.cursor_byte();
        self.input.insert(at, c);
        self.input_cursor += 1;
    }

    pub fn input_backspace(&mut self) {
        // Backspacing a suggestion clears the whole thing: it was never typed, so
        // erasing it one character at a time is busywork.
        if self.input_is_suggestion {
            self.take_over_suggestion();
            return;
        }
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    /// Delete the character under the cursor.
    pub fn input_delete(&mut self) {
        if self.input_is_suggestion {
            self.take_over_suggestion();
            return;
        }
        if self.input_cursor < self.input_len() {
            let at = self.cursor_byte();
            self.input.remove(at);
        }
    }

    pub fn input_left(&mut self) {
        // Deliberately moving inside the text means the user intends to edit it,
        // so keep it rather than discarding it on the next keystroke.
        self.input_is_suggestion = false;
        if self.input_cursor > 0 {
            self.input_cursor -= 1;
        }
    }

    pub fn input_right(&mut self) {
        self.input_is_suggestion = false;
        if self.input_cursor < self.input_len() {
            self.input_cursor += 1;
        }
    }

    /// Highlight option `index` and mirror it into the path field, so the field
    /// always shows what Enter would open.
    fn select(&mut self, index: usize) {
        if self.visible.is_empty() {
            return;
        }
        self.cursor = index.min(self.visible.len() - 1);
        if let Some(opt) = self.selected() {
            let shown = opt.path.to_string_lossy().to_string();
            self.set_suggestion(shown);
        }
    }

    fn select_next(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let next = (self.cursor + 1) % self.visible.len();
        self.select(next);
    }

    fn select_prev(&mut self) {
        if self.visible.is_empty() {
            return;
        }
        let prev = if self.cursor == 0 {
            self.visible.len() - 1
        } else {
            self.cursor - 1
        };
        self.select(prev);
    }

    fn select_first(&mut self) {
        self.select(0);
    }

    fn select_last(&mut self) {
        self.select(self.visible.len().saturating_sub(1));
    }

    /// Confirm the current selection.
    ///
    /// The three outcomes are distinct on purpose. A typed path that does not
    /// resolve used to fall through to the highlighted list entry, so a typo
    /// silently opened somewhere else — worse than doing nothing, because the
    /// user believes they arrived where they asked.
    pub fn confirm(&self) -> PickerChoice {
        let typed = self.input.trim();
        if !typed.is_empty() {
            return match self.resolve_path(typed) {
                Some(p) => PickerChoice::Open(p),
                None => PickerChoice::NotADirectory(expand_tilde(typed)),
            };
        }
        // An empty field means "open what is highlighted", which is how Enter
        // picks an entry from the list.
        match self.selected() {
            // A host row's Enter is a connect, not a directory open; the caller
            // dispatches on the row's kind.
            Some(opt) if opt.is_host() => PickerChoice::Connect(opt.path.to_string_lossy().to_string()),
            Some(opt) => PickerChoice::Open(opt.path.clone()),
            None => PickerChoice::Nothing,
        }
    }

    /// Begin reordering `path`, which must already be in the pinned block.
    ///
    /// Public counterpart of [`Self::begin_move`], for the app to call after it
    /// has pinned an entry and rebuilt the list.
    pub fn start_move(&mut self, path: &Path) {
        if self
            .options
            .iter()
            .any(|o| o.path == path && o.tier == crate::hosts::DirTier::Pinned)
        {
            self.focus = PickerFocus::List;
            self.begin_move(path.to_path_buf());
        }
    }

    /// Begin reordering `path` within the pinned block.
    fn begin_move(&mut self, path: PathBuf) {
        let original: Vec<String> = self
            .options
            .iter()
            .filter(|o| o.tier == crate::hosts::DirTier::Pinned)
            .map(|o| o.path.to_string_lossy().to_string())
            .collect();
        self.moving = Some(MoveState {
            path,
            original,
            unpinning: false,
        });
    }

    /// Number of rows currently in the pinned block.
    fn pinned_count(&self) -> usize {
        self.options
            .iter()
            .filter(|o| o.tier == crate::hosts::DirTier::Pinned)
            .count()
    }

    /// Drive an in-progress reorder. Returns whether the app keeps running.
    fn handle_move_key(&mut self, code: KeyCode) -> bool {
        let Some(state) = self.moving.clone() else {
            return true;
        };
        match code {
            KeyCode::Char('j') | KeyCode::Down => {
                let last_pinned = self.pinned_count().saturating_sub(1);
                if self.cursor < last_pinned {
                    self.swap_rows(self.cursor, self.cursor + 1);
                    self.cursor += 1;
                } else if !state.unpinning && self.cursor < self.options.len() - 1 {
                    // One step past the bottom of the block takes the entry out
                    // of it — a move doubles as an unpin. The row's tier changes
                    // with it, so the marker and colour show what will happen
                    // before Enter commits to it.
                    if let Some(m) = self.moving.as_mut() {
                        m.unpinning = true;
                    }
                    if let Some(row) = self.options.get_mut(self.cursor) {
                        row.tier = crate::hosts::DirTier::Saved;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if state.unpinning {
                    // Back into the block it just left.
                    if let Some(m) = self.moving.as_mut() {
                        m.unpinning = false;
                    }
                    if let Some(row) = self.options.get_mut(self.cursor) {
                        row.tier = crate::hosts::DirTier::Pinned;
                    }
                } else if self.cursor > 0 {
                    self.swap_rows(self.cursor - 1, self.cursor);
                    self.cursor -= 1;
                }
            }
            KeyCode::Enter => {
                let order: Vec<String> = self
                    .options
                    .iter()
                    .filter(|o| o.tier == crate::hosts::DirTier::Pinned)
                    .map(|o| o.path.to_string_lossy().to_string())
                    .collect();
                let unpin = state.unpinning.then(|| state.path.clone());
                self.moving = None;
                self.pending_edit = Some(FavoriteEdit::Reorder { order, unpin });
            }
            KeyCode::Esc => {
                // Put the block back exactly as it was.
                self.moving = None;
                self.pending_edit = Some(FavoriteEdit::Reorder {
                    order: state.original.clone(),
                    unpin: None,
                });
            }
            _ => {}
        }
        true
    }

    /// Swap two rows in the displayed order, for live feedback during a move.
    fn swap_rows(&mut self, a: usize, b: usize) {
        if a < self.options.len() && b < self.options.len() {
            self.options.swap(a, b);
        }
    }

    /// Put the keyboard back in the path field, for correcting a bad entry.
    pub fn focus_field(&mut self) {
        self.focus = PickerFocus::Field;
        self.input_is_suggestion = false;
        self.input_cursor = self.input_len();
    }

    /// Handle a raw key event for the dir picker's input field.
    /// Returns `Some(true)` to keep running, `Some(false)` to quit,
    /// or `None` if the key was not consumed (fall through to keybinding).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<bool> {
        use crossterm::event::KeyModifiers;

        // Tab moves the keyboard between the two halves. Checked first so it can
        // never be typed into the path.
        if key.code == KeyCode::Tab {
            self.toggle_focus();
            return Some(true);
        }

        // Ctrl combinations belong to the app (Ctrl+C to quit, and so on); never
        // absorb them into the field.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return None;
        }

        // Arrows always drive the list, whichever half has focus: they are
        // unambiguous, and they were the only way to browse before Tab existed.
        //
        // The first press from the path field *engages* the list at the row
        // already highlighted rather than stepping past it. The cursor starts at
        // index 0, so an unconditional step made arrowing down out of the field
        // land on the second entry and the first one unreachable that way. Tab
        // was unaffected, since it only moves focus.
        match key.code {
            KeyCode::Up | KeyCode::Down => {
                let engaging = self.focus == PickerFocus::Field;
                self.focus = PickerFocus::List;
                if engaging {
                    // Mirror the highlighted row into the field, which
                    // `select_*` would otherwise have done.
                    self.select(self.cursor);
                } else if key.code == KeyCode::Down {
                    self.select_next();
                } else {
                    self.select_prev();
                }
                return Some(true);
            }
            _ => {}
        }

        // A reorder in progress owns the keyboard: j/k slide the entry, Enter
        // commits, Esc puts it back. Everything else is ignored rather than
        // acted on half-way through a move.
        if self.moving.is_some() {
            return Some(self.handle_move_key(key.code));
        }

        // While searching, printable keys narrow the list rather than doing
        // whatever they normally would — otherwise typing "desktop" would fire
        // `d` (delete), `e` (edit), `p` (pin) and the rest along the way.
        if self.searching {
            match key.code {
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.recompute_visible();
                    return Some(true);
                }
                KeyCode::Backspace => {
                    // Backspacing past the start leaves search rather than
                    // sitting in an empty prompt that swallows every key.
                    if self.query.pop().is_none() {
                        self.searching = false;
                    }
                    self.recompute_visible();
                    return Some(true);
                }
                KeyCode::Esc => {
                    // Abandon the search and show everything again.
                    self.searching = false;
                    self.query.clear();
                    self.recompute_visible();
                    return Some(true);
                }
                // Enter accepts the filter and hands the keys back, leaving the
                // narrowed list in place to choose from.
                KeyCode::Enter => {
                    self.searching = false;
                    return Some(true);
                }
                // Arrows still move, so a match can be picked without leaving.
                KeyCode::Up => {
                    self.select_prev();
                    return Some(true);
                }
                KeyCode::Down => {
                    self.select_next();
                    return Some(true);
                }
                _ => return Some(true),
            }
        }

        if self.focus == PickerFocus::List {
            return match key.code {
                // Text-editing keys mean the user wants the field, so hand it
                // back rather than ignoring them. `End` after arrowing to an
                // entry is how you append a subdirectory to a listed path.
                KeyCode::Home | KeyCode::End | KeyCode::Left | KeyCode::Right => {
                    self.focus = PickerFocus::Field;
                    match key.code {
                        KeyCode::Home => self.input_cursor = 0,
                        KeyCode::End => {
                            self.input_is_suggestion = false;
                            self.input_cursor = self.input_len();
                        }
                        KeyCode::Left => self.input_left(),
                        _ => self.input_right(),
                    }
                    Some(true)
                }
                // vi motion over the list — what the screen has always claimed
                // these keys did, while the field was in fact swallowing them.
                KeyCode::Char('j') => {
                    self.select_next();
                    Some(true)
                }
                KeyCode::Char('k') => {
                    self.select_prev();
                    Some(true)
                }
                KeyCode::Char('g') => {
                    self.select_first();
                    Some(true)
                }
                KeyCode::Char('G') => {
                    self.select_last();
                    Some(true)
                }
                // Incremental search over everything on screen — but only when
                // the field is empty. Most typed paths start with `/`, and after
                // browsing the list the field holds a suggestion the user is
                // about to type over; hijacking that keystroke made absolute
                // paths impossible to enter from the list.
                //
                // Only on an empty field. `/` is how nearly every absolute path
                // starts, and browsing the list leaves a suggestion the user is
                // usually about to type over — taking that keystroke would make
                // "arrow to something nearby, then type the real path" impossible.
                // From a suggestion, one Backspace clears the field (it was never
                // typed) and `/` then searches.
                KeyCode::Char('/') if self.input.is_empty() => {
                    self.searching = true;
                    self.query.clear();
                    self.recompute_visible();
                    Some(true)
                }
                // Edit a saved host's details. Directories have nothing to edit
                // beyond their path, which `a` and `d` already cover.
                KeyCode::Char('e') => {
                    if let Some(opt) = self.selected() {
                        self.pending_edit = match &opt.host {
                            Some(host) => Some(FavoriteEdit::EditHost(host.label.clone())),
                            // A saved directory is edited as its path, so a typo
                            // or a moved directory can be corrected in place
                            // rather than deleted and re-added.
                            None if opt.is_favorite => {
                                Some(FavoriteEdit::EditDir(opt.path.clone()))
                            }
                            None => None,
                        };
                    }
                    Some(true)
                }
                // Pin the highlighted entry to the bottom of the pinned block,
                // or take it out of that block again.
                KeyCode::Char('p') => {
                    if let Some(opt) = self.selected() {
                        if opt.is_favorite && opt.tier != crate::hosts::DirTier::Pinned {
                            self.pending_edit = Some(FavoriteEdit::Pin(opt.path.clone()));
                        }
                    }
                    Some(true)
                }
                KeyCode::Char('u') => {
                    if let Some(opt) = self.selected() {
                        if opt.tier == crate::hosts::DirTier::Pinned {
                            self.pending_edit = Some(FavoriteEdit::Unpin(opt.path.clone()));
                        }
                    }
                    Some(true)
                }
                // Start a reorder.
                //
                // Only the pinned block has an order to arrange, so `m` on an
                // entry outside it pins that entry first and moves the new pin.
                // Requiring `p` beforehand made `m` do nothing at all on most
                // rows, with no feedback to say why — "move this" is a clear
                // enough request to act on.
                KeyCode::Char('m') => {
                    match self.selected() {
                        Some(opt) if opt.tier == crate::hosts::DirTier::Pinned => {
                            let path = opt.path.clone();
                            self.begin_move(path);
                        }
                        // A built-in location has nothing saved to pin; `a` adds
                        // it first. Everything else can be pinned in place.
                        Some(opt) if opt.is_favorite => {
                            let path = opt.path.clone();
                            self.pending_edit = Some(FavoriteEdit::PinAndMove(path));
                        }
                        _ => {}
                    }
                    Some(true)
                }
                // Save / forget, matching the dialing directory's a and d.
                // Only bound while the list has focus, so typing a path that
                // contains either letter is unaffected.
                KeyCode::Char('a') => {
                    self.pending_edit = Some(FavoriteEdit::PromptAdd);
                    Some(true)
                }
                KeyCode::Char('d') => {
                    if let Some(opt) = self.selected() {
                        match &opt.host {
                            // Removing a host is destructive enough to confirm,
                            // which the app does; a directory entry is only a
                            // shortcut, so it goes immediately.
                            Some(h) => {
                                self.pending_edit =
                                    Some(FavoriteEdit::DeleteHost(h.label.clone()))
                            }
                            None if opt.is_favorite => {
                                self.pending_edit = Some(FavoriteEdit::Remove(opt.path.clone()))
                            }
                            None => {}
                        }
                    }
                    Some(true)
                }
                // Anything else printable is the start of a path, so move the
                // keyboard to the field and take the character with it. Typing a
                // path is the picker's whole purpose; making that a no-op until
                // the user finds Tab would be its own bug.
                KeyCode::Char(c) => {
                    self.focus = PickerFocus::Field;
                    self.input_char(c);
                    Some(true)
                }
                KeyCode::Backspace => {
                    // Clearing a suggestion is discarding text the user never
                    // typed, so it is not the start of an edit and the keyboard
                    // stays with the list. Backspacing over something they *did*
                    // type is an edit, and hands the field back.
                    if !self.input_is_suggestion {
                        self.focus = PickerFocus::Field;
                    }
                    self.input_backspace();
                    Some(true)
                }
                // Enter and Esc are the app's (confirm / go back).
                _ => None,
            };
        }

        match key.code {
            KeyCode::Char(c) => {
                self.input_char(c);
                Some(true)
            }
            KeyCode::Backspace => {
                self.input_backspace();
                Some(true)
            }
            KeyCode::Delete => {
                self.input_delete();
                Some(true)
            }
            KeyCode::Left => {
                self.input_left();
                Some(true)
            }
            KeyCode::Right => {
                self.input_right();
                Some(true)
            }
            KeyCode::Home => {
                self.input_is_suggestion = false;
                self.input_cursor = 0;
                Some(true)
            }
            KeyCode::End => {
                self.input_is_suggestion = false;
                self.input_cursor = self.input_len();
                Some(true)
            }
            _ => None,
        }
    }
}

impl super::ScreenState for DirPickerState {
    fn cursor_down(&mut self) -> bool {
        self.select_next();
        true
    }

    fn cursor_up(&mut self) -> bool {
        self.select_prev();
        true
    }

    fn render(&mut self, frame: &mut Frame, area: Rect) {
        let vertical = Layout::vertical([Constraint::Length(3), Constraint::Length(3), Constraint::Min(1)]).split(area);

        // Title.
        let title = Paragraph::new(Span::styled(
            "Go to (Tab switches field/list, Esc to go back)",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ));
        frame.render_widget(title, vertical[0]);

        // Input field. The block text cursor is drawn only while the field has
        // focus — showing a caret in an unfocused box is what made it look like
        // typing would go there when j/k were in fact driving the list.
        let field_focused = self.focus == PickerFocus::Field;
        let input_line = if !field_focused {
            // Unfocused: show the value (or a hint) with no caret.
            if self.input.is_empty() {
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        "Tab or type to enter a path...",
                        Style::default().fg(Color::DarkGray),
                    ),
                ])
            } else {
                Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(self.input.clone(), Style::default().fg(Color::Yellow)),
                ])
            }
        } else if self.input.is_empty() {
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "Enter a path...",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        } else {
            // Split the value at the cursor and render a block glyph there.
            let cut = self
                .input
                .char_indices()
                .nth(self.input_cursor)
                .map(|(i, _)| i)
                .unwrap_or(self.input.len());
            let (before, after) = self.input.split_at(cut);
            Line::from(vec![
                Span::styled("> ", Style::default().fg(Color::Yellow)),
                Span::styled(before.to_string(), Style::default().fg(Color::Yellow)),
                Span::styled("█", Style::default().fg(Color::Yellow)),
                Span::styled(after.to_string(), Style::default().fg(Color::Yellow)),
            ])
        };
        // Focused pane gets the bright cyan border the main view uses for the
        // active panel; the other goes dark grey. Same visual language throughout.
        let (focused_border, unfocused_border) = (Color::Cyan, Color::DarkGray);
        let input_para = Paragraph::new(input_line).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if field_focused {
                    focused_border
                } else {
                    unfocused_border
                }))
                .title(if field_focused {
                    " Path (Enter to go) "
                } else {
                    " Path "
                }),
        );
        frame.render_widget(input_para, vertical[1]);

        // Option list, drawn from the visible rows so a search shows only what
        // it matched, with a header wherever the kind changes.
        let mut rendered: Vec<Line> = Vec::new();
        let mut last_section: Option<PickerSection> = None;
        for (row, &oi) in self.visible.iter().enumerate() {
            let opt = &self.options[oi];
            let section = opt.section();
            if last_section != Some(section) {
                last_section = Some(section);
                rendered.push(Line::from(Span::styled(
                    match section {
                        PickerSection::Directories => "  ── directories ──",
                        PickerSection::Hosts => "  ── remote hosts ──",
                    },
                    Style::default().fg(Color::DarkGray),
                )));
            }

            // The marker says which tier a row is in, so it is obvious which
            // rows `d` removes, `p` pins and `m` can reorder. A row being moved
            // shows a grip instead, for live feedback.
            let being_moved = self
                .moving
                .as_ref()
                .map(|m| m.path == opt.path)
                .unwrap_or(false);
            let mark = if being_moved {
                "⠿ "
            } else {
                match opt.tier {
                    // Single-width glyphs only: an emoji pin renders two columns
                    // wide and pushed pinned rows out of line with starred ones.
                    crate::hosts::DirTier::Pinned => "▲ ",
                    _ if opt.is_favorite => "★ ",
                    _ => "  ",
                }
            };
            let count = if opt.uses > 0 {
                format!("  ({})", opt.uses)
            } else {
                String::new()
            };
            // A host shows where it actually goes, not just its label — two
            // saved entries can easily carry the same short name.
            let detail = match &opt.host {
                Some(h) => format!("{}  {}", opt.label, h.to_url()),
                None => opt.label.clone(),
            };
            let text = format!("{}{}{}", mark, detail, count);

            rendered.push(if row == self.cursor {
                // Reversed only while the list is driving, so the highlight marks
                // "the keys move this" rather than just "last touched".
                let style = if field_focused {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::REVERSED)
                };
                Line::from(Span::styled(format!("> {}", text), style))
            } else if being_moved {
                Line::from(Span::styled(
                    format!("  {}", text),
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if opt.tier == crate::hosts::DirTier::Pinned {
                Line::from(Span::styled(
                    format!("  {}", text),
                    Style::default().fg(Color::Cyan),
                ))
            } else if opt.is_favorite {
                Line::from(Span::styled(
                    format!("  {}", text),
                    Style::default().fg(Color::Green),
                ))
            } else {
                Line::from(format!("  {}", text))
            });
        }
        if rendered.is_empty() {
            rendered.push(Line::from(Span::styled(
                if self.query.is_empty() {
                    "  (nothing saved yet — type a path, or press a)".to_string()
                } else {
                    format!("  (nothing matches '{}')", self.query)
                },
                Style::default().fg(Color::DarkGray),
            )));
        }
        let lines: Text = Text::from(rendered);

        let list = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if field_focused {
                    unfocused_border
                } else {
                    focused_border
                }))
                // Only claim j/k when they actually work. The title said "j/k to
                // navigate" unconditionally while the path field was swallowing
                // both keys.
                .title(if self.moving.is_some() {
                    " Moving — j/k reposition · Enter confirm · Esc cancel ".to_string()
                } else if self.searching {
                    format!(" Search: {}_  ({} shown, Esc clears) ", self.query, self.visible.len())
                } else if !self.query.is_empty() {
                    format!(" Filtered: {}  ({} shown, / to change) ", self.query, self.visible.len())
                } else if field_focused {
                    " Destinations (↑/↓, or Tab for j/k) ".to_string()
                } else {
                    " a save · d forget · e edit · p pin · m move · / search ".to_string()
                }),
        );
        frame.render_widget(list, vertical[2]);
    }
}

/// Expand a leading `~` to the user's home directory.
///
/// Only `~` alone and `~/…` expand. `~other` names another user's home, which
/// this does not resolve — silently treating it as the *current* user's would
/// open the wrong directory rather than reporting that it could not be found.
///
/// The remainder is joined manually rather than with `PathBuf::push`, which
/// *replaces* the buffer when handed an absolute path: stripping `~` from
/// `~/code` leaves `/code`, so pushing that discarded the home prefix entirely
/// and the path resolved against the filesystem root.
fn expand_tilde(path: &str) -> PathBuf {
    let Some(rest) = path.strip_prefix('~') else {
        return PathBuf::from(path);
    };
    // `~` or `~/…` only.
    if !rest.is_empty() && !rest.starts_with('/') {
        return PathBuf::from(path);
    }
    let Some(home) = std::env::var_os("HOME") else {
        return PathBuf::from(path);
    };
    let home = PathBuf::from(home);
    let rest = rest.trim_start_matches('/');
    if rest.is_empty() {
        home
    } else {
        home.join(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `f` with `HOME` set to `home`, restoring it afterwards.
    ///
    /// `set_var` is process-global, so these run under one mutex rather than in
    /// parallel — otherwise they would sabotage each other and any other test
    /// that reads `HOME`.
    fn with_home<T>(home: &str, f: impl FnOnce() -> T) -> T {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", home) };
        let out = f();
        match previous {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
        out
    }

    #[test]
    fn tilde_expands_to_the_home_directory() {
        // `PathBuf::push` *replaces* the buffer when given an absolute path, and
        // stripping `~` from `~/code` leaves `/code` — so the home prefix was
        // silently dropped and the path resolved against the filesystem root.
        // Bare `~` still worked, which is why this looked like partial support.
        with_home("/home/testuser", || {
            assert_eq!(
                expand_tilde("~/code"),
                PathBuf::from("/home/testuser/code"),
                "~/… must keep the home prefix"
            );
            assert_eq!(
                expand_tilde("~/code/untest"),
                PathBuf::from("/home/testuser/code/untest")
            );
            assert_eq!(expand_tilde("~"), PathBuf::from("/home/testuser"));
            assert_eq!(expand_tilde("~/"), PathBuf::from("/home/testuser"));
        });
    }

    #[test]
    fn only_a_leading_tilde_path_segment_expands() {
        with_home("/home/testuser", || {
            // An absolute or relative path is untouched.
            assert_eq!(expand_tilde("/etc"), PathBuf::from("/etc"));
            assert_eq!(expand_tilde("code"), PathBuf::from("code"));
            // A tilde inside the path is an ordinary character.
            assert_eq!(expand_tilde("/tmp/~backup"), PathBuf::from("/tmp/~backup"));
            // `~user` is another user's home, which this does not resolve — it
            // must not be mistaken for the current user's.
            assert_eq!(expand_tilde("~other/code"), PathBuf::from("~other/code"));
        });
    }
}

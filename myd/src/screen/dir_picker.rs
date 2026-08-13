use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use std::path::{Path, PathBuf};

/// Which pane of the picker has the keyboard.
///
/// The screen is a path field, a list *and* a panel of per-entry actions, so a
/// bare `j` is ambiguous. Rather than guess, focus is explicit and `Tab` cycles
/// it, matching how `Tab` already switches panels in the main view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerFocus {
    /// Typing edits the path; `j`/`k` are ordinary characters.
    Field,
    /// `j`/`k` walk the list, and any other printable character narrows it —
    /// the list is a thing you search, not a thing you type paths into.
    List,
    /// The actions available for the highlighted row; Enter runs one.
    Actions,
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
    /// The traversal mode a directory with no remembered preference will open
    /// in — the session default, which `-s` sets.
    ///
    /// Held here only to be *shown*: the app resolves the mode for real when
    /// Enter is pressed. A typed path used to give no clue which way it would
    /// go, so `myd -s -d` and `myd -d` looked identical right up until one of
    /// them started measuring.
    shallow_default: bool,
    /// Remembered per-directory preferences, keyed as the catalog stores them.
    ///
    /// The typed field can name any path, including one the list does not show,
    /// so answering "how will *this* open" needs the same lookup `Confirm`
    /// does rather than the highlighted row's flag.
    dir_prefs: std::collections::HashMap<String, bool>,
    /// Which row of the actions panel is highlighted.
    ///
    /// Indexes [`Self::actions`], which is derived from the highlighted entry
    /// and so changes under this cursor whenever the list moves — every read
    /// clamps rather than trusting it.
    action_cursor: usize,
}

/// One operation offered for the highlighted entry.
///
/// The panel replaced a title-bar legend of single letters. Those letters were
/// bound directly on the list, which is what made "type to search" impossible
/// there: `d` in "downloads" deleted an entry instead. Naming the operations
/// and giving them their own pane frees every printable key for the search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerAction {
    /// Open (or connect to) the highlighted entry.
    Go,
    /// Save a new directory to the list.
    Save,
    /// Forget the highlighted entry.
    Forget,
    /// Edit the entry: a host's details, or a directory's path.
    Edit,
    /// Add the entry to the pinned block.
    Pin,
    /// Take the entry out of the pinned block.
    Unpin,
    /// Reorder the entry within the pinned block.
    Move,
    /// Flip whether the directory is browsed without measuring sizes.
    Shallow,
}

impl PickerAction {
    /// The label drawn in the panel.
    pub fn label(self, opt: Option<&PickerOption>) -> String {
        match self {
            // "Go" is honest for a directory but not for a host, where Enter
            // dials rather than opens.
            PickerAction::Go => match opt {
                Some(o) if o.is_host() => "Connect".to_string(),
                _ => "Go".to_string(),
            },
            PickerAction::Save => "Save a directory".to_string(),
            PickerAction::Forget => "Forget".to_string(),
            PickerAction::Edit => "Edit".to_string(),
            PickerAction::Pin => "Pin".to_string(),
            PickerAction::Unpin => "Unpin".to_string(),
            PickerAction::Move => "Move".to_string(),
            // The label states the current setting, since the action is a
            // toggle and "Shallow" alone does not say which way it goes.
            PickerAction::Shallow => match opt {
                Some(o) if o.shallow => "Measure sizes".to_string(),
                _ => "Skip measuring".to_string(),
            },
        }
    }
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
    /// Flip whether this directory is browsed without measuring its sizes.
    ToggleShallow(PathBuf),
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
    /// Whether this directory is browsed without measuring its sizes.
    ///
    /// Shown on the row, so the toggle has something to point at — a preference
    /// that only takes effect on the next open is invisible otherwise.
    pub shallow: bool,
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
    /// One list of everywhere you might go. There was once a hosts-only variant
    /// behind `gs`, but `/` narrows to the hosts just as well, so the scope
    /// parameter went with the chord.
    pub fn with_catalog(catalog: &crate::hosts::HostCatalog) -> Self {
        let mut picker = Self::with_favorites(catalog.favorites());

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
                shallow: false,
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
                shallow: f.shallow,
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
                shallow: false,
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
            searching: false,
            query: String::new(),
            visible: Vec::new(),
            moving: None,
            pending_edit: None,
            shallow_default: false,
            dir_prefs: std::collections::HashMap::new(),
            action_cursor: 0,
        };
        // `visible` is what the cursor indexes and what render walks, so it has
        // to be populated before the picker is handed out — an empty one leaves
        // a list with rows that cannot be selected or drawn.
        picker.recompute_visible();
        picker
    }

    /// Tell the picker how a directory with no remembered preference will open,
    /// and what the remembered ones are, so it can show the mode before Enter.
    ///
    /// Pushed in by the app rather than read here: the catalog lives on the app,
    /// and the picker is deliberately unable to touch it.
    pub fn set_traversal_context(
        &mut self,
        shallow_default: bool,
        dir_prefs: std::collections::HashMap<String, bool>,
    ) {
        self.shallow_default = shallow_default;
        self.dir_prefs = dir_prefs;
    }

    /// How the current selection would open: `true` for shallow.
    ///
    /// Resolves exactly as `Action::Confirm` does — the directory's remembered
    /// preference if it has one, else the session default — so what the picker
    /// promises and what it does cannot drift apart.
    pub fn effective_shallow(&self) -> Option<bool> {
        let target = self.pending_target();
        if target.is_empty() {
            return None;
        }
        // A remote destination is a connect, not a local walk: it is never
        // measured, so claiming either mode for it would be a lie. A host row's
        // path *is* its URL, so this one check covers both a typed URL and a
        // highlighted host.
        if is_remote_url(target.trim()) {
            return None;
        }
        Some(self.dir_pref_for(&target).unwrap_or(self.shallow_default))
    }

    /// The path Enter would act on: what is typed, or the highlighted row.
    ///
    /// The same precedence [`Self::confirm`] applies, so the indicator describes
    /// the destination that will actually open.
    fn pending_target(&self) -> String {
        let typed = self.input.trim();
        if !typed.is_empty() {
            return expand_tilde(typed).to_string_lossy().to_string();
        }
        self.selected()
            .map(|o| o.path.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// A remembered preference for `path`, matching [`HostCatalog::dir_shallow_pref`]
    /// — the literal string, or its canonical form.
    fn dir_pref_for(&self, path: &str) -> Option<bool> {
        if let Some(v) = self.dir_prefs.get(path) {
            return Some(*v);
        }
        let canonical = std::fs::canonicalize(path)
            .ok()
            .map(|p| p.to_string_lossy().to_string())?;
        self.dir_prefs.get(&canonical).copied()
    }

    /// Rebuild the visible index list from the scope and the search query.
    fn recompute_visible(&mut self) {
        let query = self.query.to_lowercase();
        self.visible = self
            .options
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                query.is_empty() || o.search_text().to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len().saturating_sub(1);
        }
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

    /// The operations offered for the highlighted entry, in display order.
    ///
    /// Derived rather than stored: which ones apply depends entirely on what the
    /// cursor is on, and a stored list would go stale the moment the list moved
    /// or the catalog was rebuilt after an edit.
    ///
    /// Only applicable operations appear. Greying out the rest was the
    /// alternative, but a panel of mostly-dead rows makes the user work out why
    /// each one is dead; an absent row asks nothing.
    pub fn actions(&self) -> Vec<PickerAction> {
        // `Save` needs no selection — it prompts for a path — so it is the one
        // action an empty list still offers.
        let Some(opt) = self.selected() else {
            return vec![PickerAction::Save];
        };

        let mut out = vec![PickerAction::Go, PickerAction::Save];
        if opt.is_host() {
            // A host is a saved address: it can be edited and forgotten, but
            // pinning and the traversal toggle are directory notions.
            out.push(PickerAction::Edit);
            out.push(PickerAction::Forget);
            return out;
        }
        if opt.is_favorite {
            out.push(PickerAction::Edit);
            out.push(PickerAction::Forget);
            if opt.tier == crate::hosts::DirTier::Pinned {
                out.push(PickerAction::Unpin);
            } else {
                out.push(PickerAction::Pin);
            }
            // `m` pinned an unpinned entry first; the panel keeps that, so
            // "Move" is offered on any saved directory.
            out.push(PickerAction::Move);
            out.push(PickerAction::Shallow);
        }
        out
    }

    /// The highlighted action, if the panel has any rows.
    pub fn selected_action(&self) -> Option<PickerAction> {
        let actions = self.actions();
        actions.get(self.action_cursor.min(actions.len().saturating_sub(1))).copied()
    }

    /// Index of the highlighted action, clamped to what is currently offered.
    ///
    /// The list of actions changes as the entry under the list cursor changes,
    /// so a raw `action_cursor` can point past the end; every reader goes
    /// through this.
    pub fn action_cursor(&self) -> usize {
        let len = self.actions().len();
        self.action_cursor.min(len.saturating_sub(1))
    }

    fn action_next(&mut self) {
        let len = self.actions().len();
        if len > 0 {
            self.action_cursor = (self.action_cursor() + 1) % len;
        }
    }

    fn action_prev(&mut self) {
        let len = self.actions().len();
        if len > 0 {
            self.action_cursor = if self.action_cursor() == 0 {
                len - 1
            } else {
                self.action_cursor() - 1
            };
        }
    }

    /// Run the highlighted action. Returns `false` when the caller must handle
    /// it instead — `Go` is the app's `Confirm`, which this screen cannot do.
    fn run_action(&mut self) -> bool {
        let Some(action) = self.selected_action() else {
            return true;
        };
        // Cloned up front: each arm needs the row, and `pending_edit` borrows
        // `self` mutably.
        let opt = self.selected().cloned();
        match action {
            // Handed back unconsumed so the app's Confirm resolves it, which is
            // the one place that knows how to open a directory or dial a host.
            PickerAction::Go => return false,
            PickerAction::Save => self.pending_edit = Some(FavoriteEdit::PromptAdd),
            PickerAction::Forget => {
                if let Some(o) = opt {
                    self.pending_edit = match &o.host {
                        Some(h) => Some(FavoriteEdit::DeleteHost(h.label.clone())),
                        None if o.is_favorite => Some(FavoriteEdit::Remove(o.path.clone())),
                        None => None,
                    };
                }
            }
            PickerAction::Edit => {
                if let Some(o) = opt {
                    self.pending_edit = match &o.host {
                        Some(h) => Some(FavoriteEdit::EditHost(h.label.clone())),
                        None if o.is_favorite => Some(FavoriteEdit::EditDir(o.path.clone())),
                        None => None,
                    };
                }
            }
            PickerAction::Pin => {
                if let Some(o) = opt {
                    if o.is_favorite && o.tier != crate::hosts::DirTier::Pinned {
                        self.pending_edit = Some(FavoriteEdit::Pin(o.path.clone()));
                    }
                }
            }
            PickerAction::Unpin => {
                if let Some(o) = opt {
                    if o.tier == crate::hosts::DirTier::Pinned {
                        self.pending_edit = Some(FavoriteEdit::Unpin(o.path.clone()));
                    }
                }
            }
            PickerAction::Move => {
                // A reorder only has meaning inside the pinned block, so an
                // unpinned entry is pinned first and the new pin is what moves.
                // The keys go back to the list, which is where a move is driven.
                match opt {
                    Some(o) if o.tier == crate::hosts::DirTier::Pinned => {
                        self.focus = PickerFocus::List;
                        self.begin_move(o.path.clone());
                    }
                    Some(o) if o.is_favorite => {
                        self.pending_edit = Some(FavoriteEdit::PinAndMove(o.path.clone()));
                    }
                    _ => {}
                }
            }
            PickerAction::Shallow => {
                if let Some(o) = opt {
                    if !o.is_host() && o.is_favorite {
                        self.pending_edit = Some(FavoriteEdit::ToggleShallow(o.path.clone()));
                    }
                }
            }
        }
        true
    }


    /// Carry the keyboard state across a rebuild, so adding or removing a
    /// favourite does not dump the user back into the path field.
    pub fn adopt_focus_from(&mut self, other: &Self) {
        self.focus = other.focus;
        self.input.clone_from(&other.input);
        self.input_cursor = other.input_cursor;
        self.input_is_suggestion = other.input_is_suggestion;
        self.action_cursor = other.action_cursor;
        // The search survives too. Pinning from a narrowed list used to dump the
        // user back into the unfiltered one, so the entry they had just acted on
        // was somewhere else entirely by the time the screen came back.
        self.query.clone_from(&other.query);
        self.searching = other.searching;
        self.recompute_visible();
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

    /// Move the keyboard on to the next pane: field → list → actions → field.
    ///
    /// The actions belong to the highlighted entry, so they sit *after* the list
    /// in the cycle: you choose what to act on, then what to do to it.
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            PickerFocus::Field => PickerFocus::List,
            PickerFocus::List => {
                // Entering the panel starts at its first row rather than
                // wherever the last entry's panel happened to leave the cursor —
                // the lists differ per row, so a carried-over index points at an
                // unrelated action.
                self.action_cursor = 0;
                PickerFocus::Actions
            }
            PickerFocus::Actions => PickerFocus::Field,
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
        // Not while a search is narrowing the list. The field is what `confirm`
        // prefers, so mirroring into it mid-search would make Enter open the
        // suggestion rather than the row the user filtered down to — and the
        // field is drawn empty during a search, so the text would be invisible.
        if self.searching {
            return;
        }
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
            // A remote URL is a destination like any other, so the one field
            // takes both. Without this the picker was local-only and connecting
            // to an address you had not already saved needed a separate chord.
            if is_remote_url(typed) {
                return PickerChoice::Connect(typed.to_string());
            }
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
            // In the actions panel the arrows walk *it*, not the list: it is the
            // pane with focus, and moving the list from under it would swap the
            // actions out beneath the cursor.
            KeyCode::Up | KeyCode::Down if self.focus == PickerFocus::Actions => {
                if key.code == KeyCode::Down {
                    self.action_next();
                } else {
                    self.action_prev();
                }
                return Some(true);
            }
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

        // The actions panel: j/k (and the arrows, handled above) walk it, Enter
        // runs the highlighted operation. Nothing here searches — the panel is a
        // fixed handful of rows, and typing at it is almost certainly meant for
        // the list or the field.
        if self.focus == PickerFocus::Actions {
            return match key.code {
                KeyCode::Char('j') => {
                    self.action_next();
                    Some(true)
                }
                KeyCode::Char('k') => {
                    self.action_prev();
                    Some(true)
                }
                KeyCode::Enter => {
                    // `run_action` returns false for `Go`, which only the app can
                    // carry out; that falls through to `Action::Confirm`.
                    if self.run_action() {
                        Some(true)
                    } else {
                        None
                    }
                }
                // Esc is the app's: it backs out of the picker.
                _ => None,
            };
        }

        if self.focus == PickerFocus::List {
            return match key.code {
                // Mid-search, Home/End are the only way left to reach the ends
                // of the list in one press: `g`/`G` type into the query like
                // every other letter, and jumping to the field would abandon
                // the filter the user is in the middle of building.
                KeyCode::Home if self.searching || !self.query.is_empty() => {
                    self.select_first();
                    Some(true)
                }
                KeyCode::End if self.searching || !self.query.is_empty() => {
                    self.select_last();
                    Some(true)
                }
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
                // Typing narrows the list. Every printable character does, with
                // no `/` to press first: having tabbed to a list of places, what
                // else would typing mean? The operations that used to own these
                // letters live in the actions panel now, which is what freed
                // them — `d` in "downloads" filters rather than forgetting an
                // entry, and a path is still typed by tabbing back to the field.
                //
                // This costs the vi motions: `j` and `k` filter like anything
                // else, since a list you search with bare letters cannot also
                // reserve two of them. The arrows are handled above and move
                // regardless of focus, so the list is still navigable mid-search.
                KeyCode::Char(c) => {
                    self.searching = true;
                    self.query.push(c);
                    self.recompute_visible();
                    // The narrowed list is what Enter should act on, so the
                    // suggestion mirrored in from the previously highlighted row
                    // has to go: `confirm` prefers the field, and would
                    // otherwise open whatever was highlighted before the search.
                    self.input.clear();
                    self.input_cursor = 0;
                    self.input_is_suggestion = false;
                    Some(true)
                }
                KeyCode::Backspace => {
                    // Backspace unwinds the search a character at a time. Past
                    // the start it hands the field back, which is the only way
                    // Backspace can still mean "edit the path I typed".
                    if self.query.pop().is_none() {
                        self.searching = false;
                        self.focus = PickerFocus::Field;
                        self.input_backspace();
                    } else {
                        if self.query.is_empty() {
                            self.searching = false;
                        }
                        self.recompute_visible();
                    }
                    Some(true)
                }
                // Esc drops the filter and shows everything again. Only when
                // there is one: with no search running it is the app's, and
                // backs out of the picker.
                KeyCode::Esc if !self.query.is_empty() => {
                    self.searching = false;
                    self.query.clear();
                    self.recompute_visible();
                    Some(true)
                }
                // Enter on a search narrowed to exactly one row opens it: having
                // typed enough to leave a single candidate, being made to press
                // Enter again is ceremony. Otherwise it falls through to the
                // app's Confirm, which opens the highlighted row.
                KeyCode::Enter if self.searching && self.visible.len() == 1 => {
                    self.searching = false;
                    self.cursor = 0;
                    None
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
            "Go to (Tab cycles path/list/actions, Esc to go back)",
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
                        // No longer "or type": typing drives the list's search
                        // now, and Tab is the only way back to the field.
                        "Tab here to enter a path...",
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
                // The traversal mode rides in the title so it is visible before
                // Enter rather than inferred afterwards from whether sizes
                // appeared. Nothing is claimed for a remote target or an empty
                // field, where there is no local walk to describe.
                .title(match (field_focused, self.effective_shallow()) {
                    (true, Some(true)) => " Path (Enter to go) — shallow ".to_string(),
                    (true, Some(false)) => " Path (Enter to go) — full ".to_string(),
                    (true, None) => " Path (Enter to go) ".to_string(),
                    (false, Some(true)) => " Path — shallow ".to_string(),
                    (false, Some(false)) => " Path — full ".to_string(),
                    (false, None) => " Path ".to_string(),
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
                None if opt.shallow => format!("{}  (shallow)", opt.label),
                None => opt.label.clone(),
            };
            let text = format!("{}{}{}", mark, detail, count);

            rendered.push(if row == self.cursor {
                // Reversed while the list drives *or* while the actions panel
                // does: the panel acts on this row, so it has to stay visibly
                // the subject even though the keys have moved on to the panel.
                let style = if self.focus == PickerFocus::Field {
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

        // The list and its actions sit side by side: the panel acts on whatever
        // the list highlights, so the two have to be readable at once. The panel
        // is given a fixed share rather than a percentage so its labels do not
        // wrap on a narrow terminal, and the list keeps the rest.
        let columns =
            Layout::horizontal([Constraint::Min(20), Constraint::Length(24)]).split(vertical[2]);

        let list_focused = self.focus == PickerFocus::List;
        let list = Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if list_focused {
                    focused_border
                } else {
                    unfocused_border
                }))
                .title(if self.moving.is_some() {
                    " Moving — j/k reposition · Enter confirm · Esc cancel ".to_string()
                } else if self.searching {
                    format!(
                        " Search: {}_  ({} shown, Esc clears) ",
                        self.query,
                        self.visible.len()
                    )
                } else if !self.query.is_empty() {
                    format!(
                        " Filtered: {}  ({} shown, Esc clears) ",
                        self.query,
                        self.visible.len()
                    )
                } else if list_focused {
                    // Says what typing does, since that is the change: letters
                    // no longer fire commands here, they search.
                    " Destinations (type to search) ".to_string()
                } else {
                    " Destinations (↑/↓, or Tab) ".to_string()
                }),
        );
        frame.render_widget(list, columns[0]);

        // The actions panel: what Enter would do to the highlighted entry.
        let actions_focused = self.focus == PickerFocus::Actions;
        let selected_opt = self.selected().cloned();
        let actions = self.actions();
        let action_cursor = self.action_cursor();
        let action_lines: Vec<Line> = actions
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let label = a.label(selected_opt.as_ref());
                if actions_focused && i == action_cursor {
                    Line::from(Span::styled(
                        format!("> {}", label),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::REVERSED),
                    ))
                } else if actions_focused {
                    Line::from(Span::styled(
                        format!("  {}", label),
                        Style::default().fg(Color::Yellow),
                    ))
                } else {
                    // Dimmed while another pane drives: the panel is showing
                    // what is *available*, not what is about to happen.
                    Line::from(Span::styled(
                        format!("  {}", label),
                        Style::default().fg(Color::DarkGray),
                    ))
                }
            })
            .collect();

        let actions_panel = Paragraph::new(Text::from(action_lines)).block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Plain)
                .border_style(Style::default().fg(if actions_focused {
                    focused_border
                } else {
                    unfocused_border
                }))
                .title(if actions_focused {
                    " Actions (Enter runs) ".to_string()
                } else {
                    " Actions ".to_string()
                }),
        );
        frame.render_widget(actions_panel, columns[1]);
    }
}

/// Whether `s` names a remote target rather than a local path.
///
/// Only the two explicit schemes count. A bare `user@host:/path` is deliberately
/// *not* remote here: it is also a perfectly legal local filename, and guessing
/// wrong would turn a typo into a connection attempt to somewhere unintended.
/// The parse itself is left to `SftpTarget::parse`, which reports a bad URL far
/// better than a silent "not a directory" would.
fn is_remote_url(s: &str) -> bool {
    s.starts_with("sftp://") || s.starts_with("ssh://")
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
    fn a_typed_url_connects_whatever_it_ends_with() {
        // The trailing slash is the address people actually type. It must still
        // be recognised as a remote URL — not fall through to the local-path
        // branch and be reported as "not a directory" — and it must reach the
        // connect path verbatim, since SftpTarget::parse is the one place that
        // decides what a trailing slash means.
        for typed in ["sftp://gb10", "sftp://gb10/", "sftp://gb10//"] {
            let mut p = DirPickerState::new();
            p.input = typed.to_string();
            match p.confirm() {
                PickerChoice::Connect(url) => assert_eq!(url, typed, "passed through as typed"),
                other => panic!("{} should connect, got {:?}", typed, other),
            }
        }
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

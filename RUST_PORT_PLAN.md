# Rust Port Plan: File Browser TUI

## Project Overview

The Python project is a **vi-style terminal file browser** built with the Textual framework. It has:
- A directory picker startup screen
- A main screen with a file tree (left) and file info panel (right)
- Vi-like keybindings with chord detection (`dd`, `gg`, `G`, `gu`, `gd`)
- Proportional size bars next to each file/directory entry
- Sort modes (dirs-first, files-first, largest, smallest) with a size cache
- File/directory operations: delete, rename, search, change root
- Modal dialogs: confirm, input, progress overlay
- File info panel: type, size, permissions (`drwxr-xr-x`), owner/group, timestamps

## Crate Selection

| Python | Rust | Purpose |
|--------|------|---------|
| `textual` | **ratatui** + **crossterm** | TUI rendering + terminal control. `ratatui` is the actively-maintained successor to `tui-rs`. |
| `pathlib` | `std::path::Path` | Path manipulation (stdlib). |
| `os.walk` | **walkdir** | Recursive directory traversal. |
| (custom `format_size`) | **filesize** | Human-readable byte formatting (`"1.5 KB"`). |
| `pwd` / `grp` | **uname** | Owner/group name resolution from uid/gid on Unix. |
| `datetime` | `std::time::SystemTime` + **chrono** (optional) | Timestamp formatting. Stdlib may suffice. |
| `sys.argv` | **clap** | CLI argument parsing (`#[derive(Parser)]`). |
| `asyncio` (Textual workers) | `tokio` (background tasks) | Async directory enumeration / size computation. |
| (none -- manual) | **thiserror** + **anyhow** | Idiomatic error handling (`thiserror` for library errors, `anyhow` in `main`). |
| (none -- manual) | **tracing** | Structured logging / debug instrumentation. |
| (none -- manual) | **dashmap** | Concurrent hash map for shared mutable state (size cache). |
| `pytest` | **cargo-test** + **tempfile** | Native test framework. |

## Dependencies

```toml
[package]
name = "file-browser"
version = "0.1.0"
edition = "2021"

[dependencies]
ratatui = "0.29"
crossterm = "0.28"
clap = { version = "4", features = ["derive"] }
walkdir = "2"
filesize = "0.2"
uname = "0.1"
dashmap = "6"
tokio = { version = "1", features = ["full"] }
thiserror = "2"
anyhow = "1"
tracing = "0.1"

[dev-dependencies]
tempfile = "3"
```

## Crate Structure

```
file-browser/
├── Cargo.toml
├── src/
│   ├── main.rs          -- clap CLI entry, app launch
│   ├── app.rs           -- App struct: state machine, screen management, config
│   ├── screen/
│   │   ├── mod.rs       -- Screen enum, screen trait
│   │   ├── main.rs      -- MainScreen: file tree + info panel layout
│   │   └── dir_picker.rs -- DirPickerScreen: startup directory selection
│   ├── widget/
│   │   ├── mod.rs
│   │   ├── file_tree.rs  -- FileTree: tree data, render, vi navigation, chords
│   │   ├── file_info.rs  -- FileInfoPanel: stat rendering
│   │   ├── size_bar.rs   -- SizeBar: proportional bar (█/░)
│   │   ├── dialog.rs     -- ConfirmDialog, InputDialog modals
│   │   └── progress.rs   -- ProgressOverlay
│   ├── utils/
│   │   ├── mod.rs
│   │   └── sizes.rs      -- get_size, get_shallow_size, get_dir_size, cache
│   └── keybinding.rs     -- Chord detection, binding table, key event processing
├── tests/
│   └── integration.rs    -- Integration tests (temp dir fixtures)
└── README.md
```

## Key Architectural Decisions

### 1. Event Loop (replacing Textual's reactive model)

`ratatui` is a rendering library, not a reactive framework. The event loop is manual:

```rust
loop {
    select! {
        // crossterm events (key presses, resize)
        event = crossterm_event_stream.next() => handle_event(event),
        // tokio background task completions (directory load, size compute)
        result = background_task => handle_result(result),
    }
}
```

State transitions drive re-rendering. After any state mutation, call `terminal.draw(|f| render(f, &state))`.

### 2. Screen Management (replacing Textual's `Screen` / `push_screen`)

An enum with associated state, matching Textual's screen stack:

```rust
enum Screen {
    DirPicker(DirPickerState),
    Main(MainScreenState),
}

struct AppState {
    screen_stack: Vec<Screen>,
    config: AppConfig,
}
```

### 3. FileTree Data Structure (replacing `Tree<DirEntry>`)

A custom tree with lazy loading:

```rust
struct TreeNode {
    path: PathBuf,
    children: Option<Vec<TreeNode>>,  // None = not expanded yet
    is_expanded: bool,
}

struct FileTree {
    root: TreeNode,
    cursor: usize,           // flat line index (like Textual's cursor_line)
    sort_mode: SortMode,
    show_hidden: bool,
    show_size_bar: bool,
    size_cache: DashMap<PathBuf, u64>,  // concurrent hash map for size cache
    lines: Vec<TreeLine>,    // flattened, rendered lines
}
```

Use **dashmap** (`DashMap`) for the size cache instead of `parking_lot::Mutex<HashMap<..>>` -- it allows concurrent reads from background workers without lock contention.

### 4. Chord Detection (replacing manual `_chord_key` / `_chord_timer`)

```rust
enum ChordState {
    Idle,
    Waiting { key: Key, deadline: Instant },
}

struct KeyBindingHandler {
    chord: ChordState,
    timeout: Duration,  // 500ms
}
```

On each key event, check if the chord is still alive and complete, or start a new one.

### 5. Size Computation (replacing Textual `@work(thread=..)`)

Background size computation via `tokio::task::spawn` with `mpsc` channel for results:

```rust
// In file_tree.rs
let (tx, mut rx) = mpsc::channel::<(PathBuf, u64)>;

tokio::spawn(async move {
    for entry in dir_entries {
        let size = compute_size(&entry.path, sort_mode);
        tx.send((entry.path, size)).await;
    }
});

// In event loop
while let Some((path, size)) = rx.recv().await {
    tree.size_cache.insert(path, size);
    needs_render = true;
}
```

### 6. Modal Dialogs (replacing `ModalScreen`)

An overlay flag on the app state:

```rust
enum Modal {
    None,
    Confirm { message: String, result: Option<bool> },
    Input { message: String, placeholder: String, value: String, result: Option<String> },
    Progress,
}
```

When a modal is active, key events route to the modal handler instead of the screen.

### 7. Permissions Formatting (replacing `_format_permissions`)

Use bit operations with `std::os::unix::fs::PermissionsExt`:

```rust
fn format_permissions(mode: u32) -> String {
    let file_type = match mode & libc::S_IFMT {
        libc::S_IFDIR => 'd',
        libc::S_IFLNK => 'l',
        _ => '-',
    };
    // rwx bits for owner/group/other...
}
```

## Module-by-Module Port Plan

**Phase 1: Foundation** -- `Cargo.toml`, `main.rs`, `app.rs`, event loop skeleton, `clap` setup

**Phase 2: FileTree** -- `widget/file_tree.rs` with tree data, lazy load, sort, flatten-to-lines, render via `ratatui::Widget` trait

**Phase 3: Navigation** -- `keybinding.rs` with chord detection, vi bindings (`j`/`k`/`h`/`l`/`gg`/`G`/`dd`/`R`/`s`/`H`/`b`/`/`)

**Phase 4: Size system** -- `utils/sizes.rs` with `filesize` crate, `walkdir` for recursive, `DashMap` cache, background workers

**Phase 5: Info panel** -- `widget/file_info.rs` with `std::fs::Metadata`, `uname` for owner/group, `chrono` for timestamps

**Phase 6: Layout** -- `screen/main.rs` with `ratatui::Layout` (horizontal split: 70/30), `ratatui::widgets::{Block, Borders, Scrollbar}`

**Phase 7: Dialogs** -- `widget/dialog.rs` (confirm + input modals as overlay widgets using `ratatui::Position::centered`)

**Phase 8: Dir picker** -- `screen/dir_picker.rs` with common directory list

**Phase 9: File operations** -- delete (`std::fs::remove_file` / `remove_dir_all`), rename (`std::fs::rename`), search (flat line scan)

**Phase 10: Tests** -- integration tests with `tempfile` crate, mirroring the existing pytest test coverage

## Rust Idioms to Apply

| Python pattern | Rust idiom |
|---|---|
| `enum StrEnum` | `#[derive(Debug, Clone, Copy, PartialEq)] enum` + `#[derive(strum::Display)]` |
| `reactive` attributes | Manual dirty-flag re-render after state mutation |
| `ModalScreen[T]` | `enum Modal` with `Result<T>` variant |
| `@work(thread)` | `tokio::spawn` + `mpsc::Receiver` |
| `path.stat().st_size` | `path.metadata().unwrap().len()` |
| `os.walk` | `walkdir::WalkDir` |
| `pwd.getpwuid` | `uname::get_username_by_uid` |
| `grp.getgrgid` | `uname::get_group_by_gid` |
| `sys.argv` | `clap::Parser` derive macro |
| `format_size` | `filesize::filesize()` |
| `dict[Path, int]` cache | `dashmap::DashMap<PathBuf, u64>` |
| `Binding(key, action)` | `match key => Action::Variant` |
| `pytest tmp_path` | `tempfile::TempDir` |
| exception handling | `Result<T, AppError>` + `?` operator + `thiserror` |

## Progress Tracking

- [ ] Phase 1: Foundation
- [ ] Phase 2: FileTree
- [ ] Phase 3: Navigation
- [ ] Phase 4: Size system
- [ ] Phase 5: Info panel
- [ ] Phase 6: Layout
- [ ] Phase 7: Dialogs
- [ ] Phase 8: Dir picker
- [ ] Phase 9: File operations
- [ ] Phase 10: Tests

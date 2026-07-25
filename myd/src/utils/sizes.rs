use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use walkdir::WalkDir;

/// A cooperative cancellation flag shared between the UI and a background
/// directory walk. The walk checks it periodically and bails out early, so a
/// scan the user has abandoned stops promptly instead of running to completion.
#[derive(Clone, Debug, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation. Any walk observing this token will stop soon after.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }
}

/// Shared, concurrent size cache: resolved path -> size in bytes.
///
/// Cloning shares the underlying map rather than copying it, so a subtree
/// opened from a parent directory reuses the sizes already computed instead of
/// rescanning the disk. Call `clear` to force a rescan.
///
/// Keyed on `PathBuf` rather than `String` so a lookup can borrow a `&Path`.
/// Stringifying the key allocated on *every* read, and reads are the hot path:
/// re-sorting or reflattening a tree does one per line, plus one per sort key.
#[derive(Clone, Debug, Default)]
pub struct SizeCache {
    inner: std::sync::Arc<DashMap<PathBuf, u64>>,
}

impl SizeCache {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(DashMap::new()),
        }
    }

    pub fn get(&self, path: &Path) -> Option<u64> {
        self.inner.get(path).map(|v| *v.value())
    }

    pub fn insert(&self, path: &Path, size: u64) {
        self.inner.insert(path.to_path_buf(), size);
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

}

/// Format bytes into a human-readable string (e.g., "  1.5 KB").
/// Returns a fixed-width string: number right-justified in 8 chars,
/// unit right-justified in 2 chars = 10 chars total for clean alignment.
pub fn format_size(size_bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = size_bytes as f64;
    let mut unit_index = 0usize;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    let unit = UNITS[unit_index]; // "B"(1 char) or "KB"/"MB"/...(2 chars)
    if unit_index == 0 {
        format!("{:>4} B", size_bytes)
    } else {
        format!("{:>4.1}{}", size, unit)
    }
}

/// Check if a path is on a virtual (pseudo) filesystem.
/// Files on virtual filesystems report meaningless sizes (e.g., /proc/kcore
/// reports the size of physical memory + swap).
fn is_virtual_fs(path: &Path) -> bool {
    // Check the resolved (canonical) path.
    let p = path.canonicalize().ok().unwrap_or(path.to_path_buf());
    p.starts_with("/proc") || p.starts_with("/sys") || p.starts_with("/dev")
}

/// Get size of a single file via metadata.
/// Returns 0 for files on virtual filesystems (where metadata().len() is meaningless).
pub fn get_file_size(path: &Path) -> u64 {
    if is_virtual_fs(path) {
        return 0;
    }
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Get shallow size of a directory (only direct children).
pub fn get_shallow_size(path: &Path) -> u64 {
    if is_virtual_fs(path) {
        return 0;
    }
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if is_virtual_fs(&entry_path) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Recursively compute directory size (like `du`).
pub fn get_dir_size(path: &Path) -> u64 {
    if is_virtual_fs(path) {
        return 0;
    }
    let mut total: u64 = 0;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        // Skip virtual filesystem entries (files and directories).
        if is_virtual_fs(entry.path()) {
            continue;
        }
        if entry.file_type().is_file() {
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Recursively compute a directory's size, recording the size of every
/// directory encountered along the way into `cache`.
///
/// Measuring a directory already requires visiting all of its descendants, so
/// the per-directory subtotals come for free. Populating them here means
/// opening a subdirectory later is a cache hit rather than a second walk over
/// the same bytes.
pub fn get_dir_size_caching(path: &Path, cache: &SizeCache) -> u64 {
    get_dir_size_caching_cancellable(path, cache, &CancelToken::new())
}

/// As [`get_dir_size_caching_cancellable`], but also reports each visited entry
/// to `progress` (files, dirs, and bytes) so a scan can show a live count.
pub fn get_dir_size_caching_cancellable_progress(
    path: &Path,
    cache: &SizeCache,
    cancel: &CancelToken,
    progress: Option<&crate::widget::progress::OpProgress>,
) -> u64 {
    get_dir_size_inner(path, cache, cancel, progress)
}

/// As [`get_dir_size_caching`], but abandons the walk when `cancel` is tripped.
///
/// On cancellation the partial totals gathered so far are discarded and 0 is
/// returned — the caller is expected to throw away the whole scan.
pub fn get_dir_size_caching_cancellable(
    path: &Path,
    cache: &SizeCache,
    cancel: &CancelToken,
) -> u64 {
    get_dir_size_inner(path, cache, cancel, None)
}

fn get_dir_size_inner(
    path: &Path,
    cache: &SizeCache,
    cancel: &CancelToken,
    progress: Option<&crate::widget::progress::OpProgress>,
) -> u64 {
    if is_virtual_fs(path) {
        return 0;
    }

    // `contents_first` yields every child before its parent, so a directory's
    // running total is complete by the time we reach it. Totals are keyed by
    // path and folded into the parent as each directory closes.
    use std::collections::HashMap;
    let mut totals: HashMap<std::path::PathBuf, u64> = HashMap::new();
    let mut root_total: u64 = 0;

    // Checking an atomic on every entry is cheap next to the per-entry syscalls,
    // so cancellation is observed within a handful of directory entries.
    for entry in WalkDir::new(path)
        .contents_first(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if cancel.is_cancelled() {
            return 0;
        }
        if is_virtual_fs(entry.path()) {
            continue;
        }

        let size = if entry.file_type().is_dir() {
            // Children have already contributed their totals.
            let total = totals.remove(entry.path()).unwrap_or(0);
            cache.insert(entry.path(), total);
            if let Some(p) = progress {
                p.add_dir();
            }
            total
        } else if entry.file_type().is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            cache.insert(entry.path(), size);
            if let Some(p) = progress {
                p.add_file(size);
            }
            size
        } else {
            // Symlinks and other node types contribute nothing.
            0
        };

        match entry.path().parent() {
            Some(parent) if entry.path() != path => {
                *totals.entry(parent.to_path_buf()).or_insert(0) += size;
            }
            _ => root_total = size,
        }
    }

    root_total
}

/// Recursively compute directory size with a timeout.
/// Returns 0 if the traversal takes longer than the deadline.
pub fn get_dir_size_timeout(path: &Path, timeout: std::time::Duration) -> u64 {
    if is_virtual_fs(path) {
        return 0;
    }
    let deadline = std::time::Instant::now() + timeout;
    let mut total: u64 = 0;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if std::time::Instant::now() > deadline {
            break;
        }
        if is_virtual_fs(entry.path()) {
            continue;
        }
        if entry.file_type().is_file() {
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Get size of a file or directory.
pub fn get_size(path: &Path, recursive: bool) -> u64 {
    if path.is_file() {
        return get_file_size(path);
    }
    if recursive {
        return get_dir_size(path);
    }
    get_shallow_size(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "   0 B");
        assert_eq!(format_size(500), " 500 B");
        assert_eq!(format_size(1024), " 1.0KB");
        assert_eq!(format_size(1536), " 1.5KB");
        assert_eq!(format_size(1_048_576), " 1.0MB");
    }

    #[test]
    fn test_get_file_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("test.txt");
        std::fs::write(&file, "hello").unwrap();
        assert_eq!(get_file_size(&file), 5);
    }

    #[test]
    fn test_get_shallow_size() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "12345").unwrap();
        std::fs::write(dir.path().join("b.txt"), "abcde").unwrap();
        assert_eq!(get_shallow_size(dir.path()), 10);
    }

    #[test]
    fn test_cache() {
        let cache = SizeCache::new();
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "xyz").unwrap();

        assert!(cache.get(&file).is_none());
        cache.insert(&file, 3);
        assert_eq!(cache.get(&file), Some(3));
    }

    #[test]
    fn test_cancelled_walk_returns_early() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/a.bin"), vec![0u8; 1000]).unwrap();

        // A token cancelled before the walk starts yields 0 and caches nothing.
        let cancel = CancelToken::new();
        cancel.cancel();
        let cache = SizeCache::new();
        assert_eq!(
            get_dir_size_caching_cancellable(dir.path(), &cache, &cancel),
            0
        );
        assert_eq!(cache.len(), 0, "a cancelled walk should record no sizes");
    }

    #[test]
    fn test_uncancelled_cancellable_walk_matches_plain() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/f.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("a/b/g.bin"), vec![0u8; 400]).unwrap();

        let cache = SizeCache::new();
        let cancel = CancelToken::new(); // never tripped
        let total = get_dir_size_caching_cancellable(dir.path(), &cache, &cancel);
        assert_eq!(total, 500);
        assert_eq!(total, get_dir_size(dir.path()));
    }
}

use dashmap::DashMap;
use std::path::Path;
use walkdir::WalkDir;

/// Shared, concurrent size cache: resolved Path string -> size in bytes.
#[derive(Clone, Debug, Default)]
pub struct SizeCache {
    inner: DashMap<String, u64>,
}

impl SizeCache {
    pub fn new() -> Self {
        Self {
            inner: DashMap::new(),
        }
    }

    pub fn get(&self, path: &Path) -> Option<u64> {
        let key = Self::cache_key(path);
        self.inner.get(&key).map(|v| *v.value())
    }

    pub fn insert(&self, path: &Path, size: u64) {
        let key = Self::cache_key(path);
        self.inner.insert(key, size);
    }

    pub fn clear(&self) {
        self.inner.clear();
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    fn cache_key(path: &Path) -> String {
        path.canonicalize()
            .ok()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string_lossy().to_string())
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

/// Get size of a single file via metadata.
pub fn get_file_size(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Get shallow size of a directory (only direct children).
pub fn get_shallow_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Recursively compute directory size (like `du`).
pub fn get_dir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if let Ok(metadata) = entry.metadata() {
            if metadata.is_file() {
                total += metadata.len();
            }
        }
    }
    total
}

/// Recursively compute directory size with a timeout.
/// Returns 0 if the traversal takes longer than the deadline.
pub fn get_dir_size_timeout(path: &Path, timeout: std::time::Duration) -> u64 {
    let deadline = std::time::Instant::now() + timeout;
    let mut total: u64 = 0;
    for entry in WalkDir::new(path).into_iter().filter_map(|e| e.ok()) {
        if std::time::Instant::now() > deadline {
            // Timeout — return what we have so far.
            break;
        }
        if let Ok(metadata) = entry.metadata() {
            if metadata.is_file() {
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
}

use std::path::{Path, PathBuf};

/// Which backend a path belongs to: an index into the app's
/// [`BackendRegistry`](super::BackendRegistry).
///
/// Backend 0 is always the local filesystem, so `BackendId::default()` is local
/// and code paths that predate remote support keep working unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct BackendId(pub usize);

impl BackendId {
    /// The local filesystem, which is always registered first.
    pub const LOCAL: BackendId = BackendId(0);

    /// Whether this is the local filesystem backend.
    #[inline]
    pub fn is_local(&self) -> bool {
        self.0 == Self::LOCAL.0
    }
}

/// A path plus the backend that owns it.
///
/// The tree stores plain `PathBuf`s per node and one `BackendId` per panel, so
/// this pairing is only materialised where an operation needs to know *which*
/// filesystem to talk to — chiefly transfers, which routinely have a source and
/// destination on different backends.
///
/// Remote paths are always absolute, `/`-separated, and never canonicalized:
/// resolving symlinks remotely would cost a round trip per node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VPath {
    pub backend: BackendId,
    pub path: PathBuf,
}

impl VPath {
    pub fn new(backend: BackendId, path: impl Into<PathBuf>) -> Self {
        Self {
            backend,
            path: path.into(),
        }
    }

    /// A path on the local filesystem.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Self::new(BackendId::LOCAL, path)
    }

    #[inline]
    pub fn is_local(&self) -> bool {
        self.backend.is_local()
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// The final component, for display.
    pub fn file_name(&self) -> Option<&str> {
        self.path.file_name().and_then(|s| s.to_str())
    }

    /// Append a component, staying on the same backend.
    pub fn join(&self, name: impl AsRef<Path>) -> Self {
        Self {
            backend: self.backend,
            path: self.path.join(name),
        }
    }

    /// The parent directory, staying on the same backend.
    pub fn parent(&self) -> Option<Self> {
        self.path.parent().map(|p| Self {
            backend: self.backend,
            path: p.to_path_buf(),
        })
    }

    /// Re-root this path from `base` onto `dest` — the mapping a recursive copy
    /// applies to each entry it walks, possibly crossing backends.
    pub fn rebase(&self, base: &VPath, dest: &VPath) -> Option<Self> {
        let rel = self.path.strip_prefix(&base.path).ok()?;
        Some(dest.join(rel))
    }
}

impl std::fmt::Display for VPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Remote paths are prefixed so the UI can never present a remote path as
        // if it were local.
        if self.is_local() {
            write!(f, "{}", self.path.display())
        } else {
            write!(f, "remote:{}", self.path.display())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_backend_is_zero_and_default() {
        assert!(BackendId::default().is_local());
        assert!(VPath::local("/tmp").is_local());
    }

    #[test]
    fn join_and_parent_preserve_backend() {
        let remote = VPath::new(BackendId(3), "/srv");
        let child = remote.join("data");
        assert_eq!(child.backend, BackendId(3));
        assert_eq!(child.path, PathBuf::from("/srv/data"));
        assert_eq!(child.parent().unwrap(), remote);
    }

    #[test]
    fn rebase_maps_across_backends() {
        let base = VPath::new(BackendId(1), "/remote/src");
        let dest = VPath::local("/local/dest");
        let entry = VPath::new(BackendId(1), "/remote/src/a/b.txt");

        let mapped = entry.rebase(&base, &dest).unwrap();
        assert!(mapped.is_local());
        assert_eq!(mapped.path, PathBuf::from("/local/dest/a/b.txt"));
    }

    #[test]
    fn rebase_rejects_unrelated_path() {
        let base = VPath::local("/a");
        let dest = VPath::local("/b");
        assert!(VPath::local("/elsewhere/f").rebase(&base, &dest).is_none());
    }

    #[test]
    fn display_marks_remote_paths() {
        assert_eq!(VPath::local("/tmp/f").to_string(), "/tmp/f");
        assert_eq!(
            VPath::new(BackendId(1), "/tmp/f").to_string(),
            "remote:/tmp/f"
        );
    }
}

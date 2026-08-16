use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::Path;

use super::{VEntry, VMetadata, VPath, VRead, VWrite, Vfs};
use crate::utils::sizes::{self, CancelToken, SizeCache};
use crate::widget::progress::OpProgress;

/// The local filesystem, always registered as backend 0.
///
/// This wraps the `std::fs` calls the widgets previously made directly, so
/// routing them through the trait changes no behavior. Blocking calls are moved
/// onto the blocking pool, matching how `screen::loading_with_cache` already
/// offloads directory scans.
#[derive(Debug, Default, Clone)]
pub struct LocalFs;

impl LocalFs {
    pub fn new() -> Self {
        Self
    }
}

/// Build a [`VEntry`] from a directory entry, reusing the metadata the OS
/// already handed us rather than issuing a second `stat`.
fn entry_from_dir_entry(entry: &std::fs::DirEntry) -> VEntry {
    let path = entry.path();
    let name = entry.file_name().to_string_lossy().to_string();

    // `file_type` on a DirEntry is free on Linux (it comes from the readdir
    // result); `symlink_metadata` is the fallback when it isn't available.
    let file_type = entry.file_type().ok();
    let is_symlink = file_type.map(|t| t.is_symlink()).unwrap_or(false);

    // For a symlink, report the target's directory-ness so expanding a symlinked
    // directory behaves as it did before (`Path::is_dir` follows links).
    let is_dir = match file_type {
        Some(t) if t.is_symlink() => path.is_dir(),
        Some(t) => t.is_dir(),
        None => path.is_dir(),
    };

    let meta = entry.metadata().ok();
    VEntry {
        name,
        is_dir,
        is_symlink,
        len: meta.as_ref().map(|m| m.len()).unwrap_or(0),
        mtime: meta.as_ref().and_then(|m| m.modified().ok()),
        atime: meta.as_ref().and_then(|m| m.accessed().ok()),
        mode: meta.as_ref().map(mode_of),
        uid: meta.as_ref().map(uid_of),
        gid: meta.as_ref().map(gid_of),
    }
}

#[cfg(unix)]
fn mode_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.mode()
}
#[cfg(not(unix))]
fn mode_of(_m: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn uid_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.uid()
}
#[cfg(not(unix))]
fn uid_of(_m: &std::fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn gid_of(m: &std::fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    m.gid()
}
#[cfg(not(unix))]
fn gid_of(_m: &std::fs::Metadata) -> u32 {
    0
}

/// Convert `std::fs::Metadata` into the backend-neutral form.
pub fn vmetadata_from(meta: &std::fs::Metadata) -> VMetadata {
    VMetadata {
        is_dir: meta.is_dir(),
        is_symlink: meta.file_type().is_symlink(),
        len: meta.len(),
        mode: Some(mode_of(meta)),
        uid: Some(uid_of(meta)),
        gid: Some(gid_of(meta)),
        mtime: meta.modified().ok(),
        atime: meta.accessed().ok(),
        ctime: meta.created().ok(),
    }
}

/// Read a directory synchronously. Kept separate so the blocking-pool wrapper
/// and any synchronous caller share one implementation.
pub fn read_dir_blocking(dir: &Path) -> Result<Vec<VEntry>> {
    let mut out = Vec::new();
    let rd = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?;
    for entry in rd.flatten() {
        out.push(entry_from_dir_entry(&entry));
    }
    Ok(out)
}

#[async_trait]
impl Vfs for LocalFs {
    fn scheme(&self) -> &'static str {
        "file"
    }

    fn display_name(&self) -> String {
        "local".to_string()
    }

    async fn read_dir(&self, path: &VPath) -> Result<Vec<VEntry>> {
        let dir = path.path.clone();
        tokio::task::spawn_blocking(move || read_dir_blocking(&dir)).await?
    }

    async fn stat(&self, path: &VPath) -> Result<VMetadata> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            let meta =
                std::fs::metadata(&p).with_context(|| format!("failed to stat {}", p.display()))?;
            Ok(vmetadata_from(&meta))
        })
        .await?
    }

    async fn symlink_stat(&self, path: &VPath) -> Result<VMetadata> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&p)
                .with_context(|| format!("failed to stat {}", p.display()))?;
            Ok(vmetadata_from(&meta))
        })
        .await?
    }

    async fn create_dir_all(&self, path: &VPath) -> Result<()> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&p)
                .with_context(|| format!("failed to create directory {}", p.display()))
        })
        .await?
    }

    async fn remove_file(&self, path: &VPath) -> Result<()> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::remove_file(&p).with_context(|| format!("failed to remove {}", p.display()))
        })
        .await?
    }

    async fn remove_dir(&self, path: &VPath) -> Result<()> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            std::fs::remove_dir(&p)
                .with_context(|| format!("failed to remove directory {}", p.display()))
        })
        .await?
    }

    async fn rename(&self, from: &VPath, to: &VPath) -> Result<()> {
        let (f, t) = (from.path.clone(), to.path.clone());
        tokio::task::spawn_blocking(move || {
            std::fs::rename(&f, &t)
                .with_context(|| format!("failed to rename {} to {}", f.display(), t.display()))
        })
        .await?
    }

    async fn set_mode(&self, path: &VPath, mode: u32) -> Result<()> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                // Only the permission bits are ours to set: `mode` comes from a
                // dialog that parsed `644` or `rw-r--r--`, neither of which can
                // express the file type, and passing the whole word through
                // would clear the setuid and sticky bits a directory may rely
                // on. Masked to 0o7777 so setuid/setgid/sticky survive when the
                // user does type them.
                let perms = std::fs::Permissions::from_mode(mode & 0o7777);
                std::fs::set_permissions(&p, perms)
                    .with_context(|| format!("failed to set permissions on {}", p.display()))
            }
            #[cfg(not(unix))]
            {
                let _ = (p, mode);
                anyhow::bail!("permissions can only be changed on unix")
            }
        })
        .await?
    }

    async fn set_owner(&self, path: &VPath, uid: Option<u32>, gid: Option<u32>) -> Result<()> {
        let p = path.path.clone();
        tokio::task::spawn_blocking(move || {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                // `chown` takes -1 for "leave this one alone", which is what
                // lets owner and group be set independently in one call.
                let uid = uid.unwrap_or(u32::MAX);
                let gid = gid.unwrap_or(u32::MAX);
                let c_path = std::ffi::CString::new(p.as_os_str().as_bytes())
                    .with_context(|| format!("{} is not a valid path", p.display()))?;
                // `lchown`, not `chown`: following a symlink here would change
                // the owner of whatever it points at, which may be outside the
                // tree entirely — the same reasoning that makes the recursive
                // walk use `symlink_stat`.
                let rc = unsafe { libc::lchown(c_path.as_ptr(), uid, gid) };
                if rc != 0 {
                    let err = std::io::Error::last_os_error();
                    return Err(anyhow::Error::new(err)
                        .context(format!("failed to change owner of {}", p.display())));
                }
                Ok(())
            }
            #[cfg(not(unix))]
            {
                let _ = (p, uid, gid);
                anyhow::bail!("ownership can only be changed on unix")
            }
        })
        .await?
    }

    async fn open_read(&self, path: &VPath) -> Result<Box<dyn VRead>> {
        let file = tokio::fs::File::open(&path.path)
            .await
            .with_context(|| format!("failed to open {}", path.path.display()))?;
        Ok(Box::new(file))
    }

    async fn open_write(&self, path: &VPath, _len_hint: Option<u64>) -> Result<Box<dyn VWrite>> {
        // Create the parent so a transfer into a fresh subtree works, matching
        // the existing `copy_path` behavior.
        //
        // A failure is not returned: the directory may already exist, and the
        // `File::create` below produces the better message either way. It is
        // logged, though — an unwritable parent is the usual cause of the
        // "permission denied" that follows, and silently dropping it left the
        // real reason invisible.
        if let Some(parent) = path.path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                tracing::debug!(
                    parent = %parent.display(),
                    error = %e,
                    "could not create the local destination directory"
                );
            }
        }
        let file = tokio::fs::File::create(&path.path)
            .await
            .with_context(|| format!("failed to create {}", path.path.display()))?;
        Ok(Box::new(file))
    }

    async fn dir_size(
        &self,
        path: &VPath,
        cache: &SizeCache,
        cancel: &CancelToken,
        progress: Option<&OpProgress>,
    ) -> u64 {
        let p = path.path.clone();
        let (cache, cancel, progress) = (cache.clone(), cancel.clone(), progress.cloned());
        tokio::task::spawn_blocking(move || {
            sizes::get_dir_size_caching_cancellable_progress(&p, &cache, &cancel, progress.as_ref())
        })
        .await
        .unwrap_or(0)
    }

    fn has_recursive_sizes(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    #[test]
    fn read_dir_reports_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello").unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let fs = LocalFs::new();
        let entries = rt()
            .block_on(fs.read_dir(&VPath::local(dir.path())))
            .unwrap();

        assert_eq!(entries.len(), 2);
        let file = entries.iter().find(|e| e.name == "a.txt").unwrap();
        assert!(!file.is_dir);
        assert_eq!(file.len, 5);
        assert!(entries.iter().find(|e| e.name == "sub").unwrap().is_dir);
    }

    #[test]
    fn stat_reports_size_and_kind() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.bin");
        std::fs::write(&f, vec![0u8; 128]).unwrap();

        let fs = LocalFs::new();
        let meta = rt().block_on(fs.stat(&VPath::local(&f))).unwrap();
        assert_eq!(meta.len, 128);
        assert!(!meta.is_dir);
        assert!(meta.mode.is_some());
    }

    #[test]
    fn stat_missing_path_is_an_error_not_a_panic() {
        let fs = LocalFs::new();
        assert!(rt()
            .block_on(fs.stat(&VPath::local("/definitely/not/here")))
            .is_err());
    }

    #[test]
    fn create_remove_and_rename_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFs::new();
        let rt = rt();

        let nested = VPath::local(dir.path().join("a/b/c"));
        rt.block_on(fs.create_dir_all(&nested)).unwrap();
        assert!(nested.path.is_dir());

        let src = VPath::local(dir.path().join("one.txt"));
        std::fs::write(&src.path, "x").unwrap();
        let dst = VPath::local(dir.path().join("two.txt"));
        rt.block_on(fs.rename(&src, &dst)).unwrap();
        assert!(!src.path.exists() && dst.path.exists());

        rt.block_on(fs.remove_file(&dst)).unwrap();
        assert!(!dst.path.exists());

        rt.block_on(fs.remove_dir(&nested)).unwrap();
        assert!(!nested.path.exists());
    }

    #[test]
    fn open_write_creates_parents_and_round_trips_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let fs = LocalFs::new();
        let rt = rt();
        let target = VPath::local(dir.path().join("deep/nested/out.bin"));

        rt.block_on(async {
            let mut w = fs.open_write(&target, Some(3)).await.unwrap();
            w.write_all(b"abc").await.unwrap();
            w.flush().await.unwrap();
            drop(w);

            let mut r = fs.open_read(&target).await.unwrap();
            let mut buf = Vec::new();
            r.read_to_end(&mut buf).await.unwrap();
            assert_eq!(buf, b"abc");
        });
    }

    #[test]
    fn dir_size_is_recursive_and_matches_helper() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.path().join("sub/b.bin"), vec![0u8; 400]).unwrap();

        let fs = LocalFs::new();
        let cache = SizeCache::new();
        let size = rt().block_on(fs.dir_size(
            &VPath::local(dir.path()),
            &cache,
            &CancelToken::new(),
            None,
        ));

        assert_eq!(size, 500);
        assert!(fs.has_recursive_sizes());
    }

    #[test]
    fn dir_size_respects_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();

        let cancel = CancelToken::new();
        cancel.cancel();

        let fs = LocalFs::new();
        let size =
            rt().block_on(fs.dir_size(&VPath::local(dir.path()), &SizeCache::new(), &cancel, None));
        // An already-cancelled walk reports nothing rather than running to
        // completion.
        assert_eq!(size, 0);
    }
}

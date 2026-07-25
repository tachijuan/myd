//! Filesystem operations that work on any [`Vfs`] backend.
//!
//! The `Vfs` trait deliberately exposes only primitives — a single-level
//! `remove_dir`, a rename that cannot cross backends. The operations a user
//! actually asks for (delete a directory tree, move a file to another host) are
//! built here, once, so local and remote panels behave identically.

use anyhow::{bail, Result};
use std::sync::Arc;

use crate::transfer::{run_transfer, TransferConfig, TransferId, TransferJob, TransferProgress};
use crate::utils::sizes::CancelToken;
use crate::vfs::{VPath, Vfs};
use crate::widget::progress::OpProgress;

/// Recursively delete `path`, whatever it is.
///
/// Directories are emptied depth-first, because neither backend's `remove_dir`
/// removes a non-empty directory (SFTP's RMDIR fails outright, and the local
/// one is `std::fs::remove_dir`). Bumps `progress` once per entry removed so a
/// large tree reports live progress.
pub fn delete_recursive<'a>(
    fs: &'a Arc<dyn Vfs>,
    path: &'a VPath,
    progress: Option<&'a OpProgress>,
    cancel: &'a CancelToken,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
    Box::pin(async move {
        if cancel.is_cancelled() {
            return Ok(());
        }

        // Ask about the link itself, not its target: deleting a symlink must
        // unlink it, never recurse into (or delete) what it points at.
        let meta = fs.symlink_stat(path).await?;
        if meta.is_dir && !meta.is_symlink {
            // Children first — the directory must be empty before it can go.
            let entries = fs.read_dir(path).await?;
            for entry in entries {
                if cancel.is_cancelled() {
                    return Ok(());
                }
                delete_recursive(fs, &path.join(&entry.name), progress, cancel).await?;
            }
            fs.remove_dir(path).await?;
        } else {
            fs.remove_file(path).await?;
        }

        if let Some(p) = progress {
            p.inc_done();
        }
        Ok(())
    })
}

/// Count the entries under `path` (itself included), for a progress total.
///
/// Best-effort: a directory that fails to list counts as one entry rather than
/// aborting the operation it is sizing.
pub fn count_entries<'a>(
    fs: &'a Arc<dyn Vfs>,
    path: &'a VPath,
    cancel: &'a CancelToken,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = u64> + Send + 'a>> {
    Box::pin(async move {
        if cancel.is_cancelled() {
            return 0;
        }
        let Ok(meta) = fs.symlink_stat(path).await else {
            return 1;
        };
        if !meta.is_dir || meta.is_symlink {
            return 1;
        }
        let Ok(entries) = fs.read_dir(path).await else {
            return 1;
        };
        let mut total = 1;
        for entry in entries {
            total += count_entries(fs, &path.join(&entry.name), cancel).await;
        }
        total
    })
}

/// How a move was carried out — useful for messages and for tests that need to
/// tell the cheap path from the expensive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Same backend: a rename, which only rewrites directory entries.
    Rename,
    /// Different backends: a copy followed by deleting the source.
    CopyThenDelete,
}

/// Move `src` to `dest`, choosing the cheapest correct strategy.
///
/// Within one backend this is a rename: the data never moves, so it is instant
/// regardless of size — the same reason `mv` is instant inside a filesystem.
/// Across backends there is no such shortcut, so the bytes are copied and the
/// source is removed only after the copy has fully succeeded. A failed copy
/// leaves the source untouched.
pub async fn move_path(
    src_fs: &Arc<dyn Vfs>,
    dest_fs: &Arc<dyn Vfs>,
    src: &VPath,
    dest: &VPath,
    progress: Option<&OpProgress>,
    cancel: &CancelToken,
) -> Result<MoveKind> {
    // Never overwrite: `rename` replaces the destination silently on both
    // backends, so a move onto an existing name would destroy it with no
    // confirmation and no way back. The caller decides what to do instead.
    if dest_fs.symlink_stat(dest).await.is_ok() {
        bail!(
            "'{}' already exists at the destination",
            dest.path.display()
        );
    }

    if src.backend == dest.backend {
        // Same filesystem: just relink it.
        if let Some(parent) = dest.parent() {
            dest_fs.create_dir_all(&parent).await.ok();
        }
        src_fs.rename(src, dest).await?;
        if let Some(p) = progress {
            p.inc_done();
        }
        return Ok(MoveKind::Rename);
    }

    // Different backends: copy the bytes, then remove the source. The delete is
    // deliberately last and conditional — losing the source to a half-finished
    // copy is the one outcome a move must never produce.
    let transfer = TransferProgress::new(0);
    run_transfer(TransferJob {
        id: TransferId(0),
        src_fs: src_fs.clone(),
        dest_fs: dest_fs.clone(),
        src: src.clone(),
        dest: dest.clone(),
        progress: Arc::new(transfer),
        cancel: cancel.clone(),
        config: TransferConfig::default(),
    })
    .await?;

    if cancel.is_cancelled() {
        // Cancelled mid-copy: the destination is incomplete, so keep the source.
        bail!("move cancelled before the source could be removed");
    }

    delete_recursive(src_fs, src, progress, cancel).await?;
    Ok(MoveKind::CopyThenDelete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::{BackendId, LocalFs};

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Runtime::new().unwrap()
    }

    fn local() -> Arc<dyn Vfs> {
        Arc::new(LocalFs::new())
    }

    #[test]
    fn deletes_a_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(root.join("a/b/c")).unwrap();
        std::fs::write(root.join("a/f1.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/f2.txt"), "y").unwrap();
        std::fs::write(root.join("a/b/c/f3.txt"), "z").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        rt().block_on(delete_recursive(
            &fs,
            &VPath::local(&root),
            None,
            &cancel,
        ))
        .expect("delete failed");

        assert!(!root.exists(), "the tree should be gone");
    }

    #[test]
    fn deleting_a_symlink_leaves_its_target_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("real");
        std::fs::create_dir(&target_dir).unwrap();
        std::fs::write(target_dir.join("keep.txt"), "important").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target_dir, &link).unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        rt().block_on(delete_recursive(&fs, &VPath::local(&link), None, &cancel))
            .expect("delete failed");

        assert!(!link.exists(), "the link should be gone");
        assert!(
            target_dir.join("keep.txt").exists(),
            "deleting a symlink must not touch what it points at"
        );
    }

    #[test]
    fn same_backend_move_is_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("from.txt");
        let dest = dir.path().join("sub/to.txt");
        std::fs::write(&src, "payload").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        let kind = rt()
            .block_on(move_path(
                &fs,
                &fs,
                &VPath::local(&src),
                &VPath::local(&dest),
                None,
                &cancel,
            ))
            .expect("move failed");

        assert_eq!(kind, MoveKind::Rename);
        assert!(!src.exists(), "source should be gone");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "payload");
    }

    #[test]
    fn cross_backend_move_copies_then_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("from.bin");
        let dest = dir.path().join("to.bin");
        let payload = vec![3u8; 64 * 1024];
        std::fs::write(&src, &payload).unwrap();

        // Two distinct backend ids over the same local filesystem: enough to
        // drive the cross-backend path, since routing keys on the id.
        let fs = local();
        let cancel = CancelToken::new();
        let kind = rt()
            .block_on(move_path(
                &fs,
                &fs,
                &VPath::new(BackendId(0), &src),
                &VPath::new(BackendId(1), &dest),
                None,
                &cancel,
            ))
            .expect("move failed");

        assert_eq!(kind, MoveKind::CopyThenDelete);
        assert!(!src.exists(), "source should be removed after a good copy");
        assert_eq!(std::fs::read(&dest).unwrap(), payload);
    }

    #[test]
    fn counts_every_entry_in_a_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("t");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/1.txt"), "1").unwrap();
        std::fs::write(root.join("a/2.txt"), "2").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        // root + a + 2 files
        assert_eq!(
            rt().block_on(count_entries(&fs, &VPath::local(&root), &cancel)),
            4
        );
    }
}

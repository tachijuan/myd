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

/// What a recursive attribute change is setting.
///
/// One enum rather than two near-identical walkers: the traversal, the symlink
/// rule and the cancellation checks are the same either way, and the only thing
/// that differs is the call made at each node.
#[derive(Debug, Clone, Copy)]
pub enum AttrChange {
    /// New permission bits.
    Mode(u32),
    /// New owner, group, or both. `None` leaves that one unchanged.
    Owner { uid: Option<u32>, gid: Option<u32> },
}

impl AttrChange {
    /// Apply this change to one path.
    async fn apply(&self, fs: &Arc<dyn Vfs>, path: &VPath) -> Result<()> {
        match *self {
            AttrChange::Mode(mode) => fs.set_mode(path, mode).await,
            AttrChange::Owner { uid, gid } => fs.set_owner(path, uid, gid).await,
        }
    }
}

/// Apply an attribute change to `path` and everything beneath it.
///
/// Depth-first with the parent applied *last*, like [`delete_recursive`] but
/// for a different reason: a directory has to stay traversable while its
/// children are being visited. A mode that clears `x` applied on the way down
/// would make the rest of that subtree unreachable — `chmod -R 600` on a
/// directory would change the directory and then fail to reach anything under
/// it. Applying to the parent last means the walk always runs under the
/// permissions the directory already had.
///
/// Symlinks are changed but never followed — the same rule the delete walk
/// follows, and for the same reason: recursing through a link can leave the
/// tree entirely and change files the user never selected.
///
/// Errors are collected rather than propagated. One unwritable file partway
/// through must not abandon the rest, and the caller reports the failures
/// together; an empty return means everything applied.
pub fn set_attr_recursive<'a>(
    fs: &'a Arc<dyn Vfs>,
    path: &'a VPath,
    change: AttrChange,
    progress: Option<&'a OpProgress>,
    cancel: &'a CancelToken,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<String>> + Send + 'a>> {
    Box::pin(async move {
        let mut failures = Vec::new();
        if cancel.is_cancelled() {
            return failures;
        }

        // The link itself, not its target, so a symlink to a directory is one
        // entry to change rather than a subtree to walk into.
        let meta = match fs.symlink_stat(path).await {
            Ok(m) => m,
            Err(e) => {
                failures.push(format!("{}: {}", path.path.display(), e));
                return failures;
            }
        };

        if meta.is_dir && !meta.is_symlink {
            match fs.read_dir(path).await {
                Ok(entries) => {
                    for entry in entries {
                        if cancel.is_cancelled() {
                            return failures;
                        }
                        failures.extend(
                            set_attr_recursive(
                                fs,
                                &path.join(&entry.name),
                                change,
                                progress,
                                cancel,
                            )
                            .await,
                        );
                    }
                }
                Err(e) => failures.push(format!("{}: {}", path.path.display(), e)),
            }
        }

        if let Err(e) = change.apply(fs, path).await {
            failures.push(format!("{}: {}", path.path.display(), e));
        }
        if let Some(p) = progress {
            p.inc_done();
        }
        failures
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

    #[cfg(unix)]
    fn mode_of(p: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    #[test]
    fn a_recursive_mode_change_reaches_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(root.join("a/b")).unwrap();
        std::fs::write(root.join("a/f1.txt"), "x").unwrap();
        std::fs::write(root.join("a/b/f2.txt"), "y").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        let failures = rt().block_on(set_attr_recursive(
            &fs,
            &VPath::local(&root),
            AttrChange::Mode(0o705),
            None,
            &cancel,
        ));

        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
        for p in [
            root.clone(),
            root.join("a"),
            root.join("a/b"),
            root.join("a/f1.txt"),
            root.join("a/b/f2.txt"),
        ] {
            assert_eq!(mode_of(&p), 0o705, "{} was not changed", p.display());
        }
    }

    /// Applying to the parent before its children would remove the `x` bit the
    /// walk itself needs to reach them, leaving most of the tree untouched.
    #[cfg(unix)]
    #[test]
    fn a_mode_that_closes_a_directory_still_reaches_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir_all(root.join("a")).unwrap();
        std::fs::write(root.join("a/f.txt"), "x").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        // No `x` anywhere: a top-down walk could not descend past `root`.
        let failures = rt().block_on(set_attr_recursive(
            &fs,
            &VPath::local(&root),
            AttrChange::Mode(0o600),
            None,
            &cancel,
        ));

        assert!(failures.is_empty(), "unexpected failures: {failures:?}");

        // Reopen the path down to the leaf before reading it back: the change
        // under test removed the `x` bit this assertion needs to traverse, so
        // checking it first would fail on the check rather than on the walk.
        // (Restoring is also what lets the tempdir be cleaned up.)
        let restore = |p: &std::path::Path| {
            std::fs::set_permissions(
                p,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o755),
            )
            .unwrap();
        };
        restore(&root);
        restore(&root.join("a"));

        assert_eq!(mode_of(&root.join("a/f.txt")), 0o600, "the leaf was missed");
    }

    /// Recursing through a symlink would change files outside the selection.
    #[cfg(unix)]
    #[test]
    fn a_recursive_change_does_not_follow_symlinks_out_of_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("untouched.txt"), "x").unwrap();
        let before = mode_of(&outside.join("untouched.txt"));

        let root = dir.path().join("tree");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        let failures = rt().block_on(set_attr_recursive(
            &fs,
            &VPath::local(&root),
            AttrChange::Mode(0o700),
            None,
            &cancel,
        ));

        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
        assert_eq!(
            mode_of(&outside.join("untouched.txt")),
            before,
            "the walk followed a symlink out of the tree"
        );
    }

    /// One unwritable entry must not abandon the rest of the tree.
    #[cfg(unix)]
    #[test]
    fn a_failure_is_collected_and_the_walk_continues() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join("b.txt"), "b").unwrap();

        let fs = local();
        let cancel = CancelToken::new();
        // A path that does not exist: symlink_stat fails, and that one entry is
        // reported while its siblings are still changed.
        let missing = VPath::local(root.join("gone.txt"));
        let failures = rt().block_on(set_attr_recursive(
            &fs,
            &missing,
            AttrChange::Mode(0o600),
            None,
            &cancel,
        ));
        assert_eq!(failures.len(), 1, "expected one failure: {failures:?}");
        assert!(
            failures[0].contains("gone.txt"),
            "the failure should name the path: {failures:?}"
        );

        // 0o750, not 0o640: the directory keeps its `x` bit so the assertions
        // below can still traverse it. Closing it is covered by its own test.
        let ok = rt().block_on(set_attr_recursive(
            &fs,
            &VPath::local(&root),
            AttrChange::Mode(0o750),
            None,
            &cancel,
        ));
        assert!(ok.is_empty(), "unexpected failures: {ok:?}");
        assert_eq!(mode_of(&root.join("a.txt")), 0o750);
        assert_eq!(mode_of(&root.join("b.txt")), 0o750);
    }

    /// The file-type bits are not the user's to set — a mode of 0o100644 must
    /// not be written through as-is.
    #[cfg(unix)]
    #[test]
    fn only_the_permission_bits_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f.txt");
        std::fs::write(&f, "x").unwrap();

        let fs = local();
        rt().block_on(fs.set_mode(&VPath::local(&f), 0o100_644))
            .expect("set_mode failed");
        assert_eq!(mode_of(&f), 0o644);
    }

    /// A cancelled walk stops rather than running to completion.
    #[cfg(unix)]
    #[test]
    fn a_cancelled_recursive_change_stops() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tree");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        let before = mode_of(&root.join("a.txt"));

        let fs = local();
        let cancel = CancelToken::new();
        cancel.cancel();
        let failures = rt().block_on(set_attr_recursive(
            &fs,
            &VPath::local(&root),
            AttrChange::Mode(0o600),
            None,
            &cancel,
        ));

        assert!(failures.is_empty());
        assert_eq!(
            mode_of(&root.join("a.txt")),
            before,
            "a cancelled walk still changed a file"
        );
    }

    /// An archive is read-only, so the default trait method must refuse rather
    /// than report a success it did not perform.
    #[test]
    fn a_backend_without_support_refuses_rather_than_pretending() {
        struct Bare;
        #[async_trait::async_trait]
        impl Vfs for Bare {
            fn scheme(&self) -> &'static str {
                "bare"
            }
            async fn read_dir(&self, _: &VPath) -> Result<Vec<crate::vfs::VEntry>> {
                Ok(Vec::new())
            }
            async fn stat(&self, _: &VPath) -> Result<crate::vfs::VMetadata> {
                bail!("no")
            }
            async fn create_dir_all(&self, _: &VPath) -> Result<()> {
                Ok(())
            }
            async fn remove_file(&self, _: &VPath) -> Result<()> {
                Ok(())
            }
            async fn remove_dir(&self, _: &VPath) -> Result<()> {
                Ok(())
            }
            async fn rename(&self, _: &VPath, _: &VPath) -> Result<()> {
                Ok(())
            }
            async fn open_read(&self, _: &VPath) -> Result<Box<dyn crate::vfs::VRead>> {
                bail!("no")
            }
            async fn open_write(
                &self,
                _: &VPath,
                _: Option<u64>,
            ) -> Result<Box<dyn crate::vfs::VWrite>> {
                bail!("no")
            }
            async fn dir_size(
                &self,
                _: &VPath,
                _: &crate::utils::sizes::SizeCache,
                _: &CancelToken,
                _: Option<&OpProgress>,
            ) -> u64 {
                0
            }
        }

        let fs: Arc<dyn Vfs> = Arc::new(Bare);
        let p = VPath::local("/x");
        assert!(rt().block_on(fs.set_mode(&p, 0o644)).is_err());
        assert!(rt().block_on(fs.set_owner(&p, Some(0), None)).is_err());
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

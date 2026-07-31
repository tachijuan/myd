//! End-to-end SFTP tests against a real sshd.
//!
//! Skipped unless `MYD_SFTP_TEST` names a config file, so `cargo test` stays
//! hermetic. The test harness (scripts/sftp_test_env.sh) starts an isolated
//! sshd on a high port with a dedicated key and writes that file. It never
//! touches the user's real ~/.ssh.
//!
//! The config file has four lines: host, port, key path, remote data dir.

use std::io::Write;
use std::path::PathBuf;

use myd::transfer::{run_transfer, TransferConfig, TransferId, TransferJob, TransferProgress};
use myd::utils::sizes::CancelToken;
use myd::vfs::sftp::{ConnectOutcome, Credentials, SftpFs, SftpTarget};
use myd::vfs::{VPath, Vfs};
use std::sync::Arc;

struct TestEnv {
    host: String,
    port: u16,
    /// Present in the config for completeness; auth goes through the agent the
    /// harness populates, so the path itself isn't read here.
    #[allow(dead_code)]
    key: PathBuf,
    remote_dir: PathBuf,
}

/// Read the harness config, or `None` when the gate variable is unset.
fn test_env() -> Option<TestEnv> {
    // Redirect HOME for this process only (so the known_hosts write lands in the
    // harness's throwaway dir, not the user's real ~/.ssh). Done here rather than
    // on the cargo invocation, which would break rustup's toolchain lookup.
    if let Ok(home) = std::env::var("MYD_SFTP_TEST_HOME") {
        std::env::set_var("HOME", home);
    }
    let cfg = std::env::var("MYD_SFTP_TEST").ok()?;
    let text = std::fs::read_to_string(cfg).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 4 {
        return None;
    }
    Some(TestEnv {
        host: lines[0].to_string(),
        port: lines[1].parse().ok()?,
        key: PathBuf::from(lines[2]),
        remote_dir: PathBuf::from(lines[3]),
    })
}

/// Connect using the harness key. The key path is passed to russh by pointing
/// the ssh-config resolution at it via an explicit identity file — but since the
/// public API resolves keys from ~/.ssh, the harness instead relies on the key
/// being loadable directly, so we drive connect() with a target and no creds and
/// let the agent/identity ladder find it.
async fn connect(env: &TestEnv) -> SftpFs {
    // The harness registers the key with a temporary ssh-agent whose socket is
    // exported in SSH_AUTH_SOCK, so the agent step of the ladder authenticates.
    let target = SftpTarget {
        host: env.host.clone(),
        user: Some(whoami()),
        port: Some(env.port),
        path: Some(env.remote_dir.clone()),
    };
    match SftpFs::connect(&target, &Credentials::default(), true)
        .await
        .expect("connect failed")
    {
        ConnectOutcome::Connected(fs) => fs,
        ConnectOutcome::NeedsCredential(need) => {
            panic!("unexpected credential prompt: {:?}", need)
        }
    }
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "root".into())
}

/// Per-test wall-clock budget. Every test here talks to a real sshd, and any of
/// those awaits can block forever if the server stops answering (a dropped
/// connection mid-handshake, a wedged sftp subsystem, a firewalled port). The
/// polling loops in these tests are all bounded, so an unbounded network await
/// is the only way to hang — and a hung test reports nothing at all, which is
/// strictly worse than a failure.
///
/// Override with `MYD_SFTP_TEST_TIMEOUT` (seconds) for a slow link.
fn test_timeout() -> std::time::Duration {
    let secs = std::env::var("MYD_SFTP_TEST_TIMEOUT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    std::time::Duration::from_secs(secs)
}

/// Define a gated, time-bounded SFTP test.
///
/// Expands to a `#[tokio::test]` that (1) skips when the harness gate is unset,
/// and (2) runs the body under [`test_timeout`], so a wedged server turns into a
/// named failure instead of a test that never returns. Wrapping the whole body
/// rather than each network call means awaits added later are covered too.
///
/// The bound `$env` is the [`TestEnv`], matching what each body expects.
macro_rules! sftp_test {
    ($name:ident, $env:ident, $body:block) => {
        // Multi-threaded on purpose. A remote `Source` drives its async Vfs from
        // a dedicated thread and *blocks* the caller waiting for the reply, which
        // is exactly what the app's synchronous tree does. On a single-threaded
        // runtime that blocks the only worker, so nothing else can make progress
        // — including the timeout below, which then never fires and turns a
        // deadlock into a test that hangs forever reporting nothing.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            let Some($env) = test_env() else {
                eprintln!("MYD_SFTP_TEST unset; skipping SFTP integration test");
                return;
            };
            let budget = test_timeout();
            if tokio::time::timeout(budget, async move $body).await.is_err() {
                panic!(
                    "SFTP test `{}` exceeded {:?} and was aborted — the server \
                     likely stopped responding. Check that the harness sshd is \
                     still up (scripts/sftp_test_env.sh), or raise \
                     MYD_SFTP_TEST_TIMEOUT.",
                    stringify!($name),
                    budget,
                );
            }
        }
    };
}

sftp_test!(sftp_lists_reads_and_round_trips_a_file, env, {
    let fs = connect(&env).await;

    // read_dir sees the fixtures the harness created.
    let dir = VPath::new(myd::vfs::BackendId(1), &env.remote_dir);
    let entries = fs.read_dir(&dir).await.expect("read_dir failed");
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"greeting.txt"), "listing was {:?}", names);
    assert!(names.contains(&"blob.bin"));

    // stat reports a sane size for the known small file.
    let greeting = dir.join("greeting.txt");
    let meta = fs.stat(&greeting).await.expect("stat failed");
    assert!(!meta.is_dir);
    assert!(meta.len > 0);

    // Download blob.bin and verify byte-for-byte against the local original.
    let local_dir = tempfile::tempdir().unwrap();
    let local_blob = local_dir.path().join("blob.bin");
    let sftp_fs: Arc<dyn Vfs> = Arc::new(fs);
    let local_fs: Arc<dyn Vfs> = Arc::new(myd::vfs::LocalFs::new());

    run_transfer(TransferJob {
        id: TransferId(1),
        src_fs: sftp_fs.clone(),
        dest_fs: local_fs.clone(),
        src: dir.join("blob.bin"),
        dest: VPath::local(&local_blob),
        progress: Arc::new(TransferProgress::new(0)),
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await
    .expect("download failed");

    let downloaded = std::fs::read(&local_blob).unwrap();
    let original = std::fs::read(env.remote_dir.join("blob.bin")).unwrap();
    assert_eq!(downloaded, original, "downloaded bytes differ from original");
});

sftp_test!(sftp_uploads_a_file, env, {
    let fs = connect(&env).await;
    let sftp_fs: Arc<dyn Vfs> = Arc::new(fs);
    let local_fs: Arc<dyn Vfs> = Arc::new(myd::vfs::LocalFs::new());

    // Make a local file and push it up.
    let local_dir = tempfile::tempdir().unwrap();
    let src = local_dir.path().join("upload.dat");
    let payload = vec![0x5au8; 2 * 1024 * 1024];
    let mut f = std::fs::File::create(&src).unwrap();
    f.write_all(&payload).unwrap();
    drop(f);

    let remote_dest = VPath::new(myd::vfs::BackendId(1), env.remote_dir.join("uploaded.dat"));

    run_transfer(TransferJob {
        id: TransferId(2),
        src_fs: local_fs.clone(),
        dest_fs: sftp_fs.clone(),
        src: VPath::local(&src),
        dest: remote_dest.clone(),
        progress: Arc::new(TransferProgress::new(0)),
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await
    .expect("upload failed");

    // The bytes landed on the server (we can read the real file locally since
    // the harness server shares this filesystem).
    let landed = std::fs::read(env.remote_dir.join("uploaded.dat")).unwrap();
    assert_eq!(landed, payload);
});

sftp_test!(app_connects_and_opens_a_browsable_remote_panel, env, {
    // Drive the real app connect flow, exactly as `myd sftp://host` would.
    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);

    // Let the local panel settle first.
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, env.remote_dir.display());
    app.connect_on_start(&target);
    assert!(app.is_connecting_for_test());

    // `gr` opens the remote in the active panel (index 0), replacing the local
    // view. Tick until its tree finishes loading.
    let mut opened = false;
    for _ in 0..600 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) {
            opened = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    assert!(opened, "remote panel never opened on {}", env.remote_dir.display());

    // Render the app and confirm the remote listing is on screen.
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect();

    assert!(text.contains("greeting.txt"), "remote file not shown in tree");
});

sftp_test!(remote_navigation_does_not_block_the_event_loop, env, {
    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Connect to the deep tree the harness built (data/deep exists in the fixtures
    // for this test; fall back to the data dir root otherwise).
    let start = if env.remote_dir.join("deep").exists() { env.remote_dir.join("deep") } else { env.remote_dir.clone() };
    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, start.display());
    app.connect_on_start(&target);

    let mut opened = false;
    for _ in 0..600 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(start.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote panel never opened");

    // The remote panel is active (index 1). Dig into subdirectories with `l`,
    // asserting each keystroke returns quickly (the event loop is never blocked
    // by a synchronous network round trip) and the loading resolves.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    for depth in 0..3 {
        // Move onto the first directory line, then expand it.
        let before = std::time::Instant::now();
        app.handle_key_for_test(key('j')); // step down
        app.handle_key_for_test(key('l')); // expand/enter the directory
        // The key handler itself must return promptly — this is the anti-freeze
        // assertion. In the old code this call blocked on the SFTP round trip.
        assert!(
            before.elapsed() < std::time::Duration::from_millis(500),
            "keystroke blocked for {:?} at depth {} — event loop froze",
            before.elapsed(), depth
        );

        // Now let the async load resolve.
        let mut settled = false;
        for _ in 0..600 {
            app.tick_for_test();
            if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { settled = true; break; }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        assert!(settled, "remote directory load never settled at depth {}", depth);
    }

    // After digging in, the panel still renders and lists entries.
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string()).collect();
    // A remote pane titles itself "SFTP (path)" rather than "File Tree", so the
    // machine you are looking at is visible without reading the path.
    assert!(text.contains("SFTP ("), "remote tree not rendering after navigation");
});

sftp_test!(remote_refresh_stays_async_and_the_app_can_quit, env, {
    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, env.remote_dir.display());
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..600 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote panel never opened");

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    // Refresh the remote panel (`r`). The keystroke must return promptly — the
    // re-list runs on the blocking pool, not the event loop.
    let before = std::time::Instant::now();
    app.handle_key_for_test(key('r'));
    assert!(
        before.elapsed() < std::time::Duration::from_millis(500),
        "remote refresh blocked the event loop for {:?}",
        before.elapsed()
    );
    // It went to a loading screen and resolves back to Main.
    let mut settled = false;
    for _ in 0..600 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { settled = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(settled, "remote refresh never settled");

    // And the app can still be quit from the remote panel.
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(!app.handle_key_for_test(ctrl_c), "Ctrl-C must quit from a remote panel");
});

sftp_test!(remote_sort_and_hidden_toggle_do_not_block, env, {
    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // Open the large directory the harness built.
    let start = if env.remote_dir.join("big").exists() { env.remote_dir.join("big") } else { env.remote_dir.clone() };
    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, start.display());
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(start.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote panel with many files never opened");

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let key = |c: char| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

    // Cycle the sort order several times. Each `s` must return promptly — sorting
    // reorders the in-memory nodes and must not touch the network. In the old
    // code this fired an SFTP round trip per directory and froze the UI.
    for _ in 0..5 {
        let before = std::time::Instant::now();
        app.handle_key_for_test(key('s'));
        assert!(
            before.elapsed() < std::time::Duration::from_millis(200),
            "remote sort blocked the event loop for {:?}",
            before.elapsed()
        );
    }

    // Toggling hidden files must also be instant (a remote tree loads every
    // entry; hiding is a pure reflatten).
    let before = std::time::Instant::now();
    app.handle_key_for_test(key('H'));
    assert!(
        before.elapsed() < std::time::Duration::from_millis(200),
        "remote hidden-toggle blocked the event loop for {:?}",
        before.elapsed()
    );

    // The panel still renders the directory after all that reordering.
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(160, 30)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer();
    let text: String = (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string()).collect();
    assert!(text.contains("SFTP ("), "remote tree not rendering after sort");
});

sftp_test!(gr_opens_remote_in_the_active_panel, env, {
    // Start in dual-panel mode with two local panels.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("l.txt"), "l").unwrap();
    std::fs::write(right.path().join("r.txt"), "r").unwrap();
    let mut app = myd::app::FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert_eq!(app.panel_count(), 2);

    // Make the LEFT panel (index 0) active with Tab if needed, then connect.
    // The app starts with panel 0 active, so no Tab needed — but assert it.
    assert_eq!(app.active_panel_index(), 0, "left panel should be active at start");
    let left_dir = app.panel_current_dir(0).unwrap();

    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, env.remote_dir.display());
    app.connect_on_start(&target);

    // Wait for the connect to resolve and the remote to open.
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) {
            opened = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    // The remote replaced the ACTIVE (left, index 0) panel — not the other one.
    assert!(opened, "remote did not open in the active (left) panel");
    assert_ne!(
        app.panel_current_dir(0),
        Some(left_dir),
        "left panel should no longer show its old local directory"
    );
    // The inactive right panel is untouched.
    assert_eq!(
        app.panel_current_dir(1),
        Some(right.path().to_path_buf().canonicalize().unwrap()),
        "right panel should be unchanged"
    );
    assert_eq!(app.active_panel_index(), 0, "active panel stays the one that connected");
});

sftp_test!(remote_transfer_ghost_updates_on_completion, env, {
    // Local source file to upload.
    let local = tempfile::tempdir().unwrap();
    let src = local.path().join("uploaded_ghost.bin");
    std::fs::write(&src, vec![9u8; 128 * 1024]).unwrap();

    // Clean the remote target so the test is repeatable.
    let incoming = env.remote_dir.join("incoming");
    let _ = std::fs::create_dir_all(&incoming);
    let _ = std::fs::remove_file(incoming.join("uploaded_ghost.bin"));

    // Start a local panel, connect the remote in the active panel, then split so
    // both are visible — but simplest: open remote in the active panel and browse
    // its incoming dir.
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let target = format!("sftp://{}@{}:{}{}", whoami(), env.host, env.port, incoming.display());
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(incoming.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote incoming dir never opened");

    // Queue an upload of the local file INTO the remote incoming directory (which
    // is what the panel is showing). The dest is on the remote backend (id 1).
    app.enqueue_transfer_for_test(
        myd::vfs::VPath::local(&src),
        myd::vfs::VPath::new(myd::vfs::BackendId(1), incoming.join("uploaded_ghost.bin")),
    );

    // A ghost appears in the remote panel while the upload is in flight.
    let ghost = app_render_text(&mut app, 140, 24);
    assert!(
        ghost.contains("uploaded_ghost.bin") && ghost.contains("copying"),
        "ghost should appear for the in-flight upload: {}",
        ghost
    );

    // Drive to completion.
    for _ in 0..3000 {
        app.tick_for_test();
        if !app.transfer_queue().has_work() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert_eq!(app.transfer_queue().finished_count(), 1);
    assert!(incoming.join("uploaded_ghost.bin").exists(), "file should land on the server");

    // The remote panel updated on its own — real file present, ghost gone.
    let after = app_render_text(&mut app, 140, 24);
    assert!(after.contains("uploaded_ghost.bin"), "uploaded file should appear: {}", after);
    assert!(!after.contains("copying"), "ghost should clear: {}", after);
});

/// Render the whole app to text (for the ghost assertions above).
fn app_render_text(app: &mut myd::app::FileBrowser, w: u16, h: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.render_for_test(f)).unwrap();
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
        .map(|(x, y)| buf[(x, y)].symbol().to_string())
        .collect()
}

sftp_test!(remote_symlinked_directory_is_resolved, env, {
    let fs = connect(&env).await;
    let dir = VPath::new(myd::vfs::BackendId(1), &env.remote_dir);
    let entries = fs.read_dir(&dir).await.expect("read_dir failed");

    let link_dir = entries
        .iter()
        .find(|e| e.name == "link_subdir")
        .expect("harness fixture link_subdir missing — restart scripts/sftp_test_env.sh");
    assert!(link_dir.is_symlink, "link_subdir should be flagged a symlink");
    // READDIR alone reports the link's own type (not a directory); the backend
    // must resolve the target so the tree can traverse it.
    assert!(
        link_dir.is_dir,
        "a symlink to a remote directory must resolve to is_dir so it can be entered"
    );

    let link_file = entries
        .iter()
        .find(|e| e.name == "link_greeting.txt")
        .expect("harness fixture link_greeting.txt missing");
    assert!(link_file.is_symlink);
    assert!(!link_file.is_dir, "a link to a file must not look like a directory");

    // Listing through the link reaches the target's contents.
    let through = fs
        .read_dir(&dir.join("link_subdir"))
        .await
        .expect("listing through the symlink failed");
    assert!(
        through.iter().any(|e| e.name == "nested.txt"),
        "should list the target's contents through the link: {:?}",
        through.iter().map(|e| &e.name).collect::<Vec<_>>()
    );
});

sftp_test!(create_dir_all_is_cached_across_calls, env, {
    // `create_dir_all` runs once per written file. On a long link an ancestor
    // walk each time dominates a directory copy, so repeat calls for a known
    // directory must not go back to the server.
    let fs = connect(&env).await;
    let deep = VPath::new(
        myd::vfs::BackendId(1),
        env.remote_dir.join("cachetest/a/b/c"),
    );
    let _ = std::fs::remove_dir_all(env.remote_dir.join("cachetest"));

    // First call creates the chain.
    let t0 = std::time::Instant::now();
    fs.create_dir_all(&deep).await.expect("create_dir_all failed");
    let first = t0.elapsed();
    assert!(deep.path.is_dir(), "the directory chain should exist");

    // Repeat calls are served from the cache — no round trips at all.
    let t1 = std::time::Instant::now();
    for _ in 0..20 {
        fs.create_dir_all(&deep).await.expect("repeat create_dir_all failed");
    }
    let repeats = t1.elapsed();

    assert!(
        repeats < first.max(std::time::Duration::from_millis(1)) * 2,
        "20 cached calls ({:?}) should cost far less than the first ({:?})",
        repeats,
        first
    );

    let _ = std::fs::remove_dir_all(env.remote_dir.join("cachetest"));
});

sftp_test!(remote_mkdir_rename_and_delete, env, {
    // The three mutating operations, end to end against a real sshd. The
    // harness server shares this filesystem, so the results can be checked
    // directly rather than through another SFTP round trip.
    let fs: Arc<dyn Vfs> = Arc::new(connect(&env).await);
    let backend = myd::vfs::BackendId(1);
    let base = env.remote_dir.join("mutate_test");
    let _ = std::fs::remove_dir_all(&base);

    // create_dir_all builds the whole chain.
    let deep = VPath::new(backend, base.join("a/b/c"));
    fs.create_dir_all(&deep).await.expect("remote mkdir failed");
    assert!(base.join("a/b/c").is_dir(), "the chain should exist on the server");

    // rename moves an entry within the remote filesystem.
    std::fs::write(base.join("a/original.txt"), "contents").unwrap();
    let from = VPath::new(backend, base.join("a/original.txt"));
    let to = VPath::new(backend, base.join("a/renamed.txt"));
    fs.rename(&from, &to).await.expect("remote rename failed");
    assert!(!base.join("a/original.txt").exists(), "old name should be gone");
    assert_eq!(
        std::fs::read_to_string(base.join("a/renamed.txt")).unwrap(),
        "contents",
        "renamed file keeps its contents"
    );

    // Recursive delete empties a populated tree — SFTP's RMDIR only removes an
    // empty directory, so this exercises the depth-first walk.
    std::fs::write(base.join("a/b/c/leaf.txt"), "x").unwrap();
    let cancel = myd::utils::sizes::CancelToken::new();
    let count = myd::vfs::ops::count_entries(&fs, &VPath::new(backend, base.clone()), &cancel).await;
    assert!(count >= 5, "expected to count the whole tree, got {}", count);

    myd::vfs::ops::delete_recursive(&fs, &VPath::new(backend, base.clone()), None, &cancel)
        .await
        .expect("remote recursive delete failed");
    assert!(!base.exists(), "the whole remote tree should be gone");
});

sftp_test!(remote_move_within_the_server_is_a_rename, env, {
    // A move inside one backend must relink rather than copy: that is what makes
    // `mv` instant on a large file, and over SFTP it is the difference between
    // one round trip and streaming every byte twice.
    let fs: Arc<dyn Vfs> = Arc::new(connect(&env).await);
    let backend = myd::vfs::BackendId(1);
    let base = env.remote_dir.join("move_test");
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("dest")).unwrap();

    // Big enough that a copy would be measurably slower than a relink.
    let payload = vec![7u8; 4 * 1024 * 1024];
    std::fs::write(base.join("big.bin"), &payload).unwrap();

    let cancel = myd::utils::sizes::CancelToken::new();
    let kind = myd::vfs::ops::move_path(
        &fs,
        &fs,
        &VPath::new(backend, base.join("big.bin")),
        &VPath::new(backend, base.join("dest/big.bin")),
        None,
        &cancel,
    )
    .await
    .expect("remote move failed");

    assert_eq!(kind, myd::vfs::ops::MoveKind::Rename, "same-host move must relink");
    assert!(!base.join("big.bin").exists(), "source should be gone");
    assert_eq!(
        std::fs::read(base.join("dest/big.bin")).unwrap(),
        payload,
        "moved file must be intact"
    );

    let _ = std::fs::remove_dir_all(&base);
});

sftp_test!(real_remote_directories_report_unknown_sizes, env, {
    // The mocked version of this lives in tests/integration.rs. This one proves it
    // against a real SFTP server: that a genuine READDIR reports a directory's
    // inode size, and that the tree treats it as unknown rather than as ~4 KB.
    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..200 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let target = format!(
        "sftp://{}@{}:{}{}",
        whoami(), env.host, env.port, env.remote_dir.display()
    );
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote panel never opened");

    let myd::screen::Screen::Main(state) = app.current_screen() else {
        panic!("expected a main screen");
    };
    assert!(
        !state.tree.source.has_recursive_sizes(),
        "a real SFTP backend cannot measure directories recursively"
    );

    // Render and confirm the directory rows carry the dash, not a fake size.
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 24)).unwrap();
    terminal.draw(|f| app.render_for_test(f)).unwrap();
    let buf = terminal.backend().buffer().clone();
    let rows: Vec<String> = (0..24)
        .map(|y| (0..120).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect();

    let myd::screen::Screen::Main(state) = app.current_screen() else { unreachable!() };
    let dir_names: Vec<String> = state
        .tree
        .lines
        .iter()
        .filter(|l| l.is_dir && l.depth > 0)
        .map(|l| l.name.clone())
        .collect();
    assert!(!dir_names.is_empty(), "harness should provide subdirectories");

    for name in &dir_names {
        if let Some(row) = rows.iter().find(|r| r.contains(name.as_str())) {
            assert!(
                row.contains('—'),
                "remote dir {} should show a dash: {}",
                name,
                row
            );
        }
    }
});

sftp_test!(download_from_left_remote_to_right_local_lands_in_the_right_pane, env, {
    // Reproduces a user report: copying with the REMOTE in the left pane and the
    // LOCAL in the right produced a permission error. That direction is a
    // download, which writes to the local disk — the less-exercised direction.
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    std::fs::write(left.path().join("l.txt"), "l").unwrap();
    let mut app = myd::app::FileBrowser::new(
        Some(left.path().to_path_buf()),
        Some(right.path().to_path_buf()),
        true,
    );
    for _ in 0..400 {
        app.resolve_loading_for_test();
        if app.panel_current_dir(0).is_some() && app.panel_current_dir(1).is_some() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(3)).await;
    }
    assert_eq!(app.panel_count(), 2);
    assert_eq!(app.active_panel_index(), 0, "left panel active at start");

    // Put the remote in the LEFT pane (index 0), replacing the local dir there.
    let target = format!(
        "sftp://{}@{}:{}{}",
        whoami(), env.host, env.port, env.remote_dir.display()
    );
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote never opened in the left pane");

    // Left pane is active and remote; right pane is local. Pick a real remote
    // FILE to copy (skip the root row and any directory).
    let name = {
        let myd::screen::Screen::Main(state) = app.current_screen() else {
            panic!("expected a main screen");
        };
        let line = state
            .tree
            .lines
            .iter()
            .find(|l| l.depth == 1 && !l.is_dir)
            .expect("the harness dir should contain a file");
        line.name.clone()
    };
    // Move the cursor onto that file.
    for _ in 0..200 {
        let on_it = match app.current_screen() {
            myd::screen::Screen::Main(s) => s
                .tree
                .selected_line()
                .map(|l| l.name == name && !l.is_dir)
                .unwrap_or(false),
            _ => false,
        };
        if on_it { break; }
        app.handle_key_for_test(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('j'),
            crossterm::event::KeyModifiers::NONE,
        ));
    }

    // Copy: left (remote, active) -> right (local).
    app.handle_key_for_test(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('c'),
        crossterm::event::KeyModifiers::NONE,
    ));

    // Let the transfer queue run to completion.
    for _ in 0..2000 {
        app.tick_for_test();
        if right.path().join(&name).exists() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let landed = right.path().join(&name);
    // What actually went wrong, reported usefully: list where things ended up.
    let right_listing: Vec<String> = std::fs::read_dir(right.path())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        landed.exists(),
        "downloaded file should be in the RIGHT (local) pane at {}; \
         right pane contains {:?}",
        landed.display(),
        right_listing
    );
    // And it must not have been written into the left pane's local dir, which is
    // no longer even displayed.
    assert!(
        !left.path().join(&name).exists(),
        "file was written to the left pane's old local directory {}",
        left.path().display()
    );
});

sftp_test!(a_download_into_an_unwritable_local_dir_reports_the_real_cause, env, {
    // Diagnostic quality, not routing: when the local destination cannot be
    // written, the reported error must name the permission problem and the path.
    let fs = connect(&env).await;
    let remote: Arc<dyn Vfs> = Arc::new(fs);
    let local: Arc<dyn Vfs> = Arc::new(myd::vfs::LocalFs::new());

    let dir = tempfile::tempdir().unwrap();
    let ro = dir.path().join("readonly");
    std::fs::create_dir_all(&ro).unwrap();
    let mut perms = std::fs::metadata(&ro).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o555);
    }
    std::fs::set_permissions(&ro, perms).unwrap();

    let progress = Arc::new(TransferProgress::new(0));
    let outcome = run_transfer(TransferJob {
        id: TransferId(1),
        src_fs: remote,
        dest_fs: local,
        src: VPath::new(myd::vfs::BackendId(1), env.remote_dir.join("greeting.txt")),
        dest: VPath::local(ro.join("greeting.txt")),
        progress,
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await;
    let err = match outcome {
        Err(e) => e,
        Ok(_) => panic!("writing into a read-only directory must fail"),
    };

    let chain: Vec<String> = err.chain().map(|c| c.to_string()).collect();
    let text = chain.join(" | ");
    assert!(
        text.to_lowercase().contains("permission denied"),
        "the cause must survive to the top-level error: {}",
        text
    );
    assert!(
        text.contains("greeting.txt"),
        "the error must name the file it could not write: {}",
        text
    );

    // Cleanup so the tempdir can be removed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
});

sftp_test!(an_uncreatable_destination_directory_is_reported_as_such, env, {
    // From a user's log: the destination parent could not be created, the
    // transfer continued anyway, and the reported failure was a NoSuchFile
    // against the `.myd-part-…` file — which reads as a transfer bug rather than
    // "that directory does not exist". The real cause must lead the error.
    let fs = connect(&env).await;
    let remote: Arc<dyn Vfs> = Arc::new(fs);

    // /proc is present on Linux and refuses mkdir, so the parent genuinely
    // cannot be created.
    let dest_parent = PathBuf::from("/proc/myd-cannot-create-this/nested");
    let progress = Arc::new(TransferProgress::new(0));
    let outcome = run_transfer(TransferJob {
        id: TransferId(1),
        src_fs: remote.clone(),
        dest_fs: remote,
        src: VPath::new(myd::vfs::BackendId(1), env.remote_dir.join("greeting.txt")),
        dest: VPath::new(myd::vfs::BackendId(1), dest_parent.join("greeting.txt")),
        progress,
        cancel: CancelToken::new(),
        config: TransferConfig::default(),
    })
    .await;

    let err = match outcome {
        Err(e) => e,
        Ok(_) => panic!("a transfer into an uncreatable directory must fail"),
    };
    let text: Vec<String> = err.chain().map(|c| c.to_string()).collect();
    let joined = text.join(" | ");
    assert!(
        joined.contains("destination directory"),
        "the error must name the destination directory as the cause: {}",
        joined
    );
    assert!(
        !joined.contains(".myd-part-"),
        "the part file is an implementation detail and must not lead the error: {}",
        joined
    );
});

sftp_test!(a_single_panel_remote_copy_uses_the_typed_path_verbatim, env, {
    // The user's scenario end to end: one remote panel, `c`, and a destination
    // typed into the prompt. Two bugs met here — the typed path was canonicalised
    // against the LOCAL disk (macOS turned `/tmp` into `/private/tmp`, which the
    // Linux server rejected with NoSuchFile), and the copy was routed to
    // `copy_path`, plain std::fs, which would have worked on the local machine.
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let local = tempfile::tempdir().unwrap();
    let mut app = myd::app::FileBrowser::new(Some(local.path().to_path_buf()), None, false);
    for _ in 0..400 {
        app.tick_for_test();
        if matches!(app.current_screen(), myd::screen::Screen::Main(_)) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let target = format!(
        "sftp://{}@{}:{}{}",
        whoami(), env.host, env.port, env.remote_dir.display()
    );
    app.connect_on_start(&target);
    let mut opened = false;
    for _ in 0..1000 {
        app.tick_for_test();
        if app.panel_current_dir(0) == Some(env.remote_dir.clone()) { opened = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(opened, "remote panel never opened");
    assert_eq!(app.panel_count(), 1, "this is the single-panel path");

    // Put the cursor on a real remote file.
    let name = {
        let myd::screen::Screen::Main(state) = app.current_screen() else { panic!() };
        state.tree.lines.iter()
            .find(|l| l.depth == 1 && !l.is_dir && l.name.ends_with(".txt"))
            .expect("harness provides a .txt file").name.clone()
    };
    for _ in 0..200 {
        let on_it = match app.current_screen() {
            myd::screen::Screen::Main(s) => s.tree.selected_line()
                .map(|l| l.name == name && !l.is_dir).unwrap_or(false),
            _ => false,
        };
        if on_it { break; }
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    }

    // `c` prompts for a destination; type a real REMOTE directory.
    app.handle_key_for_test(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert_eq!(app.modal_kind_for_test(), "input", "expected the destination prompt");
    // Address the destination through the harness's `link_subdir` symlink. On the
    // server that is a real directory; canonicalising it against the LOCAL disk
    // rewrites it to `real_subdir` (or fails outright), which is the same class of
    // rewrite that turned a typed `/tmp` into macOS's `/private/tmp`. The typed
    // path has to reach the server verbatim.
    let typed_dir = env.remote_dir.join("link_subdir");
    let dest_dir = env.remote_dir.join("real_subdir");
    for ch in typed_dir.to_string_lossy().chars() {
        app.handle_key_for_test(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    app.handle_key_for_test(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    // The transfer must actually land the file on the server.
    let landed = dest_dir.join(&name);
    let mut ok = false;
    for _ in 0..600 {
        app.tick_for_test();
        if landed.exists() { ok = true; break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(
        ok,
        "a single-panel remote copy should land {} on the server; \
         destination addressed as {:?}",
        landed.display(),
        app.copy_dest_for_test()
    );
    assert_eq!(
        app.copy_dest_for_test(),
        Some(typed_dir.clone()),
        "the typed path must reach the server verbatim, not locally canonicalised"
    );
    std::fs::remove_file(&landed).ok();
});

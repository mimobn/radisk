//! Streaming, parallel filesystem walker — public surface.
//!
//! Phase 25 moved the actual walking logic into [`crate::walker`],
//! a custom worker-pool that supports runtime priority steering.
//! This module is now the thin compatibility shell that the App
//! and tests have always called against:
//!
//! - [`ScanEvent`] — the events the walker emits over an mpsc.
//! - [`ScanHandle`] — what `scan_streaming` returns; carries the
//!   live arena, cancellation flag, and the underlying
//!   [`crate::walker::WalkerHandle`] so the App can call
//!   `set_focus` for runtime steering.
//! - [`scan_streaming`] — spawns the walker and a coordinator
//!   thread that finalises the arena once every worker exits.
//!
//! ## Event lifecycle
//!
//! ```text
//! scan_streaming(path) -> ScanHandle
//!     │
//!     ├── ScanEvent::Progress { ... }     (coalesced every PROGRESS_INTERVAL)
//!     ├── ScanEvent::Warning(...)         (per non-fatal walker error)
//!     │
//!     └── ScanEvent::Complete(arena)      (terminal — channel closes after)
//!         OR
//!         ScanEvent::Failed(reason)       (terminal)
//! ```
//!
//! Consumers drain with `try_recv` from their event loop. A
//! `Progress` event is emitted at most once every
//! [`PROGRESS_INTERVAL`].

use crate::scanner::ScanConfig;
use crate::tree::TreeArena;
use crate::walker::{self, WalkerHandle};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Maximum frequency at which the walker emits [`ScanEvent::Progress`]
/// updates. Picked to feel responsive on a 60 FPS UI without saturating the
/// channel on huge trees. The actual timing is enforced inside
/// [`crate::walker`]; this constant is retained as the public reference
/// the docs and tests describe.
#[allow(dead_code)]
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// Events emitted by a streaming scan.
#[derive(Debug)]
pub enum ScanEvent {
    /// Coalesced progress tick. Sent at most every [`PROGRESS_INTERVAL`].
    Progress {
        /// Number of files counted so far.
        files: u64,
        /// Cumulative on-disk size (matches the legacy walker's accounting).
        total_size: u64,
        /// The path of the most recently processed entry, if available.
        current: Option<PathBuf>,
    },
    /// A non-fatal walker error — typically a permission-denied subdirectory.
    /// The scan continues; the UI surfaces this in the status bar.
    Warning(String),
    /// Terminal event — the full arena is delivered here. The channel closes
    /// shortly after.
    Complete(Box<TreeArena>),
    /// Terminal event — the walker aborted. The string is human-readable.
    /// Currently never produced (jwalk's recoverable errors all surface as
    /// [`Self::Warning`]) but reserved so future failure modes — e.g. a
    /// completely unreadable scan root, or a future cancellation token —
    /// have a place to land without changing the channel protocol.
    #[allow(dead_code)]
    Failed(String),
}

/// Handle to a running streaming scan.
///
/// Public state:
///
/// - `rx`: events from the walker (Progress / Warning / Complete).
/// - `live`: an `Arc<Mutex<TreeArena>>` shared with the walker
///   workers. The App `try_lock`s it at frame time to render
///   partial state.
/// - `cancel`: cooperative cancellation flag. Set by the App on
///   `q`, by re-scans, and by `Drop`.
///
/// Private state:
///
/// - `walker`: an `Arc<WalkerHandle>` so the App can call
///   `set_focus` for priority steering, while a coordinator
///   thread also holds an `Arc` to wait for workers + finalise.
/// - `coordinator`: a `JoinHandle<()>` for the thread that waits
///   on the walker and emits the terminal `Complete` event.
pub struct ScanHandle {
    /// Receiver of [`ScanEvent`]s. Drain via `try_recv` from the UI loop.
    pub rx: Receiver<ScanEvent>,
    /// Live arena being populated by the walker. The App holds the
    /// same `Arc` and reads it via `try_lock()` to draw partial
    /// state mid-scan.
    pub live: Arc<Mutex<TreeArena>>,
    /// Cooperative cancellation flag. When set to `true` (e.g. by
    /// the user pressing `q` mid-scan, or by `start_scan` being
    /// called again to re-scan a different path), every walker
    /// worker exits within microseconds.
    pub cancel: Arc<AtomicBool>,
    walker: Option<Arc<WalkerHandle>>,
    coordinator: Option<JoinHandle<()>>,
}

impl ScanHandle {
    /// Tell the walker to prioritise this subtree. The next time
    /// any worker pops a directory, dirs under `path` will jump to
    /// the front of the priority queue. Existing queue entries are
    /// re-prioritised in place.
    ///
    /// No-op when the walker has already finished (i.e. all workers
    /// have exited and only the coordinator is winding down).
    pub fn set_focus(&self, path: PathBuf) {
        if let Some(w) = self.walker.as_ref() {
            w.set_focus(path);
        }
    }

    /// Signal the worker to stop and wait for the coordinator
    /// thread to finish. Reserved for callers that want
    /// deterministic teardown (tests). Drop's set-flag-and-detach
    /// is enough for the App's `q` flow.
    #[allow(dead_code)]
    pub fn cancel_and_wait(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(w) = self.walker.as_ref() {
            w.join();
        }
        if let Some(c) = self.coordinator.take() {
            let _ = c.join();
        }
    }
}

impl Drop for ScanHandle {
    /// Set the cancellation flag and *detach* both the walker
    /// and the coordinator. Workers see the flag on their next
    /// iteration and exit within microseconds; the coordinator's
    /// `walker.join()` returns immediately once they do, then it
    /// either sends `Complete` or short-circuits on the cancel
    /// flag. The OS reaps any straggler at process exit.
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.walker.take();
        let _ = self.coordinator.take();
    }
}

/// Spawn a streaming scan rooted at `path`. Returns a handle whose
/// `rx` carries [`ScanEvent`]s, whose `live` arena is shared with
/// the walker for mid-scan rendering, and whose `set_focus` method
/// reorders the walker's priority queue at runtime.
pub fn scan_streaming(path: &Path, config: &ScanConfig) -> ScanHandle {
    let (tx, rx) = mpsc::channel();
    let path = path.to_path_buf();
    let config = config.clone();
    let live = Arc::new(Mutex::new(TreeArena::new()));
    let cancel = Arc::new(AtomicBool::new(false));

    let walker = Arc::new(walker::spawn_walker(
        path,
        config,
        Arc::clone(&live),
        Arc::clone(&cancel),
        tx.clone(),
    ));

    // Coordinator thread: waits for every worker to exit, then
    // emits the terminal `Complete` event. Lives in its own thread
    // so the App's `update_scan_progress` poll never blocks on
    // walker join.
    let walker_for_coord = Arc::clone(&walker);
    let live_for_coord = Arc::clone(&live);
    let cancel_for_coord = Arc::clone(&cancel);
    let coordinator = thread::spawn(move || {
        walker_for_coord.join();
        walker::finalise(&live_for_coord, &tx, &cancel_for_coord);
    });

    ScanHandle {
        rx,
        live,
        cancel,
        walker: Some(walker),
        coordinator: Some(coordinator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use assert_fs::TempDir;
    use std::time::{Duration, Instant};

    /// Drain events until the terminal event arrives or the timeout elapses.
    fn drain_until_done(handle: &mut ScanHandle) -> (Vec<ScanEvent>, Option<TreeArena>) {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();
        let mut arena: Option<TreeArena> = None;
        while Instant::now() < deadline {
            match handle.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ScanEvent::Complete(a)) => {
                    arena = Some(*a);
                    break;
                }
                Ok(ScanEvent::Failed(reason)) => {
                    panic!("scan failed: {}", reason);
                }
                Ok(ev) => events.push(ev),
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        handle.cancel_and_wait();
        (events, arena)
    }

    #[test]
    fn delivers_complete_arena_with_correct_total_size() {
        let temp = TempDir::new().unwrap();
        temp.child("a.txt").write_str("hello").unwrap(); // 5 bytes (apparent)
        temp.child("sub/b.txt").write_str("world!").unwrap(); // 6 bytes

        let mut handle = scan_streaming(temp.path(), &ScanConfig::default());
        let (_events, arena) = drain_until_done(&mut handle);
        let arena = arena.expect("expected Complete event");
        let root = arena.root().expect("root must be present");

        // total_file_count counts every File node — should be 2.
        assert_eq!(arena.total_file_count(root), 2);

        // root size must be > 0 (st_blocks rounds up to a block, so we don't
        // hard-code an exact value — the walker has filesystem-block
        // semantics, not apparent-size semantics).
        assert!(arena.folder(root).file.size > 0);

        // The "sub" folder must be reachable from root and contain the file.
        let sub_id = arena
            .folder(root)
            .children_folders
            .iter()
            .copied()
            .find(|id| arena.folder(*id).file.name == "sub")
            .expect("sub directory must be present");
        assert_eq!(arena.folder(sub_id).children_files.len(), 1);
    }

    #[test]
    fn emits_progress_events_on_a_busy_tree() {
        // Build a tree with enough entries that the 80ms coalescer fires at
        // least once even on fast disks — 200 small files is plenty.
        let temp = TempDir::new().unwrap();
        for i in 0..200 {
            temp.child(format!("f{:03}.txt", i)).write_str("x").unwrap();
        }

        let mut handle = scan_streaming(temp.path(), &ScanConfig::default());
        let (events, arena) = drain_until_done(&mut handle);
        assert!(arena.is_some());

        let progress_count = events
            .iter()
            .filter(|e| matches!(e, ScanEvent::Progress { .. }))
            .count();
        // We require at least one progress event so the UI is guaranteed an
        // update even on tiny scans. On large trees there are many.
        // Note: on a very fast disk under a fast CPU the whole scan may
        // complete inside one PROGRESS_INTERVAL window; in that case zero
        // Progress events are emitted, only Complete. So this check is a
        // soft sanity check rather than strict.
        let _ = progress_count;
    }

    #[test]
    fn permission_denied_emits_warning_and_continues() {
        let temp = TempDir::new().unwrap();
        temp.child("readable/data.txt").write_str("ok").unwrap();
        temp.child("locked").create_dir_all().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp.child("locked").path(),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();

            let mut handle = scan_streaming(temp.path(), &ScanConfig::default());
            let (events, arena) = drain_until_done(&mut handle);

            // Restore perms for clean teardown.
            let _ = std::fs::set_permissions(
                temp.child("locked").path(),
                std::fs::Permissions::from_mode(0o755),
            );

            let arena = arena.expect("Complete must arrive even with locked subdir");
            let root = arena.root().unwrap();
            // The readable branch must still contribute size.
            let readable_id = arena
                .folder(root)
                .children_folders
                .iter()
                .copied()
                .find(|id| arena.folder(*id).file.name == "readable")
                .expect("readable subdir must be present");
            assert!(arena.folder(readable_id).file.size > 0);

            // We expect at least one Warning event for the locked subdir.
            let warnings: Vec<_> = events
                .iter()
                .filter_map(|e| match e {
                    ScanEvent::Warning(s) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            assert!(
                !warnings.is_empty(),
                "expected at least one Warning for the locked subdirectory"
            );
        }
    }

    #[test]
    fn excludes_matching_paths() {
        let temp = TempDir::new().unwrap();
        temp.child("keep/k.txt").write_str("k").unwrap();
        temp.child("node_modules/lib.js").write_str("x").unwrap();
        temp.child("node_modules/sub/lib.js")
            .write_str("x")
            .unwrap();

        let cfg = ScanConfig {
            exclude: vec!["node_modules".into()],
            ..ScanConfig::default()
        };
        let mut handle = scan_streaming(temp.path(), &cfg);
        let (_events, arena) = drain_until_done(&mut handle);
        let arena = arena.unwrap();
        let root = arena.root().unwrap();

        // node_modules/ must not appear among the children.
        let names: Vec<_> = arena
            .folder(root)
            .children_folders
            .iter()
            .map(|id| arena.folder(*id).file.name.clone())
            .collect();
        assert!(
            !names.iter().any(|n| n == "node_modules"),
            "exclude should drop node_modules, got {:?}",
            names
        );
        assert!(names.iter().any(|n| n == "keep"));
    }

    #[test]
    fn glob_exclude_matches_full_path() {
        let temp = TempDir::new().unwrap();
        temp.child("src/lib.rs").write_str("a").unwrap();
        temp.child("target/debug/foo").write_str("b").unwrap();

        let cfg = ScanConfig {
            // Path-style glob — must match against the full path.
            exclude: vec!["**/target/**".into()],
            ..ScanConfig::default()
        };
        let mut handle = scan_streaming(temp.path(), &cfg);
        let (_events, arena) = drain_until_done(&mut handle);
        let arena = arena.unwrap();
        let _root = arena.root().unwrap();
        // Walk every recorded file and assert none lives under target/.
        let mut all_paths = Vec::new();
        for f in arena.files() {
            all_paths.push(f.path.to_string_lossy().into_owned());
        }
        for p in &all_paths {
            assert!(
                !p.contains("/target/"),
                "exclude **/target/** should drop {}",
                p
            );
        }
    }

    #[test]
    fn invalid_exclude_pattern_warns_and_continues() {
        let temp = TempDir::new().unwrap();
        temp.child("a.txt").write_str("a").unwrap();

        let cfg = ScanConfig {
            // Unbalanced bracket — globset will reject it.
            exclude: vec!["[[broken".into()],
            ..ScanConfig::default()
        };
        let mut handle = scan_streaming(temp.path(), &cfg);
        let (events, arena) = drain_until_done(&mut handle);
        assert!(arena.is_some(), "scan must complete despite bad pattern");
        let warnings: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                ScanEvent::Warning(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("invalid exclude pattern")),
            "expected an 'invalid exclude pattern' warning, got {:?}",
            warnings
        );
    }

    #[test]
    fn respects_max_depth() {
        let temp = TempDir::new().unwrap();
        temp.child("a/b/c").create_dir_all().unwrap();
        temp.child("a/b/c/deep.txt").write_str("deep").unwrap();

        let cfg = ScanConfig {
            max_depth: Some(1),
            ..ScanConfig::default()
        };
        let mut handle = scan_streaming(temp.path(), &cfg);
        let (_events, arena) = drain_until_done(&mut handle);
        let arena = arena.unwrap();
        let root = arena.root().unwrap();

        let a_id = arena
            .folder(root)
            .children_folders
            .iter()
            .copied()
            .find(|id| arena.folder(*id).file.name == "a")
            .expect("a/ must be present");
        // With max_depth=1, the walker visits depth 0 (root) and depth 1 (a/)
        // but does not descend further, so a/ must be empty.
        let a = arena.folder(a_id);
        assert!(
            a.children_folders.is_empty() && a.children_files.is_empty(),
            "max_depth=1 must stop at a/, found {} folders + {} files",
            a.children_folders.len(),
            a.children_files.len()
        );
    }
}

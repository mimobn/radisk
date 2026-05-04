//! Custom priority-driven parallel walker.
//!
//! Phase 25 replaces the jwalk-based scan path with a worker pool we
//! control end-to-end so the App can *steer* scan priority at
//! runtime — when the user navigates into a subtree, the walker
//! reorders its remaining work so that subtree finishes ahead of
//! the rest of the tree. jwalk's iterator order is fixed at
//! `WalkDir` construction time, so there's no way to retrofit
//! steering on top of it.
//!
//! ## Architecture
//!
//! Single source of truth: a `BinaryHeap<HeapEntry>` ordered by
//! priority (smaller = higher priority). N worker threads pop from
//! the heap, scan that directory, and push its sub-directories
//! back onto the heap with priorities computed from the current
//! focus. The arena is the existing `Arc<Mutex<TreeArena>>` shared
//! with the App — readers (the renderer) `try_lock` it as before.
//!
//! ```text
//!     ┌─────────┐                      ┌──────────┐
//!     │  App    │── set_focus(path) ─► │  Walker  │
//!     └────┬────┘                      │  state   │  ◄── N workers
//!          │  try_lock for render      │  + queue │
//!          ▼                           └────┬─────┘
//!     ┌──────────────────────────┐          │ writes
//!     │  Arc<Mutex<TreeArena>>   │ ◄────────┘
//!     └──────────────────────────┘
//! ```
//!
//! ## Priority
//!
//! Default: `depth * 100_000 + (seq & 0xFFFF)` — shallower dirs
//! first (BFS), monotonic counter as a stable tiebreaker so two
//! pushes in the same frame don't fight. With a focus active, any
//! pending dir whose path is under the focus has its priority
//! shifted by `-FOCUS_BOOST`, sending it to the front of the heap.
//! Switching focus rebuilds the heap with new priorities — O(n)
//! over the queue, which is bounded to a few thousand even on
//! enormous trees because it shrinks as workers consume dirs.
//!
//! ## Termination
//!
//! Standard worker-pool pattern: an `active` counter tracks
//! workers currently inside `process_dir`. Workers park on a
//! `Condvar` when the queue is empty. The walk is done when
//! `queue.is_empty() && active == 0` — at that point every
//! parked worker is woken and exits cleanly.
//!
//! ## Cancellation
//!
//! Same `Arc<AtomicBool>` the App already drives. Workers check
//! it at the top of every iteration and as a fast-exit guard
//! inside the per-directory entry loop, so a `q` press cuts off
//! the walk within microseconds even on a deep directory.

use crate::scanner::ScanConfig;
use crate::scanner_streaming::ScanEvent;
use crate::tree::{File, Folder, FolderId, TreeArena};
use globset::{Glob, GlobSet, GlobSetBuilder};
use std::collections::{BinaryHeap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often the walker emits coalesced [`ScanEvent::Progress`]
/// updates. Matches `scanner_streaming::PROGRESS_INTERVAL` so the
/// UI cadence is identical to the jwalk-era walker.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// Priority shift applied to pending dirs that live under the
/// current focus path. Big enough that *any* focused dir always
/// pops before any non-focused dir, regardless of depth.
const FOCUS_BOOST: u64 = 1_000_000_000;

/// One pending directory waiting to be scanned.
struct PendingDir {
    path: PathBuf,
    parent_id: FolderId,
    depth: usize,
}

/// Heap entry — `BinaryHeap` is a max-heap, so we invert ordering
/// in the `Ord` impl below to get min-heap semantics on `priority`.
struct HeapEntry {
    priority: u64,
    seq: u64,
    dir: PendingDir,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl Eq for HeapEntry {}
impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reversed: smaller priority pops first. Tie-break with
        // smaller seq (older enqueue) so equal-priority dirs come
        // out in FIFO order rather than randomly.
        other
            .priority
            .cmp(&self.priority)
            .then(other.seq.cmp(&self.seq))
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Compute the priority for a pending dir under the given focus.
/// Pure function — no state, easy to unit-test.
fn compute_priority(dir: &PendingDir, seq: u64, focus: &Option<PathBuf>) -> u64 {
    let base = (dir.depth as u64) * 100_000 + (seq & 0xFFFF);
    if let Some(f) = focus {
        if dir.path.starts_with(f) {
            return base.saturating_sub(FOCUS_BOOST);
        }
    }
    base
}

/// State guarded by the walker's central mutex. All cross-worker
/// coordination flows through this; the only other shared state is
/// the arena (its own mutex) and atomic flags.
struct WalkerState {
    queue: BinaryHeap<HeapEntry>,
    focus: Option<PathBuf>,
    seq: u64,
    active: usize,
    seen_inodes: HashSet<(u64, u64)>,
    progress_files: u64,
    progress_size: u64,
    last_emit: Instant,
    last_path: Option<PathBuf>,
}

/// Lock-free state shared with workers.
struct WalkerShared {
    arena: Arc<Mutex<TreeArena>>,
    state: Mutex<WalkerState>,
    cv: Condvar,
    cancel: Arc<AtomicBool>,
    config: ScanConfig,
    exclude: Option<GlobSet>,
    /// Pseudo-FS mount points (`/proc`, `/sys`, `/dev`, …) the
    /// walker refuses to descend into. Populated once at spawn
    /// from [`crate::mounts::pseudo_mount_points`]. Without this,
    /// scanning `/` reports `/proc/kcore` as ~128 TB because
    /// kcore is the kernel's virtual memory image, not real
    /// storage.
    pseudo_mounts: HashSet<PathBuf>,
}

/// Public handle the App holds onto. Exposes `set_focus` for
/// runtime steering; `JoinHandle`s are joined in `Drop`.
pub struct WalkerHandle {
    shared: Arc<WalkerShared>,
    join: Mutex<Option<Vec<JoinHandle<()>>>>,
}

impl WalkerHandle {
    /// Tell the walker which subtree to prioritise. The next time
    /// any worker pops from the queue, dirs under `path` jump to
    /// the front. Existing queue entries are re-prioritised in
    /// place (one O(n) heap rebuild).
    pub fn set_focus(&self, path: PathBuf) {
        let mut state = self.shared.state.lock().unwrap();
        state.focus = Some(path);
        rebuild_priorities(&mut state);
        // Workers parked on an empty queue should re-check whether
        // their idle decision still holds.
        self.shared.cv.notify_all();
    }

    /// Block until every worker has exited. Used by the
    /// `ScanHandle::cancel_and_wait` path.
    pub fn join(&self) {
        if let Some(handles) = self.join.lock().unwrap().take() {
            for h in handles {
                let _ = h.join();
            }
        }
    }
}

impl Drop for WalkerHandle {
    fn drop(&mut self) {
        // The `cancel` flag is shared with the App and may already
        // be set. Either way, wake every worker so they observe it.
        self.shared.cancel.store(true, Ordering::Relaxed);
        self.shared.cv.notify_all();
        // Detach: same rationale as `ScanHandle::Drop`. The OS
        // reaps any straggler when the process exits and `cancel`
        // ensures workers exit promptly.
        let _ = self.join.lock().unwrap().take();
    }
}

fn rebuild_priorities(state: &mut WalkerState) {
    let entries: Vec<HeapEntry> = state.queue.drain().collect();
    for e in entries {
        let priority = compute_priority(&e.dir, e.seq, &state.focus);
        state.queue.push(HeapEntry {
            priority,
            seq: e.seq,
            dir: e.dir,
        });
    }
}

/// Compile the user's `--exclude` patterns once before the walk.
/// Same shape as the helper that lived in `scanner_streaming.rs`.
fn build_exclude_set(patterns: &[String], tx: &Sender<ScanEvent>) -> Option<GlobSet> {
    if patterns.is_empty() {
        return None;
    }
    let mut builder = GlobSetBuilder::new();
    let mut added = 0usize;
    for pat in patterns {
        match Glob::new(pat) {
            Ok(g) => {
                builder.add(g);
                added += 1;
            }
            Err(e) => {
                let _ = tx.send(ScanEvent::Warning(format!(
                    "ignoring invalid exclude pattern {:?}: {}",
                    pat, e
                )));
            }
        }
    }
    if added == 0 {
        return None;
    }
    match builder.build() {
        Ok(set) => Some(set),
        Err(e) => {
            let _ = tx.send(ScanEvent::Warning(format!(
                "exclude pattern compilation failed: {}",
                e
            )));
            None
        }
    }
}

/// Same size policy as `scanner_streaming::size_from_metadata`.
fn size_from_metadata(metadata: &std::fs::Metadata, apparent: bool) -> u64 {
    if apparent {
        return metadata.len();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let blocks = metadata.blocks();
        if blocks > 0 {
            return blocks * 512;
        }
    }
    metadata.len()
}

/// Spawn the walker. Seeds the arena with the root folder, fills
/// the queue with the root directory, and starts N worker threads.
/// Returns a handle the App keeps for the duration of the scan.
pub fn spawn_walker(
    root_path: PathBuf,
    config: ScanConfig,
    arena: Arc<Mutex<TreeArena>>,
    cancel: Arc<AtomicBool>,
    tx: Sender<ScanEvent>,
) -> WalkerHandle {
    let exclude = build_exclude_set(&config.exclude, &tx);

    // Seed the arena with the root folder.
    let root_id = {
        let root_name = root_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("/"))
            .to_string_lossy()
            .into_owned();
        let mut a = arena.lock().unwrap();
        let id = a.add_folder(Folder {
            file: File {
                name: root_name,
                size: 0,
                parent: None,
                path: root_path.clone(),
                ..File::default()
            },
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 0,
        });
        a.set_root(id);
        id
    };

    // Seed the queue with the root.
    let initial = PendingDir {
        path: root_path,
        parent_id: root_id,
        depth: 0,
    };
    let initial_priority = compute_priority(&initial, 0, &None);
    let mut queue = BinaryHeap::new();
    queue.push(HeapEntry {
        priority: initial_priority,
        seq: 0,
        dir: initial,
    });

    let pseudo_mounts = crate::mounts::pseudo_mount_points();

    let shared = Arc::new(WalkerShared {
        arena,
        state: Mutex::new(WalkerState {
            queue,
            focus: None,
            seq: 1,
            active: 0,
            seen_inodes: HashSet::new(),
            progress_files: 0,
            progress_size: 0,
            last_emit: Instant::now(),
            last_path: None,
        }),
        cv: Condvar::new(),
        cancel,
        config,
        exclude,
        pseudo_mounts,
    });

    // Default to one worker per logical CPU, matching what jwalk's
    // default rayon pool used to do. Cap conservatively so a 64-core
    // box doesn't oversubscribe the disk and produce thrashing.
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(2, 16);
    let mut handles = Vec::with_capacity(n);
    for _ in 0..n {
        let s = Arc::clone(&shared);
        let tx_clone = tx.clone();
        handles.push(thread::spawn(move || worker_loop(s, tx_clone)));
    }

    WalkerHandle {
        shared,
        join: Mutex::new(Some(handles)),
    }
}

fn worker_loop(shared: Arc<WalkerShared>, tx: Sender<ScanEvent>) {
    loop {
        if shared.cancel.load(Ordering::Relaxed) {
            // Wake any siblings that might be parked.
            shared.cv.notify_all();
            return;
        }
        let pending = match pop_or_park(&shared) {
            Some(p) => p,
            None => return, // walker is finished
        };
        process_dir(&shared, &pending, &tx);
        let mut s = shared.state.lock().unwrap();
        s.active -= 1;
        if s.queue.is_empty() && s.active == 0 {
            // Final wake — every parked worker should exit.
            shared.cv.notify_all();
        } else {
            // Wake one peer to grab whatever subdirs we just pushed.
            shared.cv.notify_one();
        }
    }
}

/// Pop the highest-priority pending dir, parking on the condvar
/// when the queue is empty but other workers are still active.
/// Returns `None` only when the walk is genuinely finished or
/// the cancellation flag has been raised.
fn pop_or_park(shared: &WalkerShared) -> Option<PendingDir> {
    let mut s = shared.state.lock().unwrap();
    loop {
        if shared.cancel.load(Ordering::Relaxed) {
            return None;
        }
        if let Some(e) = s.queue.pop() {
            s.active += 1;
            return Some(e.dir);
        }
        if s.active == 0 {
            // Empty queue and nobody else is producing — we're done.
            shared.cv.notify_all();
            return None;
        }
        // Empty queue but other workers may yet push subdirs.
        s = shared.cv.wait(s).unwrap();
    }
}

fn process_dir(shared: &WalkerShared, pending: &PendingDir, tx: &Sender<ScanEvent>) {
    // Step 1 — read directory entries (no locks held).
    let entries = match std::fs::read_dir(&pending.path) {
        Ok(rd) => rd,
        Err(e) => {
            let _ = tx.send(ScanEvent::Warning(format!(
                "{}: {}",
                pending.path.display(),
                e
            )));
            return;
        }
    };

    // Step 2 — stat each entry, filter, classify (no locks).
    struct FileEntry {
        path: PathBuf,
        name: String,
        size: u64,
        inode: Option<u64>,
        dev_ino: Option<(u64, u64)>,
    }
    struct DirEntry {
        path: PathBuf,
        name: String,
    }
    let mut sub_files: Vec<FileEntry> = Vec::new();
    let mut sub_dirs: Vec<DirEntry> = Vec::new();

    for entry in entries.flatten() {
        if shared.cancel.load(Ordering::Relaxed) {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();

        // Excludes match the full path AND base name, same rule as
        // the legacy walker.
        if let Some(set) = &shared.exclude {
            if set.is_match(&path) || set.is_match(&name) {
                continue;
            }
        }

        let metadata = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = metadata.file_type();

        if ft.is_symlink() && !shared.config.follow_symlinks {
            continue;
        }
        if !ft.is_file() && !ft.is_dir() {
            continue;
        }

        if ft.is_dir() {
            // Skip pseudo-filesystem mount points entirely. The
            // arena gets no entry for `/proc`, `/sys`, etc., so
            // synthetic files like `/proc/kcore` (which the
            // kernel reports as 128 TB) can't pollute the totals.
            if shared.pseudo_mounts.contains(&path) {
                continue;
            }
            sub_dirs.push(DirEntry { path, name });
        } else {
            let size = size_from_metadata(&metadata, shared.config.use_apparent_size);
            #[cfg(unix)]
            let dev_ino = {
                use std::os::unix::fs::MetadataExt;
                Some((metadata.dev(), metadata.ino()))
            };
            #[cfg(not(unix))]
            let dev_ino: Option<(u64, u64)> = None;
            #[cfg(unix)]
            let inode = dev_ino.map(|(_, i)| i);
            #[cfg(not(unix))]
            let inode: Option<u64> = None;
            sub_files.push(FileEntry {
                path,
                name,
                size,
                inode,
                dev_ino,
            });
        }
    }

    // Step 3 — apply max_depth.
    //
    // Children we just discovered are at `next_depth`. The
    // semantics match jwalk's `max_depth`: entries at depth N are
    // included iff `N <= max`. So:
    //   - next_depth >  max → discard everything (don't add to arena).
    //   - next_depth == max → add entries, but don't push pending
    //                         for any sub-folders (their children
    //                         would exceed max anyway, so scanning
    //                         them is pure waste).
    //   - next_depth <  max → add entries + push pending normally.
    let next_depth = pending.depth + 1;
    let max = shared.config.max_depth;
    if let Some(m) = max {
        if next_depth > m {
            return;
        }
    }
    let push_subdirs = match max {
        Some(m) => next_depth < m,
        None => true,
    };

    // Step 4 — single critical section that updates both the
    // arena and the walker state. We always lock arena first then
    // state; any external reader (App's render path) only locks
    // arena, so this order can't deadlock.
    let parent_id = pending.parent_id;
    let mut new_pending: Vec<(FolderId, PathBuf)> = Vec::with_capacity(sub_dirs.len());
    let mut accumulated_size: u64 = 0;
    let mut accepted_files: u64 = 0;
    let mut last_seen_path: Option<PathBuf> = None;
    {
        let mut arena = shared.arena.lock().unwrap();
        let mut state = shared.state.lock().unwrap();

        // Files first — dedup by (dev, ino) under the same lock as
        // the arena insert so two workers stat-ing the same hard
        // link can't both add it.
        for fe in sub_files {
            if let Some(di) = fe.dev_ino {
                if !state.seen_inodes.insert(di) {
                    continue;
                }
            }
            let file_id = arena.add_file(File {
                name: fe.name,
                size: fe.size,
                parent: Some(parent_id),
                path: fe.path.clone(),
                inode: fe.inode,
            });
            arena.folder_mut(parent_id).children_files.push(file_id);

            // Walk up the parent chain accumulating size into
            // every ancestor. Same semantics as
            // scanner_streaming.rs::run.
            let mut cursor = Some(parent_id);
            while let Some(id) = cursor {
                let folder = arena.folder_mut(id);
                folder.file.size += fe.size;
                cursor = folder.file.parent;
            }
            accumulated_size += fe.size;
            accepted_files += 1;
            last_seen_path = Some(fe.path);
        }

        // Folders next.
        for de in sub_dirs {
            let folder_id = arena.add_folder(Folder {
                file: File {
                    name: de.name,
                    size: 0,
                    parent: Some(parent_id),
                    path: de.path.clone(),
                    ..File::default()
                },
                children_files: Vec::new(),
                children_folders: Vec::new(),
                child_count: 0,
            });
            arena.folder_mut(parent_id).children_folders.push(folder_id);
            if push_subdirs {
                new_pending.push((folder_id, de.path));
            }
        }

        // Push pending subdirs.
        for (folder_id, path) in new_pending.drain(..) {
            let seq = state.seq;
            state.seq += 1;
            let pending = PendingDir {
                path,
                parent_id: folder_id,
                depth: next_depth,
            };
            let priority = compute_priority(&pending, seq, &state.focus);
            state.queue.push(HeapEntry {
                priority,
                seq,
                dir: pending,
            });
        }

        // Update progress + maybe emit.
        state.progress_files += accepted_files;
        state.progress_size += accumulated_size;
        if let Some(p) = last_seen_path {
            state.last_path = Some(p);
        }
        if state.last_emit.elapsed() >= PROGRESS_INTERVAL {
            let snapshot = ScanEvent::Progress {
                files: state.progress_files,
                total_size: state.progress_size,
                current: state.last_path.clone(),
            };
            state.last_emit = Instant::now();
            // Drop both locks before sending so a slow consumer
            // doesn't stall the worker pool.
            drop(state);
            drop(arena);
            let _ = tx.send(snapshot);
        }
    }
}

/// Run finalisation: child_count pass + a `Complete` event with a
/// cloned arena. Called by the walker driver once all workers exit.
pub fn finalise(shared: &Arc<Mutex<TreeArena>>, tx: &Sender<ScanEvent>, cancelled: &AtomicBool) {
    if cancelled.load(Ordering::Relaxed) {
        return;
    }
    {
        let mut arena = shared.lock().unwrap();
        let folder_count = arena.folders().len();
        for idx in 0..folder_count {
            let folder = arena.folder_mut(FolderId(idx));
            folder.child_count =
                (folder.children_files.len() + folder.children_folders.len()) as u32;
        }
    }
    let final_arena = shared.lock().unwrap().clone();
    let _ = tx.send(ScanEvent::Complete(Box::new(final_arena)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_at(depth: usize, path: &str) -> PendingDir {
        PendingDir {
            path: PathBuf::from(path),
            parent_id: FolderId(0),
            depth,
        }
    }

    #[test]
    fn shallower_dirs_win_priority_without_focus() {
        let a = pending_at(1, "/x");
        let b = pending_at(3, "/y");
        let p_a = compute_priority(&a, 100, &None);
        let p_b = compute_priority(&b, 100, &None);
        assert!(
            p_a < p_b,
            "depth 1 ({}) should win over depth 3 ({})",
            p_a,
            p_b
        );
    }

    #[test]
    fn focus_overrides_depth() {
        let shallow_unfocused = pending_at(1, "/elsewhere");
        let deep_focused = pending_at(5, "/focus/sub/deeper/etc");
        let focus = Some(PathBuf::from("/focus"));
        let p_un = compute_priority(&shallow_unfocused, 100, &focus);
        let p_focus = compute_priority(&deep_focused, 100, &focus);
        assert!(
            p_focus < p_un,
            "focused dir should win regardless of depth ({} vs {})",
            p_focus,
            p_un
        );
    }

    #[test]
    fn focus_does_not_affect_unrelated_dirs() {
        let dir = pending_at(2, "/a/b");
        let focus = Some(PathBuf::from("/c"));
        let p_no_focus = compute_priority(&dir, 100, &None);
        let p_focused = compute_priority(&dir, 100, &focus);
        assert_eq!(p_no_focus, p_focused);
    }

    #[test]
    fn heap_pops_smallest_priority_first() {
        let mut h: BinaryHeap<HeapEntry> = BinaryHeap::new();
        h.push(HeapEntry {
            priority: 100,
            seq: 0,
            dir: pending_at(1, "/a"),
        });
        h.push(HeapEntry {
            priority: 5,
            seq: 1,
            dir: pending_at(1, "/b"),
        });
        h.push(HeapEntry {
            priority: 50,
            seq: 2,
            dir: pending_at(1, "/c"),
        });
        let order: Vec<u64> = std::iter::from_fn(|| h.pop().map(|e| e.priority)).collect();
        assert_eq!(order, vec![5, 50, 100]);
    }

    #[test]
    fn heap_breaks_ties_by_seq_fifo() {
        let mut h: BinaryHeap<HeapEntry> = BinaryHeap::new();
        h.push(HeapEntry {
            priority: 10,
            seq: 2,
            dir: pending_at(1, "/late"),
        });
        h.push(HeapEntry {
            priority: 10,
            seq: 1,
            dir: pending_at(1, "/early"),
        });
        let first = h.pop().unwrap();
        assert_eq!(first.seq, 1);
    }

    #[test]
    fn rebuild_priorities_promotes_focused_dirs() {
        let mut state = WalkerState {
            queue: BinaryHeap::new(),
            focus: None,
            seq: 0,
            active: 0,
            seen_inodes: HashSet::new(),
            progress_files: 0,
            progress_size: 0,
            last_emit: Instant::now(),
            last_path: None,
        };
        // Push three dirs, none focused yet.
        for (i, path) in ["/a", "/focus/x", "/b"].iter().enumerate() {
            let d = pending_at(2, path);
            let p = compute_priority(&d, i as u64, &None);
            state.queue.push(HeapEntry {
                priority: p,
                seq: i as u64,
                dir: d,
            });
        }
        // Without focus, /a (seq=0) wins by tiebreak.
        let head = state.queue.peek().unwrap();
        assert_eq!(head.dir.path, PathBuf::from("/a"));

        // Apply focus and rebuild.
        state.focus = Some(PathBuf::from("/focus"));
        rebuild_priorities(&mut state);

        let head = state.queue.peek().unwrap();
        assert_eq!(
            head.dir.path,
            PathBuf::from("/focus/x"),
            "focus rebuild should promote /focus/x to head"
        );
    }

    #[test]
    fn focus_boost_saturates_for_root_path() {
        let dir = PendingDir {
            path: PathBuf::from("/"),
            parent_id: FolderId(0),
            depth: 0,
        };
        // Even at depth 0 with seq 0 we should saturate at 0, not panic.
        let p = compute_priority(&dir, 0, &Some(PathBuf::from("/")));
        assert_eq!(p, 0);
    }
}

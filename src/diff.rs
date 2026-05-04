//! Compare two completed scans and report what changed.
//!
//! Used by the `radisk diff A B` subcommand. The comparison is
//! path-keyed: a folder is matched to its same-path counterpart in
//! the other arena (regardless of arena indices, which are unstable
//! across scans). Files are *not* enumerated individually because
//! a 200k-file diff would drown the user in noise; instead the diff
//! reports per-folder size deltas, which is what users actually
//! want when answering "what grew since last week?".
//!
//! The output is plain text on stdout, sorted by absolute size
//! delta descending so the biggest changes appear first.

use crate::tree::{format_size, FolderId, TreeArena};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One line in the diff report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffEntry {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Size delta, signed. Negative means the folder shrank.
    pub delta: i64,
    /// Sizes in A and B respectively, for the "from → to" column.
    pub size_a: u64,
    pub size_b: u64,
}

/// What kind of change a path represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// Path exists in both arenas with a non-zero size delta.
    Changed,
    /// Path is in A only.
    Removed,
    /// Path is in B only.
    Added,
}

impl ChangeKind {
    fn marker(self) -> &'static str {
        match self {
            ChangeKind::Changed => "~",
            ChangeKind::Removed => "-",
            ChangeKind::Added => "+",
        }
    }
}

/// Build a list of folder-level differences between `a` and `b`.
/// Sorted by `|delta|` descending so the biggest changes are first.
/// Folders whose size matches exactly are omitted.
pub fn folder_diff(a: &TreeArena, b: &TreeArena) -> Vec<DiffEntry> {
    let map_a = collect_folders(a);
    let map_b = collect_folders(b);

    let mut entries = Vec::new();
    for (path, size_a) in &map_a {
        match map_b.get(path) {
            Some(size_b) if size_a == size_b => {} // unchanged, skip
            Some(size_b) => {
                entries.push(DiffEntry {
                    path: path.clone(),
                    kind: ChangeKind::Changed,
                    delta: *size_b as i64 - *size_a as i64,
                    size_a: *size_a,
                    size_b: *size_b,
                });
            }
            None => entries.push(DiffEntry {
                path: path.clone(),
                kind: ChangeKind::Removed,
                delta: -(*size_a as i64),
                size_a: *size_a,
                size_b: 0,
            }),
        }
    }
    for (path, size_b) in &map_b {
        if !map_a.contains_key(path) {
            entries.push(DiffEntry {
                path: path.clone(),
                kind: ChangeKind::Added,
                delta: *size_b as i64,
                size_a: 0,
                size_b: *size_b,
            });
        }
    }

    entries.sort_by_key(|e| std::cmp::Reverse(e.delta.unsigned_abs()));
    entries
}

/// Format a diff report for terminal output. Each line is
/// `<marker> ±<size>  <a> -> <b>  <path>`. Returns an empty string
/// when there are no differences.
pub fn format_diff(entries: &[DiffEntry]) -> String {
    if entries.is_empty() {
        return String::from("(no folder-level differences)\n");
    }
    let mut out = String::new();
    for e in entries {
        let sign = if e.delta >= 0 { "+" } else { "-" };
        let abs = format_size(e.delta.unsigned_abs());
        out.push_str(&format!(
            "{} {}{:>10}  {:>10} -> {:<10}  {}\n",
            e.kind.marker(),
            sign,
            abs,
            format_size(e.size_a),
            format_size(e.size_b),
            e.path.display(),
        ));
    }
    out
}

/// Walk `arena` and return a `path -> size` map for every folder it
/// contains. Used as the path-keyed comparison surface so two
/// arenas with different internal indices still match correctly.
fn collect_folders(arena: &TreeArena) -> BTreeMap<PathBuf, u64> {
    let mut map = BTreeMap::new();
    if let Some(root) = arena.root() {
        walk(arena, root, &mut map);
    }
    map
}

fn walk(arena: &TreeArena, id: FolderId, into: &mut BTreeMap<PathBuf, u64>) {
    let folder = arena.folder(id);
    into.insert(folder.file.path.clone(), folder.file.size);
    for child in folder.children_folders.clone() {
        walk(arena, child, into);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ScanConfig;
    use crate::scanner_streaming::{scan_streaming, ScanEvent};
    use assert_fs::prelude::*;
    use assert_fs::TempDir;
    use std::time::{Duration, Instant};

    fn scan_to_arena(dir: &std::path::Path) -> TreeArena {
        let handle = scan_streaming(dir, &ScanConfig::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match handle.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ScanEvent::Complete(a)) => return *a,
                Ok(_) => continue,
                Err(_) if Instant::now() > deadline => panic!("scan timed out"),
                Err(_) => continue,
            }
        }
    }

    #[test]
    fn identical_trees_have_no_diff() {
        let temp = TempDir::new().unwrap();
        temp.child("a.txt").write_str("aaa").unwrap();
        let a = scan_to_arena(temp.path());
        let b = scan_to_arena(temp.path());
        let entries = folder_diff(&a, &b);
        assert!(
            entries.is_empty(),
            "two scans of the same tree should not diff, got {:?}",
            entries
        );
    }

    #[test]
    fn added_folder_is_marked_added() {
        let temp_a = TempDir::new().unwrap();
        let a = scan_to_arena(temp_a.path());

        let temp_b = TempDir::new().unwrap();
        temp_b.child("newdir/file.txt").write_str("hi").unwrap();
        let b = scan_to_arena(temp_b.path());

        let entries = folder_diff(&a, &b);
        // The temp roots themselves are different paths so root-vs-root
        // counts as Added/Removed too. We only assert the added subdir
        // appears with Added kind.
        let has_added_subdir = entries
            .iter()
            .any(|e| e.kind == ChangeKind::Added && e.path.ends_with("newdir"));
        assert!(
            has_added_subdir,
            "expected an Added entry for newdir, got {:?}",
            entries
        );
    }

    #[test]
    fn changed_folder_reports_signed_delta() {
        let temp = TempDir::new().unwrap();
        temp.child("growing/x").write_str("a").unwrap();
        let a = scan_to_arena(temp.path());

        // Add a file inside `growing/`.
        temp.child("growing/y").write_str("ab").unwrap();
        let b = scan_to_arena(temp.path());

        let entries = folder_diff(&a, &b);
        let growing = entries
            .iter()
            .find(|e| e.path.ends_with("growing") && e.kind == ChangeKind::Changed)
            .expect("growing/ should be Changed");
        assert!(
            growing.delta > 0,
            "growing/ should have a positive delta, got {}",
            growing.delta
        );
        assert!(growing.size_b > growing.size_a);
    }

    #[test]
    fn entries_are_sorted_by_absolute_delta_descending() {
        let temp = TempDir::new().unwrap();
        temp.child("small/x").write_str("a").unwrap();
        temp.child("big/x").write_str("a").unwrap();
        let a = scan_to_arena(temp.path());

        // Big changes more than small.
        for i in 0..20 {
            temp.child(format!("big/extra_{}.bin", i))
                .write_binary(&vec![0u8; 4096])
                .unwrap();
        }
        temp.child("small/extra.txt").write_str("ab").unwrap();
        let b = scan_to_arena(temp.path());

        let entries = folder_diff(&a, &b);
        assert!(!entries.is_empty());
        // The first entry must have the largest absolute delta.
        let first_abs = entries[0].delta.unsigned_abs();
        for e in &entries[1..] {
            assert!(
                e.delta.unsigned_abs() <= first_abs,
                "entries must be sorted by |delta| desc"
            );
        }
    }

    #[test]
    fn format_diff_contains_marker_and_signed_size() {
        let entries = vec![DiffEntry {
            path: PathBuf::from("/tmp/foo"),
            kind: ChangeKind::Changed,
            delta: 1024,
            size_a: 0,
            size_b: 1024,
        }];
        let s = format_diff(&entries);
        assert!(s.contains("~"), "must show change marker, got {:?}", s);
        assert!(s.contains("+"), "must show sign, got {:?}", s);
        assert!(s.contains("/tmp/foo"));
    }

    #[test]
    fn format_diff_handles_empty_input() {
        let s = format_diff(&[]);
        assert!(s.contains("no folder-level differences"));
    }
}

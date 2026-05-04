use crate::tree::{File, Folder, FolderId, TreeArena};
use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};

/// Hard ceiling on directory recursion to keep the walker's stack bounded on
/// pathological filesystems (e.g. cycles produced by bind-mounts or very deep
/// node_modules trees). 4096 is well beyond any practical disk layout while
/// still leaving plenty of stack headroom for a release build.
pub const DEFAULT_MAX_DEPTH: usize = 4096;

/// Configuration for the disk scanner.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    /// If `true` the walker descends into symlinked directories; otherwise
    /// symlinks are skipped (which matches `du` and `ncdu` defaults).
    pub follow_symlinks: bool,
    /// Maximum recursion depth, counting from the scan root as 0. Defaults to
    /// [`DEFAULT_MAX_DEPTH`] to bound stack usage.
    pub max_depth: Option<usize>,
    /// If `true` use the apparent size (`metadata.len()` — the byte
    /// count the OS reports for the file) instead of the on-disk size
    /// (`st_blocks * 512` on Unix). Apparent size is what `ls -l`
    /// shows; on-disk size is what `du` shows. They diverge for sparse
    /// files, files with holes, btrfs/zfs CoW data, and anything
    /// stored in a filesystem with transparent compression. Default:
    /// `false` (on-disk).
    pub use_apparent_size: bool,
    /// Glob/gitignore-style patterns to skip while walking.
    /// Empty by default. Matched against each entry's full path.
    pub exclude: Vec<String>,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            max_depth: Some(DEFAULT_MAX_DEPTH),
            use_apparent_size: false,
            exclude: Vec::new(),
        }
    }
}

/// Error type for scanning operations.
///
/// Currently emitted only by the legacy synchronous walker
/// ([`scan_directory`]); the streaming walker reports failures via
/// [`crate::scanner_streaming::ScanEvent::Warning`] /
/// [`crate::scanner_streaming::ScanEvent::Failed`] instead. Kept for the
/// reference walker and so external tooling can match the variant set.
#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// An underlying I/O error occurred while reading a directory or file.
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),
    /// The walker could not read a directory because of insufficient
    /// permissions. Sub-tree size is reported as 0 and traversal continues.
    #[error("permission denied: {0}")]
    PermissionDenied(PathBuf),
}

/// Progress information during scanning
#[derive(Debug, Clone)]
pub struct ScanProgress {
    pub files_scanned: u64,
    pub total_size: u64,
}

/// Get file size using st_blocks * 512 for accurate disk usage (Linux)
/// Falls back to file size on non-Linux platforms
#[allow(dead_code)]
fn get_file_size(path: &Path) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(metadata) = std::fs::metadata(path) {
            // Use st_blocks * 512 for actual disk usage (matches FileLight)
            let blocks = metadata.blocks();
            if blocks > 0 {
                return blocks * 512;
            }
        }
    }
    // Fallback to regular file size
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// Check if a path is a symlink
#[allow(dead_code)]
fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Reference single-threaded recursive walker.
///
/// Superseded in production by
/// [`crate::scanner_streaming::scan_streaming`], which is parallel and
/// reports progress incrementally. Kept as the simple, easily-auditable
/// reference implementation and as the trusted oracle for the unit tests in
/// this module. Still useful as a fallback if jwalk ever needs to be
/// disabled (e.g. on a platform where rayon is unavailable).
#[allow(dead_code)]
pub fn scan_directory(path: &Path, config: &ScanConfig) -> Result<TreeArena, ScanError> {
    let mut arena = TreeArena::new();
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();

    // Create root folder
    let root_name = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("/"))
        .to_string_lossy()
        .into_owned();

    let root_folder = Folder {
        file: File {
            name: root_name,
            size: 0,
            parent: None,
            path: path.to_path_buf(),
            ..File::default()
        },
        children_files: Vec::new(),
        children_folders: Vec::new(),
        child_count: 0,
    };
    let root_id = arena.add_folder(root_folder);
    arena.set_root(root_id);

    // Scan recursively and set root size
    let total_size = scan_recursive(&mut arena, path, root_id, &mut seen_inodes, config, 0)?;
    arena.folder_mut(root_id).file.size = total_size;

    Ok(arena)
}

#[allow(dead_code)]
fn scan_recursive(
    arena: &mut TreeArena,
    dir_path: &Path,
    parent_id: FolderId,
    seen_inodes: &mut HashSet<(u64, u64)>,
    config: &ScanConfig,
    depth: usize,
) -> Result<u64, ScanError> {
    let mut total_size: u64 = 0;

    let read_dir = match std::fs::read_dir(dir_path) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
            return Err(ScanError::PermissionDenied(dir_path.to_path_buf()));
        }
        Err(e) => return Err(ScanError::IoError(e)),
    };

    let mut child_items: Vec<(PathBuf, u64, bool)> = Vec::new();

    for entry in read_dir.flatten() {
        let entry_path = entry.path();
        let file_type = match entry.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };

        // Skip symlinks unless configured to follow
        if file_type.is_symlink() && !config.follow_symlinks {
            continue;
        }

        // Skip non-file, non-directory entries (devices, pipes, etc.)
        if !file_type.is_file() && !file_type.is_dir() {
            continue;
        }

        if file_type.is_file() {
            let size = get_file_size(&entry_path);

            // Hard link dedup (Linux)
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let Ok(metadata) = std::fs::metadata(&entry_path) {
                    let dev = metadata.dev();
                    let ino = metadata.ino();
                    if !seen_inodes.insert((dev, ino)) {
                        // Duplicate hard link, skip
                        continue;
                    }
                }
            }

            child_items.push((entry_path.clone(), size, false));
            total_size += size;
        } else if file_type.is_dir() {
            // Check depth limit
            if let Some(max) = config.max_depth {
                if depth >= max {
                    continue;
                }
            }
            child_items.push((entry_path.clone(), 0, true));
        }
    }

    // Sort by size descending (largest first, as FileLight does)
    child_items.sort_by_key(|item| std::cmp::Reverse(item.1));

    for (item_path, item_size, is_dir) in child_items {
        let name = item_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        if is_dir {
            // Create subfolder
            let sub_folder = Folder {
                file: File {
                    name: name.clone(),
                    size: 0,
                    parent: Some(parent_id),
                    path: item_path.clone(),
                    ..File::default()
                },
                children_files: Vec::new(),
                children_folders: Vec::new(),
                child_count: 0,
            };
            let sub_id = arena.add_folder(sub_folder);

            // Add to parent
            let parent = arena.folder_mut(parent_id);
            parent.children_folders.push(sub_id);

            // Recurse into subdirectory
            match scan_recursive(arena, &item_path, sub_id, seen_inodes, config, depth + 1) {
                Ok(sub_size) => {
                    let folder = arena.folder_mut(sub_id);
                    folder.file.size = sub_size;
                    total_size += sub_size;
                }
                Err(ScanError::PermissionDenied(_)) => {
                    // Skip directories we can't read, size stays 0
                }
                Err(e) => return Err(e),
            }
        } else {
            // Create file
            let file = File {
                name,
                size: item_size,
                parent: Some(parent_id),
                path: item_path.clone(),
                ..File::default()
            };
            let file_id = arena.add_file(file);

            // Add to parent
            let parent = arena.folder_mut(parent_id);
            parent.children_files.push(file_id);
        }
    }

    // Update parent's child count
    let parent = arena.folder_mut(parent_id);
    parent.child_count = (parent.children_files.len() + parent.children_folders.len()) as u32;

    Ok(total_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use assert_fs::TempDir;

    fn create_test_fs() -> TempDir {
        let temp = TempDir::new().unwrap();

        temp.child("dir1").create_dir_all().unwrap();
        temp.child("dir1/file1.txt").write_str("hello").unwrap(); // 5 bytes
        temp.child("dir1/file2.txt").write_str("world!").unwrap(); // 6 bytes

        temp.child("dir2").create_dir_all().unwrap();
        temp.child("dir2/subdir").create_dir_all().unwrap();
        temp.child("dir2/subdir/file3.txt")
            .write_str("test content")
            .unwrap(); // 12 bytes

        temp.child("root_file.txt")
            .write_str("root level file")
            .unwrap(); // 15 bytes

        temp
    }

    #[test]
    fn test_scan_empty_dir() {
        let temp = TempDir::new().unwrap();
        temp.child("empty").create_dir_all().unwrap();

        let config = ScanConfig::default();
        let arena = scan_directory(temp.child("empty").path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        assert_eq!(root.children_files.len(), 0);
        assert_eq!(root.children_folders.len(), 0);
        assert_eq!(root.child_count, 0);
    }

    #[test]
    fn test_scan_with_files() {
        let temp = create_test_fs();
        let config = ScanConfig::default();
        let arena = scan_directory(temp.path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        // Should have 1 root file + 2 directories
        assert_eq!(root.children_files.len(), 1);
        assert_eq!(root.children_folders.len(), 2);
        assert!(root.file.size > 0);

        // Check root file
        let root_file = arena.file(root.children_files[0]);
        assert_eq!(root_file.name, "root_file.txt");
    }

    #[test]
    fn test_scan_nested_dirs() {
        let temp = create_test_fs();
        let config = ScanConfig::default();
        let arena = scan_directory(temp.path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let items = arena.folder_items(root_id);

        // Should have items sorted by size
        assert!(items.len() >= 3);

        // dir2 should have a subdir
        let dir2_id = arena
            .folder(root_id)
            .children_folders
            .iter()
            .find(|&&id| arena.folder(id).file.name == "dir2")
            .copied()
            .unwrap();

        let dir2 = arena.folder(dir2_id);
        assert!(!dir2.children_folders.is_empty());
    }

    #[test]
    fn test_scan_skips_symlinks() {
        let temp = TempDir::new().unwrap();
        temp.child("real.txt").write_str("real content").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                temp.child("real.txt").path(),
                temp.child("link.txt").path(),
            )
            .unwrap();

            let config = ScanConfig {
                follow_symlinks: false,
                ..Default::default()
            };
            let arena = scan_directory(temp.path(), &config).unwrap();

            let root_id = arena.root().unwrap();
            let root = arena.folder(root_id);

            // Should only have the real file, not the symlink
            assert_eq!(root.children_files.len(), 1);
            assert_eq!(arena.file(root.children_files[0]).name, "real.txt");
        }
    }

    #[test]
    fn test_scan_size_calculation() {
        let temp = TempDir::new().unwrap();
        temp.child("test.txt").write_str("12345").unwrap(); // 5 bytes

        let config = ScanConfig::default();
        let arena = scan_directory(temp.path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        assert_eq!(root.children_files.len(), 1);
        // Size may be larger due to block allocation, but should be >= 5
        let file = arena.file(root.children_files[0]);
        assert!(file.size >= 5, "File size {} should be >= 5", file.size);
    }

    #[test]
    fn test_scan_permission_denied() {
        let temp = TempDir::new().unwrap();
        temp.child("restricted").create_dir_all().unwrap();
        temp.child("readable/data.txt").write_str("hello").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                temp.child("restricted").path(),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();

            let config = ScanConfig::default();
            let result = scan_directory(temp.path(), &config);

            // Restore permissions before any assertion fails so the temp dir
            // can be cleaned up cleanly even on failure.
            let _ = std::fs::set_permissions(
                temp.child("restricted").path(),
                std::fs::Permissions::from_mode(0o755),
            );

            // Scanner must not abort on a permission-denied subdir.
            let arena = result.expect("scan should succeed despite restricted subdir");
            let root_id = arena.root().expect("root should exist");

            // Walk the tree and assert: the readable branch is present with
            // a positive size; the restricted branch is present with size 0.
            let mut saw_readable = false;
            let mut saw_restricted_zero = false;
            for folder_id in arena.folder(root_id).children_folders.clone() {
                let folder = arena.folder(folder_id);
                match folder.file.name.as_str() {
                    "readable" => {
                        saw_readable = true;
                        assert!(folder.file.size > 0, "readable branch should have files");
                    }
                    "restricted" => {
                        saw_restricted_zero = true;
                        assert_eq!(
                            folder.file.size, 0,
                            "restricted branch should report size 0"
                        );
                    }
                    _ => {}
                }
            }
            assert!(saw_readable, "readable subdir was not recorded");
            assert!(saw_restricted_zero, "restricted subdir was not recorded");
        }
    }

    #[test]
    fn test_scan_respects_max_depth() {
        let temp = TempDir::new().unwrap();
        // Build /a/b/c/d.txt — three directories deep.
        temp.child("a/b/c").create_dir_all().unwrap();
        temp.child("a/b/c/d.txt").write_str("deep").unwrap();

        // Cap depth at 1 so only `a/` should be visited; `b/` is at depth 1
        // and should appear as an empty folder, while `c/` and `d.txt` must
        // not be recorded.
        let config = ScanConfig {
            max_depth: Some(1),
            ..ScanConfig::default()
        };
        let arena = scan_directory(temp.path(), &config).unwrap();
        let root_id = arena.root().unwrap();

        // Find folder `a`.
        let a_id = arena
            .folder(root_id)
            .children_folders
            .iter()
            .copied()
            .find(|id| arena.folder(*id).file.name == "a")
            .expect("expected folder a");

        // `a` should have no recorded child folders or files because the
        // walker stopped descending past depth 1.
        let a = arena.folder(a_id);
        assert!(
            a.children_folders.is_empty() && a.children_files.is_empty(),
            "a should be empty when max_depth=1, got {} folders + {} files",
            a.children_folders.len(),
            a.children_files.len()
        );
    }

    #[test]
    fn test_default_max_depth_is_bounded() {
        let config = ScanConfig::default();
        assert_eq!(
            config.max_depth,
            Some(DEFAULT_MAX_DEPTH),
            "default scanner config must cap recursion to prevent stack overflow"
        );
    }

    #[test]
    fn test_scan_folder_sizes_accumulate() {
        let temp = create_test_fs();
        let config = ScanConfig::default();
        let arena = scan_directory(temp.path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        // Root size should equal sum of all files
        assert!(root.file.size > 0);

        // Each subfolder should have a size > 0
        for &dir_id in &root.children_folders {
            let dir = arena.folder(dir_id);
            assert!(
                dir.file.size > 0,
                "Directory {} should have size > 0",
                dir.file.name
            );
        }
    }

    #[test]
    fn test_scan_files_sorted_by_size() {
        let temp = TempDir::new().unwrap();
        temp.child("small.txt").write_str("a").unwrap();
        temp.child("medium.txt").write_str("aaaa").unwrap();
        temp.child("large.txt").write_str("aaaaaaa").unwrap();

        let config = ScanConfig::default();
        let arena = scan_directory(temp.path(), &config).unwrap();

        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        // Files should be sorted by size descending
        for i in 1..root.children_files.len() {
            let prev = arena.file(root.children_files[i - 1]).size;
            let curr = arena.file(root.children_files[i]).size;
            assert!(
                prev >= curr,
                "Files not sorted: {} ({}) before {} ({})",
                arena.file(root.children_files[i - 1]).name,
                prev,
                arena.file(root.children_files[i]).name,
                curr
            );
        }
    }
}

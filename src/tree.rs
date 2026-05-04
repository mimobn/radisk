use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Ordering applied when listing a folder's children. Stable across
/// sessions via the `[display].sort_mode` config key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    /// Largest first — the default since v0.1; matches FileLight/ncdu.
    #[default]
    SizeDesc,
    /// Smallest first — useful when looking for the long tail of empty
    /// or near-empty entries before pruning.
    SizeAsc,
    /// Case-insensitive ASCII alphabetical — most file-manager-y.
    NameAsc,
}

impl SortMode {
    /// Cycle to the next sort mode. The order is the most useful
    /// rotation in practice: size desc → size asc → name → back to
    /// size desc.
    pub fn next(self) -> Self {
        match self {
            SortMode::SizeDesc => SortMode::SizeAsc,
            SortMode::SizeAsc => SortMode::NameAsc,
            SortMode::NameAsc => SortMode::SizeDesc,
        }
    }

    /// Short human-readable label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            SortMode::SizeDesc => "size↓",
            SortMode::SizeAsc => "size↑",
            SortMode::NameAsc => "name",
        }
    }
}

/// Unique identifier for files in the arena
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FileId(pub usize);

/// Unique identifier for folders in the arena
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FolderId(pub usize);

/// A file entry in the tree
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct File {
    pub name: String,
    pub size: u64,
    pub parent: Option<FolderId>,
    pub path: PathBuf,
    /// Unix inode captured at scan time. `None` on non-Unix
    /// platforms or when the metadata call failed. Used by the
    /// delete path to refuse acting on a path that has been
    /// swapped out from under the user since the dialog opened.
    /// `#[serde(default)]` so older snapshots without this field
    /// load cleanly with `inode = None`.
    #[serde(default)]
    pub inode: Option<u64>,
}

/// A folder entry in the tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub file: File,
    pub children_files: Vec<FileId>,
    pub children_folders: Vec<FolderId>,
    pub child_count: u32,
}

/// Arena allocator for the file tree
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TreeArena {
    files: Vec<File>,
    folders: Vec<Folder>,
    root: Option<FolderId>,
}

impl TreeArena {
    pub fn new() -> Self {
        Self {
            files: Vec::new(),
            folders: Vec::new(),
            root: None,
        }
    }

    pub fn add_file(&mut self, file: File) -> FileId {
        let id = FileId(self.files.len());
        self.files.push(file);
        id
    }

    pub fn add_folder(&mut self, folder: Folder) -> FolderId {
        let id = FolderId(self.folders.len());
        self.folders.push(folder);
        id
    }

    pub fn set_root(&mut self, id: FolderId) {
        self.root = Some(id);
    }

    pub fn root(&self) -> Option<FolderId> {
        self.root
    }

    pub fn file(&self, id: FileId) -> &File {
        &self.files[id.0]
    }

    pub fn folder(&self, id: FolderId) -> &Folder {
        &self.folders[id.0]
    }

    pub fn folder_mut(&mut self, id: FolderId) -> &mut Folder {
        &mut self.folders[id.0]
    }

    #[allow(dead_code)]
    pub fn files(&self) -> &[File] {
        &self.files
    }

    #[allow(dead_code)]
    pub fn folders(&self) -> &[Folder] {
        &self.folders
    }

    /// Get all items (files and folders) of a folder, sorted by size
    /// descending. Convenience wrapper around
    /// [`Self::folder_items_sorted`] for callers that don't care about
    /// the user-selected sort order (e.g. the radial layout, which has
    /// always been size-driven).
    pub fn folder_items(&self, folder_id: FolderId) -> Vec<TreeItem> {
        self.folder_items_sorted(folder_id, SortMode::SizeDesc)
    }

    /// Get all items (files and folders) of a folder, sorted by `mode`.
    pub fn folder_items_sorted(&self, folder_id: FolderId, mode: SortMode) -> Vec<TreeItem> {
        let folder = &self.folders[folder_id.0];
        let mut items: Vec<TreeItem> =
            Vec::with_capacity(folder.children_files.len() + folder.children_folders.len());

        for &fid in &folder.children_files {
            items.push(TreeItem::File(fid, self.files[fid.0].size));
        }
        for &fid in &folder.children_folders {
            items.push(TreeItem::Folder(fid, self.folders[fid.0].file.size));
        }

        match mode {
            SortMode::SizeDesc => {
                items.sort_by_key(|item| std::cmp::Reverse(item.size()));
            }
            SortMode::SizeAsc => {
                items.sort_by_key(|item| item.size());
            }
            SortMode::NameAsc => {
                items.sort_by_key(|item| self.item_name_lowercase(item));
            }
        }
        items
    }

    fn item_name_lowercase(&self, item: &TreeItem) -> String {
        match item {
            TreeItem::File(id, _) => self.files[id.0].name.to_ascii_lowercase(),
            TreeItem::Folder(id, _) => self.folders[id.0].file.name.to_ascii_lowercase(),
        }
    }

    /// Look up a folder by its on-disk path, walking down from the
    /// arena's root. Returns `None` when the path is outside the
    /// arena (e.g. the user is trying to navigate to a sibling of
    /// the original scan root) or when the walker hasn't reached
    /// the path yet during a live scan.
    ///
    /// This is the fallback for `App::navigate_into(path)` when
    /// only the path is known. The hot navigation path
    /// (`navigate_into_id`) skips this lookup entirely by carrying
    /// the `FolderId` from sidebar / segment data.
    pub fn find_folder_by_path(&self, path: &std::path::Path) -> Option<FolderId> {
        let root_id = self.root()?;
        let root_path = self.folder(root_id).file.path.as_path();
        let suffix = path.strip_prefix(root_path).ok()?;
        let mut current = root_id;
        for component in suffix.components() {
            let name = component.as_os_str().to_string_lossy();
            let folder = self.folder(current);
            let next = folder
                .children_folders
                .iter()
                .copied()
                .find(|id| self.folder(*id).file.name == name)?;
            current = next;
        }
        Some(current)
    }

    /// Get total file count in a folder (recursive)
    pub fn total_file_count(&self, folder_id: FolderId) -> u64 {
        let folder = &self.folders[folder_id.0];
        let mut count = folder.children_files.len() as u64;

        for &child_id in &folder.children_folders {
            count += self.total_file_count(child_id);
        }

        count
    }

    /// Create a simple test tree for testing
    #[cfg(test)]
    pub fn create_test_tree() -> Self {
        let mut arena = Self::new();

        let root_path = PathBuf::from("/test");
        let root_file = File {
            name: "test".to_string(),
            size: 0,
            parent: None,
            path: root_path.clone(),
            ..Default::default()
        };
        let root_folder = Folder {
            file: root_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 0,
        };
        let root_id = arena.add_folder(root_folder);
        arena.set_root(root_id);

        // Add files
        let f1 = File {
            name: "big.txt".to_string(),
            size: 1000,
            parent: Some(root_id),
            path: root_path.join("big.txt"),
            ..Default::default()
        };
        let f1_id = arena.add_file(f1);

        let f2 = File {
            name: "small.txt".to_string(),
            size: 100,
            parent: Some(root_id),
            path: root_path.join("small.txt"),
            ..Default::default()
        };
        let f2_id = arena.add_file(f2);

        // Add subfolder
        let sub_path = root_path.join("subdir");
        let sub_file = File {
            name: "subdir".to_string(),
            size: 500,
            parent: Some(root_id),
            path: sub_path.clone(),
            ..Default::default()
        };
        let sub_folder = Folder {
            file: sub_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 0,
        };
        let sub_id = arena.add_folder(sub_folder);

        let f3 = File {
            name: "nested.txt".to_string(),
            size: 500,
            parent: Some(sub_id),
            path: sub_path.join("nested.txt"),
            ..Default::default()
        };
        let f3_id = arena.add_file(f3);

        // Wire up children
        let root = arena.folder_mut(root_id);
        root.children_files.push(f1_id);
        root.children_files.push(f2_id);
        root.children_folders.push(sub_id);
        root.file.size = 1600;
        root.child_count = 3;

        let sub = arena.folder_mut(sub_id);
        sub.children_files.push(f3_id);
        sub.child_count = 1;

        arena
    }
}

impl Default for TreeArena {
    fn default() -> Self {
        Self::new()
    }
}

/// Represents either a file or folder in tree traversal
#[derive(Debug, Clone, Copy)]
pub enum TreeItem {
    File(FileId, u64),
    Folder(FolderId, u64),
}

impl TreeItem {
    pub fn size(&self) -> u64 {
        match self {
            TreeItem::File(_, s) | TreeItem::Folder(_, s) => *s,
        }
    }

    pub fn is_folder(&self) -> bool {
        matches!(self, TreeItem::Folder(..))
    }
}

/// Format file size to human-readable string
pub fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    const TB: u64 = GB * 1024;

    if size >= TB {
        format!("{:.1} TB", size as f64 / TB as f64)
    } else if size >= GB {
        format!("{:.1} GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1} MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1} KB", size as f64 / KB as f64)
    } else {
        format!("{} B", size)
    }
}

/// Magnitude bucket used by the UI to colour-code sizes so the
/// eye picks out the genuinely large entries at a glance.
///
/// - `Tiny`   (< 1 MiB)  — dim grey, mostly metadata
/// - `Small`  (< 100 MiB) — default fg, "normal" files
/// - `Medium` (< 1 GiB)   — cyan, worth a look
/// - `Large`  (< 10 GiB)  — yellow, watch this
/// - `Huge`   (≥ 10 GiB)  — red, this is what's eating your disk
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMagnitude {
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
}

impl SizeMagnitude {
    pub fn classify(size: u64) -> Self {
        const MB: u64 = 1024 * 1024;
        const GB: u64 = MB * 1024;
        if size >= 10 * GB {
            SizeMagnitude::Huge
        } else if size >= GB {
            SizeMagnitude::Large
        } else if size >= 100 * MB {
            SizeMagnitude::Medium
        } else if size >= MB {
            SizeMagnitude::Small
        } else {
            SizeMagnitude::Tiny
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_creation() {
        let file = File {
            name: "test.txt".to_string(),
            size: 1024,
            parent: None,
            path: PathBuf::from("/test.txt"),
            ..Default::default()
        };
        assert_eq!(file.name, "test.txt");
        assert_eq!(file.size, 1024);
    }

    #[test]
    fn test_folder_creation() {
        let file = File {
            name: "mydir".to_string(),
            size: 0,
            parent: None,
            path: PathBuf::from("/mydir"),
            ..Default::default()
        };
        let folder = Folder {
            file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 0,
        };
        assert_eq!(folder.file.name, "mydir");
        assert_eq!(folder.children_files.len(), 0);
        assert_eq!(folder.child_count, 0);
    }

    #[test]
    fn test_arena_add_and_retrieve() {
        let mut arena = TreeArena::new();

        let file = File {
            name: "a.txt".to_string(),
            size: 100,
            parent: None,
            path: PathBuf::from("/a.txt"),
            ..Default::default()
        };
        let fid = arena.add_file(file);
        assert_eq!(fid, FileId(0));
        assert_eq!(arena.file(fid).name, "a.txt");
        assert_eq!(arena.file(fid).size, 100);
    }

    #[test]
    fn test_size_accumulation() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let root = arena.folder(root_id);

        // Root should have accumulated all sizes: 1000 + 100 + 500 = 1600
        assert_eq!(root.file.size, 1600);
        assert_eq!(root.child_count, 3);
    }

    #[test]
    fn find_folder_by_path_resolves_arena_root() {
        let arena = TreeArena::create_test_tree();
        let root = arena.root().unwrap();
        let by_path = arena.find_folder_by_path(&PathBuf::from("/test")).unwrap();
        assert_eq!(by_path.0, root.0);
    }

    #[test]
    fn find_folder_by_path_resolves_descendant() {
        let arena = TreeArena::create_test_tree();
        let by_path = arena
            .find_folder_by_path(&PathBuf::from("/test/subdir"))
            .expect("subdir should be reachable");
        assert_eq!(arena.folder(by_path).file.name, "subdir");
    }

    #[test]
    fn find_folder_by_path_returns_none_for_outside_paths() {
        let arena = TreeArena::create_test_tree();
        assert!(arena
            .find_folder_by_path(&PathBuf::from("/somewhere/else"))
            .is_none());
        assert!(arena
            .find_folder_by_path(&PathBuf::from("/test/missing"))
            .is_none());
    }

    #[test]
    fn test_children_sorted_by_size() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let items = arena.folder_items(root_id);

        assert_eq!(items.len(), 3);
        // Should be sorted descending: big.txt(1000), subdir(500), small.txt(100)
        assert_eq!(items[0].size(), 1000);
        assert_eq!(items[1].size(), 500);
        assert_eq!(items[2].size(), 100);
    }

    #[test]
    fn folder_items_sorted_size_asc_reverses_order() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let items = arena.folder_items_sorted(root_id, SortMode::SizeAsc);
        assert_eq!(items[0].size(), 100);
        assert_eq!(items[1].size(), 500);
        assert_eq!(items[2].size(), 1000);
    }

    #[test]
    fn folder_items_sorted_name_is_alphabetical_case_insensitive() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let items = arena.folder_items_sorted(root_id, SortMode::NameAsc);

        // Resolve each item to its display name and check ordering.
        let names: Vec<String> = items
            .iter()
            .map(|item| match item {
                TreeItem::File(id, _) => arena.file(*id).name.clone(),
                TreeItem::Folder(id, _) => arena.folder(*id).file.name.clone(),
            })
            .collect();
        let mut sorted = names.clone();
        sorted.sort_by_key(|s| s.to_ascii_lowercase());
        assert_eq!(names, sorted, "NameAsc must yield alphabetical order");
    }

    #[test]
    fn sort_mode_cycles() {
        assert_eq!(SortMode::SizeDesc.next(), SortMode::SizeAsc);
        assert_eq!(SortMode::SizeAsc.next(), SortMode::NameAsc);
        assert_eq!(SortMode::NameAsc.next(), SortMode::SizeDesc);
        assert_eq!(SortMode::default(), SortMode::SizeDesc);
    }

    #[test]
    fn test_folder_items_contains_both_types() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let items = arena.folder_items(root_id);

        let file_count = items.iter().filter(|i| !i.is_folder()).count();
        let folder_count = items.iter().filter(|i| i.is_folder()).count();

        assert_eq!(file_count, 2);
        assert_eq!(folder_count, 1);
    }

    #[test]
    fn test_empty_folder_items() {
        let mut arena = TreeArena::new();

        let file = File {
            name: "empty".to_string(),
            size: 0,
            parent: None,
            path: PathBuf::from("/empty"),
            ..Default::default()
        };
        let folder = Folder {
            file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 0,
        };
        let id = arena.add_folder(folder);

        let items = arena.folder_items(id);
        assert!(items.is_empty());
    }

    #[test]
    fn test_tree_depth() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();
        let sub_id = arena.folder(root_id).children_folders[0];

        // Subfolder has 1 file
        let sub_items = arena.folder_items(sub_id);
        assert_eq!(sub_items.len(), 1);
        assert_eq!(sub_items[0].size(), 500);
    }

    #[test]
    fn test_total_file_count() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();

        let count = arena.total_file_count(root_id);
        assert_eq!(count, 3); // big.txt, small.txt, nested.txt
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1024), "1.0 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1048576), "1.0 MB");
        assert_eq!(format_size(1073741824), "1.0 GB");
        assert_eq!(format_size(1099511627776), "1.0 TB");
    }

    #[test]
    fn test_folder_items_preserves_parent_reference() {
        let arena = TreeArena::create_test_tree();
        let root_id = arena.root().unwrap();

        // Check that files reference the root as parent
        let root = arena.folder(root_id);
        for &fid in &root.children_files {
            assert_eq!(arena.file(fid).parent, Some(root_id));
        }
    }
}

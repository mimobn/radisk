use crate::tree::{FolderId, TreeArena, TreeItem};
use uuid::Uuid;

/// Constants matching FileLight's radialMap.h
pub const MAX_DEGREE: u32 = 5760; // 360 * 16 (16ths of degrees)
pub const DEGREE_FACTOR: u32 = 16;
pub const DEFAULT_RING_DEPTH: usize = 5;
#[allow(dead_code)]
pub const MIN_RING_DEPTH: usize = 0;

/// A segment in the radial map (port of RadialMap::Segment)
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Segment {
    pub uuid: Uuid,
    pub name: String,
    pub size: u64,
    pub start_angle: u32,  // in 16ths of degrees
    pub angle_length: u32, // in 16ths of degrees
    pub is_folder: bool,
    pub is_fake: bool,   // true for FilesGroup
    pub file_count: u64, // for folders, number of children
    pub path: String,
    pub depth: usize,
    pub has_hidden_children: bool,
}

impl Segment {
    pub fn end_angle(&self) -> u32 {
        self.start_angle + self.angle_length
    }

    pub fn contains_angle(&self, angle: u32) -> bool {
        angle >= self.start_angle && angle < self.end_angle()
    }

    /// Convert to degrees (for rendering)
    pub fn start_degrees(&self) -> f64 {
        self.start_angle as f64 / DEGREE_FACTOR as f64
    }

    pub fn sweep_degrees(&self) -> f64 {
        self.angle_length as f64 / DEGREE_FACTOR as f64
    }

    #[allow(dead_code)]
    pub fn end_degrees(&self) -> f64 {
        self.end_angle() as f64 / DEGREE_FACTOR as f64
    }
}

/// A ring level containing multiple segments
#[derive(Debug, Clone)]
pub struct RingLevel {
    pub depth: usize,
    pub segments: Vec<Segment>,
    pub inner_radius: f64,
    pub outer_radius: f64,
}

/// The complete radial map signature
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct RadialMap {
    pub rings: Vec<RingLevel>,
    pub center_radius: f64,
    pub ring_breadth: f64,
    pub root_size: u64,
    pub root_name: String,
    pub root_path: String,
    pub root_file_count: u64,
}

/// Configuration for the radial map
#[derive(Debug, Clone)]
pub struct RadialConfig {
    pub ring_depth: usize,
    pub terminal_width: u16,
    pub terminal_height: u16,
    pub show_small_files: bool,
    pub small_file_factor: u64, // multiplier for minimum visible size
}

impl Default for RadialConfig {
    fn default() -> Self {
        Self {
            ring_depth: DEFAULT_RING_DEPTH,
            terminal_width: 80,
            terminal_height: 24,
            show_small_files: true,
            small_file_factor: 6,
        }
    }
}

/// Build the radial map from a tree
pub fn build_radial_map(arena: &TreeArena, root_id: FolderId, config: &RadialConfig) -> RadialMap {
    let root = arena.folder(root_id);
    let root_size = root.file.size;
    let root_name = root.file.name.clone();
    let root_path = root.file.path.to_string_lossy().into_owned();
    let root_file_count = arena.total_file_count(root_id);

    // Calculate available canvas space
    // Layout: sidebar takes 25%, status bar takes 2 rows
    // Canvas has borders (2 cells total)
    let sidebar_width = (config.terminal_width as f64 * 0.25).ceil() as u16;
    let canvas_inner_width = config
        .terminal_width
        .saturating_sub(sidebar_width)
        .saturating_sub(2);
    let canvas_inner_height = config.terminal_height.saturating_sub(2).saturating_sub(2); // status bar + borders

    // Braille resolution: 2 dots wide, 4 dots tall per cell
    let pixel_width = canvas_inner_width as f64 * 2.0;
    let pixel_height = canvas_inner_height as f64 * 4.0;

    // Use the smaller dimension to ensure circle fits
    let available_pixels = pixel_width.min(pixel_height);

    // Calculate ring_breadth to fit within available space
    // Total radius = center_radius + (depth + 1) * ring_breadth
    // For center: radius = ring_breadth * 0.5
    // Total = 0.5 * ring_breadth + (depth + 1) * ring_breadth = ring_breadth * (depth + 1.5)
    let total_depth_factor = config.ring_depth as f64 + 1.5;
    let max_ring_breadth = (available_pixels * 0.3) / total_depth_factor; // 30% of available space
    let ring_breadth = max_ring_breadth.max(2.0); // Minimum 2 pixels

    // Center radius is half a ring breadth
    let center_radius = ring_breadth * 0.5;

    // Calculate visibility limits per depth (minimum file size to show)
    let mut limits: Vec<u64> = Vec::with_capacity(config.ring_depth + 1);
    let pi2b = std::f64::consts::PI * 4.0 * ring_breadth;
    for depth in 0..=config.ring_depth {
        let limit = if pi2b * (depth as f64 + 1.0) > 0.0 {
            (root_size as f64 / (pi2b * (depth as f64 + 1.0))) as u64
        } else {
            1
        };
        limits.push(limit.max(1));
    }

    // Build rings
    let mut rings: Vec<RingLevel> = Vec::with_capacity(config.ring_depth + 1);

    for depth in 0..=config.ring_depth {
        let inner_radius = center_radius + depth as f64 * ring_breadth;
        let outer_radius = inner_radius + ring_breadth;

        let segments = if depth == 0 {
            // First ring: segments from root's direct children
            build_segments_for_folder(
                arena, root_id, root_size, 0, MAX_DEGREE, depth, &limits, config,
            )
        } else {
            // Inner rings: build from parent segments
            let parent_ring = &rings[depth - 1];
            let mut all_segments = Vec::new();

            for parent_seg in &parent_ring.segments {
                if parent_seg.is_folder && !parent_seg.is_fake {
                    // Find the folder in the arena
                    if let Some(folder_id) = find_folder_by_path(arena, root_id, &parent_seg.path) {
                        let child_segments = build_segments_for_folder(
                            arena,
                            folder_id,
                            root_size,
                            parent_seg.start_angle,
                            parent_seg.start_angle + parent_seg.angle_length,
                            depth,
                            &limits,
                            config,
                        );
                        all_segments.extend(child_segments);
                    }
                }
            }

            all_segments
        };

        rings.push(RingLevel {
            depth,
            segments,
            inner_radius,
            outer_radius,
        });
    }

    // Mark segments that have hidden children
    for depth in 0..rings.len() {
        // Collect paths from next ring first to avoid borrow issues
        let next_ring_paths: Vec<String> = if depth < rings.len() - 1 {
            rings[depth + 1]
                .segments
                .iter()
                .map(|s| s.path.clone())
                .collect()
        } else {
            Vec::new()
        };

        for seg in &mut rings[depth].segments {
            if seg.is_folder && !seg.is_fake && !next_ring_paths.is_empty() {
                // Check if any child segments exist in next ring
                let child_exists = next_ring_paths
                    .iter()
                    .any(|p| p.starts_with(&seg.path) && *p != seg.path);
                seg.has_hidden_children = !child_exists;
            }
        }
    }

    RadialMap {
        rings,
        center_radius,
        ring_breadth,
        root_size,
        root_name,
        root_path,
        root_file_count,
    }
}

/// Build segments for a folder's direct children.
///
/// Eight parameters is unavoidable here: the recursive layout has to thread
/// the arena, the current angular slice, depth-indexed visibility limits, and
/// configuration through each level. Phase 2 will replace this with a builder
/// that owns the per-level context.
#[allow(clippy::too_many_arguments)]
fn build_segments_for_folder(
    arena: &TreeArena,
    folder_id: FolderId,
    root_size: u64,
    start_angle: u32,
    end_angle: u32,
    depth: usize,
    limits: &[u64],
    config: &RadialConfig,
) -> Vec<Segment> {
    if root_size == 0 {
        return Vec::new();
    }

    let _folder = arena.folder(folder_id);
    let items = arena.folder_items(folder_id);

    let mut segments = Vec::new();
    let mut current_angle = start_angle;
    let mut hidden_size: u64 = 0;
    let mut hidden_file_count: u64 = 0;
    let limit = limits.get(depth).copied().unwrap_or(1);
    let min_visible_size = limit * config.small_file_factor;

    for item in &items {
        let item_size = item.size();

        // Check if item is too small to show
        if item_size < min_visible_size {
            hidden_size += item_size;
            hidden_file_count += 1;
            if item.is_folder() {
                // Add child count for folders
                if let TreeItem::Folder(fid, _) = item {
                    hidden_file_count += arena.total_file_count(*fid);
                }
            }
            continue;
        }

        // Calculate angle length proportional to size
        let angle_length =
            ((MAX_DEGREE as f64 * item_size as f64 / root_size as f64) as u32).max(1);

        if current_angle + angle_length > end_angle {
            // Not enough room, add to hidden
            hidden_size += item_size;
            hidden_file_count += 1;
            continue;
        }

        let (name, is_folder, file_count, path) = match item {
            TreeItem::File(fid, _) => {
                let f = arena.file(*fid);
                (
                    f.name.clone(),
                    false,
                    0,
                    f.path.to_string_lossy().into_owned(),
                )
            }
            TreeItem::Folder(fid, _) => {
                let f = arena.folder(*fid);
                (
                    f.file.name.clone(),
                    true,
                    arena.total_file_count(*fid),
                    f.file.path.to_string_lossy().into_owned(),
                )
            }
        };

        segments.push(Segment {
            uuid: Uuid::new_v4(),
            name,
            size: item_size,
            start_angle: current_angle,
            angle_length,
            is_folder,
            is_fake: false,
            file_count,
            path,
            depth,
            has_hidden_children: false,
        });

        current_angle += angle_length;
    }

    // Create FilesGroup for hidden files if any
    if config.show_small_files && hidden_size >= limit && hidden_file_count > 0 {
        let angle_length = end_angle.saturating_sub(current_angle);
        if angle_length > 0 {
            segments.push(Segment {
                uuid: Uuid::new_v4(),
                name: format!("{} small files", hidden_file_count),
                size: hidden_size,
                start_angle: current_angle,
                angle_length,
                is_folder: false,
                is_fake: true,
                file_count: hidden_file_count,
                path: String::new(),
                depth,
                has_hidden_children: false,
            });
        }
    }

    segments
}

/// Find a folder in the arena by its path relative to root
fn find_folder_by_path(
    arena: &TreeArena,
    folder_id: FolderId,
    target_path: &str,
) -> Option<FolderId> {
    let folder = arena.folder(folder_id);
    if folder.file.path.to_string_lossy() == target_path {
        return Some(folder_id);
    }

    for &child_id in &folder.children_folders {
        if let Some(found) = find_folder_by_path(arena, child_id, target_path) {
            return Some(found);
        }
    }

    None
}

/// Get all segments from the radial map (flattened)
#[allow(dead_code)]
pub fn all_segments(map: &RadialMap) -> Vec<&Segment> {
    map.rings.iter().flat_map(|r| r.segments.iter()).collect()
}

/// Find a segment by UUID
#[allow(dead_code)]
pub fn find_segment<'a>(map: &'a RadialMap, uuid: &Uuid) -> Option<&'a Segment> {
    all_segments(map).into_iter().find(|s| s.uuid == *uuid)
}

/// Find segment at a given angle and radius
#[allow(dead_code)]
pub fn find_segment_at(map: &RadialMap, angle_degrees: f64, radius: f64) -> Option<&Segment> {
    // Convert angle to 16ths of degrees
    let angle_16ths = ((angle_degrees * DEGREE_FACTOR as f64) as u32) % MAX_DEGREE;

    // Find which ring this radius falls in
    for ring in map.rings.iter().rev() {
        if radius >= ring.inner_radius && radius < ring.outer_radius {
            // Find segment in this ring
            for seg in &ring.segments {
                if seg.contains_angle(angle_16ths) {
                    return Some(seg);
                }
            }
        }
    }

    // Check if it's in the center
    if radius < map.center_radius {
        return None; // Center circle (root)
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{File, Folder, TreeArena};

    fn create_test_arena_equal_files(count: usize, size_each: u64) -> (TreeArena, FolderId) {
        let mut arena = TreeArena::new();
        let root_file = File {
            name: "root".to_string(),
            size: count as u64 * size_each,
            parent: None,
            path: std::path::PathBuf::from("/root"),
            ..Default::default()
        };
        let root_folder = Folder {
            file: root_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: count as u32,
        };
        let root_id = arena.add_folder(root_folder);
        arena.set_root(root_id);

        for i in 0..count {
            let file = File {
                name: format!("file{}.txt", i),
                size: size_each,
                parent: Some(root_id),
                path: std::path::PathBuf::from(format!("/root/file{}.txt", i)),
                ..Default::default()
            };
            let fid = arena.add_file(file);
            arena.folder_mut(root_id).children_files.push(fid);
        }

        (arena, root_id)
    }

    #[test]
    fn test_single_file_angle() {
        let mut arena = TreeArena::new();
        let root_file = File {
            name: "root".to_string(),
            size: 1000,
            parent: None,
            path: std::path::PathBuf::from("/root"),
            ..Default::default()
        };
        let root_folder = Folder {
            file: root_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 1,
        };
        let root_id = arena.add_folder(root_folder);
        arena.set_root(root_id);

        let file = File {
            name: "big.txt".to_string(),
            size: 1000,
            parent: Some(root_id),
            path: std::path::PathBuf::from("/root/big.txt"),
            ..Default::default()
        };
        let fid = arena.add_file(file);
        arena.folder_mut(root_id).children_files.push(fid);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // Single file should take full circle (5760 units)
        assert!(!map.rings.is_empty());
        let first_ring = &map.rings[0];
        assert!(!first_ring.segments.is_empty());
        let seg = &first_ring.segments[0];
        assert_eq!(seg.angle_length, MAX_DEGREE);
    }

    #[test]
    fn test_equal_size_files_split_circle() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        let first_ring = &map.rings[0];
        // 4 equal files should each take 1/4 of the circle
        for seg in &first_ring.segments {
            if !seg.is_fake {
                assert_eq!(seg.angle_length, MAX_DEGREE / 4);
            }
        }
    }

    #[test]
    fn test_small_file_grouping() {
        let (arena, root_id) = create_test_arena_equal_files(10, 100);

        let config = RadialConfig {
            ring_depth: 2,
            terminal_height: 100,
            terminal_width: 100,
            small_file_factor: 100, // Large factor to force grouping
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // With large small_file_factor, files should be grouped
        let first_ring = &map.rings[0];
        let has_fake = first_ring.segments.iter().any(|s| s.is_fake);
        assert!(has_fake, "Should have a FilesGroup segment");
    }

    #[test]
    fn test_ring_depth_calculation() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            ring_depth: 3,
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        assert_eq!(map.rings.len(), 4); // depth 0, 1, 2, 3
    }

    #[test]
    fn test_ring_breadth_bounds() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            terminal_width: 200,
            terminal_height: 200,
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // Ring breadth should be positive
        assert!(map.ring_breadth > 0.0, "Ring breadth should be positive");
        assert!(map.center_radius > 0.0, "Center radius should be positive");
    }

    #[test]
    fn test_empty_directory() {
        let mut arena = TreeArena::new();
        let root_file = File {
            name: "empty".to_string(),
            size: 0,
            parent: None,
            path: std::path::PathBuf::from("/empty"),
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

        let config = RadialConfig::default();
        let map = build_radial_map(&arena, root_id, &config);

        assert_eq!(map.root_size, 0);
        for ring in &map.rings {
            assert!(ring.segments.is_empty());
        }
    }

    #[test]
    fn test_large_file_dominates() {
        let mut arena = TreeArena::new();
        let root_file = File {
            name: "root".to_string(),
            size: 1100,
            parent: None,
            path: std::path::PathBuf::from("/root"),
            ..Default::default()
        };
        let root_folder = Folder {
            file: root_file,
            children_files: Vec::new(),
            children_folders: Vec::new(),
            child_count: 2,
        };
        let root_id = arena.add_folder(root_folder);
        arena.set_root(root_id);

        // Large file: 1000, small file: 100
        let large = File {
            name: "large.bin".to_string(),
            size: 1000,
            parent: Some(root_id),
            path: std::path::PathBuf::from("/root/large.bin"),
            ..Default::default()
        };
        let fid1 = arena.add_file(large);
        arena.folder_mut(root_id).children_files.push(fid1);

        let small = File {
            name: "small.txt".to_string(),
            size: 100,
            parent: Some(root_id),
            path: std::path::PathBuf::from("/root/small.txt"),
            ..Default::default()
        };
        let fid2 = arena.add_file(small);
        arena.folder_mut(root_id).children_files.push(fid2);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        let first_ring = &map.rings[0];
        let large_seg = first_ring
            .segments
            .iter()
            .find(|s| s.name == "large.bin")
            .unwrap();
        let small_seg = first_ring
            .segments
            .iter()
            .find(|s| s.name == "small.txt")
            .unwrap();

        assert!(large_seg.angle_length > small_seg.angle_length);
    }

    #[test]
    fn test_segments_cover_full_circle() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        let first_ring = &map.rings[0];
        let total_angle: u32 = first_ring.segments.iter().map(|s| s.angle_length).sum();
        assert!(
            total_angle <= MAX_DEGREE,
            "Total angle {} exceeds MAX_DEGREE {}",
            total_angle,
            MAX_DEGREE
        );
    }

    #[test]
    fn test_find_segment_at() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // Find segment at angle 0, radius in first ring
        let ring = &map.rings[0];
        let radius = (ring.inner_radius + ring.outer_radius) / 2.0;

        let seg = find_segment_at(&map, 0.0, radius);
        assert!(seg.is_some());
    }

    #[test]
    fn test_ring_radii_increase() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            ring_depth: 3,
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // Each ring should have larger radii than the previous
        for i in 1..map.rings.len() {
            assert!(
                map.rings[i].inner_radius >= map.rings[i - 1].outer_radius,
                "Ring {} inner radius {} should be >= ring {} outer radius {}",
                i,
                map.rings[i].inner_radius,
                i - 1,
                map.rings[i - 1].outer_radius
            );
        }
    }

    #[test]
    fn test_segment_angle_conversion() {
        let seg = Segment {
            uuid: Uuid::new_v4(),
            name: "test".to_string(),
            size: 100,
            start_angle: 576,  // 36 degrees
            angle_length: 288, // 18 degrees
            is_folder: false,
            is_fake: false,
            file_count: 0,
            path: String::new(),
            depth: 0,
            has_hidden_children: false,
        };

        approx::assert_relative_eq!(seg.start_degrees(), 36.0);
        approx::assert_relative_eq!(seg.sweep_degrees(), 18.0);
        approx::assert_relative_eq!(seg.end_degrees(), 54.0);
    }

    #[test]
    fn test_visibility_limits() {
        let (arena, root_id) = create_test_arena_equal_files(4, 250);

        let config = RadialConfig {
            ring_depth: 3,
            small_file_factor: 1,
            ..Default::default()
        };
        let map = build_radial_map(&arena, root_id, &config);

        // Root size should match
        assert_eq!(map.root_size, 1000);
    }
}

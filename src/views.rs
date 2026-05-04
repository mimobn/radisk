//! Pluggable rendering modes for the main canvas.
//!
//! Phase 4 introduces two views — the existing `Radial` (sunburst) and a
//! new `Tree` (ncdu-style indented list with size bars). The user toggles
//! between them with the `toggle_view` action (default chord: `v`).
//!
//! The `View` enum lives outside the per-mode rendering code so the App
//! can store the active view without depending on either renderer; the
//! UI layer dispatches on `app.view` to pick the right path.
//!
//! Adding a third view (e.g. a side-by-side split) means: add a variant
//! to [`View`], extend [`View::next`] with the new transition, and add a
//! match arm in `ui::render_viewing`. The keybind doesn't need to
//! change — the toggle simply cycles through every variant.

use crate::app::App;
use crate::tree::{format_size, SizeMagnitude, TreeArena, TreeItem};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem};
use ratatui::Frame;
use std::path::PathBuf;

fn size_color(mag: SizeMagnitude) -> Color {
    match mag {
        SizeMagnitude::Tiny => Color::DarkGray,
        SizeMagnitude::Small => Color::Gray,
        SizeMagnitude::Medium => Color::Cyan,
        SizeMagnitude::Large => Color::Yellow,
        SizeMagnitude::Huge => Color::Red,
    }
}

/// Which view is currently displayed in the main (non-sidebar) area.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The radisk-original radial / sunburst visualisation.
    #[default]
    Radial,
    /// ncdu-style indented list with proportional size bars.
    Tree,
    /// Top-N largest *files* across the entire scanned tree,
    /// regardless of folder boundaries. Inspired by the partition
    /// tools' "largest files anywhere" report — answers the
    /// "what's actually eating my disk?" question in one screen.
    Largest,
}

impl View {
    /// Cycle to the next view. Wraps around so a single keybind is
    /// enough to reach every mode regardless of how many we add.
    pub fn next(self) -> Self {
        match self {
            View::Radial => View::Tree,
            View::Tree => View::Largest,
            View::Largest => View::Radial,
        }
    }

    /// Short label for the status / hint bar. Surfaced via the
    /// `cycle_view` action's transient status message; reserved as a
    /// public helper for future overlays that display the active view.
    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            View::Radial => "radial",
            View::Tree => "tree",
            View::Largest => "largest",
        }
    }
}

/// One row in the tree view, ready to be rendered as a `ListItem`.
///
/// Built from a [`TreeItem`] plus the current folder's total size, so
/// each row carries everything the renderer needs without having to
/// look back at the arena. Pulling row construction into a pure
/// function keeps it unit-testable without spinning up ratatui.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeRow {
    pub name: String,
    pub size: u64,
    pub size_label: String,
    pub percent: f32,
    pub bar: String,
    pub is_folder: bool,
}

/// Width of the proportional bar in a tree row, in cells. Picked to
/// keep the row pleasant on an 80-column terminal once the size and
/// percent prefixes are accounted for.
const TREE_BAR_WIDTH: usize = 10;

/// Build the row list for `folder_id`'s direct children in the arena.
/// Children are already size-sorted by [`TreeArena::folder_items`]; we
/// just transform each entry into a row.
pub fn build_rows(arena: &TreeArena, items: &[TreeItem]) -> Vec<TreeRow> {
    // Total size for percentage calculation: the largest child's size
    // sets the bar's "100%" so the visualisation is comparative even
    // when the focused folder itself doesn't aggregate every byte
    // (e.g. when filters are applied later). Falls back to 1 to
    // avoid division by zero on an empty folder.
    let scale = items
        .iter()
        .map(|it| match it {
            TreeItem::File(_, s) | TreeItem::Folder(_, s) => *s,
        })
        .max()
        .unwrap_or(0)
        .max(1);

    items
        .iter()
        .map(|item| match item {
            TreeItem::File(id, size) => {
                let name = arena.file(*id).name.clone();
                make_row(name, *size, false, scale)
            }
            TreeItem::Folder(id, size) => {
                let mut name = arena.folder(*id).file.name.clone();
                // Mark folders with a trailing slash, ncdu/du-style.
                name.push('/');
                make_row(name, *size, true, scale)
            }
        })
        .collect()
}

fn make_row(name: String, size: u64, is_folder: bool, scale: u64) -> TreeRow {
    let percent = (size as f64 / scale as f64) as f32 * 100.0;
    let percent = percent.clamp(0.0, 100.0);
    let filled = ((percent / 100.0) * TREE_BAR_WIDTH as f32).round() as usize;
    let filled = filled.min(TREE_BAR_WIDTH);
    let mut bar = String::with_capacity(TREE_BAR_WIDTH);
    for _ in 0..filled {
        bar.push('█');
    }
    for _ in filled..TREE_BAR_WIDTH {
        bar.push('░');
    }
    TreeRow {
        size_label: format_size(size),
        size,
        percent,
        bar,
        is_folder,
        name,
    }
}

/// Render the tree view into `area`. Reuses the same selection state
/// as the sidebar (`app.sidebar_index`), so cursor position and
/// keyboard navigation stay consistent across views.
pub fn render_tree(f: &mut Frame, app: &App, area: Rect) {
    let Some(arena) = app.arena.as_ref() else {
        let placeholder = ratatui::widgets::Paragraph::new("No data").block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Tree "),
        );
        f.render_widget(placeholder, area);
        return;
    };

    let items = app.sidebar_items();
    let rows = build_rows(arena, &items);

    let list_items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let icon = if row.is_folder { "▸ " } else { "· " };
            let size_fg = size_color(SizeMagnitude::classify(row.size));
            let mut spans = vec![
                Span::styled(
                    format!("{:>5.1}% ", row.percent),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{} ", row.bar), Style::default().fg(size_fg)),
                Span::styled(
                    format!("{:>10} ", row.size_label),
                    Style::default().fg(size_fg),
                ),
                Span::styled(
                    icon,
                    if row.is_folder {
                        Style::default().fg(Color::LightCyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    },
                ),
                Span::styled(
                    row.name.clone(),
                    if row.is_folder {
                        Style::default()
                            .fg(Color::LightCyan)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
            ];
            // Highlight the selected row.
            if i == app.sidebar_index {
                for span in &mut spans {
                    span.style = span.style.bg(Color::DarkGray);
                }
            }
            ListItem::new(Line::from(spans))
        })
        .collect();

    let title = app
        .current_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/".to_string());

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    format!(" ▸ Tree: {} ", title),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::default().fg(Color::White));

    f.render_widget(list, area);
}

/// One row in the largest-files view. Pure data so the row builder
/// can be unit-tested without ratatui.
#[derive(Debug, Clone, PartialEq)]
pub struct LargestRow {
    pub path: PathBuf,
    pub size: u64,
    pub size_label: String,
    pub bar: String,
    pub percent: f32,
}

/// Number of largest files surfaced in the view. Picked to fit on a
/// modest terminal without scrolling; the renderer truncates to the
/// available rows automatically.
pub const LARGEST_LIMIT: usize = 100;

/// Build the top-N largest *files* (not folders) across the entire
/// arena. Sorted by size descending. Bar percentage is relative to
/// the largest file in the result (so the leader always reads 100%).
pub fn build_largest_rows(arena: &TreeArena, limit: usize) -> Vec<LargestRow> {
    let mut all: Vec<(PathBuf, u64)> = arena
        .files()
        .iter()
        .map(|f| (f.path.clone(), f.size))
        .collect();
    all.sort_by_key(|(_, s)| std::cmp::Reverse(*s));
    all.truncate(limit);

    let scale = all.first().map(|(_, s)| *s).unwrap_or(0).max(1);

    all.into_iter()
        .map(|(path, size)| {
            let percent = ((size as f64 / scale as f64) as f32 * 100.0).clamp(0.0, 100.0);
            let filled = ((percent / 100.0) * TREE_BAR_WIDTH as f32).round() as usize;
            let filled = filled.min(TREE_BAR_WIDTH);
            let mut bar = String::with_capacity(TREE_BAR_WIDTH);
            for _ in 0..filled {
                bar.push('█');
            }
            for _ in filled..TREE_BAR_WIDTH {
                bar.push('░');
            }
            LargestRow {
                size_label: format_size(size),
                size,
                bar,
                percent,
                path,
            }
        })
        .collect()
}

/// Render the top-N largest files into `area`. Selection cursor is
/// not shared with the sidebar (the sidebar's selection is folder-
/// scoped and the global view spans folders).
pub fn render_largest(f: &mut Frame, app: &App, area: Rect) {
    let Some(arena) = app.arena.as_ref() else {
        let placeholder = ratatui::widgets::Paragraph::new("No data").block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Largest "),
        );
        f.render_widget(placeholder, area);
        return;
    };

    let rows = build_largest_rows(arena, LARGEST_LIMIT);
    if rows.is_empty() {
        let placeholder = ratatui::widgets::Paragraph::new("No files in this tree").block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Largest "),
        );
        f.render_widget(placeholder, area);
        return;
    }

    let list_items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let size_fg = size_color(SizeMagnitude::classify(row.size));
            let line = Line::from(vec![
                Span::styled(
                    format!("{:>4} ", i + 1),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(format!("{} ", row.bar), Style::default().fg(size_fg)),
                Span::styled(
                    format!("{:>10} ", row.size_label),
                    Style::default().fg(size_fg),
                ),
                Span::styled("· ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    row.path.display().to_string(),
                    Style::default().fg(Color::White),
                ),
            ]);
            ListItem::new(line)
        })
        .collect();

    let list = List::new(list_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Span::styled(
                    format!(" ▸ Largest {} files ", rows.len()),
                    Style::default()
                        .fg(Color::LightCyan)
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::default().fg(Color::White));
    f.render_widget(list, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{File, Folder, TreeArena};

    fn arena_with(files: &[(&str, u64)], folders: &[(&str, u64)]) -> (TreeArena, Vec<TreeItem>) {
        let mut arena = TreeArena::new();
        let root = arena.add_folder(Folder {
            file: File {
                name: "root".into(),
                size: 0,
                parent: None,
                path: std::path::PathBuf::from("/"),
                ..Default::default()
            },
            children_files: vec![],
            children_folders: vec![],
            child_count: 0,
        });
        arena.set_root(root);

        for (name, size) in files {
            let id = arena.add_file(File {
                name: (*name).to_string(),
                size: *size,
                parent: Some(root),
                path: std::path::PathBuf::from(name),
                ..File::default()
            });
            arena.folder_mut(root).children_files.push(id);
        }
        for (name, size) in folders {
            let id = arena.add_folder(Folder {
                file: File {
                    name: (*name).to_string(),
                    size: *size,
                    parent: Some(root),
                    path: std::path::PathBuf::from(name),
                    ..Default::default()
                },
                children_files: vec![],
                children_folders: vec![],
                child_count: 0,
            });
            arena.folder_mut(root).children_folders.push(id);
        }

        let items = arena.folder_items(root);
        (arena, items)
    }

    #[test]
    fn rows_are_sorted_by_size_descending() {
        let (arena, items) = arena_with(&[("a.txt", 50), ("b.txt", 100)], &[]);
        let rows = build_rows(&arena, &items);
        assert_eq!(rows.len(), 2);
        // The largest item is first (folder_items sorts; we preserve order).
        assert_eq!(rows[0].name, "b.txt");
        assert_eq!(rows[1].name, "a.txt");
    }

    #[test]
    fn percent_uses_largest_child_as_scale() {
        // Largest is 100 -> 100%; 50 -> 50%; 25 -> 25%.
        let (arena, items) = arena_with(&[("big", 100), ("mid", 50), ("small", 25)], &[]);
        let rows = build_rows(&arena, &items);
        assert!((rows[0].percent - 100.0).abs() < 0.01);
        assert!((rows[1].percent - 50.0).abs() < 0.01);
        assert!((rows[2].percent - 25.0).abs() < 0.01);
    }

    #[test]
    fn bar_width_is_consistent() {
        let (arena, items) = arena_with(&[("a", 10), ("b", 5), ("c", 1)], &[]);
        let rows = build_rows(&arena, &items);
        for row in &rows {
            assert_eq!(
                row.bar.chars().count(),
                TREE_BAR_WIDTH,
                "every bar should be exactly {} cells wide, got {:?}",
                TREE_BAR_WIDTH,
                row.bar
            );
        }
    }

    #[test]
    fn folder_names_get_trailing_slash() {
        let (arena, items) = arena_with(&[], &[("Pictures", 1000)]);
        let rows = build_rows(&arena, &items);
        assert_eq!(rows[0].name, "Pictures/");
        assert!(rows[0].is_folder);
    }

    #[test]
    fn empty_folder_yields_empty_rows() {
        let (arena, items) = arena_with(&[], &[]);
        let rows = build_rows(&arena, &items);
        assert!(rows.is_empty());
    }

    #[test]
    fn single_byte_files_do_not_panic_on_scale_zero() {
        // All-zero sizes used to risk a div-by-zero before the
        // `.max(1)` floor; cover the regression path.
        let (arena, items) = arena_with(&[("empty1", 0), ("empty2", 0)], &[]);
        let rows = build_rows(&arena, &items);
        for row in &rows {
            assert!(row.percent.is_finite());
            assert_eq!(row.size, 0);
        }
    }

    #[test]
    fn view_toggle_cycles() {
        assert_eq!(View::Radial.next(), View::Tree);
        assert_eq!(View::Tree.next(), View::Largest);
        assert_eq!(View::Largest.next(), View::Radial);
        assert_eq!(View::default(), View::Radial);
    }

    fn arena_with_files(files: &[(&str, u64)]) -> TreeArena {
        let mut arena = TreeArena::new();
        let root = arena.add_folder(crate::tree::Folder {
            file: crate::tree::File {
                name: "root".into(),
                size: 0,
                parent: None,
                path: PathBuf::from("/"),
                ..Default::default()
            },
            children_files: vec![],
            children_folders: vec![],
            child_count: 0,
        });
        arena.set_root(root);
        for (name, size) in files {
            let id = arena.add_file(crate::tree::File {
                name: (*name).to_string(),
                size: *size,
                parent: Some(root),
                path: PathBuf::from(format!("/{}", name)),
                ..crate::tree::File::default()
            });
            arena.folder_mut(root).children_files.push(id);
        }
        arena
    }

    #[test]
    fn largest_rows_sorted_by_size_descending() {
        let arena = arena_with_files(&[("a", 50), ("b", 100), ("c", 25)]);
        let rows = build_largest_rows(&arena, 100);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].size, 100);
        assert_eq!(rows[1].size, 50);
        assert_eq!(rows[2].size, 25);
    }

    #[test]
    fn largest_rows_respect_limit() {
        let arena = arena_with_files(&[("a", 1), ("b", 2), ("c", 3), ("d", 4), ("e", 5)]);
        let rows = build_largest_rows(&arena, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].size, 5);
        assert_eq!(rows[1].size, 4);
    }

    #[test]
    fn largest_leader_reads_100_percent() {
        let arena = arena_with_files(&[("big", 100), ("mid", 50), ("small", 1)]);
        let rows = build_largest_rows(&arena, 100);
        assert!((rows[0].percent - 100.0).abs() < 0.01);
    }

    #[test]
    fn largest_handles_empty_arena() {
        let arena = arena_with_files(&[]);
        let rows = build_largest_rows(&arena, 10);
        assert!(rows.is_empty());
    }

    #[test]
    fn largest_handles_all_zero_sizes_without_dividing_by_zero() {
        let arena = arena_with_files(&[("a", 0), ("b", 0)]);
        let rows = build_largest_rows(&arena, 10);
        for row in &rows {
            assert!(row.percent.is_finite());
        }
    }
}

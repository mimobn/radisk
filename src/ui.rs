use crate::app::{App, AppMode, Focus};
use crate::color::center_color;
use crate::tree::{format_size, SizeMagnitude, TreeItem};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

/// Pick a foreground colour from a [`SizeMagnitude`] so the eye
/// flags genuinely large entries without us having to add columns
/// or chrome. Centralised so every size-rendering surface
/// (sidebar, tree view, tooltip, status bar) agrees.
fn size_color(mag: SizeMagnitude) -> Color {
    match mag {
        SizeMagnitude::Tiny => Color::DarkGray,
        SizeMagnitude::Small => Color::Gray,
        SizeMagnitude::Medium => Color::Cyan,
        SizeMagnitude::Large => Color::Yellow,
        SizeMagnitude::Huge => Color::Red,
    }
}

/// Glyphs used across every list-style view. Kept as ASCII-fallback
/// safe characters that still render on bare consoles; modern
/// terminals draw them as expected.
const ICON_FOLDER: &str = "▸";
const ICON_FILE: &str = "·";
const ICON_HOVER: &str = "▶";
const ICON_SELECTED: &str = "✓";
const ICON_BLANK: &str = " ";

/// Main render function
pub fn render(f: &mut Frame, app: &App) {
    match app.mode {
        AppMode::Scanning => render_scanning(f, app),
        AppMode::Viewing => render_viewing(f, app),
        AppMode::Help => render_help(f, app),
        AppMode::ConfirmDelete => render_delete_confirmation(f, app),
    }
}

/// Render scanning mode.
///
/// Once Phase 21's live arena has produced a partial radial map (Some
/// children visible), we render the full Viewing layout — sidebar +
/// radial + status — so the user sees the biggest folders fill in as
/// the walker discovers them. Until then we show the original
/// "Scanning..." placeholder so an empty frame doesn't confuse new
/// users.
///
/// The status bar in either branch threads the live file/byte count
/// from `scan_progress` and the most-recently-touched path so users
/// always have a "scan is alive" cue even on huge trees.
fn render_scanning(f: &mut Frame, app: &App) {
    if app.radial_map.is_some() {
        // Partial render: same layout as Viewing mode, just with a
        // status bar that calls out we're still scanning.
        render_viewing(f, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(f.area());

    let progress_text = if let Some(ref progress) = app.scan_progress {
        format!(
            "Scanning {}...\n{} files ({})",
            app.current_path.display(),
            progress.files_scanned,
            format_size(progress.total_size)
        )
    } else {
        format!("Scanning {}...", app.current_path.display())
    };

    let progress = Paragraph::new(progress_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(Line::from(Span::styled(
                    " ▸ radisk ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))),
        )
        .style(Style::default().fg(Color::Cyan))
        .wrap(Wrap { trim: true });
    f.render_widget(progress, chunks[0]);

    let status = Paragraph::new("Press ESC or 'q' to quit")
        .block(Block::default().borders(Borders::TOP))
        .style(Style::default().fg(Color::Gray));
    f.render_widget(status, chunks[1]);
}

/// Render viewing mode
fn render_viewing(f: &mut Frame, app: &App) {
    let area = f.area();

    // Main layout: sidebar | map
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(area);

    // Sidebar
    render_sidebar(f, app, main_chunks[0]);

    // Map area with status bar
    let map_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(main_chunks[1]);

    // Main view (radial / tree / largest-files)
    match app.view {
        crate::views::View::Radial => render_radial_map(f, app, map_chunks[0]),
        crate::views::View::Tree => crate::views::render_tree(f, app, map_chunks[0]),
        crate::views::View::Largest => crate::views::render_largest(f, app, map_chunks[0]),
    }

    // Status bar
    render_status_bar(f, app, map_chunks[1]);

    // Tooltip if hovered (only when context menu is not visible)
    if !app.context_menu.visible {
        if let Some(tooltip) = app.tooltip_text() {
            render_tooltip(f, &tooltip);
        }
    }

    // Context menu (rendered last so it's on top)
    if app.context_menu.visible {
        render_context_menu(f, app);
    }
}

/// Render sidebar with file list.
///
/// All per-row data (name + path) is extracted under a single
/// `app.with_arena(...)` closure so we acquire the live-arena lock
/// at most once per frame instead of once per row. Pre-Phase-22
/// the per-row lookup hit `app.arena` directly, which is `None`
/// during scan — the user saw every row rendered as `[D] ? (size)`
/// even though the radial showed real folders.
fn render_sidebar(f: &mut Frame, app: &App, area: Rect) {
    use crate::theme::Role;
    let theme = &app.theme;

    /// Pre-resolved row data — what the renderer actually needs.
    struct Row {
        name: String,
        size: u64,
        path: std::path::PathBuf,
        is_folder: bool,
    }

    // Single arena lock for the whole row resolution. The walker
    // is held off for the few microseconds this takes; readers
    // never see a "?" placeholder again for a row that genuinely
    // exists in the live arena.
    let rows: Vec<Row> = app
        .with_arena(|arena| {
            let Some(focus) = app.focus_folder_id(arena) else {
                return Vec::new();
            };
            arena
                .folder_items_sorted(focus, app.sort_mode)
                .into_iter()
                .map(|item| match item {
                    TreeItem::File(id, size) => {
                        let f = arena.file(id);
                        Row {
                            name: f.name.clone(),
                            size,
                            path: f.path.clone(),
                            is_folder: false,
                        }
                    }
                    TreeItem::Folder(id, size) => {
                        let f = arena.folder(id);
                        Row {
                            name: f.file.name.clone(),
                            size,
                            path: f.file.path.clone(),
                            is_folder: true,
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    // Reserve a right-hand column for the size so every row has a
    // clean, aligned size column instead of "name (size)". Width
    // scales with the panel; minimum 9 cells fits "999.9 GB".
    let inner_w = area.width.saturating_sub(2) as usize;
    let size_col = 10usize.min(inner_w.saturating_sub(6));
    // Marker (1) + icon (1) + space (1) + size_col + space (1) =
    // overhead. Name gets whatever's left.
    let name_col = inner_w.saturating_sub(size_col + 4);

    let items: Vec<ListItem> = rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            // Marker in column 0: ▶ for hover, ✓ for selected,
            // blank otherwise. Selection wins because it's the
            // committed state.
            let marker = if app.selected_paths.contains(&row.path) {
                ICON_SELECTED
            } else if app.sidebar_hover_index == Some(i) {
                ICON_HOVER
            } else {
                ICON_BLANK
            };

            let icon = if row.is_folder {
                ICON_FOLDER
            } else {
                ICON_FILE
            };

            // Truncate name to fit the column with a trailing
            // ellipsis so long file names don't push the size off
            // screen.
            let name = if row.name.chars().count() > name_col {
                let mut s: String = row.name.chars().take(name_col.saturating_sub(1)).collect();
                s.push('…');
                s
            } else {
                row.name.clone()
            };

            let name_style = if row.is_folder {
                Style::default()
                    .fg(theme.color(Role::Folder))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.color(Role::File))
            };
            let size_str = format_size(row.size);
            let size_style = Style::default().fg(size_color(SizeMagnitude::classify(row.size)));

            let mut spans = vec![
                Span::styled(
                    format!("{} ", marker),
                    Style::default().fg(theme.color(Role::Folder)),
                ),
                Span::styled(format!("{} ", icon), name_style),
                Span::styled(format!("{:<width$} ", name, width = name_col), name_style),
                Span::styled(
                    format!("{:>width$}", size_str, width = size_col),
                    size_style,
                ),
            ];
            // Selection cursor: paint the row background. No more
            // UNDERLINED on hover — the leading ▶ marker carries
            // the same information without the visual noise.
            if i == app.sidebar_index {
                let bg = theme.color(Role::SelectionBg);
                for span in &mut spans {
                    span.style = span.style.bg(bg);
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

    let border_color = if app.focus == Focus::Sidebar {
        theme.color(Role::BorderFocused)
    } else {
        theme.color(Role::Border)
    };
    let title_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "▸ ",
            Style::default()
                .fg(theme.color(Role::Folder))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            title,
            Style::default()
                .fg(theme.color(Role::Foreground))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let sidebar = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(title_line)
                .border_style(Style::default().fg(border_color)),
        )
        .style(Style::default().fg(theme.color(Role::Foreground)));

    f.render_widget(sidebar, area);
}

/// Render radial map using canvas
fn render_radial_map(f: &mut Frame, app: &App, area: Rect) {
    use ratatui::symbols::Marker;
    use ratatui::widgets::canvas::Canvas;

    let Some(map) = app.radial_map.as_ref() else {
        let placeholder = Paragraph::new("No data").block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Map "),
        );
        f.render_widget(placeholder, area);
        return;
    };

    // Calculate max radius from map (this is already scaled to fit)
    let max_radius = map
        .rings
        .last()
        .map(|r| r.outer_radius)
        .unwrap_or(map.center_radius);

    // Canvas area after borders
    let inner_width = (area.width.saturating_sub(2)) as f64;
    let inner_height = (area.height.saturating_sub(2)) as f64;

    // Braille resolution: 2 dots wide, 4 dots tall per cell
    let pixel_width = inner_width * 2.0;
    let pixel_height = inner_height * 4.0;

    // Calculate bounds that will fit the map exactly
    // We need the map to fit within the canvas, so bounds should match max_radius
    // But account for aspect ratio to keep it circular
    let aspect_ratio = pixel_height / pixel_width;

    // Set bounds - the map's max_radius should fill most of the canvas
    // Use max_radius as the x bound, and scale y to maintain aspect ratio
    let x_bound = max_radius;
    let y_bound = max_radius * aspect_ratio;

    let title_line = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "▸ ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            app.current_path.display().to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ]);
    let canvas = Canvas::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(title_line)
                .border_style(if app.focus == Focus::Map {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
        )
        .marker(Marker::Braille)
        .x_bounds([-x_bound, x_bound])
        .y_bounds([-y_bound, y_bound])
        .paint(|ctx| {
            // Draw center circle first (background)
            let center_clr = center_color(&app.renderer.config);
            ctx.draw(&crate::renderer::CenterShape {
                radius: map.center_radius,
                color: center_clr.to_ratatui(),
                center_x: 0.0,
                center_y: 0.0,
            });

            // Draw segments and strokes
            let (fill_shapes, stroke_shapes, circle_shapes) = app.renderer.render_shapes(map);

            for shape in fill_shapes {
                ctx.draw(&shape);
            }
            for shape in stroke_shapes {
                ctx.draw(&shape);
            }
            for shape in circle_shapes {
                ctx.draw(&shape);
            }
        });

    f.render_widget(canvas, area);

    // Overlay center text (folder name + total size)
    let center_text = format!(
        "{}\n{}",
        map.root_name,
        crate::tree::format_size(map.root_size)
    );

    // Calculate text area size based on center circle radius
    // The diagonal of the inscribed square = 2 * radius / sqrt(2)
    let text_width = (map.center_radius * 1.2) as u16;
    let text_height = 2;
    let text_width = text_width.max(8).min(area.width.saturating_sub(4));

    // Position text in center of canvas area
    let text_area = Rect {
        x: area.x + area.width / 2 - text_width / 2,
        y: area.y + area.height / 2 - text_height / 2,
        width: text_width,
        height: text_height,
    };

    let label = Paragraph::new(center_text)
        .style(Style::default().fg(Color::White).bg(Color::Rgb(30, 30, 46)))
        .alignment(ratatui::layout::Alignment::Center);

    f.render_widget(label, text_area);
}

/// Render status bar.
///
/// Two-row layout:
/// - Row 0 = top border + status text. During scan the text leads
///   with a small spinner glyph that advances with the file count
///   so the user has a "still alive" cue even when the path
///   suffix scrolls off.
/// - Row 1 = compact key hints in dim grey.
fn render_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let scanning = matches!(app.mode, AppMode::Scanning);
    let help_text = if scanning {
        // Compact during scan — most keys are still wired (Phase
        // 23) but advertise only the essentials so the bar fits.
        "[h/l] In/Up  [j/k] Move  [v] View  [q] Quit"
    } else if app.hovered_uuid.is_some() {
        "[u/Backspace] Up  [Enter] Open  [d] Del  [+/-] Zoom  [r] Rescan  [?] Help  [q] Quit"
    } else {
        "[h/l] In/Up  [j/k] Move  [d] Del  [+/-] Zoom  [r] Rescan  [Tab] Focus  [v] View  [S] Sort  [a] Apparent  [?] Help  [q] Quit"
    };

    // Spinner during scan — frame index keys off file count so the
    // glyph advances with progress, not wall-clock.
    let spinner_glyph = if scanning {
        const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let idx = app
            .scan_progress
            .as_ref()
            .map(|p| (p.files_scanned / 32) as usize % FRAMES.len())
            .unwrap_or(0);
        FRAMES[idx]
    } else {
        "▸"
    };

    let status_line = Line::from(vec![
        Span::styled(
            format!("{} ", spinner_glyph),
            Style::default().fg(if scanning { Color::Yellow } else { Color::Cyan }),
        ),
        Span::styled(app.status_text(), Style::default().fg(Color::White)),
        Span::raw("  "),
        Span::styled(help_text, Style::default().fg(Color::DarkGray)),
    ]);

    let status = Paragraph::new(status_line).block(Block::default().borders(Borders::TOP));
    f.render_widget(status, area);
}

/// Render tooltip near cursor
fn render_tooltip(f: &mut Frame, text: &str) {
    let area = f.area();

    // Position tooltip in top-right area
    let tooltip_width = 30.min(area.width / 3);
    let tooltip_height = text.lines().count() as u16 + 2;
    let tooltip_area = Rect {
        x: area.width.saturating_sub(tooltip_width + 2),
        y: 1,
        width: tooltip_width,
        height: tooltip_height,
    };

    let tooltip = Paragraph::new(text.to_string())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .style(Style::default().fg(Color::White).bg(Color::DarkGray))
        .wrap(Wrap { trim: true });

    f.render_widget(tooltip, tooltip_area);
}

/// Render help overlay.
///
/// The previous implementation rendered the main view, then a dim
/// `Block` over the whole frame, then the help block on top. The
/// problem: `Block::render` only *styles* cells — it does not
/// reset their `symbol`, so the radial-canvas Braille glyphs and
/// the sidebar text bled through the help body.
///
/// Fix: use [`Clear`] (which writes a space to every cell of its
/// area, with default style) before each layer that needs to look
/// truly opaque. The result is a clean, readable overlay regardless
/// of what's behind it.
fn render_help(f: &mut Frame, app: &App) {
    let area = f.area();

    // 1. Background pass. The main view is still drawn underneath
    //    so the modal feels grounded; we wipe everything with Clear
    //    afterwards and tint the whole frame with a dim panel so the
    //    help foreground has an unambiguous backdrop.
    render_viewing(f, app);
    f.render_widget(Clear, area);
    let dim = Block::default().style(Style::default().bg(Color::Rgb(15, 15, 22)));
    f.render_widget(dim, area);

    // 2. Carve out the help panel. 70% wide, 80% tall so the keymap
    //    fits without wrapping at typical terminal widths.
    let help_area = centered_rect(70, 80, area);

    // 3. Wipe the panel cells specifically so the dim tint above
    //    can't leak symbol bits in either, then paint the help.
    f.render_widget(Clear, help_area);

    let key_style = Style::default()
        .fg(Color::LightCyan)
        .add_modifier(Modifier::BOLD);
    let head_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let body_style = Style::default().fg(Color::White);
    let dim_style = Style::default().fg(Color::Gray);

    let row = |chord: &str, label: &str| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(format!("{:<14}", chord), key_style),
            Span::styled(label.to_string(), body_style),
        ])
    };
    let head = |t: &str| Line::from(Span::styled(t.to_string(), head_style));
    let blank = || Line::from("");

    let help_text = vec![
        head("Navigation"),
        row("h / ← / u / ⌫", "Go to parent directory"),
        row("l / → / Enter", "Descend into hovered folder"),
        row("j / k  ↓ / ↑", "Move sidebar selection"),
        row("gg / G", "Jump to first / last item"),
        row("Ctrl-d / Ctrl-u", "Half-page down / up"),
        row("Tab", "Toggle focus (map ↔ sidebar)"),
        blank(),
        head("View"),
        row("v", "Cycle radial / tree / largest-files"),
        row("Shift+S", "Cycle sort  (size↓ → size↑ → name)"),
        row("a", "Apparent vs on-disk size (rescans)"),
        row("+ / = / -", "Zoom rings (in / in / out)"),
        blank(),
        head("Actions"),
        row("r", "Rescan"),
        row("d", "Delete (trash if trash-put / gio is installed)"),
        row("Space", "Toggle item in/out of multi-select"),
        row("Shift+D", "Delete every selected item (one confirm)"),
        row("Shift+X", "Clear multi-select"),
        row("o", "Show package owner in status bar"),
        row("?", "Show / hide this help"),
        row("q / Esc", "Quit"),
        blank(),
        head("Mouse"),
        row("Left click", "Open folder / go up (centre)"),
        row("Right click", "Open context menu"),
        row("Scroll", "Zoom rings"),
        blank(),
        Line::from(Span::styled(
            "  Press q / Esc / ? / Enter to close",
            dim_style,
        )),
        Line::from(Span::styled(
            "  Every chord above is rebindable — see docs/KEYBINDS.md",
            dim_style,
        )),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Help ")
                .title_style(head_style)
                .border_style(Style::default().fg(Color::White))
                .style(Style::default().bg(Color::Rgb(20, 20, 30))),
        )
        .style(Style::default().bg(Color::Rgb(20, 20, 30)));

    f.render_widget(help, help_area);
}

/// Render context menu popup
fn render_context_menu(f: &mut Frame, app: &App) {
    let menu = &app.context_menu;
    let items = menu.menu_items();
    let area = f.area();

    // Calculate menu dimensions
    let menu_width: u16 = 25;
    let menu_height: u16 = items.len() as u16 + 2; // +2 for borders

    // Position menu at cursor, but keep within screen bounds
    let menu_x = menu.x.min(area.width.saturating_sub(menu_width));
    let menu_y = menu.y.min(area.height.saturating_sub(menu_height));

    let menu_area = Rect {
        x: menu_x,
        y: menu_y,
        width: menu_width,
        height: menu_height,
    };

    // Build menu items
    let menu_items: Vec<ListItem> = items
        .iter()
        .enumerate()
        .map(|(i, action)| {
            let style = if i == menu.selected_index || menu.hovered_index == Some(i) {
                // Yellow background for selected/hovered
                Style::default().fg(Color::White).bg(Color::Yellow)
            } else {
                // Normal
                Style::default().fg(Color::White)
            };
            let prefix = if i == menu.selected_index || menu.hovered_index == Some(i) {
                "> "
            } else {
                "  "
            };
            let content = format!("{}{}", prefix, action.label());
            ListItem::new(content).style(style)
        })
        .collect();

    let menu_widget = List::new(menu_items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(format!(" {} ", menu.segment_name))
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .style(Style::default().bg(Color::DarkGray));

    f.render_widget(menu_widget, menu_area);
}

/// Create a centered rectangle
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

/// Render delete confirmation dialog
fn render_delete_confirmation(f: &mut Frame, app: &App) {
    // First render the main view behind
    render_viewing(f, app);

    // Wipe the cells (Block-only style does not reset symbols, so
    // the radial Braille glyphs would otherwise bleed through).
    let area = f.area();
    f.render_widget(Clear, area);
    let dim = Block::default().style(Style::default().bg(Color::Rgb(15, 15, 22)));
    f.render_widget(dim, area);

    let delete_area = centered_rect(40, 25, area);
    f.render_widget(Clear, delete_area);

    // For batch deletes, the dialog summarises the selection
    // instead of pointing at a single missing path.
    let batch_count = app.selected_paths.len();
    let path_display = if batch_count > 0 {
        format!("{} selected entries", batch_count)
    } else {
        app.delete_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "unknown".to_string())
    };

    let type_text = if batch_count > 0 {
        "selection"
    } else if app.delete_is_folder {
        "folder"
    } else {
        "file"
    };

    // Truncate path if too long
    let max_path_len = (delete_area.width as usize).saturating_sub(4);
    let path_display = if path_display.len() > max_path_len {
        format!(
            "...{}",
            &path_display[path_display.len() - max_path_len + 3..]
        )
    } else {
        path_display
    };

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Confirm Delete",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("Delete this {}?", type_text),
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(Span::styled(
            path_display,
            Style::default().fg(Color::White),
        )),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " [Y]es ",
                if app.delete_selected {
                    Style::default().fg(Color::Black).bg(Color::Gray)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
            Span::raw("   "),
            Span::styled(
                "[N]o",
                if !app.delete_selected {
                    Style::default().fg(Color::Black).bg(Color::Gray)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ]),
        Line::from(""),
    ];

    let confirm = Paragraph::new(lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .title(" Delete ")
                .border_style(Style::default().fg(Color::Red)),
        )
        .style(Style::default().bg(Color::Black))
        .alignment(ratatui::layout::Alignment::Center);

    f.render_widget(confirm, delete_area);
}

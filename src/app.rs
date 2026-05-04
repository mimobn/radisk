use crate::color::ColorConfig;
use crate::config::Config;
use crate::context_menu::{ContextMenu, MenuAction};
use crate::delete::{delete as delete_path, DeleteStrategy};
use crate::keybinds::{Action, Keybinds};
use crate::radial::{build_radial_map, RadialConfig, RadialMap, Segment};
use crate::renderer::{CanvasCoords, RadialRenderer};
use crate::scanner::ScanProgress;
use crate::scanner_streaming::{scan_streaming, ScanEvent, ScanHandle};
use crate::theme::Theme;
use crate::tree::{format_size, FolderId, SortMode, TreeArena, TreeItem};
use crate::views::View;
use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use std::path::PathBuf;
use uuid::Uuid;

/// Application mode
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppMode {
    Scanning,
    Viewing,
    Help,
    ConfirmDelete,
}

/// Application focus
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    Map,
    Sidebar,
}

/// Navigation history entry
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct NavEntry {
    pub path: PathBuf,
    pub root_id: FolderId,
}

/// Application state
pub struct App {
    pub mode: AppMode,
    pub focus: Focus,
    pub arena: Option<TreeArena>,
    pub current_path: PathBuf,
    pub ring_depth: usize,
    /// Resolved user configuration. Owned by the App because most config
    /// reads are not in performance-critical paths and a single owner
    /// avoids cloning sub-structs to event handlers.
    pub config: Config,
    /// Keybind table, derived from `config.keybinds` at construction
    /// time. Looked up once per key event in `handle_viewing_key`.
    pub keybinds: Keybinds,
    /// Resolved colour palette for UI chrome. Built once from
    /// `config.colors`; passed by reference to renderers.
    pub theme: Theme,
    /// Multi-select set keyed by absolute path. Used by the batch
    /// delete flow (`delete_selected` action) so a user can
    /// space-mark several entries from the sidebar and delete them
    /// in one confirmation dialog.
    pub selected_paths: std::collections::HashSet<PathBuf>,
    /// Cache of pacman ownership lookups, keyed by absolute path so
    /// repeated queries on the same row are free. Populated lazily
    /// by the `show_owner` action.
    pub owner_cache: std::collections::HashMap<PathBuf, crate::ownership::Owner>,
    /// Currently active main-area view (radial, tree, …). Toggled by
    /// the `toggle_view` action.
    pub view: View,
    /// User-selected ordering for sidebar and tree-view rows. Cycled
    /// by the `cycle_sort` action; the radial layout is always
    /// size-driven and ignores this.
    pub sort_mode: SortMode,
    /// How `execute_delete` removes entries. Detected once at startup
    /// (trash-put → gio trash → permanent rm) and reused for every
    /// delete the user issues; surfaced in the status message so
    /// users know whether the action was recoverable.
    pub delete_strategy: DeleteStrategy,
    pub radial_map: Option<RadialMap>,
    pub renderer: RadialRenderer,
    pub hovered_uuid: Option<Uuid>,
    #[allow(dead_code)]
    pub selected_uuid: Option<Uuid>,
    pub sidebar_index: usize,
    pub sidebar_hover_index: Option<usize>,
    pub terminal_size: (u16, u16),
    pub should_quit: bool,
    pub scan_progress: Option<ScanProgress>,
    /// Handle to the active streaming scan, if any. `None` means there is no
    /// scan in progress (either because we have already moved to `Viewing`
    /// mode or because no scan has started yet).
    pub scan_handle: Option<ScanHandle>,
    /// Path most recently observed by the streaming scan, displayed in the
    /// status bar so the user can see what the walker is currently chewing.
    pub scan_current_path: Option<PathBuf>,
    pub nav_history: Vec<PathBuf>,
    pub status_message: String,
    pub canvas_coords: Option<CanvasCoords>,
    pub context_menu: ContextMenu,
    pub delete_path: Option<PathBuf>,
    pub delete_is_folder: bool,
    /// Inode captured at scan time for the staged delete target, when
    /// known. The delete path passes this to `delete::delete()` as a
    /// TOCTOU guard — if the on-disk inode no longer matches when the
    /// user confirms, the delete is refused with a clear error rather
    /// than silently following the renamed path. `None` for folders
    /// (we only capture inodes on files), for radial-map deletes
    /// (the segment doesn't carry an inode), and on non-Unix builds.
    pub delete_inode: Option<u64>,
    /// Currently highlighted choice in the delete confirmation dialog.
    /// `true` = Yes, `false` = No. Always initialised to `false` so a stray
    /// Enter keypress can never trigger an irreversible deletion.
    pub delete_selected: bool,
    /// `true` while the user has typed `g` and we're waiting for the
    /// follow-up keystroke that completes a vim-style two-key
    /// sequence (currently only `gg` → jump to first item). Reset on
    /// any non-`g` keypress.
    pub pending_g: bool,
    /// Currently focused folder within the arena — the "view root"
    /// for the radial / sidebar. `None` falls through to the arena's
    /// own root, which is the original scan target. Updated by
    /// `navigate_into` / `navigate_up` so the user can browse the
    /// arena without restarting the scan; the walker keeps running
    /// in the background regardless of where the user has zoomed.
    pub current_folder_id: Option<FolderId>,
}

impl App {
    pub fn new(path: PathBuf, config: Config, term_width: u16, term_height: u16) -> Self {
        let ring_depth = config.display.ring_depth;
        // An invalid keybind config falls back to defaults rather than
        // refusing to start the App; the user already saw the parse
        // error at config-load time, and locking them out of the tool
        // because of a bad rebind is worse than running with defaults.
        let keybinds = Keybinds::from_config(&config.keybinds).unwrap_or_else(|err| {
            eprintln!("warning: ignoring invalid keybinds config: {:#}", err);
            Keybinds::defaults()
        });
        let theme = Theme::from_config(&config.colors);
        // Bubble theme parse warnings into the initial status
        // message so users see typo'd hex colours at startup.
        let initial_status = if theme.warnings.is_empty() {
            String::new()
        } else {
            theme.warnings.join("; ")
        };
        Self {
            mode: AppMode::Scanning,
            focus: Focus::Map,
            arena: None,
            current_path: path,
            ring_depth,
            config,
            keybinds,
            theme,
            selected_paths: std::collections::HashSet::new(),
            owner_cache: std::collections::HashMap::new(),
            view: View::default(),
            sort_mode: SortMode::default(),
            delete_strategy: DeleteStrategy::detect(),
            radial_map: None,
            renderer: RadialRenderer::new(ColorConfig::default()),
            hovered_uuid: None,
            selected_uuid: None,
            sidebar_index: 0,
            sidebar_hover_index: None,
            terminal_size: (term_width, term_height),
            should_quit: false,
            scan_progress: None,
            scan_handle: None,
            scan_current_path: None,
            nav_history: Vec::new(),
            status_message: initial_status,
            canvas_coords: None,
            context_menu: ContextMenu::new(),
            delete_path: None,
            delete_is_folder: false,
            delete_inode: None,
            // Default to No: a destructive action requires deliberate input.
            delete_selected: false,
            pending_g: false,
            current_folder_id: None,
        }
    }

    /// Resolve which folder is currently in focus — `current_folder_id`
    /// when the user has explicitly zoomed in; otherwise the arena's
    /// own root. Returns `None` only when no arena exists yet (early
    /// startup, before the walker has even seeded the root).
    pub fn focus_folder_id(&self, arena: &TreeArena) -> Option<FolderId> {
        if let Some(id) = self.current_folder_id {
            // Defend against a stale id from a snapshot that didn't
            // round-trip with the current arena. The cloned post-scan
            // arena keeps the same indices, so this `is_some` check
            // really only fires after a snapshot import edge case.
            if id.0 < arena.folders().len() {
                return Some(id);
            }
        }
        arena.root()
    }

    /// Kick off a streaming parallel scan of `current_path`. The walker runs
    /// in its own thread and emits [`ScanEvent`]s back through a channel
    /// owned by `scan_handle`. The UI loop drains the channel via
    /// [`Self::update_scan_progress`] each frame.
    pub fn start_scan(&mut self) {
        self.mode = AppMode::Scanning;
        self.scan_progress = Some(ScanProgress {
            files_scanned: 0,
            total_size: 0,
        });
        self.scan_current_path = None;
        // Drop any prior radial map so the canvas shows "scanning" placeholder
        // instead of stale data when the user re-scans a different path.
        self.radial_map = None;
        self.arena = None;
        // Reset zoom focus — a fresh scan rebuilds the arena from
        // scratch, so any prior focus FolderId is meaningless.
        self.current_folder_id = None;
        self.sidebar_index = 0;

        let scan_config = self.config.to_scan_config();
        self.scan_handle = Some(scan_streaming(&self.current_path, &scan_config));
    }

    /// Rebuild the radial map from the live (mid-scan) arena via
    /// `try_lock`. Skips silently if the walker holds the lock — we'll
    /// retry on the next frame. Used by `update_scan_progress` so the
    /// radial fills in progressively while jwalk is still running.
    fn rebuild_map_from_live(&mut self) {
        let Some(handle) = self.scan_handle.as_ref() else {
            return;
        };
        let Ok(arena) = handle.live.try_lock() else {
            return;
        };
        let Some(focus) = self.focus_folder_id(&arena) else {
            return;
        };
        let config = RadialConfig {
            ring_depth: self.ring_depth,
            terminal_width: self.terminal_size.0,
            terminal_height: self.terminal_size.1,
            ..Default::default()
        };
        self.radial_map = Some(build_radial_map(&arena, focus, &config));
        self.canvas_coords = Some(CanvasCoords::new(
            self.terminal_size.0 as usize,
            self.terminal_size.1 as usize,
        ));
    }

    /// Rebuild the radial map from current state
    pub fn rebuild_map(&mut self) {
        if let Some(ref arena) = self.arena {
            if let Some(focus) = self.focus_folder_id(arena) {
                let config = RadialConfig {
                    ring_depth: self.ring_depth,
                    terminal_width: self.terminal_size.0,
                    terminal_height: self.terminal_size.1,
                    ..Default::default()
                };
                self.radial_map = Some(build_radial_map(arena, focus, &config));
                self.canvas_coords = Some(CanvasCoords::new(
                    self.terminal_size.0 as usize,
                    self.terminal_size.1 as usize,
                ));
            }
        }
    }

    /// Handle terminal resize
    pub fn resize(&mut self, width: u16, height: u16) {
        self.terminal_size = (width, height);
        self.rebuild_map();
    }

    /// Handle key event
    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            AppMode::Scanning => {
                // Phase 23: full navigation works during scan. The
                // walker keeps cataloguing in the background while
                // the user clicks around the partial arena. Esc/q
                // still quit, all other keys flow through the same
                // dispatch as Viewing mode.
                if self.context_menu.visible {
                    self.handle_context_menu_key(key);
                } else {
                    self.handle_viewing_key(key);
                }
            }
            AppMode::Viewing => {
                // If context menu is visible, handle menu navigation
                if self.context_menu.visible {
                    self.handle_context_menu_key(key);
                } else {
                    self.handle_viewing_key(key);
                }
            }
            AppMode::Help => {
                // Vim-style: `q` / `Esc` / `?` close the overlay.
                // Any other keypress is ignored so users can't drop
                // into a stale state by mashing keys while the help
                // is open.
                if matches!(
                    key.code,
                    KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('?') | KeyCode::Enter
                ) {
                    self.mode = AppMode::Viewing;
                }
            }
            AppMode::ConfirmDelete => {
                match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        if self.delete_selected {
                            self.execute_delete();
                        } else {
                            self.delete_path = None;
                            self.mode = AppMode::Viewing;
                        }
                    }
                    KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Char('j')
                    | KeyCode::Char('k')
                    | KeyCode::Char('h')
                    | KeyCode::Char('l')
                    | KeyCode::Tab => {
                        // Yes/No is a horizontal choice — h/l (vim
                        // horizontal motion) and Tab join the existing
                        // arrows / j / k as togglers.
                        self.delete_selected = !self.delete_selected;
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        // Cancel delete
                        self.delete_path = None;
                        self.mode = AppMode::Viewing;
                    }
                    _ => {} // Ignore other keys
                }
            }
        }
    }

    /// Handle key event when context menu is visible
    fn handle_context_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.context_menu.hide(),
            KeyCode::Up | KeyCode::Char('k') => self.context_menu.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.context_menu.move_down(),
            KeyCode::Enter => self.execute_menu_action(),
            _ => {}
        }
    }

    fn handle_viewing_key(&mut self, key: KeyEvent) {
        // Vim two-key sequence: `gg` jumps to the first item.
        // The state machine here is intentionally tiny — only one
        // sequence is supported, so a single bool is enough. Any
        // non-`g` keystroke clears the pending state.
        if self.pending_g {
            self.pending_g = false;
            if matches!(key.code, KeyCode::Char('g')) {
                self.dispatch_action(Action::MoveToFirst);
                return;
            }
            // fall through to normal dispatch below
        } else if matches!(key.code, KeyCode::Char('g'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.pending_g = true;
            return;
        }

        let Some(action) = self.keybinds.action_for(key) else {
            return;
        };
        self.dispatch_action(action);
    }

    /// Execute a resolved [`Action`] against the App. Centralised so
    /// future input sources (e.g. mouse-driven menu items) can fan into
    /// the same dispatch table.
    fn dispatch_action(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::Help => self.mode = AppMode::Help,
            Action::NavigateUp => self.navigate_up(),
            Action::NavigateInto => self.navigate_into_hovered(),
            Action::ZoomIn => self.zoom_in(),
            Action::ZoomOut => self.zoom_out(),
            Action::Rescan => self.start_scan(),
            Action::Delete => self.trigger_delete(),
            Action::ToggleFocus => self.toggle_focus(),
            Action::MoveUp => self.move_hover_up(),
            Action::MoveDown => self.move_hover_down(),
            Action::ToggleView => self.view = self.view.next(),
            Action::CycleSort => {
                self.sort_mode = self.sort_mode.next();
                self.status_message = format!("sort: {}", self.sort_mode.label());
            }
            Action::ToggleApparentSize => {
                self.config.scan.use_apparent_size = !self.config.scan.use_apparent_size;
                self.status_message = if self.config.scan.use_apparent_size {
                    "apparent size — rescanning…".to_string()
                } else {
                    "on-disk size — rescanning…".to_string()
                };
                // Re-walk: the size accounting changed at every node.
                self.start_scan();
            }
            Action::ToggleSelect => self.toggle_select_current(),
            Action::DeleteSelected => self.trigger_delete_selected(),
            Action::ClearSelection => {
                let n = self.selected_paths.len();
                self.selected_paths.clear();
                self.status_message = format!("Cleared {} selection(s)", n);
            }
            Action::ShowOwner => self.show_owner_for_current(),
            Action::MoveToFirst => self.move_sidebar_to(0),
            Action::MoveToLast => {
                let n = self.sidebar_items().len();
                if n > 0 {
                    self.move_sidebar_to(n - 1);
                }
            }
            Action::MoveHalfPageDown => self.move_sidebar_by(self.half_page_step() as isize),
            Action::MoveHalfPageUp => self.move_sidebar_by(-(self.half_page_step() as isize)),
        }
    }

    /// Half the visible sidebar height in rows, with a sane minimum
    /// so a tiny terminal still moves at least one row per Ctrl-d.
    fn half_page_step(&self) -> usize {
        // Status bar (2) + borders (2) ≈ 4; halve the rest.
        let rows = self.terminal_size.1.saturating_sub(4) / 2;
        rows.max(1) as usize
    }

    fn move_sidebar_to(&mut self, idx: usize) {
        let n = self.sidebar_items().len();
        if n == 0 {
            return;
        }
        self.sidebar_index = idx.min(n - 1);
    }

    fn move_sidebar_by(&mut self, delta: isize) {
        let n = self.sidebar_items().len() as isize;
        if n == 0 {
            return;
        }
        let cur = self.sidebar_index as isize;
        let next = (cur + delta).clamp(0, n - 1);
        self.sidebar_index = next as usize;
    }

    /// Look up (or recall from cache) the pacman package owning the
    /// current sidebar entry and surface it in the status bar. No-op
    /// when the sidebar is empty or pacman is unavailable.
    fn show_owner_for_current(&mut self) {
        let Some(arena) = self.arena.as_ref() else {
            return;
        };
        let items = self.sidebar_items();
        let Some(item) = items.get(self.sidebar_index) else {
            return;
        };
        let path = match item {
            TreeItem::File(id, _) => arena.file(*id).path.clone(),
            TreeItem::Folder(id, _) => arena.folder(*id).file.path.clone(),
        };
        let owner = match self.owner_cache.get(&path) {
            Some(cached) => cached.clone(),
            None => {
                let fresh = crate::ownership::query(&path);
                self.owner_cache.insert(path.clone(), fresh.clone());
                fresh
            }
        };
        self.status_message = format!("{}: {}", path.display(), owner.label());
    }

    /// Toggle the current sidebar item in/out of `selected_paths`.
    /// No-op if the sidebar is empty or the arena hasn't loaded yet.
    fn toggle_select_current(&mut self) {
        let Some(arena) = self.arena.as_ref() else {
            return;
        };
        let items = self.sidebar_items();
        let Some(item) = items.get(self.sidebar_index) else {
            return;
        };
        let path = match item {
            TreeItem::File(id, _) => arena.file(*id).path.clone(),
            TreeItem::Folder(id, _) => arena.folder(*id).file.path.clone(),
        };
        if self.selected_paths.remove(&path) {
            self.status_message = format!(
                "- {}  ({} selected)",
                path.display(),
                self.selected_paths.len()
            );
        } else {
            self.selected_paths.insert(path.clone());
            self.status_message = format!(
                "+ {}  ({} selected)",
                path.display(),
                self.selected_paths.len()
            );
        }
    }

    /// Open the confirmation dialog for the multi-select batch.
    /// Reuses the existing `ConfirmDelete` mode; the executor in
    /// `execute_delete` walks `selected_paths` instead of the
    /// single `delete_path` when the set is non-empty.
    fn trigger_delete_selected(&mut self) {
        if self.selected_paths.is_empty() {
            self.status_message = "no items selected (space to mark, then Shift+D)".into();
            return;
        }
        // Stage with a sentinel path so the existing dialog still
        // has something to display; the actual targets come from
        // `selected_paths`.
        self.delete_path = None;
        self.delete_inode = None;
        self.delete_is_folder = false;
        self.delete_selected = false;
        self.mode = AppMode::ConfirmDelete;
    }

    /// Trigger delete action for the currently selected item
    fn trigger_delete(&mut self) {
        // Check sidebar selection first
        if let Some(arena) = self.arena.as_ref() {
            let items = self.sidebar_items();
            if let Some(item) = items.get(self.sidebar_index) {
                let (path, is_folder, inode) = match item {
                    TreeItem::File(id, _) => {
                        let file = arena.file(*id);
                        (file.path.clone(), false, file.inode)
                    }
                    TreeItem::Folder(id, _) => {
                        let folder = arena.folder(*id);
                        // We don't currently capture folder inodes
                        // (folders are recursive deletes; the
                        // identity check is most meaningful for
                        // single files). Pass None to opt out of the
                        // TOCTOU guard for now.
                        (folder.file.path.clone(), true, None)
                    }
                };
                self.delete_path = Some(path);
                self.delete_is_folder = is_folder;
                self.delete_inode = inode;
                // Always reset highlight to No when opening the dialog.
                self.delete_selected = false;
                self.mode = AppMode::ConfirmDelete;
                return;
            }
        }

        // Otherwise use map hover
        if let Some(uuid) = self.hovered_uuid {
            if let Some(ref map) = self.radial_map {
                if let Some(segment) = self.renderer.find_segment(map, &uuid) {
                    self.delete_path = Some(PathBuf::from(&segment.path));
                    self.delete_is_folder = segment.is_folder;
                    // The segment doesn't carry an inode; the TOCTOU
                    // guard only fires for sidebar-driven deletes
                    // until renderer carries inode through too.
                    self.delete_inode = None;
                    // Always reset highlight to No when opening the dialog.
                    self.delete_selected = false;
                    self.mode = AppMode::ConfirmDelete;
                }
            }
        }
    }

    /// Handle mouse event
    pub fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if self.context_menu.visible {
                    // Check if clicking on menu item
                    self.handle_context_menu_click(mouse.column, mouse.row);
                } else {
                    self.handle_click_at(mouse.column, mouse.row);
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                // Show context menu if hovering over a segment
                self.show_context_menu(mouse.column, mouse.row);
            }
            MouseEventKind::Moved => {
                if self.context_menu.visible {
                    // Update context menu hover
                    self.context_menu.update_hover(
                        mouse.column,
                        mouse.row,
                        self.terminal_size.0,
                        self.terminal_size.1,
                    );
                } else {
                    self.handle_hover_at(mouse.column, mouse.row);
                }
            }
            MouseEventKind::ScrollUp => self.zoom_in(),
            MouseEventKind::ScrollDown => self.zoom_out(),
            _ => {}
        }
    }

    /// Calculate canvas area based on terminal size
    /// This matches the actual canvas widget area including its borders
    fn canvas_area(&self) -> ratatui::layout::Rect {
        let total_width = self.terminal_size.0;
        let total_height = self.terminal_size.1;
        let sidebar_width = (total_width * 25) / 100;

        // Map area is after sidebar
        let map_width = total_width - sidebar_width;
        // Canvas height is total height minus status bar (2 rows)
        let canvas_height = total_height - 2;

        ratatui::layout::Rect {
            x: sidebar_width,
            y: 0,
            width: map_width,
            height: canvas_height,
        }
    }

    /// Calculate sidebar area based on terminal size
    fn sidebar_area(&self) -> ratatui::layout::Rect {
        let total_width = self.terminal_size.0;
        let sidebar_width = (total_width * 25) / 100;
        ratatui::layout::Rect {
            x: 0,
            y: 0,
            width: sidebar_width,
            height: self.terminal_size.1,
        }
    }

    /// Convert terminal cell coordinates to canvas coordinates
    /// Returns None if the point is not in the canvas area
    fn terminal_to_canvas(&self, col: u16, row: u16) -> Option<(f64, f64)> {
        let canvas = self.canvas_area();

        // The canvas widget has borders (Block::default().borders(Borders::ALL))
        // So the inner drawing area starts at x+1, y+1 and ends at x+w-1, y+h-1
        let inner_x = canvas.x + 1;
        let inner_y = canvas.y + 1;
        let inner_width = canvas.width.saturating_sub(2);
        let inner_height = canvas.height.saturating_sub(2);

        // Check if within the inner canvas area
        if col < inner_x
            || col >= inner_x + inner_width
            || row < inner_y
            || row >= inner_y + inner_height
        {
            return None;
        }

        // Get the radial map bounds
        let radial_map = self.radial_map.as_ref()?;
        let max_radius = radial_map
            .rings
            .last()
            .map(|r| r.outer_radius)
            .unwrap_or(radial_map.center_radius);

        // Match the bounds calculation from render_radial_map
        let pixel_width = inner_width as f64 * 2.0;
        let pixel_height = inner_height as f64 * 4.0;
        let aspect_ratio = pixel_height / pixel_width;

        // Bounds match what's set in ui.rs
        let x_bound = max_radius;
        let y_bound = max_radius * aspect_ratio;

        // Convert from cell position to relative position (0 to 1)
        let rel_x = (col - inner_x) as f64 / inner_width as f64;
        let rel_y = (row - inner_y) as f64 / inner_height as f64;

        // Convert to canvas coordinates
        // Canvas: x goes from -x_bound (left) to +x_bound (right)
        // Canvas: y goes from -y_bound (bottom) to +y_bound (top)
        // Terminal: y=0 at top, increases downward
        let canvas_x = -x_bound + rel_x * 2.0 * x_bound;
        let canvas_y = y_bound - rel_y * 2.0 * y_bound; // Y inverted

        Some((canvas_x, canvas_y))
    }

    /// Handle click at screen position
    fn handle_click_at(&mut self, col: u16, row: u16) {
        let sidebar = self.sidebar_area();

        // Check if click is within sidebar
        if col >= sidebar.x
            && col < sidebar.x + sidebar.width
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
        {
            // Calculate item index (subtract 1 for border, 1 for title)
            if row >= 2 {
                let clicked_index = (row - 2) as usize;
                let items = self.sidebar_items();
                if clicked_index < items.len() {
                    self.sidebar_index = clicked_index;
                    self.focus = Focus::Sidebar;

                    // If it's a folder, navigate into it
                    let item = items[clicked_index];
                    if item.is_folder() {
                        if let Some(ref arena) = self.arena {
                            if let TreeItem::Folder(id, _) = item {
                                let path = arena.folder(id).file.path.clone();
                                self.navigate_into(path);
                            }
                        }
                    }
                    return;
                }
            }
            self.focus = Focus::Sidebar;
            return;
        }

        // Convert to canvas coordinates
        if let Some((canvas_x, canvas_y)) = self.terminal_to_canvas(col, row) {
            self.handle_canvas_click(canvas_x, canvas_y);
        }
    }

    /// Handle hover at screen position
    fn handle_hover_at(&mut self, col: u16, row: u16) {
        let sidebar = self.sidebar_area();

        // Check if hover is within sidebar
        if col >= sidebar.x
            && col < sidebar.x + sidebar.width
            && row >= sidebar.y
            && row < sidebar.y + sidebar.height
        {
            if row >= 2 {
                let hover_index = (row - 2) as usize;
                let items = self.sidebar_items();
                if hover_index < items.len() {
                    self.sidebar_hover_index = Some(hover_index);
                    self.sync_hover_to_canvas();
                } else {
                    self.sidebar_hover_index = None;
                    self.hovered_uuid = None;
                    self.renderer.set_hovered(None);
                }
            } else {
                self.sidebar_hover_index = None;
                self.hovered_uuid = None;
                self.renderer.set_hovered(None);
            }
            return;
        }

        // Clear sidebar hover when in map
        self.sidebar_hover_index = None;

        // Handle as map hover (convert to canvas coordinates first)
        if let Some((canvas_x, canvas_y)) = self.terminal_to_canvas(col, row) {
            self.handle_canvas_hover(canvas_x, canvas_y);
            self.sync_hover_to_sidebar();
        } else {
            self.hovered_uuid = None;
            self.renderer.set_hovered(None);
        }
    }

    /// Handle mouse click in canvas coordinates
    fn handle_canvas_click(&mut self, x: f64, y: f64) {
        if let Some(ref map) = self.radial_map {
            // Calculate radius from center (0, 0) in canvas coords
            let radius = (x * x + y * y).sqrt();

            // Check if clicked on center (go up)
            if radius < map.center_radius {
                self.navigate_up();
                return;
            }

            // Calculate angle from canvas coordinates
            let mut angle = y.atan2(x).to_degrees();
            if angle < 0.0 {
                angle += 360.0;
            }

            // Find segment at this position
            for ring in &map.rings {
                if radius >= ring.inner_radius && radius <= ring.outer_radius {
                    let angle_16ths = ((angle * 16.0) as u32) % 5760;
                    for segment in &ring.segments {
                        if segment.contains_angle(angle_16ths) {
                            if segment.is_folder && !segment.is_fake {
                                self.navigate_into(PathBuf::from(&segment.path));
                            }
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Handle mouse hover in canvas coordinates
    fn handle_canvas_hover(&mut self, x: f64, y: f64) {
        if let Some(ref map) = self.radial_map {
            // Calculate radius and angle from center
            let radius = (x * x + y * y).sqrt();
            let mut angle = y.atan2(x).to_degrees();
            if angle < 0.0 {
                angle += 360.0;
            }

            // Find segment at this position
            for ring in &map.rings {
                if radius >= ring.inner_radius && radius <= ring.outer_radius {
                    let angle_16ths = ((angle * 16.0) as u32) % 5760;
                    for segment in &ring.segments {
                        if segment.contains_angle(angle_16ths) {
                            self.hovered_uuid = Some(segment.uuid);
                            self.renderer.set_hovered(Some(segment.uuid));
                            return;
                        }
                    }
                }
            }
        }

        // No segment found
        self.hovered_uuid = None;
        self.renderer.set_hovered(None);
        self.sidebar_hover_index = None;
    }

    /// Sync canvas hover to sidebar
    fn sync_hover_to_sidebar(&mut self) {
        let Some(uuid) = self.hovered_uuid else {
            self.sidebar_hover_index = None;
            return;
        };
        let Some(map) = self.radial_map.as_ref() else {
            self.sidebar_hover_index = None;
            return;
        };
        let Some(segment) = self.renderer.find_segment(map, &uuid) else {
            self.sidebar_hover_index = None;
            return;
        };
        let Some(arena) = self.arena.as_ref() else {
            self.sidebar_hover_index = None;
            return;
        };

        let items = self.sidebar_items();
        for (i, item) in items.iter().enumerate() {
            let item_path = match item {
                TreeItem::File(id, _) => arena.file(*id).path.to_string_lossy().into_owned(),
                TreeItem::Folder(id, _) => {
                    arena.folder(*id).file.path.to_string_lossy().into_owned()
                }
            };
            if item_path == segment.path {
                self.sidebar_hover_index = Some(i);
                return;
            }
        }
        self.sidebar_hover_index = None;
    }

    /// Sync sidebar hover → radial hover.
    ///
    /// When the lookup succeeds (exact match or the "small files" group
    /// fallback in [`Self::find_segment_uuid_for_sidebar_hover`]), the
    /// radial highlight tracks the sidebar cursor on every j/k. When
    /// the lookup truly returns `None` — radial map empty, no segments
    /// at all — we keep whatever hover was already showing rather than
    /// flicker it off; otherwise quick keyboard navigation past the
    /// last visible segment would blank the radial.
    fn sync_hover_to_canvas(&mut self) {
        if let Some(uuid) = self.find_segment_uuid_for_sidebar_hover() {
            self.hovered_uuid = Some(uuid);
            self.renderer.set_hovered(Some(uuid));
        }
    }

    /// Resolve the segment UUID corresponding to the currently sidebar-hovered
    /// item, if any.
    ///
    /// Behaviour
    /// - Exact path match wins.
    /// - When the sidebar item is too small to draw its own slice — the
    ///   radial layout rolls those into a fake "N small files" segment —
    ///   we highlight the small-files group instead, so j/k navigation
    ///   still produces a visible cue rather than silently clearing the
    ///   radial hover.
    /// - Returns `None` only when the radial layout is genuinely empty
    ///   (no map, no segments at all). In that case the caller leaves the
    ///   prior hover in place rather than clearing — see
    ///   [`Self::sync_hover_to_canvas`].
    fn find_segment_uuid_for_sidebar_hover(&self) -> Option<Uuid> {
        let hover_idx = self.sidebar_hover_index?;
        let arena = self.arena.as_ref()?;
        let items = self.sidebar_items();
        let item = items.get(hover_idx)?;
        let path = match item {
            TreeItem::File(id, _) => arena.file(*id).path.to_string_lossy().into_owned(),
            TreeItem::Folder(id, _) => arena.folder(*id).file.path.to_string_lossy().into_owned(),
        };
        let map = self.radial_map.as_ref()?;

        // Pass 1: exact path match.
        for ring in &map.rings {
            for segment in &ring.segments {
                if segment.path == path {
                    return Some(segment.uuid);
                }
            }
        }
        // Pass 2: the item rolled into a small-files group. Return the
        // first ring's fake segment so the user still sees a highlight.
        // Only the innermost ring carries a fake group for the current
        // root, so we stop after finding one in any ring.
        for ring in &map.rings {
            for segment in &ring.segments {
                if segment.is_fake {
                    return Some(segment.uuid);
                }
            }
        }
        None
    }

    /// Show context menu at cursor position if hovering over a segment
    fn show_context_menu(&mut self, col: u16, row: u16) {
        // Check if right-click is in sidebar
        let sidebar = self.sidebar_area();
        if col >= sidebar.x && col < sidebar.x + sidebar.width {
            // Click is in sidebar
            if row >= 2 {
                let clicked_index = (row - 2) as usize;
                if let Some(arena) = self.arena.as_ref() {
                    let items = self.sidebar_items();
                    if let Some(item) = items.get(clicked_index) {
                        let (name, path, is_folder) = match item {
                            TreeItem::File(id, _) => {
                                let file = arena.file(*id);
                                (
                                    file.name.clone(),
                                    file.path.to_string_lossy().into_owned(),
                                    false,
                                )
                            }
                            TreeItem::Folder(id, _) => {
                                let folder = arena.folder(*id);
                                (
                                    folder.file.name.clone(),
                                    folder.file.path.to_string_lossy().into_owned(),
                                    true,
                                )
                            }
                        };
                        // Sidebar items have no segment UUID; use a sentinel.
                        self.context_menu
                            .show(col, row, Uuid::nil(), name, path, is_folder);
                        return;
                    }
                }
            }
            return;
        }

        // Otherwise check for map segment
        if let Some(uuid) = self.hovered_uuid {
            if let Some(ref map) = self.radial_map {
                // Find the segment
                for ring in &map.rings {
                    for segment in &ring.segments {
                        if segment.uuid == uuid {
                            self.context_menu.show(
                                col,
                                row,
                                segment.uuid,
                                segment.name.clone(),
                                segment.path.clone(),
                                segment.is_folder,
                            );
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Handle click on context menu
    fn handle_context_menu_click(&mut self, col: u16, row: u16) {
        // Calculate which menu item was clicked
        let menu = &self.context_menu;
        let items = menu.menu_items();
        let menu_width: u16 = 25;
        let menu_height: u16 = items.len() as u16 + 2; // +2 for borders

        // Apply same bounds checking as render_context_menu
        let menu_x = menu.x.min(self.terminal_size.0.saturating_sub(menu_width));
        let menu_y = menu.y.min(self.terminal_size.1.saturating_sub(menu_height));

        // Check if click is within menu bounds
        if col >= menu_x && col < menu_x + menu_width && row > menu_y && row < menu_y + menu_height
        {
            // Click is inside menu - calculate which item
            let clicked_index = (row - menu_y - 1) as usize; // -1 for top border
            if clicked_index < items.len() {
                self.context_menu.selected_index = clicked_index;
                self.execute_menu_action();
            }
        } else {
            // Click outside menu - close it
            self.context_menu.hide();
        }
    }

    /// Execute the currently selected menu action
    fn execute_menu_action(&mut self) {
        if let Some(action) = self.context_menu.selected_action() {
            let path = self.context_menu.segment_path.clone();
            let is_folder = self.context_menu.is_folder;

            match action {
                MenuAction::Open => {
                    if is_folder {
                        self.navigate_into(PathBuf::from(&path));
                    } else {
                        // Open file with system handler
                        #[cfg(unix)]
                        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
                        #[cfg(target_os = "macos")]
                        let _ = std::process::Command::new("open").arg(&path).spawn();
                        #[cfg(target_os = "windows")]
                        let _ = std::process::Command::new("cmd")
                            .args(["/C", "start", &path])
                            .spawn();
                    }
                }
                MenuAction::OpenTerminal => {
                    if is_folder {
                        // Try common terminal emulators
                        let terminals =
                            ["alacritty", "kitty", "gnome-terminal", "xterm", "konsole"];
                        for term in &terminals {
                            if let Ok(mut child) =
                                std::process::Command::new(term).current_dir(&path).spawn()
                            {
                                let _ = child.wait();
                                break;
                            }
                        }
                    }
                }
                MenuAction::CenterMap => {
                    if is_folder {
                        self.navigate_into(PathBuf::from(&path));
                    }
                }
                MenuAction::CopyPath => {
                    // Try wl-copy (Wayland) first
                    let result = std::process::Command::new("wl-copy")
                        .arg(&path)
                        .stdin(std::process::Stdio::null())
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn();

                    // Fallback to xclip (X11)
                    if result.is_err() {
                        let _ = std::process::Command::new("xclip")
                            .args(["-selection", "clipboard"])
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn()
                            .and_then(|mut child| {
                                use std::io::Write;
                                if let Some(mut stdin) = child.stdin.take() {
                                    stdin.write_all(path.as_bytes())?;
                                }
                                Ok(())
                            });
                    }
                }
                MenuAction::Rescan => {
                    self.start_scan();
                }
                MenuAction::Delete => {
                    // Show confirmation dialog
                    self.delete_path = Some(PathBuf::from(&path));
                    self.delete_is_folder = is_folder;
                    self.context_menu.hide();
                    self.mode = AppMode::ConfirmDelete;
                }
            }
        }

        self.context_menu.hide();
    }

    /// Navigate up to parent directory
    /// Navigate up. If the current focus has a parent inside the
    /// arena, just shift focus — no rescan, no work thrown away.
    /// Only when we'd leave the arena root (the originally-scanned
    /// path) do we fall back to a fresh scan rooted at the new
    /// directory. This is what enables "go back, see your old
    /// scan still there" without restarting.
    pub fn navigate_up(&mut self) {
        // First, try to walk up inside the arena.
        let in_arena_target = self.with_arena(|arena| {
            let focus = self.focus_folder_id(arena)?;
            let parent = arena.folder(focus).file.parent?;
            let path = arena.folder(parent).file.path.clone();
            Some((parent, path))
        });
        if let Some(Some((parent_id, path))) = in_arena_target {
            self.nav_history.push(self.current_path.clone());
            self.current_path = path.clone();
            self.current_folder_id = Some(parent_id);
            self.hovered_uuid = None;
            self.renderer.set_hovered(None);
            self.sidebar_index = 0;
            self.rebuild_map();
            self.notify_focus_change(path);
            return;
        }
        // We're at the arena's root. Going further up means leaving
        // the originally-scanned tree — that genuinely requires a
        // fresh scan.
        if let Some(parent) = self.current_path.parent() {
            self.nav_history.push(self.current_path.clone());
            self.current_path = parent.to_path_buf();
            self.hovered_uuid = None;
            self.renderer.set_hovered(None);
            self.start_scan();
        }
    }

    /// Navigate into a folder by absolute path. Used by mouse
    /// clicks and the context menu where only the path is known
    /// (not a `FolderId`). When the path is inside the current
    /// arena we shift focus without restarting; otherwise this
    /// is a "navigate to a different tree" intent and triggers a
    /// fresh scan.
    pub fn navigate_into(&mut self, path: PathBuf) {
        if !path.is_dir() {
            return;
        }
        let in_arena_id = self.with_arena(|arena| arena.find_folder_by_path(&path));
        if let Some(Some(folder_id)) = in_arena_id {
            self.nav_history.push(self.current_path.clone());
            self.current_path = path.clone();
            self.current_folder_id = Some(folder_id);
            self.hovered_uuid = None;
            self.renderer.set_hovered(None);
            self.sidebar_index = 0;
            self.rebuild_map();
            self.notify_focus_change(path);
            return;
        }
        // Path is outside the current arena — fall back to a fresh
        // scan with this as the new root.
        self.nav_history.push(self.current_path.clone());
        self.current_path = path;
        self.current_folder_id = None;
        self.hovered_uuid = None;
        self.renderer.set_hovered(None);
        self.start_scan();
    }

    /// Fast in-arena navigation by `FolderId` — used by sidebar /
    /// segment clicks where we already have the target's id and
    /// don't need a path lookup at all.
    fn navigate_into_id(&mut self, folder_id: FolderId) {
        let path = self.with_arena(|arena| arena.folder(folder_id).file.path.clone());
        let Some(path) = path else { return };
        self.nav_history.push(self.current_path.clone());
        self.current_path = path.clone();
        self.current_folder_id = Some(folder_id);
        self.hovered_uuid = None;
        self.renderer.set_hovered(None);
        self.sidebar_index = 0;
        self.rebuild_map();
        self.notify_focus_change(path);
    }

    /// Tell the running scan (if any) that the user has shifted
    /// focus. The walker reorders its priority queue so dirs under
    /// `path` jump to the front. If there's no active scan, the
    /// call is a cheap no-op.
    fn notify_focus_change(&self, path: PathBuf) {
        if let Some(handle) = self.scan_handle.as_ref() {
            handle.set_focus(path);
        }
    }

    /// Navigate into the currently hovered folder
    fn navigate_into_hovered(&mut self) {
        // Sidebar wins. The selected item already has a FolderId,
        // so route through navigate_into_id and skip the path
        // round-trip.
        let items = self.sidebar_items();
        if let Some(TreeItem::Folder(id, _)) = items.get(self.sidebar_index).copied() {
            self.navigate_into_id(id);
            return;
        }

        // Map hover: segment carries a path string. Resolve it
        // through the arena so we still get the FolderId fast-path
        // inside `navigate_into`.
        if let Some(uuid) = self.hovered_uuid {
            if let Some(ref map) = self.radial_map {
                if let Some(segment) = self.renderer.find_segment(map, &uuid) {
                    if segment.is_folder && !segment.is_fake {
                        self.navigate_into(PathBuf::from(&segment.path));
                    }
                }
            }
        }
    }

    /// Zoom in (reduce ring depth)
    pub fn zoom_in(&mut self) {
        if self.ring_depth > 1 {
            self.ring_depth -= 1;
            self.rebuild_map();
            self.status_message = format!("Zoom: {} rings", self.ring_depth);
        }
    }

    /// Execute the pending delete operation through the configured
    /// [`DeleteStrategy`]. Status message reflects whether the entry
    /// was trashed (recoverable) or permanently removed.
    fn execute_delete(&mut self) {
        // Multi-select batch wins over single-target if non-empty.
        if !self.selected_paths.is_empty() {
            let total = self.selected_paths.len();
            let paths: Vec<PathBuf> = self.selected_paths.drain().collect();
            let mut ok = 0usize;
            let mut errors: Vec<String> = Vec::new();
            for p in &paths {
                // No inode TOCTOU guard for batch deletes — we
                // currently capture inodes for files only and
                // batches may include folders. The contextful
                // error from `delete()` still surfaces failures.
                match delete_path(&self.delete_strategy, p, None) {
                    Ok(()) => ok += 1,
                    Err(e) => errors.push(format!("{}: {:#}", p.display(), e)),
                }
            }
            self.status_message = if errors.is_empty() {
                format!("Deleted {} via {}", ok, self.delete_strategy.label())
            } else {
                format!(
                    "Deleted {}/{}, {} failed: {}",
                    ok,
                    total,
                    errors.len(),
                    errors.first().map(String::as_str).unwrap_or("")
                )
            };
            self.delete_path = None;
            self.delete_inode = None;
            self.mode = AppMode::Viewing;
            self.start_scan();
            return;
        }

        if let Some(path) = self.delete_path.take() {
            let expected_inode = self.delete_inode.take();
            match delete_path(&self.delete_strategy, &path, expected_inode) {
                Ok(()) => {
                    self.status_message = format!(
                        "Deleted ({}): {}",
                        self.delete_strategy.label(),
                        path.display()
                    );
                    self.start_scan();
                }
                Err(e) => {
                    self.status_message = format!("Delete failed: {:#}", e);
                }
            }
        }
        self.mode = AppMode::Viewing;
    }

    /// Zoom out (increase ring depth)
    pub fn zoom_out(&mut self) {
        if self.ring_depth < 10 {
            self.ring_depth += 1;
            self.rebuild_map();
            self.status_message = format!("Zoom: {} rings", self.ring_depth);
        }
    }

    /// Toggle focus between map and sidebar
    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Map => Focus::Sidebar,
            Focus::Sidebar => Focus::Map,
        };
    }

    /// Move hover up in sidebar.
    fn move_hover_up(&mut self) {
        if self.sidebar_index > 0 {
            self.sidebar_index -= 1;
            self.sidebar_hover_index = Some(self.sidebar_index);
            self.sync_hover_to_canvas();
        }
    }

    /// Move hover down in sidebar.
    ///
    /// Goes through `self.sidebar_items()` (which knows how to read
    /// the live arena via `with_arena`). Pre-Phase-26 this read
    /// `self.arena` directly — that field is `None` during a scan,
    /// so `j` was a silent no-op until the scan completed even
    /// though Phase 23 had already enabled navigation in
    /// `Scanning` mode.
    fn move_hover_down(&mut self) {
        let n = self.sidebar_items().len();
        if self.sidebar_index + 1 < n {
            self.sidebar_index += 1;
            self.sidebar_hover_index = Some(self.sidebar_index);
            self.sync_hover_to_canvas();
        }
    }

    /// Get the currently hovered segment
    pub fn hovered_segment(&self) -> Option<&Segment> {
        if let Some(uuid) = self.hovered_uuid {
            if let Some(ref map) = self.radial_map {
                return self.renderer.find_segment(map, &uuid);
            }
        }
        None
    }

    /// Run `f` with whichever arena is currently authoritative —
    /// the post-scan owned `arena` or, mid-scan, the live arena
    /// shared with the walker via `try_lock`. Returns `None` only
    /// when neither is available (no scan started yet, or the
    /// walker holds the lock at this exact moment).
    ///
    /// The closure shape exists so render code reads everything it
    /// needs (names, paths, sizes) under a *single* lock acquisition.
    /// The naive alternative — `sidebar_items()` followed by per-row
    /// `app.arena.as_ref()...` — does N+1 `try_lock`s per frame and
    /// drops into the `"?"` placeholder for any row whose lock
    /// happened to clash with the walker.
    pub fn with_arena<R>(&self, f: impl FnOnce(&TreeArena) -> R) -> Option<R> {
        if let Some(ref arena) = self.arena {
            return Some(f(arena));
        }
        let handle = self.scan_handle.as_ref()?;
        let arena = handle.live.try_lock().ok()?;
        Some(f(&arena))
    }

    /// Get segments for sidebar display, ordered by the user's
    /// currently-selected [`SortMode`]. Shared with the tree view so
    /// both surfaces stay consistent without duplicating the lookup.
    ///
    /// During a scan the post-scan `arena` is empty; fall back to a
    /// non-blocking `try_lock` on the live `Arc<Mutex<TreeArena>>`
    /// shared with the walker so the sidebar populates progressively.
    /// A failed `try_lock` returns the empty list — the next frame
    /// will retry.
    pub fn sidebar_items(&self) -> Vec<crate::tree::TreeItem> {
        self.with_arena(|arena| {
            self.focus_folder_id(arena)
                .map(|focus| arena.folder_items_sorted(focus, self.sort_mode))
                .unwrap_or_default()
        })
        .unwrap_or_default()
    }

    /// Drain pending [`ScanEvent`]s from the streaming scan, if any. Called
    /// once per frame so the status bar advances and the radial map appears
    /// the moment the walker finishes.
    pub fn update_scan_progress(&mut self) {
        let Some(handle) = self.scan_handle.as_ref() else {
            return;
        };
        // We need to drain events without holding a borrow of `self.scan_handle`
        // across mutations of other `self.*` fields, so collect first.
        let events: Vec<ScanEvent> = handle.rx.try_iter().collect();
        let mut completed = false;
        for event in events {
            match event {
                ScanEvent::Progress {
                    files,
                    total_size,
                    current,
                } => {
                    self.scan_progress = Some(ScanProgress {
                        files_scanned: files,
                        total_size,
                    });
                    self.scan_current_path = current;
                    // Refresh the radial from the live (mid-scan)
                    // arena. `try_lock` keeps us off the hot path of
                    // the walker thread; if we miss this beat we'll
                    // pick it up at the next Progress (~80ms).
                    self.rebuild_map_from_live();
                }
                ScanEvent::Warning(msg) => {
                    // Surface the most recent warning in the status bar.
                    // A future iteration can keep a bounded ring buffer for
                    // a "warnings" overlay — this single-line view is fine
                    // for v1.
                    self.status_message = format!("warn: {}", msg);
                }
                ScanEvent::Complete(arena) => {
                    self.install_completed_arena(*arena);
                    completed = true;
                }
                ScanEvent::Failed(reason) => {
                    self.status_message = format!("Scan failed: {}", reason);
                    self.mode = AppMode::Viewing;
                    completed = true;
                }
            }
        }
        if completed {
            // Drop the handle; its `Drop` impl joins the worker thread.
            self.scan_handle = None;
            self.scan_current_path = None;
        }
    }

    /// Install a completed arena into the App state and switch to viewing.
    /// Crate-public so `main` can drive an `--import` flow without
    /// kicking off a scan: it constructs the App, calls
    /// [`Self::adopt_loaded_arena`], and the event loop runs as if a
    /// scan had just finished.
    pub(crate) fn adopt_loaded_arena(&mut self, arena: TreeArena, source_label: String) {
        self.scan_handle = None;
        self.scan_progress = None;
        self.scan_current_path = None;
        self.install_completed_arena(arena);
        // Override the standard "Scanned N files" message with one
        // that calls out the snapshot path so users know the data is
        // not live.
        self.status_message = format!("Loaded snapshot: {}", source_label);
    }

    /// Install a completed arena into the App state and switch to viewing.
    fn install_completed_arena(&mut self, arena: TreeArena) {
        match arena.root() {
            Some(root_id) => {
                let total_files = arena.total_file_count(root_id);
                let total_size = arena.folder(root_id).file.size;
                self.arena = Some(arena);
                self.mode = AppMode::Viewing;
                self.rebuild_map();
                self.status_message = format!(
                    "Scanned {} files ({})",
                    total_files,
                    format_size(total_size)
                );
            }
            None => {
                self.status_message = "Scan completed but produced no root entry".to_string();
                self.mode = AppMode::Viewing;
            }
        }
    }

    /// Get status message
    pub fn status_text(&self) -> String {
        match self.mode {
            AppMode::Scanning => {
                let header = if let Some(ref progress) = self.scan_progress {
                    format!(
                        "Scanning... {} files ({})",
                        progress.files_scanned,
                        format_size(progress.total_size)
                    )
                } else {
                    "Scanning...".to_string()
                };
                // Append the most recent path so the user can see what the
                // walker is on, truncated to a sensible width.
                match &self.scan_current_path {
                    Some(path) => {
                        let display = path.display().to_string();
                        let truncated: String = if display.chars().count() > 60 {
                            let tail: String = display
                                .chars()
                                .rev()
                                .take(57)
                                .collect::<Vec<_>>()
                                .into_iter()
                                .rev()
                                .collect();
                            format!("...{}", tail)
                        } else {
                            display
                        };
                        format!("{}  {}", header, truncated)
                    }
                    None => header,
                }
            }
            AppMode::Viewing => {
                if !self.status_message.is_empty() {
                    self.status_message.clone()
                } else if let Some(ref arena) = self.arena {
                    if let Some(root_id) = arena.root() {
                        format!(
                            "{} files ({})",
                            arena.total_file_count(root_id),
                            format_size(arena.folder(root_id).file.size)
                        )
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                }
            }
            AppMode::Help => "Press any key to close help".to_string(),
            AppMode::ConfirmDelete => "Press Y to confirm, any other key to cancel".to_string(),
        }
    }

    /// Get tooltip text for hovered segment
    pub fn tooltip_text(&self) -> Option<String> {
        if let Some(segment) = self.hovered_segment() {
            let mut lines = vec![segment.name.clone(), format_size(segment.size)];

            if segment.is_folder {
                lines.push(format!("{} files", segment.file_count));
            }

            if segment.is_fake {
                lines.push(format!("{} small files", segment.file_count));
            }

            Some(lines.join("\n"))
        } else {
            None
        }
    }
}

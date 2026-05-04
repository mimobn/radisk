# radisk — Architecture

This document describes the runtime layout of `radisk`. It is meant for
contributors and is updated whenever the structure changes. Read this before
the source — it short-circuits a lot of reading.

> **Status**: living document. Sections marked **(planned)** describe work in
> progress; everything else reflects the code on the current branch.

---

## High-level layers

```
┌───────────────────────────────────────────────────────────┐
│ main.rs                                                   │
│   - parse CLI, set up terminal (raw mode + alt screen)    │
│   - install panic hook + Drop guard for terminal restore  │
│   - drive the event loop                                  │
└───────────────────────────────────────────────────────────┘
                         │
                         ▼
┌───────────────────────────────────────────────────────────┐
│ app.rs                                                    │
│   App: state machine (AppMode), input handlers, sidebar,  │
│   delete confirmation, navigation history, context menu   │
│   dispatch. Holds the arena, the radial map, and a        │
│   ScanHandle whose channel is drained each frame.         │
└───────────────────────────────────────────────────────────┘
        │                  │                          │
        │                  │                          ▼
        │                  │                ┌─────────────┐
        │                  │                │ ui.rs       │
        │                  │                │ ratatui     │
        │                  │                │ layout +    │
        │                  │                │ sidebar +   │
        │                  │                │ help/dialog │
        │                  ▼                └─────────────┘
        │        ┌─────────────────┐
        │        │ tree.rs         │
        │        │ Arena (Vec-     │
        │        │ backed) of      │
        │        │ File / Folder   │
        │        │ nodes           │
        │        └─────────────────┘
        ▼
┌──────────────────────────────────────────────────────────┐
│ scanner_streaming.rs (production walker)                 │
│   jwalk parallel walk on a worker thread; consumer       │
│   thread builds the arena single-threaded; emits         │
│   ScanEvent::{Progress, Warning, Complete, Failed} over  │
│   std::mpsc to the App.                                  │
│                                                          │
│ scanner.rs (reference walker)                            │
│   Single-threaded recursive walker kept for tests and    │
│   as a portable fallback. Marked #[allow(dead_code)] in  │
│   prod since the App no longer calls it.                 │
└──────────────────────────────────────────────────────────┘
                                                    │
                                                    ▼
                                       ┌────────────────────────┐
                                       │ radial.rs / renderer.rs│
                                       │ Angle math + Braille   │
                                       │ canvas painting        │
                                       └────────────────────────┘
                                                    │
                                                    ▼
                                       ┌────────────────────────┐
                                       │ color.rs               │
                                       │ Material palette       │
                                       │ (HSL/Lab math)         │
                                       └────────────────────────┘

context_menu.rs is a small leaf consumed by app.rs.
```

---

## Module responsibilities

| Module | Owns | Knows about |
|--------|------|-------------|
| `main` | CLI, config bootstrap, terminal lifecycle, event loop | `App`, `Config`, ratatui backend |
| `app` | `App` state, input dispatch (via `Action`), scan orchestration | `scanner`, `scanner_streaming`, `tree`, `radial`, `renderer`, `ui`, `context_menu`, `config`, `keybinds` |
| `config` | TOML loader, smart-merge defaults, `to_scan_config()` bridge | `scanner` (for `DEFAULT_MAX_DEPTH`) |
| `keybinds` | `Action` enum, `KeyChord` DSL parser, `Keybinds::from_config` | `config` |
| `scanner_streaming` | Production walker (jwalk), `ScanEvent`, `ScanHandle` | `scanner` (`ScanConfig`), `tree` |
| `scanner` | Reference walker, `ScanConfig`, `ScanError`, `DEFAULT_MAX_DEPTH` | `tree` |
| `tree` | `TreeArena`, `File`, `Folder`, `FolderId`, `FileId` | nothing internal |
| `radial` | `RadialMap` and segment angles | `tree` |
| `renderer` | Braille canvas painting + segment lookup | `radial`, `color` |
| `ui` | ratatui layout, sidebar, help, status, confirm dialog, view dispatch | `app`, `radial`, `renderer`, `color`, `views` |
| `views` | `View` enum (Radial, Tree), tree-view row builder + renderer | `app`, `tree` |
| `delete` | `DeleteStrategy` (TrashPut, GioTrash, Permanent) detection + TOCTOU-checked delete | nothing internal |
| `snapshot` | `.radisk` export/import (RDSK magic, u16 version, zstd+postcard arena) | `tree` |
| `diff` | Folder-level snapshot comparison (`folder_diff`, `format_diff`) | `tree` |
| `mounts` | `/proc/mounts` parser + `statvfs` (Linux); pseudo-fs filter | `nix` (Linux only) |
| `picker` | Pre-scan partition-style mount picker (ratatui) | `mounts`, `tree` |
| `theme` | `Role` enum + hex/ansi palette from `[colors]` | `config` |
| `ownership` | Multi-PM ownership lookup: pacman/AUR, dpkg, rpm, apk; npm/pip/uv/cargo/flatpak/snap path patterns | `OnceLock` for caches |
| `color` | Palette, gradients, HSL/Lab math | nothing internal |
| `context_menu` | Right-click menu state | nothing internal |

---

## Data flow

1. **Startup**: `main` parses CLI, canonicalises the target path, builds
   `App::new(...)`. `App::start_scan` spins a progress thread (mpsc-based) and
   runs a synchronous scan into a `TreeArena`. **(planned)** the synchronous
   part will be replaced by a streaming parallel walker.
2. **After scan**: `App` builds a `RadialMap` from the arena via
   `radial::build_radial_map`. The map's segments carry stable `Uuid`s used
   for hover/selection.
3. **Frame**: `ui::render` lays out sidebar + canvas, calls
   `RadialRenderer::render` for the canvas, then overlays help / confirm /
   context-menu as appropriate.
4. **Input**: `App::handle_key` / `handle_mouse` mutate state and may trigger
   a `rebuild_map` (e.g. on navigate, zoom, resize).
5. **Delete**: `trigger_delete` stages a path + a default choice (now **No**)
   into `AppMode::ConfirmDelete`; if the user confirms, the path is removed
   and the tree is rescanned.

---

## Key invariants

- **Arena lifecycle**: `App::arena` is `None` while scanning, `Some(...)` once
  the first scan completes. Code paths that need it use `if let Some(arena)
  = self.arena.as_ref()` rather than `unwrap`. **(planned)** the type will be
  refined to make this state explicit at compile time (`enum AppData`).
- **Radial map lifecycle**: parallel to the arena — `None` while scanning,
  `Some(...)` afterwards. `ui::render` short-circuits if absent.
- **`TreeArena::root` after `set_root`**: always `Some(root_id)`. The few
  places that call `arena.root()` outside the post-scan path treat `None` as a
  recoverable error rather than panicking.
- **Inode dedup is best-effort**: hard-link files are counted once per
  `(dev, ino)` pair on Unix; on other platforms duplicates are counted.
- **Sort ordering** is decoupled from the arena: `TreeArena::folder_items`
  always returns size-descending (preserving the radial layout's
  contract), while `TreeArena::folder_items_sorted(_, mode)` honours
  the user's `SortMode`. The sidebar and tree view route through the
  latter via `App::sidebar_items`.
- **Size accounting** is determined by `ScanConfig::use_apparent_size`
  at scan time. Toggling this in-app re-runs the streaming walker
  because every node's stored size changes.
- **Excludes** are applied entry-by-entry in the streaming walker
  against both the full path and the base name. Patterns that fail to
  parse become `ScanEvent::Warning` and the rest of the matcher is
  built without them.

---

## Error handling

- The application uses `anyhow::Result<T>` at boundaries (`main`, terminal
  setup, top-level event loop calls).
- The scanner exposes a typed `ScanError` (`thiserror`-derived **(planned)**)
  so callers can match on `PermissionDenied` and continue rather than abort.
- `unwrap()` / `expect()` are confined to test code and to spots where the
  invariant is documented and locally verifiable.

---

## Build & verify

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo doc --no-deps
```

CI **(planned)** will run all of the above on every PR.

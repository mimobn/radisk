# Configuration reference

radisk reads a TOML file at startup. Every key is **optional**;
missing keys fall back to compiled-in defaults. A completely empty
file is valid and behaves exactly like running with no config at all.

## File location

| Platform | Default path |
| -------- | ------------ |
| Linux    | `$XDG_CONFIG_HOME/radisk/config.toml` (typically `~/.config/radisk/config.toml`) |
| macOS    | `~/Library/Application Support/radisk/config.toml` |
| Windows  | `%APPDATA%\radisk\config.toml` |

Override with `--config /path/to/file.toml` from the CLI.

A complete annotated example ships at
[`docs/config.example.toml`](config.example.toml). Drop it into your
config dir and uncomment what you want to change.

## Sections

### `[display]`

Visual layout of the main window.

| Key | Type | Default | Range / Notes |
| --- | --- | --- | --- |
| `ring_depth` | integer | `5` | 1..=20. Number of concentric rings in the radial view. CLI `--depth` overrides this. |
| `sidebar_percent` | integer | `25` | clamped to 10..=60. Percentage of terminal width used by the sidebar. |

### `[scan]`

Filesystem walk behaviour.

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `follow_symlinks` | bool | `false` | Match `du`/`ncdu` semantics by default. |
| `max_depth` | integer | `4096` | Stack-bounding ceiling. Lower it to scan only top levels. |
| `use_apparent_size` | bool | `false` | When `true`, account by `metadata.len()` (what `ls -l` shows) instead of `st_blocks * 512` (what `du` shows). Useful on sparse files / btrfs / zfs CoW. Toggle in-app with `a`. |
| `exclude` | array of string | `[]` | Glob patterns matched against full path **and** base name. `--exclude PAT` adds to (does not replace) this list. |

### `[keybinds]`

Action → chord overrides. See [`KEYBINDS.md`](KEYBINDS.md) for the
full action list and chord DSL.

### `[colors]`

UI palette overrides. Two value shapes:

- `"#rrggbb"` — true-colour 24-bit hex.
- `"ansi:N"` — indexed palette (`N ∈ 0..=255`). Lets wallust / pywal
  users have radisk follow their terminal-theme manager without
  re-stating every hex value.

| Role | Default | Used for |
| --- | --- | --- |
| `foreground`     | `White` | Body text colour |
| `file`           | `White` | File rows in sidebar / tree view |
| `folder`         | `Cyan`  | Folder rows (also bolded) |
| `selection_bg`   | `DarkGray` | Background of the selected row |
| `border`         | `DarkGray` | Border of an unfocused panel |
| `border_focused` | `White` | Border of the focused panel |
| `status`         | `White` | Status-bar foreground |

Unknown role names and malformed colour values become startup
warnings (visible in the initial status bar) rather than aborting
the App — a typo can never lock you out of the tool.

## Error handling

| Situation                     | Behaviour                                              |
| ----------------------------- | ------------------------------------------------------ |
| File missing                  | Use compiled defaults silently.                        |
| File present, malformed TOML  | Anyhow error at startup with file:line of the parser. |
| Unknown TOML key              | Ignored silently — your config keeps working when you upgrade radisk and it adopts new keys. |
| Bad value (e.g. `ring_depth = -3`) | Clamped to a safe range; never aborts.            |
| Bad keybind chord             | Warning at startup; the affected action falls back to its default chord. |
| Bad colour value              | Warning at startup; the affected role keeps its default colour. |

## Examples

**Minimal — change one thing:**

```toml
[display]
ring_depth = 7
```

**Wallust / pywal user:**

```toml
[colors]
folder         = "ansi:4"
selection_bg   = "ansi:8"
border_focused = "ansi:3"
```

**Heavy excludes for a code workstation:**

```toml
[scan]
exclude = [
    "node_modules",
    "**/target/**",
    "**/.cache/**",
    "**/.venv/**",
    "**/__pycache__/**",
]
```

**vim user with a custom command leader:**

```toml
[keybinds]
quit              = "ctrl+c"
toggle_view       = "f2"
cycle_sort        = "f3"
toggle_apparent_size = "f4"
```

## Where to file feedback

If the smart-merge defaults bite you (e.g. clamping a value you
expected to use literally), or you want a config key the schema
doesn't have yet, file an issue. The `[colors]` and `[keybinds]`
schemas are stable across releases — adding new entries doesn't
break old configs.

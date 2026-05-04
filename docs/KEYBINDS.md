# Keybinds

radisk's input model is built around a closed set of *actions* and a
rebindable *chord → action* table. Every action has a stable
`config_name` you can reference in your `~/.config/radisk/config.toml`.

## Default chords

### Navigation

| Action          | Default chords                | `config_name`       |
| --------------- | ----------------------------- | ------------------- |
| Quit            | `q`, `Esc`                    | `quit`              |
| Help overlay    | `?`                           | `help`              |
| Parent dir      | `h`, `←`, `u`, `Backspace`    | `navigate_up`       |
| Descend         | `l`, `→`, `Enter`             | `navigate_into`     |
| Move down       | `j`, `↓`                      | `move_down`         |
| Move up         | `k`, `↑`                      | `move_up`           |
| Jump to first   | `gg` (two-key)                | `move_to_first`     |
| Jump to last    | `G`                           | `move_to_last`      |
| Half-page down  | `Ctrl-d`                      | `move_half_page_down` |
| Half-page up    | `Ctrl-u`                      | `move_half_page_up` |
| Toggle focus    | `Tab`                         | `toggle_focus`      |

### Display

| Action                | Default | `config_name`           |
| --------------------- | ------- | ----------------------- |
| Cycle view            | `v`     | `toggle_view`           |
| Cycle sort mode       | `S`     | `cycle_sort`            |
| Toggle apparent size  | `a`     | `toggle_apparent_size`  |
| Zoom in (more rings)  | `+`, `=` | `zoom_in`              |
| Zoom out              | `-`     | `zoom_out`              |

### Actions on entries

| Action            | Default  | `config_name`     |
| ----------------- | -------- | ----------------- |
| Rescan            | `r`      | `rescan`          |
| Delete            | `d`      | `delete`          |
| Toggle multi-sel  | `Space`  | `toggle_select`   |
| Batch delete      | `Shift+D` | `delete_selected` |
| Clear selection   | `Shift+X` | `clear_selection` |
| Show owner        | `o`      | `show_owner`      |

## Chord syntax

The string you put in `config.toml` follows a small DSL — the same
one used in `KeyChord::parse` in `src/keybinds.rs`.

```text
"q"          → KeyCode::Char('q'), no modifiers
"?"          → KeyCode::Char('?')
"esc"        → KeyCode::Esc           (case-insensitive)
"enter"      → KeyCode::Enter
"tab"        → KeyCode::Tab
"backspace"  → KeyCode::Backspace
"space"      → KeyCode::Char(' ')
"up"/"down"/"left"/"right"
"home"/"end"/"pageup"/"pagedown"
"f1" .. "f12"

"ctrl+q"            → CONTROL + 'q'
"shift+up"          → SHIFT + Up
"alt+enter"         → ALT + Enter           ("meta"/"option" alias)
"ctrl+shift+pgdn"   → multiple modifiers, '+'-joined
"super+l"           → SUPER + 'l'           ("win"/"cmd" alias)
```

A bare letter normalises away SHIFT, so a config of `"q"` matches
whether or not your terminal forwards SHIFT alongside the keypress —
some terminals do, some don't, and you shouldn't have to care.

## Example overrides

```toml
[keybinds]
# Use Ctrl+C to quit (in addition to / instead of q + Esc)
quit                 = "ctrl+c"

# vim users who want : as a "command" prefix later — bind to no-op now
# help                 = ":"

# Use n / N (next / previous) instead of j / k
move_down            = "n"
move_up              = "shift+n"

# Use Ctrl+S to save a quick snapshot — currently unbinding by
# default, but Phase 19+ will surface this kind of action.

# Bind tree-view toggle to a function key
toggle_view          = "f2"
```

## Override semantics

When you set a key under `[keybinds]`, the override **replaces every
default chord for that action** and adds your single chord. Other
actions keep their defaults. So if you write:

```toml
[keybinds]
quit = "ctrl+c"
```

the bindings for `q` and `Esc` are removed (only `Ctrl-c` quits), but
`?` still opens help, `h` still navigates up, etc.

If you want multiple chords per action, file an issue — Phase 19+
might lift the table from string to array.

## When it goes wrong

- A typo in the chord string (`"ctlr+q"`) prints a warning to stderr
  at startup and the App falls back to the default chord for that
  action — you keep a working keymap.
- A typo in the action name (`"qiut"`) prints a warning listing every
  valid action and is otherwise ignored.
- Run with `RUST_LOG=warn` to see the warnings even when the status
  bar gets overwritten by your first scan.

## Mode-specific keys (not rebindable)

A few chords live outside the action system because they only make
sense in one mode:

- **In the help overlay**: `q`, `Esc`, `?`, `Enter` close it.
- **In the delete confirmation dialog**: `y` / `Y` confirm; `n` / `N`
  / `Esc` cancel; `h` / `l` / `Tab` / arrows / `j` / `k` toggle the
  Yes/No selection.
- **In the mount picker** (`--mounts`): `j` / `k` / arrows move,
  `Enter` selects, `g` / `G` first/last, `q` / `Esc` cancels.

These will move into the rebindable system when Phase 19+ adds
mode-aware chord tables.

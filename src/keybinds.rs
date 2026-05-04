//! Rebindable keybindings for the in-app `Viewing` mode.
//!
//! Architecture
//!
//! 1. [`Action`] is a closed set of user-bindable verbs the App
//!    understands. Adding a new action means adding a variant here and a
//!    match arm in `App::handle_action`.
//! 2. [`KeyChord`] is the wire-form of a key+modifier combination. It
//!    parses from a small string DSL (`"q"`, `"esc"`, `"ctrl+q"`,
//!    `"shift+down"`, `"alt+enter"`) so the TOML config can stay
//!    human-friendly.
//! 3. [`Keybinds`] holds the chord→action map. Defaults are baked in;
//!    user overrides loaded from [`crate::config::KeybindsConfig`]
//!    *replace* every default chord for the action they reference, then
//!    add the supplied chord. This keeps the resolution rule simple:
//!    "the user's chord, plus any defaults that survived for *other*
//!    actions, is what runs."
//!
//! The Phase 3 surface intentionally stays small — it covers every
//! current `Viewing`-mode key but leaves Help, Confirm, and Context
//! Menu modes on their hard-coded handlers. Subsequent phases can
//! lift those into [`Action`] without breaking the config schema.

use crate::config::KeybindsConfig;
use anyhow::{anyhow, bail, Context, Result};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::HashMap;

/// Closed set of bindable actions in the `Viewing` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Quit,
    Help,
    NavigateUp,
    NavigateInto,
    ZoomIn,
    ZoomOut,
    Rescan,
    Delete,
    ToggleFocus,
    MoveUp,
    MoveDown,
    /// Cycle through the available [`crate::views::View`]s — radial,
    /// tree, etc.
    ToggleView,
    /// Cycle the [`crate::tree::SortMode`] used to order children in
    /// the sidebar and tree view (radial geometry stays size-driven).
    CycleSort,
    /// Toggle between apparent file size (`metadata.len()`) and
    /// on-disk size (`st_blocks * 512` on Unix). Triggers a rescan.
    ToggleApparentSize,
    /// Toggle the current sidebar item in/out of the multi-select
    /// set used by [`Action::DeleteSelected`].
    ToggleSelect,
    /// Open the delete confirmation dialog for every item currently
    /// in the multi-select set.
    DeleteSelected,
    /// Clear every entry from the multi-select set.
    ClearSelection,
    /// Show the pacman package owner of the selected entry in the
    /// status bar (Arch-only; no-op on other distros).
    ShowOwner,
    /// Jump to the first sidebar item (vim-style `gg`).
    MoveToFirst,
    /// Jump to the last sidebar item (vim-style `G`).
    MoveToLast,
    /// Move half a screen down (vim-style `Ctrl-d`).
    MoveHalfPageDown,
    /// Move half a screen up (vim-style `Ctrl-u`).
    MoveHalfPageUp,
}

impl Action {
    /// Canonical config name for this action. Stable: changing one
    /// breaks every existing user config that references it.
    pub fn config_name(self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::Help => "help",
            Action::NavigateUp => "navigate_up",
            Action::NavigateInto => "navigate_into",
            Action::ZoomIn => "zoom_in",
            Action::ZoomOut => "zoom_out",
            Action::Rescan => "rescan",
            Action::Delete => "delete",
            Action::ToggleFocus => "toggle_focus",
            Action::MoveUp => "move_up",
            Action::MoveDown => "move_down",
            Action::ToggleView => "toggle_view",
            Action::CycleSort => "cycle_sort",
            Action::ToggleApparentSize => "toggle_apparent_size",
            Action::ToggleSelect => "toggle_select",
            Action::DeleteSelected => "delete_selected",
            Action::ClearSelection => "clear_selection",
            Action::ShowOwner => "show_owner",
            Action::MoveToFirst => "move_to_first",
            Action::MoveToLast => "move_to_last",
            Action::MoveHalfPageDown => "move_half_page_down",
            Action::MoveHalfPageUp => "move_half_page_up",
        }
    }

    fn from_config_name(s: &str) -> Option<Self> {
        match s {
            "quit" => Some(Action::Quit),
            "help" => Some(Action::Help),
            "navigate_up" => Some(Action::NavigateUp),
            "navigate_into" => Some(Action::NavigateInto),
            "zoom_in" => Some(Action::ZoomIn),
            "zoom_out" => Some(Action::ZoomOut),
            "rescan" => Some(Action::Rescan),
            "delete" => Some(Action::Delete),
            "toggle_focus" => Some(Action::ToggleFocus),
            "move_up" => Some(Action::MoveUp),
            "move_down" => Some(Action::MoveDown),
            "toggle_view" => Some(Action::ToggleView),
            "cycle_sort" => Some(Action::CycleSort),
            "toggle_apparent_size" => Some(Action::ToggleApparentSize),
            "toggle_select" => Some(Action::ToggleSelect),
            "delete_selected" => Some(Action::DeleteSelected),
            "clear_selection" => Some(Action::ClearSelection),
            "show_owner" => Some(Action::ShowOwner),
            "move_to_first" => Some(Action::MoveToFirst),
            "move_to_last" => Some(Action::MoveToLast),
            "move_half_page_down" => Some(Action::MoveHalfPageDown),
            "move_half_page_up" => Some(Action::MoveHalfPageUp),
            _ => None,
        }
    }

    /// Every action — used to enumerate defaults and validate config
    /// keys without having to hand-maintain a parallel list.
    fn all() -> &'static [Action] {
        &[
            Action::Quit,
            Action::Help,
            Action::NavigateUp,
            Action::NavigateInto,
            Action::ZoomIn,
            Action::ZoomOut,
            Action::Rescan,
            Action::Delete,
            Action::ToggleFocus,
            Action::MoveUp,
            Action::MoveDown,
            Action::ToggleView,
            Action::CycleSort,
            Action::ToggleApparentSize,
            Action::ToggleSelect,
            Action::DeleteSelected,
            Action::ClearSelection,
            Action::ShowOwner,
            Action::MoveToFirst,
            Action::MoveToLast,
            Action::MoveHalfPageDown,
            Action::MoveHalfPageUp,
        ]
    }
}

/// A key + modifier combination, normalised so that comparisons are
/// modifier-agnostic for plain ASCII characters (typing 'q' produces
/// `KeyEvent { code: 'q', mods: NONE }`; we never want a config of
/// `"q"` to require the user to hold Shift).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        // Normalise: a bare printable character implicitly carries SHIFT
        // when it's an uppercase letter. We strip SHIFT for letter keys
        // because the KeyCode already encodes the case, and matching on
        // `Char('q'), NONE` should win whether or not the terminal sent
        // SHIFT alongside the lowercase letter.
        let mods = match code {
            KeyCode::Char(c) if c.is_ascii_alphanumeric() => mods - KeyModifiers::SHIFT,
            _ => mods,
        };
        Self { code, mods }
    }

    pub fn from_event(ev: KeyEvent) -> Self {
        Self::new(ev.code, ev.modifiers)
    }

    /// Parse a chord from the small DSL described at the module level.
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if s.is_empty() {
            bail!("empty key chord");
        }

        let mut mods = KeyModifiers::NONE;
        let mut tail = s;

        // Modifiers are `+`-separated and case-insensitive.
        while let Some((head, rest)) = tail.split_once('+') {
            let head_lower = head.trim().to_ascii_lowercase();
            match head_lower.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "shift" => mods |= KeyModifiers::SHIFT,
                "alt" | "meta" | "option" => mods |= KeyModifiers::ALT,
                "super" | "win" | "cmd" => mods |= KeyModifiers::SUPER,
                // Not a known modifier — assume the rest of the string
                // is a key name that itself contains `+` (e.g. the literal
                // `'+'` key). Bail out of the modifier loop.
                _ => break,
            }
            tail = rest.trim();
        }

        let code =
            parse_key_name(tail).with_context(|| format!("unknown key in chord: {:?}", s))?;
        Ok(Self::new(code, mods))
    }
}

fn parse_key_name(s: &str) -> Result<KeyCode> {
    let s_trim = s.trim();
    if s_trim.is_empty() {
        bail!("empty key name");
    }
    // A single char is the most common case.
    let mut chars = s_trim.chars();
    let first = chars
        .next()
        .ok_or_else(|| anyhow!("empty key name after modifier strip"))?;
    if chars.next().is_none() {
        return Ok(KeyCode::Char(first));
    }

    // Otherwise compare against the named keys.
    let lower = s_trim.to_ascii_lowercase();
    Ok(match lower.as_str() {
        "esc" | "escape" => KeyCode::Esc,
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" | "ins" => KeyCode::Insert,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        // "f1" .. "f12"
        n if n.starts_with('f') && n[1..].chars().all(|c| c.is_ascii_digit()) => {
            let num: u8 = n[1..]
                .parse()
                .with_context(|| format!("bad function-key number in {:?}", s))?;
            KeyCode::F(num)
        }
        other => bail!("unknown key name: {:?}", other),
    })
}

/// Resolved keybind table. Built once at startup from
/// [`KeybindsConfig`] over the compiled-in defaults.
#[derive(Debug, Clone)]
pub struct Keybinds {
    map: HashMap<KeyChord, Action>,
}

impl Keybinds {
    /// Compiled-in defaults — the bindings users have today.
    pub fn defaults() -> Self {
        let mut map = HashMap::new();
        let mut add = |code: KeyCode, mods: KeyModifiers, action: Action| {
            map.insert(KeyChord::new(code, mods), action);
        };

        add(KeyCode::Char('q'), KeyModifiers::NONE, Action::Quit);
        add(KeyCode::Esc, KeyModifiers::NONE, Action::Quit);

        add(KeyCode::Char('?'), KeyModifiers::NONE, Action::Help);

        add(KeyCode::Char('u'), KeyModifiers::NONE, Action::NavigateUp);
        add(KeyCode::Backspace, KeyModifiers::NONE, Action::NavigateUp);
        // Vim alternates: h goes "left" (out of the directory),
        // l goes "right" (into the highlighted directory). Mirror
        // arrow-key behaviour for users who don't live on hjkl.
        add(KeyCode::Char('h'), KeyModifiers::NONE, Action::NavigateUp);
        add(KeyCode::Left, KeyModifiers::NONE, Action::NavigateUp);

        add(KeyCode::Enter, KeyModifiers::NONE, Action::NavigateInto);
        add(KeyCode::Char('l'), KeyModifiers::NONE, Action::NavigateInto);
        add(KeyCode::Right, KeyModifiers::NONE, Action::NavigateInto);

        add(KeyCode::Char('+'), KeyModifiers::NONE, Action::ZoomIn);
        add(KeyCode::Char('='), KeyModifiers::NONE, Action::ZoomIn);
        add(KeyCode::Char('-'), KeyModifiers::NONE, Action::ZoomOut);

        add(KeyCode::Char('r'), KeyModifiers::NONE, Action::Rescan);
        add(KeyCode::Char('d'), KeyModifiers::NONE, Action::Delete);
        add(KeyCode::Tab, KeyModifiers::NONE, Action::ToggleFocus);

        add(KeyCode::Up, KeyModifiers::NONE, Action::MoveUp);
        add(KeyCode::Char('k'), KeyModifiers::NONE, Action::MoveUp);
        add(KeyCode::Down, KeyModifiers::NONE, Action::MoveDown);
        add(KeyCode::Char('j'), KeyModifiers::NONE, Action::MoveDown);

        add(KeyCode::Char('v'), KeyModifiers::NONE, Action::ToggleView);

        // Capital S so a single 's' stays free for a future "search"
        // action (ncdu has one), and to match the convention that
        // "destructive" or "structural" toggles use the Shift form.
        add(KeyCode::Char('S'), KeyModifiers::SHIFT, Action::CycleSort);

        // Lowercase 'a' for "apparent" — ncdu uses the same chord.
        add(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            Action::ToggleApparentSize,
        );

        // Multi-select chords. Space toggles selection of the current
        // sidebar item; capital D triggers the batch delete; capital
        // X clears every selection.
        add(KeyCode::Char(' '), KeyModifiers::NONE, Action::ToggleSelect);
        add(
            KeyCode::Char('D'),
            KeyModifiers::SHIFT,
            Action::DeleteSelected,
        );
        add(
            KeyCode::Char('X'),
            KeyModifiers::SHIFT,
            Action::ClearSelection,
        );

        // Lowercase 'o' for "owner" — Arch users will recognise it
        // as the pacman -Qo flag.
        add(KeyCode::Char('o'), KeyModifiers::NONE, Action::ShowOwner);

        // Vim navigation extras. `G` jumps to the bottom of the
        // sidebar; the matching `gg` (top) is handled in
        // `App::handle_viewing_key` because chord sequences
        // aren't part of the single-keypress lookup table.
        add(KeyCode::Char('G'), KeyModifiers::SHIFT, Action::MoveToLast);
        add(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
            Action::MoveHalfPageDown,
        );
        add(
            KeyCode::Char('u'),
            KeyModifiers::CONTROL,
            Action::MoveHalfPageUp,
        );

        Self { map }
    }

    /// Build a keybind table by overlaying user overrides on
    /// [`Self::defaults`]. For every action the user mentions in
    /// `[keybinds]`, every default chord that pointed to that action is
    /// removed first and the user's chord is inserted. Other actions
    /// keep their defaults untouched.
    pub fn from_config(cfg: &KeybindsConfig) -> Result<Self> {
        let mut me = Self::defaults();
        for (raw_action, raw_chord) in &cfg.overrides {
            let action = Action::from_config_name(raw_action).ok_or_else(|| {
                anyhow!(
                    "unknown action in [keybinds]: {:?}; valid: {}",
                    raw_action,
                    Action::all()
                        .iter()
                        .map(|a| a.config_name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            let chord = KeyChord::parse(raw_chord)
                .with_context(|| format!("invalid chord for [keybinds].{}", raw_action))?;
            // Drop existing chords that map to this action.
            me.map.retain(|_, a| *a != action);
            me.map.insert(chord, action);
        }
        Ok(me)
    }

    /// Look up the bound action for an incoming key event, if any.
    pub fn action_for(&self, ev: KeyEvent) -> Option<Action> {
        self.map.get(&KeyChord::from_event(ev)).copied()
    }
}

impl Default for Keybinds {
    fn default() -> Self {
        Self::defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn parse_simple_chars() {
        assert_eq!(
            KeyChord::parse("q").unwrap(),
            KeyChord::new(KeyCode::Char('q'), KeyModifiers::NONE)
        );
        assert_eq!(
            KeyChord::parse("?").unwrap(),
            KeyChord::new(KeyCode::Char('?'), KeyModifiers::NONE)
        );
    }

    #[test]
    fn parse_named_keys() {
        assert_eq!(KeyChord::parse("esc").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyChord::parse("Escape").unwrap().code, KeyCode::Esc);
        assert_eq!(KeyChord::parse("PgDn").unwrap().code, KeyCode::PageDown);
        assert_eq!(KeyChord::parse("F5").unwrap().code, KeyCode::F(5));
    }

    #[test]
    fn parse_modifiers() {
        let chord = KeyChord::parse("ctrl+q").unwrap();
        assert_eq!(chord.code, KeyCode::Char('q'));
        assert!(chord.mods.contains(KeyModifiers::CONTROL));
        assert!(!chord.mods.contains(KeyModifiers::SHIFT));

        let chord = KeyChord::parse("ctrl+shift+up").unwrap();
        assert_eq!(chord.code, KeyCode::Up);
        assert!(chord.mods.contains(KeyModifiers::CONTROL));
        assert!(chord.mods.contains(KeyModifiers::SHIFT));

        let chord = KeyChord::parse("alt+enter").unwrap();
        assert_eq!(chord.code, KeyCode::Enter);
        assert!(chord.mods.contains(KeyModifiers::ALT));
    }

    #[test]
    fn parse_unknown_key_errors_with_context() {
        let err = KeyChord::parse("ctrl+gobbledygook").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("unknown key"), "missing context in: {}", msg);
    }

    #[test]
    fn defaults_cover_every_existing_binding() {
        let kb = Keybinds::defaults();
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('q'), KeyModifiers::NONE)),
            Some(Action::Quit)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Esc, KeyModifiers::NONE)),
            Some(Action::Quit)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('?'), KeyModifiers::NONE)),
            Some(Action::Help)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('u'), KeyModifiers::NONE)),
            Some(Action::NavigateUp)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Backspace, KeyModifiers::NONE)),
            Some(Action::NavigateUp)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Enter, KeyModifiers::NONE)),
            Some(Action::NavigateInto)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Tab, KeyModifiers::NONE)),
            Some(Action::ToggleFocus)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('j'), KeyModifiers::NONE)),
            Some(Action::MoveDown)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('k'), KeyModifiers::NONE)),
            Some(Action::MoveUp)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('+'), KeyModifiers::NONE)),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('='), KeyModifiers::NONE)),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('-'), KeyModifiers::NONE)),
            Some(Action::ZoomOut)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(Action::Rescan)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('d'), KeyModifiers::NONE)),
            Some(Action::Delete)
        );
    }

    #[test]
    fn override_replaces_defaults_for_that_action_only() {
        let mut cfg = KeybindsConfig::default();
        cfg.overrides.insert("quit".into(), "ctrl+c".into());
        let kb = Keybinds::from_config(&cfg).unwrap();

        // New chord works.
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(Action::Quit)
        );
        // Defaults for quit are gone.
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('q'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(kb.action_for(ev(KeyCode::Esc, KeyModifiers::NONE)), None);
        // Defaults for *other* actions survive.
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('?'), KeyModifiers::NONE)),
            Some(Action::Help)
        );
    }

    #[test]
    fn unknown_action_in_config_errors() {
        let mut cfg = KeybindsConfig::default();
        cfg.overrides
            .insert("teleport_to_mars".into(), "ctrl+m".into());
        let err = Keybinds::from_config(&cfg).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("unknown action"), "msg = {}", msg);
        assert!(msg.contains("teleport_to_mars"), "msg = {}", msg);
    }

    #[test]
    fn invalid_chord_in_config_errors_with_action_name() {
        let mut cfg = KeybindsConfig::default();
        cfg.overrides.insert("quit".into(), "not a chord".into());
        let err = Keybinds::from_config(&cfg).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("[keybinds].quit"), "msg = {}", msg);
    }

    #[test]
    fn vim_aliases_are_bound() {
        let kb = Keybinds::defaults();
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('h'), KeyModifiers::NONE)),
            Some(Action::NavigateUp)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('l'), KeyModifiers::NONE)),
            Some(Action::NavigateInto)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Left, KeyModifiers::NONE)),
            Some(Action::NavigateUp)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Right, KeyModifiers::NONE)),
            Some(Action::NavigateInto)
        );
    }

    #[test]
    fn vim_navigation_extras_are_bound() {
        let kb = Keybinds::defaults();
        // G — capital G, normalised SHIFT-stripped.
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('G'), KeyModifiers::SHIFT)),
            Some(Action::MoveToLast)
        );
        // Ctrl-d / Ctrl-u
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('d'), KeyModifiers::CONTROL)),
            Some(Action::MoveHalfPageDown)
        );
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('u'), KeyModifiers::CONTROL)),
            Some(Action::MoveHalfPageUp)
        );
    }

    #[test]
    fn shift_letter_normalises_to_lowercase_chord() {
        // Some terminals send 'Q' with SHIFT; others send 'q' with SHIFT.
        // Normalisation should make a config of `"q"` match either.
        let kb = Keybinds::defaults();
        assert_eq!(
            kb.action_for(ev(KeyCode::Char('q'), KeyModifiers::SHIFT)),
            Some(Action::Quit)
        );
    }
}

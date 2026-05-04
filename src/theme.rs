//! User-themable colour palette for UI chrome.
//!
//! Phase 3 shipped the `[colors]` config section as a parsed
//! `BTreeMap<String, String>` reserved for future use. This module
//! turns that data into actual ratatui [`Color`] values for every
//! "role" the renderer paints — sidebar foreground, selection
//! highlight, status bar, tooltip border, etc.
//!
//! ## Wire format
//!
//! Two value shapes are accepted, in the same string namespace as
//! the chord DSL:
//!
//! - `"#rrggbb"` true-colour hex (24-bit). Used directly when the
//!   terminal supports it.
//! - `"ansi:N"` indexed palette with `N ∈ 0..=255`. Useful for
//!   wallust / pywal users who want radisk to follow their theme
//!   manager — the indices map to whatever the terminal palette
//!   resolves them to.
//!
//! Unrecognised role names are ignored (so a user can copy a
//! future-version config back without errors); unparseable values
//! fall back to the compiled-in default with a status-bar warning
//! routed via [`Theme::warnings`].
//!
//! ## Roles
//!
//! Every role has a sensible default that matches the look radisk
//! shipped with before Phase 11 — overriding nothing yields the
//! same screen as a v0.6 user remembers.

use crate::config::ColorsConfig;
use ratatui::style::Color;

/// Closed set of UI roles that the renderer paints. Adding a new
/// role means: a new variant here, a default in `Theme::default`,
/// and an `apply` arm. Stable across releases — renaming one
/// breaks every existing user config that referenced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Default foreground for body text.
    Foreground,
    /// File-row label colour in the sidebar / tree view.
    File,
    /// Folder-row label colour (also bolded).
    Folder,
    /// Background of the row under the selection cursor.
    SelectionBg,
    /// Border colour for an unfocused panel.
    Border,
    /// Border colour for the focused panel.
    BorderFocused,
    /// Foreground of the status bar.
    Status,
}

impl Role {
    /// Canonical config name. Stable; renaming breaks user configs.
    pub fn config_name(self) -> &'static str {
        match self {
            Role::Foreground => "foreground",
            Role::File => "file",
            Role::Folder => "folder",
            Role::SelectionBg => "selection_bg",
            Role::Border => "border",
            Role::BorderFocused => "border_focused",
            Role::Status => "status",
        }
    }

    fn from_config_name(s: &str) -> Option<Self> {
        match s {
            "foreground" => Some(Role::Foreground),
            "file" => Some(Role::File),
            "folder" => Some(Role::Folder),
            "selection_bg" => Some(Role::SelectionBg),
            "border" => Some(Role::Border),
            "border_focused" => Some(Role::BorderFocused),
            "status" => Some(Role::Status),
            _ => None,
        }
    }
}

/// Resolved palette. Built once at startup from
/// [`ColorsConfig::overrides`]; looked up per-frame by the renderer.
#[derive(Debug, Clone)]
pub struct Theme {
    pub foreground: Color,
    pub file: Color,
    pub folder: Color,
    pub selection_bg: Color,
    pub border: Color,
    pub border_focused: Color,
    pub status: Color,
    /// Non-fatal parse warnings collected during construction. The
    /// App surfaces them in the status bar so a user with a typo'd
    /// hex colour sees the problem at startup rather than wondering
    /// why their override didn't take effect.
    pub warnings: Vec<String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            // These mirror the colours the renderer hard-coded
            // before Phase 11 — overriding nothing is bit-identical.
            foreground: Color::White,
            file: Color::White,
            folder: Color::Cyan,
            selection_bg: Color::DarkGray,
            border: Color::DarkGray,
            border_focused: Color::White,
            status: Color::White,
            warnings: Vec::new(),
        }
    }
}

impl Theme {
    /// Look up the colour for a role. Always returns *some* colour
    /// (a missing override falls through to the compiled default),
    /// so renderer code never has to handle absence.
    pub fn color(&self, role: Role) -> Color {
        match role {
            Role::Foreground => self.foreground,
            Role::File => self.file,
            Role::Folder => self.folder,
            Role::SelectionBg => self.selection_bg,
            Role::Border => self.border,
            Role::BorderFocused => self.border_focused,
            Role::Status => self.status,
        }
    }

    /// Build a theme by overlaying user overrides on the defaults.
    /// Unknown role names and malformed colour values are recorded
    /// in [`Self::warnings`] but never abort construction — a typo
    /// in `[colors]` shouldn't lock the user out of the App.
    pub fn from_config(cfg: &ColorsConfig) -> Self {
        let mut theme = Theme::default();
        for (raw_role, raw_value) in &cfg.overrides {
            let Some(role) = Role::from_config_name(raw_role) else {
                theme
                    .warnings
                    .push(format!("[colors] ignored unknown role {:?}", raw_role));
                continue;
            };
            match parse_color(raw_value) {
                Ok(c) => theme.apply(role, c),
                Err(e) => {
                    theme
                        .warnings
                        .push(format!("[colors].{} ignored: {}", role.config_name(), e));
                }
            }
        }
        theme
    }

    fn apply(&mut self, role: Role, c: Color) {
        match role {
            Role::Foreground => self.foreground = c,
            Role::File => self.file = c,
            Role::Folder => self.folder = c,
            Role::SelectionBg => self.selection_bg = c,
            Role::Border => self.border = c,
            Role::BorderFocused => self.border_focused = c,
            Role::Status => self.status = c,
        }
    }
}

/// Parse a `"#rrggbb"` or `"ansi:N"` string into a ratatui [`Color`].
/// Lenient on whitespace and case; strict on shape so typos surface
/// as warnings rather than the wrong colour.
pub fn parse_color(raw: &str) -> Result<Color, String> {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix('#') {
        return parse_hex(rest);
    }
    if let Some(rest) = s.strip_prefix("ansi:") {
        let n: u8 = rest
            .trim()
            .parse()
            .map_err(|_| format!("expected ansi index 0..=255, got {:?}", rest))?;
        return Ok(Color::Indexed(n));
    }
    Err(format!("expected \"#rrggbb\" or \"ansi:N\", got {:?}", raw))
}

fn parse_hex(raw: &str) -> Result<Color, String> {
    let s = raw.trim();
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("expected 6 hex digits, got {:?}", s));
    }
    let r = u8::from_str_radix(&s[0..2], 16).unwrap();
    let g = u8::from_str_radix(&s[2..4], 16).unwrap();
    let b = u8::from_str_radix(&s[4..6], 16).unwrap();
    Ok(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_color() {
        assert_eq!(parse_color("#ff8800").unwrap(), Color::Rgb(255, 136, 0));
        assert_eq!(parse_color(" #00FFAA ").unwrap(), Color::Rgb(0, 255, 170));
    }

    #[test]
    fn parses_ansi_index() {
        assert_eq!(parse_color("ansi:5").unwrap(), Color::Indexed(5));
        assert_eq!(parse_color("ansi: 200").unwrap(), Color::Indexed(200));
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_color("not a color").is_err());
        assert!(parse_color("#zzz").is_err());
        assert!(parse_color("#12345").is_err());
        assert!(parse_color("ansi:300").is_err());
    }

    #[test]
    fn defaults_match_legacy_palette() {
        let t = Theme::default();
        assert_eq!(t.color(Role::Foreground), Color::White);
        assert_eq!(t.color(Role::Folder), Color::Cyan);
        assert_eq!(t.color(Role::SelectionBg), Color::DarkGray);
    }

    #[test]
    fn from_config_overrides_named_roles() {
        let mut cfg = ColorsConfig::default();
        cfg.overrides.insert("folder".into(), "#3aa0ff".into());
        cfg.overrides.insert("file".into(), "ansi:7".into());
        let theme = Theme::from_config(&cfg);
        assert_eq!(theme.color(Role::Folder), Color::Rgb(58, 160, 255));
        assert_eq!(theme.color(Role::File), Color::Indexed(7));
        // Unaltered roles keep defaults.
        assert_eq!(theme.color(Role::SelectionBg), Color::DarkGray);
        assert!(theme.warnings.is_empty());
    }

    #[test]
    fn unknown_role_yields_warning_not_error() {
        let mut cfg = ColorsConfig::default();
        cfg.overrides.insert("teleport".into(), "#000000".into());
        let theme = Theme::from_config(&cfg);
        assert!(theme.warnings.iter().any(|w| w.contains("teleport")));
        // Defaults are otherwise intact.
        assert_eq!(theme.color(Role::Foreground), Color::White);
    }

    #[test]
    fn malformed_value_yields_warning_with_role_name() {
        let mut cfg = ColorsConfig::default();
        cfg.overrides.insert("folder".into(), "neon green".into());
        let theme = Theme::from_config(&cfg);
        assert!(theme.warnings.iter().any(|w| w.contains("[colors].folder")));
        // The role kept its default since the value didn't parse.
        assert_eq!(theme.color(Role::Folder), Color::Cyan);
    }
}

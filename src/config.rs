//! User configuration loaded from a TOML file.
//!
//! Layout follows the same shape used by sysdx (the user's other Rust
//! TUI tool): every field has a compiled-in default, the on-disk file is
//! treated as a *partial* override, and missing keys fall back without an
//! error. This means a brand-new install with no config still runs, and a
//! user can drop in just the few keys they care about.
//!
//! # File location
//!
//! Resolution order:
//!
//! 1. The path passed via `--config <PATH>` (handled by the CLI parser).
//! 2. `$XDG_CONFIG_HOME/radisk/config.toml` (typically
//!    `~/.config/radisk/config.toml` on Linux).
//! 3. The platform-appropriate config dir reported by [`directories`]
//!    on macOS / Windows.
//!
//! When the file does not exist, [`Config::default`] is used. When the
//! file exists but is malformed, [`Config::load_from_path`] returns an
//! error so the user sees the parse failure instead of silently getting
//! defaults.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level configuration. Every field has a default, so deserialising
/// an empty TOML file produces the same value as [`Config::default`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub display: DisplayConfig,
    pub scan: ScanConfigOverrides,
    pub keybinds: KeybindsConfig,
    pub colors: ColorsConfig,
}

/// Display / UI knobs.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayConfig {
    /// Number of concentric rings to render. Overridable via the `-d` CLI
    /// flag (which takes precedence over the file).
    pub ring_depth: usize,
    /// Sidebar width as a percentage of the terminal width. Clamped to
    /// `[10, 60]` at load time so a malformed value cannot break layout.
    pub sidebar_percent: u16,
}

/// Scanner overrides exposed to the user. Mirrors the relevant fields of
/// [`crate::scanner::ScanConfig`] but keeps that struct free of serde so
/// the scanner crate stays loadable from non-config callers (tests, the
/// reference walker, the example bench harness).
#[derive(Debug, Clone, PartialEq)]
pub struct ScanConfigOverrides {
    pub follow_symlinks: bool,
    /// Hard recursion ceiling. `None` is rejected at load time and folded
    /// into the compile-time [`crate::scanner::DEFAULT_MAX_DEPTH`].
    pub max_depth: usize,
    /// Use apparent file size (`metadata.len()`) instead of on-disk
    /// size (`st_blocks * 512`). Toggleable in-app with the
    /// `toggle_apparent_size` action.
    pub use_apparent_size: bool,
    /// Glob patterns whose matches are skipped during the walk.
    pub exclude: Vec<String>,
}

/// Keybind configuration.
///
/// Phase 3 keeps this struct minimal — actually rebinding keys lives in
/// the `keybinds` module — but exposing the struct here means the TOML
/// schema is stable from day one and the merge plumbing already covers
/// it. New fields can be added later without breaking existing files.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KeybindsConfig {
    /// Optional per-action overrides keyed by the action's canonical
    /// name (e.g. `"quit"`, `"navigate_up"`). Empty in the default
    /// config; populated from `[keybinds]` in TOML.
    pub overrides: std::collections::BTreeMap<String, String>,
}

/// Colour configuration.
///
/// Phase 3 stub: same rationale as [`KeybindsConfig`]. The `[colors]`
/// section in TOML is parsed and stored verbatim so a future commit can
/// turn it into a live theme without changing the file format.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ColorsConfig {
    /// Optional named-colour overrides keyed by role (e.g. `"file"`,
    /// `"folder"`). Values are accepted as `"#rrggbb"` hex or `"ansi:N"`
    /// indices; validation happens when the theme is built, not at load.
    pub overrides: std::collections::BTreeMap<String, String>,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            ring_depth: 5,
            sidebar_percent: 25,
        }
    }
}

impl Default for ScanConfigOverrides {
    fn default() -> Self {
        Self {
            follow_symlinks: false,
            max_depth: crate::scanner::DEFAULT_MAX_DEPTH,
            use_apparent_size: false,
            exclude: Vec::new(),
        }
    }
}

// --- Wire format (deserialisation) ----------------------------------------

#[derive(Debug, Default, Deserialize)]
struct PartialConfig {
    #[serde(default)]
    display: PartialDisplay,
    #[serde(default)]
    scan: PartialScan,
    #[serde(default)]
    keybinds: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    colors: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialDisplay {
    ring_depth: Option<usize>,
    sidebar_percent: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct PartialScan {
    follow_symlinks: Option<bool>,
    max_depth: Option<usize>,
    use_apparent_size: Option<bool>,
    exclude: Option<Vec<String>>,
}

impl PartialConfig {
    fn into_full(self) -> Config {
        let mut cfg = Config::default();
        if let Some(v) = self.display.ring_depth {
            cfg.display.ring_depth = v.max(1);
        }
        if let Some(v) = self.display.sidebar_percent {
            cfg.display.sidebar_percent = v.clamp(10, 60);
        }
        if let Some(v) = self.scan.follow_symlinks {
            cfg.scan.follow_symlinks = v;
        }
        if let Some(v) = self.scan.max_depth {
            cfg.scan.max_depth = v.max(1);
        }
        if let Some(v) = self.scan.use_apparent_size {
            cfg.scan.use_apparent_size = v;
        }
        if let Some(v) = self.scan.exclude {
            cfg.scan.exclude = v;
        }
        cfg.keybinds.overrides = self.keybinds;
        cfg.colors.overrides = self.colors;
        cfg
    }
}

// --- Loading --------------------------------------------------------------

impl Config {
    /// Load a config from `path`. Missing files yield [`Config::default`];
    /// malformed files yield a contextual error.
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let raw = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(err) => {
                return Err(err).with_context(|| format!("failed to read {}", path.display()));
            }
        };
        let partial: PartialConfig =
            toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))?;
        Ok(partial.into_full())
    }

    /// Resolve the default config path for this platform without reading
    /// it. Returns `None` when no home directory can be determined.
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("", "", "radisk").map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Load from [`Config::default_path`] if it exists, otherwise return
    /// defaults. Convenience wrapper for `main`.
    pub fn load_default() -> Result<Self> {
        match Self::default_path() {
            Some(path) => Self::load_from_path(&path),
            None => Ok(Self::default()),
        }
    }

    /// Convert the user-visible scan overrides into the scanner's own
    /// [`crate::scanner::ScanConfig`]. Lives here (not in
    /// `ScanConfigOverrides`) so the scanner crate doesn't have to know
    /// about the config layer.
    pub fn to_scan_config(&self) -> crate::scanner::ScanConfig {
        crate::scanner::ScanConfig {
            follow_symlinks: self.scan.follow_symlinks,
            max_depth: Some(self.scan.max_depth),
            use_apparent_size: self.scan.use_apparent_size,
            exclude: self.scan.exclude.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn missing_file_yields_defaults() {
        let path = std::path::PathBuf::from("/this/path/definitely/does/not/exist/radisk.toml");
        let cfg = Config::load_from_path(&path).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn empty_file_yields_defaults() {
        let f = write_temp("");
        let cfg = Config::load_from_path(f.path()).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn partial_file_only_overrides_named_keys() {
        let f = write_temp(
            r#"
            [display]
            ring_depth = 8

            [scan]
            follow_symlinks = true
            "#,
        );
        let cfg = Config::load_from_path(f.path()).unwrap();
        // overridden
        assert_eq!(cfg.display.ring_depth, 8);
        assert!(cfg.scan.follow_symlinks);
        // unchanged from defaults
        assert_eq!(
            cfg.display.sidebar_percent,
            DisplayConfig::default().sidebar_percent
        );
        assert_eq!(cfg.scan.max_depth, ScanConfigOverrides::default().max_depth);
    }

    #[test]
    fn malformed_file_returns_error_with_path_in_message() {
        let f = write_temp("this is not = valid = toml = at all");
        let err = Config::load_from_path(f.path()).expect_err("should fail");
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("failed to parse"),
            "missing parse-failure context: {}",
            msg
        );
    }

    #[test]
    fn sidebar_percent_is_clamped() {
        let f = write_temp(
            r#"
            [display]
            sidebar_percent = 99
            "#,
        );
        let cfg = Config::load_from_path(f.path()).unwrap();
        assert!(
            (10..=60).contains(&cfg.display.sidebar_percent),
            "sidebar_percent must be clamped, got {}",
            cfg.display.sidebar_percent
        );
    }

    #[test]
    fn ring_depth_zero_is_promoted_to_one() {
        let f = write_temp(
            r#"
            [display]
            ring_depth = 0
            "#,
        );
        let cfg = Config::load_from_path(f.path()).unwrap();
        assert_eq!(cfg.display.ring_depth, 1);
    }

    #[test]
    fn keybinds_and_colors_pass_through_verbatim() {
        let f = write_temp(
            r##"
            [keybinds]
            quit = "ctrl+q"
            navigate_up = "u"

            [colors]
            file = "#3aa"
            folder = "ansi:5"
            "##,
        );
        let cfg = Config::load_from_path(f.path()).unwrap();
        assert_eq!(
            cfg.keybinds.overrides.get("quit").map(String::as_str),
            Some("ctrl+q")
        );
        assert_eq!(
            cfg.colors.overrides.get("folder").map(String::as_str),
            Some("ansi:5")
        );
    }

    #[test]
    fn to_scan_config_threads_overrides_through() {
        let mut cfg = Config::default();
        cfg.scan.follow_symlinks = true;
        cfg.scan.max_depth = 12;
        let scan = cfg.to_scan_config();
        assert!(scan.follow_symlinks);
        assert_eq!(scan.max_depth, Some(12));
    }
}

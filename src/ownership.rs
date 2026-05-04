//! Cross-ecosystem package-ownership lookup.
//!
//! Inspired by Revo Uninstaller's "leftovers detection" — answers
//! "who put this file here, and is the source even installed any
//! more?" for the row under the cursor. radisk's value-add over a
//! plain disk-usage tool is that it can distinguish:
//!
//! - System packages (pacman / AUR / dpkg / rpm / apk) via the
//!   distro's own ownership query.
//! - Userspace ecosystems (npm / pip / uv / cargo / flatpak / snap)
//!   via path-pattern recognition — `node_modules/foo/bar` is
//!   *obviously* owned by the npm package `foo`, no shell-out
//!   needed.
//! - The orphan case (no manager claims this path).
//!
//! ## Lookup order
//!
//! 1. **Userspace patterns first.** They're free (no fork/exec)
//!    and they're more specific — `node_modules/foo` is npm
//!    regardless of the distro. If a path matches, we're done.
//! 2. **System package manager second.** One fork+exec per
//!    distinct path, cached for the lifetime of the App in
//!    `App.owner_cache`. We probe `$PATH` once per process to
//!    decide which manager to query (pacman → dpkg → rpm → apk,
//!    in that order).
//!
//! ## Why this design
//!
//! A naive "shell out to every PM" approach would fork five
//! processes per lookup and still miss npm/pip files that live
//! at user-controlled paths. Path-pattern detection covers the
//! interesting userspace cases at zero cost; the system query is
//! reserved for the genuine "this is in /usr, who put it there?"
//! case.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

/// Result of an ownership lookup. Stays small and `Clone` so the
/// App can cache it cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// File belongs to a system package installed via the distro
    /// package manager (pacman / dpkg / rpm / apk / …).
    SystemPackage {
        /// Display name of the manager — `"pacman"`, `"dpkg"`, etc.
        manager: &'static str,
        /// Source the package was installed from. Distinguishes
        /// AUR builds from official-repo packages on Arch.
        source: PackageSource,
        name: String,
        version: String,
    },
    /// File belongs to a userspace ecosystem inferred from the
    /// path itself — npm, pip/uv, cargo, flatpak, snap, …
    Userspace {
        ecosystem: &'static str,
        name: String,
    },
    /// No detector claims this path.
    None,
    /// No system package manager is installed on this host. Cached
    /// so we don't keep re-probing.
    NoSystemManager,
}

/// Where a system package was installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageSource {
    /// Official distribution repo.
    Repo,
    /// Arch User Repository (AUR) — build-from-source community
    /// recipes. We detect these with `pacman -Qm`.
    Aur,
    /// Anything else we can't classify (PPA, Copr, manually
    /// `--installed` packages, etc.).
    #[allow(dead_code)]
    Other,
}

impl Owner {
    /// Short human label for the status bar.
    pub fn label(&self) -> String {
        match self {
            Owner::SystemPackage {
                manager,
                source,
                name,
                version,
            } => {
                let src_tag = match source {
                    PackageSource::Repo => "",
                    PackageSource::Aur => " (aur)",
                    PackageSource::Other => " (other)",
                };
                format!("{}: {} {}{}", manager, name, version, src_tag)
            }
            Owner::Userspace { ecosystem, name } => format!("{}: {}", ecosystem, name),
            Owner::None => "(no package owns this path)".to_string(),
            Owner::NoSystemManager => "no system package manager available".to_string(),
        }
    }
}

// ─── Public entrypoint ────────────────────────────────────────────────────

/// Look up the owner of `path`. Cheap path-pattern detectors run
/// first; the system package manager is consulted only when no
/// userspace ecosystem claims the path.
pub fn query(path: &Path) -> Owner {
    if let Some(u) = detect_userspace(path) {
        return u;
    }
    detect_system(path)
}

// ─── Userspace path-pattern detection ─────────────────────────────────────

fn detect_userspace(path: &Path) -> Option<Owner> {
    let s = path.to_string_lossy();
    if let Some(name) = detect_npm(&s) {
        return Some(Owner::Userspace {
            ecosystem: "npm",
            name,
        });
    }
    if let Some(name) = detect_python_site_packages(&s) {
        return Some(Owner::Userspace {
            ecosystem: "pip/uv",
            name,
        });
    }
    if let Some(name) = detect_cargo_registry(&s) {
        return Some(Owner::Userspace {
            ecosystem: "cargo",
            name,
        });
    }
    if let Some(name) = detect_flatpak(&s) {
        return Some(Owner::Userspace {
            ecosystem: "flatpak",
            name,
        });
    }
    if let Some(name) = detect_snap(&s) {
        return Some(Owner::Userspace {
            ecosystem: "snap",
            name,
        });
    }
    None
}

/// `node_modules/<pkg>/...` or `node_modules/@scope/<pkg>/...`
pub fn detect_npm(path: &str) -> Option<String> {
    let idx = path.find("/node_modules/")?;
    let after = &path[idx + "/node_modules/".len()..];
    let mut iter = after.split('/');
    let first = iter.next()?;
    if first.is_empty() {
        return None;
    }
    if let Some(stripped) = first.strip_prefix('@') {
        let second = iter.next()?;
        if second.is_empty() {
            return None;
        }
        return Some(format!("@{}/{}", stripped, second));
    }
    Some(first.to_string())
}

/// `**/site-packages/<pkg>/...`,
/// `**/site-packages/<pkg>-<ver>.dist-info/...`,
/// or `**/site-packages/<pkg>-<ver>.egg-info/...`
pub fn detect_python_site_packages(path: &str) -> Option<String> {
    let idx = path.find("/site-packages/")?;
    let after = &path[idx + "/site-packages/".len()..];
    let first = after.split('/').next()?;
    if first.is_empty() {
        return None;
    }
    for suffix in [".dist-info", ".egg-info"] {
        if let Some(stem) = first.strip_suffix(suffix) {
            return Some(
                stem.rsplit_once('-')
                    .map(|(n, _)| n)
                    .unwrap_or(stem)
                    .to_string(),
            );
        }
    }
    Some(first.to_string())
}

/// `~/.cargo/registry/src/<index>/<pkg>-<ver>/...`
pub fn detect_cargo_registry(path: &str) -> Option<String> {
    let idx = path.find("/.cargo/registry/src/")?;
    let after = &path[idx + "/.cargo/registry/src/".len()..];
    let mut iter = after.splitn(3, '/');
    let _index = iter.next()?;
    let pkg_dir = iter.next()?;
    Some(
        pkg_dir
            .rsplit_once('-')
            .filter(|(_, ver)| ver.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(|(n, _)| n)
            .unwrap_or(pkg_dir)
            .to_string(),
    )
}

/// `/var/lib/flatpak/app/<id>/...` or
/// `~/.local/share/flatpak/app/<id>/...`
pub fn detect_flatpak(path: &str) -> Option<String> {
    for prefix in ["/var/lib/flatpak/app/", "/share/flatpak/app/"] {
        if let Some(idx) = path.find(prefix) {
            let after = &path[idx + prefix.len()..];
            let first = after.split('/').next()?;
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    None
}

/// `/snap/<pkg>/...`, `~/snap/<pkg>/...`,
/// `/var/lib/snapd/snaps/<pkg>_*.snap`
pub fn detect_snap(path: &str) -> Option<String> {
    for prefix in ["/snap/", "/var/lib/snapd/snaps/"] {
        if let Some(idx) = path.find(prefix) {
            let after = &path[idx + prefix.len()..];
            let first = after.split('/').next()?;
            if first.is_empty() {
                continue;
            }
            let stem = first.split('_').next().unwrap_or(first);
            return Some(stem.trim_end_matches(".snap").to_string());
        }
    }
    None
}

// ─── System package manager detection ─────────────────────────────────────

fn system_manager() -> Option<&'static str> {
    static MGR: OnceLock<Option<&'static str>> = OnceLock::new();
    *MGR.get_or_init(|| {
        ["pacman", "dpkg", "rpm", "apk"]
            .into_iter()
            .find(|c| which(c))
    })
}

fn which(cmd: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        if dir.join(cmd).is_file() {
            return true;
        }
    }
    false
}

fn detect_system(path: &Path) -> Owner {
    match system_manager() {
        Some("pacman") => query_pacman(path),
        Some("dpkg") => query_dpkg(path),
        Some("rpm") => query_rpm(path),
        Some("apk") => query_apk(path),
        Some(_) | None => Owner::NoSystemManager,
    }
}

fn run_capture(cmd: &str, args: &[&OsStr]) -> Option<String> {
    let mut command = Command::new(cmd);
    for a in args {
        command.arg(a);
    }
    let out = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ─── Pacman backend ───────────────────────────────────────────────────────

fn aur_packages() -> &'static std::collections::HashSet<String> {
    static SET: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    SET.get_or_init(|| {
        let mut set = std::collections::HashSet::new();
        if let Some(out) = run_capture("pacman", &[OsStr::new("-Qm")]) {
            for line in out.lines() {
                if let Some((name, _)) = line.split_once(' ') {
                    set.insert(name.to_string());
                }
            }
        }
        set
    })
}

fn query_pacman(path: &Path) -> Owner {
    let Some(out) = run_capture("pacman", &[OsStr::new("-Qo"), path.as_os_str()]) else {
        return Owner::None;
    };
    let Some(line) = out.lines().next() else {
        return Owner::None;
    };
    parse_pacman_line(line)
        .map(|(name, version)| {
            let source = if aur_packages().contains(&name) {
                PackageSource::Aur
            } else {
                PackageSource::Repo
            };
            Owner::SystemPackage {
                manager: "pacman",
                source,
                name,
                version,
            }
        })
        .unwrap_or(Owner::None)
}

/// `/usr/bin/ls is owned by coreutils 9.7-1`
pub fn parse_pacman_line(line: &str) -> Option<(String, String)> {
    let (_, after) = line.trim().split_once(" is owned by ")?;
    let mut parts = after.split_whitespace();
    let name = parts.next()?.to_string();
    let version = parts.next().unwrap_or("?").to_string();
    Some((name, version))
}

// ─── dpkg backend (Debian / Ubuntu) ───────────────────────────────────────

fn query_dpkg(path: &Path) -> Owner {
    let Some(out) = run_capture("dpkg", &[OsStr::new("-S"), path.as_os_str()]) else {
        return Owner::None;
    };
    let Some(line) = out.lines().next() else {
        return Owner::None;
    };
    parse_dpkg_line(line)
        .map(|name| {
            let version = run_capture(
                "dpkg-query",
                &[
                    OsStr::new("-W"),
                    OsStr::new("-f=${Version}"),
                    OsStr::new(&name),
                ],
            )
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| "?".to_string());
            Owner::SystemPackage {
                manager: "dpkg",
                source: PackageSource::Repo,
                name,
                version,
            }
        })
        .unwrap_or(Owner::None)
}

/// `coreutils: /usr/bin/ls` — possibly diversion-prefixed.
pub fn parse_dpkg_line(line: &str) -> Option<String> {
    let (name, _) = line.split_once(':')?;
    Some(name.trim().to_string())
}

// ─── rpm backend (Fedora / RHEL / openSUSE) ───────────────────────────────

fn query_rpm(path: &Path) -> Owner {
    let Some(out) = run_capture(
        "rpm",
        &[
            OsStr::new("-qf"),
            OsStr::new("--queryformat"),
            OsStr::new("%{NAME} %{VERSION}-%{RELEASE}\\n"),
            path.as_os_str(),
        ],
    ) else {
        return Owner::None;
    };
    let Some(line) = out.lines().next() else {
        return Owner::None;
    };
    parse_rpm_line(line)
        .map(|(name, version)| Owner::SystemPackage {
            manager: "rpm",
            source: PackageSource::Repo,
            name,
            version,
        })
        .unwrap_or(Owner::None)
}

pub fn parse_rpm_line(line: &str) -> Option<(String, String)> {
    let mut parts = line.split_whitespace();
    let name = parts.next()?.to_string();
    let version = parts.next().unwrap_or("?").to_string();
    Some((name, version))
}

// ─── apk backend (Alpine) ─────────────────────────────────────────────────

fn query_apk(path: &Path) -> Owner {
    let Some(out) = run_capture(
        "apk",
        &[
            OsStr::new("info"),
            OsStr::new("--who-owns"),
            path.as_os_str(),
        ],
    ) else {
        return Owner::None;
    };
    let Some(line) = out.lines().next() else {
        return Owner::None;
    };
    parse_apk_line(line)
        .map(|(name, version)| Owner::SystemPackage {
            manager: "apk",
            source: PackageSource::Repo,
            name,
            version,
        })
        .unwrap_or(Owner::None)
}

/// `/usr/bin/ls is owned by busybox-1.36.1-r0`
pub fn parse_apk_line(line: &str) -> Option<(String, String)> {
    let (_, after) = line.trim().split_once(" is owned by ")?;
    let pkg = after.trim();
    if let Some((name, version)) = split_apk_pkg(pkg) {
        return Some((name, version));
    }
    Some((pkg.to_string(), "?".to_string()))
}

/// Split `name-X.Y-rN` into `(name, X.Y-rN)` by finding the
/// rightmost `-` followed by a digit.
fn split_apk_pkg(s: &str) -> Option<(String, String)> {
    let mut last = None;
    for (i, _) in s.match_indices('-') {
        if let Some(c) = s[i + 1..].chars().next() {
            if c.is_ascii_digit() {
                last = Some(i);
            }
        }
    }
    let i = last?;
    Some((s[..i].to_string(), s[i + 1..].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Userspace detectors ───────────────────────────────────────────

    #[test]
    fn npm_simple() {
        assert_eq!(
            detect_npm("/home/me/proj/node_modules/lodash/index.js"),
            Some("lodash".to_string())
        );
    }

    #[test]
    fn npm_scoped() {
        assert_eq!(
            detect_npm("/proj/node_modules/@types/node/index.d.ts"),
            Some("@types/node".to_string())
        );
    }

    #[test]
    fn npm_no_match() {
        assert_eq!(detect_npm("/usr/bin/ls"), None);
    }

    #[test]
    fn site_packages_plain() {
        assert_eq!(
            detect_python_site_packages(
                "/home/me/.venv/lib/python3.12/site-packages/requests/__init__.py"
            ),
            Some("requests".to_string())
        );
    }

    #[test]
    fn site_packages_dist_info_strips_version() {
        assert_eq!(
            detect_python_site_packages(
                "/usr/lib/python3.12/site-packages/requests-2.31.0.dist-info/METADATA"
            ),
            Some("requests".to_string())
        );
    }

    #[test]
    fn cargo_registry_strips_version() {
        assert_eq!(
            detect_cargo_registry(
                "/home/me/.cargo/registry/src/index.crates.io-1cd66030c0c0e6e8/serde-1.0.219/src/lib.rs"
            ),
            Some("serde".to_string())
        );
    }

    #[test]
    fn flatpak_user_install() {
        assert_eq!(
            detect_flatpak(
                "/home/me/.local/share/flatpak/app/org.mozilla.firefox/x86_64/stable/active/files"
            ),
            Some("org.mozilla.firefox".to_string())
        );
    }

    #[test]
    fn flatpak_system_install() {
        assert_eq!(
            detect_flatpak("/var/lib/flatpak/app/com.discordapp.Discord/x86_64/stable"),
            Some("com.discordapp.Discord".to_string())
        );
    }

    #[test]
    fn snap_classic_path() {
        assert_eq!(
            detect_snap("/snap/firefox/current/firefox"),
            Some("firefox".to_string())
        );
    }

    // ── Backend parsers ────────────────────────────────────────────────

    #[test]
    fn parses_pacman_line_pkg() {
        assert_eq!(
            parse_pacman_line("/usr/bin/ls is owned by coreutils 9.7-1"),
            Some(("coreutils".to_string(), "9.7-1".to_string()))
        );
    }

    #[test]
    fn parses_dpkg_line_pkg() {
        assert_eq!(
            parse_dpkg_line("coreutils: /usr/bin/ls"),
            Some("coreutils".to_string())
        );
    }

    #[test]
    fn parses_rpm_line_pkg() {
        assert_eq!(
            parse_rpm_line("coreutils 9.4-7.fc40"),
            Some(("coreutils".to_string(), "9.4-7.fc40".to_string()))
        );
    }

    #[test]
    fn parses_apk_line_pkg() {
        assert_eq!(
            parse_apk_line("/usr/bin/ls is owned by busybox-1.36.1-r0"),
            Some(("busybox".to_string(), "1.36.1-r0".to_string()))
        );
    }

    // ── Owner labels ───────────────────────────────────────────────────

    #[test]
    fn aur_package_has_aur_tag_in_label() {
        let o = Owner::SystemPackage {
            manager: "pacman",
            source: PackageSource::Aur,
            name: "yay".into(),
            version: "12.5.0-2".into(),
        };
        assert!(o.label().contains("(aur)"));
    }

    #[test]
    fn userspace_label_includes_ecosystem() {
        let o = Owner::Userspace {
            ecosystem: "npm",
            name: "lodash".into(),
        };
        assert_eq!(o.label(), "npm: lodash");
    }
}

//! Filesystem deletion with trash-cli fallthrough.
//!
//! The legacy delete path called `std::fs::remove_file` /
//! `remove_dir_all` directly, which is irreversible. Most Linux desktops
//! ship a [trash-cli][1] (`trash-put`) or gvfs (`gio trash`) helper
//! that moves the entry into the user's `~/.local/share/Trash/` so the
//! file can be undone. radisk now prefers those when available and
//! falls back to a permanent `rm` when neither is present.
//!
//! [1]: https://github.com/andreafrancia/trash-cli
//!
//! The detection runs once per `App` invocation, so the cost is one
//! `which`-shaped lookup at startup, not per delete.

use anyhow::{anyhow, Context, Result};
use std::path::Path;
use std::process::{Command, Stdio};

/// Strategy chosen for the current process. Computed once via
/// [`Self::detect`] and reused for every delete the user issues. Stays
/// public so the App can show "Trash: trash-put" in the help screen
/// or status bar later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteStrategy {
    /// `trash-put` from the trash-cli package.
    TrashPut,
    /// `gio trash` from glib's gvfs tooling.
    GioTrash,
    /// No trash helper available — falls back to a permanent
    /// `std::fs::remove_*` call. The UI surfaces this so the user
    /// knows the action is irreversible.
    Permanent,
}

impl DeleteStrategy {
    /// Probe `$PATH` and pick the best available strategy. Order:
    /// `trash-put` → `gio trash` → permanent.
    pub fn detect() -> Self {
        if which_in_path("trash-put") {
            DeleteStrategy::TrashPut
        } else if which_in_path("gio") {
            DeleteStrategy::GioTrash
        } else {
            DeleteStrategy::Permanent
        }
    }

    /// Human-readable label for the status bar / help screen.
    pub fn label(&self) -> &'static str {
        match self {
            DeleteStrategy::TrashPut => "trash-put",
            DeleteStrategy::GioTrash => "gio trash",
            DeleteStrategy::Permanent => "permanent (no trash helper)",
        }
    }

    /// `true` if deletes via this strategy are recoverable. Used by
    /// the help screen and the confirmation dialog to warn users
    /// when they are about to perform a permanent removal.
    #[allow(dead_code)]
    pub fn is_recoverable(&self) -> bool {
        matches!(self, DeleteStrategy::TrashPut | DeleteStrategy::GioTrash)
    }
}

/// Delete `path` using the chosen strategy. Validates that the path
/// still exists *and* (on Unix) that its inode matches what we
/// expected, closing the small TOCTOU window between the user
/// confirming the dialog and the actual `unlink` call. If the user
/// has not asked for an inode check, pass `expected_inode = None`.
pub fn delete(strategy: &DeleteStrategy, path: &Path, expected_inode: Option<u64>) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat {} before delete", path.display()))?;

    #[cfg(unix)]
    if let Some(expected) = expected_inode {
        use std::os::unix::fs::MetadataExt;
        let actual = metadata.ino();
        if actual != expected {
            return Err(anyhow!(
                "{} changed identity since selection (inode {} -> {}); refusing to delete",
                path.display(),
                expected,
                actual
            ));
        }
    }
    // Suppress an unused-warning on non-unix builds; the parameter is
    // still part of the public signature so callers compile uniformly.
    #[cfg(not(unix))]
    let _ = expected_inode;

    let is_dir = metadata.file_type().is_dir();

    match strategy {
        DeleteStrategy::TrashPut => run_helper("trash-put", &[path]),
        DeleteStrategy::GioTrash => run_helper("gio", &[Path::new("trash"), path]),
        DeleteStrategy::Permanent => {
            if is_dir {
                std::fs::remove_dir_all(path)
                    .with_context(|| format!("failed to remove directory {}", path.display()))
            } else {
                std::fs::remove_file(path)
                    .with_context(|| format!("failed to remove file {}", path.display()))
            }
        }
    }
}

/// Invoke `cmd` with `args`, returning a contextful error if it
/// exits non-zero or fails to spawn.
fn run_helper(cmd: &str, args: &[&Path]) -> Result<()> {
    let mut command = Command::new(cmd);
    for a in args {
        command.arg(a);
    }
    let output = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("failed to invoke `{}`", cmd))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(anyhow!(
        "`{}` exited with {}: {}",
        cmd,
        output.status,
        stderr.trim()
    ))
}

/// Best-effort `which`. Walks `$PATH` and returns true on the first
/// executable hit. Avoids pulling in the `which` crate for this one
/// startup-time call.
fn which_in_path(name: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(name);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if meta.permissions().mode() & 0o111 != 0 {
                        return true;
                    }
                }
                #[cfg(not(unix))]
                {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::prelude::*;
    use assert_fs::TempDir;

    #[test]
    fn detect_returns_a_strategy() {
        // Whatever the test environment looks like, detection must
        // always return *something*.
        let s = DeleteStrategy::detect();
        let _ = s.label();
        let _ = s.is_recoverable();
    }

    #[test]
    fn permanent_strategy_removes_a_file() {
        let temp = TempDir::new().unwrap();
        let f = temp.child("doomed.txt");
        f.write_str("bye").unwrap();
        assert!(f.path().exists());

        delete(&DeleteStrategy::Permanent, f.path(), None).unwrap();
        assert!(!f.path().exists());
    }

    #[test]
    fn permanent_strategy_removes_a_directory_recursively() {
        let temp = TempDir::new().unwrap();
        temp.child("dir/inner.txt").write_str("a").unwrap();
        let dir = temp.child("dir");
        assert!(dir.path().exists());

        delete(&DeleteStrategy::Permanent, dir.path(), None).unwrap();
        assert!(!dir.path().exists());
    }

    #[test]
    fn missing_path_yields_contextful_error() {
        let temp = TempDir::new().unwrap();
        let path = temp.child("never-existed.txt");
        let err = delete(&DeleteStrategy::Permanent, path.path(), None).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("failed to stat"),
            "missing pre-stat context: {}",
            msg
        );
    }

    #[cfg(unix)]
    #[test]
    fn inode_mismatch_blocks_the_delete() {
        let temp = TempDir::new().unwrap();
        let f = temp.child("guarded.txt");
        f.write_str("safe").unwrap();
        // Pass an obviously-wrong inode.
        let err = delete(&DeleteStrategy::Permanent, f.path(), Some(0)).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("changed identity"),
            "missing TOCTOU guard message: {}",
            msg
        );
        // The file must still exist after the refusal.
        assert!(f.path().exists());
    }

    #[test]
    fn label_and_is_recoverable_match_variant() {
        assert_eq!(DeleteStrategy::TrashPut.label(), "trash-put");
        assert!(DeleteStrategy::TrashPut.is_recoverable());
        assert!(DeleteStrategy::GioTrash.is_recoverable());
        assert!(!DeleteStrategy::Permanent.is_recoverable());
    }
}

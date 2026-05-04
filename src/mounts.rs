//! Discover mounted filesystems for the partition-style picker.
//!
//! Inspired by tools like EaseUS Partition Master / MiniTool / NIUBI /
//! AOMEI: before drilling into a single tree, give the user a view of
//! the *whole disk landscape* so they can pick which mount to scan.
//!
//! ## Implementation
//!
//! On Linux we parse `/proc/mounts` (which the kernel keeps fresh) for
//! the device / mount-point / fstype tuples, then call `statvfs` on
//! each mount to get total / free / used in bytes. Pseudo filesystems
//! (`proc`, `sysfs`, `cgroup`, `devtmpfs`, …) are filtered out — they
//! are not interesting for disk-usage analysis and showing them in the
//! picker would just be noise.
//!
//! On non-Linux platforms the discovery returns an empty list. The
//! `--mounts` picker degrades gracefully ("no mountpoints discovered")
//! rather than refusing to start.

use std::path::PathBuf;

/// One row in the mount picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    /// Device node (`/dev/sda1`, `tmpfs`, …).
    pub device: String,
    /// Where the device is mounted.
    pub mount_point: PathBuf,
    /// Filesystem type as reported by the kernel (`ext4`, `btrfs`,
    /// `nfs`, `tmpfs`, …).
    pub fstype: String,
    /// Total bytes on the filesystem (`f_blocks * f_frsize`).
    pub total: u64,
    /// Used bytes (`(f_blocks - f_bfree) * f_frsize`).
    pub used: u64,
    /// Bytes available to non-root processes (`f_bavail * f_frsize`).
    pub free: u64,
}

impl MountInfo {
    /// Used fraction in `[0.0, 1.0]`. Returns 0.0 on empty filesystems
    /// so the picker's progress bar never tries to divide by zero.
    pub fn used_fraction(&self) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        (self.used as f64 / self.total as f64) as f32
    }
}

/// Filesystem types we never want to show in the picker — mostly
/// kernel pseudo-FS and overlays that don't represent storage. Match
/// is exact, lowercase, against the fstype string from `/proc/mounts`.
const PSEUDO_FSTYPES: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "ramfs",
    "mqueue",
    "hugetlbfs",
    "pstore",
    "bpf",
    "tracefs",
    "debugfs",
    "securityfs",
    "configfs",
    "fusectl",
    "binfmt_misc",
    "autofs",
    "rpc_pipefs",
    "nfsd",
    "fuse.gvfsd-fuse",
    "fuse.portal",
    "overlay",
    "squashfs",
    "efivarfs",
    "selinuxfs",
];

/// Discover *pseudo* mount points — `/proc`, `/sys`, `/dev`,
/// `cgroup`, etc. — so the walker can skip them.
///
/// Phase 27 wires this into [`crate::walker`] to fix the
/// `/proc/kcore reports 128 TB` bug: kcore is the kernel's
/// virtual-memory image, exposed as a file in `/proc`. Its
/// "size" is the kernel's address-space width, not real
/// storage. Walking *any* file under a pseudo-FS gives
/// nonsense numbers in a disk-usage tool.
///
/// On non-Linux platforms `/proc/mounts` doesn't exist; the
/// hard-coded set is the safety net.
pub fn pseudo_mount_points() -> std::collections::HashSet<PathBuf> {
    let mut out: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    #[cfg(target_os = "linux")]
    {
        if let Ok(text) = std::fs::read_to_string("/proc/mounts") {
            for line in text.lines() {
                let fields: Vec<&str> = line.split_whitespace().collect();
                if fields.len() < 6 {
                    continue;
                }
                let fstype = fields[2];
                if PSEUDO_FSTYPES
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(fstype))
                {
                    out.insert(decode_mount_path(fields[1]));
                }
            }
        }
    }
    // Belt and braces on every platform: ensure the canonical
    // pseudo dirs are skipped even if /proc/mounts was empty or
    // unreadable.
    for p in ["/proc", "/sys", "/dev", "/run"] {
        out.insert(PathBuf::from(p));
    }
    out
}

/// Discover all "real" mount points on the current host. On Linux
/// this parses `/proc/mounts`; everything else returns an empty list.
pub fn discover() -> Vec<MountInfo> {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string("/proc/mounts") {
            Ok(s) => parse_proc_mounts(&s),
            Err(_) => Vec::new(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// Parse the contents of `/proc/mounts` and statvfs each non-pseudo
/// entry. Public for testing — the discovery wrapper feeds it a
/// `read_to_string` of the real file.
pub fn parse_proc_mounts(text: &str) -> Vec<MountInfo> {
    let mut out = Vec::new();
    let mut seen_mount_points = std::collections::HashSet::new();

    for line in text.lines() {
        // /proc/mounts: device mount-point fstype options dump-freq pass
        // We require all six fields to be present so a stray sentence
        // can never be mis-parsed as a row.
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 6 {
            continue;
        }
        let device = fields[0].to_string();
        let mount_point = decode_mount_path(fields[1]);
        let fstype = fields[2].to_string();

        // Mount points must be absolute Unix paths. Use a literal '/'
        // prefix check rather than `Path::is_absolute()`, which is
        // OS-aware and returns false on Windows for `/foo` — that
        // breaks parsing /proc/mounts samples in cross-platform tests.
        if !fields[1].starts_with('/') {
            continue;
        }
        if PSEUDO_FSTYPES
            .iter()
            .any(|p| p.eq_ignore_ascii_case(&fstype))
        {
            continue;
        }
        // Skip overlay / bind-mount duplicates pointing at the same place.
        if !seen_mount_points.insert(mount_point.clone()) {
            continue;
        }

        let (total, free) = statvfs_bytes(&mount_point).unwrap_or((0, 0));
        let used = total.saturating_sub(free);

        out.push(MountInfo {
            device,
            mount_point,
            fstype,
            total,
            used,
            free,
        });
    }

    // Sort by used bytes desc — fullest first, like a partition tool's
    // "look at this, this is the disk that needs attention" view.
    out.sort_by_key(|m| std::cmp::Reverse(m.used));
    out
}

/// Call `statvfs(2)` and return `(total, free)` in bytes, or `None`
/// if the syscall fails (e.g. on a stale NFS mount).
fn statvfs_bytes(path: &std::path::Path) -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        match nix::sys::statvfs::statvfs(path) {
            Ok(stat) => {
                let frsize = stat.fragment_size();
                let total = stat.blocks() * frsize;
                let free = stat.blocks_available() * frsize;
                Some((total, free))
            }
            Err(_) => None,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        None
    }
}

/// `/proc/mounts` encodes whitespace, tabs, backslashes, and newlines
/// in the mount-point column with octal escapes (`\040` for space,
/// `\011` for tab, …). Decode them back to their literal characters.
fn decode_mount_path(raw: &str) -> PathBuf {
    let mut out = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 3 < bytes.len() {
            let octal = std::str::from_utf8(&bytes[i + 1..i + 4]).ok();
            if let Some(s) = octal {
                if let Ok(n) = u32::from_str_radix(s, 8) {
                    if let Some(c) = char::from_u32(n) {
                        out.push(c);
                        i += 4;
                        continue;
                    }
                }
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    PathBuf::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_realistic_proc_mounts() {
        // A trimmed sample mirroring real /proc/mounts output.
        let sample = "\
proc /proc proc rw,nosuid,nodev,noexec,relatime 0 0
/dev/sda1 / ext4 rw,relatime 0 0
/dev/sda2 /home ext4 rw,relatime 0 0
tmpfs /tmp tmpfs rw,nosuid,nodev 0 0
/dev/sdb1 /mnt/data btrfs rw,relatime 0 0
sysfs /sys sysfs rw,nosuid,nodev,noexec,relatime 0 0
cgroup2 /sys/fs/cgroup cgroup2 rw,nosuid,nodev,noexec,relatime 0 0
";
        let mounts = parse_proc_mounts(sample);

        // Pseudo-FS entries must not appear.
        for m in &mounts {
            assert!(
                !PSEUDO_FSTYPES
                    .iter()
                    .any(|p| p.eq_ignore_ascii_case(&m.fstype)),
                "pseudo-fs {:?} should have been filtered",
                m.fstype
            );
        }

        // The three real mounts should be present (ext4 x2 + btrfs).
        let has = |p: &str| {
            mounts
                .iter()
                .any(|m| m.mount_point.as_path() == std::path::Path::new(p))
        };
        assert!(has("/"));
        assert!(has("/home"));
        assert!(has("/mnt/data"));
        // tmpfs at /tmp is correctly filtered (tmpfs is in the
        // pseudo list — even though it backs real RAM data, it does
        // not represent persistent storage and isn't useful in the
        // picker).
        assert!(!has("/tmp"));
    }

    #[test]
    fn duplicate_mount_points_are_deduped() {
        let sample = "\
/dev/sda1 / ext4 rw 0 0
/dev/sda1 / ext4 ro 0 0
";
        assert_eq!(parse_proc_mounts(sample).len(), 1);
    }

    #[test]
    fn octal_escapes_in_mount_points_decode() {
        // The classic case: a mount point with a space.
        let sample = "/dev/sdc1 /mnt/My\\040Disk ext4 rw 0 0\n";
        let mounts = parse_proc_mounts(sample);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0].mount_point, PathBuf::from("/mnt/My Disk"));
    }

    #[test]
    fn malformed_lines_are_skipped_silently() {
        let sample = "\
/dev/sda1 / ext4 rw 0 0
this line has too few fields
another bad entry
/dev/sdb1 /home ext4 rw 0 0
";
        let mounts = parse_proc_mounts(sample);
        assert_eq!(mounts.len(), 2, "got {:?}", mounts);
    }

    #[test]
    fn used_fraction_handles_empty_filesystem() {
        let m = MountInfo {
            device: "x".into(),
            mount_point: PathBuf::from("/"),
            fstype: "ext4".into(),
            total: 0,
            used: 0,
            free: 0,
        };
        assert_eq!(m.used_fraction(), 0.0);
    }

    #[test]
    fn used_fraction_is_clamped_to_unit_interval_for_normal_input() {
        let m = MountInfo {
            device: "x".into(),
            mount_point: PathBuf::from("/"),
            fstype: "ext4".into(),
            total: 1000,
            used: 500,
            free: 500,
        };
        let f = m.used_fraction();
        assert!((0.0..=1.0).contains(&f), "expected ratio, got {}", f);
        assert!((f - 0.5).abs() < 0.01);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn discover_returns_at_least_root() {
        // On a Linux test host we should always see `/` in the list.
        let mounts = discover();
        assert!(
            mounts
                .iter()
                .any(|m| m.mount_point.as_path() == std::path::Path::new("/")),
            "root filesystem should always be discoverable on Linux, got {:?}",
            mounts.iter().map(|m| &m.mount_point).collect::<Vec<_>>()
        );
    }
}

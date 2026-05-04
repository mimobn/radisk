//! On-disk snapshot of a completed scan.
//!
//! ## Format
//!
//! ```text
//!   bytes 0..4    magic        b"RDSK"
//!   bytes 4..6    version      u16  little-endian
//!   bytes 6..N    payload      zstd(postcard(TreeArena))
//! ```
//!
//! - `RDSK` so a stray `file foo.radisk` shows the brand and the
//!   format is recognisable in hex dumps.
//! - `version` is bumped any time the wire layout of [`TreeArena`] or
//!   the magic header changes; loaders refuse unknown versions with a
//!   contextual error, so an old binary cannot misinterpret a new
//!   snapshot.
//! - The payload is `postcard`-encoded (small, fast, no schema
//!   metadata) and then run through `zstd` with a moderate level for
//!   a strong size win on the heavily-repeated path strings.
//!
//! ## Why postcard + zstd
//!
//! Path-shaped trees compress extremely well — the same directory
//! prefix appears thousands of times. Empirically a postcard+zstd
//! snapshot of a 200k-file tree is ~3-5 MiB; the same data as JSON is
//! roughly an order of magnitude larger. Postcard is also ~10-50×
//! faster than JSON for tree-shaped data, which keeps `--export`
//! cheap on top of the scan itself.

use crate::tree::TreeArena;
use anyhow::{anyhow, bail, Context, Result};
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Magic bytes prefixing every radisk snapshot.
pub const MAGIC: &[u8; 4] = b"RDSK";

/// Current on-disk format version. Bumped only on incompatible
/// changes (postcard schema reshapes, magic-header changes). Adding
/// new optional fields with `#[serde(default)]` is *not* an
/// incompatible change.
pub const VERSION: u16 = 1;

/// `zstd` compression level. 5 is roughly twice as fast as the
/// default 21 and only ~5% worse on path-heavy payloads.
const ZSTD_LEVEL: i32 = 5;

/// Serialise `arena` into a snapshot at `path`.
///
/// Returns the byte length of the resulting file so callers can
/// surface "wrote 4.2 MiB" in the status bar.
pub fn save(arena: &TreeArena, path: &Path) -> Result<u64> {
    let payload = postcard::to_allocvec(arena).context("failed to encode arena as postcard")?;
    let compressed =
        zstd::encode_all(&payload[..], ZSTD_LEVEL).context("failed to zstd-encode snapshot")?;

    let file = File::create(path)
        .with_context(|| format!("failed to create snapshot {}", path.display()))?;
    let mut writer = BufWriter::new(file);

    writer
        .write_all(MAGIC)
        .context("failed to write snapshot magic")?;
    writer
        .write_all(&VERSION.to_le_bytes())
        .context("failed to write snapshot version")?;
    writer
        .write_all(&compressed)
        .context("failed to write snapshot payload")?;
    writer.flush().context("failed to flush snapshot")?;

    let bytes = (MAGIC.len() + 2 + compressed.len()) as u64;
    Ok(bytes)
}

/// Read a snapshot from `path` and return the decoded arena.
pub fn load(path: &Path) -> Result<TreeArena> {
    let file =
        File::open(path).with_context(|| format!("failed to open snapshot {}", path.display()))?;
    let mut reader = BufReader::new(file);

    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .context("failed to read snapshot magic")?;
    if &magic != MAGIC {
        bail!(
            "{} is not a radisk snapshot (magic was {:?}, expected {:?})",
            path.display(),
            magic,
            MAGIC
        );
    }

    let mut version_bytes = [0u8; 2];
    reader
        .read_exact(&mut version_bytes)
        .context("failed to read snapshot version")?;
    let version = u16::from_le_bytes(version_bytes);
    if version != VERSION {
        return Err(anyhow!(
            "{} is snapshot version {} but this radisk understands version {}; \
             upgrade radisk or re-export the snapshot",
            path.display(),
            version,
            VERSION
        ));
    }

    let mut compressed = Vec::new();
    reader
        .read_to_end(&mut compressed)
        .context("failed to read snapshot payload")?;
    let payload = zstd::decode_all(&compressed[..]).context("failed to zstd-decode snapshot")?;
    let arena: TreeArena =
        postcard::from_bytes(&payload).context("failed to decode arena postcard")?;
    Ok(arena)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::ScanConfig;
    use crate::scanner_streaming::{scan_streaming, ScanEvent};
    use assert_fs::prelude::*;
    use assert_fs::TempDir;
    use std::time::{Duration, Instant};

    fn make_arena(temp: &TempDir) -> TreeArena {
        temp.child("a.txt").write_str("hello").unwrap();
        temp.child("sub/b.txt").write_str("world!").unwrap();
        let handle = scan_streaming(temp.path(), &ScanConfig::default());
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match handle.rx.recv_timeout(Duration::from_millis(100)) {
                Ok(ScanEvent::Complete(a)) => return *a,
                Ok(_) => continue,
                Err(_) if Instant::now() > deadline => panic!("scan timed out"),
                Err(_) => continue,
            }
        }
    }

    #[test]
    fn round_trip_preserves_tree_shape() {
        let temp = TempDir::new().unwrap();
        let original = make_arena(&temp);

        let dest = temp.child("snap.radisk");
        let bytes = save(&original, dest.path()).unwrap();
        assert!(bytes > 6, "file must contain header + payload");

        let restored = load(dest.path()).unwrap();
        let orig_root = original.root().unwrap();
        let new_root = restored.root().unwrap();

        assert_eq!(
            original.total_file_count(orig_root),
            restored.total_file_count(new_root)
        );
        assert_eq!(
            original.folder(orig_root).file.size,
            restored.folder(new_root).file.size
        );
        assert_eq!(
            original.folder(orig_root).children_folders.len(),
            restored.folder(new_root).children_folders.len()
        );
    }

    #[test]
    fn missing_magic_is_rejected_with_path_in_message() {
        let temp = TempDir::new().unwrap();
        let bogus = temp.child("garbage.radisk");
        bogus.write_binary(b"not a snapshot at all").unwrap();
        let err = load(bogus.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("not a radisk snapshot"), "msg = {}", msg);
        assert!(
            msg.contains(bogus.path().to_str().unwrap()),
            "missing path in: {}",
            msg
        );
    }

    #[test]
    fn unknown_version_is_rejected() {
        let temp = TempDir::new().unwrap();
        let path = temp.child("future.radisk");
        // Magic + version 0xFFFF (definitely future).
        let mut blob = MAGIC.to_vec();
        blob.extend_from_slice(&u16::MAX.to_le_bytes());
        blob.extend_from_slice(&[0u8; 8]); // arbitrary trailing bytes
        path.write_binary(&blob).unwrap();
        let err = load(path.path()).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("snapshot version"), "msg = {}", msg);
        assert!(msg.contains("upgrade radisk"), "msg = {}", msg);
    }

    #[test]
    fn save_to_unwritable_path_errors_contextually() {
        let err = save(
            &TreeArena::new(),
            Path::new("/proc/cannot/write/here.radisk"),
        )
        .unwrap_err();
        let msg = format!("{:#}", err);
        assert!(
            msg.contains("failed to create snapshot"),
            "missing context in: {}",
            msg
        );
    }
}

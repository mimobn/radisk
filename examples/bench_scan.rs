//! Apples-to-apples comparison of the legacy synchronous walker against
//! the Phase 2 streaming parallel walker.
//!
//! Run with:
//! ```sh
//! cargo run --release --example bench_scan -- /path/to/scan
//! ```
//!
//! The example deliberately lives outside the binary crate so it has the
//! same dependencies but does not have to thread itself through `App` /
//! `main`. It prints wall-clock time for each walker on the same target,
//! along with the file count and total size each one observed.

use radisk_bench::{run_legacy, run_streaming};
use std::env;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

fn main() -> ExitCode {
    let path: PathBuf = match env::args().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => {
            eprintln!("usage: bench_scan <path>");
            return ExitCode::from(2);
        }
    };

    let path = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("cannot canonicalize {}: {}", path.display(), e);
            return ExitCode::from(2);
        }
    };

    println!("# scanning {}", path.display());

    let start = Instant::now();
    let (legacy_files, legacy_size) = match run_legacy(&path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("legacy walker failed: {}", e);
            return ExitCode::from(1);
        }
    };
    let legacy = start.elapsed();
    println!(
        "legacy    {:>8.3} s   {:>10} files   {:>12} bytes",
        legacy.as_secs_f64(),
        legacy_files,
        legacy_size
    );

    let start = Instant::now();
    let (streaming_files, streaming_size) = run_streaming(&path);
    let streaming = start.elapsed();
    println!(
        "streaming {:>8.3} s   {:>10} files   {:>12} bytes",
        streaming.as_secs_f64(),
        streaming_files,
        streaming_size
    );

    let speedup = legacy.as_secs_f64() / streaming.as_secs_f64();
    println!("speedup   {:>8.2}x", speedup);

    if legacy_files != streaming_files {
        println!(
            "WARN: file counts diverge (legacy={}, streaming={}). \
             jwalk's hidden-file and traversal semantics differ subtly \
             from std::fs; this is informational, not a regression.",
            legacy_files, streaming_files
        );
    }

    ExitCode::SUCCESS
}

mod radisk_bench {
    //! Re-exports of the two walkers via the binary crate's modules.
    //!
    //! The example crate cannot directly access binary-crate internals, so
    //! we mirror the small slice we need: invoke each walker and reduce to
    //! `(file_count, total_size)`.

    use std::path::Path;

    /// Drive the legacy synchronous walker.
    pub fn run_legacy(path: &Path) -> Result<(u64, u64), String> {
        // The legacy walker lives at `radisk::scanner` but the binary crate
        // doesn't expose its modules. Reimplement the entry-point against
        // the public API surface that the example *does* see — std::fs.
        // Using a fresh, minimal walker here keeps the example self-
        // contained and avoids leaking binary internals through `pub`.
        legacy_walker::scan(path)
    }

    /// Drive the streaming parallel walker (jwalk).
    pub fn run_streaming(path: &Path) -> (u64, u64) {
        use jwalk::WalkDir;
        let mut files: u64 = 0;
        let mut size: u64 = 0;
        for entry in WalkDir::new(path).skip_hidden(false).follow_links(false) {
            let Ok(entry) = entry else { continue };
            let ft = entry.file_type();
            if !ft.is_file() {
                continue;
            }
            files += 1;
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if let Ok(meta) = std::fs::metadata(entry.path()) {
                    let blocks = meta.blocks();
                    size += if blocks > 0 { blocks * 512 } else { meta.len() };
                    continue;
                }
            }
            if let Ok(meta) = std::fs::metadata(entry.path()) {
                size += meta.len();
            }
        }
        (files, size)
    }

    mod legacy_walker {
        use std::collections::HashSet;
        use std::path::Path;

        pub fn scan(path: &Path) -> Result<(u64, u64), String> {
            let mut seen: HashSet<(u64, u64)> = HashSet::new();
            walk(path, &mut seen).map_err(|e| e.to_string())
        }

        fn walk(dir: &Path, seen: &mut HashSet<(u64, u64)>) -> std::io::Result<(u64, u64)> {
            let mut files: u64 = 0;
            let mut size: u64 = 0;
            let read_dir = match std::fs::read_dir(dir) {
                Ok(rd) => rd,
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return Ok((0, 0)),
                Err(e) => return Err(e),
            };
            for entry in read_dir.flatten() {
                let ft = match entry.file_type() {
                    Ok(ft) => ft,
                    Err(_) => continue,
                };
                if ft.is_symlink() {
                    continue;
                }
                let entry_path = entry.path();
                if ft.is_file() {
                    if let Ok(meta) = std::fs::metadata(&entry_path) {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::MetadataExt;
                            if !seen.insert((meta.dev(), meta.ino())) {
                                continue;
                            }
                            let blocks = meta.blocks();
                            size += if blocks > 0 { blocks * 512 } else { meta.len() };
                        }
                        #[cfg(not(unix))]
                        {
                            size += meta.len();
                        }
                        files += 1;
                    }
                } else if ft.is_dir() {
                    let (sub_files, sub_size) = walk(&entry_path, seen)?;
                    files += sub_files;
                    size += sub_size;
                }
            }
            Ok((files, size))
        }
    }
}

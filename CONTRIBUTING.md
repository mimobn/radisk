# Contributing to radisk

Thanks for considering a contribution! This document is the
practical "what do I run, where does the code live" guide. For
*why* radisk is shaped the way it is, see
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Build & verify

radisk needs stable Rust 1.85 or newer.

```sh
git clone https://github.com/mimobn/radisk
cd radisk

# Standard dev loop
cargo build
cargo test

# Same gates CI runs (.github/workflows/ci.yml)
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps --document-private-items

# Reproduce the scanner benchmarks
cargo run --release --example bench_scan -- /usr/share
```

CI runs `test` on Linux / macOS / Windows and `fmt` / `clippy` /
`doc` on Linux. A PR is mergeable when all four jobs are green.

## Where things live

| You want to … | Touch |
| --- | --- |
| Add a CLI flag | `src/main.rs` (the `Cli` struct) |
| Add a new in-app action | `src/keybinds.rs` (`Action` enum), then `App::dispatch_action` |
| Change scanner behaviour | `src/scanner_streaming.rs` (production) / `src/scanner.rs` (legacy reference) |
| Add a config key | `src/config.rs` (`*ConfigOverrides`, `Partial*`, `into_full`) + `docs/CONFIG.md` |
| Tweak the radial layout | `src/radial.rs` (angle math) + `src/renderer.rs` (Braille canvas) |
| Add a view | `src/views.rs` (`View` enum + a render function), then `ui::render_viewing` |
| Change a colour | `src/theme.rs` (Roles + defaults) + `docs/CONFIG.md` |
| Support another package manager | `src/ownership.rs` (new `query_<pm>` + dispatcher arm) |
| Update on-disk snapshot format | `src/snapshot.rs` (bump `VERSION`) + `docs/SNAPSHOT_FORMAT.md` |

The full module map with one-line responsibilities is in
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Coding style

- **Formatting:** rustfmt defaults (`cargo fmt`).
- **Lints:** clippy, treated as errors (`-D warnings`). Don't paper
  over a lint with `#[allow(...)]` unless you've documented *why*.
- **Errors:** `anyhow::Result<T>` at boundaries; `thiserror` for
  domain errors that need to be matched on. Reach for `unwrap()` /
  `expect()` only in tests or where the invariant is locally
  documented.
- **Tests:** colocated `#[cfg(test)] mod tests` blocks. Aim for
  pure-function tests over end-to-end ones — most of radisk's logic
  splits cleanly into `build_*` / `parse_*` / `query_*` helpers
  with no UI side-effects.
- **Comments:** explain *why*, not *what*. The diff already shows
  what.

## Commit style

Conventional Commits — `<type>: <summary>` with one of:

```text
feat:      user-visible new capability
fix:       bug fix (regression or otherwise)
refactor:  internal change, no observable behaviour difference
chore:     deps, tooling, formatting
docs:      README / ARCHITECTURE / CHANGELOG / inline rustdoc
test:      adding or restructuring tests
ci:        GitHub Actions / cargo config
perf:      perf-focused change with measurement
```

The body should explain why, list any non-obvious trade-offs, and
mention anything a reviewer might miss in the diff. radisk's
existing log is a reasonable shape reference — `git log --oneline
master..HEAD` to see the recent style.

## Branching

We work on phase-shaped feature branches stacked off `master`:

```text
phase-1/stability
phase-2/streaming-scanner
…
phase-N/<topic>
```

Stack new work on the latest phase branch when there's a clear
ordering dependency, otherwise branch from `master`. Each phase is
intended to be one PR.

## Adding a feature: the playbook

1. **Brainstorm on an issue first** if the change is structural.
   Cheap to course-correct in a comment thread; expensive once the
   PR is open.
2. **Plan.** For anything touching ≥ 3 modules, sketch the data
   flow before you start typing — it's easier to land a clean
   architecture in one PR than to refactor across three.
3. **Tests first** for anything algorithmic (parsers, layout
   builders, sort orders). Pure functions get pure-function tests.
4. **One concept per commit.** A PR with eight tightly-scoped
   commits is easier to review than two commits that mix concerns.
5. **Update the docs in the same commit** as the code that needed
   them. CHANGELOG + the relevant `docs/*.md` + inline rustdoc.

## CHANGELOG

We follow [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
New PRs add entries under `## [Unreleased]` until a tagged release
moves them into a versioned block.

## Performance work

- Every PR that claims a speedup reproduces with `cargo run
  --release --example bench_scan -- <path>` — adapt the example if
  you're benching something else.
- Don't ship a perf change without numbers in the commit body.
- The scanner is the main hot path; the radial renderer is the
  main allocation hot path. Most other code paths are O(rows).

## Security-sensitive bits

- The delete path (`src/delete.rs`) is the most dangerous piece of
  code in the repo. New features there get extra review.
- Inode TOCTOU guard (`expected_inode`) is load-bearing — don't
  silently bypass it.
- File-deletion default is **No**, never **Yes** — Phase 1 cleanup
  enforces this and it should stay that way.

## Releases

Maintainer-only:

```sh
# Bump version
cargo set-version 0.X.Y
# Move [Unreleased] → [0.X.Y] in CHANGELOG with today's date
# Tag and push
git tag -a v0.X.Y -m "release v0.X.Y"
git push --tags
```

The release workflow handles cargo publish / archive uploads /
AUR `.SRCINFO` regeneration.

## Filing issues

Useful issue body:
- radisk version (`radisk --version`)
- OS + terminal emulator
- Minimal reproduction (path or snapshot)
- What you expected vs what happened

For crashes, run with `RUST_BACKTRACE=1` and include the trace.

## License

By contributing you agree your work ships under
[GPL-3.0-or-later](LICENSE).

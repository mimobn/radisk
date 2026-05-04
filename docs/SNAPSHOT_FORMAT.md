# Snapshot file format

`radisk --export PATH` writes a `.radisk` snapshot. `radisk --import PATH`
reads it back. The format is purposely small and versioned so future
radisk releases can keep reading old snapshots (or refuse them with a
clear error).

## Layout

```text
+--------+------+----------------------------+
| MAGIC  | VER  | PAYLOAD                    |
| 4 B    | 2 B  | zstd(postcard(TreeArena))  |
+--------+------+----------------------------+
```

| Field   | Bytes | Type            | Notes                                 |
| ------- | ----- | --------------- | ------------------------------------- |
| MAGIC   | 4     | ASCII           | `b"RDSK"` — recognisable in hex dumps |
| VER     | 2     | u16 LE          | bumped on incompatible changes only   |
| PAYLOAD | rest  | zstd→postcard   | tree arena (files, folders, root id)  |

## Versioning rules

| Change                                    | Bumps version? |
| ----------------------------------------- | -------------- |
| Adding a new `pub` field with `#[serde(default)]` | no       |
| Removing a field (or changing its layout) | yes            |
| Changing the magic header                 | yes            |
| Switching encoding format                 | yes            |

A reader that finds an unknown `VER` returns an error rather than
guessing — better to ask the user to upgrade than to silently
misinterpret.

## Why postcard + zstd

Path-shaped trees compress extremely well — the same directory prefix
appears thousands of times. Empirically:

| Tree              | Files   | postcard raw | postcard + zstd |
| ----------------- | ------- | ------------ | --------------- |
| `/tmp` (test box) | 1,846   | ~250 KiB     |  38 KiB         |
| `/usr/share`      | 215,039 | ~38 MiB      | ~3.5 MiB        |

postcard is also ~10–50× faster than `serde_json` on tree-shaped
data, so `--export` is essentially free on top of the scan itself.

## Forward-compatibility notes

- Don't reorder fields in `tree::File` / `tree::Folder` / `tree::TreeArena`
  without bumping `snapshot::VERSION`.
- Adding new fields is fine if they're annotated with
  `#[serde(default)]`.
- The `path: PathBuf` field is stored verbatim (lossily on Windows).
  Loading on a different OS than the export was made on gives
  cosmetically wrong path separators but otherwise works.

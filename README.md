# Premiere Hunter

Fast parallel search of Adobe Premiere Pro `.prproj` files, plus listing and relinking missing media.

`.prproj` files are gzipped XML. Premiere stores media as **text** in `<FilePath>` and `<ActualMediaFilePath>` — this tool searches that payload and, with `--fix`, actually rewrites those paths.

## Install

```bash
cargo build --release
```

Binary: `target/release/premiere-hunter` (`.exe` on Windows).

## Search

```bash
# Positional search text (case-insensitive)
premiere-hunter "clair de lune" --paths ./projects

# Same thing with -s
premiere-hunter -s "camera_015" --paths "D:\Projects"

# Snippet around the hit
premiere-hunter "jock" --paths . --show-snippets
```

Hits also print a nearby clip `<Name>` / `<Title>` when Premiere stored one.

If you pass a `.prproj` file, only that project is opened — no drive walk.

```bash
premiere-hunter -s "newplane" --paths "./chapterone.prproj"
```

## List and relink media

```bash
# Show FOUND / MISSING for every referenced clip
premiere-hunter --list-assets --paths "./chapterone.prproj"

# Preview relinks without writing
premiere-hunter --list-assets --fix --dry-run --paths "./chapterone.prproj" --paths "./media"

# Relink in place (writes a .prproj.bak first). --fix implies --list-assets.
premiere-hunter --fix --paths "./chapterone.prproj"

# Only the missing rows
premiere-hunter --list-assets --missing-only --paths "./chapterone.prproj"
```

`--fix` matches missing files **by filename**, then picks the best on-disk copy (same parent folder and drive beat Adobe Auto-Save / preview folders). It rewrites every copy of that path in the project XML, including `<ActualMediaFilePath>` and `<RelativePath>` that embed the absolute path.

`--search-all-drives` adds every existing drive letter on Windows, or `$HOME` plus `/mnt` `/media` `/run/media` on Unix.

## Config

```bash
premiere-hunter --config examples/config.yaml
```

CLI wins over YAML. `%USERPROFILE%`, `%APPDATA%`, `$HOME`, and `${HOME}` expand in `paths` and `exclude_dirs`.

```yaml
search_text: "clair de lune"
paths:
  - "%USERPROFILE%/Documents"
threads: 8
extensions: [prproj]
max_file_size_mb: 500
exclude_dirs:
  - Archive
```

System folders (`Windows`, `$Recycle.Bin`, `AppData`, …) are always skipped unless you are pointing at a specific file.

## Flags worth knowing

| Flag | What it does |
| --- | --- |
| `--threads N` | Rayon pool size |
| `--search-all-drives` | Expand roots to local drives / mounts |
| `--rescan` | Rebuild the relink filename cache |
| `--dry-run` | Print relinks, write nothing |
| `--no-backup` | Skip the `.prproj.bak` next to a rewritten project |
| `--show-snippets` | Print a slice of XML around a text match |
| `--missing-only` | With --list-assets / --fix, hide FOUND rows |

Cache for `--fix` lives at `~/.premiere-hunter/file_cache.json`. Text search does **not** build that index.

## How search works

1. Merge YAML + CLI paths (or default to `C:\`/`D:\` on Windows, the current directory on Unix).
2. Walk for `*.prproj` (and any extra `extensions`), skipping excluded folders.
3. Stream each gzipped project on a Rayon thread, case-insensitive.
4. Print matches as they land, then a summary.

Ctrl+C stops after in-flight files finish (exit 130).

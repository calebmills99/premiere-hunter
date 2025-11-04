# Premiere Hunter

A fast, parallel Rust CLI tool to search for text within Adobe Premiere Pro project files (.prproj).

## Features

- **Fast parallel processing** using the Rayon crate
- **Recursive search** across entire drives or specific directories
- **Case-insensitive search** for text within .prproj files
- **Progress bar** showing real-time search status
- **Error handling** that gracefully skips inaccessible files
- **Cross-platform** (though optimized for Windows paths)

## Installation

### Prerequisites

- [Rust](https://rustup.rs/) (1.70 or later)

### Build from source

```bash
git clone <repository-url>
cd premiere-hunter
cargo build --release
```

The compiled binary will be available at `target/release/premiere-hunter` (or `premiere-hunter.exe` on Windows).

## Usage

### Basic usage

Search for "clair de lune" in C:\ and D:\ drives (default on Windows):

```bash
premiere-hunter "clair de lune"
```

### Custom search paths

Search specific directories:

```bash
premiere-hunter "your search term" --paths "C:\Users\YourName\Documents","D:\Projects"
```

### Adjust thread count

Specify the number of threads to use:

```bash
premiere-hunter "search term" --threads 8
```

### Help

View all available options:

```bash
premiere-hunter --help
```

## How it works

1. Recursively scans specified directories for `.prproj` files
2. Processes files in parallel using multiple CPU cores
3. Performs case-insensitive substring matching on file contents
4. Displays matching file paths in real-time
5. Shows summary statistics when complete

## Example output

```
Searching for: 'clair de lune'
Search paths: ["C:\\", "D:\\"]
Scanning for .prproj files...

Found 47 .prproj files to search

[00:00:03] [==============================>---------] 32/47 files (10/s)

✓ MATCH: C:\Users\YourName\Videos\Projects\Wedding_2024\wedding_final.prproj
✓ MATCH: D:\Archive\Music Videos\debussy_tribute.prproj

============================================================
Search complete!
Files processed: 47
Matches found: 2
Files skipped (errors): 3
============================================================
```

## Dependencies

- [walkdir](https://crates.io/crates/walkdir) - Recursive directory traversal
- [rayon](https://crates.io/crates/rayon) - Parallel processing
- [clap](https://crates.io/crates/clap) - Command-line argument parsing
- [indicatif](https://crates.io/crates/indicatif) - Progress bars

## License

MIT

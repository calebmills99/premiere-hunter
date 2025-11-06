# Premiere Hunter

A fast, parallel Rust CLI tool to search for text within Adobe Premiere Pro project files (.prproj).

## Features

- **Fast parallel processing** using the Rayon crate
- **Recursive search** across entire drives or specific directories
- **Case-insensitive search** for text within project files
- **Progress bar** showing real-time search status
- **Error handling** that gracefully skips inaccessible files
- **YAML configuration** for persistent search settings
- **Streaming file search** for memory-efficient searching of large files
- **Flexible file filtering** by extensions, directories, and size
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

### Using YAML configuration

Create a configuration file to store your search settings:

```bash
premiere-hunter --config config.yaml
```

CLI arguments override YAML settings. You can combine both:

```bash
premiere-hunter "override search" --config config.yaml --threads 16
```

See `examples/config.example.yaml` for a complete configuration example.

### Help

View all available options:

```bash
premiere-hunter --help
```

## Configuration

You can use a YAML configuration file to set default search parameters. Here's an example:

```yaml
# Search text (can be overridden by CLI argument)
search_text: "clair de lune"

# Directories to search
paths:
  - "C:\\Users\\YourName\\Documents"
  - "D:\\Projects"

# Number of threads (optional, defaults to CPU cores)
threads: 8

# File extensions to search (defaults to ["prproj"])
extensions:
  - "prproj"
  - "aep"  # Also search After Effects projects

# Follow symbolic links (defaults to false)
follow_links: false

# Maximum file size in MB (optional, files larger are skipped)
# Defaults to 100 MB. Set to 0 to disable the limit entirely.
max_file_size_mb: 100

# Directories to exclude from search (optional)
exclude_dirs:
  - "node_modules"
  - ".git"
  - "temp"
```

All settings are optional. CLI arguments take precedence over YAML settings.

## How it works

1. Loads configuration from YAML file (if provided) and merges with CLI arguments
2. Recursively scans specified directories for files matching the configured extensions
3. Filters out excluded directories and files exceeding size limits
4. Processes files in parallel using multiple CPU cores
5. Performs case-insensitive streaming search with minimal memory usage
6. Displays matching file paths in real-time
7. Shows summary statistics when complete

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
- [serde](https://crates.io/crates/serde) - Serialization/deserialization
- [serde_yaml](https://crates.io/crates/serde_yaml) - YAML configuration support

## License

MIT



## Auto-discover drives for Premiere projects

You can now automatically include common Windows drives in your search roots so the tool will scan for `.prproj` files across the whole machine.

- CLI: `--auto-drives` merges C:\ and D:\ (when present) with any `--paths` and config paths.
- Config: set `auto_drives: true` to enable the same behavior from YAML.

Examples:

```
# Scan C:\ and D:\ for .prproj, prompt for search text if missing
premiere-hunter --auto-drives

# Use config + also include C:\ and D:\
premiere-hunter --config examples\config.yaml --auto-drives

# Provide search text and auto-drives
premiere-hunter "camera-15" --auto-drives
```

Notes:
- If no paths are provided via CLI or config, the tool already defaults to scanning `C:\` and `D:\` when they exist.
- `--auto-drives` makes this explicit and merges with provided paths instead of overriding them.
- Keep your `extensions` to `prproj` for the best performance unless you intentionally want other types.

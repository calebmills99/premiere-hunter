use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::collections::HashSet;
use walkdir::{DirEntry, WalkDir};
use flate2::read::GzDecoder;
use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Deserialize, Serialize)]
struct Config {
    search_text: Option<String>,
    paths: Option<Vec<PathBuf>>,
    threads: Option<usize>,
    /// When true, automatically include common drives (C:\ and D:\) in the search roots
    auto_drives: Option<bool>,
    #[serde(default = "default_extensions")]
    extensions: Vec<String>,
    #[serde(default)]
    follow_links: bool,
    max_file_size_mb: Option<usize>,
    exclude_dirs: Option<Vec<String>>,
}

fn default_extensions() -> Vec<String> {
    vec!["prproj".to_string()]
}

#[derive(Parser, Debug)]
#[command(name = "premiere-hunter")]
#[command(about = "Fast parallel search for text in Premiere Pro project files", long_about = None)]
struct Args {
    /// Text to search for (case-insensitive). When used with --list-assets, it acts as a filter.
    #[arg(short = 's', long = "search")]
    search_text: Option<String>,

    /// Paths to search (defaults to C:\ and D:\ on Windows)
    #[arg(short, long, value_delimiter = ',')]
    paths: Option<Vec<PathBuf>>,

    /// DEPRECATED: Use --search-all-drives instead. Include common fixed drives in the search roots.
    /// When used, these are merged with any provided --paths and config paths
    #[arg(long, default_value_t = false)]
    auto_drives: bool,

    /// Number of threads to use (defaults to number of CPU cores)
    #[arg(short, long)]
    threads: Option<usize>,

    /// Path to YAML configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// List assets used in each .prproj instead of free-text search. If SEARCH_TEXT is provided, it filters assets by substring (case-insensitive).
    #[arg(long, default_value_t = false)]
    list_assets: bool,

    /// Print a text snippet around each match (extracted from the project file)
    #[arg(long, default_value_t = false)]
    show_snippets: bool,

    /// Max number of characters to show in each snippet (total)
    #[arg(long, default_value_t = 120)]
    snippet_chars: usize,

    /// Search all local fixed drives and the Google Drive virtual drive.
    #[arg(long, default_value_t = false)]
    search_all_drives: bool,

    /// Forces a full rescan of the filesystem, ignoring and overwriting any existing cache.
    #[arg(long, default_value_t = false)]
    rescan: bool,

    /// After listing assets, attempts to fix the paths of missing assets by rewriting the .prproj file.
    #[arg(
        long,
        default_value_t = false,
        requires = "list_assets",
        aliases = ["fix-missing", "relink"],
        help = "When used with --list-assets, attempts to relink missing assets by rewriting the .prproj file"
    )]
    fix: bool,
}
#[derive(Parser, Debug)]
struct FixArgs {
    // This is a placeholder for future `--fix-missing` specific arguments
}

fn load_config(path: &PathBuf) -> Result<Config, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let config: Config = serde_yaml::from_str(&content)?;
    Ok(config)
}

fn file_contains_case_insensitive(
    path: &PathBuf,
    search_text: &str,
    max_size_bytes: Option<usize>,
) -> Result<bool, std::io::Error> {
    // Check file size if limit is set (on-disk size)
    if let Some(max_bytes) = max_size_bytes {
        let metadata = fs::metadata(path)?;
        if metadata.len() > max_bytes as u64 {
            return Ok(false); // Skip files that are too large
        }
    }

    // Open file and detect gzip by magic bytes 0x1F 0x8B
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?; // rewind after peek

    let reader: Box<dyn Read> = if n == 2 && magic == [0x1F, 0x8B] {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };

    let reader = BufReader::new(reader);

    let search_lower = search_text.to_lowercase();
    let search_len = search_text.len();

    let mut overlap = String::new();

    // Read lines as UTF-8; if an encoding error occurs, surface it so caller counts as error
    for line in reader.lines() {
        let line = line?;
        let combined = format!("{}{}", overlap, line);

        if combined.to_lowercase().contains(&search_lower) {
            return Ok(true);
        }

        // Keep overlap of last (search_len - 1) chars for matches across lines
        if combined.len() >= search_len && search_len > 0 {
            overlap = combined
                .chars()
                .rev()
                .take(search_len - 1)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        } else {
            overlap = combined;
        }
    }

    Ok(false)
}

// Streaming search that returns the first matched text snippet for display
fn file_snippet_case_insensitive(
    path: &PathBuf,
    search_text: &str,
    max_size_bytes: Option<usize>,
    snippet_chars: usize,
) -> Result<Option<String>, std::io::Error> {
    // Check file size if limit is set (on-disk size)
    if let Some(max_bytes) = max_size_bytes {
        let metadata = fs::metadata(path)?;
        if metadata.len() > max_bytes as u64 {
            return Ok(None); // Skip files that are too large
        }
    }

    // Open file and detect gzip by magic bytes 0x1F 0x8B
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?; // rewind after peek

    let reader: Box<dyn Read> = if n == 2 && magic == [0x1F, 0x8B] {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };

    let reader = BufReader::new(reader);

    let needle_lower = search_text.to_ascii_lowercase();
    let search_len = needle_lower.len();
    let mut overlap = String::new();

    let total_chars = if snippet_chars == 0 { 120 } else { snippet_chars };
    let half = total_chars / 2;

    for line in reader.lines() {
        let line = line?;
        let combined = format!("{}{}", overlap, line);
        let combined_lower = combined.to_ascii_lowercase();

        if let Some(pos) = combined_lower.find(&needle_lower) {
            let match_start = pos;
            let match_end = pos + search_len;

            let start = match_start.saturating_sub(half);
            let end = std::cmp::min(combined.len(), match_end + half);

            // Ensure we slice on char boundaries
            let start = combined.char_indices().map(|(i, _)| i).take_while(|i| *i <= start).last().unwrap_or(0);
            let end = combined.char_indices().map(|(i, _)| i).take_while(|i| *i <= end).last().unwrap_or(combined.len());

            let mut snippet = combined[start..end].to_string();
            // Compact whitespace/newlines (though `line` doesn't include newlines)
            snippet = snippet.replace('\t', " ");

            let prefix = if start > 0 { "..." } else { "" };
            let suffix = if end < combined.len() { "..." } else { "" };

            let snippet = format!("{}{}{}", prefix, snippet, suffix);
            return Ok(Some(snippet));
        }

        // Keep overlap of last (search_len - 1) chars for matches across lines
        if combined.len() >= search_len && search_len > 0 {
            overlap = combined
                .chars()
                .rev()
                .take(search_len - 1)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        } else {
            overlap = combined;
        }
    }

    Ok(None)
}

fn is_excluded_dir(entry: &DirEntry, exclude_dirs: &Option<Vec<String>>) -> bool {
    if let Some(ref excludes) = exclude_dirs {
        if let Some(name) = entry.file_name().to_str() {
            return excludes.iter().any(|exc| name.eq_ignore_ascii_case(exc));
        }
    }
    false
}

fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

fn normalize_asset_path(raw: &str) -> String {
    let mut v = raw.trim().to_string();
    // Remove URL prefix if present
    if v.to_lowercase().starts_with("file:///") {
        v = v[8..].to_string();
    } else if v.to_lowercase().starts_with("file://") {
        v = v[7..].to_string();
    }
    v = v.replace('/', "\\");
    let mut v = xml_unescape(&v);

    let upper_v = v.to_uppercase();

    // Case 1: Find a Windows drive letter (e.g., "D:\") and treat it as the root.
    let mut drive_letter_pos: Option<usize> = None;
    for letter in "ABCDEFGHIJKLMNOPQRSTUVWXYZ".chars() {
        let pattern = format!("{}:\\", letter);
        if let Some(pos) = upper_v.find(&pattern) {
            if drive_letter_pos.is_none() || pos < drive_letter_pos.unwrap() {
                drive_letter_pos = Some(pos);
            }
        }
    }
    if let Some(pos) = drive_letter_pos {
        v = v[pos..].to_string();
        return v; // Found the most likely root, we're done.
    }

    // Case 2: Find a macOS-style "/Volumes/" path and treat it as the root.
    if let Some(pos) = upper_v.find("\\VOLUMES\\") {
        // We take from the beginning of "\Volumes\"
        v = v[pos..].to_string();
        return v;
    }
    v // If neither of the above, return the path as is for normal relative/absolute path handling.
}

#[derive(Debug)]
struct AssetInfo {
    path: String,
    found: bool,
}

fn extract_assets_from_prproj(path: &Path, max_size_bytes: Option<usize>) -> Result<Vec<AssetInfo>, std::io::Error> {
    // Check on-disk size limit before reading
    if let Some(max_bytes) = max_size_bytes {
        let metadata = fs::metadata(path)?;
        if metadata.len() > max_bytes as u64 {
            return Ok(Vec::new());
        }
    }

    // Open and maybe gzip-decode
    let mut file = fs::File::open(path)?;
    let mut magic = [0u8; 2];
    let n = file.read(&mut magic)?;
    file.seek(SeekFrom::Start(0))?;

    let reader: Box<dyn Read> = if n == 2 && magic == [0x1F, 0x8B] {
        Box::new(GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut buf_reader = BufReader::new(reader);

    let mut bytes = Vec::new();
    buf_reader.read_to_end(&mut bytes)?;

    // Collect candidates
    let mut seen: HashSet<String> = HashSet::new();
    let mut assets: Vec<AssetInfo> = Vec::new();

    let asset_exts: HashSet<&'static str> = [
        "mp4", "mov", "mxf", "mts", "m2ts", "avi", "mkv", "wmv", "m4v", "3gp",
        "wav", "mp3", "aac", "m4a", "aif", "aiff", "flac", "ogg",
        "png", "jpg", "jpeg", "tif", "tiff", "bmp", "gif", "psd", "ai", "svg", "dng", "cr2", "nef", "arw",
        "prfpset", "mogrt"
    ].into_iter().collect();

    // Initialize XML reader
    let mut reader = Reader::from_reader(bytes.as_slice());
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut want_text = false;

    fn is_path_name(n: &str) -> bool {
        matches!(n.to_ascii_lowercase().as_str(), "absolutepath" | "filepath" | "path" | "relativepath" | "relpath")
    }

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name_bytes = e.name().as_ref().to_vec();
                let name = String::from_utf8_lossy(&name_bytes).to_string();
                let is_path_tag = is_path_name(&name);
                if is_path_tag {
                    want_text = true;
                }
                for a in e.attributes().with_checks(false) {
                    if let Ok(attr) = a {
                        let key = String::from_utf8_lossy(attr.key.as_ref());
                        if is_path_name(&key) {
                            if let Ok(val) = attr.unescape_value() {
                                let norm = normalize_asset_path(&val);
                                if let Some(ext) = Path::new(&norm).extension().and_then(|e| e.to_str()) {
                                    let key = norm.to_lowercase();
                                    if asset_exts.contains(&ext.to_ascii_lowercase()[..]) && seen.insert(key) {
                                        let asset_path = PathBuf::from(&norm);
                                        let found = asset_path.exists();
                                        assets.push(AssetInfo { path: norm, found });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                // Handle attributes on empty tags
                for a in e.attributes().with_checks(false) {
                    if let Ok(attr) = a {
                        let key = String::from_utf8_lossy(attr.key.as_ref());
                        if is_path_name(&key) {
                            if let Ok(val) = attr.unescape_value() {
                                let norm = normalize_asset_path(&val);
                                if let Some(ext) = Path::new(&norm).extension().and_then(|e| e.to_str()) {
                                    let key = norm.to_lowercase();
                                    if asset_exts.contains(&ext.to_ascii_lowercase()[..]) && seen.insert(key) {
                                        let asset_path = PathBuf::from(&norm);
                                        let found = asset_path.exists();
                                        assets.push(AssetInfo { path: norm, found });
                                    }
                                }
                            }
                        }
                    }
                }
                want_text = false;
            }
            Ok(Event::Text(t)) => {
                if want_text {
                    if let Ok(val) = t.unescape() {
                        let norm = normalize_asset_path(&val);
                        if let Some(ext) = Path::new(&norm).extension().and_then(|e| e.to_str()) {
                            let key = norm.to_lowercase();
                            if asset_exts.contains(&ext.to_ascii_lowercase()[..]) && seen.insert(key) {
                                let asset_path = PathBuf::from(&norm);
                                let found = asset_path.exists();
                                assets.push(AssetInfo { path: norm, found });
                            }
                        }
                    }
                }
            }
            Ok(Event::End(_)) => {
                want_text = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => break, // On malformed XML, return what we have
            _ => {}
        }
        buf.clear();
    }

    assets.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(assets)
}

fn is_path_name(n: &str) -> bool {
    matches!(n.to_ascii_lowercase().as_str(), "absolutepath" | "filepath" | "path" | "relativepath" | "relpath")
}

use quick_xml::Writer;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::Cursor;
// use quick_xml::events::BytesStart; // not used

fn rewrite_prproj(
    project_path: &Path,
    corrections: &HashMap<String, String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Read the original file and detect if it's gzipped.
    let original_bytes = fs::read(project_path)?;
    let is_gzipped = original_bytes.get(0..2) == Some(&[0x1F, 0x8B]);

    let xml_bytes: Vec<u8> = if is_gzipped {
        let mut decoder = GzDecoder::new(&original_bytes[..]);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        decompressed
    } else {
        original_bytes
    };

    // 2. Prepare to read the XML and write to a new buffer.
    let mut reader = Reader::from_reader(xml_bytes.as_slice());
    reader.trim_text(true);
    let mut writer = Writer::new(Cursor::new(Vec::new()));
    let mut buf = Vec::new();

    // 3. Loop and rewrite.
    loop {
        let event = reader.read_event_into(&mut buf)?;
        match event {
            Event::Start(e) => {
                let mut new_elem = e.to_owned();
                let mut was_modified = false;
                // Hold owned copies of attributes to satisfy lifetimes
                let mut new_attrs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

                for attr in e.attributes() {
                    let attr = attr?;
                    // Own key and value bytes to avoid borrowing from temporary `attr`
                    let key_owned: Vec<u8> = attr.key.as_ref().to_vec();
                    let mut val_owned: Vec<u8> = attr.value.as_ref().to_vec();
                    let key_str = std::str::from_utf8(&key_owned)?;

                    if is_path_name(key_str) {
                        let val_str = attr.unescape_value()?;
                        let norm_path = normalize_asset_path(&val_str);
                        if let Some(new_path) = corrections.get(&norm_path) {
                            val_owned = new_path.as_bytes().to_vec();
                            was_modified = true;
                        }
                    }
                    new_attrs.push((key_owned, val_owned));
                }

                if was_modified {
                    new_elem.clear_attributes();
                    for (k, v) in new_attrs.iter() {
                        new_elem.push_attribute((k.as_slice(), v.as_slice()));
                    }
                }
                writer.write_event(Event::Start(new_elem))?;
            }
            Event::Empty(e) => {
                let mut new_elem = e.to_owned();
                let mut was_modified = false;
                // Hold owned copies of attributes to satisfy lifetimes
                let mut new_attrs: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

                for attr in e.attributes() {
                    let attr = attr?;
                    // Own key and value bytes to avoid borrowing from temporary `attr`
                    let key_owned: Vec<u8> = attr.key.as_ref().to_vec();
                    let mut val_owned: Vec<u8> = attr.value.as_ref().to_vec();
                    let key_str = std::str::from_utf8(&key_owned)?;

                    if is_path_name(key_str) {
                        let val_str = attr.unescape_value()?;
                        let norm_path = normalize_asset_path(&val_str);
                        if let Some(new_path) = corrections.get(&norm_path) {
                            val_owned = new_path.as_bytes().to_vec();
                            was_modified = true;
                        }
                    }
                    new_attrs.push((key_owned, val_owned));
                }

                if was_modified {
                    new_elem.clear_attributes();
                    for (k, v) in new_attrs.iter() {
                        new_elem.push_attribute((k.as_slice(), v.as_slice()));
                    }
                }
                writer.write_event(Event::Empty(new_elem))?;
            }
            Event::Eof => break,
            e => writer.write_event(e)?,
        }
        buf.clear();
    }

    let final_bytes = writer.into_inner().into_inner();
    // Preserve original compression: if the source was gzipped, write gzipped output
    if is_gzipped {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        use std::io::Write as _;
        encoder.write_all(&final_bytes)?;
        let compressed = encoder.finish()?;
        fs::write(project_path, compressed)?;
    } else {
        fs::write(project_path, final_bytes)?;
    }
    Ok(())
}

fn get_cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|mut path| {
        path.push(".premiere-hunter");
        path.push("file_cache.json");
        path
    })
}

fn load_cache(path: &Path) -> Result<HashMap<String, Vec<PathBuf>>, Box<dyn std::error::Error>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let map = serde_json::from_reader(reader)?;
    Ok(map)
}

fn save_cache(path: &Path, map: &HashMap<String, Vec<PathBuf>>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(path)?;
    let mut writer = io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, map)?;
    Ok(())
}

fn main() {
    let args = Args::parse();

    // Load config from file if provided
    let config = if let Some(ref config_path) = args.config {
        match load_config(config_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                eprintln!("Error loading config file: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        None
    };

    // Merge CLI args with config (CLI takes precedence); if none provided and not in --list-assets mode, prompt interactively
    let mut search_text_opt = args
        .search_text.as_ref()
        .or_else(|| config.as_ref().and_then(|c| c.search_text.as_ref())).cloned();

    if !args.list_assets {
        if search_text_opt.is_none() {
            println!("No search text provided via CLI or config. Please enter the text to search for:");
            print!("> ");
            io::stdout().flush().ok();
            let mut input = String::new();
            match io::stdin().read_line(&mut input) {
                Ok(_) => {
                    let trimmed = input.trim().to_string();
                    if trimmed.is_empty() {
                        eprintln!("Error: Search text cannot be empty");
                        std::process::exit(1);
                    } else {
                        search_text_opt = Some(trimmed);
                    }
                }
                Err(e) => {
                    eprintln!("Error reading input: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }

    // In list-assets mode, SEARCH_TEXT is an optional filter; in search mode, it must be present
    let required_search_text: Option<String> = if args.list_assets {
        None
    } else {
        Some(search_text_opt.clone().expect("search text must be set"))
    };

    let threads = args
        .threads
        .or_else(|| config.as_ref().and_then(|c| c.threads));

    // Merge paths from config and CLI (deduplicated), with both included if provided
    let cli_paths = args.paths.clone();
    let cfg_paths = config.as_ref().and_then(|c| c.paths.clone());

    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut source_parts: Vec<&str> = Vec::new();

    if let Some(ref cfg) = cfg_paths {
        if !cfg.is_empty() {
            search_paths.extend(cfg.clone());
            source_parts.push("config");
        }
    }
    if let Some(ref cli) = cli_paths {
        if !cli.is_empty() {
            search_paths.extend(cli.clone());
            source_parts.push("CLI");
        }
    }

    // Auto-include common drives when requested (C:\ and D:\ if they exist)
    let search_all_drives = args.search_all_drives || args.auto_drives || config.as_ref().and_then(|c| c.auto_drives).unwrap_or(false);
    if search_all_drives {
        let mut added_any = false;

        // Enumerate all fixed drives on Windows
        let drive_letters = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
        for letter in drive_letters.chars() {
            let drive_path_str = format!("{}:\\", letter);
            let drive_path = Path::new(&drive_path_str);
            // `GetDriveTypeW` returns 3 for DRIVE_FIXED
            if drive_path.exists() {
                let drive_type = unsafe { kernel32::GetDriveTypeW(widestring::U16CString::from_str(&drive_path_str).unwrap().as_ptr()) };
                if drive_type == 2 || drive_type == 3 || drive_type == 4 {
                    search_paths.push(drive_path.to_path_buf());
                    added_any = true;
                }
            }
        }

        // Specifically check for Google Drive's default mount point
        let gdrive_path = PathBuf::from("G:\\");
        if gdrive_path.exists() {
            // Check if volume label contains "Google Drive"
            let mut volume_name_buffer = [0u16; 256];
            if unsafe { kernel32::GetVolumeInformationW(widestring::U16CString::from_str("G:\\").unwrap().as_ptr(), volume_name_buffer.as_mut_ptr(), volume_name_buffer.len() as u32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut(), 0) } != 0 {
                if String::from_utf16_lossy(&volume_name_buffer).contains("Google Drive") {
                    search_paths.push(gdrive_path);
                    added_any = true;
                }
            }
        }

        if added_any {
            source_parts.push("auto");
        }
    }

    if search_paths.is_empty() {
        search_paths = vec![PathBuf::from("C:\\"), PathBuf::from("D:\\")];
        // Only add "defaults" if auto-search wasn't requested but paths were still empty
        if !search_all_drives {
            source_parts.push("defaults");
        }
    }

    // Deduplicate paths (case-insensitive for Windows)
    let mut seen: HashSet<String> = HashSet::new();
    search_paths.retain(|p| {
        let key = p.to_string_lossy().to_lowercase();
        seen.insert(key)
    });

    let path_source = if source_parts.len() > 1 {
        format!("{} (merged)", source_parts.join("+"))
    } else {
        source_parts.get(0).cloned().unwrap_or("unknown").to_string()
    };

    let extensions = config
        .as_ref()
        .map(|c| c.extensions.clone())
        .unwrap_or_else(|| vec!["prproj".to_string()]);

    let follow_links = config.as_ref().map(|c| c.follow_links).unwrap_or(false);

    // Max file size is now opt-in via config. By default, there is no limit.
    let max_file_size_mb = config.as_ref().and_then(|c| c.max_file_size_mb);
    let max_file_size_bytes = max_file_size_mb.and_then(|mb| if mb == 0 { None } else { Some(mb * 1024 * 1024) });

    let exclude_dirs = config.as_ref().and_then(|c| c.exclude_dirs.clone());

    // Set up thread pool
    if let Some(threads) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .unwrap();
    }


    let list_assets = args.list_assets;
    if list_assets {
        println!("Listing assets used in Premiere project files");
        if args.search_text.is_some() {
            // This is a good place for a note if needed in the future.
        }

        if let Some(ref f) = search_text_opt {
            println!("Asset filter (case-insensitive): '{}'", f);
        }
    } else {
        let st = required_search_text.as_ref().expect("search text must be set");
        println!("Searching for: '{}'", st);
    }
    println!("Search paths ({}): {:?}", path_source, search_paths);
    println!("Extensions: {:?}", extensions);
    if let Some(ref excludes) = exclude_dirs {
        println!("Excluding directories: {:?}", excludes);
    }
    if let Some(max_mb) = max_file_size_mb {
        println!("Max file size: {} MB", max_mb);
    }
    // If user provided specific file paths, we'll skip the global scan/cache and only operate on those files.
    // Otherwise, we perform the usual discovery and (optional) caching.
    println!("Scanning for files...\n");

    // Ctrl+C (SIGINT) graceful interruption
    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let int_flag = Arc::clone(&interrupted);
        if let Err(e) = ctrlc::set_handler(move || {
            // Only print on first interrupt
            if !int_flag.swap(true, Ordering::SeqCst) {
                eprintln!("\nReceived Ctrl+C — stopping early (letting active tasks finish)...");
            }
        }) {
            eprintln!("Warning: failed to set Ctrl+C handler: {}", e);
        }
    }

    // Detect if any provided paths are explicit files
    let mut explicit_targets: Vec<PathBuf> = Vec::new();
    for p in &search_paths {
        if p.is_file() {
            explicit_targets.push(p.clone());
        }
    }

    // Apply extension and size filters to explicit files (if any)
    if !explicit_targets.is_empty() {
        explicit_targets.retain(|p| {
            // extension filter
            let ok_ext = p.extension()
                .and_then(|e| e.to_str())
                .map(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
                .unwrap_or(false);
            if !ok_ext { return false; }
            // size filter (on-disk size)
            if let Some(max_bytes) = max_file_size_bytes {
                if let Ok(md) = fs::metadata(p) {
                    if md.len() > max_bytes as u64 { return false; }
                }
            }
            true
        });
        if explicit_targets.is_empty() {
            println!("No files matched filters (extensions/size).");
            return;
        }
        if args.fix {
            println!("Using {} provided file(s) as targets; building file index for relink (fix mode).", explicit_targets.len());
        } else {
            println!("Using {} provided file(s); skipping system-wide scan and cache.", explicit_targets.len());
        }
    }

    // Collect all matching files first
    // And build a map of all files on the system for the "fixer" phase
    let mut file_map = HashMap::new();
    let cache_path_opt = get_cache_path();
    let use_cache = !args.rescan;

    if use_cache {
        if let Some(ref cache_path) = cache_path_opt {
            if cache_path.exists() {
                println!("Loading file index from cache...");
                match load_cache(cache_path) {
                    Ok(map) => {
                        file_map = map;
                        println!("Cache loaded successfully.");
                    }
                    Err(e) => {
                        eprintln!("Warning: Could not load cache (will rescan): {}", e);
                        // Fall through to rescan
                    }
                }
            }
        }
    }

    // Only scan the filesystem when we aren't in explicit single-file mode
    // But always scan if --fix is requested, because relinking requires an index of files.
    if file_map.is_empty() && (explicit_targets.is_empty() || args.fix) {
        // When in fix mode and the user provided explicit project files, also add their parent
        // directories as scan roots so we at least index nearby media. Users can still provide
        // broader directories or --search-all-drives for a full-machine index.
        let mut scan_roots: Vec<PathBuf> = search_paths.clone();
        if args.fix && !explicit_targets.is_empty() {
            let mut added = 0usize;
            for f in &explicit_targets {
                if let Some(parent) = f.parent() {
                    // Prefer the nearest real directory
                    if parent.exists() {
                        scan_roots.push(parent.to_path_buf());
                        added += 1;
                    }
                }
            }
            if added > 0 {
                println!(
                    "Fix mode: also indexing {} parent folder(s) of provided project file(s). For broader relinking, add more --paths or use --search-all-drives.",
                    added
                );
            }
        }

        // Deduplicate roots (case-insensitive on Windows) and keep only existing directories
        let mut seen_roots: HashSet<String> = HashSet::new();
        scan_roots.retain(|p| {
            if p.is_dir() && p.exists() {
                let key = p.to_string_lossy().to_lowercase();
                seen_roots.insert(key)
            } else {
                false
            }
        });

        println!("Scanning for files... (This may take a while on the first run)");
        let discovered_map = Arc::new(Mutex::new(HashMap::<String, Vec<PathBuf>>::new()));
        for path in &scan_roots {
            if interrupted.load(Ordering::SeqCst) {
                break;
            }
            if !path.exists() {
                eprintln!("Warning: Path does not exist: {:?}", path);
                continue;
            }

            let path_file_map = Arc::clone(&discovered_map);
            for entry in WalkDir::new(path)
                .follow_links(follow_links)
                .into_iter()
                .filter_entry(|e| !is_excluded_dir(e, &exclude_dirs))
                .filter_map(|e| e.ok())
            {
                if interrupted.load(Ordering::SeqCst) {
                    break;
                }
                if entry.file_type().is_file() {
                    if let Some(name) = entry.file_name().to_str() {
                        path_file_map.lock().unwrap()
                            .entry(name.to_lowercase())
                            .or_default().push(entry.path().to_path_buf());
                    }
                }
            }
        }
        file_map = Arc::try_unwrap(discovered_map).unwrap().into_inner().unwrap();

        if let Some(ref cache_path) = cache_path_opt {
            println!("Saving file index to cache...");
            if let Err(e) = save_cache(cache_path, &file_map) {
                eprintln!("Warning: Could not save cache: {}", e);
            }
        }
    }

    // Now, determine target files
    let mut target_files: Vec<PathBuf> = Vec::new();
    if !explicit_targets.is_empty() {
        target_files = explicit_targets;
    } else {
        for (name, paths) in &file_map {
            if let Some(ext) = Path::new(name).extension().and_then(|s| s.to_str()) {
                if extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
                    for path in paths {
                        if let Some(max_bytes) = max_file_size_bytes {
                            if let Ok(metadata) = fs::metadata(path) {
                                if metadata.len() > max_bytes as u64 {
                                    continue;
                                }
                            }
                        }
                        target_files.push(path.clone());
                    }
                }
            }
        }
    }

    let total_files = target_files.len();
    println!("Found {} files to search\n", total_files);

    if interrupted.load(Ordering::SeqCst) {
        eprintln!("Interrupted during file discovery. Found {} files so far.", total_files);
        println!("\n{}", "=".repeat(60));
        println!("Search interrupted by user before processing.");
        println!("Files discovered: {}", total_files);
        println!("{}", "=".repeat(60));
        std::process::exit(130);
    }

    if total_files == 0 {
        println!("No files found.");
        return;
    }

    // Set up progress bar
    let progress = ProgressBar::new(total_files as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})")
            .unwrap()
            .progress_chars("=>-"),
    );

    // Counters for statistics
    let files_processed = Arc::new(AtomicUsize::new(0));
    let files_matched = Arc::new(AtomicUsize::new(0));
    let total_assets = Arc::new(AtomicUsize::new(0));
    let all_missing_assets = Arc::new(Mutex::new(HashSet::new()));
    let errors = Arc::new(AtomicUsize::new(0));

    let show_snippets = args.show_snippets && !list_assets;
    let snippet_chars = args.snippet_chars;

    // For search mode, capture the required search text once
    let search_text_for_search_mode = required_search_text.clone();
    let asset_filter = search_text_opt.clone();
    let fix_mode = args.fix;

    // Search files in parallel with early-exit on Ctrl+C
    let interrupted_clone = Arc::clone(&interrupted);
    let search_result: Result<(), ()> = target_files.par_iter().try_for_each(|path| {
        if interrupted_clone.load(Ordering::SeqCst) {
            return Err(());
        }

        let files_processed = Arc::clone(&files_processed);
        let files_matched = Arc::clone(&files_matched);
        let total_assets = Arc::clone(&total_assets);
        let all_missing_assets = Arc::clone(&all_missing_assets);
        let errors = Arc::clone(&errors);

        if list_assets {
            match extract_assets_from_prproj(path, max_file_size_bytes) {
                Ok(mut assets) => {
                    // Optional filter by substring (case-insensitive)

                    if let Some(ref filt) = asset_filter {
                        let needle = filt.to_lowercase();
                        assets.retain(|a| a.path.to_lowercase().contains(&needle));
                    }
                    if !assets.is_empty() {
                        let mut missing_for_this_project = Vec::new();
                        println!("\nProject: {}", path.display());
                        for a in &assets {
                            let status = if a.found {
                                "[FOUND]  "
                            } else {
                                "[MISSING]"
                            };
                            if !a.found {
                                missing_for_this_project.push(a.path.clone());
                                all_missing_assets.lock().unwrap().insert(a.path.clone());
                            }
                            println!("  - {} {}", status, a.path);
                        }
                        total_assets.fetch_add(assets.len(), Ordering::Relaxed);
                        files_matched.fetch_add(1, Ordering::Relaxed);

                        // --- FIXING LOGIC ---
                        if fix_mode && !missing_for_this_project.is_empty() {
                            println!("--- Attempting to fix {} missing assets for {} ---", missing_for_this_project.len(), path.display());
                            let mut corrections = HashMap::new();
                            for missing_path_str in &missing_for_this_project {
                                if let Some(file_name) = Path::new(missing_path_str).file_name().and_then(|n| n.to_str()) {
                                    if let Some(locations) = file_map.get(&file_name.to_lowercase()) {
                                        if let Some(best_location) = locations.first() {
                                            println!("  - Relinking '{}' to '{}'", missing_path_str, best_location.display());
                                            corrections.insert(missing_path_str.clone(), best_location.to_string_lossy().to_string());
                                        }
                                    }
                                }
                            }

                            if !corrections.is_empty() {
                                match rewrite_prproj(path, &corrections) {
                                    Ok(_) => println!("  --- Successfully rewrote project file. ---"),
                                    Err(e) => eprintln!("  --- ERROR: Failed to rewrite project file: {} ---", e),
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else if show_snippets {
            let st = search_text_for_search_mode.as_ref().expect("search text");
            match file_snippet_case_insensitive(path, st, max_file_size_bytes, snippet_chars) {
                Ok(Some(snippet)) => {
                    println!("\n✓ MATCH: {}", path.display());
                    println!("    {}", snippet);
                    files_matched.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => {}
                Err(_) => {
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        } else {
            let st = search_text_for_search_mode.as_ref().expect("search text");
            match file_contains_case_insensitive(path, st, max_file_size_bytes) {
                Ok(true) => {
                    // Print match immediately
                    println!("\n✓ MATCH: {}", path.display());
                    files_matched.fetch_add(1, Ordering::Relaxed);
                }
                Ok(false) => {}
                Err(_) => {
                    // Silently skip files that can't be read (permissions, binary files, etc.)
                    errors.fetch_add(1, Ordering::Relaxed);
                }
            }
        }

        files_processed.fetch_add(1, Ordering::Relaxed);
        progress.inc(1);

        if interrupted_clone.load(Ordering::SeqCst) {
            Err(())
        } else {
            Ok(())
        }
    });

    progress.finish_and_clear();

    let was_interrupted = interrupted.load(Ordering::SeqCst) || search_result.is_err();

    // Print summary
    println!("\n{}", "=".repeat(60));
    if was_interrupted {
        println!("Search interrupted by user (partial results):");
    } else {
        println!("Search complete!");
    }
    println!(
        "Files processed: {}",
        files_processed.load(Ordering::Relaxed)
    );
    if list_assets {
        println!("Projects with listed assets: {}", files_matched.load(Ordering::Relaxed));

        println!("Total assets listed: {}", total_assets.load(Ordering::Relaxed));
    } else {
        println!("Matches found: {}", files_matched.load(Ordering::Relaxed));
    }

    let error_count = errors.load(Ordering::Relaxed);
    if error_count > 0 {
        println!("Files skipped (errors): {}", error_count);
    }
    println!("{}", "=".repeat(60));

    // --- FIX MISSING ASSETS ---
    let missing_assets_to_find: Vec<String> = all_missing_assets.lock().unwrap().iter().cloned().collect();
    if !missing_assets_to_find.is_empty() {
        println!("\nFound {} unique missing assets. Cross-referencing with discovered files...", missing_assets_to_find.len());

        println!("\n--- Missing Asset Report ---");
        // let file_map = file_map.lock().unwrap();

        for missing_path in &missing_assets_to_find {
            println!("\nMISSING: {}", missing_path);

            if let Some(file_name) = Path::new(missing_path).file_name().and_then(|n| n.to_str()) {
                if let Some(locations) = file_map.get(&file_name.to_lowercase()) {
                    for loc in locations {
                        println!("  -> FOUND AT: {}", loc.display());
                    }
                } else {
                    println!("  -> MIA (Missing In Action)");
                }
            } else {
                println!("  -> MIA (Missing In Action)");
            }
        }
        println!("\n{}", "=".repeat(60));
    }


    if was_interrupted {
        // Use 130 as a conventional exit code for Ctrl+C
        std::process::exit(130);
    }
}

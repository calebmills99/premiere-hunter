use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering, AtomicBool};
use std::sync::Arc;
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
    /// Text to search for (case-insensitive)
    #[arg(value_name = "SEARCH_TEXT")]
    search_text: Option<String>,

    /// Paths to search (defaults to C:\ and D:\ on Windows)
    #[arg(short, long, value_delimiter = ',')]
    paths: Option<Vec<PathBuf>>,

    /// Include all common fixed drives (e.g., C:\\ and D:\\) in the search roots
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
    xml_unescape(&v)
}

fn extract_assets_from_prproj(path: &Path, max_size_bytes: Option<usize>) -> Result<Vec<String>, std::io::Error> {
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
    let mut assets: Vec<String> = Vec::new();

    let asset_exts: HashSet<&'static str> = [
        "mp4","mov","mxf","mts","m2ts","avi","mkv","wmv","m4v","3gp",
        "wav","mp3","aac","m4a","aif","aiff","flac","ogg",
        "png","jpg","jpeg","tif","tiff","bmp","gif","psd","ai","svg","dng","cr2","nef","arw",
        "prfpset","mogrt"
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
                                        assets.push(norm);
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
                                        assets.push(norm);
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
                                assets.push(norm);
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

    assets.sort();
    Ok(assets)
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
        .search_text
        .or_else(|| config.as_ref().and_then(|c| c.search_text.clone()));

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
    let auto_drives = args.auto_drives || config.as_ref().and_then(|c| c.auto_drives).unwrap_or(false);
    if auto_drives {
        let candidates = [PathBuf::from("C:\\"), PathBuf::from("D:\\")];
        let mut added_any = false;
        for p in candidates.iter() {
            if p.exists() {
                search_paths.push(p.clone());
                added_any = true;
            }
        }
        if added_any {
            source_parts.push("auto");
        }
    }

    if search_paths.is_empty() {
        search_paths = vec![PathBuf::from("C:\\"), PathBuf::from("D:\\")];
        source_parts.push("defaults");
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

    // Default max file size to 100 MB unless explicitly set to 0 (which disables the limit)
    let max_file_size_mb = config
        .as_ref()
        .and_then(|c| c.max_file_size_mb)
        .and_then(|mb| if mb == 0 { None } else { Some(mb) })
        .or(Some(100));

    let max_file_size_bytes = max_file_size_mb.map(|mb| mb * 1024 * 1024);

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

    // Collect all matching files first
    let mut target_files = Vec::new();
    for path in &search_paths {
        if interrupted.load(Ordering::SeqCst) {
            break;
        }
        if !path.exists() {
            eprintln!("Warning: Path does not exist: {:?}", path);
            continue;
        }

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
                if let Some(ext) = entry.path().extension() {
                    if extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
                        // Check file size if limit is set
                        if let Some(max_bytes) = max_file_size_bytes {
                            if let Ok(metadata) = entry.metadata() {
                                if metadata.len() > max_bytes as u64 {
                                    continue; // Skip files that are too large
                                }
                            }
                        }
                        target_files.push(entry.path().to_path_buf());
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
    let errors = Arc::new(AtomicUsize::new(0));

    let show_snippets = args.show_snippets && !list_assets;
    let snippet_chars = args.snippet_chars;

    // For search mode, capture the required search text once
    let search_text_for_search_mode = required_search_text.clone();
    let asset_filter = search_text_opt.clone();

    // Search files in parallel with early-exit on Ctrl+C
    let interrupted_clone = Arc::clone(&interrupted);
    let search_result: Result<(), ()> = target_files.par_iter().try_for_each(|path| {
        if interrupted_clone.load(Ordering::SeqCst) {
            return Err(());
        }

        let files_processed = Arc::clone(&files_processed);
        let files_matched = Arc::clone(&files_matched);
        let total_assets = Arc::clone(&total_assets);
        let errors = Arc::clone(&errors);

        if list_assets {
            match extract_assets_from_prproj(path, max_file_size_bytes) {
                Ok(mut assets) => {
                    // Optional filter by substring (case-insensitive)
                    if let Some(ref filt) = asset_filter {
                        let needle = filt.to_ascii_lowercase();
                        assets.retain(|a| a.to_ascii_lowercase().contains(&needle));
                    }
                    if !assets.is_empty() {
                        println!("\nProject: {}", path.display());
                        for a in &assets {
                            println!("  - {}", a);
                        }
                        total_assets.fetch_add(assets.len(), Ordering::Relaxed);
                        files_matched.fetch_add(1, Ordering::Relaxed);
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

    if was_interrupted {
        // Use 130 as a conventional exit code for Ctrl+C
        std::process::exit(130);
    }
}

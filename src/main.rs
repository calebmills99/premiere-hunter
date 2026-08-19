mod assets;
mod paths;
mod prproj;
mod relink;
mod rewrite;
mod scan;
mod search;

use assets::extract_assets_from_prproj;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use paths::expand_env;
use rayon::prelude::*;
use relink::pick_best_location;
use rewrite::rewrite_prproj;
use scan::{
    default_search_paths, enumerate_search_drives, get_cache_path, load_cache, load_config,
    merge_excludes, save_cache, scan_files, FileConfig,
};
use search::{file_contains_case_insensitive, file_snippet_case_insensitive};
use std::collections::{HashMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Parser, Debug)]
#[command(name = "premiere-hunter")]
#[command(version)]
#[command(about = "Fast parallel search and media relink for Premiere Pro .prproj files")]
struct Args {
    /// Text to search for (case-insensitive). With --list-assets, filters listed assets.
    #[arg(index = 1)]
    search_pos: Option<String>,

    /// Same as the positional search text.
    #[arg(short = 's', long = "search")]
    search_flag: Option<String>,

    /// Directories or .prproj files to search. Comma-separated or repeated.
    #[arg(short, long, value_delimiter = ',')]
    paths: Option<Vec<PathBuf>>,

    /// Deprecated alias for --search-all-drives.
    #[arg(long, default_value_t = false, hide = true)]
    auto_drives: bool,

    /// Number of threads (defaults to CPU cores).
    #[arg(short, long)]
    threads: Option<usize>,

    /// YAML configuration file.
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// List media referenced by each .prproj instead of free-text search.
    #[arg(long, default_value_t = false)]
    list_assets: bool,

    /// Print a text snippet around each search match.
    #[arg(long, default_value_t = false)]
    show_snippets: bool,

    /// Max characters in each snippet.
    #[arg(long, default_value_t = 120)]
    snippet_chars: usize,

    /// Search all local drives (Windows) or home + mounted volumes (Unix).
    #[arg(long, default_value_t = false)]
    search_all_drives: bool,

    /// Ignore and overwrite the file-name cache used for relinking.
    #[arg(long, default_value_t = false)]
    rescan: bool,

    /// With --list-assets, relink missing media by rewriting the .prproj.
    #[arg(
        long,
        default_value_t = false,
        requires = "list_assets",
        aliases = ["fix-missing", "relink"]
    )]
    fix: bool,

    /// Show what --fix would change without writing the project.
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Skip writing a .prproj.bak next to the project before --fix.
    #[arg(long, default_value_t = false)]
    no_backup: bool,
}

fn search_text_from_args(args: &Args, config: &Option<FileConfig>) -> Option<String> {
    args.search_flag
        .clone()
        .or_else(|| args.search_pos.clone())
        .or_else(|| config.as_ref().and_then(|c| c.search_text.clone()))
}

fn prompt_search_text() -> String {
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
            }
            trimmed
        }
        Err(e) => {
            eprintln!("Error reading input: {}", e);
            std::process::exit(1);
        }
    }
}

fn merge_paths(args: &Args, config: &Option<FileConfig>) -> (Vec<PathBuf>, String) {
    let mut search_paths: Vec<PathBuf> = Vec::new();
    let mut source_parts: Vec<&str> = Vec::new();

    if let Some(cfg) = config.as_ref().and_then(|c| c.paths.as_ref()) {
        if !cfg.is_empty() {
            search_paths.extend(cfg.iter().map(|p| PathBuf::from(expand_env(&p.to_string_lossy()))));
            source_parts.push("config");
        }
    }
    if let Some(ref cli) = args.paths {
        if !cli.is_empty() {
            search_paths.extend(cli.iter().cloned());
            source_parts.push("CLI");
        }
    }

    let search_all_drives =
        args.search_all_drives || args.auto_drives || config.as_ref().and_then(|c| c.auto_drives).unwrap_or(false);
    if search_all_drives {
        let extra = enumerate_search_drives();
        if !extra.is_empty() {
            search_paths.extend(extra);
            source_parts.push("auto");
        }
    }

    if search_paths.is_empty() {
        search_paths = default_search_paths();
        if !search_all_drives {
            source_parts.push("defaults");
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    search_paths.retain(|p| seen.insert(p.to_string_lossy().to_lowercase()));

    let path_source = if source_parts.len() > 1 {
        format!("{} (merged)", source_parts.join("+"))
    } else {
        source_parts.first().copied().unwrap_or("unknown").to_string()
    };
    (search_paths, path_source)
}

fn main() {
    let args = Args::parse();

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

    let mut search_text_opt = search_text_from_args(&args, &config);
    if !args.list_assets && search_text_opt.is_none() {
        search_text_opt = Some(prompt_search_text());
    }

    let threads = args.threads.or_else(|| config.as_ref().and_then(|c| c.threads));
    let (search_paths, path_source) = merge_paths(&args, &config);

    let extensions = config
        .as_ref()
        .map(|c| c.extensions.clone())
        .unwrap_or_else(|| vec!["prproj".to_string()]);
    let follow_links = config.as_ref().map(|c| c.follow_links).unwrap_or(false);
    let max_file_size_mb = config.as_ref().and_then(|c| c.max_file_size_mb);
    let max_file_size_bytes =
        max_file_size_mb.and_then(|mb| if mb == 0 { None } else { Some(mb * 1024 * 1024) });
    let exclude_dirs = merge_excludes(config.as_ref().and_then(|c| c.exclude_dirs.clone()));

    if let Some(n) = threads {
        if n > 0 {
            rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build_global()
                .unwrap();
        }
    }

    let list_assets = args.list_assets;
    if list_assets {
        println!("Listing assets used in Premiere project files");
        if let Some(ref f) = search_text_opt {
            println!("Asset filter (case-insensitive): '{}'", f);
        }
        if args.fix {
            if args.dry_run {
                println!("Fix mode: dry-run (no project files will be written)");
            } else {
                println!("Fix mode: missing media will be relinked in-place");
            }
        }
    } else {
        println!("Searching for: '{}'", search_text_opt.as_ref().expect("search text"));
    }
    println!("Search paths ({}): {:?}", path_source, search_paths);
    println!("Extensions: {:?}", extensions);
    println!("Excluding directories: {:?}", exclude_dirs);
    if let Some(max_mb) = max_file_size_mb {
        println!("Max file size: {} MB", max_mb);
    }
    println!("Scanning for files...\n");

    let interrupted = Arc::new(AtomicBool::new(false));
    {
        let int_flag = Arc::clone(&interrupted);
        if let Err(e) = ctrlc::set_handler(move || {
            if !int_flag.swap(true, Ordering::SeqCst) {
                eprintln!("\nReceived Ctrl+C — stopping early (letting active tasks finish)...");
            }
        }) {
            eprintln!("Warning: failed to set Ctrl+C handler: {}", e);
        }
    }

    let mut explicit_targets: Vec<PathBuf> = Vec::new();
    for p in &search_paths {
        if p.is_file() {
            explicit_targets.push(p.clone());
        }
    }

    if !explicit_targets.is_empty() {
        explicit_targets.retain(|p| {
            let ok_ext = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|ext| extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)))
                .unwrap_or(false);
            if !ok_ext {
                return false;
            }
            if let Some(max_bytes) = max_file_size_bytes {
                if let Ok(md) = std::fs::metadata(p) {
                    if md.len() > max_bytes as u64 {
                        return false;
                    }
                }
            }
            true
        });
        if explicit_targets.is_empty() {
            println!("No files matched filters (extensions/size).");
            return;
        }
        if args.fix {
            println!(
                "Using {} provided file(s) as targets; building file index for relink (fix mode).",
                explicit_targets.len()
            );
        } else {
            println!(
                "Using {} provided file(s); skipping system-wide scan and cache.",
                explicit_targets.len()
            );
        }
    }

    let mut file_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let cache_path_opt = get_cache_path();
    let need_index = args.fix;
    let use_cache = need_index && !args.rescan;

    if use_cache {
        if let Some(ref cache_path) = cache_path_opt {
            if cache_path.exists() {
                println!("Loading file index from cache...");
                match load_cache(cache_path) {
                    Ok(map) => {
                        file_map = map;
                        println!("Cache loaded ({} filenames).", file_map.len());
                    }
                    Err(e) => {
                        eprintln!("Warning: Could not load cache (will rescan): {}", e);
                    }
                }
            }
        }
    }

    let mut discovered_targets: Vec<PathBuf> = Vec::new();
    let should_scan = explicit_targets.is_empty() || (args.fix && file_map.is_empty());
    if should_scan {
        let mut scan_roots: Vec<PathBuf> = search_paths.clone();
        if args.fix && !explicit_targets.is_empty() {
            let mut added = 0usize;
            for f in &explicit_targets {
                if let Some(parent) = f.parent() {
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

        let mut seen_roots: HashSet<String> = HashSet::new();
        scan_roots.retain(|p| {
            if p.is_dir() && p.exists() {
                seen_roots.insert(p.to_string_lossy().to_lowercase())
            } else {
                false
            }
        });

        if !scan_roots.is_empty() {
            let (targets, discovered) = scan_files(
                &scan_roots,
                &extensions,
                &exclude_dirs,
                follow_links,
                need_index && file_map.is_empty(),
                &interrupted,
            );
            discovered_targets = targets;
            if file_map.is_empty() && need_index {
                file_map = discovered;
                if let Some(ref cache_path) = cache_path_opt {
                    println!("Saving file index to cache...");
                    if let Err(e) = save_cache(cache_path, &file_map) {
                        eprintln!("Warning: Could not save cache: {}", e);
                    }
                }
            }
        }
    }

    let mut target_files: Vec<PathBuf> = Vec::new();
    if !explicit_targets.is_empty() {
        target_files = explicit_targets;
    } else if !discovered_targets.is_empty() {
        target_files = discovered_targets;
        if let Some(max_bytes) = max_file_size_bytes {
            target_files.retain(|path| {
                std::fs::metadata(path)
                    .map(|md| md.len() <= max_bytes as u64)
                    .unwrap_or(true)
            });
        }
    } else if !file_map.is_empty() {
        for (name, paths) in &file_map {
            if let Some(ext) = Path::new(name).extension().and_then(|s| s.to_str()) {
                if extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
                    for path in paths {
                        if let Some(max_bytes) = max_file_size_bytes {
                            if let Ok(metadata) = std::fs::metadata(path) {
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

    let mut seen_targets: HashSet<String> = HashSet::new();
    target_files.retain(|p| seen_targets.insert(p.to_string_lossy().to_lowercase()));

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

    let progress = ProgressBar::new(total_files as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} files ({per_sec})")
            .unwrap()
            .progress_chars("=>-"),
    );

    let files_processed = Arc::new(AtomicUsize::new(0));
    let files_matched = Arc::new(AtomicUsize::new(0));
    let total_assets = Arc::new(AtomicUsize::new(0));
    let files_rewritten = Arc::new(AtomicUsize::new(0));
    let all_missing_assets = Arc::new(Mutex::new(HashSet::new()));
    let errors = Arc::new(AtomicUsize::new(0));
    let print_lock = Arc::new(Mutex::new(()));

    let show_snippets = args.show_snippets && !list_assets;
    let snippet_chars = args.snippet_chars;
    let search_text_for_search_mode = search_text_opt.clone();
    let asset_filter = search_text_opt.clone();
    let fix_mode = args.fix;
    let dry_run = args.dry_run;
    let write_backup = !args.no_backup;

    let interrupted_clone = Arc::clone(&interrupted);
    let file_map = Arc::new(file_map);
    let search_result: Result<(), ()> = target_files.par_iter().try_for_each(|path| {
        if interrupted_clone.load(Ordering::SeqCst) {
            return Err(());
        }

        if list_assets {
            match extract_assets_from_prproj(path, max_file_size_bytes) {
                Ok(mut assets) => {
                    if let Some(ref filt) = asset_filter {
                        let needle = filt.to_lowercase();
                        assets.retain(|a| a.path.to_lowercase().contains(&needle));
                    }
                    if !assets.is_empty() {
                        let mut missing_for_this_project = Vec::new();
                        {
                            let _g = print_lock.lock().unwrap();
                            println!("\nProject: {}", path.display());
                            for a in &assets {
                                let status = if a.found { "[FOUND]  " } else { "[MISSING]" };
                                if !a.found {
                                    missing_for_this_project.push(a.path.clone());
                                    all_missing_assets.lock().unwrap().insert(a.path.clone());
                                }
                                println!("  - {} {}", status, a.path);
                            }
                        }
                        total_assets.fetch_add(assets.len(), Ordering::Relaxed);
                        files_matched.fetch_add(1, Ordering::Relaxed);

                        if fix_mode && !missing_for_this_project.is_empty() {
                            let mut corrections = HashMap::new();
                            {
                                let _g = print_lock.lock().unwrap();
                                println!(
                                    "--- Attempting to fix {} missing assets for {} ---",
                                    missing_for_this_project.len(),
                                    path.display()
                                );
                                for missing_path_str in &missing_for_this_project {
                                    if let Some(file_name) =
                                        Path::new(missing_path_str).file_name().and_then(|n| n.to_str())
                                    {
                                        if let Some(locations) = file_map.get(&file_name.to_lowercase()) {
                                            if let Some(best_location) =
                                                pick_best_location(missing_path_str, locations)
                                            {
                                                println!(
                                                    "  - Relinking '{}' → '{}'",
                                                    missing_path_str,
                                                    best_location.display()
                                                );
                                                corrections.insert(
                                                    missing_path_str.clone(),
                                                    best_location.to_string_lossy().to_string(),
                                                );
                                            } else {
                                                println!(
                                                    "  - No on-disk match for '{}'",
                                                    missing_path_str
                                                );
                                            }
                                        } else {
                                            println!(
                                                "  - No indexed filename match for '{}'",
                                                missing_path_str
                                            );
                                        }
                                    }
                                }
                            }

                            if !corrections.is_empty() {
                                if dry_run {
                                    let _g = print_lock.lock().unwrap();
                                    println!(
                                        "  --- Dry-run: would rewrite {} path(s). ---",
                                        corrections.len()
                                    );
                                } else {
                                    match rewrite_prproj(path, &corrections, write_backup) {
                                        Ok(result) => {
                                            files_rewritten.fetch_add(1, Ordering::Relaxed);
                                            let _g = print_lock.lock().unwrap();
                                            if let Some(bak) = result.backup {
                                                println!(
                                                    "  --- Rewrote project ({} replacements). Backup: {} ---",
                                                    result.replacements,
                                                    bak.display()
                                                );
                                            } else {
                                                println!(
                                                    "  --- Rewrote project ({} replacements). ---",
                                                    result.replacements
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            let _g = print_lock.lock().unwrap();
                                            eprintln!(
                                                "  --- ERROR: Failed to rewrite project file: {} ---",
                                                e
                                            );
                                        }
                                    }
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
                    let _g = print_lock.lock().unwrap();
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
                    let _g = print_lock.lock().unwrap();
                    println!("\n✓ MATCH: {}", path.display());
                    files_matched.fetch_add(1, Ordering::Relaxed);
                }
                Ok(false) => {}
                Err(_) => {
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
        println!(
            "Projects with listed assets: {}",
            files_matched.load(Ordering::Relaxed)
        );
        println!(
            "Total assets listed: {}",
            total_assets.load(Ordering::Relaxed)
        );
        let rewritten = files_rewritten.load(Ordering::Relaxed);
        if fix_mode && rewritten > 0 {
            println!("Projects rewritten: {}", rewritten);
        }
    } else {
        println!("Matches found: {}", files_matched.load(Ordering::Relaxed));
    }

    let error_count = errors.load(Ordering::Relaxed);
    if error_count > 0 {
        println!("Files skipped (errors): {}", error_count);
    }
    println!("{}", "=".repeat(60));

    let missing_assets_to_find: Vec<String> =
        all_missing_assets.lock().unwrap().iter().cloned().collect();
    if !missing_assets_to_find.is_empty() {
        println!(
            "\nFound {} unique missing assets. Cross-referencing with discovered files...",
            missing_assets_to_find.len()
        );
        println!("\n--- Missing Asset Report ---");
        for missing_path in &missing_assets_to_find {
            println!("\nMISSING: {}", missing_path);
            if let Some(file_name) = Path::new(missing_path).file_name().and_then(|n| n.to_str()) {
                if let Some(locations) = file_map.get(&file_name.to_lowercase()) {
                    let existing: Vec<_> = locations.iter().filter(|p| p.exists()).collect();
                    if existing.is_empty() {
                        println!("  -> MIA (Missing In Action)");
                    } else {
                        for loc in existing {
                            println!("  -> FOUND AT: {}", loc.display());
                        }
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
        std::process::exit(130);
    }
}

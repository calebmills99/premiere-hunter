use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use walkdir::WalkDir;

#[derive(Parser, Debug)]
#[command(name = "premiere-hunter")]
#[command(about = "Fast parallel search for text in Premiere Pro project files", long_about = None)]
struct Args {
    /// Text to search for (case-insensitive)
    #[arg(value_name = "SEARCH_TEXT")]
    search_text: String,

    /// Paths to search (defaults to C:\ and D:\ on Windows)
    #[arg(short, long, value_delimiter = ',')]
    paths: Option<Vec<PathBuf>>,

    /// Number of threads to use (defaults to number of CPU cores)
    #[arg(short, long)]
    threads: Option<usize>,
}

fn main() {
    let args = Args::parse();

    // Set up thread pool
    if let Some(threads) = args.threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global()
            .unwrap();
    }

    // Default to C:\ and D:\ drives on Windows
    let search_paths = args.paths.unwrap_or_else(|| {
        vec![
            PathBuf::from("C:\\"),
            PathBuf::from("D:\\"),
        ]
    });

    let search_text_lower = args.search_text.to_lowercase();

    println!("Searching for: '{}'", args.search_text);
    println!("Search paths: {:?}", search_paths);
    println!("Scanning for .prproj files...\n");

    // Collect all .prproj files first
    let mut prproj_files = Vec::new();
    for path in &search_paths {
        if !path.exists() {
            eprintln!("Warning: Path does not exist: {:?}", path);
            continue;
        }

        for entry in WalkDir::new(path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                if let Some(ext) = entry.path().extension() {
                    if ext.eq_ignore_ascii_case("prproj") {
                        prproj_files.push(entry.path().to_path_buf());
                    }
                }
            }
        }
    }

    let total_files = prproj_files.len();
    println!("Found {} .prproj files to search\n", total_files);

    if total_files == 0 {
        println!("No .prproj files found.");
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
    let errors = Arc::new(AtomicUsize::new(0));

    // Search files in parallel
    prproj_files.par_iter().for_each(|path| {
        let files_processed = Arc::clone(&files_processed);
        let files_matched = Arc::clone(&files_matched);
        let errors = Arc::clone(&errors);

        // Try to read and search the file
        match fs::read_to_string(path) {
            Ok(contents) => {
                if contents.to_lowercase().contains(&search_text_lower) {
                    // Print match immediately
                    println!("\n✓ MATCH: {}", path.display());
                    files_matched.fetch_add(1, Ordering::Relaxed);
                }
            }
            Err(_) => {
                // Silently skip files that can't be read (permissions, binary files, etc.)
                errors.fetch_add(1, Ordering::Relaxed);
            }
        }

        files_processed.fetch_add(1, Ordering::Relaxed);
        progress.inc(1);
    });

    progress.finish_and_clear();

    // Print summary
    println!("\n{}", "=".repeat(60));
    println!("Search complete!");
    println!("Files processed: {}", files_processed.load(Ordering::Relaxed));
    println!("Matches found: {}", files_matched.load(Ordering::Relaxed));

    let error_count = errors.load(Ordering::Relaxed);
    if error_count > 0 {
        println!("Files skipped (errors): {}", error_count);
    }
    println!("{}", "=".repeat(60));
}

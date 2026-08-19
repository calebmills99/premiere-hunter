use crate::paths::default_exclude_dirs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use indicatif::{ProgressBar, ProgressStyle};
use walkdir::{DirEntry, WalkDir};

pub fn get_cache_path() -> Option<PathBuf> {
    dirs::home_dir().map(|mut path| {
        path.push(".premiere-hunter");
        path.push("file_cache.json");
        path
    })
}

pub fn load_cache(path: &Path) -> Result<HashMap<String, Vec<PathBuf>>, Box<dyn std::error::Error>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let map = serde_json::from_reader(reader)?;
    Ok(map)
}

pub fn save_cache(path: &Path, map: &HashMap<String, Vec<PathBuf>>) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = fs::File::create(path)?;
    let mut writer = std::io::BufWriter::new(file);
    serde_json::to_writer(&mut writer, map)?;
    writer.flush()?;
    Ok(())
}

pub fn is_excluded_dir(entry: &DirEntry, exclude_dirs: &[String]) -> bool {
    if let Some(name) = entry.file_name().to_str() {
        return exclude_dirs.iter().any(|exc| name.eq_ignore_ascii_case(exc));
    }
    false
}

pub fn merge_excludes(from_config: Option<Vec<String>>) -> Vec<String> {
    let mut out = default_exclude_dirs();
    if let Some(extra) = from_config {
        for e in extra {
            if !out.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
                out.push(e);
            }
        }
    }
    out
}

/// Collect files under `roots`. If `index_all` is true, every file is indexed by lowercase name
/// (needed for relink). Otherwise only files matching `extensions` are returned as targets.
pub fn scan_files(
    roots: &[PathBuf],
    extensions: &[String],
    exclude_dirs: &[String],
    follow_links: bool,
    index_all: bool,
    interrupted: &AtomicBool,
) -> (Vec<PathBuf>, HashMap<String, Vec<PathBuf>>) {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(Duration::from_millis(80));
    spinner.set_message("Scanning…");

    let mut file_map: HashMap<String, Vec<PathBuf>> = HashMap::new();
    let mut targets: Vec<PathBuf> = Vec::new();
    let mut seen_files: usize = 0;

    for path in roots {
        if interrupted.load(Ordering::SeqCst) {
            break;
        }
        if !path.exists() {
            eprintln!("Warning: Path does not exist: {:?}", path);
            continue;
        }
        if path.is_file() {
            continue;
        }

        for entry in WalkDir::new(path)
            .follow_links(follow_links)
            .into_iter()
            .filter_entry(|e| !is_excluded_dir(e, exclude_dirs))
            .filter_map(|e| e.ok())
        {
            if interrupted.load(Ordering::SeqCst) {
                break;
            }
            if !entry.file_type().is_file() {
                continue;
            }
            seen_files += 1;
            if seen_files % 500 == 0 {
                spinner.set_message(format!("Scanning… {} files seen", seen_files));
            }

            let name = match entry.file_name().to_str() {
                Some(n) => n,
                None => continue,
            };
            let path_buf = entry.path().to_path_buf();

            if index_all {
                file_map
                    .entry(name.to_lowercase())
                    .or_default()
                    .push(path_buf.clone());
            }

            if let Some(ext) = Path::new(name).extension().and_then(|s| s.to_str()) {
                if extensions.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
                    targets.push(path_buf);
                }
            }
        }
    }

    spinner.finish_and_clear();
    println!("Indexed {} files ({} project files).", seen_files, targets.len());
    (targets, file_map)
}

pub fn enumerate_search_drives() -> Vec<PathBuf> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        for letter in 'A'..='Z' {
            let drive = PathBuf::from(format!("{}:\\", letter));
            if drive.exists() {
                out.push(drive);
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(home) = dirs::home_dir() {
            out.push(home);
        }
        for extra in ["/mnt", "/media", "/run/media"] {
            let p = PathBuf::from(extra);
            if p.exists() {
                out.push(p);
            }
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if !out.iter().any(|p| p == &cwd) {
            out.push(cwd);
        }
    }
    out
}

pub fn default_search_paths() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        let mut v = Vec::new();
        for letter in ['C', 'D'] {
            let p = PathBuf::from(format!("{}:\\", letter));
            if p.exists() {
                v.push(p);
            }
        }
        if v.is_empty() {
            v.push(PathBuf::from("."));
        }
        v
    }
    #[cfg(not(windows))]
    {
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FileConfig {
    pub search_text: Option<String>,
    pub paths: Option<Vec<PathBuf>>,
    pub threads: Option<usize>,
    pub auto_drives: Option<bool>,
    #[serde(default = "default_extensions")]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub follow_links: bool,
    pub max_file_size_mb: Option<usize>,
    pub exclude_dirs: Option<Vec<String>>,
}

pub fn default_extensions() -> Vec<String> {
    vec!["prproj".to_string()]
}

pub fn load_config(path: &PathBuf) -> Result<FileConfig, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let mut config: FileConfig = serde_yaml::from_str(&content)?;
    if let Some(ref mut paths) = config.paths {
        for p in paths.iter_mut() {
            *p = PathBuf::from(crate::paths::expand_env(&p.to_string_lossy()));
        }
    }
    if let Some(ref mut excludes) = config.exclude_dirs {
        for e in excludes.iter_mut() {
            *e = crate::paths::expand_env(e);
        }
    }
    Ok(config)
}

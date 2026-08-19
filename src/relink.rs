use crate::paths::drive_letter;
use std::path::{Path, PathBuf};

/// Pick the most plausible on-disk stand-in for a missing Premiere media file.
///
/// Prefers: matching parent folders, same drive, not Adobe auto-save/preview junk.
pub fn pick_best_location<'a>(missing: &str, candidates: &'a [PathBuf]) -> Option<&'a PathBuf> {
    let missing_path = Path::new(missing);
    let missing_comps: Vec<String> = missing_path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
        .collect();
    let missing_drive = drive_letter(missing_path);
    let missing_parent = missing_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_ascii_lowercase());

    let mut best: Option<(i32, &'a PathBuf)> = None;
    for cand in candidates {
        if !cand.exists() || !cand.is_file() {
            continue;
        }
        let mut score = 0i32;
        let cand_comps: Vec<String> = cand
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect();

        let mut matched = 0;
        for (a, b) in missing_comps.iter().rev().zip(cand_comps.iter().rev()) {
            if a == b {
                matched += 1;
            } else {
                break;
            }
        }
        score += matched * 12;

        if let (Some(d1), Some(d2)) = (missing_drive, drive_letter(cand)) {
            if d1.eq_ignore_ascii_case(&d2) {
                score += 6;
            }
        }

        if let Some(ref parent) = missing_parent {
            if cand
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().eq_ignore_ascii_case(parent))
                .unwrap_or(false)
            {
                score += 8;
            }
        }

        let s = cand.to_string_lossy().to_ascii_lowercase();
        if s.contains("auto-save")
            || s.contains("captured and generated")
            || s.contains("adobe premiere pro preview files")
            || s.contains("peak files")
        {
            score -= 25;
        }

        if cand.as_path() == missing_path {
            continue;
        }

        match best {
            Some((best_score, _)) if score <= best_score => {}
            _ => best = Some((score, cand)),
        }
    }
    best.map(|(_, p)| p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn prefers_matching_parent_over_autosave() {
        let dir = std::env::temp_dir().join(format!("ph-relink-{}", std::process::id()));
        let good = dir.join("Copied_Chapter One BHIFF").join("newplane.mp4");
        let autosave = dir
            .join("Adobe Premiere Pro Auto-Save")
            .join("newplane.mp4");
        fs::create_dir_all(good.parent().unwrap()).unwrap();
        fs::create_dir_all(autosave.parent().unwrap()).unwrap();
        fs::write(&good, b"a").unwrap();
        fs::write(&autosave, b"b").unwrap();

        let missing = r"D:\gdrive\Copied_Chapter One BHIFF\newplane.mp4";
        let cands = vec![autosave.clone(), good.clone()];
        let picked = pick_best_location(missing, &cands).unwrap();
        assert_eq!(picked, &good);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_nonexistent_candidates() {
        let missing = r"D:\nope\clip.mp4";
        let cands = vec![PathBuf::from(r"/definitely/not/here/clip.mp4")];
        assert!(pick_best_location(missing, &cands).is_none());
    }
}

use crate::paths::{format_path_like, path_search_variants};
use crate::prproj::{read_prproj_xml, write_prproj_xml};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct RewriteResult {
    pub replacements: usize,
    pub backup: Option<PathBuf>,
}

pub fn backup_project(project_path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let backup = if project_path.with_extension("prproj.bak").exists() {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        project_path.with_extension(format!("prproj.bak-{}", ts))
    } else {
        project_path.with_extension("prproj.bak")
    };
    fs::copy(project_path, &backup)?;
    Ok(backup)
}

/// Rewrite missing asset paths inside a .prproj by replacing the stored path strings.
/// Premiere keeps media locations as XML text (`<FilePath>`, `<ActualMediaFilePath>`),
/// and often repeats the same absolute path inside `<RelativePath>`.
pub fn rewrite_prproj(
    project_path: &Path,
    corrections: &HashMap<String, String>,
    write_backup: bool,
) -> Result<RewriteResult, Box<dyn std::error::Error>> {
    if corrections.is_empty() {
        return Ok(RewriteResult {
            replacements: 0,
            backup: None,
        });
    }

    let backup = if write_backup {
        Some(backup_project(project_path)?)
    } else {
        None
    };

    let (xml_bytes, is_gzipped) = read_prproj_xml(project_path)?;
    let (rewritten, replacements) = rewrite_xml_bytes(&xml_bytes, corrections);
    write_prproj_xml(project_path, &rewritten, is_gzipped)?;
    Ok(RewriteResult {
        replacements,
        backup,
    })
}

pub fn rewrite_xml_bytes(xml_bytes: &[u8], corrections: &HashMap<String, String>) -> (Vec<u8>, usize) {
    let mut xml_str = String::from_utf8_lossy(xml_bytes).into_owned();
    let mut replacements = 0usize;

    let mut pairs: Vec<(&String, &String)> = corrections.iter().collect();
    pairs.sort_by_key(|(old, _)| std::cmp::Reverse(old.len()));

    for (old, new) in pairs {
        let new_path = Path::new(new);
        for variant in path_search_variants(old) {
            if variant.is_empty() {
                continue;
            }
            let replacement = format_path_like(&variant, new_path);
            if variant == replacement {
                continue;
            }
            let count = xml_str.matches(&variant).count();
            if count > 0 {
                xml_str = xml_str.replace(&variant, &replacement);
                replacements += count;
            }
        }
    }
    (xml_str.into_bytes(), replacements)
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    fn sample_xml(old: &str) -> String {
        format!(
            r#"<?xml version="1.0"?>
<PremiereData>
  <FilePath>{old}</FilePath>
  <ActualMediaFilePath>{old}</ActualMediaFilePath>
  <RelativePath>..\{old}</RelativePath>
</PremiereData>"#
        )
    }

    #[test]
    fn rewrites_text_nodes_and_relative_embed() {
        let dir = std::env::temp_dir().join(format!("ph-rewrite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("proj.prproj");
        let old = r"D:\missing\clip.mp4";
        let xml = sample_xml(old);
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(xml.as_bytes()).unwrap();
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();

        let new = dir.join("found").join("clip.mp4");
        std::fs::create_dir_all(new.parent().unwrap()).unwrap();
        std::fs::write(&new, b"media").unwrap();

        let mut corrections = HashMap::new();
        corrections.insert(old.to_string(), new.to_string_lossy().replace('/', "\\"));
        let result = rewrite_prproj(&path, &corrections, true).unwrap();
        assert!(result.replacements >= 3, "replacements={}", result.replacements);
        assert!(result.backup.unwrap().exists());

        let (out, gzipped) = read_prproj_xml(&path).unwrap();
        assert!(gzipped);
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains(old), "old path should be gone: {text}");
        assert!(text.contains("clip.mp4"));
        assert!(text.contains("<ActualMediaFilePath>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rewrite_xml_bytes_hits_all_copies() {
        let old = r"D:\missing\clip.mp4";
        let xml = sample_xml(old);
        let mut corrections = HashMap::new();
        corrections.insert(old.to_string(), r"E:\found\clip.mp4".to_string());
        let (out, n) = rewrite_xml_bytes(xml.as_bytes(), &corrections);
        let text = String::from_utf8_lossy(&out);
        assert!(n >= 3);
        assert_eq!(text.matches(old).count(), 0);
        assert!(text.contains(r"E:\found\clip.mp4"));
        assert!(text.contains(r"..\E:\found\clip.mp4"));
    }

    #[test]
    fn rewrite_keeps_xml_escaping_for_ampersands() {
        let old = r"D:\clips\Tom & Jerry\old.mp4";
        let xml = sample_xml(r"D:\clips\Tom &amp; Jerry\old.mp4");
        let mut corrections = HashMap::new();
        corrections.insert(old.to_string(), r"E:\found\Tom & Jerry\new.mp4".to_string());
        let (out, n) = rewrite_xml_bytes(xml.as_bytes(), &corrections);
        let text = String::from_utf8_lossy(&out);
        assert!(n >= 1);
        assert!(text.contains(r"Tom &amp; Jerry\new.mp4"), "{text}");
        assert!(!text.contains("Tom & Jerry"), "{text}");
    }
}

use crate::paths::{is_media_extension, is_path_tag_name, normalize_asset_path};
use crate::prproj::{open_maybe_gzip, skip_oversize};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashSet;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct AssetInfo {
    /// Path as stored after light normalization (drive-rooted, unescaped).
    pub path: String,
    pub found: bool,
}

pub fn extract_assets_from_prproj(
    path: &Path,
    max_size_bytes: Option<usize>,
) -> Result<Vec<AssetInfo>, std::io::Error> {
    if skip_oversize(path, max_size_bytes)? {
        return Ok(Vec::new());
    }

    let mut buf_reader = BufReader::new(open_maybe_gzip(path)?);
    let mut bytes = Vec::new();
    buf_reader.read_to_end(&mut bytes)?;

    let mut seen: HashSet<String> = HashSet::new();
    let mut assets: Vec<AssetInfo> = Vec::new();

    let mut push_if_asset = |raw: &str, assets: &mut Vec<AssetInfo>| {
        let norm = normalize_asset_path(raw);
        if norm.trim().is_empty() {
            return;
        }
        let ext = Path::new(&norm)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        if !is_media_extension(ext) {
            return;
        }
        let key = norm.to_lowercase();
        if seen.insert(key) {
            let found = PathBuf::from(&norm).exists();
            assets.push(AssetInfo { path: norm, found });
        }
    };

    let mut reader = Reader::from_reader(bytes.as_slice());
    reader.trim_text(true);
    let mut buf = Vec::new();
    let mut want_text = false;

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                want_text = is_path_tag_name(&name);
                for a in e.attributes().with_checks(false) {
                    if let Ok(attr) = a {
                        let key = String::from_utf8_lossy(attr.key.as_ref());
                        if is_path_tag_name(&key) {
                            if let Ok(val) = attr.unescape_value() {
                                push_if_asset(&val, &mut assets);
                            }
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                for a in e.attributes().with_checks(false) {
                    if let Ok(attr) = a {
                        let key = String::from_utf8_lossy(attr.key.as_ref());
                        if is_path_tag_name(&key) {
                            if let Ok(val) = attr.unescape_value() {
                                push_if_asset(&val, &mut assets);
                            }
                        }
                    }
                }
                want_text = false;
            }
            Ok(Event::Text(t)) => {
                if want_text {
                    if let Ok(val) = t.unescape() {
                        push_if_asset(&val, &mut assets);
                    }
                }
            }
            Ok(Event::End(_)) => {
                want_text = false;
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
        buf.clear();
    }

    assets.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(assets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_filepath_and_actualmediafilepath() {
        let dir = std::env::temp_dir().join(format!("ph-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("proj.prproj");
        let xml = r#"<?xml version="1.0"?>
<PremiereData>
  <FilePath>D:\media\interview.mp4</FilePath>
  <ActualMediaFilePath>D:\media\interview.mp4</ActualMediaFilePath>
  <RelativePath>..\D:\media\interview.mp4</RelativePath>
  <FilePath>E:\stills\still.jpg</FilePath>
</PremiereData>"#;
        std::fs::write(&path, xml).unwrap();
        let assets = extract_assets_from_prproj(&path, None).unwrap();
        let paths: Vec<_> = assets.iter().map(|a| a.path.as_str()).collect();
        assert!(paths.contains(&r"D:\media\interview.mp4"));
        assert!(paths.contains(&r"E:\stills\still.jpg"));
        assert_eq!(paths.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

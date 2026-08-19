use std::path::Path;

pub fn xml_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

pub fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub fn is_path_tag_name(n: &str) -> bool {
    matches!(
        n.to_ascii_lowercase().as_str(),
        "absolutepath"
            | "filepath"
            | "path"
            | "relativepath"
            | "relpath"
            | "actualmediafilepath"
            | "pathname"
            | "filepathname"
    )
}

pub fn is_media_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mp4" | "mov" | "mxf" | "mts" | "m2ts" | "avi" | "mkv" | "wmv" | "m4v" | "3gp"
            | "mpg" | "mpeg" | "m2v" | "ts" | "vob" | "r3d" | "braw" | "ari"
            | "wav" | "mp3" | "aac" | "m4a" | "aif" | "aiff" | "flac" | "ogg" | "wma"
            | "png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "psd" | "ai"
            | "svg" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "orf" | "rw2" | "heic"
            | "prfpset" | "mogrt" | "aep" | "aepx"
    )
}

/// Expand `%VAR%`, `$VAR`, and `${VAR}` in config strings.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '%' {
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == '%') {
                let name: String = chars[i + 1..i + 1 + end].iter().collect();
                if !name.is_empty() {
                    if let Ok(val) = std::env::var(&name) {
                        out.push_str(&val);
                        i += end + 2;
                        continue;
                    }
                }
            }
        }
        if chars[i] == '$' {
            if i + 1 < chars.len() && chars[i + 1] == '{' {
                if let Some(end) = chars[i + 2..].iter().position(|&c| c == '}') {
                    let name: String = chars[i + 2..i + 2 + end].iter().collect();
                    if let Ok(val) = std::env::var(&name) {
                        out.push_str(&val);
                        i += end + 3;
                        continue;
                    }
                }
            } else {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && (chars[end].is_ascii_alphanumeric() || chars[end] == '_') {
                    end += 1;
                }
                if end > start {
                    let name: String = chars[start..end].iter().collect();
                    if let Ok(val) = std::env::var(&name) {
                        out.push_str(&val);
                        i = end;
                        continue;
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

pub fn normalize_asset_path(raw: &str) -> String {
    let mut v = raw.trim().to_string();
    let lower = v.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("file:///") {
        v = v[v.len() - rest.len()..].to_string();
    } else if let Some(rest) = lower.strip_prefix("file://") {
        v = v[v.len() - rest.len()..].to_string();
    }
    v = v.replace('/', "\\");
    let v = xml_unescape(&v);

    let upper_v = v.to_ascii_uppercase();

    let mut drive_letter_pos: Option<usize> = None;
    for letter in 'A'..='Z' {
        let pattern = format!("{}:\\", letter);
        if let Some(pos) = upper_v.find(&pattern) {
            if drive_letter_pos.is_none() || pos < drive_letter_pos.unwrap() {
                drive_letter_pos = Some(pos);
            }
        }
    }
    if let Some(pos) = drive_letter_pos {
        return v[pos..].to_string();
    }

    if let Some(pos) = upper_v.find("\\VOLUMES\\") {
        return v[pos..].to_string();
    }
    v
}

/// Spell a replacement path the way Premiere stored the original.
pub fn format_path_like(original: &str, new_path: &Path) -> String {
    let new = new_path.to_string_lossy();
    if original.contains('/') && !original.contains('\\') {
        new.replace('\\', "/")
    } else {
        new.replace('/', "\\")
    }
}

pub fn path_search_variants(stored: &str) -> Vec<String> {
    let mut variants = Vec::new();
    let mut push_unique = |s: String| {
        if !s.is_empty() && !variants.iter().any(|v: &String| v == &s) {
            variants.push(s);
        }
    };

    push_unique(stored.to_string());
    let slashed = stored.replace('\\', "/");
    push_unique(slashed.clone());
    push_unique(xml_escape(stored));
    push_unique(xml_escape(&slashed));

    if stored.len() >= 2 && stored.as_bytes()[1] == b':' {
        push_unique(format!("file:///{}", slashed.trim_start_matches('/')));
        push_unique(format!("file://{}", slashed));
    }

    variants.sort_by_key(|s| std::cmp::Reverse(s.len()));
    variants
}

pub fn default_exclude_dirs() -> Vec<String> {
    vec![
        "$Recycle.Bin".into(),
        "System Volume Information".into(),
        "Windows".into(),
        "Program Files".into(),
        "Program Files (x86)".into(),
        "AppData".into(),
        "node_modules".into(),
        ".git".into(),
        ".svn".into(),
        "Recovery".into(),
        "Temp".into(),
        "tmp".into(),
    ]
}

pub fn drive_letter(path: &Path) -> Option<char> {
    path.to_string_lossy()
        .chars()
        .next()
        .filter(|c| c.is_ascii_alphabetic())
        .filter(|_| {
            let s = path.to_string_lossy();
            s.chars().nth(1) == Some(':')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_file_url_and_finds_drive() {
        let raw = "file:///D:/gdrive/clip.mp4";
        assert_eq!(normalize_asset_path(raw), "D:\\gdrive\\clip.mp4");
    }

    #[test]
    fn normalize_pulls_drive_out_of_relative_junk() {
        let raw = r"..\D:\gdrive\clip.mp4";
        assert_eq!(normalize_asset_path(raw), r"D:\gdrive\clip.mp4");
    }

    #[test]
    fn expand_env_windows_and_unix() {
        std::env::set_var("PH_TEST_VAR", "Documents");
        assert_eq!(expand_env(r"C:\%PH_TEST_VAR%\x"), r"C:\Documents\x");
        assert_eq!(expand_env("$PH_TEST_VAR/x"), "Documents/x");
        assert_eq!(expand_env("${PH_TEST_VAR}/x"), "Documents/x");
    }

    #[test]
    fn xml_roundtrip_ampersand() {
        let s = r"D:\clips\Tom & Jerry.mp4";
        assert_eq!(xml_unescape(&xml_escape(s)), s);
    }
}

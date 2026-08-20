use crate::prproj::{open_maybe_gzip, skip_oversize};
use std::io::{BufRead, BufReader};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub snippet: String,
    pub clip: Option<String>,
}

#[allow(dead_code)]
pub fn file_contains_case_insensitive(
    path: &Path,
    search_text: &str,
    max_size_bytes: Option<usize>,
) -> Result<bool, std::io::Error> {
    if skip_oversize(path, max_size_bytes)? {
        return Ok(false);
    }

    let reader = BufReader::new(open_maybe_gzip(path)?);
    let needle_lower = search_text.to_lowercase();
    let needle_chars = needle_lower.chars().count();
    let mut overlap = String::new();

    for line in reader.lines() {
        let line = line?;
        let combined = join_overlap(&overlap, &line);
        if combined.to_lowercase().contains(&needle_lower) {
            return Ok(true);
        }
        overlap = overlap_tail(&combined, needle_chars);
    }

    Ok(false)
}

pub fn file_snippet_case_insensitive(
    path: &Path,
    search_text: &str,
    max_size_bytes: Option<usize>,
    snippet_chars: usize,
) -> Result<Option<SearchHit>, std::io::Error> {
    if skip_oversize(path, max_size_bytes)? {
        return Ok(None);
    }

    let reader = BufReader::new(open_maybe_gzip(path)?);
    let needle_lower = search_text.to_ascii_lowercase();
    let needle_chars = needle_lower.chars().count();
    let mut overlap = String::new();

    let total_chars = if snippet_chars == 0 { 120 } else { snippet_chars };
    let half = total_chars / 2;

    for line in reader.lines() {
        let line = line?;
        let combined = join_overlap(&overlap, &line);
        let combined_lower = combined.to_ascii_lowercase();

        if let Some(pos) = combined_lower.find(&needle_lower) {
            let match_end = pos + needle_lower.len();
            let start = pos.saturating_sub(half);
            let end = std::cmp::min(combined.len(), match_end + half);

            let start = floor_char_boundary(&combined, start);
            let end = floor_char_boundary(&combined, end).max(start);

            let mut snippet = combined[start..end].replace('\t', " ");
            snippet = compact_ws(&snippet);
            let prefix = if start > 0 { "..." } else { "" };
            let suffix = if end < combined.len() { "..." } else { "" };
            return Ok(Some(SearchHit {
                snippet: format!("{}{}{}", prefix, snippet, suffix),
                clip: nearby_clip_label(&combined, pos),
            }));
        }

        overlap = overlap_tail(&combined, needle_chars);
    }

    Ok(None)
}

fn nearby_clip_label(haystack: &str, pos: usize) -> Option<String> {
    let start = floor_char_boundary(haystack, pos.saturating_sub(1200));
    let end = floor_char_boundary(haystack, (pos + 400).min(haystack.len()));
    let end = if end < start { haystack.len() } else { end };
    let window = &haystack[start..end];
    last_tag_value(window, "Name").or_else(|| last_tag_value(window, "Title"))
}

fn last_tag_value(window: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut last = None;
    let mut from = 0;
    while let Some(rel) = window[from..].find(&open) {
        let abs = from + rel + open.len();
        if let Some(end) = window[abs..].find(&close) {
            let value = compact_ws(&window[abs..abs + end]);
            if !value.is_empty() && value.len() <= 180 && !value.contains('<') {
                last = Some(value);
            }
            from = abs + end + close.len();
        } else {
            break;
        }
    }
    last
}

fn join_overlap(overlap: &str, line: &str) -> String {
    if overlap.is_empty() {
        line.to_string()
    } else {
        format!("{} {}", overlap.trim_end(), line.trim_start())
    }
}

fn overlap_tail(combined: &str, needle_chars: usize) -> String {
    if needle_chars == 0 {
        return String::new();
    }
    combined
        .chars()
        .rev()
        .take(needle_chars.saturating_sub(1))
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    s.char_indices()
        .map(|(i, _)| i)
        .take_while(|i| *i <= idx)
        .last()
        .unwrap_or(0)
}

fn compact_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            prev_space = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;

    #[test]
    fn finds_text_in_gzipped_prproj() {
        let dir = std::env::temp_dir().join(format!("ph-search-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.prproj");
        let xml = br#"<Project><Title>Clair de Lune</Title></Project>"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(xml).unwrap();
        std::fs::write(&path, encoder.finish().unwrap()).unwrap();
        assert!(file_contains_case_insensitive(&path, "clair de lune", None).unwrap());
        let snippet = file_snippet_case_insensitive(&path, "lune", None, 40)
            .unwrap()
            .unwrap();
        assert!(snippet.snippet.to_lowercase().contains("lune"));
        assert_eq!(snippet.clip.as_deref(), Some("Clair de Lune"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn match_can_span_lines() {
        let dir = std::env::temp_dir().join(format!("ph-span-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("span.prproj");
        std::fs::write(&path, "clair de\nlune in sequence").unwrap();
        assert!(file_contains_case_insensitive(&path, "clair de lune", None).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

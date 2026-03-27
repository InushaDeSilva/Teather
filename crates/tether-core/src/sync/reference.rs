//! Heuristic reference discovery for Inventor-style assemblies (`.iam`, `.ipt`).
//!
//! For full parity, extend with formal parsers and IPJ library paths.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;

fn iam_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?i)(?:FileName|Name|Reference|Component)\s*=\s*"([^"]+\.(?:iam|ipt|ipn|idw))""#)
            .expect("regex")
    })
}

fn loose_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?i)"([^"]+\.(?:iam|ipt|ipn|idw))""#).expect("regex"))
}

/// Parse a rough list of referenced filenames from file bytes (UTF-8 lossy).
pub fn parse_inventor_references(data: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    for cap in iam_regex().captures_iter(&s) {
        if let Some(m) = cap.get(1) {
            out.push(m.as_str().to_string());
        }
    }
    if out.is_empty() {
        for cap in loose_regex().captures_iter(&s) {
            if let Some(m) = cap.get(1) {
                let name = m.as_str();
                if name.contains('.') && name.len() < 260 {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// Build a prefetch list: host + references as paths under `root` / relative to host parent.
pub fn prefetch_closure_paths(
    host_relative: &Path,
    root: &Path,
    reference_names: &[String],
) -> Vec<PathBuf> {
    let parent = host_relative
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let mut paths = Vec::new();
    for name in reference_names {
        let rel = parent.join(name);
        paths.push(root.join(rel));
    }
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_iam_text() {
        let data = br#"
        <Component Name="Child.ipt" />
        FileName="foo.ipt"
        "#;
        let refs = parse_inventor_references(data);
        assert!(refs.iter().any(|r| r.contains("foo.ipt")));
    }
}

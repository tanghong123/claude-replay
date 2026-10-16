//! The pinned browser runtime: one compressed static shared by HTTP and offline export.

use std::io::Read;

use super::HttpResponse;

static RUNTIME_GZIP: &[u8] = include_bytes!("../../../vendor/mermaid/mermaid.min.js.gz");
pub(super) const RUNTIME_PATH: &str = include_str!("../../../vendor/mermaid/runtime-path.txt");

/// Match generated fence markup inside the serialized record stream, not a mention of Mermaid.
pub(super) fn contains_diagram(jsonl: &str) -> bool {
    jsonl.contains(r#"<div class=\"fence mermaid-fence\" data-mermaid>"#)
}

/// Serve immutable release bytes. The listener negotiates an identity fallback when needed.
pub(super) fn route(name: &str) -> Option<HttpResponse> {
    if name != RUNTIME_PATH.trim_start_matches('/') {
        return None;
    }
    let mut response = HttpResponse::ok("text/javascript; charset=utf-8", RUNTIME_GZIP.to_vec());
    response.headers.extend([
        "Content-Encoding: gzip".into(),
        "Vary: Accept-Encoding".into(),
        "Cache-Control: public, max-age=31536000, immutable".into(),
    ]);
    Some(response)
}

/// Decompress once while writing an offline page. Keep the release inert until a diagram mounts.
pub(super) fn inline_runtime() -> String {
    let mut runtime = String::new();
    flate2::read::GzDecoder::new(RUNTIME_GZIP)
        .read_to_string(&mut runtime)
        .expect("the checksummed Mermaid runtime is valid gzip-encoded UTF-8");
    let source = serde_json::to_string(&runtime)
        .expect("serializing a string cannot fail")
        .replace('<', "\\u003c");
    format!("<script id=\"mermaid-runtime\" type=\"application/json\">{source}</script>\n")
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    #[test]
    fn public_release_files_match_the_pinned_checksums() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/mermaid");
        for line in include_str!("../../../vendor/mermaid/release.sha256").lines() {
            let (expected, name) = line.split_once("  ").expect("checksum and filename");
            let bytes = std::fs::read(root.join(name)).expect("pinned release file");
            assert_eq!(format!("{:x}", Sha256::digest(bytes)), expected, "{name}");
        }
    }

    #[test]
    fn offline_runtime_preserves_the_library_without_html_script_delimiters() {
        let html = super::inline_runtime();
        let json = html
            .split_once('>')
            .unwrap()
            .1
            .strip_suffix("</script>\n")
            .unwrap();
        assert!(!json.contains('<'));
        assert_eq!(
            serde_json::from_str::<String>(json).unwrap(),
            include_str!("../../../vendor/mermaid/mermaid.min.js")
        );
        assert!(super::RUNTIME_GZIP.len() < 1_700_000);
    }

    #[test]
    fn diagram_presence_distinguishes_fences_from_literal_markup() {
        for (markdown, expected) in [
            ("```mermaid\nflowchart LR\n A --> B\n```", true),
            ("The attribute is `data-mermaid`.", false),
            (
                "```html\n<div class=\"fence mermaid-fence\" data-mermaid>\n```",
                false,
            ),
        ] {
            let record = serde_json::json!({"h": super::super::md_html(markdown)}).to_string();
            assert_eq!(super::contains_diagram(&record), expected, "{markdown}");
        }
    }
}

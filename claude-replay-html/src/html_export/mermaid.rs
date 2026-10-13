//! The public Mermaid runtime, pinned into the binary for transcript diagrams.

pub(super) const RUNTIME: &str = include_str!("../../../vendor/mermaid/mermaid.min.js");

/// Offline pages keep the runtime as inert JSON until a diagram is mounted. Escaping
/// every `<` prevents a library string from closing the HTML script element.
pub(super) fn inline_runtime() -> String {
    let source = serde_json::to_string(RUNTIME)
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
            super::RUNTIME
        );
    }
}

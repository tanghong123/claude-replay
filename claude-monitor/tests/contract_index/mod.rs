//! The route index of `docs/backend-contract.md` (#s421, design/embedding.md step 2), read by the
//! tests that hold the document to the code: the page's calls to it (`tests/request_layer.rs`),
//! and it to the route table (`src/routes.rs`, and `src/codex_workspace.rs` for Codex native
//! mode). One file, included by each by path.

#![allow(dead_code)]

use std::path::Path;

/// One row: a route, or a prefix when its path ends in `/`.
pub struct Row {
    pub method: String,
    pub path: String,
    pub tier: String,
    /// The path a test asks the table for, `<mdrev>` and `<mermaid>` still to be filled.
    pub probe: String,
}

impl Row {
    pub fn prefix(&self) -> bool {
        self.path.ends_with('/')
    }

    /// The path up to its first placeholder (`/mdrev/<mdrev>/` is `/mdrev/`).
    pub fn literal(&self) -> &str {
        self.path.split('<').next().unwrap_or("")
    }

    /// Whether a server path the page names falls under this row: the path itself, or for a
    /// prefix anything under it, the prefix named without its last slash included.
    pub fn covers(&self, path: &str) -> bool {
        if self.prefix() {
            let literal = self.literal();
            path.starts_with(literal) || format!("{path}/") == literal
        } else {
            path == self.path
        }
    }
}

/// The document's text.
pub fn read() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/backend-contract.md");
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The version the document's `Contract version:` line names.
pub fn version(doc: &str) -> Option<u16> {
    doc.lines()
        .find_map(|line| line.strip_prefix("Contract version: "))
        .and_then(|v| v.trim().parse().ok())
}

/// The table under the `<!-- route-index -->` marker.
pub fn rows(doc: &str) -> Vec<Row> {
    let mut lines = doc
        .lines()
        .skip_while(|line| line.trim() != "<!-- route-index -->")
        .skip(1);
    let header = lines.next().expect("the route index has a header");
    assert!(
        header.starts_with("| Method | Path | Tier | Probe |"),
        "the route index's columns moved: {header}"
    );
    lines.next().expect("the route index has a separator");
    lines
        .take_while(|line| line.starts_with('|'))
        .map(|line| {
            let cells: Vec<String> = line
                .trim()
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().trim_matches('`').to_string())
                .collect();
            assert!(cells.len() >= 4, "a short row in the route index: {line}");
            Row {
                method: cells[0].clone(),
                path: cells[1].clone(),
                tier: cells[2].clone(),
                probe: cells[3].clone(),
            }
        })
        .collect()
}

/// A probe with its placeholders filled, and without its leading slash (as the table is asked).
pub fn probe(row: &Row, mdrev: &str, mermaid: &str) -> String {
    row.probe
        .replace("<mdrev>", mdrev)
        .replace("<mermaid>", mermaid)
        .trim_start_matches('/')
        .to_string()
}

/// The method a probe uses: a row that takes any asks with GET.
pub fn method(row: &Row) -> &str {
    if row.method == "ANY" {
        "GET"
    } else {
        &row.method
    }
}

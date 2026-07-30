//! `FoldPolicy` — which block types start collapsed. Derived from the CLI flags (`--fold`/
//! `--unfold`/`--full`) and consumed by BOTH presenters: the TUI (`View`) and the headless
//! HTML export (`data-open`). It's shared display *policy* keyed on the core's own block
//! classification (`model::fold_key`) — not view state — so it lives in the core beside
//! the vocabulary it indexes.

use crate::model::Block;
use std::collections::HashSet;

/// The canonical block-type keys (see `model::fold_key`).
const FOLD_KEYS: &[&str] = &[
    "user",
    "assistant",
    "thinking",
    "read",
    "bash",
    "edit",
    "write",
    "tool",
    "skill",
    "agent",
    "tool_result",
    "command",
];

/// Map a user-typed key to its canonical `&'static str` (accepts a few aliases).
fn canon_key(k: &str) -> Option<&'static str> {
    let k = k.trim().to_lowercase();
    let k = match k.as_str() {
        "result" | "results" | "toolresult" => "tool_result",
        "reads" => "read",
        "edits" => "edit",
        "writes" => "write",
        "think" => "thinking",
        other => other,
    };
    FOLD_KEYS.iter().copied().find(|c| *c == k)
}

fn parse_keys(csv: Option<&str>) -> Vec<&'static str> {
    csv.into_iter()
        .flat_map(|s| s.split(','))
        .filter_map(canon_key)
        .collect()
}

/// Which block types start collapsed. Defaults mirror Claude Code (thinking,
/// tool_result, and reads folded); `--fold`/`--unfold` adjust per type
/// (`--unfold` wins), and `--full` unfolds everything.
#[derive(Clone)]
pub struct FoldPolicy {
    folded: HashSet<&'static str>,
}

impl Default for FoldPolicy {
    fn default() -> Self {
        // Claude-Code-like: thinking, shell, reads, writes, and other tool calls
        // collapse; user/assistant/edit stay expanded.
        Self {
            folded: [
                "thinking",
                "tool_result",
                "read",
                "bash",
                "tool",
                "skill",
                "agent",
                "command",
                "write",
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl FoldPolicy {
    /// A policy that folds nothing (everything expanded).
    pub fn none() -> Self {
        Self {
            folded: HashSet::new(),
        }
    }

    /// Build from plain flag values (`--full` / `--fold` / `--unfold` CSVs) — the
    /// clap-free constructor. CLI callers go through `Args::fold_policy`.
    pub fn from_flags(full: bool, fold: Option<&str>, unfold: Option<&str>) -> Self {
        let mut p = if full { Self::none() } else { Self::default() };
        for k in parse_keys(fold) {
            p.folded.insert(k);
        }
        for k in parse_keys(unfold) {
            p.folded.remove(k); // --unfold wins over --fold and the defaults
        }
        p
    }

    /// Does this policy start `b` collapsed? Also drives the HTML export's
    /// `data-open`, so a dump and an export fold identically.
    pub fn collapses(&self, b: &Block) -> bool {
        self.folded.contains(crate::model::fold_key(b))
    }

    /// Initial per-block fold state for a block list under this policy.
    pub fn collapsed_for(&self, blocks: &[Block]) -> Vec<bool> {
        blocks.iter().map(|b| self.collapses(b)).collect()
    }
}

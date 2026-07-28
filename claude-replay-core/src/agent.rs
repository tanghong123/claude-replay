//! The agent that produced a session — a plain data enum in the parser core.
//!
//! Kept free of clap on purpose: the viewer's `--agent` flag parses into this via a
//! `value_parser` (see `Args` in the binary crate), so the core carries no CLI dependency.

/// Which agent produced a session. Detected per file from its contents; the viewer's
/// `--agent` flag only *filters* the picker / `--latest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Short label for the picker row / CLI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    /// Parse a `label()` string (round-trips `meta`'s `agent=` field).
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

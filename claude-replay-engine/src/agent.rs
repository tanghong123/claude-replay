//! The agent that produced a session — an **open identity**, not a closed enum (#87
//! step 2). The engine never enumerates agents: behavior lives behind the adapter
//! registry, and this type is only the *name* that keys it. A third-party agent mints
//! its own id with [`Agent::new`] — no variant to add, nothing to fork.
//!
//! Kept free of clap on purpose: the viewer's `--agent` flag parses into this via a
//! `value_parser` (see `Args` in the binary crate), so the core carries no CLI
//! dependency.

/// Which agent produced a session — an interned id (`"claude"`, `"codex"`, …).
/// Detected per file from its contents; the viewer's `--agent` flag only *filters*
/// the picker / `--latest`. `Copy`/`Eq`/`Hash` like the enum it replaced; the three
/// built-ins are associated constants, and `match` sites use them as const patterns
/// (with a wildcard arm — the space is open by design).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Agent(&'static str);

impl Agent {
    pub const CLAUDE: Agent = Agent("claude");
    pub const CODEX: Agent = Agent("codex");
    /// QoderWork — a Claude-Code-format client with its own store (`~/.qoderwork/projects/`)
    /// and a `runtime-config` head line. Parsing delegates to the Claude implementations.
    pub const QODERWORK: Agent = Agent("qoderwork");

    /// Mint an agent id a third-party adapter registers under. The id doubles as the
    /// display label, so keep it short and lowercase (`"gemini"`).
    pub const fn new(id: &'static str) -> Agent {
        Agent(id)
    }

    /// Short label for the picker row / CLI — the id itself.
    pub fn label(self) -> &'static str {
        self.0
    }

    /// Parse a `label()` string (round-trips `meta`'s `agent=` field). Resolves the
    /// built-ins; a third-party label resolves through its adapter's registry entry
    /// (see `discover`), not here.
    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "claude" => Some(Self::CLAUDE),
            "codex" => Some(Self::CODEX),
            "qoderwork" => Some(Self::QODERWORK),
            _ => None,
        }
    }
}

// Serialized as the label (`"claude"`), deserialized through [`Agent::from_label`] —
// the only places this rides are ephemeral cache sidecars, where an unknown label
// simply fails the load and the session re-folds from source (the designed
// degradation for any stale sidecar).
impl serde::Serialize for Agent {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.0)
    }
}
impl<'de> serde::Deserialize<'de> for Agent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Agent::from_label(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown agent label {s:?}")))
    }
}

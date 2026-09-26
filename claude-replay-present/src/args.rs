//! The shared viewer options type. `Args` is plain data every frontend reads; the
//! `cli` feature adds the clap derive so the `claude-replay` binary can parse it —
//! library consumers building their own frontend stay clap-free.

use claude_replay_core::fold::FoldPolicy;
use claude_replay_core::Agent;

/// clap `value_parser` for `--agent`: parse a `claude`/`codex` label into [`Agent`]. Keeps
/// the `ValueEnum` derive (and thus clap) out of the core `Agent` type.
pub fn parse_agent(s: &str) -> std::result::Result<Agent, String> {
    Agent::from_label(s)
        .ok_or_else(|| format!("unknown agent '{s}' (expected: claude, codex, qoder, qoderwork)"))
}

/// View flags. Defaults mirror the bash `claude-peek`: thinking + user turns +
/// code-modifying actions shown; non-modifying ops, tool output hidden.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "cli", derive(clap::Parser))]
#[cfg_attr(
    feature = "cli",
    command(
        name = "agent-replay",
        version,
        about = "Read an AI agent session transcript like a screen (read-only)."
    )
)]
pub struct Args {
    /// Session id, or a path to a .jsonl transcript.
    pub target: Option<String>,

    /// Only show sessions from this agent (claude or codex). Default: all agents.
    #[cfg_attr(feature = "cli", arg(long, value_parser = parse_agent))]
    pub agent: Option<Agent>,

    /// Open the most-recently-active transcript for this directory (or its
    /// nearest ancestor that has sessions) — not the global newest.
    #[cfg_attr(feature = "cli", arg(long))]
    pub latest: bool,

    /// **Deprecated, and ignored.** The viewer and the HTML server always tail; a dump never
    /// does. Kept so existing commands and scripts keep working.
    #[cfg_attr(feature = "cli", arg(short = 'f', long, hide = true))]
    pub follow: bool,

    /// Do not use (or write) the durable session cache — fold every session from scratch.
    /// Also the escape hatch for a second read-only view of a session another instance holds.
    #[cfg_attr(feature = "cli", arg(long))]
    pub no_cache: bool,

    /// Hide ✻ thinking summaries (shown by default).
    #[cfg_attr(feature = "cli", arg(long))]
    pub no_thinking: bool,

    /// Include non-modifying ops (Read/grep/ls/test) — hidden by default.
    #[cfg_attr(feature = "cli", arg(long))]
    pub reads: bool,

    /// Include tool output / results — hidden by default.
    #[cfg_attr(feature = "cli", arg(long))]
    pub results: bool,

    /// Hide user turns.
    #[cfg_attr(feature = "cli", arg(long))]
    pub no_user: bool,

    /// Show everything expanded (unfold every block type).
    #[cfg_attr(feature = "cli", arg(short = 'v', long))]
    pub full: bool,

    /// Start these block types collapsed (comma-separated): user, assistant,
    /// thinking, read, bash, edit, write, tool, skill, agent, tool_result, command.
    #[cfg_attr(feature = "cli", arg(long, value_name = "TYPES"))]
    pub fold: Option<String>,

    /// Start these block types expanded (comma-separated). Wins over --fold and
    /// the defaults. Same type keys as --fold.
    #[cfg_attr(feature = "cli", arg(long, value_name = "TYPES"))]
    pub unfold: Option<String>,

    /// Also show Read calls whose file path contains this substring.
    #[cfg_attr(feature = "cli", arg(long))]
    pub read_match: Option<String>,

    /// Render the whole transcript (no TUI) and exit. With no value, write
    /// `<stem>.txt` + `<stem>.ansi` using a deduced stem; `--dump <stem>` writes to
    /// that stem; `--dump -` prints plain text to stdout (for pipes / tests).
    #[cfg_attr(feature = "cli", arg(long, num_args(0..=1), value_name = "STEM"))]
    pub dump: Option<Option<String>>,
    /// With `--dump`: emit the **structured block stream** instead of rendered text (#34)
    /// — JSON Lines, one object per block: `kind` from the shared block classification,
    /// per-TURN timestamps (`turn`, `turn_ts` — the model holds no per-block times), and
    /// tool execution facts (`status`/`exit`/`ms`) where the source recorded them.
    /// `--dump - --json` streams to stdout; with a stem, writes `<stem>.json`.
    ///
    /// With `--unknown`: one JSON object per unrecognised shape instead of the table (#276) —
    /// `{agent, count, where, name, version, example}` — and NOTHING on stdout when nothing is
    /// new.
    #[cfg_attr(feature = "cli", arg(long))]
    pub json: bool,
    /// Width for `--dump` (columns). Defaults to the terminal width, else 100.
    #[cfg_attr(feature = "cli", arg(long, value_name = "N"))]
    pub width: Option<usize>,

    /// Export a single self-contained `.html` (no TUI). With no value, write
    /// `<stem>.html` using a deduced stem; `--dump-html <stem>` writes to that
    /// stem; `--dump-html -` prints the page to stdout. Honors --fold/--unfold/
    /// --full. A dump is a **snapshot** — to watch a session, use `--html`.
    #[cfg_attr(feature = "cli", arg(long, num_args(0..=1), value_name = "STEM", conflicts_with = "dump"))]
    pub dump_html: Option<Option<String>>,

    /// Export an offline **directory bundle** (no TUI): a shared `index.html` plus one
    /// `<id>.jsonl` per sub-agent reachable from the root, cross-linked so the whole
    /// agent tree is navigable offline. With no value, write to a deduced `<stem>/`
    /// directory; `--dump-all-html <dir>` writes there. Serve it with any static file
    /// server (`python3 -m http.server`). Unlike `--dump-html` (a single flat file),
    /// this preserves sub-agent drill-down. Honors --fold/--unfold/--full.
    #[cfg_attr(feature = "cli", arg(long, num_args(0..=1), value_name = "DIR", conflicts_with_all = ["dump", "dump_html"]))]
    pub dump_all_html: Option<Option<String>>,

    /// Open the transcript as an HTML page in your browser instead of the TUI.
    /// Serves over a loopback HTTP server (so a tool-path click can reveal the
    /// file in Finder) and prints the URL; Ctrl-C stops it. The page **follows
    /// the session live**. Honors --fold/--unfold/--full.
    #[cfg_attr(feature = "cli", arg(long, conflicts_with_all = ["dump", "dump_html"]))]
    pub html: bool,

    /// Print the transcript's directory facts as one JSON object and exit — no viewer.
    /// A shell-out entry to the `discover` path vocabulary for tools that can't link the
    /// crate (e.g. a Python collector): `{path, session_id, first_cwd, latest_cwd,
    /// project_path, repo_root}`, each `null` when unresolved. Honors the target /
    /// `--latest` / `--agent` selection exactly like the viewer.
    #[cfg_attr(feature = "cli", arg(long))]
    pub paths: bool,

    /// Parse transcripts and print what the adapters DROPPED because they did not know about
    /// it (#264), then exit — no viewer. One row per unrecognised shape, most frequent first,
    /// with the client version that wrote the first one and a session to go and look at.
    ///
    /// This is the answer to "has the transcript format moved?", and it exists because the
    /// last time it moved we found out from a screenshot a week later: Claude Code began
    /// recording a real diff for every file-editing Bash command on 2026-09-13 and nothing in
    /// this tool read it (#263). A shape the adapters KNOW and ignore is never reported —
    /// `attachment` alone has twenty types and most are bookkeeping — so anything printed here
    /// is genuinely new since the vocabulary was last taken.
    ///
    /// It also names every MODEL that produced tokens but has no price in `pricing.json`
    /// (`model.unpriced`; its count is sessions): the session's cost silently becomes a lower
    /// bound, which is the pricing half of the same question (#276).
    ///
    /// With no target it sweeps EVERY agent's store on this machine, whatever directory it is
    /// run from — the newest 200 transcripts, or with `--since` every one modified within that
    /// window. `--json` prints one object per shape, for a job to read (#276).
    #[cfg_attr(feature = "cli", arg(long))]
    pub unknown: bool,

    /// With `--paths`: sweep **every agent's store** instead of resolving one session, and
    /// print a JSON ARRAY — one object per transcript on this machine, each carrying the
    /// same directory facts plus `agent`, `mtime` and the canonical `session_key`. This is
    /// what lets a non-Rust consumer stop hard-coding store layouts: the adapter registry
    /// already knows where every agent writes, and a copy of that knowledge in another
    /// language drifts the moment an adapter is added. Not a viewer; ignores the target.
    #[cfg_attr(feature = "cli", arg(long, requires = "paths"))]
    pub all: bool,

    /// With `--paths --all` or `--unknown`: only transcripts modified within this window —
    /// `90m`, `24h`, `7d`. Filtered on mtime BEFORE any file is opened, which is the difference
    /// between a sweep that costs milliseconds and one that reads every byte on the machine
    /// (`--unknown` parses every file it is given; `latest_cwd` reads a file's end, and the
    /// whole of it only when the end records no cwd, #10).
    #[cfg_attr(feature = "cli", arg(long, value_name = "WINDOW"))]
    pub since: Option<String>,
}

impl Args {
    /// The fold policy these flags select (`--full`/`--fold`/`--unfold`) — the clap-side
    /// bridge to the core's [`FoldPolicy::from_flags`].
    pub fn fold_policy(&self) -> FoldPolicy {
        FoldPolicy::from_flags(self.full, self.fold.as_deref(), self.unfold.as_deref())
    }
}

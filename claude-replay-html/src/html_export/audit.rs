//! The **rendering-audit corpus** (#174, property P1) — one fixture that exercises every
//! `(record kind, body part)` cell of the record stream both frontends consume.
//!
//! WHY IT IS DERIVED AND NOT ENUMERATED. Every earlier equivalence audit in this repo was
//! built on a hand-made list of renderings, so it inherited the very omission it was auditing
//! for: if a person has to remember to add a rendering, a forgotten one is invisible to the
//! audit as well as to the reader. So the corpus's COLUMNS are a function of the type system:
//!
//!  * the record-kind axis is `BlockKind` — [`corpus_for`] matches it with **no wildcard
//!    arm**, so a new variant does not compile until someone writes corpus data for it, and
//!    `every_variant_is_listed` re-reads the engine's own source so [`ALL_KINDS`] cannot fall
//!    behind either. `BlockKind` has 16 variants and `BlockKind::html()` maps `ToolResult` and
//!    `Tool` both to `"tool"`, so the stream carries exactly FIFTEEN kind strings.
//!  * the part axis is the emitter's own `"p"` vocabulary — EIGHT kinds (`md think pre raw
//!    note num diff blocks`), scanned out of this module's sibling `mod.rs` by the coverage
//!    test rather than typed here, so a ninth part kind fails the test the day it is emitted.
//!
//! WHAT A CELL IS. `(kind, part)` — "an `edit` record carrying a `diff` part". Most of the
//! 15 × 8 grid is structurally impossible (an `attachment` record has no body at all), which
//! is a claim about the emitter, so [`CLAIMS`] states it per kind with the arm that makes it
//! true and the test checks it BOTH ways: a declared-possible cell the corpus misses fails,
//! and a cell the corpus produces that the claim calls impossible fails too (a stale claim).
//!
//! WHAT IT IS FOR. P2/P3 (the same corpus rendered in a real browser on both shells) need the
//! same cells; [`audit_cells`] is public so a browser fixture built the only way that suite can
//! build one — a Claude-shaped `.jsonl` a monitor can serve — can be PROVED to cover the same
//! cells as this one, instead of being asserted to.

use crate::fold::FoldPolicy;
use crate::model::{
    AgentStatus, AttachmentContent, AttachmentKind, Block, BlockKind, CompactTrigger, Hunk,
    SubAgent,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// Every `BlockKind` variant, in the engine's declaration order.
///
/// Held complete two ways: [`corpus_for`] and [`variant_name`] match on the kind with no
/// wildcard, so a new variant breaks the build, and the `every_variant_is_listed` test reads
/// the enum out of `claude-replay-engine/src/model.rs` and fails if this slice is short.
pub const ALL_KINDS: &[BlockKind] = &[
    BlockKind::User,
    BlockKind::Queue,
    BlockKind::Assistant,
    BlockKind::Think,
    BlockKind::Act,
    BlockKind::ToolResult,
    BlockKind::Attachment,
    BlockKind::Agent,
    BlockKind::Command,
    BlockKind::Bash,
    BlockKind::Edit,
    BlockKind::Write,
    BlockKind::Read,
    BlockKind::Skill,
    BlockKind::Tool,
    BlockKind::Compaction,
];

/// The variant's Rust NAME (not its wire string) — the join key to the engine's source when
/// the test checks [`ALL_KINDS`] against the enum. Exhaustive on purpose.
pub fn variant_name(kind: BlockKind) -> &'static str {
    use BlockKind::*;
    match kind {
        User => "User",
        Queue => "Queue",
        Assistant => "Assistant",
        Think => "Think",
        Act => "Act",
        ToolResult => "ToolResult",
        Attachment => "Attachment",
        Agent => "Agent",
        Command => "Command",
        Bash => "Bash",
        Edit => "Edit",
        Write => "Write",
        Read => "Read",
        Skill => "Skill",
        Tool => "Tool",
        Compaction => "Compaction",
    }
}

/// What body parts a kind CAN carry, and the emitter arm that makes it so. A claim, stated
/// where it can be falsified: the coverage test fails if the corpus misses one of these, and
/// fails if the corpus produces a part this row does not list.
pub struct KindClaim {
    /// The wire kind — `BlockKind::html()`.
    pub kind: &'static str,
    /// Every `"p"` value a record of this kind can carry, sorted.
    pub parts: &'static [&'static str],
    /// Why the rest of the 8 are impossible: the arm in `html_export::mod.rs` that decides it.
    pub why: &'static str,
}

/// The per-kind claim table — 15 rows, one per wire kind. Reasons name the emitter arm rather
/// than asserting; see the module docs.
pub const CLAIMS: &[KindClaim] = &[
    KindClaim {
        kind: "user",
        parts: &["md", "raw"],
        why: "the `UserText` arm's whole body is `user_body_parts`, which emits markdown and \
              lifted preformatted runs and nothing else",
    },
    KindClaim {
        kind: "queue",
        parts: &["md"],
        why: "the `QueueEvent` arm pushes one `md` part (the queued text) and returns",
    },
    KindClaim {
        kind: "assistant",
        parts: &["md"],
        why: "both assistant arms (`AssistantText`, `AssistantMessage`) push exactly one `md`",
    },
    KindClaim {
        kind: "think",
        parts: &["think"],
        why: "`Thinking` is classified `Think` only when `tools` is EMPTY, so its `blocks` \
              part cannot be reached from this kind; the prose is a `think` part",
    },
    KindClaim {
        kind: "act",
        parts: &["blocks", "think"],
        why: "`Thinking` with tools — the tools become the one `blocks` part in the stream, \
              the prose a `think` part",
    },
    KindClaim {
        kind: "attachment",
        parts: &[],
        why: "the `Attachment` arm writes head fields only (name, kind, path, stamps, embedded \
              bytes) and never pushes a body part — the card IS the head",
    },
    KindClaim {
        kind: "agent",
        parts: &["md", "note"],
        why: "`SubAgent`/`AgentDone` push the prompt and result as `md` and the agent id as a \
              `note`",
    },
    KindClaim {
        kind: "command",
        parts: &["md", "pre"],
        why: "the `Command` arm pushes the args as `md` and each `local-command-stdout` chunk \
              through `pre_part`",
    },
    KindClaim {
        kind: "compaction",
        parts: &["md"],
        why: "the `Compaction` arm pushes the continuation summary as `md`; the sizes are head \
              fields",
    },
    KindClaim {
        kind: "bash",
        parts: &["pre"],
        why: "`Bash` takes the `ToolUse` generic output arm — `pre_part(output)`",
    },
    KindClaim {
        kind: "edit",
        parts: &["diff", "note", "pre"],
        why: "the `\"edit\"` arm emits the edit summary `note` + the `diff`, or falls back to \
              `pre_part(output)` when the call recorded no rows to diff",
    },
    KindClaim {
        kind: "write",
        parts: &["diff", "note", "num"],
        why: "the `\"write\"` arm emits a `note` plus either the overwrite `diff` (a call with \
              a structuredPatch) or the fresh file's `num` body",
    },
    KindClaim {
        kind: "read",
        parts: &["num", "pre"],
        why: "`Read`/`NotebookRead` return file bytes as `num`; the search tools that share \
              this kind (Grep/Glob/LS) take the generic `pre` arm — #173's rule",
    },
    KindClaim {
        kind: "skill",
        parts: &["pre"],
        why: "`Skill` takes the `ToolUse` generic output arm — `pre_part(output)`",
    },
    KindClaim {
        kind: "tool",
        parts: &["pre"],
        why: "a bare `ToolResult` and every unrecognised `ToolUse` both end in `pre_part`",
    },
];

/// The corpus: every kind, each carrying every part [`CLAIMS`] says it can.
///
/// One flat block list, in `ALL_KINDS` order, so a reader of the rendered page sees the audit
/// laid out in the engine's own vocabulary.
pub fn audit_corpus() -> Vec<Block> {
    ALL_KINDS.iter().copied().flat_map(corpus_for).collect()
}

/// The blocks that exercise one kind. Exhaustive — adding a `BlockKind` variant stops the
/// build here, which is the entire point of the corpus being derived.
pub fn corpus_for(kind: BlockKind) -> Vec<Block> {
    use BlockKind::*;
    match kind {
        // Prose plus a pasted box: markdown would collapse the box's padding, so the emitter
        // lifts it into a verbatim `raw` part — the only place `raw` comes from.
        User => vec![Block::UserText(
            "Audit the renderings. It printed this:\n\n\
             ╭──────────────────╮\n\
             │  cells: 15 x 8   │\n\
             │                  │\n\
             ╰──────────────────╯\n\n\
             then it stopped."
                .into(),
        )],
        Queue => vec![Block::QueueEvent {
            text: "and re-run the gate when that lands".into(),
        }],
        Assistant => vec![
            Block::AssistantText("The corpus is derived, not enumerated.".into()),
            Block::AssistantMessage {
                text: "Every cell is accounted for.".into(),
                phase: crate::model::AssistantPhase::Final,
                inferred: false,
            },
        ],
        // Thinking with no tools — `block_kind` calls that `Think`.
        Think => vec![Block::Thinking {
            text: "Weighing the two derivations.".into(),
            duration_secs: Some(4),
            tools: vec![],
        }],
        // Thinking WITH tools is `Act`, and the tools are the stream's only `blocks` part.
        // The nested call is a record in its own right, so this cell also covers the nesting.
        Act => vec![Block::Thinking {
            text: "Checking the tree before deciding.".into(),
            duration_secs: Some(2),
            tools: vec![tool_use(
                "Bash",
                "git status --short",
                Some("M claude-replay-html/src/html_export/mod.rs"),
                vec![],
                None,
                None,
            )],
        }],
        // A `tool_result` with no call before it (#122) — kind "tool", body `pre`.
        ToolResult => vec![Block::ToolResult(
            "bare result: 3 files changed, 41 insertions(+)".into(),
        )],
        // Path-only, so nothing is loaded or embedded — and either way this arm emits no body.
        // `use BlockKind::*` shadows the struct's name here, hence the full path.
        Attachment => vec![Block::Attachment(crate::model::Attachment {
            kind: AttachmentKind::File,
            name: "notes.md".into(),
            path: Some("/tmp/audit-corpus/notes.md".into()),
            content: AttachmentContent::None,
        })],
        Agent => vec![
            Block::SubAgent(SubAgent {
                agent_id: "a1b2c3".into(),
                tool_use_id: "toolu_audit_1".into(),
                agent_type: "general-purpose".into(),
                description: "audit the renderings".into(),
                prompt: "Derive the corpus from the closed sets.".into(),
                status: AgentStatus::Completed,
                result: Some("Fifteen kinds, eight parts.".into()),
                output_file: None,
                blocks: vec![],
                subtree_cost: None,
            }),
            Block::AgentDone {
                agent_id: "a1b2c3".into(),
                agent_type: "general-purpose".into(),
                description: "audit the renderings".into(),
                status: AgentStatus::Completed,
                result: Some("Every cell is covered or declared.".into()),
            },
        ],
        Command => vec![Block::Command {
            name: "/audit".into(),
            args: "renderings --all".into(),
            output: vec!["15 kinds x 8 parts".into()],
        }],
        Bash => vec![tool_use(
            "Bash",
            "cargo test -p claude-replay-html",
            Some("test result: ok. 214 passed; 0 failed"),
            vec![],
            None,
            None,
        )],
        Edit => vec![
            // With rows to diff: the summary `note` and the `diff` body.
            tool_use(
                "Edit",
                "claude-replay-html/src/html_export/audit.rs",
                None,
                vec![("let cells = 0;\n".into(), "let cells = 120;\n".into())],
                Some(vec![Hunk {
                    old_start: 12,
                    new_start: 12,
                    lines: vec![
                        " fn cells() {".into(),
                        "-    let cells = 0;".into(),
                        "+    let cells = 120;".into(),
                        " }".into(),
                    ],
                }]),
                None,
            ),
            // An edit whose call recorded no rows to diff falls back to its raw output.
            tool_use(
                "Edit",
                "claude-replay-html/src/html_export/audit.rs",
                Some("The file has been updated."),
                vec![],
                None,
                None,
            ),
        ],
        Write => vec![
            // A fresh file: the `note` plus the numbered body.
            tool_use(
                "Write",
                "claude-replay-html/src/html_export/audit.rs",
                None,
                vec![(
                    String::new(),
                    "fn main() {\n    println!(\"corpus\");\n}\n".into(),
                )],
                None,
                None,
            ),
            // An OVERWRITE carries a structuredPatch and renders as a diff (#92).
            tool_use(
                "NotebookEdit",
                "notebooks/audit.ipynb",
                None,
                vec![("cells = 0\n".into(), "cells = 120\n".into())],
                Some(vec![Hunk {
                    old_start: 1,
                    new_start: 1,
                    lines: vec!["-cells = 0".into(), "+cells = 120".into()],
                }]),
                None,
            ),
        ],
        Read => vec![
            // A file read: bytes of a file, so a numbered gutter is a true claim.
            tool_use(
                "Read",
                "claude-replay-html/src/html_export/audit.rs",
                Some("//! the audit corpus\nuse crate::model::Block;\n"),
                vec![],
                None,
                Some(2),
            ),
            // A search tool shares the kind but returns rows, not a file (#173).
            tool_use(
                "Grep",
                "BlockKind::html",
                Some("claude-replay-engine/src/model.rs:611:    pub fn html(self)"),
                vec![],
                None,
                None,
            ),
        ],
        Skill => vec![tool_use(
            "Skill",
            "taskq",
            Some("#174 pending — the equivalence matrix"),
            vec![],
            None,
            None,
        )],
        Tool => vec![tool_use(
            "WebFetch",
            "https://example.invalid/spec",
            Some("200 OK · 4.1 KB"),
            vec![],
            None,
            None,
        )],
        Compaction => vec![Block::Compaction {
            trigger: CompactTrigger::Auto,
            pre_tokens: 996_000,
            post_tokens: 18_000,
            summary: "The audit derives its corpus from BlockKind and the emitter's parts.".into(),
        }],
    }
}

/// A `ToolUse` with the fields this corpus varies and defaults for the rest.
fn tool_use(
    name: &str,
    target: &str,
    output: Option<&str>,
    diffs: Vec<(String, String)>,
    patch: Option<Vec<Hunk>>,
    read_lines: Option<usize>,
) -> Block {
    Block::ToolUse {
        name: name.into(),
        target: target.into(),
        diffs,
        output: output.map(str::to_string),
        patch,
        read_lines,
        cwd: String::new(),
        execution: None,
        published: None,
    }
}

/// Render blocks the way the pages receive them: one wire record per block, through the same
/// `render_blocks` the live server and the offline bundle both use.
///
/// Public so a browser fixture (P2/P3) can render the same corpus and compare cells.
pub fn audit_records(blocks: &[Block]) -> Vec<Value> {
    let mut state = super::EmitState::default();
    super::render_blocks(
        blocks,
        &[],
        &FoldPolicy::default(),
        "",
        false,
        false,
        None,
        None,
        &mut state,
    )
    .iter()
    .map(|line| serde_json::from_str(line).expect("every wire record is JSON"))
    .collect()
}

/// The `(kind, part)` cells a rendered stream actually contains, nested records included —
/// a `blocks` part's items are records of their own kind.
pub fn audit_cells(records: &[Value]) -> BTreeSet<(String, String)> {
    let mut cells = BTreeSet::new();
    for record in records {
        walk(record, &mut |rec| {
            let kind = rec["kind"].as_str().unwrap_or_default().to_string();
            for part in rec["body"].as_array().into_iter().flatten() {
                let p = part["p"].as_str().unwrap_or_default().to_string();
                cells.insert((kind.clone(), p));
            }
        });
    }
    cells
}

/// The record KINDS a rendered stream contains — separate from [`audit_cells`] because a
/// record with no body (an attachment) contributes a kind and no cell.
pub fn audit_kinds(records: &[Value]) -> BTreeSet<String> {
    let mut kinds = BTreeSet::new();
    for record in records {
        walk(record, &mut |rec| {
            kinds.insert(rec["kind"].as_str().unwrap_or_default().to_string());
        });
    }
    kinds
}

/// Apply `f` to a record and to every record nested in one of its `blocks` parts.
fn walk(record: &Value, f: &mut impl FnMut(&Value)) {
    f(record);
    for part in record["body"].as_array().into_iter().flatten() {
        if part["p"] == "blocks" {
            for item in part["items"].as_array().into_iter().flatten() {
                walk(item, f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The engine's own source — the authority on what variants exist. `include_str!` means
    /// the compiler checks the path, so a moved `model.rs` fails the build rather than
    /// silently emptying the derivation.
    const ENGINE_MODEL_RS: &str = include_str!("../../../claude-replay-engine/src/model.rs");
    /// This crate's emitter — the authority on what part kinds exist.
    const EMITTER_RS: &str = include_str!("mod.rs");

    /// The `BlockKind` variant names, read out of the engine's source.
    fn source_variants() -> Vec<String> {
        let at = ENGINE_MODEL_RS
            .find("pub enum BlockKind {")
            .expect("the engine declares BlockKind");
        let body = &ENGINE_MODEL_RS[at..];
        let end = body.find("\n}").expect("the enum closes");
        body[..end]
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|l| l.ends_with(','))
            .map(|l| l.trim_end_matches(',').to_string())
            .collect()
    }

    /// Every `"p": "…"` the emitter writes, anchored on the KEY. (An unanchored grep is what
    /// produced the phantom `cap`/`items`/`rows` "part kinds" in this task's first pass: those
    /// are FIELDS of a part, and `search`/`truncate`/`wrap` on the JS side are CSS classes.)
    fn emitted_parts() -> BTreeSet<String> {
        let mut parts = BTreeSet::new();
        let bytes = EMITTER_RS.as_bytes();
        for (at, _) in EMITTER_RS.match_indices("\"p\"") {
            let mut i = at + 3;
            while i < bytes.len() && (bytes[i] == b' ' || bytes[i] == b':' || bytes[i] == b'\n') {
                i += 1;
            }
            if i >= bytes.len() || bytes[i] != b'"' {
                continue;
            }
            let rest = &EMITTER_RS[i + 1..];
            if let Some(close) = rest.find('"') {
                parts.insert(rest[..close].to_string());
            }
        }
        parts
    }

    /// The kind axis cannot be typed by hand: `ALL_KINDS` is checked against the enum in the
    /// engine's source, so a variant added there fails HERE even if `corpus_for`'s match were
    /// somehow satisfied.
    #[test]
    fn every_variant_is_listed() {
        let source = source_variants();
        assert!(
            source.len() >= 16,
            "the BlockKind parse found only {} variants — the derivation went vacuous",
            source.len()
        );
        let listed: Vec<String> = ALL_KINDS.iter().map(|k| variant_name(*k).into()).collect();
        assert_eq!(
            listed, source,
            "ALL_KINDS must be the engine's BlockKind, in order"
        );
    }

    /// Fifteen wire kinds from sixteen variants: `ToolResult` and `Tool` share `"tool"`.
    #[test]
    fn the_wire_vocabulary_is_fifteen_kinds() {
        let kinds: BTreeSet<&str> = ALL_KINDS.iter().map(|k| k.html()).collect();
        assert_eq!(kinds.len(), 15, "the stream's kind strings: {kinds:?}");
        assert_eq!(BlockKind::ToolResult.html(), BlockKind::Tool.html());
        let claimed: BTreeSet<&str> = CLAIMS.iter().map(|c| c.kind).collect();
        assert_eq!(claimed, kinds, "CLAIMS must state one row per wire kind");
    }

    /// The part axis is the EMITTER's, scanned out of its source rather than typed here — so
    /// a ninth part kind fails this test the day it is emitted, with no list to maintain.
    #[test]
    fn the_corpus_covers_every_part_kind_the_emitter_can_write() {
        let emitted = emitted_parts();
        assert!(
            emitted.len() >= 8,
            "the part-key scan found only {emitted:?} — the derivation went vacuous"
        );
        let covered: BTreeSet<String> = audit_cells(&audit_records(&audit_corpus()))
            .into_iter()
            .map(|(_, part)| part)
            .collect();
        assert_eq!(
            emitted, covered,
            "every part the emitter can write must appear in the corpus (and vice versa)"
        );
    }

    /// THE coverage test. Every cell the claim table calls possible is produced by the corpus,
    /// and every cell the corpus produces is claimed — a stale "impossible" fails just as
    /// loudly as a missing rendering.
    #[test]
    fn the_corpus_covers_every_claimed_cell_and_no_other() {
        let records = audit_records(&audit_corpus());
        let measured = audit_cells(&records);
        let declared: BTreeSet<(String, String)> = CLAIMS
            .iter()
            .flat_map(|c| c.parts.iter().map(|p| (c.kind.to_string(), p.to_string())))
            .collect();

        let missing: Vec<_> = declared.difference(&measured).collect();
        let unclaimed: Vec<_> = measured.difference(&declared).collect();
        assert!(
            missing.is_empty(),
            "declared possible but the corpus never produced it: {missing:?}"
        );
        assert!(
            unclaimed.is_empty(),
            "the corpus produced a cell no claim allows — the claim is stale: {unclaimed:?}"
        );

        // Every wire kind reaches the page, body or no body (an attachment has none).
        let kinds = audit_kinds(&records);
        for kind in ALL_KINDS {
            assert!(
                kinds.contains(kind.html()),
                "kind {:?} never reached the stream",
                kind.html()
            );
        }
    }

    /// The 15 x 8 table, printed by the run that produced it — so the audit's table is
    /// measured rather than typed. `cargo test -- --nocapture the_coverage_table`.
    #[test]
    fn the_coverage_table() {
        let measured = audit_cells(&audit_records(&audit_corpus()));
        let parts: Vec<String> = emitted_parts().into_iter().collect();
        let claims: BTreeMap<&str, &KindClaim> = CLAIMS.iter().map(|c| (c.kind, c)).collect();
        let head: Vec<String> = parts
            .iter()
            .map(|p| format!("{p:<width$}", width = p.len().max(5)))
            .collect();
        println!("{:<11} {}", "kind", head.join("  "));
        for kind in ALL_KINDS.iter().map(|k| k.html()).collect::<BTreeSet<_>>() {
            let row: Vec<String> = parts
                .iter()
                .map(|p| {
                    let cell = (kind.to_string(), p.clone());
                    let mark = if measured.contains(&cell) {
                        "cover"
                    } else if claims[kind].parts.contains(&p.as_str()) {
                        "MISS!"
                    } else {
                        "  -  "
                    };
                    format!("{mark:<width$}", width = p.len().max(5))
                })
                .collect();
            println!("{kind:<11} {}", row.join("  "));
        }
        println!("\n(- = structurally impossible; the reason is that kind's CLAIMS row)");
        for claim in CLAIMS {
            println!("  {:<11} {}", claim.kind, claim.why);
        }
    }

    /// THE REAL-CORPUS CROSS-CHECK — the bound on this audit's one residual risk: a cell the
    /// generator cannot produce because the emitter reaches it only from data we did not think
    /// to synthesize. Sweeps the transcripts of every agent store ON THIS MACHINE (the same
    /// `store_transcripts` scan the monitor uses) through the same parse and the same emitter,
    /// and asserts the corpus covers every `(kind, part)` reality produced.
    ///
    /// `#[ignore]`d because it reads this machine's own sessions: not hermetic, not CI-runnable,
    /// and its input is private. It prints CELLS AND COUNTS ONLY — never a path, never a byte
    /// of transcript text. Run it by hand:
    ///
    /// ```text
    /// cargo test -p claude-replay-html real_sessions -- --ignored --nocapture
    /// ```
    ///
    /// `AUDIT_SWEEP_SECS` (default 180) bounds the wall clock and `AUDIT_SWEEP_MB` (default 96)
    /// the size of one transcript; both skips are counted in the report, so a partial sweep
    /// says so instead of reading as a clean one.
    #[test]
    #[ignore]
    fn real_sessions_add_no_cell_the_corpus_lacks() {
        use std::time::{Duration, Instant};

        let env_num = |key: &str, default: u64| {
            std::env::var(key)
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(default)
        };
        let budget = Duration::from_secs(env_num("AUDIT_SWEEP_SECS", 180));
        let size_cap = env_num("AUDIT_SWEEP_MB", 96) * 1024 * 1024;
        let started = Instant::now();

        // Newest first, so a clipped sweep is clipped at the OLD end.
        let mut found: Vec<(std::time::SystemTime, std::path::PathBuf, crate::Agent)> = Vec::new();
        for adapter in claude_replay_core::adapters() {
            for path in adapter.store_transcripts() {
                let mtime = std::fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                found.push((mtime, path, adapter.agent()));
            }
        }
        found.sort_by_key(|entry| std::cmp::Reverse(entry.0));
        assert!(
            !found.is_empty(),
            "no agent store on this machine — the cross-check would be vacuous"
        );

        let mut sessions_with: BTreeMap<(String, String), usize> = BTreeMap::new();
        let (mut swept, mut too_big, mut out_of_time, mut unreadable) = (0, 0, 0, 0);
        for (_, path, agent) in &found {
            if started.elapsed() > budget {
                out_of_time += 1;
                continue;
            }
            if std::fs::metadata(path).map(|m| m.len()).unwrap_or(0) > size_cap {
                too_big += 1;
                continue;
            }
            let Ok(session) = crate::parse_session_as(*agent, path) else {
                unreadable += 1;
                continue;
            };
            for cell in audit_cells(&audit_records(&session.blocks())) {
                *sessions_with.entry(cell).or_default() += 1;
            }
            swept += 1;
        }

        let corpus = audit_cells(&audit_records(&audit_corpus()));
        println!(
            "swept {swept} of {} transcripts ({too_big} over the size cap, {out_of_time} past \
             the time budget, {unreadable} unreadable) in {:?}",
            found.len(),
            started.elapsed()
        );
        println!(
            "distinct (kind, part) cells in real sessions: {}",
            sessions_with.len()
        );
        for ((kind, part), sessions) in &sessions_with {
            let held = if corpus.contains(&(kind.clone(), part.clone())) {
                "in corpus"
            } else {
                "NOT IN CORPUS"
            };
            println!("  {kind:<11} {part:<7} {sessions:>5} sessions  {held}");
        }

        let real: BTreeSet<(String, String)> = sessions_with.keys().cloned().collect();
        let missing: Vec<_> = real.difference(&corpus).collect();
        assert!(
            missing.is_empty(),
            "real sessions render cells the corpus cannot produce: {missing:?}"
        );
    }
}

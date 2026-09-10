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
use serde_json::{json, Value};
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

/// The corpus again, as a Claude-shaped `.jsonl` a monitor can SERVE (#174 P2).
///
/// WHY THIS EXISTS AT ALL. [`audit_corpus`] builds [`Block`]s directly, which is the right shape
/// for the Rust half — it can state a cell without inventing a transcript that produces it. The
/// browser half cannot: `claude-replay-browser-tests` can only point a monitor at a store, and a
/// store holds transcripts. So the same corpus has to exist twice, in two forms, and the ONLY
/// thing that makes that safe is the test below: `the_two_corpora_cover_the_same_cells` parses
/// this text back through the real adapter and asserts the cells it yields cover every cell
/// [`audit_corpus`] yields. Two hand-made lists that are asserted to match is exactly the failure
/// mode P1 was written to remove; two hand-made lists with a proof between them is not.
///
/// It lives HERE rather than in the browser crate for the same reason: `parse_session_as` is
/// crate-private to the layers below, so the proof can only run beside the corpus it is proving.
pub fn audit_jsonl() -> String {
    let mut out = String::new();
    let mut at = 0u32;
    let mut stamp = || {
        at += 1;
        format!("2026-06-30T03:{:02}:00.000Z", at)
    };
    let mut push = |value: Value| {
        out.push_str(&value.to_string());
        out.push('\n');
    };

    // user / md — the ordinary prompt.
    push(json!({"type":"user","cwd":"/w","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"text","text":"why does the audit need two corpora?"}]}}));
    // user / raw — prose around a run the emitter LIFTS out of markdown (`preformatted_runs`:
    // a leading indent is enough), which is the only way a user turn grows a `raw` part.
    push(json!({"type":"user","cwd":"/w","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"text","text":"it printed this:\n\n\u{256d}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256e}\n\u{2502} cells \u{2502}\n\u{2570}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{2500}\u{256f}\n\nand then it stopped"}]}}));
    // queue / md — a prompt typed while the agent was busy, not yet picked up.
    push(
        json!({"type":"queue-operation","operation":"enqueue","timestamp":stamp(),
        "content":"and one more thing"}),
    );
    // assistant / md.
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"text","text":"Because a browser can only be pointed at a **store**."}],
                   "usage":{"input_tokens":10,"output_tokens":20}}}));
    // think / think — thinking with NO tool in the same message, which is what makes it `Think`
    // rather than `Act` (the `blocks` part is unreachable from this kind; see CLAIMS).
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"thinking","thinking":"the two forms have to be proved equal, not asserted equal"}]}}));
    // …and a turn between the two thinking records: a lone `think` that sits directly against
    // the thinking-with-tools one never surfaces as its own kind (measured).
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"text","text":"So the twin has to earn its cells."}],
                   "usage":{"input_tokens":4,"output_tokens":6}}}));
    // act / think + act / blocks — thinking AND a tool in one message: the prose is the `think`
    // part and the tools become the single `blocks` part.
    let act = "call-act";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[
            {"type":"thinking","thinking":"check the emitter before guessing at the shape"},
            {"type":"tool_use","id":act,"name":"Bash","input":{"command":"grep -n numbered_part mod.rs"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":act,"content":"499:fn numbered_part(\n1055: body.push(numbered_part(\n"}]}}));
    // attachment / (no body part at all) — the card IS the head.
    push(json!({"type":"attachment","timestamp":stamp(),
        "attachment":{"type":"edited_text_file","filename":"/w/src/html_export/audit.rs","snippet":"1\tintro"}}));
    // agent / md + agent / note — the spawn's prompt and result as `md`, the agent id as a `note`.
    let agent = "call-agent";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":agent,"name":"Agent",
            "input":{"subagent_type":"general-purpose","description":"sweep the corpus","prompt":"sweep the corpus"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "toolUseResult":{"kind":"agent-result","agentId":"a-0001","agentType":"general-purpose","content":"swept"},
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":agent,"content":"swept"}]}}));
    // command / md + command / pre — the args are `md`, each `local-command-stdout` chunk `pre`.
    push(json!({"type":"user","cwd":"/w","timestamp":stamp(),
        "message":{"role":"user","content":"<command-message>audit</command-message>\n<command-name>/audit</command-name>\n<command-args>--all</command-args>"}}));
    push(json!({"type":"user","cwd":"/w","timestamp":stamp(),
        "message":{"role":"user","content":"<local-command-stdout>23 cells covered, 0 uncovered</local-command-stdout>"}}));
    // compaction / md — the continuation summary; the token counts are head fields.
    push(
        json!({"type":"system","subtype":"compact_boundary","timestamp":stamp(),
        "content":"Conversation compacted","compactMetadata":{"trigger":"auto","preTokens":594718,"postTokens":8617}}),
    );
    // The boundary carries the SIZES; the summary prose arrives as the `isCompactSummary` user
    // record that follows it (claude/model.rs pairs the two into one divider). Without it the
    // block has an empty summary and no `md` part at all.
    push(
        json!({"type":"user","isCompactSummary":true,"timestamp":stamp(),
        "message":{"content":"The audit derives its corpus from BlockKind and the emitter's parts."}}),
    );
    // bash / pre — the generic tool-output arm.
    let bash = "call-bash";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":bash,"name":"Bash","input":{"command":"cargo test -p claude-replay-html audit"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":bash,"content":"running 9 tests\ntest result: ok. 9 passed\n"}]}}));
    // edit / note + edit / diff — a call whose result recorded rows to diff.
    let edit = "call-edit";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":edit,"name":"Edit",
            "input":{"file_path":"/w/src/html_export/audit.rs","old_string":"let cells = 0;","new_string":"let cells = 120;"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "toolUseResult":{"filePath":"/w/src/html_export/audit.rs",
            "structuredPatch":[{"oldStart":12,"oldLines":3,"newStart":12,"newLines":3,
                "lines":[" fn cells() {","-    let cells = 0;","+    let cells = 120;"," }"]}]},
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":edit,"content":"The file has been updated."}]}}));
    // edit / pre — the same tool with NO rows to diff, which falls back to its raw output.
    let edit_bare = "call-edit-bare";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":edit_bare,"name":"Edit",
            "input":{"file_path":"/w/README.md"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "toolUseResult":{"filePath":"/w/README.md"},
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":edit_bare,"content":"applied 1 replacement in /w/README.md\nno rows recorded"}]}}));
    // write / note + write / num — a FRESH file: the summary note and the numbered body.
    let write = "call-write";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":write,"name":"Write",
            "input":{"file_path":"/w/scripts/audit.py","content":"#!/usr/bin/env python3\nprint(\"cells\")\nprint(\"parts\")\n"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":write,"content":"Wrote 3 lines to /w/scripts/audit.py"}]}}));
    // write / note + write / diff — an OVERWRITE, whose result carries a patch.
    let overwrite = "call-overwrite";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":overwrite,"name":"Write",
            "input":{"file_path":"/w/scripts/audit.py","content":"#!/usr/bin/env python3\nprint(\"cells\")\nprint(\"claims\")\n"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "toolUseResult":{"filePath":"/w/scripts/audit.py",
            "structuredPatch":[{"oldStart":3,"oldLines":1,"newStart":3,"newLines":1,
                "lines":["-print(\"parts\")","+print(\"claims\")"]}]},
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":overwrite,"content":"The file has been updated."}]}}));
    // read / num — Read returns the BYTES OF A FILE, so the gutter is a claim about a file.
    let read = "call-read";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":read,"name":"Read","input":{"file_path":"/w/src/html_export/audit.rs"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "toolUseResult":{"type":"text","file":{"filePath":"/w/src/html_export/audit.rs","numLines":3,"startLine":1,"totalLines":3,
            "content":"pub const ALL_KINDS: &[BlockKind] = &[\n    BlockKind::User,\n];\n"}},
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":read,
            "content":"pub const ALL_KINDS: &[BlockKind] = &[\n    BlockKind::User,\n];\n"}]}}));
    // read / pre — the SEARCH tools share this kind and return rows, not file bytes (#173).
    let grep = "call-grep";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":grep,"name":"Grep","input":{"pattern":"numbered_part","path":"/w/src"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":grep,
            "content":"/w/src/html_export/mod.rs:499:fn numbered_part(\n/w/src/html_export/mod.rs:1055:body.push(numbered_part(\n"}]}}));
    // skill / pre — the generic output arm again, under its own kind.
    let skill = "call-skill";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":skill,"name":"Skill","input":{"command":"taskq list"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":skill,"content":"#174 [in_progress] the matrix\n"}]}}));
    // tool / pre — an unrecognised tool: the kind of last resort, and the one a new tool lands in.
    let other = "call-other";
    push(json!({"type":"assistant","timestamp":stamp(),
        "message":{"role":"assistant","content":[{"type":"tool_use","id":other,"name":"Frobnicate","input":{"target":"the corpus"}}]}}));
    push(json!({"type":"user","timestamp":stamp(),
        "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":other,"content":"frobnicated 23 cells\n"}]}}));

    out
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

    /// The two forms of the corpus must cover the SAME cells.
    ///
    /// This is the only thing that makes the browser half of #174 trustworthy. `audit_corpus()`
    /// states a cell by constructing the block; `audit_jsonl()` has to earn it, by writing a
    /// transcript the real adapter turns into that block. If the two ever drift, every P2/P3
    /// result is measured against a corpus nobody checked — which is the shape of every audit
    /// this task exists to replace.
    ///
    /// Directional on purpose. The `.jsonl` must cover EVERY cell the block corpus declares;
    /// it may cover more (a transcript record carries fields a hand-built block need not), and
    /// an extra cell is reported rather than failed, because it is information about the
    /// emitter and not a defect in either corpus.
    #[test]
    fn the_two_corpora_cover_the_same_cells() {
        let dir = std::env::temp_dir().join("audit-jsonl-twin");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.jsonl");
        std::fs::write(&path, audit_jsonl()).expect("write the twin");

        let session = crate::parse_session_as(crate::Agent::CLAUDE, &path).expect("parse the twin");
        let served = audit_cells(&audit_records(&session.blocks()));
        let built = audit_cells(&audit_records(&audit_corpus()));

        // The ONE cell a Claude transcript cannot reach, and why — the only place a human
        // judgement enters this proof, so it is written down rather than quietly subtracted.
        //
        // `edit`/`pre` is the emitter's fallback for an Edit that recorded no rows to diff:
        // `else if let Some(out) = output { body.push(pre_part(out)) }` (mod.rs, the `"edit"`
        // arm). It cannot fire on this agent, because the Claude adapter hard-codes
        //
        //     "Edit" | "MultiEdit" | "Write" | "NotebookEdit" => None
        //
        // in `tool_output` (claude/model.rs:500) — an Edit NEVER carries output text on this
        // agent, by design: "Edit/Write show their diff/code, not the boilerplate result".
        // Measured, not assumed: an Edit with no `old_string`/`new_string` and no
        // `structuredPatch` comes back with an EMPTY body — no diff, no pre, nothing.
        //
        // Two things follow, and both belong to #174 rather than to this test. The fallback is
        // reachable only from an adapter that does give an edit-kind call an output, so it is
        // not dead code — but nothing proves another one does. And a Claude Edit with no rows
        // renders as a bare head with no body at all, which is a rendering nobody chose.
        const UNREACHABLE_ON_CLAUDE: &[(&str, &str)] = &[("edit", "pre")];
        let excused: BTreeSet<(String, String)> = UNREACHABLE_ON_CLAUDE
            .iter()
            .map(|(k, p)| ((*k).to_string(), (*p).to_string()))
            .collect();
        for cell in &excused {
            assert!(
                built.contains(cell),
                "{cell:?} is excused from the twin but the built corpus no longer declares it — \
                 delete the excuse rather than leaving it to hide a real gap"
            );
            assert!(
                !served.contains(cell),
                "{cell:?} is excused from the twin and the twin now REACHES it — delete the \
                 excuse, the adapter must have changed"
            );
        }
        let missing: Vec<_> = built
            .difference(&served)
            .filter(|c| !excused.contains(*c))
            .collect();
        assert!(
            missing.is_empty(),
            "the served corpus does not reach {} of the {} cells the built one declares: \
             {missing:?}. A browser case run against it would be auditing a SMALLER matrix than \
             the Rust half and would not say so.",
            missing.len(),
            built.len()
        );
        let extra: Vec<_> = served.difference(&built).collect();
        if !extra.is_empty() {
            println!(
                "the served corpus also reaches {} cell(s) the built one does not: {extra:?}",
                extra.len()
            );
        }
        println!(
            "both corpora cover the same {} of {} cells; {} excused: {UNREACHABLE_ON_CLAUDE:?}",
            built.len() - excused.len(),
            built.len(),
            excused.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
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

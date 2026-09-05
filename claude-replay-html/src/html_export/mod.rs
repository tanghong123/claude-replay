//! HTML export: render a transcript to a single self-contained `.html`.
//!
//! **Structure vs. data.** The page is a fixed shell (CSS + a renderer script,
//! both embedded at compile time) plus an **append-only JSONL stream** of block
//! objects. A one-off export inlines that stream in a `<script>`; a live export
//! (`-f`) additionally writes it to a companion `<stem>.jsonl` and tells the page
//! to poll it, so new blocks can simply be appended as the session grows. The
//! renderer has exactly one code path: parse a line, dispatch on `t`, append DOM.
//!
//! Rust does the work the browser can't — markdown → HTML (`pulldown-cmark`),
//! syntax highlighting (`syntect`), diff computation, and the Claude-Code-style
//! collapsed summary strings — and ships the results as ready-to-insert
//! fragments. Everything that reaches the page is HTML-escaped here; the renderer
//! uses `textContent` for all raw text so nothing can inject markup.

use crate::diff::{diff_row_groups, DiffKind};
use crate::fold::FoldPolicy;
use crate::highlight;
use crate::model::{AssistantPhase, AttachmentContent, Block, LoadedAttachment};
use crate::present::{
    compaction_summary, display_name, edit_summary, spawn_chip, thinking_summary,
    tool_execution_failed, tool_execution_summary, write_content, WRITE_PREVIEW,
};
use crate::{discover, Agent, Transcript};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{json, Map, Value};
use std::path::Path;

// This module is the render core (markdown → HTML, the JSON block emitter, page assembly).
// The offline bundles (`dump_html`/`dump_all_html`) live in `bundle`; the `--html` live
// server in `serve`. All three public entries are re-exported so `html_export::{dump_html,
// dump_all_html, serve}` stays the crate's surface.
mod bundle;
mod record_store;
mod serve;
pub mod shared;
pub(crate) mod sig; // capability signatures for the local-file routes
pub use bundle::{dump_all_html, dump_html};
pub use serve::{
    existing_server, get_request, handoff_url, mint_token, query_get, serve, service_routes,
    spawn_listener, spawn_listener_gated, start_server, AuthGate, HttpResponse, LiveServer,
    Request, RootLock, RouteHandler, ServiceConfig, SessionService, StaleEpoch,
};

const CSS: &str = include_str!("../html/export.css");
const JS: &str = include_str!("../html/export.js");

/// Rows of a diff shown before the `⋯ N more lines` expander.
const DIFF_PREVIEW: usize = 12;
/// Lines of tool output shown before the expander.
const OUTPUT_PREVIEW: usize = 12;
/// How often (ms) a live page re-reads its companion JSONL.
pub(super) const POLL_MS: u64 = 2000;

// ── HTML escaping ────────────────────────────────────────────────────────

/// Escape text for HTML body/attribute context. Covers `&<>"'` so the same
/// function is safe in both places.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

// ── syntax highlighting → CSS classes ────────────────────────────────────

/// Map the shared Claude-Code palette (as 256-colour indices from `highlight`)
/// onto the four `--kw/--str/--fn/--com` token classes. Default text gets no
/// span so it inherits the surrounding colour.
fn syntax_class(fg: u8) -> Option<&'static str> {
    match fg {
        81 => Some("kw"),   // keyword / storage
        141 => Some("kw"),  // number / constant (purple)
        197 => Some("kw"),  // self / language variable
        148 => Some("fn"),  // function / macro
        186 => Some("str"), // string
        242 => Some("com"), // comment
        _ => None,
    }
}

/// Syntax-highlight `code`, returning one HTML fragment per line.
fn highlight_lines(code: &str, token: &str) -> Vec<String> {
    highlight::highlight_spans(code, token)
        .into_iter()
        .map(|spans| {
            let mut out = String::new();
            for s in spans {
                let text = esc(&s.text);
                match s.fg.and_then(syntax_class) {
                    Some(c) => out.push_str(&format!("<span class=\"{c}\">{text}</span>")),
                    None => out.push_str(&text),
                }
            }
            out
        })
        .collect()
}

// ── markdown → native HTML ───────────────────────────────────────────────

/// Render markdown to HTML. Tables/lists/blockquotes become native elements (the
/// browser wraps — no width maths); fenced code becomes a `.fence` card with a
/// language label, a copy button, and syntect-highlighted spans. Raw HTML in the
/// source is **escaped**, never passed through.
fn md_html(src: &str) -> String {
    md_html_inner(src, false)
}

/// [`md_html`] with single newlines kept as line breaks — the USER-turn render (#93):
/// a typed prompt's line structure is literal (CC preserves it), unlike assistant
/// markdown where a single newline is a soft wrap.
fn md_html_user(src: &str) -> String {
    md_html_inner(src, true)
}

// ── Pasted terminal art inside a user turn ────────────────────────────────────────────
//
// A user turn is a markdown document (people write prose, `code`, lists) — but it is also
// where pasted TERMINAL OUTPUT lands, and markdown destroys that: HTML collapses runs of
// spaces, the paragraph parse strips leading indentation, and a proportional font gives
// `│` and letters different widths. A box drawn in a terminal arrives here as
// `│ │` — the alignment gone and unrecoverable.
//
// So the markdown default stays, and runs that are unmistakably art are lifted OUT of it
// into a verbatim part. The detector is deliberately TIGHT: a missed paste renders exactly
// as it does today (no loss), while a false positive would set someone's prose in a
// monospace box. It therefore needs an ANCHOR of certainty and corroborating BULK —
// one stray glyph in a paragraph is never enough:
//
//   * `strong` — the line is unmistakably machine-shaped: box-drawing/block characters
//     (U+2500–U+259F), a line that is nothing but structure (`{`, `},`, `]`), a JSON
//     member (`"key": …`), or a structural terminator (ends in `{`, `[`, `;`, `}`, `]`).
//   * `good`   — strong, OR interior padding (2+ spaces between text), OR a leading indent.
//   * a run of consecutive `good` lines qualifies iff it holds >= 3 non-blank lines,
//     >= 2 of them strong, and the strong ones are at least HALF of it.
//
// Two exclusions carry their weight (both were false positives on a real 3,630-message
// corpus before they went in): inline code spans are stripped before the test, because
// prose ABOUT box characters ("emit a `├─┼─┤` rule between rows") is authored markdown
// and renders correctly already; and fenced blocks are skipped entirely, markdown having
// handled them properly all along. Measured on that corpus the rule fires on 0.11% of
// user turns and every hit is real terminal art.

/// A line with its inline code spans removed — an unterminated backtick swallows the rest,
/// which biases toward NOT detecting (the tight direction).
fn without_code_spans(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut in_code = false;
    for c in line.chars() {
        if c == '`' {
            in_code = !in_code;
        } else if !in_code {
            out.push(c);
        }
    }
    out
}

/// A JSON member — `"key": value` — the shape that makes a pasted config unmistakable.
fn is_json_member(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('"') else {
        return false;
    };
    let Some(close) = rest.find('"') else {
        return false;
    };
    rest[close + 1..].trim_start().starts_with(':')
}

/// Ends the way structure ends, not the way a sentence does. A trailing comma is stripped
/// first (`},` `],`); `:` is deliberately NOT here — prose ends with a colon all the time.
fn ends_structurally(t: &str) -> bool {
    let t = t.trim_end();
    let t = t.strip_suffix(',').unwrap_or(t);
    matches!(t.chars().last(), Some('{' | '[' | ';' | '}' | ']'))
}

/// The ANCHOR: this line could not plausibly be prose. Code spans are stripped first, so a
/// sentence *about* structure ("emit a `├─┼─┤` rule") is not mistaken for the thing itself.
fn is_anchor(line: &str) -> bool {
    let bare = without_code_spans(line);
    let t = bare.trim();
    if t.is_empty() {
        return false;
    }
    // Terminal art.
    t.chars().any(|c| ('\u{2500}'..='\u{259F}').contains(&c))
        // Nothing but structure: `{`, `},`, `]`, `);`.
        || t.chars().all(|c| "{}[]()<>,;:".contains(c))
        || is_json_member(t)
        || ends_structurally(t)
}

/// A run of 2+ spaces BETWEEN text — column padding, not sentence spacing at the margins.
fn has_interior_padding(line: &str) -> bool {
    let (mut seen_text, mut run) = (false, 0usize);
    for c in line.trim_end().chars() {
        if c == ' ' {
            if seen_text {
                run += 1;
            }
        } else {
            if seen_text && run >= 2 {
                return true;
            }
            run = 0;
            seen_text = true;
        }
    }
    false
}

fn has_leading_indent(line: &str) -> bool {
    !line.trim().is_empty() && line.chars().take_while(|c| *c == ' ' || *c == '\t').count() >= 2
}

/// Corroborating evidence — necessary for every line of a run, sufficient for none.
fn looks_preformatted(line: &str) -> bool {
    is_anchor(line) || has_interior_padding(line) || has_leading_indent(line)
}

/// The `[start, end)` line ranges of `lines` that are pasted terminal art (see the note above).
fn preformatted_runs(lines: &[&str]) -> Vec<(usize, usize)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut start: Option<usize> = None;
    let mut fenced = false;
    for (i, line) in lines.iter().enumerate() {
        let is_fence = line.trim_start().starts_with("```");
        if is_fence {
            fenced = !fenced;
        }
        // A fenced block is markdown's business and already renders verbatim.
        let member = if fenced || is_fence {
            false
        } else if looks_preformatted(line) {
            true
        } else {
            // A blank line stays inside a run only when art resumes right after it (a box's
            // empty row); otherwise it ends the run.
            line.trim().is_empty()
                && start.is_some()
                && lines.get(i + 1).is_some_and(|n| looks_preformatted(n))
        };
        match (member, start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                spans.push((s, i));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, lines.len()));
    }
    spans.retain(|&(s, e)| {
        let body = lines[s..e].iter().filter(|l| !l.trim().is_empty());
        let (total, anchored) = body.fold((0usize, 0usize), |(t, a), l| {
            (t + 1, a + usize::from(is_anchor(l)))
        });
        total >= 3 && anchored >= 2 && anchored * 2 >= total
    });
    spans
}

/// A user turn's body parts: markdown for prose, a verbatim `raw` part for pasted art.
/// A turn with no detected art emits exactly one `md` part — byte-identical to before.
fn user_body_parts(text: &str) -> Vec<Value> {
    let lines: Vec<&str> = text.split('\n').collect();
    let runs = preformatted_runs(&lines);
    if runs.is_empty() {
        return vec![json!({ "p": "md", "h": md_html_user(text) })];
    }
    let mut parts = Vec::new();
    let mut prose_from = 0usize;
    let flush = |from: usize, to: usize, parts: &mut Vec<Value>| {
        if from < to {
            let prose = lines[from..to].join("\n");
            if !prose.trim().is_empty() {
                parts.push(json!({ "p": "md", "h": md_html_user(&prose) }));
            }
        }
    };
    for (s, e) in runs {
        flush(prose_from, s, &mut parts);
        parts.push(json!({ "p": "raw", "x": lines[s..e].join("\n") }));
        prose_from = e;
    }
    flush(prose_from, lines.len(), &mut parts);
    parts
}

fn md_html_inner(src: &str, hard_breaks: bool) -> String {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut out = String::new();
    // Fenced-code accumulator: (language token, body).
    let mut fence: Option<(String, String)> = None;
    // Table state: are we inside the head row (→ <th>) or the body (→ <td>)?
    let mut in_head = false;

    for ev in Parser::new_ext(src, opts) {
        // Inside a fence every event is raw text; collect until it closes.
        if let Some((_, body)) = fence.as_mut() {
            match ev {
                Event::Text(t) => {
                    body.push_str(&t);
                    continue;
                }
                Event::End(TagEnd::CodeBlock) => {
                    let (lang, body) = fence.take().expect("fence is Some");
                    out.push_str(&fence_html(&lang, &body));
                    continue;
                }
                _ => continue,
            }
        }
        match ev {
            Event::Start(Tag::Paragraph) => out.push_str("<p>"),
            Event::End(TagEnd::Paragraph) => out.push_str("</p>"),
            Event::Start(Tag::Heading { level, .. }) => {
                let n = match level {
                    HeadingLevel::H1 => 1,
                    HeadingLevel::H2 => 2,
                    _ => 3,
                };
                out.push_str(&format!("<div class=\"md-h{n}\">"));
            }
            Event::End(TagEnd::Heading(_)) => out.push_str("</div>"),
            Event::Start(Tag::Strong) => out.push_str("<strong>"),
            Event::End(TagEnd::Strong) => out.push_str("</strong>"),
            Event::Start(Tag::Emphasis) => out.push_str("<em>"),
            Event::End(TagEnd::Emphasis) => out.push_str("</em>"),
            Event::Start(Tag::Strikethrough) => out.push_str("<del>"),
            Event::End(TagEnd::Strikethrough) => out.push_str("</del>"),
            Event::Start(Tag::BlockQuote(_)) => out.push_str("<blockquote>"),
            Event::End(TagEnd::BlockQuote(_)) => out.push_str("</blockquote>"),
            Event::Start(Tag::List(Some(n))) => out.push_str(&format!("<ol start=\"{n}\">")),
            Event::Start(Tag::List(None)) => out.push_str("<ul>"),
            Event::End(TagEnd::List(true)) => out.push_str("</ol>"),
            Event::End(TagEnd::List(false)) => out.push_str("</ul>"),
            Event::Start(Tag::Item) => out.push_str("<li>"),
            Event::End(TagEnd::Item) => out.push_str("</li>"),
            Event::Start(Tag::Table(_)) => out.push_str("<table>"),
            Event::End(TagEnd::Table) => out.push_str("</tbody></table>"),
            Event::Start(Tag::TableHead) => {
                in_head = true;
                out.push_str("<thead><tr>");
            }
            Event::End(TagEnd::TableHead) => {
                in_head = false;
                out.push_str("</tr></thead><tbody>");
            }
            Event::Start(Tag::TableRow) => out.push_str("<tr>"),
            Event::End(TagEnd::TableRow) => out.push_str("</tr>"),
            Event::Start(Tag::TableCell) => out.push_str(if in_head { "<th>" } else { "<td>" }),
            Event::End(TagEnd::TableCell) => out.push_str(if in_head { "</th>" } else { "</td>" }),
            Event::Start(Tag::Link { dest_url, .. }) => out.push_str(&format!(
                "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">",
                esc(&dest_url)
            )),
            Event::End(TagEnd::Link) => out.push_str("</a>"),
            // Never emit <img>: a remote src would break "no network".
            Event::Start(Tag::Image { dest_url, .. }) => out.push_str(&format!(
                "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">[image] ",
                esc(&dest_url)
            )),
            Event::End(TagEnd::Image) => out.push_str("</a>"),
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(l) => l.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                fence = Some((lang, String::new()));
            }
            Event::Code(t) => out.push_str(&format!("<code>{}</code>", esc(&t))),
            Event::Text(t) => out.push_str(&esc(&t)),
            // Raw HTML is shown as literal text, not injected.
            Event::Html(t) | Event::InlineHtml(t) => out.push_str(&esc(&t)),
            Event::SoftBreak => {
                if hard_breaks {
                    out.push_str("<br>");
                } else {
                    out.push(' ');
                }
            }
            Event::HardBreak => out.push_str("<br>"),
            Event::Rule => out.push_str("<hr>"),
            _ => {}
        }
    }
    // An unterminated fence still renders.
    if let Some((lang, body)) = fence {
        out.push_str(&fence_html(&lang, &body));
    }
    out
}

fn proposed_plan_body(text: &str) -> Option<&str> {
    let text = text.trim();
    let inner = text.strip_prefix("<proposed_plan>\n")?;
    let inner = inner.strip_suffix("\n</proposed_plan>")?.trim();
    (!inner.is_empty()).then_some(inner)
}

fn request_user_input_projection(output: &str) -> Option<Value> {
    let value: Value = serde_json::from_str(output.trim()).ok()?;
    let answers = value.get("answers")?.as_object()?;
    let rows = answers
        .iter()
        .flat_map(|(id, answer)| {
            answer
                .get("answers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |value| {
                    value
                        .as_str()
                        .map(|label| json!({ "id": id, "label": label }))
                })
        })
        .collect::<Vec<_>>();
    Some(json!({
        "kind": "request_user_input",
        "resolved": !rows.is_empty(),
        "answers": rows,
    }))
}

fn fence_html(lang: &str, body: &str) -> String {
    let token = lang.split_whitespace().next().unwrap_or("");
    let code = highlight_lines(body.trim_end_matches('\n'), token).join("\n");
    let label = if token.is_empty() { "code" } else { token };
    format!(
        "<div class=\"fence\"><div class=\"fence-h\"><span class=\"fence-lang\">{}</span>\
         <button class=\"cpy\">copy</button></div><pre><code>{code}</code></pre></div>",
        esc(label)
    )
}

// ── block → JSON ─────────────────────────────────────────────────────────

/// The presentation kind driving `data-kind` (and the renderer's header shape) — the fine
/// projection of the one shared classification (`model::BlockKind`, M13): `think`/`act`
/// split, `tool` for a bare result.
fn html_kind(b: &Block) -> &'static str {
    crate::model::block_kind(b).html()
}

/// Is this block rendered as a collapsible fold? User prose and assistant prose
/// are always-open cards; everything else folds.
fn is_fold(b: &Block) -> bool {
    crate::model::foldable(b)
}

/// A short single-line label for the sidebar / sticky bar.
fn label_of(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= max {
        return one;
    }
    let cut: String = one.chars().take(max).collect();
    format!("{cut}…")
}

fn chip(text: impl Into<String>) -> Value {
    json!({ "x": text.into() })
}

fn chip_class(class: &str, text: impl Into<String>) -> Value {
    json!({ "c": class, "x": text.into() })
}

fn push_chip(head: &mut Map<String, Value>, value: Value) {
    head.entry("chips".to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .expect("chips is always an array")
        .push(value);
}

/// Split `text` into `{p:"pre"}` (bounded preview + hidden tail) body parts.
fn pre_part(text: &str) -> Value {
    json!({ "p": "pre", "x": text, "cap": OUTPUT_PREVIEW })
}

/// Numbered, syntax-highlighted source rows (`Write` bodies, `Read` output).
fn numbered_part(content: &str, token: &str, cap: usize) -> Value {
    let html = highlight_lines(content, token);
    let rows: Vec<Value> = html
        .iter()
        .enumerate()
        .map(|(i, h)| json!([i + 1, h]))
        .collect();
    json!({ "p": "num", "rows": rows, "cap": cap })
}

/// Diff rows for an Edit, as `[tag, num|null, text]` triples for the JS renderer. Delegates
/// the classification + line-numbering to the shared [`diff_row_groups`] (the same
/// logic the TUI renders), so real-file-line-number (patch) vs local-numbering (fallback)
/// behavior can't drift between the two presenters. The gutter grouping is a TUI concern —
/// here the groups are simply flattened.
fn diff_part(b: &Block) -> Option<(Value, usize, usize)> {
    let Block::ToolUse { diffs, patch, .. } = b else {
        return None;
    };
    let mut rows: Vec<Value> = Vec::new();
    let (mut adds, mut dels) = (0usize, 0usize);
    for row in diff_row_groups(diffs, patch.as_deref())
        .into_iter()
        .flat_map(|g| g.rows)
    {
        let tag = match row.kind {
            DiffKind::Ctx => "ctx",
            DiffKind::Add => {
                adds += 1;
                "add"
            }
            DiffKind::Del => {
                dels += 1;
                "del"
            }
        };
        // `num` is a number for context/adds (and old-side deletions from a patch), or
        // null for a fallback deletion — `json!(Option)` serializes exactly that.
        rows.push(json!([tag, row.num, row.text]));
    }
    if rows.is_empty() {
        return None;
    }
    Some((
        json!({ "p": "diff", "rows": rows, "cap": DIFF_PREVIEW }),
        adds,
        dels,
    ))
}

/// Resolve a tool target to an absolute path the way the TUI's
/// `resolve_target_path` does (`~/` → `$HOME`, relative → joined onto `cwd` — the block's
/// own recorded cwd, #173), for the header's `file://` "open" link. `None` when it can't be
/// made absolute (no cwd and a relative target). Existence is **not** required — the
/// export may be opened later or on another machine, and a stale `file://` link
/// simply fails; the browser can't reveal-in-Finder regardless.
fn resolve_abs(cwd: &str, target: &str) -> Option<String> {
    if let Some(rest) = target.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
            return Some(format!("{}/{rest}", home.trim_end_matches('/')));
        }
    }
    if target.starts_with('/') {
        return Some(target.to_string());
    }
    if cwd.is_empty() {
        return None;
    }
    Some(format!("{}/{target}", cwd.trim_end_matches('/')))
}

/// Emitter state: monotonic ids so every block deep-links (`#b7` / `#t3`).
struct Emitter<'a> {
    fold: &'a FoldPolicy,
    /// Session cwd — resolves relative tool targets to absolute `file://` links.
    cwd: &'a str,
    /// Emit reveal-in-Finder path links on file tools? Only for served `--html`
    /// (a `--dump-html` file is shared and its abs paths don't port).
    reveal: bool,
    /// Emit cross-agent navigation links (`child: "?session=<id>"`) on spawn/completion
    /// blocks? True for the multi-file modes (`--dump-all-html`, served) where each agent
    /// has its own stream to navigate to; false for the single-file `--dump-html`.
    linked: bool,
    /// Materialize embedded attachments into `<bundle>/assets/` and link the block to the
    /// written file? Set only for the offline `--dump-all-html` bundle (portable + offline
    /// downloadable); `None` for served (`reveal` Blob/data-URI) and single-file exports.
    assets: Option<&'a mut AssetSink>,
    /// The transcript these blocks came from — the source for loading a `Deferred` attachment's
    /// bytes on demand (once, when the block is emitted), instead of holding them resident.
    /// `None` when no loading is needed (a portable `--dump-html` shows the name only) or in a
    /// unit test with no backing file.
    transcript: Option<&'a Transcript>,
    next_block: crate::model::BlockIndex,
    turn: usize,
    /// The sidebar rows, in emit order — see [`SideEntry`].
    turns: Vec<SideEntry>,
}

/// One sidebar row: a user turn, or (since #108) a compaction **epoch tick**. A session
/// that compacted fifteen times reads as fifteen chapters instead of one flat turn list,
/// which is the whole reason to surface the boundary at all.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct SideEntry {
    /// The anchor to scroll to — `t<N>` for a turn, `b<N>` for an epoch.
    pub id: String,
    /// The row's text.
    pub label: String,
    /// An epoch tick rather than a turn (styled as a seam; not numbered).
    pub epoch: bool,
}

impl SideEntry {
    fn turn(id: String, label: String) -> Self {
        Self {
            id,
            label,
            epoch: false,
        }
    }
    fn epoch(id: String, label: String) -> Self {
        Self {
            id,
            label,
            epoch: true,
        }
    }
}

impl Emitter<'_> {
    fn block_id(&mut self) -> String {
        self.next_block += 1;
        format!("b{}", self.next_block)
    }

    /// Load a `Deferred` attachment's bytes from the transcript on demand — returns `None` for a
    /// path-only (`None`) locator, a missing transcript, or a read/miss. The returned value is
    /// owned and dropped by the caller after it's embedded, so nothing stays resident.
    fn load_attachment(&self, content: &AttachmentContent) -> Option<LoadedAttachment> {
        // #193: the locator travels whole — the span hint rides along, so an elided value
        // loads by direct seek and everything else by the ordinal walk.
        self.transcript?.load_attachment(content).ok().flatten()
    }

    /// One block → its JSON object, recursing into a turn's absorbed tool calls.
    fn block(&mut self, b: &Block, ts: Option<f64>) -> Value {
        let kind = html_kind(b);
        let mut o = Map::new();
        o.insert("t".into(), json!("block"));
        o.insert("kind".into(), json!(kind));
        if is_fold(b) {
            o.insert("fold".into(), json!(true));
            o.insert("open".into(), json!(u8::from(!self.fold.collapses(b))));
        }

        let mut head = Map::new();
        let mut body: Vec<Value> = Vec::new();

        match b {
            Block::UserText(text) => {
                self.turn += 1;
                let id = format!("t{}", self.turn);
                self.turns
                    .push(SideEntry::turn(id.clone(), label_of(text, 46)));
                o.insert("id".into(), json!(id));
                o.insert("turn".into(), json!(self.turn));
                o.insert("label".into(), json!(label_of(text, 80)));
                // The turn's SOURCE, verbatim, beside its rendered parts: the page offers a
                // "show as raw" toggle (globally or for one turn), and markdown rendering is
                // lossy — collapsed padding and stripped indentation cannot be recovered from
                // the HTML. The detector above catches art it is sure about; this is the
                // reader's own escape hatch for everything it deliberately let through.
                // Measured at 0.73% of a large session's record stream — the whole of the
                // user's typing is a rounding error beside its tool output.
                o.insert("src".into(), json!(text));
                body.extend(user_body_parts(text));
            }
            // A sub-agent spawn (kind "agent") — a fold whose header names the agent
            // and whose body carries the prompt, agent id, and result. Full drill-down
            // (child sections, `↓ Children`, hash routing) is a later stage; this makes
            // the spawn render as an ordinary agent-hued fold for now.
            Block::SubAgent(sa) => {
                o.insert("id".into(), json!(self.block_id()));
                head.insert("badge".into(), json!("Agent"));
                head.insert(
                    "preview".into(),
                    json!(format!("{}: {}", sa.agent_type, sa.description)),
                );
                head.insert("chips".into(), json!([chip(spawn_chip(sa))]));
                // Cross-agent navigation: link the spawn to the child's own stream.
                if self.linked && !sa.agent_id.is_empty() {
                    head.insert("child".into(), json!(format!("?session={}", sa.agent_id)));
                    head.insert("child_id".into(), json!(sa.agent_id));
                }
                if !sa.prompt.trim().is_empty() {
                    body.push(json!({ "p": "md", "h": md_html(&sa.prompt) }));
                }
                if !sa.agent_id.is_empty() {
                    body.push(json!({ "p": "note", "x": format!("⏺ {}   {}", sa.agent_id, sa.agent_type) }));
                }
                if let Some(r) = &sa.result {
                    body.push(json!({ "p": "md", "h": md_html(r) }));
                }
            }
            // A sub-agent completion event (kind "agent") — the "different message later"
            // paired with the "launched" spawn above. Header names the agent + the done
            // verb; body carries the returned result.
            Block::AgentDone {
                agent_id,
                agent_type,
                description,
                status,
                result,
            } => {
                o.insert("id".into(), json!(self.block_id()));
                head.insert("badge".into(), json!("Agent"));
                let preview = if agent_type.is_empty() {
                    description.clone()
                } else {
                    format!("{agent_type}: {description}")
                };
                head.insert("preview".into(), json!(preview));
                head.insert("chips".into(), json!([chip(status.done_verb())]));
                // Cross-agent navigation: link the completion to the agent's own stream.
                if self.linked && !agent_id.is_empty() {
                    head.insert("child".into(), json!(format!("?session={agent_id}")));
                    head.insert("child_id".into(), json!(agent_id));
                }
                // The agent id — so the reader can find the finished agent's transcript
                // (navigable via `child` above in multi-file mode; the id shows either way).
                if !agent_id.is_empty() {
                    body.push(json!({ "p": "note", "x": format!("⏺ {agent_id}   {agent_type}") }));
                }
                if let Some(r) = result {
                    body.push(json!({ "p": "md", "h": md_html(r) }));
                }
            }
            Block::AssistantText(text) => {
                o.insert("id".into(), json!(self.block_id()));
                let rendered = if let Some(plan) = proposed_plan_body(text) {
                    head.insert("presentation".into(), json!("proposed_plan"));
                    plan
                } else {
                    text
                };
                body.push(json!({ "p": "md", "h": md_html(rendered) }));
            }
            Block::AssistantMessage {
                text,
                phase,
                inferred,
            } => {
                o.insert("id".into(), json!(self.block_id()));
                o.insert(
                    "phase".into(),
                    json!(match phase {
                        AssistantPhase::Commentary => "commentary",
                        AssistantPhase::Final => "final",
                    }),
                );
                // The phase is data for whoever wants it; `phaseInferred` says whether it was
                // STATED by the transcript. Only a stated phase may change how the prose looks
                // (`.ablock.phase-*` in export.css) — see `Block::AssistantMessage`.
                if *inferred {
                    o.insert("phaseInferred".into(), json!(true));
                }
                let rendered = if let Some(plan) = proposed_plan_body(text) {
                    head.insert("presentation".into(), json!("proposed_plan"));
                    plan
                } else {
                    text
                };
                body.push(json!({ "p": "md", "h": md_html(rendered) }));
            }
            // A dim, always-open `⧗ queued: …` marker (kind "queue") — an in-flight
            // mid-turn prompt not yet picked up. Not a turn (no sidebar entry). The
            // `⧗ queued:` affordance + dim styling come from `.kind-queue` in the CSS.
            Block::QueueEvent { text } => {
                o.insert("id".into(), json!(self.block_id()));
                body.push(json!({ "p": "md", "h": md_html(text) }));
            }
            // A surfaced attachment (kind "attachment") — an always-open card naming the
            // file, with a download (embedded content) or reveal (path-only) affordance.
            // On a portable exported page (`--dump-html`, `reveal == false`) only the
            // name shows; the served `--html` page also gets the path/content to act on.
            Block::Attachment(a) => {
                o.insert("id".into(), json!(self.block_id()));
                let downloadable = matches!(a.content, AttachmentContent::Deferred { .. });
                head.insert("att_kind".into(), json!(a.kind.as_str()));
                head.insert("att_name".into(), json!(a.name.clone()));
                head.insert("att_dl".into(), json!(downloadable));
                // Only a served page (`--html`, `reveal == true`) gets the payload/path
                // to act on; a portable `--dump-html` export shows the name alone. The
                // JS downloads embedded content via a Blob (text) or a `data:` URI
                // (image) — no server endpoint needed — and reveals a path via `/__reveal`.
                // The bytes are loaded from the transcript here, at emit time (once per
                // attachment), embedded, and dropped — never held resident.
                if self.reveal {
                    if let Some(p) = &a.path {
                        // Signed like a tool header's path: the routes act only on what was
                        // offered, and an attachment's path is offered the same way. Two
                        // stamps, because they permit different things — `att_sig` reveals it
                        // in the file manager, `att_fsig` renders its bytes in the page, and
                        // only the second answers to the render policy.
                        use crate::html_export::sig;
                        if let Some(s) = sig::sign(sig::Cap::Reveal, p) {
                            head.insert("att_sig".into(), json!(s));
                        }
                        if sig::may_render(p) {
                            if let Some(s) = sig::sign(sig::Cap::File, p) {
                                head.insert("att_fsig".into(), json!(s));
                            }
                        }
                        head.insert("att_path".into(), json!(p));
                    }
                    match self.load_attachment(&a.content) {
                        Some(LoadedAttachment::Text(t)) => {
                            head.insert("att_text".into(), json!(t));
                        }
                        Some(LoadedAttachment::Base64 { mime, b64 }) => {
                            head.insert(
                                "att_datauri".into(),
                                json!(format!("data:{mime};base64,{b64}")),
                            );
                        }
                        None => {}
                    }
                } else if let Some(loaded) = self.load_attachment(&a.content) {
                    if let Some(sink) = self.assets.as_deref_mut() {
                        // Offline bundle: write the embedded bytes into `assets/` and link the
                        // block to the file (a real offline download), de-conflicting names.
                        if let Some(href) = sink.materialize(&a.name, a.path.as_deref(), &loaded) {
                            head.insert("att_href".into(), json!(href));
                        }
                    }
                }
            }
            Block::Command { name, args, output } => {
                self.turn += 1;
                let id = format!("t{}", self.turn);
                let label = if args.trim().is_empty() {
                    name.clone()
                } else {
                    format!("{name} — {}", label_of(args, 60))
                };
                self.turns
                    .push(SideEntry::turn(id.clone(), label_of(&label, 46)));
                o.insert("id".into(), json!(id));
                o.insert("turn".into(), json!(self.turn));
                o.insert("label".into(), json!(label_of(&label, 80)));
                head.insert("badge".into(), json!(name));
                head.insert("preview".into(), json!(label_of(args, 90)));
                let n = args.lines().count();
                if n > 1 {
                    head.insert("chips".into(), json!([chip(format!("{n} lines"))]));
                }
                if !args.trim().is_empty() {
                    body.push(json!({ "p": "md", "h": md_html(args) }));
                }
                for chunk in output {
                    body.push(pre_part(chunk));
                }
            }
            // A context-compaction divider (kind "compaction") — a hairline seam whose
            // summary is the fold body. It is deliberately NOT a turn: no `t<N>` id, no
            // sidebar row, no `turn` field. The sidebar marks the epoch instead (#108).
            Block::Compaction {
                trigger,
                pre_tokens,
                post_tokens,
                summary,
            } => {
                let id = self.block_id();
                let text = compaction_summary(*trigger, *pre_tokens, *post_tokens);
                o.insert("id".into(), json!(id));
                // `epoch` is what tells the renderer to add the sidebar seam — the record has no
                // `turn`, so the turn-driven sidebar path would otherwise pass it by.
                o.insert("epoch".into(), json!(true));
                head.insert("summary".into(), json!(text.clone()));
                // The facts behind the summary, structured (#86): a page that draws the epoch
                // as a glyph and "from → to" must not parse prose. Sizes only when both are
                // known, as the summary shows them.
                head.insert("compact_trigger".into(), json!(trigger.as_str()));
                if *pre_tokens > 0 && *post_tokens > 0 {
                    head.insert("compact_pre".into(), json!(pre_tokens));
                    head.insert("compact_post".into(), json!(post_tokens));
                }
                self.turns.push(SideEntry::epoch(id, text));
                if !summary.is_empty() {
                    body.push(json!({ "p": "md", "h": md_html(summary) }));
                }
            }
            Block::Thinking {
                text,
                duration_secs,
                tools,
            } => {
                o.insert("id".into(), json!(self.block_id()));
                // The exact collapsed summary the TUI renders — one shared source so the two
                // can't drift; HTML prepends the `✻` glyph.
                let summary = thinking_summary(text, *duration_secs, tools);
                head.insert("summary".into(), json!(format!("✻ {summary}")));
                if !tools.is_empty() {
                    let items: Vec<Value> = tools.iter().map(|t| self.block(t, None)).collect();
                    body.push(json!({ "p": "blocks", "items": items }));
                }
                if !text.trim().is_empty() {
                    body.push(json!({ "p": "think", "h": md_html(text) }));
                }
            }
            Block::ToolResult(text) => {
                o.insert("id".into(), json!(self.block_id()));
                head.insert("name".into(), json!("Result"));
                head.insert("target".into(), json!(label_of(text, 70)));
                body.push(pre_part(text));
            }
            Block::ToolUse {
                name,
                target,
                diffs,
                output,
                read_lines,
                cwd,
                execution,
                published,
                ..
            } => {
                o.insert("id".into(), json!(self.block_id()));
                // The tool's display name (same as the fold header) drives the
                // client-side "filter by tool use" dropdown — one `data-tool` per
                // tool fold, counted and grouped in the browser.
                o.insert("tool".into(), json!(display_name(name)));
                // The run a workflow call launched (#38). A STATIC fact of the call — its id
                // never changes — so it is safe in a cached record; the run's MEMBERS are not,
                // and ride the meta instead. This is what lets the page attach a live fleet to
                // the block that started it without the block itself ever going stale.
                if let Some(run) = self
                    .transcript
                    .and_then(|t| crate::adapter(t.agent()).spawn_run(b))
                {
                    o.insert("run".into(), json!(run));
                }
                head.insert("name".into(), json!(display_name(name)));
                head.insert("target".into(), json!(target));
                if name.eq_ignore_ascii_case("request_user_input") {
                    head.insert(
                        "interaction".into(),
                        json!({
                            "kind": "request_user_input",
                            "resolved": false,
                            "answers": [],
                        }),
                    );
                }
                head.insert(
                    "dot".into(),
                    json!(matches!(kind, "edit" | "write" | "skill" | "agent")),
                );
                // File-acting tools (read/write/edit) get a reveal-in-Finder path
                // link — ONLY when served (`--html`), where a click can reach the
                // local `/__reveal` endpoint. A `--dump-html` file is meant to be
                // shared, and its absolute paths don't resolve on another machine,
                // so its headers stay plain text.
                if self.reveal && matches!(kind, "read" | "write" | "edit") && !target.is_empty() {
                    // #173: resolve against the block's OWN recorded cwd (running-current), not
                    // the session `first_cwd` — a mid-session `cd` moved it. Fall back to the
                    // session cwd when the block carries none.
                    let base = if cwd.is_empty() { self.cwd } else { cwd };
                    if let Some(abs) = resolve_abs(base, target) {
                        // Stamp it: the routes act only on paths this server OFFERED, so a
                        // token holder cannot name a file the page never showed
                        // (`html_export::sig`). Unsigned when there is no key — the link is
                        // then inert, which is the safe direction to fail.
                        //
                        // Two stamps for two capabilities. `sig` reveals the path in the file
                        // manager and is always minted: it hands over no bytes, and on a page
                        // that renders nothing inline it is the only thing a click can do.
                        // `fsig` renders the bytes, and exists only for a path the render
                        // policy allows — not stamping IS the restriction.
                        use crate::html_export::sig;
                        if let Some(s) = sig::sign(sig::Cap::Reveal, &abs) {
                            head.insert("sig".into(), json!(s));
                        }
                        if sig::may_render(&abs) {
                            if let Some(s) = sig::sign(sig::Cap::File, &abs) {
                                head.insert("fsig".into(), json!(s));
                            }
                        }
                        head.insert("path".into(), json!(abs));
                    }
                }
                // An artifact this call published (its URL, name and description). Two jobs:
                // the header's label becomes a real link, and the page can group every
                // republish back into the handful of artifacts they address — twenty calls
                // for two decks, in the session that motivated this. A STATIC fact of the
                // call, so it is safe in a cached record.
                if let Some(p) = published.as_deref() {
                    head.insert(
                        "artifact".into(),
                        json!({
                            "url": p.url,
                            "name": p.name,
                            "icon": p.icon,
                            "desc": p.description,
                        }),
                    );
                }
                let token = highlight::token_for_target(target);
                match kind {
                    "edit" => {
                        if let Some((part, adds, dels)) = diff_part(b) {
                            let mut chips = Vec::new();
                            if adds > 0 {
                                chips.push(chip_class("add", format!("+{adds}")));
                            }
                            if dels > 0 {
                                chips.push(chip_class("del", format!("−{dels}")));
                            }
                            head.insert("chips".into(), json!(chips));
                            body.push(json!({ "p": "note", "x": edit_summary(adds, dels) }));
                            body.push(part);
                        } else if let Some(out) = output {
                            body.push(pre_part(out));
                        }
                    }
                    "write" => {
                        // An overwrite carries the harness's structuredPatch — CC
                        // renders it as a diff like an Edit (#92); only a fresh-file
                        // write keeps the numbered-content preview.
                        let overwrite = matches!(
                            b,
                            Block::ToolUse { patch: Some(h), .. } if !h.is_empty()
                        );
                        if overwrite {
                            if let Some((part, adds, dels)) = diff_part(b) {
                                let mut chips = Vec::new();
                                if adds > 0 {
                                    chips.push(chip_class("add", format!("+{adds}")));
                                }
                                if dels > 0 {
                                    chips.push(chip_class("del", format!("−{dels}")));
                                }
                                head.insert("chips".into(), json!(chips));
                                body.push(json!({ "p": "note", "x": edit_summary(adds, dels) }));
                                body.push(part);
                            }
                        } else {
                            let content = write_content(diffs);
                            let n = content.lines().count();
                            head.insert(
                                "chips".into(),
                                json!([chip_class("add", format!("{n} lines"))]),
                            );
                            body.push(json!({
                                "p": "note",
                                "x": format!("Wrote {n} lines to {target}"),
                            }));
                            body.push(numbered_part(content, token, WRITE_PREVIEW));
                        }
                    }
                    "read" => {
                        if let Some(n) = read_lines {
                            head.insert("chips".into(), json!([chip(format!("{n} lines"))]));
                        }
                        if let Some(out) = output {
                            body.push(numbered_part(out, token, WRITE_PREVIEW));
                        }
                    }
                    _ => {
                        if let Some(out) = output {
                            let n = out.lines().count();
                            head.insert("chips".into(), json!([chip(format!("{n} lines"))]));
                            if name.eq_ignore_ascii_case("request_user_input") {
                                if let Some(interaction) = request_user_input_projection(out) {
                                    head.insert("interaction".into(), interaction);
                                }
                            }
                            body.push(pre_part(out));
                        }
                    }
                }
                if let Some(execution) = execution {
                    let summary = tool_execution_summary(execution);
                    if !summary.is_empty() {
                        push_chip(
                            &mut head,
                            if tool_execution_failed(execution) {
                                chip_class("fail", summary)
                            } else {
                                chip(summary)
                            },
                        );
                    }
                }
            }
        }

        if let Some(ts) = ts {
            o.insert("ts".into(), json!(ts));
        }
        if !head.is_empty() {
            o.insert("head".into(), Value::Object(head));
        }
        o.insert("body".into(), json!(body));
        Value::Object(o)
    }
}

// ── document assembly ────────────────────────────────────────────────────

/// Build the append-only JSONL stream: one `meta` line, then one line per block. `transcript`
/// is the source for loading `Deferred` attachment bytes on demand (`None` when no loading is
/// needed — a portable export shows the name only).
#[allow(clippy::too_many_arguments)]
fn build_jsonl(
    blocks: &[Block],
    user_times: &[Option<f64>],
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    linked: bool,
    transcript: Option<&Transcript>,
    meta: Value,
) -> (String, Vec<SideEntry>) {
    build_jsonl_inner(
        blocks, user_times, fold, cwd, reveal, linked, None, transcript, meta,
    )
}

/// [`build_jsonl`] with an optional [`AssetSink`] — the offline bundle threads one through
/// so each stream materializes its attachments into a shared, de-conflicted `assets/` dir.
#[allow(clippy::too_many_arguments)]
/// The `Emitter`'s cross-block state, exposed so a consumer can render a block **range** continuing
/// from a prior render — render the committed prefix once, then the open turn from the carried-
/// forward state — instead of re-rendering the whole session every poll. Carrying it keeps the
/// settled anchors (`#bN`), turn numbers, and sidebar entries stable across ranges. `Default` is the
/// from-scratch start, so a single whole-session render is byte-identical to before.
#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct EmitState {
    next_block: crate::model::BlockIndex,
    turn: usize,
    /// How many user turns have been consumed from `user_times` so far (indexes into it).
    seen_turns: usize,
    /// The sidebar rows, accumulated across ranges — see [`SideEntry`].
    turns: Vec<SideEntry>,
}

impl EmitState {
    /// The continuation implied by a restored committed prefix of `blocks` blocks holding
    /// `turns` user turns (#96) — see the `RecordStore::adopt` that calls it
    /// for why every field here is derived rather than persisted.
    pub(super) fn resumed(blocks: usize, turns: usize) -> Self {
        Self {
            next_block: blocks,
            turn: turns,
            seen_turns: turns,
            turns: Vec::new(),
        }
    }
}

/// Render `blocks` to one JSON wire record per block, **continuing** the `Emitter` state in `st`
/// (so a later range's anchors/turns follow on). `user_times` is the WHOLE session's per-turn
/// timestamps; `st.seen_turns` indexes into it. This is the resumable core the render-once path
/// (§9) rides: committed blocks pass through once as they commit; the open turn re-renders each poll
/// from a *clone* of the committed state (its ephemeral anchors never pollute the committed state).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_blocks(
    blocks: &[Block],
    user_times: &[Option<f64>],
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    linked: bool,
    assets: Option<&mut AssetSink>,
    transcript: Option<&Transcript>,
    st: &mut EmitState,
) -> Vec<String> {
    let mut em = Emitter {
        fold,
        cwd,
        reveal,
        linked,
        assets,
        transcript,
        next_block: st.next_block,
        turn: st.turn,
        turns: std::mem::take(&mut st.turns),
    };
    let mut lines = Vec::with_capacity(blocks.len());
    for b in blocks {
        // `user_times[i]` is the ith user turn's timestamp (see `model::parse_main`).
        let ts = if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            let t = user_times.get(st.seen_turns).copied().flatten();
            st.seen_turns += 1;
            t
        } else {
            None
        };
        lines.push(em.block(b, ts).to_string());
    }
    st.next_block = em.next_block;
    st.turn = em.turn;
    st.turns = em.turns;
    lines
}

#[allow(clippy::too_many_arguments)]
fn build_jsonl_inner(
    blocks: &[Block],
    user_times: &[Option<f64>],
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    linked: bool,
    assets: Option<&mut AssetSink>,
    transcript: Option<&Transcript>,
    meta: Value,
) -> (String, Vec<SideEntry>) {
    let mut st = EmitState::default();
    let lines = render_blocks(
        blocks, user_times, fold, cwd, reveal, linked, assets, transcript, &mut st,
    );
    let mut out = Vec::with_capacity(lines.len() + 1);
    out.push(meta.to_string());
    out.extend(lines);
    (out.join("\n"), st.turns)
}

/// The page shell: embedded CSS, the inline snapshot, the renderer, and (in live
/// mode) the companion path + poll interval the renderer appends from.
fn build_html(title: &str, jsonl: &str, turns: &[SideEntry], live: Option<&str>) -> String {
    build_page(title, jsonl, turns, live, None, None)
}

/// The shared shell for a multi-file bundle (`--dump-all-html` / served `--html`): no
/// inline snapshot and an empty sidebar (the JS fetches `?session=<id>`.jsonl, defaulting
/// to `root_id`, and fills the sidebar as turns arrive). `live` makes the page poll its
/// stream (served `--html -f`); a static offline bundle sets it false.
pub(super) fn build_shell(title: &str, root_id: &str, live: bool, pull: bool) -> String {
    build_page(title, "", &[], None, Some((root_id, live, pull)), None)
}

/// Host-selectable chrome for a SERVED page (`/session?id=…`, #98 §6.3) — URL parameters,
/// never CSS reaching into the frame.
///
/// `embed` swaps the page's own `claude-replay` brand for the session's title and hides the
/// theme toggle (both are host-owned globals when the page lives inside a host's layout).
/// `theme` stamps `data-theme` after the page's own boot, overriding the localStorage pick
/// WITHOUT writing to it — the host owns the theme, the visitor's standalone preference
/// survives. Every dump path passes no chrome and emits byte-identical output — the byte
/// gate needs no new argument, and no re-baseline.
#[derive(Clone, Debug, Default)]
pub struct PageChrome {
    pub embed: bool,
    pub theme: Option<String>,
    /// Serve clicked file paths as CONTENT (the `/file` route) instead of revealing them in
    /// the OS file manager. Off by default and off for every dump: a page written to disk has
    /// no server behind it, and a host that has not opted in keeps today's reveal behaviour
    /// byte-for-byte. `agent-monitor-v2` sets it — browser-served artifacts are its goal 3.
    pub artifacts: bool,
    /// The HOST owns the search input: render the page's own box hidden, the way `embed`
    /// hides the theme toggle. A host that composes the page into its own chrome (v2 puts a
    /// rail beside it) wants ONE search field, not two — but the search itself, its scope
    /// menu and its hit stepping stay exactly where they are, driven by writing into `#q`.
    /// The page keeps every search behaviour; only the duplicate field goes.
    pub host_search: bool,
    /// A LEFT inset, in CSS pixels, that the page makes for chrome the HOST draws beside it
    /// (`agent-monitor-v2` puts a fixed rail there). The page applies it itself, in
    /// `export.css`, beside the layout it offsets — the point of the slot.
    ///
    /// Before this, v2 positioned itself with the RENDERER's internal selectors: it padded
    /// `body`, offset `#topbar`, and nudged `#newbadge`, having learned from `export.css`
    /// that `.layout` centres itself and so the inset belongs on the document rather than on
    /// its children. That knowledge lived outside the crate that owns the layout, so an
    /// `export.css` restructure would misalign v2 silently — the one place a fork of the
    /// presentation layer could start by accident.
    ///
    /// A NUMBER, not a CSS length, and deliberately: `PageChrome` is built from query
    /// parameters (`/session?…&inset=326`), so a string here would be a CSS-injection surface
    /// on a page that holds the monitor's cookie. The page formats it.
    pub host_inset: Option<u32>,
}

/// The largest inset the page will make. Well past any real rail, and it exists so a
/// nonsense query parameter cannot push the whole transcript off-screen.
pub const MAX_HOST_INSET: u32 = 2000;

/// [`build_shell`] with host chrome — the `/session?id=…&chrome=embed&theme=…` page.
pub(super) fn build_shell_chrome(title: &str, root_id: &str, chrome: &PageChrome) -> String {
    build_page(
        title,
        "",
        &[],
        None,
        Some((root_id, true, true)),
        Some(chrome),
    )
}

/// The page template. `multi` = Some((root_id, live)) makes it a multi-file shell (fetch
/// `?session=<id>`.jsonl, polling when `live`); None inlines `jsonl` + the server-rendered
/// `turns` sidebar.
fn build_page(
    title: &str,
    jsonl: &str,
    turns: &[SideEntry],
    live: Option<&str>,
    multi: Option<(&str, bool, bool)>,
    chrome: Option<&PageChrome>,
) -> String {
    let sidebar: String = turns
        .iter()
        .map(|e| {
            let class = if e.epoch { "side-epoch" } else { "side-item" };
            format!(
                "<div class=\"{class}\" data-t=\"{}\" tabindex=\"0\">{}</div>",
                esc(&e.id),
                esc(&e.label)
            )
        })
        .collect();
    let live_attrs = match (live, multi) {
        // A multi-file shell: `data-multi`/`data-root`, plus `data-poll` when served live
        // (navigation between agents is a full page load carrying `?session=<id>`).
        (_, Some((root, live_multi, pull))) => {
            let poll = if live_multi {
                format!(" data-poll=\"{POLL_MS}\"")
            } else {
                String::new()
            };
            // `data-pull` selects the pull-client transport (#85: ONE transport for every
            // server-backed page — a static page pulls once, a live page keeps polling; only
            // the offline bundle, served flat by any file server, omits it and fetches
            // `<id>.jsonl` directly).
            let pull_attr = if pull { " data-pull=\"1\"" } else { "" };
            format!(
                " data-multi=\"1\" data-root=\"{}\"{poll}{pull_attr}",
                esc(root)
            )
        }
        (Some(src), None) => format!(" data-src=\"{}\" data-poll=\"{POLL_MS}\"", esc(src)),
        (None, None) => String::new(),
    };
    // Host chrome (#98 §6.3): the served `/session` page may swap the brand for the
    // session title, hide the theme toggle, and stamp a host-picked theme AFTER the page's
    // own boot. Every no-chrome caller emits today's exact bytes — `brand`/`theme_btn`
    // reproduce the original lines verbatim and `chrome_stamp` is empty.
    // The artifact opt-in rides `<body>`'s attributes beside `data-multi`/`data-poll`, so
    // export.js reads it exactly where it reads the rest of its page mode.
    let live_attrs = if chrome.is_some_and(|c| c.artifacts) {
        format!("{live_attrs} data-artifacts=\"1\"")
    } else {
        live_attrs
    };
    // The host slot rides `<body>` too: `data-shell` is what `export.css` keys the inset
    // rules on, and `--host-inset` is the length they read. Both come from one clamped
    // integer, so the only thing a caller can vary is how much room the page makes.
    let live_attrs = match chrome.and_then(|c| c.host_inset) {
        Some(px) if px > 0 => format!(
            "{live_attrs} data-shell=\"1\" style=\"--host-inset:{}px\"",
            px.min(MAX_HOST_INSET)
        ),
        _ => live_attrs,
    };
    let embed = chrome.is_some_and(|c| c.embed);
    let brand = if embed {
        let t = esc(title);
        format!(r#"    <div class="brand" id="embed-title" title="{t}">{t}</div>"#)
    } else {
        format!(
            r#"    <div class="brand">agent-replay <span class="brand-sub">v{}</span></div>"#,
            env!("CARGO_PKG_VERSION")
        )
    };
    let theme_btn = if embed {
        r#"    <button id="btn-theme" class="tbtn" style="display:none">◐ Dark</button>"#
    } else {
        r#"    <button id="btn-theme" class="tbtn">◐ Dark</button>"#
    };
    // A host-owned search field hides the page's own box the same way — hidden, not removed:
    // `#q` is still the search's one entry point, and the host types into it.
    let searchbox = if chrome.is_some_and(|c| c.host_search) {
        r#"<div class="searchbox" style="display:none">"#
    } else {
        r#"<div class="searchbox">"#
    };
    let chrome_stamp = match chrome.and_then(|c| c.theme.as_deref()) {
        // After the main script on purpose: export.js has applied the localStorage theme by
        // now, so this wins — and the visitor's stored standalone preference is never written.
        Some(t @ ("light" | "dark")) => format!(
            "<script>document.documentElement.setAttribute(\"data-theme\",\"{t}\");</script>\n"
        ),
        _ => String::new(),
    };
    // The shared modules, inlined ahead of export.js (html_export/shared.rs): this page must
    // stand alone from file://, so what the monitor serves as ES modules arrives here as a
    // classic script publishing the same names on `window.__shared`.
    let shared_js = shared::inline_all();
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><rect width='32' height='32' rx='7' fill='%236d4fa1'/><path d='M12 9l12 7-12 7z' fill='%23fff'/></svg>">
<title>{title_esc}</title>
<style>
{CSS}
</style>
</head>
<body{live_attrs}>
<div id="topbar">
  <div class="tbrow">
{brand}
    <div class="sessionbits" id="sessionbits"></div>
    <button id="btn-exp" class="tbtn ticon" title="Expand all">▾▾</button>
    <button id="btn-col" class="tbtn ticon" title="Collapse all">▸▸</button>
    <button id="btn-raw" class="tbtn ticon" title="Show user turns as raw text — exactly as typed, whitespace intact">{{}}</button>
    <button id="btn-wide" class="tbtn ticon" title="Wide mode — drop the reading-width cap for diff-heavy sessions">⇔</button>
{theme_btn}
  </div>
  <!-- Filter and Agents sit LEFT of the search box (#156). The box is the only thing here that
       flexes, so anything after it is pinned to the right edge — which is exactly where the task
       panel is anchored, and their menus opened underneath it. Tasks stays on the right, beside
       the panel it opens; it has no menu of its own to be covered. -->
  <div class="tbrow">
    <div class="toolfilter">
      <button id="btn-tools" class="tbtn"><span class="tf-label">Filter ▾</span><span class="tf-prev" title="Previous match (N)">‹</span><span class="tf-next" title="Next match (n)">›</span><span class="tf-x" title="Clear filter">✕</span></button>
      <div id="toolmenu">
        <div class="menu-head">Filter by type / tool</div>
        <div id="toolitems"></div>
      </div>
    </div>
    <button id="btn-up" class="tbtn ticon" style="display:none" title="Back to the parent session">↑</button>
    <div class="toolfilter" id="agentnav" style="display:none">
      <button id="btn-agents" class="tbtn disabled"><span class="tf-label">Agents ▾</span></button>
      <div id="agentmenu">
        <div class="menu-head">Sub-agents of this session</div>
        <div id="agentitems"></div>
      </div>
    </div>
    <div class="toolfilter" id="artifactnav" style="display:none">
      <button id="btn-artifacts" class="tbtn disabled"><span class="tf-label">Artifacts ▾</span></button>
      <div id="artifactmenu">
        <div class="menu-head">Artifacts published from this session</div>
        <div id="artifactitems"></div>
      </div>
    </div>
    {searchbox}
      <span class="mag">⌕</span>
      <input id="q" placeholder="Search transcript  ( / )" title="⏎ next · ⇧⏎ previous · uatobrew: prefix scopes by type; w requires whole words (e.g. tw:) · a leading : searches the literal text" autocomplete="off">
      <span id="qcount"></span>
      <span id="qprev" class="qnav" title="Previous match (⇧⏎)">▲</span>
      <span id="qnext" class="qnav" title="Next match (⏎)">▼</span>
      <span class="qscopewrap">
        <span id="qscope" class="qscope" title="Restrict search by message type or whole words — mirrors the uatobrew: prefix (gray = defaults)"><svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M1.5 2.5h13L9.75 8.4v4.6l-3.5 1.5V8.4L1.5 2.5z"/></svg></span>
        <div id="qscopemenu">
          <div class="menu-head">Search only…</div>
          <label class="qs-item"><input type="checkbox" id="qs-u"> user messages (u)<span class="qs-n" id="qsn-u"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-a"> agent responses (a)<span class="qs-n" id="qsn-a"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-t"> thinking (t)<span class="qs-n" id="qsn-t"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-o"> all tools (o)<span class="qs-n" id="qsn-o"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-b"> bash output (b)<span class="qs-n" id="qsn-b"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-r"> read content (r)<span class="qs-n" id="qsn-r"></span></label>
          <label class="qs-item"><input type="checkbox" id="qs-e"> file edits/writes (e)<span class="qs-n" id="qsn-e"></span></label>
          <label class="qs-item qs-whole"><input type="checkbox" id="qs-w"> whole words (w)</label>
        </div>
      </span>
    </div>
    <div class="toolfilter" id="tasknav" style="display:none">
      <button id="btn-tasks" class="tbtn"><span class="tf-label">Tasks ▾</span></button>
    </div>
  </div>
</div>
<div id="taskpanel">
  <div class="taskpanel-head">
    <span class="tp-title" id="tp-title">Session tasks</span>
    <span class="tp-center autofocus" title="Auto-center on running tasks">⌖</span>
    <span class="tp-x" title="Close">✕</span>
  </div>
  <div class="tasks" id="taskbox"></div>
</div>
<div class="layout" id="layout">
  <nav id="sidebar">
    <div class="side-head">Turns</div>
    <div id="turnlist">{sidebar}</div>
    <div class="usage" id="usage"></div>
    <div class="usage" id="runtime"></div>
    <div class="legend">
      <span class="key">j k</span><span class="what">move</span>
      <span class="key">space</span><span class="what">fold</span>
      <span class="key">[ ]</span><span class="what">turn</span>
      <span class="key">/</span><span class="what">search</span>
      <span class="key">− +</span><span class="what">code size</span>
      <span class="key">w</span><span class="what">wrap</span>
    </div>
  </nav>
  <main id="main">
    <section class="session-header">
      <nav id="crumbs" style="display:none"></nav>
      <div class="session-title" id="title">{title_esc}</div>
      <div class="session-meta" id="meta"></div>
    </section>
    <div id="stickybar"><span class="caret">❯</span><span id="stickytext"></span></div>
    <div id="stream"></div>
  </main>
</div>
<button id="newbadge">↓ Jump to bottom</button>
<div id="livechip">⤓ following live</div>
<script id="session-data" type="application/jsonl">
{jsonl_esc}
</script>
<script>
{shared_js}
</script>
<script>
{JS}
</script>
{chrome_stamp}</body>
</html>
"#,
        title_esc = esc(title),
        // `</script>` inside the payload would close the tag early.
        jsonl_esc = jsonl.replace("</", "<\\/"),
    )
}

/// Format a byte count for the meta row.
fn human_tokens(n: u64) -> String {
    match n {
        0 => "0".into(),
        n if n >= 1_000_000 => format!("{:.2}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}K", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// User turns in a block list (the sidebar count).
fn count_turns(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter(|b| matches!(b, Block::UserText(_) | Block::Command { .. }))
        .count()
}

/// Tool calls in a block list, including those absorbed into a thinking turn.
fn count_tools(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|b| match b {
            Block::ToolUse { .. } => 1,
            Block::Thinking { tools, .. } => tools.len(),
            _ => 0,
        })
        .sum()
}

/// The render half of `snapshot`: an already-parsed session → the meta line + block-record
/// JSONL. Shared by the one-shot `snapshot` (parses first) and the incremental live follower
/// (M16, which folds only the delta via a `FollowParser` and passes the result straight here).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_snapshot(
    agent: Agent,
    path: &Path,
    blocks: &[Block],
    user_times: &[Option<f64>],
    m: &crate::metrics::Metrics,
    cwd: &str,
    fold: &FoldPolicy,
    reveal: bool,
    tasks: &crate::engine::TaskList,
) -> (String, Vec<SideEntry>) {
    let session_id = session_id(path);
    // Prefer the repo/dir name as the display title; fall back to the session id
    // when the transcript records no cwd.
    let display = repo_name(cwd).unwrap_or_else(|| session_id.clone());
    let meta = meta_json(
        agent,
        &display,
        &session_id,
        cwd,
        count_turns(blocks),
        count_tools(blocks),
        usage_json(m, false),
        tasks,
        json!({ "path": path.display().to_string(), "duration_secs": m.duration_secs }),
    );
    // The blocks hold only attachment locators; the served/bundle paths load their bytes on
    // demand from this transcript. (A portable `--dump-html` never loads — it shows the name.)
    let transcript = Transcript::open(agent, path);
    build_jsonl(
        blocks,
        user_times,
        fold,
        cwd,
        reveal,
        false, // single-file snapshot: no cross-agent links
        Some(&transcript),
        meta,
    )
}

/// The session id — the transcript file stem (the UUID Claude/Codex names it).
pub(super) fn session_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string()
}

/// The repo/directory name for a session cwd (its last path segment), for the page
/// title — `/Users/hong/personal/claude-replay` → `claude-replay`. `None` when the
/// cwd is empty (no title to derive; callers fall back to the session id).
fn repo_name(cwd: &str) -> Option<String> {
    Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The browser-tab / header title — chosen so a human can pick this session out of a row of
/// tabs. A store transcript is named by a bare session id (a UUID, or a Codex `rollout-…`),
/// which is meaningless in a tab, so we show its **project directory** instead (from the
/// recorded cwd); a transcript the user named and pointed at directly keeps its **file stem**.
/// The session name leads (it's the part that survives tab truncation) and the agent label is
/// appended, so a Claude and a Codex view of the same repo stay distinct.
pub fn display_title(agent: Agent, path: &Path) -> String {
    let stem = session_id(path);
    let name = if looks_like_session_id(&stem) {
        // A machine-generated stem names nothing. Ask the agent what the session is called
        // (#106) before falling back to the repo it was working in — a title the user or the
        // agent chose beats a directory name, which is shared by every session in that repo.
        crate::Transcript::open(agent, path.to_path_buf())
            .card()
            .and_then(|c| c.title)
            .or_else(|| {
                discover::first_cwd(path).and_then(|cwd| repo_name(&cwd.display().to_string()))
            })
            .unwrap_or(stem)
    } else {
        stem // a file the user named → the stem is the meaningful name
    };
    // #66: a merely-compatible detection — no in-band owner marker AND not in any
    // known store (e.g. an unknown Claude-format-derived agent's file handed in by
    // path) — is badged honestly rather than passed off as the parsing agent.
    let owned = discover::detection_owned(agent, path);
    if owned {
        format!("{name} · {}", agent.label())
    } else {
        format!("{name} · compatible ({})", agent.label())
    }
}

/// Does a transcript's file stem look machine-generated (a session UUID or a Codex
/// `rollout-…` name) rather than something a human would recognize?
fn looks_like_session_id(stem: &str) -> bool {
    stem.starts_with("rollout-") || is_uuid(stem)
}

/// A 8-4-4-4-12 hex UUID (the Claude/Codex session-id filename shape).
fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

/// Enough to generate + locate one agent's stream. The root and every discovered
/// sub-agent get one; `source` is the transcript the stream is parsed from.
#[derive(Clone)]
pub(super) struct AgentInfo {
    pub(super) id: String,
    pub(super) source: std::path::PathBuf,
    pub(super) title: String,
    pub(super) agent_type: String,
    /// The ancestry from the root down to this agent's parent — `(id, title)` each — for
    /// the breadcrumb. Empty for the root.
    pub(super) ancestors: Vec<(String, String)>,
}

/// A direct child spawned in a source's blocks — enough to register + resolve its own
/// source. NOT recursive: grandchildren live in each child's own source, discovered when
/// that child's stream is generated.
pub(super) struct ChildRef {
    pub(super) id: String,
    pub(super) description: String,
    pub(super) agent_type: String,
    pub(super) source: Option<std::path::PathBuf>,
    /// The spawn's own terminal status — set for a **synchronous** `Agent` spawn, whose
    /// result (and `status: "completed"`) lands inline on its `tool_use` rather than as a
    /// later `<task-notification>`. Async `Task` completion instead shows up as a separate
    /// `AgentDone` block (see `done` in `agent_stream`); the running flag ORs both signals.
    pub(super) terminal: bool,
}

/// The direct sub-agents spawned in this source (its own `SubAgent` blocks). Drives lazy
/// stream generation — parsing ONE source reveals only its direct children.
fn collect_child_refs(blocks: &[Block]) -> Vec<ChildRef> {
    // Terminal-ness from the sub-agent index (derived from spawn + finish events), not the
    // spawn block's status — the step off reading a back-patched block. Byte-identical: every
    // non-empty-id spawn is in the map, with the same terminal value the back-patch produced.
    let agents = crate::engine::build_sub_agents(blocks);
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) if !sa.agent_id.is_empty() => Some(ChildRef {
                id: sa.agent_id.clone(),
                description: sa.description.clone(),
                agent_type: sa.agent_type.clone(),
                source: None,
                terminal: agents
                    .get(&sa.agent_id)
                    .map(|m| m.status.is_terminal())
                    .unwrap_or(false),
            }),
            _ => None,
        })
        .collect()
}

/// The render half of `agent_stream`: an already-parsed agent (blocks + times + metrics) plus
/// its `AgentInfo` (title / ancestry) → the per-agent stream jsonl (meta with ancestors +
/// running-flagged children + usage, then block records) and the direct child refs. Shared by
/// the one-shot `agent_stream` (parses first) and the incremental live tailer (M16, which
/// folds only the delta via a `FollowParser` and passes the result straight here).
#[allow(clippy::too_many_arguments)]
pub(super) fn render_agent_stream(
    agent: Agent,
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    info: &AgentInfo,
    blocks: &[Block],
    user_times: &[Option<f64>],
    m: &crate::metrics::Metrics,
    tasks: &crate::engine::TaskList,
    assets: Option<&mut AssetSink>,
) -> (String, Vec<ChildRef>) {
    let (meta, child_refs) = agent_meta(agent, cwd, info, blocks, m, tasks);
    // Each agent's attachment locators point into its OWN source transcript; load from there.
    let transcript = Transcript::open(agent, &info.source);
    let (jsonl, _) = build_jsonl_inner(
        blocks,
        user_times,
        fold,
        cwd,
        reveal,
        true,
        assets,
        Some(&transcript),
        meta,
    );
    (jsonl, child_refs)
}

/// The ONE place a meta wire record is assembled (#65) — every field shared by the
/// three call shapes (stream/pull/snapshot) is inserted here exactly once, so a new
/// meta field (like #15's `tasks` or #55's `version`, which previously had to be
/// added in three places and once wasn't) cannot drift between paths. The
/// differently-sourced parts arrive as arguments; the per-shape extras via `extra`
/// (serde_json's map is ordered alphabetically on serialize, so key parity is what
/// matters, not insertion order).
#[allow(clippy::too_many_arguments)]
fn meta_json(
    agent: Agent,
    title: &str,
    sid: &str,
    cwd: &str,
    turns: usize,
    tools: usize,
    usage: Value,
    m_tasks: &crate::engine::TaskList,
    extra: Value,
) -> Value {
    let mut o = json!({
        "t": "meta", "title": title, "agent": agent.label(), "sid": sid,
        "cwd": cwd, "turns": turns, "tools": tools,
        "usage": usage,
        "version": env!("CARGO_PKG_VERSION"),
        "tasks": m_tasks.items,
    });
    if let (Value::Object(dst), Value::Object(src)) = (&mut o, extra) {
        for (k, v) in src {
            dst.insert(k, v);
        }
    }
    o
}

/// The shared usage sub-object; `with_duration` matches the stream/pull shape
/// (duration inside usage) vs the snapshot shape (top-level duration).
fn usage_json(m: &crate::metrics::Metrics, with_duration: bool) -> Value {
    let mut u = json!({
        "input": human_tokens(m.input_tokens), "output": human_tokens(m.output_tokens),
        "cache_read": human_tokens(m.cache_read_tokens),
        "cost": m.cost_usd.map(|c| m.cost_label(c)), "model": m.model_label(),
    });
    // #108. The KEY is omitted (not set to null) when the session never compacted, so a
    // non-compacting session's wire record is unchanged — the property that keeps this
    // feature invisible to every transcript it doesn't apply to.
    if let Some(label) = m.compaction_label() {
        u["compacted"] = json!(label);
    }
    // Credits-billed agents (Qoder) report zero tokens, so credits are their NATIVE cost
    // figure — `cost` above carries the same money converted at the published plan rate
    // (design/qoder-credits-usd.md), and the panel shows both rather than asking a reader to
    // trust the conversion blind. Key omitted when absent, same as `compacted`, so
    // token-billed sessions' wire records are unchanged.
    if let Some(c) = m.credits() {
        u["credits"] = json!(format!("~{c:.2}"));
    }
    if m.runtime != crate::metrics::RuntimeInfo::default() {
        let limits = m.runtime.rate_limits.as_ref();
        u["runtime"] = json!({
            "context_left": m.context_left_percent(),
            "context_used_tokens": m.runtime.context_used_tokens,
            "context_window_tokens": m.runtime.context_window_tokens,
            "effort": m.runtime.reasoning_effort.as_deref(),
            "approvals": m.runtime.approval_policy.as_deref(),
            "sandbox": m.runtime.sandbox.as_deref(),
            "permission": m.runtime.permission_profile.as_deref(),
            "mode": m.runtime.collaboration_mode.as_deref(),
            "tier": m.runtime.service_tier.as_deref(),
            "client": m.runtime.client_version.as_deref(),
            "recorded": m.runtime.recorded,
            "plan": limits.and_then(|l| l.plan_type.as_deref()),
            "reached": limits.and_then(|l| l.reached.as_deref()),
            "primary": limits.and_then(|l| l.primary.as_ref()),
            "secondary": limits.and_then(|l| l.secondary.as_ref()),
        });
    }
    if with_duration {
        u["duration_secs"] = json!(m.duration_secs);
    }
    u
}

/// Build a session's `meta` wire record (title / agent / cwd / turn+tool counts / usage / ancestry
/// / children) and its [`ChildRef`]s from the current blocks. This is the cheap O(N)-**count** part
/// of a stream render, separated from the O(N)-**render** of the blocks — so the render-once live
/// path (`/pull`) can rebuild meta each poll (light) while rendering only the changed block tail.
pub(super) fn agent_meta(
    agent: Agent,
    cwd: &str,
    info: &AgentInfo,
    blocks: &[Block],
    m: &crate::metrics::Metrics,
    tasks: &crate::engine::TaskList,
) -> (Value, Vec<ChildRef>) {
    // This agent's spawned children, in launch order, each flagged running (a spawn with
    // no matching completion yet) vs done — drives the "Agents ▾" menu (active first).
    let done: std::collections::HashSet<&str> = blocks
        .iter()
        .filter_map(|b| match b {
            Block::AgentDone { agent_id, .. } if !agent_id.is_empty() => Some(agent_id.as_str()),
            _ => None,
        })
        .collect();
    let child_refs = collect_child_refs(blocks);
    let children: Vec<Value> = child_refs
        .iter()
        .map(|c| {
            let title = if c.description.is_empty() {
                &c.agent_type
            } else {
                &c.description
            };
            // Running iff neither completion signal fired: no inline-terminal status
            // (sync `Agent`) AND no `AgentDone` block (async `Task`).
            let running = !c.terminal && !done.contains(c.id.as_str());
            json!({ "id": c.id, "title": title, "type": c.agent_type, "running": running })
        })
        .collect();
    let ancestors: Vec<Value> = info
        .ancestors
        .iter()
        .map(|(id, title)| json!({ "id": id, "title": title }))
        .collect();
    let meta = meta_json(
        agent,
        &info.title,
        &info.id,
        cwd,
        count_turns(blocks),
        count_tools(blocks),
        usage_json(m, true),
        tasks,
        json!({
            "agent_type": &info.agent_type, "ancestors": ancestors, "children": children,
            "path": info.source.display().to_string(),
        }),
    );
    (meta, child_refs)
}

/// Assemble the `/pull` meta wire record from the engine's **maintained** `SessionMeta`
/// (turn/tool counts + children, kept current by the accumulator as the tail advances) +
/// presentation info (title / ancestry / agent label) + metrics — the trivial transform from
/// engine facts to the html client's shape. Produces the same JSON [`agent_meta`] derives by
/// scanning the blocks (the oracle test proves it), without any per-poll block scan; `agent_meta`
/// stays as the block-scan assembler for the `/stream`/bundle paths (and as the oracle).
pub(super) fn assemble_meta(
    agent: Agent,
    cwd: &str,
    info: &AgentInfo,
    sm: &crate::engine::SessionMeta,
    m: &crate::metrics::Metrics,
    tasks: &crate::engine::TaskList,
) -> Value {
    let children: Vec<Value> = sm
        .children
        .iter()
        .map(|c| {
            let title = if c.description.is_empty() {
                &c.agent_type
            } else {
                &c.description
            };
            json!({ "id": c.id, "title": title, "type": c.agent_type, "running": c.running })
        })
        .collect();
    let ancestors: Vec<Value> = info
        .ancestors
        .iter()
        .map(|(id, title)| json!({ "id": id, "title": title }))
        .collect();
    meta_json(
        agent,
        &info.title,
        &info.id,
        cwd,
        sm.turns,
        sm.tools,
        usage_json(m, true),
        tasks,
        json!({
            "agent_type": &info.agent_type, "ancestors": ancestors, "children": children,
            "path": info.source.display().to_string(),
        }),
    )
}

/// The `AgentInfo` for a child `c` discovered in `parent`'s source: its title is its
/// description (else its type), and its ancestry is the parent's ancestry + the parent.
pub(super) fn child_info(
    agent: Agent,
    root_path: &Path,
    parent: &AgentInfo,
    c: ChildRef,
) -> Option<AgentInfo> {
    let source = c
        .source
        .or_else(|| discover::subagent_source(agent, root_path, &c.id))?;
    let title = if c.description.is_empty() {
        c.agent_type.clone()
    } else {
        c.description
    };
    let mut ancestors = parent.ancestors.clone();
    ancestors.push((parent.id.clone(), parent.title.clone()));
    Some(AgentInfo {
        id: c.id,
        source,
        title,
        agent_type: c.agent_type,
        ancestors,
    })
}

// The asset sink for the offline bundle — populated by the block emitter (render),
// constructed by `bundle`.
/// De-conflicting writer for embedded attachments in an offline bundle: materializes each
/// attachment into `<bundle>/assets/` under a unique filename and returns its relative
/// `assets/<name>` href. Names that collide across the tree get a `-N` suffix.
pub(super) struct AssetSink {
    dir: std::path::PathBuf,
    used: std::collections::HashMap<String, usize>,
}

impl AssetSink {
    fn new(bundle_dir: &Path) -> std::io::Result<Self> {
        let dir = bundle_dir.join("assets");
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            used: std::collections::HashMap::new(),
        })
    }

    /// Write `content` under a unique name derived from `name`/`path`; return the
    /// `assets/<file>` href, or `None` if the bytes couldn't be written.
    fn materialize(
        &mut self,
        name: &str,
        path: Option<&str>,
        content: &LoadedAttachment,
    ) -> Option<String> {
        let bytes: Vec<u8> = match content {
            LoadedAttachment::Text(t) => t.clone().into_bytes(),
            LoadedAttachment::Base64 { b64, .. } => crate::diff::base64_decode(b64)?,
        };
        // Basename only (no traversal); ensure an extension for images.
        let raw = path.unwrap_or(name);
        let mut base = Path::new(raw)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("attachment")
            .to_string();
        if base.is_empty() {
            base = "attachment".into();
        }
        if let LoadedAttachment::Base64 { mime, .. } = content {
            if !base.contains('.') {
                if let Some(ext) = mime.rsplit('/').next().filter(|e| !e.is_empty()) {
                    base = format!("{base}.{ext}");
                }
            }
        }
        // De-conflict: first use keeps the name, later ones get `-N` before the extension.
        let n = self.used.entry(base.clone()).or_insert(0);
        let fname = if *n == 0 {
            base.clone()
        } else {
            match base.rsplit_once('.') {
                Some((stem, ext)) => format!("{stem}-{n}.{ext}"),
                None => format!("{base}-{n}"),
            }
        };
        *n += 1;
        std::fs::write(self.dir.join(&fname), &bytes).ok()?;
        Some(format!("assets/{fname}"))
    }
}

#[cfg(test)]
mod tests {
    use super::{preformatted_runs, user_body_parts};

    /// Helper: the `p` tag of each body part a user turn emits.
    fn shape(text: &str) -> Vec<String> {
        user_body_parts(text)
            .iter()
            .map(|v| v["p"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    /// A pasted terminal box is lifted out of the markdown into a VERBATIM part, and comes
    /// back byte-for-byte. Through markdown it arrived as `│ │`: HTML collapses the padding
    /// runs, so the right-hand edge — the thing that makes it a rectangle — was gone and
    /// unrecoverable. The prose around it stays markdown.
    #[test]
    fn a_pasted_box_is_lifted_out_verbatim() {
        let text = "It printed this:\n\n\
                    ╭──────────────────╮\n\
                    │  code: GVRC-XGRD │\n\
                    │                  │\n\
                    ╰──────────────────╯\n\n\
                    then it hung.";
        assert_eq!(shape(text), vec!["md", "raw", "md"]);
        let parts = user_body_parts(text);
        let raw = parts[1]["x"].as_str().expect("raw part carries text");
        assert!(raw.starts_with('╭') && raw.ends_with('╯'));
        // Every drawn row is the same width — the property markdown destroyed.
        let widths: std::collections::BTreeSet<usize> =
            raw.lines().map(|l| l.chars().count()).collect();
        assert_eq!(widths.len(), 1, "the box is still rectangular: {raw}");
    }

    /// Prose that TALKS ABOUT box characters is not art. Every glyph here sits inside a
    /// code span — the author's own "this is literal" marker, which markdown already
    /// renders correctly — so stripping code spans before the test keeps this prose.
    /// (A real corpus false positive before that exclusion existed.)
    #[test]
    fn prose_about_box_characters_stays_markdown() {
        let text = "  Update `render_table` to emit a rule between rows:\n\
                    \x20 top `┌─┬─┐`, header, `├─┼─┤`, row, bottom `└─┴─┘`.\n\
                    \x20 Use the existing `table_border()` helper for each.";
        assert_eq!(shape(text), vec!["md"]);
    }

    /// Indented prose has the corroborating signal but no ANCHOR — three indented lines
    /// are a wrapped instruction, not a paste. (The other corpus false positive.)
    #[test]
    fn indented_prose_without_an_anchor_stays_markdown() {
        let text = "  You are running unattended. The human is asleep.\n\
                    \x20 Execute the tasks below to completion without stopping.\n\
                    \x20 Skip anything already done; do not create duplicates.";
        assert_eq!(shape(text), vec!["md"]);
        assert!(preformatted_runs(&text.split('\n').collect::<Vec<_>>()).is_empty());
    }

    /// One glyph is an anchor without bulk: a lone rule in a paragraph is typography, and
    /// two art lines are still under the three-line floor. Both stay prose — the tight
    /// direction, where a miss costs only today's rendering.
    #[test]
    fn an_anchor_without_bulk_stays_markdown() {
        assert_eq!(shape("Section one\n────────────\nSection two"), vec!["md"]);
        assert_eq!(shape("┌────┐\n└────┘"), vec!["md"]);
    }

    /// Pasted JSON is the common case — no box art anywhere, just structure and nesting.
    /// Markdown strips the leading indentation of every continuation line, so the blob
    /// arrived flat and unreadable; the structural anchors catch it and it comes back
    /// exactly as pasted.
    #[test]
    fn pasted_json_keeps_its_indentation() {
        let text = "it failed with:\n\
                    {\n\
                    \x20 \"error\": {\n\
                    \x20   \"category\": \"auth\",\n\
                    \x20   \"code\": 2\n\
                    \x20 }\n\
                    }";
        assert_eq!(shape(text), vec!["md", "raw"]);
        let parts = user_body_parts(text);
        let raw = parts[1]["x"].as_str().expect("raw part");
        assert!(
            raw.contains("\n    \"category\": \"auth\","),
            "nesting survives verbatim: {raw}"
        );
    }

    /// The anchors must not fire on prose that merely ends in punctuation. A colon is
    /// deliberately not structural — English sentences end with one constantly.
    #[test]
    fn prose_ending_in_a_colon_is_not_structure() {
        let text = "  Here is what I want:\n\
                    \x20 first, read the file;\n\
                    \x20 then summarize it for me.";
        // `first, read the file;` ends structurally, but one anchor cannot carry a run.
        assert_eq!(shape(text), vec!["md"]);
    }

    /// A fenced block is markdown's own verbatim construct — already correct, with
    /// highlighting — so the detector never reaches inside one.
    #[test]
    fn fenced_art_is_left_to_markdown() {
        let text = "look:\n\n```\n╭────╮\n│ hi │\n╰────╯\n```\n\ndone";
        assert_eq!(shape(text), vec!["md"]);
    }

    /// Every user turn ships its SOURCE beside the rendered parts, so the page can offer
    /// "show as raw" — globally or for one turn — without asking the server for anything.
    /// Markdown rendering is lossy (this indent does not survive it), so the source cannot
    /// be recovered from the HTML and has to travel. Measured at 0.73% of a large session's
    /// record stream.
    #[test]
    fn a_user_turn_ships_its_source_for_the_raw_toggle() {
        use super::{render_blocks, EmitState};
        use crate::fold::FoldPolicy;
        use crate::model::Block;
        let text = "look:\n    indented — markdown would eat this indent";
        let blocks = vec![Block::UserText(text.into())];
        let mut st = EmitState::default();
        let lines = render_blocks(
            &blocks,
            &[Some(1.0)],
            &FoldPolicy::default(),
            "",
            false,
            false,
            None,
            None,
            &mut st,
        );
        let rec: serde_json::Value = serde_json::from_str(&lines[0]).expect("one wire record");
        assert_eq!(rec["kind"], "user");
        assert_eq!(
            rec["src"], text,
            "the turn carries its own source, byte for byte"
        );
        // And the rendered half is still there beside it — the toggle switches between them.
        assert!(rec["body"].as_array().is_some_and(|b| !b.is_empty()));
    }

    /// The overwhelming majority of turns hold no art at all, and those must emit exactly
    /// what they always did: ONE markdown part, byte-identical.
    #[test]
    fn ordinary_prose_emits_one_markdown_part_unchanged() {
        let text = "Please fix the **auth** bug in `src/lib.rs`:\n- check the token path";
        let parts = user_body_parts(text);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["h"], super::md_html_user(text));
    }

    use super::serve::{percent_decode, query_get};
    use super::*;
    use crate::model::Hunk;

    /// The resumable-render property (render-once foundation, §9): rendering a block list in two
    /// ranges while carrying `EmitState` produces the EXACT same wire records — and the same sidebar
    /// turns — as rendering it whole. This is what lets the live server render committed blocks once
    /// and the open turn from the carried state, instead of re-rendering everything each poll.
    /// #93: a typed prompt's single newlines are literal line breaks (CC preserves
    /// them); assistant markdown keeps the soft-wrap rule.
    #[test]
    fn user_prompt_newlines_are_hard_breaks() {
        assert!(md_html_user("line one\nline two").contains("<br>"));
        assert!(!md_html("line one\nline two").contains("<br>"));
    }

    /// Credits-billed sessions (Qoder): the usage sub-object carries a `credits` key; a
    /// token-billed session's wire record is unchanged (key absent, like `compacted`).
    #[test]
    fn usage_json_carries_credits_only_when_reported() {
        let mut m = crate::metrics::Metrics::default();
        assert!(usage_json(&m, false).get("credits").is_none());
        m.extra.insert("credits_micro".into(), 12_664_262);
        assert_eq!(usage_json(&m, false)["credits"], json!("~12.66"));
    }

    #[test]
    fn usage_json_carries_persisted_runtime_snapshot() {
        use crate::metrics::{RateLimitWindow, RateLimits};
        let mut m = crate::metrics::Metrics::default();
        m.runtime.context_window_tokens = Some(200_000);
        m.runtime.context_used_tokens = Some(50_000);
        m.runtime.reasoning_effort = Some("xhigh".into());
        m.runtime.sandbox = Some("danger-full-access".into());
        m.runtime.rate_limits = Some(RateLimits {
            primary: Some(RateLimitWindow {
                used_percent: 12.5,
                window_minutes: 10_080,
                resets_at: Some(1234),
            }),
            ..RateLimits::default()
        });
        let runtime = &usage_json(&m, false)["runtime"];
        assert_eq!(runtime["context_left"], json!(75));
        assert_eq!(runtime["effort"], json!("xhigh"));
        assert_eq!(runtime["primary"]["used_percent"], json!(12.5));
    }

    /// #62: the snapshot says which facts the agent's format records, and the client
    /// version rides along — a declaration alone is enough to emit the object, so a pane
    /// can say "unknown" for a recorded fact it has not seen.
    #[test]
    fn usage_json_carries_the_runtime_declaration_and_client() {
        let mut m = crate::metrics::Metrics::default();
        assert!(usage_json(&m, false).get("runtime").is_none());
        m.runtime.recorded = vec!["effort".into(), "permission".into(), "client".into()];
        m.runtime.client_version = Some("2.1.234".into());
        let runtime = &usage_json(&m, false)["runtime"];
        assert_eq!(
            runtime["recorded"],
            json!(["effort", "permission", "client"])
        );
        assert_eq!(runtime["client"], json!("2.1.234"));
        assert_eq!(runtime["effort"], json!(null));
    }

    #[test]
    fn assistant_phase_is_structural_html_data() {
        let blocks = vec![
            Block::AssistantMessage {
                text: "working".into(),
                phase: AssistantPhase::Commentary,
                inferred: false,
            },
            Block::AssistantMessage {
                text: "done".into(),
                phase: AssistantPhase::Final,
                inferred: false,
            },
            Block::AssistantMessage {
                text: "narrating".into(),
                phase: AssistantPhase::Commentary,
                inferred: true,
            },
        ];
        let out = stream(&blocks, &FoldPolicy::none());
        assert_eq!(out[0]["phase"], json!("commentary"));
        assert_eq!(out[1]["phase"], json!("final"));
        assert!(out[0].get("phaseInferred").is_none());
        assert!(out[1].get("phaseInferred").is_none());
        // An inferred phase is still DATA — it just carries the flag that stops `export.js`
        // from restyling the prose, so a Claude session keeps looking like Claude Code.
        assert_eq!(out[2]["phase"], json!("commentary"));
        assert_eq!(out[2]["phaseInferred"], json!(true));
    }

    #[test]
    fn render_blocks_split_equals_whole() {
        let blocks = vec![
            Block::UserText("first question".into()),
            Block::AssistantText("thinking out loud".into()),
            Block::ToolUse {
                name: "Bash".into(),
                target: "ls".into(),
                diffs: vec![],
                output: Some("a\nb".into()),
                patch: None,
                read_lines: None,
                cwd: String::new(),
                execution: None,
                published: None,
            },
            Block::UserText("second question".into()),
            Block::AssistantText("done".into()),
        ];
        let times = vec![Some(1.0), Some(2.0)];
        let fold = FoldPolicy::default();
        let r = |bs: &[Block], st: &mut EmitState| {
            render_blocks(bs, &times, &fold, "", false, false, None, None, st)
        };

        let mut whole_st = EmitState::default();
        let whole = r(&blocks, &mut whole_st);

        // Split after the first turn (as a commit boundary would), carrying state across.
        let k = 3;
        let mut st = EmitState::default();
        let mut split = r(&blocks[..k], &mut st);
        split.extend(r(&blocks[k..], &mut st));

        assert_eq!(
            whole, split,
            "resumable render: split == whole (stable anchors/turns)"
        );
        assert_eq!(
            whole_st.turns, st.turns,
            "sidebar turns identical across the split"
        );
    }

    /// Emit `blocks` to the block-stream JSON (skipping the meta line) with the
    /// given fold policy and no timestamps — the shape the tests assert on. Uses
    /// `reveal = true` (served-mode shape, where file tools carry a `path`).
    fn stream(blocks: &[Block], fold: &FoldPolicy) -> Vec<Value> {
        stream_from(blocks, fold, None)
    }

    /// [`stream`] with an explicit transcript source, for tests whose blocks carry `Deferred`
    /// attachment locators that must be loaded (embedded) at emit time.
    fn stream_from(
        blocks: &[Block],
        fold: &FoldPolicy,
        transcript: Option<&Transcript>,
    ) -> Vec<Value> {
        let times: Vec<Option<f64>> = Vec::new();
        let (jsonl, _turns) = build_jsonl(
            blocks,
            &times,
            fold,
            "/repo",
            true,
            false,
            transcript,
            json!({ "t": "meta" }),
        );
        jsonl
            .lines()
            .skip(1) // meta line
            .map(|l| serde_json::from_str::<Value>(l).expect("valid JSON block line"))
            .collect()
    }

    /// ORACLE for the `/pull` meta (the byte-identity gate never drives `/pull`, so this test is
    /// the equivalence proof): the meta assembled from the engine's **maintained** `SessionMeta`
    /// must equal, as JSON, the one [`agent_meta`] derives by scanning the blocks — covering the
    /// thinking-absorbed tool count, the spawn-is-a-child-not-a-tool rule, child launch order, and
    /// `running` from both completion signals (a terminal spawn status / a later `AgentDone`).
    #[test]
    fn assemble_meta_equals_agent_meta_oracle() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = |id: &str, status| {
            Block::SubAgent(SubAgent {
                agent_id: id.into(),
                tool_use_id: format!("t_{id}"),
                agent_type: "gp".into(),
                description: format!("do {id}"),
                prompt: "go".into(),
                status,
                result: None,
                output_file: None,
                blocks: Vec::new(),
                subtree_cost: None,
            })
        };
        let tool = || Block::ToolUse {
            name: "Bash".into(),
            target: "ls".into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let blocks = vec![
            Block::UserText("first".into()),
            tool(), // 1 top-level tool
            Block::Thinking {
                text: "hm".into(),
                duration_secs: Some(2),
                tools: vec![tool(), tool()], // +2 absorbed ⇒ tools == 3
            },
            spawn("a1", AgentStatus::Running), // async child, completed by AgentDone below
            spawn("a2", AgentStatus::Completed), // sync child, terminal on the spawn itself
            Block::AgentDone {
                agent_id: "a1".into(),
                agent_type: "gp".into(),
                description: "do a1".into(),
                status: AgentStatus::Completed,
                result: None,
            },
            Block::UserText("second".into()),
        ];
        let info = AgentInfo {
            id: "agent-x".into(),
            source: std::path::PathBuf::from("/tmp/x.jsonl"),
            title: "child agent".into(),
            agent_type: "general-purpose".into(),
            ancestors: vec![("root".into(), "root title".into())],
        };
        let mut m = crate::metrics::Metrics::default();
        m.input_tokens = 1234;
        m.output_tokens = 56789;
        m.cache_read_tokens = 42;
        m.cost_usd = Some(1.234);
        m.model = "claude-x".into();
        m.duration_secs = 5;

        // Both assemblers receive the SAME task list (#15) — parity includes it.
        let tasks = crate::engine::TaskList {
            items: vec![crate::engine::TaskItem {
                id: "3".into(),
                subject: "meta parity".into(),
                status: crate::engine::TaskStatus::InProgress,
                ..Default::default()
            }],
        };
        let (oracle, _children) = agent_meta(Agent::CLAUDE, "/repo", &info, &blocks, &m, &tasks);
        let maintained = crate::engine::SessionMeta::build(&blocks);
        let got = assemble_meta(Agent::CLAUDE, "/repo", &info, &maintained, &m, &tasks);
        assert_eq!(
            got, oracle,
            "assemble_meta(SessionMeta) == agent_meta(blocks)"
        );
        assert_eq!(
            oracle["tasks"][0]["subject"], "meta parity",
            "meta carries tasks"
        );
        // Guard the fixture's coverage: the counts exercised the nested/spawn rules.
        assert_eq!(oracle["turns"], 2);
        assert_eq!(
            oracle["tools"], 3,
            "spawns excluded, absorbed tools included"
        );
        assert_eq!(oracle["children"].as_array().unwrap().len(), 2);
        assert_eq!(
            oracle["children"][0]["running"], false,
            "AgentDone cleared a1"
        );
        assert_eq!(
            oracle["children"][1]["running"], false,
            "a2 terminal on spawn"
        );
    }

    /// Write a one-line transcript to a temp file so an attachment `Deferred { at: 0 }` locator
    /// can be loaded from it. Returns the path (the caller removes it).
    fn att_transcript(line: &str) -> std::path::PathBuf {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "cr-html-att-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::File::create(&p)
            .unwrap()
            .write_all(format!("{line}\n").as_bytes())
            .unwrap();
        p
    }

    fn bash(cmd: &str, out: &str) -> Block {
        Block::ToolUse {
            name: "Bash".into(),
            target: cmd.into(),
            diffs: vec![],
            output: Some(out.into()),
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        }
    }

    fn subagent(id: &str, children: Vec<Block>) -> Block {
        use crate::model::{AgentStatus, SubAgent};
        Block::SubAgent(SubAgent {
            agent_id: id.into(),
            tool_use_id: format!("t_{id}"),
            agent_type: "gp".into(),
            description: format!("do {id}"),
            prompt: "go".into(),
            status: AgentStatus::Completed,
            result: None,
            output_file: None,
            blocks: children,
            subtree_cost: None,
        })
    }

    /// The `linked` flag emits a `child: "?session=<id>"` nav link on spawn AND
    /// completion blocks; without it (single-file `--dump-html`) no such link appears.
    #[test]
    fn linked_flag_emits_child_nav_link() {
        use crate::model::AgentStatus;
        let spawn = subagent("a1", vec![]);
        let done = Block::AgentDone {
            agent_id: "a1".into(),
            agent_type: "gp".into(),
            description: "do a1".into(),
            status: AgentStatus::Completed,
            result: None,
        };
        let times: Vec<Option<f64>> = Vec::new();
        let blocks = [spawn, done];
        // linked = true
        let (j, _) = build_jsonl(
            &blocks,
            &times,
            &FoldPolicy::none(),
            "/r",
            false,
            true,
            None,
            json!({ "t": "meta" }),
        );
        for line in j.lines().skip(1) {
            let o: Value = serde_json::from_str(line).unwrap();
            assert_eq!(o["head"]["child"], json!("?session=a1"), "line: {line}");
            assert_eq!(o["head"]["child_id"], json!("a1"));
        }
        // linked = false → no child link
        let (j2, _) = build_jsonl(
            &blocks,
            &times,
            &FoldPolicy::none(),
            "/r",
            false,
            false,
            None,
            json!({ "t": "meta" }),
        );
        for line in j2.lines().skip(1) {
            let o: Value = serde_json::from_str(line).unwrap();
            assert!(o["head"].get("child").is_none(), "no child when unlinked");
        }
    }

    /// `collect_child_refs` finds only the source's DIRECT children (grandchildren live in
    /// each child's own source and surface when that child's stream is generated).
    #[test]
    fn collect_child_refs_direct_only() {
        let blocks = vec![
            Block::UserText("root".into()),
            subagent("a1", vec![subagent("a2", vec![])]), // a2 is nested (grandchild)
            subagent("a3", vec![]),
        ];
        let ids: Vec<String> = collect_child_refs(&blocks)
            .into_iter()
            .map(|c| c.id)
            .collect();
        assert_eq!(ids, vec!["a1", "a3"], "direct children only — not a2");
    }

    /// The offline bundle materializes embedded attachments into `assets/`, decoding
    /// base64, and de-conflicts colliding names with a `-N` suffix.
    #[test]
    fn asset_sink_writes_and_deconflicts() {
        use crate::model::LoadedAttachment;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-assets-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let mut sink = AssetSink::new(&base).unwrap();
        // Text attachment → written verbatim.
        let h1 = sink
            .materialize("notes.md", None, &LoadedAttachment::Text("hi".into()))
            .unwrap();
        assert_eq!(h1, "assets/notes.md");
        assert_eq!(
            std::fs::read_to_string(base.join("assets/notes.md")).unwrap(),
            "hi"
        );
        // Same name again → de-conflicted.
        let h2 = sink
            .materialize("notes.md", None, &LoadedAttachment::Text("bye".into()))
            .unwrap();
        assert_eq!(h2, "assets/notes-1.md");
        // Base64 image → decoded; extension derived from the mime when missing.
        let h3 = sink
            .materialize(
                "shot",
                None,
                &LoadedAttachment::Base64 {
                    mime: "image/png".into(),
                    b64: "aGk=".into(), // "hi"
                },
            )
            .unwrap();
        assert_eq!(h3, "assets/shot.png");
        assert_eq!(std::fs::read(base.join("assets/shot.png")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn query_get_parses_pairs() {
        assert_eq!(query_get("session=a1&from=42", "session"), Some("a1"));
        assert_eq!(query_get("session=a1&from=42", "from"), Some("42"));
        assert_eq!(query_get("session=a1", "from"), None);
    }

    /// The multi-file shell carries `data-multi`/`data-root` and no inline snapshot;
    /// `live` adds `data-poll` (what `--html` serves); a dump's page omits it.
    #[test]
    fn build_shell_is_multi_file_without_inline() {
        let html = build_shell("My session", "root-9f3d", false, false);
        assert!(html.contains("data-multi=\"1\""), "multi flag");
        assert!(html.contains("data-root=\"root-9f3d\""));
        assert!(!html.contains("data-poll"), "static bundle does not poll");
        // Live served shell polls its stream.
        let live = build_shell("My session", "root-9f3d", true, false);
        assert!(live.contains("data-poll"), "live shell polls: {live:.0}");
        assert!(
            !live.contains("data-pull=\"1\""),
            "stream transport: no data-pull body attr"
        );
        // Live served shell in pull mode carries data-pull (the pull-client transport).
        let pull = build_shell("My session", "root-9f3d", true, true);
        assert!(
            pull.contains("data-pull=\"1\""),
            "pull transport flag: {pull:.0}"
        );
        // The inline session-data script is present but EMPTY (no block stream baked in).
        let inline = html
            .split("id=\"session-data\"")
            .nth(1)
            .and_then(|s| s.split("</script>").next())
            .unwrap_or("");
        assert!(
            !inline.contains("\"t\":\"block\""),
            "shell inlines no blocks: {inline:?}"
        );
    }

    /// #64 the bundle completeness contract: EVERY embedded, non-inline-rendered
    /// object in a transcript — prompt images, tool-result images, `file`
    /// attachments, `plan_file_reference` plans, ExitPlanMode plans — materializes
    /// into the offline bundle's assets/ with a working `att_href` link. (Path-only
    /// attachments like `edited_text_file` carry no embedded bytes — nothing to
    /// dump, by design; portable single-file `--dump-html` stays name-only.)
    #[test]
    fn bundle_materializes_every_embedded_object() {
        use std::io::Write;
        let base = std::env::temp_dir().join(format!("cr-bundle-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let sess = base.join("one-of-each.jsonl");
        // Zm9v / YmFy = "foo"/"bar" — tiny valid base64 payloads.
        let jsonl = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"look\"},{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"Zm9v\"}}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"r1\",\"name\":\"Read\",\"input\":{\"file_path\":\"/w/shot.png\"}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"r1\",\"content\":[{\"type\":\"image\",\"source\":{\"type\":\"base64\",\"media_type\":\"image/jpeg\",\"data\":\"YmFy\"}}]}]}}\n",
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"file\",\"filename\":\"/w/notes.md\",\"displayPath\":\"notes.md\",\"content\":{\"type\":\"text\",\"file\":{\"filePath\":\"/w/notes.md\",\"content\":\"# notes body\"}}}}\n",
            "{\"type\":\"attachment\",\"attachment\":{\"type\":\"plan_file_reference\",\"planFilePath\":\"/p/big-plan.md\",\"planContent\":\"# the referenced plan\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"id\":\"ep1\",\"name\":\"ExitPlanMode\",\"input\":{\"plan\":\"# the exit plan\"}}]}}\n"
        );
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(jsonl.as_bytes())
            .unwrap();
        let out = base.join("bundle");
        use clap::Parser as _;
        let args = crate::Args::parse_from([
            "claude-replay",
            sess.to_str().unwrap(),
            "--dump-all-html",
            out.to_str().unwrap(),
        ]);
        dump_all_html(&args, &sess).unwrap();
        // Every embedded object landed in assets/.
        let assets: Vec<String> = std::fs::read_dir(out.join("assets"))
            .expect("assets dir exists")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        for expect in ["notes.md", "big-plan.md", "plan.md"] {
            assert!(
                assets
                    .iter()
                    .any(|a| a.contains(expect.trim_end_matches(".md")) || a == expect),
                "missing {expect} in assets: {assets:?}"
            );
        }
        let images = assets
            .iter()
            .filter(|a| a.ends_with(".png") || a.ends_with(".jpg") || a.ends_with(".jpeg"))
            .count();
        assert!(
            images >= 2,
            "prompt + tool-result images materialized: {assets:?}"
        );
        // And each attachment record links its asset.
        let stream_file = std::fs::read_dir(&out)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .expect("agent stream present");
        let stream = std::fs::read_to_string(stream_file).unwrap();
        let hrefs = stream.matches("\"att_href\":\"assets/").count();
        assert!(
            hrefs >= 5,
            "all five embedded objects linked (got {hrefs}):\n{stream}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// End-to-end: `dump_all_html` writes `index.html` + one `<id>.jsonl` per agent, with
    /// the root's agent blocks carrying `child:` nav links and each child stream holding
    /// its own transcript.
    #[test]
    fn dump_all_html_writes_navigable_bundle() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-bundle-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sess.jsonl");
        let sadir = base.join("sess").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        let parent = r##"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"gp","description":"Dig in","prompt":"go"}}]}}
{"type":"user","toolUseResult":{"agentId":"a1","status":"async_launched"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]}}
{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>a1</task-id>\n<tool-use-id>toolu_A</tool-use-id>\n<status>completed</status>\n<summary>Agent \"Dig in\" finished</summary>\n<result>ok</result>\n</task-notification>"}
"##;
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        let child = r##"{"type":"user","message":{"content":"go"}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c1","name":"Read","input":{"file_path":"/x"}}]}}
"##;
        std::fs::File::create(sadir.join("agent-a1.jsonl"))
            .unwrap()
            .write_all(child.as_bytes())
            .unwrap();

        let out = base.join("bundle");
        use clap::Parser as _;
        let args = crate::Args::parse_from([
            "claude-replay",
            sess.to_str().unwrap(),
            "--dump-all-html",
            out.to_str().unwrap(),
        ]);
        dump_all_html(&args, &sess).unwrap();

        // Files: shell + root stream + child stream.
        assert!(out.join("index.html").is_file(), "index.html written");
        assert!(out.join("sess.jsonl").is_file(), "root stream written");
        assert!(out.join("a1.jsonl").is_file(), "child stream written");

        // Root stream: the agent blocks link to the child.
        let root = std::fs::read_to_string(out.join("sess.jsonl")).unwrap();
        let agent_lines: Vec<Value> = root
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|o| o["head"]["badge"] == json!("Agent"))
            .collect();
        assert_eq!(agent_lines.len(), 2, "spawn + completion");
        for o in &agent_lines {
            assert_eq!(o["head"]["child"], json!("?session=a1"));
        }

        // Root meta lists its children (for the "Agents ▾" menu) with a running flag, and
        // has no ancestors (it is the root breadcrumb origin).
        let root_meta: Value = serde_json::from_str(root.lines().next().unwrap()).unwrap();
        assert_eq!(root_meta["ancestors"], json!([]), "root has no ancestors");
        assert_eq!(root_meta["children"][0]["id"], json!("a1"));
        assert_eq!(
            root_meta["children"][0]["running"],
            json!(false),
            "a1 completed → not running"
        );

        // Child stream: its own meta + the Read tool block; its ancestry points at the root.
        let cj = std::fs::read_to_string(out.join("a1.jsonl")).unwrap();
        let recs: Vec<Value> = cj
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect();
        assert_eq!(recs[0]["sid"], json!("a1"), "child meta sid");
        // #81: every live/bundle meta carries its transcript path — the header's
        // click-to-copy writes THIS (an absent path used to flash "copied" over an
        // empty clipboard write).
        assert!(
            recs[0]["path"]
                .as_str()
                .is_some_and(|p| p.ends_with(".jsonl")),
            "child meta path: {:?}",
            recs[0]["path"]
        );
        assert_eq!(
            recs[0]["ancestors"],
            json!([{ "id": "sess", "title": "sess · compatible (claude)" }]),
            "child breadcrumb points at the root (titled by session name + agent)"
        );
        // The lone Read folds into an activity-span record (#57); its tool block
        // rides nested inside the span's body.
        assert!(
            cj.contains(r#""tool":"Read""#),
            "child transcript rendered: {recs:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn dump_all_html_keeps_reused_codex_agent_paths_distinct() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-codex-bundle-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let sessions = base.join("sessions/2026/07/29");
        std::fs::create_dir_all(&sessions).unwrap();
        let parent = sessions.join("rollout-parent.jsonl");
        let child_a = sessions.join("rollout-child-a.jsonl");
        let child_b = sessions.join("rollout-child-b.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"review first\"}"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-a","agent_path":"/root/review","kind":"started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-2","arguments":"{\"task_name\":\"review\",\"message\":\"review second\"}"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-2","agent_thread_id":"child-b","agent_path":"/root/review","kind":"started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-2","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n",
            ),
        )
        .unwrap();
        for (child, id, body) in [
            (&child_a, "child-a", "first child body"),
            (&child_b, "child-b", "second child body"),
        ] {
            std::fs::write(
                child,
                format!(
                    "{}\n{}\n",
                    json!({
                        "type": "session_meta",
                        "payload": {
                            "id": id,
                            "cwd": "/repo",
                            "source": {
                                "subagent": {
                                    "thread_spawn": {
                                        "parent_thread_id": "parent",
                                        "agent_path": "/root/review",
                                        "agent_nickname": "Nash"
                                    }
                                }
                            }
                        }
                    }),
                    json!({
                        "type": "response_item",
                        "payload": {
                            "type": "message",
                            "role": "user",
                            "content": [{"type": "input_text", "text": body}]
                        }
                    })
                ),
            )
            .unwrap();
        }

        let out = base.join("bundle");
        use clap::Parser as _;
        let args = crate::Args::parse_from([
            "claude-replay",
            parent.to_str().unwrap(),
            "--dump-all-html",
            out.to_str().unwrap(),
        ]);
        dump_all_html(&args, &parent).unwrap();

        assert!(out.join("index.html").is_file());
        assert!(out.join("rollout-parent.jsonl").is_file());
        assert!(out.join("child-a.jsonl").is_file());
        assert!(out.join("child-b.jsonl").is_file());

        let root = std::fs::read_to_string(out.join("rollout-parent.jsonl")).unwrap();
        let root_meta: Value = serde_json::from_str(root.lines().next().unwrap()).unwrap();
        assert_eq!(root_meta["agent"], json!("codex"));
        assert_eq!(
            root_meta["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(|child| child["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["child-a", "child-b"]
        );
        assert!(
            root_meta["children"]
                .as_array()
                .unwrap()
                .iter()
                .all(|child| child["running"] == json!(true)),
            "persistent Codex agents have no terminal lifecycle event"
        );
        let links = root
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|record| record["head"]["badge"] == json!("Agent"))
            .filter_map(|record| record["head"]["child"].as_str().map(str::to_string))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(
            links,
            [
                "?session=child-a".to_string(),
                "?session=child-b".to_string()
            ]
            .into_iter()
            .collect()
        );

        for (child_id, body) in [
            ("child-a", "first child body"),
            ("child-b", "second child body"),
        ] {
            let child_stream =
                std::fs::read_to_string(out.join(format!("{child_id}.jsonl"))).unwrap();
            let child_meta: Value =
                serde_json::from_str(child_stream.lines().next().unwrap()).unwrap();
            assert_eq!(child_meta["sid"], json!(child_id));
            assert_eq!(child_meta["ancestors"][0]["id"], json!("rollout-parent"));
            assert!(
                child_stream.contains(body),
                "{child_id} transcript rendered"
            );
        }

        std::fs::remove_dir_all(base).unwrap();
    }

    /// Regression: a **synchronous** `Agent` spawn signals completion *inline* on its
    /// `tool_result` (`toolUseResult.status == "completed"` + `agentId`), with **no** later
    /// `<task-notification>` and therefore no `AgentDone` block. The "Agents ▾" running flag
    /// must still read `false` — it reads the spawn's own terminal status, not only the
    /// (absent) `AgentDone`. Before the fix, such an agent showed "running" forever.
    #[test]
    fn sync_agent_spawn_is_not_running() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-syncagent-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sess.jsonl");
        let sadir = base.join("sess").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        // Spawn + inline completed result (mirrors the real `Agent` tool: no task-notification).
        let parent = r##"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_S","name":"Agent","input":{"subagent_type":"gp","description":"Map APIs","prompt":"go"}}]}}
{"type":"user","toolUseResult":{"agentId":"s1","status":"completed","content":"the report"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_S","content":"the report"}]}}
"##;
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(parent.as_bytes())
            .unwrap();
        std::fs::File::create(sadir.join("agent-s1.jsonl"))
            .unwrap()
            .write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"go\"}}\n")
            .unwrap();

        let out = base.join("bundle");
        use clap::Parser as _;
        let args = crate::Args::parse_from([
            "claude-replay",
            sess.to_str().unwrap(),
            "--dump-all-html",
            out.to_str().unwrap(),
        ]);
        dump_all_html(&args, &sess).unwrap();

        let root = std::fs::read_to_string(out.join("sess.jsonl")).unwrap();
        // Exactly ONE agent line (the spawn) — no separate completion event exists.
        let agent_lines = root
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .filter(|o| o["head"]["badge"] == json!("Agent"))
            .count();
        assert_eq!(
            agent_lines, 1,
            "sync spawn has no separate completion block"
        );
        let root_meta: Value = serde_json::from_str(root.lines().next().unwrap()).unwrap();
        assert_eq!(root_meta["children"][0]["id"], json!("s1"));
        assert_eq!(
            root_meta["children"][0]["running"],
            json!(false),
            "sync Agent completed inline → not running"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    fn tool(name: &str, target: &str) -> Block {
        Block::ToolUse {
            name: name.into(),
            target: target.into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        }
    }

    /// A sub-agent spawn emits the "Agent" badge + `type: description` preview + a
    /// "launched" chip (never the terminal status); its completion emits a separate
    /// AgentDone block with the "completed" chip. The JS renders badge+preview via its
    /// `head.badge` branch.
    #[test]
    fn agent_spawn_and_done_emit_badge_preview_and_status_chip() {
        use crate::model::{AgentStatus, SubAgent};
        let spawn = Block::SubAgent(SubAgent {
            agent_id: "a1".into(),
            tool_use_id: "t".into(),
            agent_type: "general-purpose".into(),
            description: "Design the engine".into(),
            prompt: "go".into(),
            status: AgentStatus::Completed, // back-patched; must NOT surface as "done"
            result: None,
            output_file: None,
            blocks: vec![],
            subtree_cost: None,
        });
        let s = stream(&[spawn], &FoldPolicy::none());
        assert_eq!(s[0]["head"]["badge"], json!("Agent"));
        assert_eq!(
            s[0]["head"]["preview"],
            json!("general-purpose: Design the engine")
        );
        assert_eq!(s[0]["head"]["chips"], json!([{ "x": "launched" }]));

        let done = Block::AgentDone {
            agent_id: "a1".into(),
            agent_type: "general-purpose".into(),
            description: "Design the engine".into(),
            status: AgentStatus::Completed,
            result: Some("done.".into()),
        };
        let d = stream(&[done], &FoldPolicy::none());
        assert_eq!(d[0]["head"]["badge"], json!("Agent"));
        assert_eq!(d[0]["head"]["chips"], json!([{ "x": "completed" }]));
    }

    #[test]
    fn every_tool_fold_carries_its_display_name_as_data_tool() {
        // The `tool` field drives the client-side tool-use filter; it must match the
        // fold header's display name (Edit/MultiEdit → "Update", others verbatim).
        let cases = [
            ("Bash", "Bash"),
            ("Read", "Read"),
            ("Edit", "Update"),
            ("MultiEdit", "Update"),
            ("Write", "Write"),
            ("Skill", "Skill"),
            ("Task", "Task"),
            ("Agent", "Agent"),
            ("WebFetch", "WebFetch"), // a generic tool keeps its own name
        ];
        for (name, want) in cases {
            let out = stream(&[tool(name, "x")], &FoldPolicy::none());
            assert_eq!(out[0]["tool"], json!(want), "tool={name}");
        }
        // Non-tool blocks carry no `tool` attribute.
        let out = stream(&[Block::AssistantText("hi".into())], &FoldPolicy::none());
        assert!(
            out[0].get("tool").is_none(),
            "assistant text has no data-tool"
        );
    }

    fn edit_with_patch() -> Block {
        Block::ToolUse {
            name: "Edit".into(),
            target: "src/x.rs".into(),
            diffs: vec![],
            output: None,
            patch: Some(vec![Hunk {
                old_start: 10,
                new_start: 10,
                lines: vec![
                    " context".into(),
                    "-gone".into(),
                    "+added one".into(),
                    "+added two".into(),
                ],
            }]),
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        }
    }

    #[test]
    fn fold_structure_marks_kind_and_default_open() {
        let fold = FoldPolicy::default();
        let blocks = vec![
            Block::UserText("hi".into()),
            bash("ls", "a\nb"),
            edit_with_patch(),
        ];
        let out = stream(&blocks, &fold);

        // User prose: an always-open card, not a fold.
        assert_eq!(out[0]["kind"], "user");
        assert!(out[0].get("fold").is_none(), "user turn is not a fold");

        // Bash folds by default (data-open 0); Edit opens by default (data-open 1).
        assert_eq!(out[1]["kind"], "bash");
        assert_eq!(out[1]["fold"], json!(true));
        assert_eq!(out[1]["open"], json!(0), "bash starts collapsed");

        assert_eq!(out[2]["kind"], "edit");
        assert_eq!(out[2]["open"], json!(1), "edit starts expanded");

        // --full unfolds everything.
        let full = stream(&blocks, &FoldPolicy::none());
        assert_eq!(full[1]["open"], json!(1), "--full opens bash too");
    }

    #[test]
    fn tool_execution_status_and_duration_are_structured_header_chips() {
        use crate::model::{ToolDuration, ToolExecution, ToolStatus};
        let mut block = bash("cargo test", "boom");
        let Block::ToolUse { execution, .. } = &mut block else {
            unreachable!()
        };
        *execution = Some(ToolExecution {
            status: Some(ToolStatus::Failed),
            exit_code: Some(7),
            duration: Some(ToolDuration {
                secs: 0,
                nanos: 42_000_000,
            }),
        });
        let out = stream(&[block], &FoldPolicy::default());
        assert!(out[0]["head"]["chips"]
            .as_array()
            .unwrap()
            .contains(&json!({"c": "fail", "x": "exit 7 · 42ms"})));
    }

    /// §8.8 per-kind keylines: the emitter tags each fold with `kind` (→ `data-kind`),
    /// and the stylesheet paints a per-kind keyline in BOTH fold states, with the
    /// filter hit applied as a CLASS override (`!important`), never an inline box-shadow
    /// — so the two rules don't fight.
    #[test]
    fn per_kind_keyline_attr_and_class() {
        let out = stream(
            &[edit_with_patch(), bash("ls", "x"), tool("Read", "/f")],
            &FoldPolicy::none(),
        );
        assert_eq!(out[0]["kind"], "edit");
        assert_eq!(out[1]["kind"], "bash");
        assert_eq!(out[2]["kind"], "read");
        // The keyline hues are keyed off data-kind in the stylesheet, in both states.
        assert!(CSS.contains(".fold[data-kind=\"edit\"]"), "edit keyline");
        assert!(CSS.contains(".fold[data-kind=\"read\"]"), "read keyline");
        assert!(
            CSS.contains(".fold-h.filter-hit") && CSS.contains("!important"),
            "filter hit must be a class override, not inline"
        );
        // No inline box-shadow keyline may be emitted (it would beat the class rule).
        assert!(
            !out.iter().any(|b| b.to_string().contains("box-shadow")),
            "keylines come from the stylesheet, not inline"
        );
    }

    /// §8.2 an authored-open fold emits `open:1` (the driver), and the renderer sets its
    /// header target to the expanded pre-wrap form inline — collapsed is one ellipsized
    /// line, expanded wraps — synced by `setFold` on every toggle.
    #[test]
    fn open_fold_wrap_contract() {
        // Edit opens by default.
        let out = stream(&[edit_with_patch()], &FoldPolicy::default());
        assert_eq!(out[0]["open"], json!(1));
        // The JS renders the expanded target inline and keeps it in sync.
        assert!(
            JS.contains("pre-wrap") && JS.contains("overflowWrap"),
            "setFold/renderBlock sync the expanded header target inline"
        );
        // The stylesheet no longer hard-codes the expanded target via a data-open rule
        // (that treatment moved to inline JS per §8.2).
        assert!(
            !CSS.contains(".fold[data-open=\"1\"] > .fold-h .tool-target"),
            "expanded target is inline now, not a CSS descendant rule"
        );
    }

    /// The fixed top bar abbreviates a bare session UUID to its first group. Codex puts the
    /// same UUID at the end of `rollout-<datetime>-<uuid>`, so that trailing group must win
    /// over blindly taking the stem's first eight characters (`rollout-`).
    /// #98: unpinned, a growth above the viewport that nobody scrolled for (an image decoding,
    /// a late reflow) is healed by putting the last anchor the reader settled on back at its
    /// offset — the same body observer that heals the pinned tail. The anchor is refreshed per
    /// scroll frame and per apply, and cleared the moment a scroll begins so a resize heard
    /// between a scroll and its frame cannot undo the scroll.
    /// #114: the classic page's small cap expansions ride with folds and raws across a reload.
    #[test]
    fn cap_expansions_survive_a_reload() {
        assert!(JS.contains("caps: Array.from(capOpen), seen: records.length"));
        assert!(JS.contains("if (Array.isArray(vs0.caps)) capOpen = new Set(vs0.caps);"));
    }

    /// #112: the classic page's time formatting is the shared module's.
    #[test]
    fn times_are_the_shared_modules() {
        assert!(JS.contains("var fmtTime = shared.fmtTime;"));
        assert!(JS.contains("var fmtDur = shared.fmtDur;"));
        assert!(super::shared::SHARED
            .iter()
            .any(|(name, _)| *name == "time"));
    }

    /// #111: the classic page's haystack rules are the shared module's.
    #[test]
    fn search_haystack_is_the_shared_modules() {
        assert!(
            JS.contains("function ownTextParts(b) { return shared.ownTextParts(b, stripHtml); }")
        );
        assert!(JS.contains("var directMask = shared.directMask;"));
        assert!(JS.contains("var countOcc = shared.countOcc;"));
        assert!(super::shared::SHARED
            .iter()
            .any(|(name, _)| *name == "search"));
    }

    /// #109: the classic page's "user turns as raw" preference is the shared reading
    /// preference (one key with the app shell), its pre-#109 key folded in once.
    #[test]
    fn raw_user_turns_is_a_shared_reading_preference() {
        assert!(JS.contains("var rawUser = reading.rawUser;"));
        assert!(JS.contains("wide: \"claude-replay-export-wide\", rawUser: RAW_KEY }, lsDel);"));
        assert!(
            JS.contains("JSON.stringify({ size: ms, wrap: wrap, wide: wide, rawUser: rawUser })")
        );
        assert!(!JS.contains("lsSet(RAW_KEY"));
    }

    /// #108: the classic page's caps run on the shared module — split, label, rows, memory —
    /// keeping its own class names and its DOM-walking ordinal stamping.
    #[test]
    fn caps_are_the_shared_modules() {
        assert!(JS.contains("var split = shared.capSplit(rows, cap);"));
        assert!(JS.contains(
            "var btn = el(\"button\", \"morebtn\", shared.capLabel(split.hidden.length, toLine));"
        ));
        assert!(JS.contains(
            "return shared.numRowsHtml(rows, CLASSIC_ROWS); // Rust-escaped + syntect spans"
        ));
        assert!(JS.contains("return shared.diffRowsHtml(rows, CLASSIC_ROWS, CLASSIC_MARKS);"));
        assert!(JS.contains("var MAX_BUFFER_LINES = shared.MAX_BUFFER_LINES;"));
        assert!(JS.contains("if (shared.capOpenHas(capOpen, recId, k)) expandMore(btns[k]);"));
        assert!(JS.contains("shared.rememberCap(capOpen, blk67.id, more.dataset.ord, hiddenLineCount(hidden67, more));"));
        assert!(super::shared::SHARED
            .iter()
            .any(|(name, _)| *name == "parts"));
    }

    #[test]
    fn unpinned_growth_is_healed_by_the_last_anchor() {
        assert!(JS.contains("else if (!following && viewAnchor) restoreAnchor(viewAnchor);"));
        assert!(JS.contains("viewAnchor = null; // this scroll moved the reader"));
        assert!(JS.contains("      spy();\n      viewAnchor = captureAnchor();\n    });"));
        assert!(JS.contains("    viewAnchor = captureAnchor();\n    spy();\n  }"));
    }

    #[test]
    fn topbar_snips_codex_rollout_to_uuid_prefix() {
        // The shortener is a shared module since #50 (html/shared/ids.js), inlined into every
        // page ahead of the page script; the page reads it rather than keeping a copy.
        let ids = super::shared::shared_source("ids").expect("the ids module is in SHARED");
        assert!(
            ids.contains(r"(?:^|-)([0-9a-f]{8})-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
                && ids.contains("return uuid ? uuid[1]")
                && JS.contains("var snipId = shared.snipId;"),
            "the session id shortener extracts a trailing UUID's first group"
        );
    }

    /// The search box supports the `uatobrew:` scope prefix (same syntax as the TUI's
    /// `/` search): a run of distinct letters — u (user+command), a (assistant),
    /// t (think+act), o (all tool kinds), b (bash), r (reads), e (edits+writes) — in
    /// any order, with a leading `:` escaping a scope-shaped literal and a PURE run
    /// searching itself. The contract with the JS: the one parser, the kind-based
    /// gate, and a tooltip that teaches the syntax.
    #[test]
    fn search_supports_scoped_and_whole_word_prefixes() {
        // #111: the scope classes and the word rules moved to html/shared/search.js; the
        // scope PARSER stays the page's.
        let search = super::shared::shared_source("search").unwrap();
        #[allow(non_snake_case)]
        let SEARCH = search;
        assert!(
            SEARCH.contains(r"/^([uatobrew+]{1,15}):/i")
                && SEARCH.contains("function parseScope")
                && JS.contains("var parseScope = shared.parseScope;"),
            "the order-free letter-run grammar is the one parser — the shared module's (#101)"
        );
        assert!(
            SEARCH.contains(r#"if (needle.charAt(0) === ":") return { set: null, len: 1 };"#),
            "a leading colon escapes a scope-shaped literal"
        );
        assert!(
            JS.contains("if (scoped.set && !rest.length) scoped = null;"),
            "a pure scope run searches itself — the scope reading has nothing to search"
        );
        assert!(
            SEARCH.contains("function directMask")
                && SEARCH.contains(r#"k === "user" || k === "command""#)
                && SEARCH.contains(r#"k === "assistant""#)
                && SEARCH.contains(r"^(bash|edit|write|read|skill|tool)$")
                && SEARCH.contains(r#"k === "edit" || k === "write""#),
            "scope gating maps u/a/t/o/b/r/e onto the record kinds via one class table"
        );
        assert!(
            JS.contains("recSearchParts")
                && JS.contains("function countRec")
                && JS.contains("each absorbed child tool owns its command/output"),
            "thinking prose and absorbed tool content have independent search projections"
        );
        assert!(
            SEARCH.contains("function wholeAt")
                && SEARCH.contains("WORD_LEFT")
                && JS.contains("whole words"),
            "w: uses explicit Unicode-aware word boundaries"
        );
        assert!(
            build_shell("t", "root", false, false).contains("uatobrew: prefix"),
            "the search box tooltip mentions the scope syntax"
        );
    }

    /// The scope's visible face: seven type checkboxes plus a whole-word toggle that
    /// rewrite the `uatobrew:` prefix in the box,
    /// and lights up reading back the active letters when a prefix is typed by hand.
    /// The box stays the single source of truth — one parser feeds both faces.
    #[test]
    fn search_scope_dropdown_mirrors_the_prefix() {
        let shell = build_shell("t", "root", false, false);
        for id in [
            "id=\"qscope\"",
            "id=\"qscopemenu\"",
            "id=\"qs-u\"",
            "id=\"qs-a\"",
            "id=\"qs-t\"",
            "id=\"qs-o\"",
            "id=\"qs-b\"",
            "id=\"qs-r\"",
            "id=\"qs-e\"",
            "id=\"qs-w\"",
        ] {
            assert!(
                shell.contains(id),
                "the dropdown is in the search box: {id}"
            );
        }
        assert!(
            shell.contains("user messages")
                && shell.contains("agent responses")
                && shell.contains("thinking")
                && shell.contains("all tools")
                && shell.contains("bash output")
                && shell.contains("read content")
                && shell.contains("file edits/writes")
                && shell.contains("whole words (w)"),
            "the type choices and whole-word modifier are named, letters included"
        );
        assert!(
            JS.contains("applyScopeFromMenu") && JS.contains("syncQScope"),
            "checkbox changes rewrite the prefix; typing re-checks the boxes"
        );
        assert!(
            JS.contains("classCounts") && JS.contains("function updateScopeCounts"),
            "every choice shows the current query's per-class hit count"
        );
        for id in [
            "qsn-u", "qsn-a", "qsn-t", "qsn-o", "qsn-b", "qsn-r", "qsn-e",
        ] {
            assert!(shell.contains(id), "the count slot exists: {id}");
        }
        assert!(
            shell.contains("<svg") && !shell.contains("scope \u{25be}"),
            "the trigger is a funnel icon, not a letter readback"
        );
        assert!(
            JS.contains("search(q.value); syncQScope();"),
            "typing re-derives the dropdown state from the box"
        );
        assert!(
            CSS.contains(".qscope.on") && CSS.contains("#qscopemenu.on"),
            "the active trigger and the open menu are styled"
        );
    }

    /// Search stepping is RECORD-first and EVERY press MOVES: the hit-record list (from
    /// the record-text counts) drives the ▲▼/⏎ walk; per record the walk visits each
    /// rendered mark once — or the record itself, once, when the DOM could mark nothing —
    /// then crosses to the next hit record. No record is skipped (the old walk cycled
    /// among the few markable blocks and rebuilt the window once per skip, freezing the
    /// page) and no press stalls in place (an occurrence the DOM cannot address is not a
    /// separate stop).
    #[test]
    fn search_stepping_is_record_first_and_every_press_moves() {
        assert!(
            JS.contains("EVERY press MOVES"),
            "the stall-free contract is stated where the walk lives"
        );
        assert!(
            JS.contains("never skip"),
            "a mark-less hit record gets a landing"
        );
        assert!(
            JS.contains("at most one") && !JS.contains("while (tries++"),
            "boundary crosses are bounded — the unbounded skip scan is gone"
        );
    }

    /// A queued in-flight prompt streams as kind "queue" — an always-open marker
    /// (not a fold, no turn id), carrying the prompt text as its body.
    #[test]
    fn queue_marker_streams_as_always_open_kind() {
        let blocks = vec![Block::QueueEvent {
            text: "fix the table".into(),
        }];
        let out = stream(&blocks, &FoldPolicy::default());
        assert_eq!(out[0]["kind"], "queue");
        assert!(out[0].get("fold").is_none(), "queue marker is not a fold");
        assert!(out[0].get("turn").is_none(), "queue marker is not a turn");
        let html = out[0]["body"][0]["h"].as_str().unwrap();
        assert!(html.contains("fix the table"), "carries the text: {html}");
    }

    /// A surfaced attachment streams as kind "attachment". On a served page it carries
    /// the payload (`att_text`) or reveal path to act on; on a portable export
    /// (`reveal == false`) only the name is emitted.
    #[test]
    fn attachment_streams_payload_only_when_served() {
        // A real transcript carrying the `file` body; the block holds only a `Deferred` locator.
        let line = r#"{"type":"attachment","attachment":{"type":"file","filename":"/w/notes.md","displayPath":"notes.md","content":{"type":"text","file":{"filePath":"/w/notes.md","content":"hello"}}}}"#;
        let tpath = att_transcript(line);
        let src = Transcript::open(crate::Agent::CLAUDE, &tpath);
        let file = Block::Attachment(crate::model::Attachment {
            kind: crate::model::AttachmentKind::File,
            name: "notes.md".into(),
            path: Some("/w/notes.md".into()),
            content: AttachmentContent::Deferred {
                at: 0,
                index: 0,
                span: None,
            },
        });
        // Served (reveal = true): name + downloadable flag + inline text (loaded) + path.
        let served = stream_from(std::slice::from_ref(&file), &FoldPolicy::none(), Some(&src));
        assert_eq!(served[0]["kind"], "attachment");
        assert!(served[0].get("fold").is_none(), "attachment is not a fold");
        let h = &served[0]["head"];
        assert_eq!(h["att_name"], "notes.md");
        assert_eq!(h["att_dl"], json!(true));
        assert_eq!(h["att_text"], "hello");
        assert_eq!(h["att_path"], "/w/notes.md");

        // Exported (reveal = false): name/kind only — no bytes, no path (portable).
        let times: Vec<Option<f64>> = Vec::new();
        let (jsonl, _) = build_jsonl(
            std::slice::from_ref(&file),
            &times,
            &FoldPolicy::none(),
            "/w",
            false,
            false,
            None,
            json!({ "t": "meta" }),
        );
        let rec: Value = serde_json::from_str(jsonl.lines().nth(1).unwrap()).unwrap();
        assert_eq!(rec["head"]["att_name"], "notes.md");
        assert!(
            rec["head"].get("att_text").is_none(),
            "no bytes when exported"
        );
        assert!(
            rec["head"].get("att_path").is_none(),
            "no path when exported"
        );
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn everything_is_html_escaped() {
        let blocks = vec![Block::UserText(
            "danger <script>alert(1)</script> & \"quotes\" and <b>x</b>".into(),
        )];
        let out = stream(&blocks, &FoldPolicy::none());
        let html = out[0]["body"][0]["h"].as_str().unwrap();
        assert!(html.contains("&lt;script&gt;"), "tag escaped: {html}");
        assert!(!html.contains("<script>"), "no raw script tag: {html}");
        assert!(html.contains("&amp;"), "ampersand escaped: {html}");

        // The page wrapper must also neutralize a literal `</script>` in the
        // payload so it can't close the data island early.
        let page = build_html("t", "{\"x\":\"</script>\"}", &[], None);
        assert!(
            !page.contains("\"</script>\"}"),
            "payload </script> broken up"
        );
        assert!(page.contains("<\\/script>"));
    }

    #[test]
    fn proposed_plan_is_projected_without_literal_transport_tags() {
        let out = stream(
            &[Block::AssistantMessage {
                text: "<proposed_plan>\n# Safe migration\n\n- Keep fallback\n</proposed_plan>"
                    .into(),
                phase: AssistantPhase::Final,
                inferred: false,
            }],
            &FoldPolicy::none(),
        );
        assert_eq!(out[0]["head"]["presentation"], "proposed_plan");
        let html = out[0]["body"][0]["h"].as_str().unwrap();
        assert!(html.contains("Safe migration"));
        assert!(!html.contains("proposed_plan"));
    }

    #[test]
    fn request_user_input_answer_has_a_structured_projection() {
        let block = Block::ToolUse {
            name: "request_user_input".into(),
            target: String::new(),
            diffs: vec![],
            output: Some(
                r#"{"answers":{"rollout_entry":{"answers":["恢复 Classic 默认 (Recommended)"]}}}"#
                    .into(),
            ),
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let out = stream(&[block], &FoldPolicy::none());
        assert_eq!(out[0]["head"]["interaction"]["kind"], "request_user_input");
        assert_eq!(out[0]["head"]["interaction"]["resolved"], true);
        assert_eq!(
            out[0]["head"]["interaction"]["answers"][0]["label"],
            "恢复 Classic 默认 (Recommended)"
        );
    }

    #[test]
    fn diff_rows_classify_add_del_context_with_real_line_numbers() {
        let out = stream(&[edit_with_patch()], &FoldPolicy::none());
        let body = out[0]["body"].as_array().unwrap();
        // First body part is the `⎿ Added…` note; the diff part follows.
        let diff = body.iter().find(|p| p["p"] == "diff").expect("diff part");
        let rows = diff["rows"].as_array().unwrap();
        // Context advances both sides (to old/new line 11), so the deletion is
        // old-line 11 and the insertions are new-lines 11 and 12 — the shared
        // `diff_row_groups` numbering the TUI renders too.
        assert_eq!(rows[0], json!(["ctx", 10, "context"]));
        assert_eq!(rows[1], json!(["del", 11, "gone"]));
        assert_eq!(rows[2], json!(["add", 11, "added one"]));
        assert_eq!(rows[3], json!(["add", 12, "added two"]));

        // Header chips report the tallies.
        let chips = out[0]["head"]["chips"].as_array().unwrap();
        assert!(chips.contains(&json!({ "c": "add", "x": "+2" })));
        assert!(chips.contains(&json!({ "c": "del", "x": "−1" })));
    }

    #[test]
    fn diff_without_patch_uses_local_numbering() {
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "f".into(),
            diffs: vec![("old line\nkeep".into(), "keep\nnew line".into())],
            output: None,
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let out = stream(&[block], &FoldPolicy::none());
        let diff = out[0]["body"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["p"] == "diff")
            .unwrap();
        let rows = diff["rows"].as_array().unwrap();
        // A deletion has no new-side number (null gutter); insertions/context do.
        assert!(rows.iter().any(|r| r[0] == "del" && r[1].is_null()));
        assert!(rows.iter().any(|r| r[0] == "add" && r[1].is_number()));
    }

    #[test]
    fn user_turn_timestamps_thread_through_in_order() {
        let blocks = vec![
            Block::UserText("first".into()),
            Block::AssistantText("reply".into()),
            Block::UserText("second".into()),
        ];
        let times = vec![Some(1000.0), Some(2000.0)]; // one per user turn
        let (jsonl, turns) = build_jsonl(
            &blocks,
            &times,
            &FoldPolicy::none(),
            "/repo",
            true,
            false,
            None,
            json!({ "t": "meta" }),
        );
        let objs: Vec<Value> = jsonl
            .lines()
            .skip(1)
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(objs[0]["ts"], json!(1000.0));
        assert!(objs[1].get("ts").is_none(), "assistant text has no ts");
        assert_eq!(objs[2]["ts"], json!(2000.0));
        // Both user turns feed the sidebar.
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].id, "t1");
        assert_eq!(turns[1].id, "t2");
    }

    /// #108: the compaction divider is a fold whose header is the seam text and whose body is
    /// the continuation summary — and it is NOT a turn: no `t<N>` id, no `turn` field, no
    /// turn-numbered sidebar row. Its `epoch` flag is what puts a chapter break in the
    /// sidebar instead, both server-rendered and (via the record) in the live renderer.
    #[test]
    fn compaction_emits_an_epoch_divider_that_is_not_a_turn() {
        let blocks = vec![
            Block::UserText("before".into()),
            Block::Compaction {
                trigger: crate::model::CompactTrigger::Auto,
                pre_tokens: 996_000,
                post_tokens: 18_000,
                summary: "continued…".into(),
            },
            Block::UserText("after".into()),
        ];
        let (jsonl, turns) = build_jsonl(
            &blocks,
            &[Some(1000.0), Some(2000.0)],
            &FoldPolicy::default(),
            "/repo",
            true,
            false,
            None,
            json!({ "t": "meta" }),
        );
        let objs: Vec<Value> = jsonl
            .lines()
            .skip(1)
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let d = &objs[1];
        assert_eq!(d["kind"], json!("compaction"));
        assert_eq!(d["epoch"], json!(true));
        assert_eq!(d["fold"], json!(true));
        assert_eq!(d["open"], json!(0), "collapsed by the default policy");
        assert_eq!(
            d["head"]["summary"],
            json!("context compacted · auto · 996.0k → 18.0k")
        );
        assert!(d["body"][0]["h"].as_str().unwrap().contains("continued"));
        assert!(d.get("turn").is_none(), "a compaction is not a turn: {d}");
        // The two REAL turns keep `t1`/`t2` — the divider must not consume a turn number,
        // or every deep link after a compaction would shift.
        assert_eq!(objs[0]["id"], json!("t1"));
        assert_eq!(objs[2]["id"], json!("t2"));
        // Sidebar: turn, epoch seam, turn — the epoch sits between them, unnumbered.
        let rows: Vec<(&str, bool)> = turns
            .iter()
            .map(|e| (e.id.as_str(), e.epoch))
            .collect::<Vec<_>>();
        assert_eq!(rows, vec![("t1", false), ("b1", true), ("t2", false)]);
    }

    #[test]
    fn write_body_keeps_full_content_behind_the_cap() {
        // 30 lines, cap is WRITE_PREVIEW (10) — but grep-ability means ALL rows
        // must be present in the file (the JS hides the tail; it isn't dropped).
        let content: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        let block = Block::ToolUse {
            name: "Write".into(),
            target: "out.txt".into(),
            diffs: vec![(String::new(), content)],
            output: None,
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        let out = stream(&[block], &FoldPolicy::none());
        let num = out[0]["body"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["p"] == "num")
            .unwrap();
        assert_eq!(
            num["rows"].as_array().unwrap().len(),
            30,
            "all 30 rows emitted, not truncated"
        );
        assert_eq!(num["cap"], json!(WRITE_PREVIEW));
    }

    #[test]
    fn markdown_renders_tables_lists_and_fences_natively() {
        let md =
            "# Title\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- one\n- two\n\n```rs\nlet x = 1;\n```";
        let html = md_html(md);
        assert!(html.contains("<table>") && html.contains("<th>"), "{html}");
        assert!(html.contains("<ul><li>one</li>"), "{html}");
        assert!(html.contains("class=\"fence\"") && html.contains("class=\"cpy\""));
        assert!(html.contains("class=\"md-h1\""));
    }

    #[test]
    fn activity_summary_reuses_the_tui_string() {
        let think = Block::Thinking {
            text: "reasoned".into(),
            duration_secs: Some(5),
            tools: vec![bash("ls", "x")],
        };
        let out = stream(&[think], &FoldPolicy::none());
        assert_eq!(out[0]["kind"], "act");
        let summary = out[0]["head"]["summary"].as_str().unwrap();
        assert!(
            summary.starts_with("✻ Thought for 5s, listed 1 directory"),
            "{summary}"
        );
        // The absorbed Bash rides along as a nested block part.
        let has_nested = out[0]["body"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["p"] == "blocks");
        assert!(has_nested, "nested tool blocks present");
    }

    /// Regression: in an offline bundle an image attachment must materialize to `assets/`
    /// and carry `att_kind:"image"` + `att_href` (no `att_datauri`) — that is exactly the
    /// pair the JS now uses to render the image inline, matching the served page.
    #[test]
    fn bundle_image_attachment_emits_href_for_inline_render() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-bimg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A real transcript carrying the base64 image; the block holds only a locator.
        let line = r#"{"type":"user","message":{"content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGk="}}]}}"#;
        let tpath = att_transcript(line);
        let src = Transcript::open(crate::Agent::CLAUDE, &tpath);
        let img = Block::Attachment(crate::model::Attachment {
            kind: crate::model::AttachmentKind::Image,
            name: "image.png".into(),
            path: None,
            content: AttachmentContent::Deferred {
                at: 0,
                index: 0,
                span: None,
            },
        });
        let mut sink = AssetSink::new(&base).unwrap();
        let (jsonl, _) = build_jsonl_inner(
            std::slice::from_ref(&img),
            &[],
            &FoldPolicy::none(),
            "/w",
            false, // exported/bundle (not served)
            false,
            Some(&mut sink),
            Some(&src),
            json!({ "t": "meta" }),
        );
        let rec: Value = serde_json::from_str(jsonl.lines().nth(1).unwrap()).unwrap();
        let h = &rec["head"];
        assert_eq!(h["att_kind"], "image");
        assert!(
            h.get("att_href").and_then(|v| v.as_str()).is_some(),
            "bundled image links to assets/: {h}"
        );
        assert!(h.get("att_datauri").is_none(), "no data URI in a bundle");
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn file_tools_get_an_absolute_path_link_but_bash_does_not() {
        let blocks = vec![
            Block::ToolUse {
                name: "Edit".into(),
                target: "src/x.rs".into(), // relative → resolved against cwd
                diffs: vec![("a".into(), "b".into())],
                output: None,
                patch: None,
                read_lines: None,
                cwd: String::new(),
                execution: None,
                published: None,
            },
            bash("ls -la", "out"),
        ];
        let out = stream(&blocks, &FoldPolicy::none()); // stream() uses reveal = true
                                                        // The Edit header carries the resolved absolute path (cwd + target).
        assert_eq!(out[0]["head"]["path"], json!("/repo/src/x.rs"));
        // Bash is a command, not a file — no path link.
        assert!(out[1]["head"].get("path").is_none(), "bash has no path");
    }

    #[test]
    fn dump_mode_omits_reveal_path_so_shared_files_dont_carry_local_paths() {
        let edit = Block::ToolUse {
            name: "Edit".into(),
            target: "src/x.rs".into(),
            diffs: vec![("a".into(), "b".into())],
            output: None,
            patch: None,
            read_lines: None,
            cwd: String::new(),
            execution: None,
            published: None,
        };
        // reveal = false (the `--dump-html` shape): the header still names the file
        // but carries no absolute `path` for the browser to link/reveal.
        let times: Vec<Option<f64>> = Vec::new();
        let (jsonl, _) = build_jsonl(
            std::slice::from_ref(&edit),
            &times,
            &FoldPolicy::none(),
            "/repo",
            false,
            false,
            None,
            json!({ "t": "meta" }),
        );
        let obj: Value = serde_json::from_str(jsonl.lines().nth(1).unwrap()).unwrap();
        assert_eq!(obj["head"]["target"], json!("src/x.rs"));
        assert!(
            obj["head"].get("path").is_none(),
            "dump omits the reveal path"
        );
    }

    #[test]
    fn resolve_abs_handles_absolute_relative_and_missing_cwd() {
        assert_eq!(
            resolve_abs("/repo", "/etc/hosts").as_deref(),
            Some("/etc/hosts")
        );
        assert_eq!(
            resolve_abs("/repo", "src/a.rs").as_deref(),
            Some("/repo/src/a.rs")
        );
        assert_eq!(
            resolve_abs("/repo/", "src/a.rs").as_deref(),
            Some("/repo/src/a.rs")
        );
        assert_eq!(
            resolve_abs("", "src/a.rs"),
            None,
            "no cwd, relative → unresolvable"
        );
    }

    #[test]
    fn percent_decode_round_trips_paths_with_spaces_and_unicode() {
        assert_eq!(percent_decode("/a/b.rs"), "/a/b.rs");
        assert_eq!(percent_decode("/a%20b/c.rs"), "/a b/c.rs"); // space
        assert_eq!(
            percent_decode("/Users/h/%E2%9C%93/x"),
            "/Users/h/\u{2713}/x" // ✓ (multi-byte utf-8)
        );
        assert_eq!(percent_decode("/a%2Fb"), "/a/b"); // encoded slash
        assert_eq!(percent_decode("bad%2"), "bad%2"); // truncated escape passes through
    }

    #[test]
    fn html_flag_parses_and_conflicts_with_the_dump_modes() {
        use crate::Args;
        use clap::Parser as _;
        // `--html` alone, and with `-f`.
        assert!(
            Args::try_parse_from(["claude-replay", "sid", "--html"])
                .unwrap()
                .html
        );
        let live = Args::try_parse_from(["claude-replay", "sid", "-f", "--html"]).unwrap();
        assert!(live.html && live.follow);
        // Mutually exclusive with the file-writing dump modes.
        assert!(
            Args::try_parse_from(["claude-replay", "sid", "--html", "--dump-html", "-"]).is_err()
        );
        assert!(Args::try_parse_from(["claude-replay", "sid", "--html", "--dump", "-"]).is_err());
    }

    #[test]
    fn live_mode_wires_the_companion_poll() {
        let page = build_html("t", "{\"t\":\"meta\"}", &[], Some("run.jsonl"));
        assert!(page.contains("<body data-src=\"run.jsonl\" data-poll="));
        assert!(page.contains(&format!("data-poll=\"{POLL_MS}\"")));
        // A one-off export's <body> carries no companion attributes.
        let oneoff = build_html("t", "{}", &[], None);
        assert!(oneoff.contains("<body>"), "plain body tag, no data-src");
    }

    #[test]
    fn title_uses_project_dir_for_uuid_and_stem_for_named_file() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let base = std::env::temp_dir().join(format!(
            "cr-title-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&base).unwrap();

        // A store transcript is named by a UUID → the title shows the project dir (from cwd),
        // with the agent appended: `<project> · <agent>`.
        let uuid = base.join("12345678-90ab-cdef-1234-567890abcdef.jsonl");
        std::fs::write(
            &uuid,
            "{\"type\":\"user\",\"cwd\":\"/Users/me/code/knack\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        // Temp-dir files sit outside every store → honestly badged (#66).
        assert_eq!(
            display_title(Agent::CLAUDE, &uuid),
            "knack · compatible (claude)"
        );

        // A transcript the user named and pointed at directly keeps its file stem.
        let named = base.join("my-session.jsonl");
        std::fs::write(
            &named,
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        assert_eq!(
            display_title(Agent::CLAUDE, &named),
            "my-session · compatible (claude)"
        );

        // The UUID shape is recognized regardless of case; a non-UUID/non-store stem is kept.
        assert!(looks_like_session_id(
            "12345678-90AB-cdef-1234-567890abcdef"
        ));
        assert!(looks_like_session_id("rollout-2026-07-27T10-00-00-abc"));
        assert!(!looks_like_session_id("knack"));

        let _ = std::fs::remove_dir_all(&base);
    }

    /// The QoderWork title regression pin: a NEW-style store session — no `<sid>-session.json`
    /// sidecar, because QoderWork stopped writing them (~July 2026) — must get its title from
    /// the SQLite store in a DEFAULT build. This broke once when the DB reader sat behind an
    /// off-by-default feature only the release passed: a plain `cargo build` showed every new
    /// session as its cryptic workspace-dir leaf (`msr69v34k38fdyyj`). The reader is now
    /// compiled in by `cfg(target_os = "macos")`, and this test is the tripwire — it fails
    /// (with exactly that leaf as the wrong answer) if the DB half ever leaves the default
    /// macOS build again.
    #[cfg(target_os = "macos")]
    #[test]
    fn qoderwork_title_comes_from_the_db_in_a_default_build() {
        let base = std::env::temp_dir().join(format!("cr-qwdb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // The real store shape: a workspace-slug project dir, a UUID-named transcript, and
        // NO sidecar beside it.
        let projects = base.join("projects");
        let proj = projects.join("-Users-x--qoderwork-workspace-mchat42");
        std::fs::create_dir_all(&proj).unwrap();
        let sid = "11111111-2222-3333-4444-555555555555";
        let transcript = proj.join(format!("{sid}.jsonl"));
        std::fs::write(
            &transcript,
            "{\"type\":\"user\",\"cwd\":\"/Users/x/.qoderwork/workspace/mchat42\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        let db = base.join("agents.db");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(&format!(
                "create table sub_chats (id text, name text, chat_id text, session_id text, updated_at integer);
                 insert into sub_chats values ('s1','重构解析器模块','mchat42','{sid}',100);",
            ))
            .unwrap();
        std::env::set_var("QODERWORK_DB", &db);
        std::env::set_var("QODERWORK_PROJECTS_DIR", &projects);
        let title = display_title(Agent::QODERWORK, &transcript);
        std::env::remove_var("QODERWORK_DB");
        std::env::remove_var("QODERWORK_PROJECTS_DIR");
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            title, "重构解析器模块 · qoderwork",
            "a sidecar-less store session must be titled from the SQLite store in the \
             default build — 'mchat42' here means it fell back to the workspace leaf"
        );
    }
}

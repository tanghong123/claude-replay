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

use crate::fold::FoldPolicy;
use crate::model::{AttachmentContent, Block};
use crate::render::{self, LineOp};
use crate::{discover, highlight, Agent};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{json, Map, Value};
use std::path::Path;

// This module is the render core (markdown → HTML, the JSON block emitter, page assembly).
// The offline bundles (`dump_html`/`dump_all_html`) live in `bundle`; the `--html` live
// server in `serve`. All three public entries are re-exported so `html_export::{dump_html,
// dump_all_html, serve}` stays the crate's surface.
mod bundle;
mod serve;
pub use bundle::{dump_all_html, dump_html};
pub use serve::serve;

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
fn syntax_class(color: ratatui::style::Color) -> Option<&'static str> {
    use ratatui::style::Color;
    match color {
        Color::Indexed(81) => Some("kw"),   // keyword / storage
        Color::Indexed(141) => Some("kw"),  // number / constant (purple)
        Color::Indexed(197) => Some("kw"),  // self / language variable
        Color::Indexed(148) => Some("fn"),  // function / macro
        Color::Indexed(186) => Some("str"), // string
        Color::Indexed(242) => Some("com"), // comment
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
                let text = esc(&s.content);
                match s.style.fg.and_then(syntax_class) {
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
            Event::SoftBreak => out.push(' '),
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

/// Diff rows for an Edit: real file line numbers when the transcript carried a
/// `structuredPatch`, else a local 1..N numbering over the new side (mirrors the
/// TUI's `render_patch` / `diff_lines`).
fn diff_part(b: &Block) -> Option<(Value, usize, usize)> {
    let Block::ToolUse { diffs, patch, .. } = b else {
        return None;
    };
    let mut rows: Vec<Value> = Vec::new();
    let (mut adds, mut dels) = (0usize, 0usize);

    if let Some(hunks) = patch {
        for h in hunks {
            let (mut n, mut o) = (h.new_start, h.old_start);
            for line in &h.lines {
                let marker = line.chars().next().unwrap_or(' ');
                let text = line.get(marker.len_utf8()..).unwrap_or("");
                match marker {
                    '+' => {
                        rows.push(json!(["add", n, text]));
                        adds += 1;
                        n += 1;
                    }
                    '-' => {
                        rows.push(json!(["del", o, text]));
                        dels += 1;
                        o += 1;
                    }
                    _ => {
                        rows.push(json!(["ctx", n, text]));
                        n += 1;
                        o += 1;
                    }
                }
            }
        }
    } else {
        for (old, new) in diffs
            .iter()
            .filter(|(o, n)| !(o.is_empty() && n.is_empty()))
        {
            let ol: Vec<&str> = old.lines().collect();
            let nl: Vec<&str> = new.lines().collect();
            let mut n = 0usize;
            for op in render::line_diff(&ol, &nl) {
                match op {
                    LineOp::Eq(l) => {
                        n += 1;
                        rows.push(json!(["ctx", n, l]));
                    }
                    LineOp::Del(l) => {
                        dels += 1;
                        rows.push(json!(["del", Value::Null, l]));
                    }
                    LineOp::Ins(l) => {
                        n += 1;
                        adds += 1;
                        rows.push(json!(["add", n, l]));
                    }
                }
            }
        }
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
/// `resolve_target_path` does (`~/` → `$HOME`, relative → joined onto the session
/// `cwd`), for the header's `file://` "open" link. `None` when it can't be made
/// absolute (no cwd and a relative target). Existence is **not** required — the
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
    next_block: usize,
    turn: usize,
    /// `(anchor id, label)` per user turn — becomes the sidebar.
    turns: Vec<(String, String)>,
}

impl Emitter<'_> {
    fn block_id(&mut self) -> String {
        self.next_block += 1;
        format!("b{}", self.next_block)
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
                self.turns.push((id.clone(), label_of(text, 46)));
                o.insert("id".into(), json!(id));
                o.insert("turn".into(), json!(self.turn));
                o.insert("label".into(), json!(label_of(text, 80)));
                body.push(json!({ "p": "md", "h": md_html(text) }));
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
                head.insert("chips".into(), json!([chip(crate::render::spawn_chip(sa))]));
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
                body.push(json!({ "p": "md", "h": md_html(text) }));
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
                head.insert("att_kind".into(), json!(a.kind));
                head.insert("att_name".into(), json!(a.name.clone()));
                head.insert("att_dl".into(), json!(a.content.is_some()));
                // Only a served page (`--html`, `reveal == true`) gets the payload/path
                // to act on; a portable `--dump-html` export shows the name alone. The
                // JS downloads embedded content via a Blob (text) or a `data:` URI
                // (image) — no server endpoint needed — and reveals a path via `/__reveal`.
                if self.reveal {
                    if let Some(p) = &a.path {
                        head.insert("att_path".into(), json!(p));
                    }
                    match &a.content {
                        Some(AttachmentContent::Text(t)) => {
                            head.insert("att_text".into(), json!(t));
                        }
                        Some(AttachmentContent::Base64 { mime, b64 }) => {
                            head.insert(
                                "att_datauri".into(),
                                json!(format!("data:{mime};base64,{b64}")),
                            );
                        }
                        None => {}
                    }
                } else if let (Some(sink), Some(content)) =
                    (self.assets.as_deref_mut(), a.content.as_ref())
                {
                    // Offline bundle: write the embedded bytes into `assets/` and link the
                    // block to the file (a real offline download), de-conflicting names.
                    if let Some(href) = sink.materialize(&a.name, a.path.as_deref(), content) {
                        head.insert("att_href".into(), json!(href));
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
                self.turns.push((id.clone(), label_of(&label, 46)));
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
            Block::Thinking {
                text,
                duration_secs,
                tools,
            } => {
                o.insert("id".into(), json!(self.block_id()));
                // Reuse the TUI's collapsed summary verbatim (see `render_collapsed`).
                let summary =
                    if text.trim().is_empty() && duration_secs.is_none() && !tools.is_empty() {
                        render::capitalize(&render::activities(tools))
                    } else if duration_secs.is_some() || !tools.is_empty() {
                        render::turn_summary(*duration_secs, tools)
                    } else {
                        format!("Thought ({} lines)", text.lines().count())
                    };
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
                ..
            } => {
                o.insert("id".into(), json!(self.block_id()));
                // The tool's display name (same as the fold header) drives the
                // client-side "filter by tool use" dropdown — one `data-tool` per
                // tool fold, counted and grouped in the browser.
                o.insert("tool".into(), json!(render::display_name(name)));
                head.insert("name".into(), json!(render::display_name(name)));
                head.insert("target".into(), json!(target));
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
                    if let Some(abs) = resolve_abs(self.cwd, target) {
                        head.insert("path".into(), json!(abs));
                    }
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
                            body.push(
                                json!({ "p": "note", "x": render::edit_summary(adds, dels) }),
                            );
                            body.push(part);
                        } else if let Some(out) = output {
                            body.push(pre_part(out));
                        }
                    }
                    "write" => {
                        let content = diffs
                            .iter()
                            .map(|(_, n)| n.as_str())
                            .find(|n| !n.is_empty())
                            .unwrap_or("");
                        let n = content.lines().count();
                        head.insert(
                            "chips".into(),
                            json!([chip_class("add", format!("{n} lines"))]),
                        );
                        body.push(json!({
                            "p": "note",
                            "x": format!("Wrote {n} lines to {target}"),
                        }));
                        body.push(numbered_part(content, token, render::WRITE_PREVIEW));
                    }
                    "read" => {
                        if let Some(n) = read_lines {
                            head.insert("chips".into(), json!([chip(format!("{n} lines"))]));
                        }
                        if let Some(out) = output {
                            body.push(numbered_part(out, token, render::WRITE_PREVIEW));
                        }
                    }
                    _ => {
                        if let Some(out) = output {
                            let n = out.lines().count();
                            head.insert("chips".into(), json!([chip(format!("{n} lines"))]));
                            body.push(pre_part(out));
                        }
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

/// Build the append-only JSONL stream: one `meta` line, then one line per block.
fn build_jsonl(
    blocks: &[Block],
    user_times: &[Option<f64>],
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    linked: bool,
    meta: Value,
) -> (String, Vec<(String, String)>) {
    build_jsonl_inner(blocks, user_times, fold, cwd, reveal, linked, None, meta)
}

/// [`build_jsonl`] with an optional [`AssetSink`] — the offline bundle threads one through
/// so each stream materializes its attachments into a shared, de-conflicted `assets/` dir.
#[allow(clippy::too_many_arguments)]
fn build_jsonl_inner(
    blocks: &[Block],
    user_times: &[Option<f64>],
    fold: &FoldPolicy,
    cwd: &str,
    reveal: bool,
    linked: bool,
    assets: Option<&mut AssetSink>,
    meta: Value,
) -> (String, Vec<(String, String)>) {
    let mut em = Emitter {
        fold,
        cwd,
        reveal,
        linked,
        assets,
        next_block: 0,
        turn: 0,
        turns: Vec::new(),
    };
    let mut lines = vec![meta.to_string()];
    // `user_times[i]` is the ith user turn's timestamp (see `model::parse_main`).
    let mut seen_turns = 0usize;
    for b in blocks {
        let ts = if matches!(b, Block::UserText(_) | Block::Command { .. }) {
            let t = user_times.get(seen_turns).copied().flatten();
            seen_turns += 1;
            t
        } else {
            None
        };
        lines.push(em.block(b, ts).to_string());
    }
    (lines.join("\n"), em.turns)
}

/// The page shell: embedded CSS, the inline snapshot, the renderer, and (in live
/// mode) the companion path + poll interval the renderer appends from.
fn build_html(title: &str, jsonl: &str, turns: &[(String, String)], live: Option<&str>) -> String {
    build_page(title, jsonl, turns, live, None)
}

/// The shared shell for a multi-file bundle (`--dump-all-html` / served `--html`): no
/// inline snapshot and an empty sidebar (the JS fetches `?session=<id>`.jsonl, defaulting
/// to `root_id`, and fills the sidebar as turns arrive). `live` makes the page poll its
/// stream (served `--html -f`); a static offline bundle sets it false.
pub(super) fn build_shell(title: &str, root_id: &str, live: bool) -> String {
    build_page(title, "", &[], None, Some((root_id, live)))
}

/// The page template. `multi` = Some((root_id, live)) makes it a multi-file shell (fetch
/// `?session=<id>`.jsonl, polling when `live`); None inlines `jsonl` + the server-rendered
/// `turns` sidebar.
fn build_page(
    title: &str,
    jsonl: &str,
    turns: &[(String, String)],
    live: Option<&str>,
    multi: Option<(&str, bool)>,
) -> String {
    let sidebar: String = turns
        .iter()
        .map(|(id, label)| {
            format!(
                "<div class=\"side-item\" data-t=\"{}\" tabindex=\"0\">{}</div>",
                esc(id),
                esc(label)
            )
        })
        .collect();
    let live_attrs = match (live, multi) {
        // A multi-file shell: `data-multi`/`data-root`, plus `data-poll` when served live
        // (navigation between agents is a full page load carrying `?session=<id>`).
        (_, Some((root, live_multi))) => {
            let poll = if live_multi {
                format!(" data-poll=\"{POLL_MS}\"")
            } else {
                String::new()
            };
            format!(" data-multi=\"1\" data-root=\"{}\"{poll}", esc(root))
        }
        (Some(src), None) => format!(" data-src=\"{}\" data-poll=\"{POLL_MS}\"", esc(src)),
        (None, None) => String::new(),
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title_esc}</title>
<style>
{CSS}
</style>
</head>
<body{live_attrs}>
<div id="topbar">
  <div class="brand">claude-replay <span class="brand-sub">· session export</span></div>
  <div class="toolfilter">
    <button id="btn-tools" class="tbtn"><span class="tf-label">Filter ▾</span><span class="tf-x" title="Clear filter">✕</span></button>
    <div id="toolmenu">
      <div class="menu-head">Filter by type / tool</div>
      <div id="toolitems"></div>
    </div>
  </div>
  <div class="toolfilter" id="agentnav" style="display:none">
    <button id="btn-agents" class="tbtn disabled"><span class="tf-label">Agents ▾</span></button>
    <div id="agentmenu">
      <div class="menu-head">Sub-agents of this session</div>
      <div id="agentitems"></div>
    </div>
  </div>
  <div class="searchbox">
    <span class="mag">⌕</span>
    <input id="q" placeholder="Search transcript  ( / )" autocomplete="off">
    <span id="qcount"></span>
  </div>
  <button id="btn-exp" class="tbtn" data-full="Expand all">Expand all</button>
  <button id="btn-col" class="tbtn" data-full="Collapse all">Collapse all</button>
  <button id="btn-wide" class="tbtn" title="Wide mode — drop the reading-width cap for diff-heavy sessions">⇔ Wide</button>
  <button id="btn-theme" class="tbtn">◐ Dark</button>
</div>
<div class="layout" id="layout">
  <nav id="sidebar">
    <div class="side-head">Turns</div>
    <div id="turnlist">{sidebar}</div>
    <div class="usage" id="usage"></div>
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
<button id="newbadge">↓ new messages</button>
<script id="session-data" type="application/jsonl">
{jsonl_esc}
</script>
<script>
{JS}
</script>
</body>
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
) -> (String, Vec<(String, String)>) {
    let session_id = session_id(path);
    // Prefer the repo/dir name as the display title; fall back to the session id
    // when the transcript records no cwd.
    let display = repo_name(cwd).unwrap_or_else(|| session_id.clone());
    let meta = json!({
        "t": "meta",
        "title": display,
        "agent": agent.label(),
        "sid": session_id,
        "path": path.display().to_string(),
        "cwd": cwd,
        "turns": count_turns(blocks),
        "tools": count_tools(blocks),
        "duration_secs": m.duration_secs,
        "usage": {
            "input": human_tokens(m.input_tokens),
            "output": human_tokens(m.output_tokens),
            "cache_read": human_tokens(m.cache_read_tokens),
            "cost": m.cost_usd.map(|c| format!("${c:.2}")),
            "model": m.model,
        },
    });
    build_jsonl(
        blocks, user_times, fold, cwd, reveal,
        false, // single-file snapshot: no cross-agent links
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
pub(super) fn display_title(agent: Agent, path: &Path) -> String {
    let stem = session_id(path);
    let name = if looks_like_session_id(&stem) {
        discover::session_cwd(path)
            .and_then(|cwd| repo_name(&cwd.display().to_string()))
            .unwrap_or(stem)
    } else {
        stem // a file the user named → the stem is the meaningful name
    };
    format!("{name} · {}", agent.label())
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

/// The block lines of a stream (everything after the leading `meta` line).
pub(super) fn block_lines(jsonl: &str) -> Vec<String> {
    jsonl.lines().skip(1).map(String::from).collect()
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
    /// The spawn's own terminal status — set for a **synchronous** `Agent` spawn, whose
    /// result (and `status: "completed"`) lands inline on its `tool_use` rather than as a
    /// later `<task-notification>`. Async `Task` completion instead shows up as a separate
    /// `AgentDone` block (see `done` in `agent_stream`); the running flag ORs both signals.
    pub(super) terminal: bool,
}

/// The direct sub-agents spawned in this source (its own `SubAgent` blocks). Drives lazy
/// stream generation — parsing ONE source reveals only its direct children.
fn collect_child_refs(blocks: &[Block]) -> Vec<ChildRef> {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::SubAgent(sa) if !sa.agent_id.is_empty() => Some(ChildRef {
                id: sa.agent_id.clone(),
                description: sa.description.clone(),
                agent_type: sa.agent_type.clone(),
                terminal: sa.status.is_terminal(),
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
    assets: Option<&mut AssetSink>,
) -> (String, Vec<ChildRef>) {
    let usage = json!({
        "input": human_tokens(m.input_tokens), "output": human_tokens(m.output_tokens),
        "cache_read": human_tokens(m.cache_read_tokens),
        "cost": m.cost_usd.map(|c| format!("${c:.2}")), "model": m.model,
        "duration_secs": m.duration_secs,
    });
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
    let meta = json!({
        "t": "meta", "title": &info.title, "agent": agent.label(), "sid": &info.id,
        "cwd": cwd, "turns": count_turns(blocks), "tools": count_tools(blocks),
        "agent_type": &info.agent_type, "usage": usage,
        "ancestors": ancestors, "children": children,
    });
    let (jsonl, _) = build_jsonl_inner(blocks, user_times, fold, cwd, reveal, true, assets, meta);
    (jsonl, child_refs)
}

/// The `AgentInfo` for a child `c` discovered in `parent`'s source: its title is its
/// description (else its type), and its ancestry is the parent's ancestry + the parent.
pub(super) fn child_info(
    agent: Agent,
    root_path: &Path,
    parent: &AgentInfo,
    c: ChildRef,
) -> Option<AgentInfo> {
    let source = discover::subagent_source(agent, root_path, &c.id)?;
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
        content: &AttachmentContent,
    ) -> Option<String> {
        let bytes: Vec<u8> = match content {
            AttachmentContent::Text(t) => t.clone().into_bytes(),
            AttachmentContent::Base64 { b64, .. } => crate::clipboard::base64_decode(b64)?,
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
        if let AttachmentContent::Base64 { mime, .. } = content {
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
    use super::serve::{line_aligned_tail, percent_decode, query_get, stream_delta};
    use super::*;
    use crate::model::Hunk;

    /// Emit `blocks` to the block-stream JSON (skipping the meta line) with the
    /// given fold policy and no timestamps — the shape the tests assert on. Uses
    /// `reveal = true` (served-mode shape, where file tools carry a `path`).
    fn stream(blocks: &[Block], fold: &FoldPolicy) -> Vec<Value> {
        let times: Vec<Option<f64>> = Vec::new();
        let (jsonl, _turns) = build_jsonl(
            blocks,
            &times,
            fold,
            "/repo",
            true,
            false,
            json!({ "t": "meta" }),
        );
        jsonl
            .lines()
            .skip(1) // meta line
            .map(|l| serde_json::from_str::<Value>(l).expect("valid JSON block line"))
            .collect()
    }

    fn bash(cmd: &str, out: &str) -> Block {
        Block::ToolUse {
            name: "Bash".into(),
            target: cmd.into(),
            diffs: vec![],
            output: Some(out.into()),
            patch: None,
            read_lines: None,
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
        use crate::model::AttachmentContent;
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
            .materialize("notes.md", None, &AttachmentContent::Text("hi".into()))
            .unwrap();
        assert_eq!(h1, "assets/notes.md");
        assert_eq!(
            std::fs::read_to_string(base.join("assets/notes.md")).unwrap(),
            "hi"
        );
        // Same name again → de-conflicted.
        let h2 = sink
            .materialize("notes.md", None, &AttachmentContent::Text("bye".into()))
            .unwrap();
        assert_eq!(h2, "assets/notes-1.md");
        // Base64 image → decoded; extension derived from the mime when missing.
        let h3 = sink
            .materialize(
                "shot",
                None,
                &AttachmentContent::Base64 {
                    mime: "image/png".into(),
                    b64: "aGk=".into(), // "hi"
                },
            )
            .unwrap();
        assert_eq!(h3, "assets/shot.png");
        assert_eq!(std::fs::read(base.join("assets/shot.png")).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The `/stream` byte cursor: serve from an offset, clamp past-EOF to empty, and never
    /// end a chunk mid-record (truncate to the last newline).
    #[test]
    fn line_aligned_tail_clamps_and_aligns() {
        let data = b"a\nbb\nccc\n";
        assert_eq!(line_aligned_tail(data, 0), b"a\nbb\nccc\n");
        assert_eq!(line_aligned_tail(data, 2), b"bb\nccc\n"); // from mid-file, line-aligned
        assert_eq!(line_aligned_tail(data, 9), b""); // at EOF → empty
        assert_eq!(line_aligned_tail(data, 999), b""); // past EOF → clamped empty
                                                       // A trailing partial line (mid-append) is withheld until its newline arrives.
        assert_eq!(line_aligned_tail(b"a\nbb\npart", 2), b"bb\n");
    }

    #[test]
    fn query_get_parses_pairs() {
        assert_eq!(query_get("session=a1&from=42", "session"), Some("a1"));
        assert_eq!(query_get("session=a1&from=42", "from"), Some("42"));
        assert_eq!(query_get("session=a1", "from"), None);
    }

    /// The live tailer's per-agent delta: a pure append emits no reset; a rewritten tail
    /// emits `{t:"reset",from:N}` at the first divergence; an unchanged stream is a no-op.
    #[test]
    fn stream_delta_appends_and_resets() {
        let meta = r#"{"t":"meta","tools":2}"#;
        // Pure append: two new blocks, no reset.
        let d = stream_delta(
            &["a".into(), "b".into()],
            &["a".into(), "b".into(), "c".into()],
            meta,
        )
        .expect("changed");
        assert!(!d.contains("reset"), "pure append has no reset: {d}");
        assert!(d.contains('c') && d.trim_end().ends_with(meta));
        // Rewritten tail: block 1 changed → reset from 1, re-emit the tail.
        let d = stream_delta(&["a".into(), "b".into()], &["a".into(), "B2".into()], meta)
            .expect("changed");
        let reset: Value = serde_json::from_str(d.lines().next().unwrap()).unwrap();
        assert_eq!(
            reset,
            json!({ "t": "reset", "from": 1 }),
            "reset at divergence"
        );
        assert!(d.contains("B2"));
        // Unchanged → no delta.
        assert_eq!(
            stream_delta(&["a".into(), "b".into()], &["a".into(), "b".into()], meta),
            None
        );
    }

    /// The multi-file shell carries `data-multi`/`data-root` and no inline snapshot;
    /// `live` adds `data-poll` (served `--html -f`), static omits it.
    #[test]
    fn build_shell_is_multi_file_without_inline() {
        let html = build_shell("My session", "root-9f3d", false);
        assert!(html.contains("data-multi=\"1\""), "multi flag");
        assert!(html.contains("data-root=\"root-9f3d\""));
        assert!(!html.contains("data-poll"), "static bundle does not poll");
        // Live served shell polls its stream.
        let live = build_shell("My session", "root-9f3d", true);
        assert!(live.contains("data-poll"), "live shell polls: {live:.0}");
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
        assert_eq!(
            recs[0]["ancestors"],
            json!([{ "id": "sess", "title": "sess · claude" }]),
            "child breadcrumb points at the root (titled by session name + agent)"
        );
        assert!(
            recs.iter().any(|o| o["head"]["name"] == json!("Read")),
            "child transcript rendered: {recs:?}"
        );
        let _ = std::fs::remove_dir_all(&base);
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
        let file = Block::Attachment(crate::model::Attachment {
            kind: "file",
            name: "notes.md".into(),
            path: Some("/w/notes.md".into()),
            content: Some(AttachmentContent::Text("hello".into())),
        });
        // Served (reveal = true): name + downloadable flag + inline text + path.
        let served = stream(std::slice::from_ref(&file), &FoldPolicy::none());
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
        let img = Block::Attachment(crate::model::Attachment {
            kind: "image",
            name: "image.png".into(),
            path: None,
            content: Some(AttachmentContent::Base64 {
                mime: "image/png".into(),
                b64: "aGk=".into(),
            }),
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
    fn diff_rows_classify_add_del_context_with_real_line_numbers() {
        let out = stream(&[edit_with_patch()], &FoldPolicy::none());
        let body = out[0]["body"].as_array().unwrap();
        // First body part is the `⎿ Added…` note; the diff part follows.
        let diff = body.iter().find(|p| p["p"] == "diff").expect("diff part");
        let rows = diff["rows"].as_array().unwrap();
        // Context advances both sides (to old/new line 11), so the deletion is
        // old-line 11 and the insertions are new-lines 11 and 12 — same numbering
        // the TUI's `render_patch` produces.
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
        assert_eq!(turns[0].0, "t1");
        assert_eq!(turns[1].0, "t2");
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
        assert_eq!(num["cap"], json!(render::WRITE_PREVIEW));
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
            summary.starts_with("✻ Ran 1 shell command (ls)"),
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

    #[test]
    fn block_lines_drops_the_leading_meta() {
        let (jsonl, _) = build_jsonl(
            &[Block::UserText("hi".into()), bash("ls", "out")],
            &[],
            &FoldPolicy::none(),
            "/repo",
            true,
            false,
            json!({ "t": "meta" }),
        );
        // The stream is meta + 2 blocks; block_lines keeps just the 2 block records.
        assert_eq!(jsonl.lines().count(), 3);
        let bl = block_lines(&jsonl);
        assert_eq!(bl.len(), 2);
        assert!(bl.iter().all(|l| l.contains("\"t\":\"block\"")), "{bl:?}");
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
        assert_eq!(display_title(Agent::Claude, &uuid), "knack · claude");

        // A transcript the user named and pointed at directly keeps its file stem.
        let named = base.join("my-session.jsonl");
        std::fs::write(
            &named,
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .unwrap();
        assert_eq!(display_title(Agent::Claude, &named), "my-session · claude");

        // The UUID shape is recognized regardless of case; a non-UUID/non-store stem is kept.
        assert!(looks_like_session_id(
            "12345678-90AB-cdef-1234-567890abcdef"
        ));
        assert!(looks_like_session_id("rollout-2026-07-27T10-00-00-abc"));
        assert!(!looks_like_session_id("knack"));

        let _ = std::fs::remove_dir_all(&base);
    }
}

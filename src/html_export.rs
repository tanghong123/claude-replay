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

use crate::model::Block;
use crate::render::{self, LineOp};
use crate::view::FoldPolicy;
use crate::{discover, highlight, metrics, Agent, Args};
use anyhow::{Context, Result};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use serde_json::{json, Map, Value};
use std::path::Path;

const CSS: &str = include_str!("html/export.css");
const JS: &str = include_str!("html/export.js");

/// Rows of a diff shown before the `⋯ N more lines` expander.
const DIFF_PREVIEW: usize = 12;
/// Lines of tool output shown before the expander.
const OUTPUT_PREVIEW: usize = 12;
/// How often (ms) a live page re-reads its companion JSONL.
const POLL_MS: u64 = 2000;

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

/// The presentation kind driving `data-kind` (and the renderer's header shape).
/// Close to `model::fold_key` but finer: it splits thinking into bare `think` vs.
/// a grouped-activity `act`, and names `skill`/`agent` so they get a tool dot.
fn html_kind(b: &Block) -> &'static str {
    match b {
        Block::UserText(_) => "user",
        Block::AssistantText(_) => "assistant",
        Block::Thinking { tools, .. } => {
            if tools.is_empty() {
                "think"
            } else {
                "act"
            }
        }
        Block::ToolResult(_) => "tool",
        Block::Command { .. } => "command",
        Block::ToolUse { name, .. } => match name.as_str() {
            "Bash" => "bash",
            "Edit" | "MultiEdit" => "edit",
            "Write" | "NotebookEdit" => "write",
            "Read" | "Grep" | "Glob" | "LS" | "NotebookRead" => "read",
            "Skill" => "skill",
            "Task" | "Agent" => "agent",
            _ => "tool",
        },
    }
}

/// Is this block rendered as a collapsible fold? User prose and assistant prose
/// are always-open cards; everything else folds.
fn is_fold(b: &Block) -> bool {
    !matches!(b, Block::UserText(_) | Block::AssistantText(_))
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
            Block::AssistantText(text) => {
                o.insert("id".into(), json!(self.block_id()));
                body.push(json!({ "p": "md", "h": md_html(text) }));
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
                // File-acting tools (read/write/edit) get a `file://` path link in
                // the header — clicking it opens the file (the browser's stand-in
                // for the TUI's reveal-in-Finder); clicking elsewhere still folds.
                if matches!(kind, "read" | "write" | "edit") && !target.is_empty() {
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
    meta: Value,
) -> (String, Vec<(String, String)>) {
    let mut em = Emitter {
        fold,
        cwd,
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
    let live_attrs = match live {
        Some(src) => format!(" data-src=\"{}\" data-poll=\"{POLL_MS}\"", esc(src)),
        None => String::new(),
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
  <div class="brand">claude-replay <span>· session export</span></div>
  <div class="spacer"></div>
  <div class="toolfilter">
    <button id="btn-tools" class="tbtn"><span class="tf-label">Tools ▾</span><span class="tf-x" title="Clear filter">✕</span></button>
    <div id="toolmenu">
      <div class="menu-head">Filter by tool use</div>
      <div id="toolitems"></div>
    </div>
  </div>
  <div class="searchbox">
    <span class="mag">⌕</span>
    <input id="q" placeholder="Search transcript  ( / )" autocomplete="off">
    <span id="qcount"></span>
  </div>
  <button id="btn-exp" class="tbtn">Expand all</button>
  <button id="btn-col" class="tbtn">Collapse all</button>
  <button id="btn-theme" class="tbtn">◐ Dark</button>
</div>
<div class="layout">
  <nav id="sidebar">
    <div class="side-head">Turns</div>
    <div id="turnlist">{sidebar}</div>
    <div class="usage" id="usage"></div>
    <div class="legend">
      <span class="key">j k</span><span class="what">move</span>
      <span class="key">space</span><span class="what">fold</span>
      <span class="key">[ ]</span><span class="what">turn</span>
      <span class="key">/</span><span class="what">search</span>
    </div>
  </nav>
  <main>
    <section class="session-header">
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

/// The whole append-only stream for `path` right now: the `meta` line followed by
/// one line per block. Re-run each poll cycle in live mode; the loop appends only
/// the lines that are new since the previous cycle.
fn snapshot(
    agent: Agent,
    path: &Path,
    args: &Args,
    fold: &FoldPolicy,
) -> Result<(String, Vec<(String, String)>)> {
    let (blocks, user_times) = crate::model::parse_path_timed_for(agent, path, args)
        .with_context(|| format!("read transcript {}", path.display()))?;
    let m = metrics::parse_reader_for(
        agent,
        std::io::BufReader::new(
            std::fs::File::open(path)
                .with_context(|| format!("open transcript {}", path.display()))?,
        ),
    );
    let session_id = session_id(path);
    let cwd = discover::session_cwd(path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    // Prefer the repo/dir name as the display title; fall back to the session id
    // when the transcript records no cwd.
    let display = repo_name(&cwd).unwrap_or_else(|| session_id.clone());
    let turn_count = blocks
        .iter()
        .filter(|b| matches!(b, Block::UserText(_) | Block::Command { .. }))
        .count();
    let tool_count = blocks
        .iter()
        .map(|b| match b {
            Block::ToolUse { .. } => 1,
            Block::Thinking { tools, .. } => tools.len(),
            _ => 0,
        })
        .sum::<usize>();
    let meta = json!({
        "t": "meta",
        "title": display,
        "agent": agent.label(),
        "sid": session_id,
        "path": path.display().to_string(),
        "cwd": &cwd,
        "turns": turn_count,
        "tools": tool_count,
        "duration_secs": m.duration_secs,
        "usage": {
            "input": human_tokens(m.input_tokens),
            "output": human_tokens(m.output_tokens),
            "cache_read": human_tokens(m.cache_read_tokens),
            "cost": m.cost_usd.map(|c| format!("${c:.2}")),
            "model": m.model,
        },
    });
    Ok(build_jsonl(&blocks, &user_times, fold, &cwd, meta))
}

/// The session id — the transcript file stem (the UUID Claude/Codex names it).
fn session_id(path: &Path) -> String {
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

/// The page's display title: the session's repo/dir name, else its id.
fn display_title(path: &Path) -> String {
    let cwd = discover::session_cwd(path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    repo_name(&cwd).unwrap_or_else(|| session_id(path))
}

/// Append to `companion` only the block lines beyond `emitted`, returning the new
/// emitted count. Regeneration is stable-prefix (positional ids), so once a block
/// line is written it never changes — matching the page's append-only consume.
/// A shrunk stream (a compaction rewrite) is skipped, not truncated, to preserve
/// the append-only contract.
fn append_new(companion: &Path, emitted: usize, lines: &[&str]) -> Result<usize> {
    if lines.len() <= emitted {
        return Ok(emitted);
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(companion)
        .with_context(|| format!("append {}", companion.display()))?;
    for line in &lines[emitted..] {
        writeln!(f, "{line}")?;
    }
    Ok(lines.len())
}

/// Entry point for `--dump-html`.
pub fn export(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    let fold = FoldPolicy::from_args(args);
    let (jsonl, turns) = snapshot(agent, path, args, &fold)?;
    // The page title is the repo name; files are named by the session id.
    let title = display_title(path);

    // `--dump-html -` streams the page to stdout (pipes / tests); never live.
    let stem = match args.dump_html.as_ref().and_then(|o| o.as_deref()) {
        Some("-") => {
            print!("{}", build_html(&title, &jsonl, &turns, None));
            return Ok(());
        }
        Some(s) => s.to_string(),
        None => crate::app::deduce_stem(path, None),
    };

    // Live: the page renders the inline snapshot immediately, then polls the
    // companion for appended lines — so it works standalone *and* keeps up. The
    // page references the companion by **basename** (same directory as the .html),
    // so `fetch` resolves it relative to the page's own URL.
    let companion = if args.follow {
        let cpath = format!("{stem}.jsonl");
        std::fs::write(&cpath, format!("{jsonl}\n")).with_context(|| format!("write {cpath}"))?;
        let src = Path::new(&cpath)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&cpath)
            .to_string();
        Some((cpath, src))
    } else {
        None
    };
    let html_path = format!("{stem}.html");
    std::fs::write(
        &html_path,
        build_html(
            &title,
            &jsonl,
            &turns,
            companion.as_ref().map(|(_, s)| s.as_str()),
        ),
    )
    .with_context(|| format!("write {html_path}"))?;

    let Some((cpath, _)) = companion else {
        eprintln!("wrote {html_path}");
        println!("{stem}");
        return Ok(());
    };

    // Live tail: poll the transcript, appending any block lines that appeared
    // since the last cycle. Runs until interrupted (like `claude-replay -f`).
    eprintln!("wrote {html_path} + {cpath} (live — open it and it follows; Ctrl-C to stop)");
    println!("{stem}");
    follow_and_append(
        agent,
        path,
        args,
        &fold,
        Path::new(&cpath),
        jsonl.lines().count(),
    )
}

/// Poll the transcript forever, appending newly-produced block lines to
/// `companion`. Shared by `--dump-html -f` and `--html -f`; returns only on error
/// (the caller runs until Ctrl-C).
fn follow_and_append(
    agent: Agent,
    path: &Path,
    args: &Args,
    fold: &FoldPolicy,
    companion: &Path,
    mut emitted: usize,
) -> Result<()> {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        let (fresh, _) = match snapshot(agent, path, args, fold) {
            Ok(s) => s,
            Err(_) => continue, // transient read error mid-write; retry next cycle
        };
        let lines: Vec<&str> = fresh.lines().collect();
        if lines.len() > emitted {
            emitted = append_new(companion, emitted, &lines)?;
            // Re-append the refreshed meta (line 0) so the page updates its usage /
            // cost / duration totals as the session grows. It renders no block, so
            // the renderer treats it as a metadata refresh, not a "new message".
            if let Some(meta) = lines.first() {
                append_line(companion, meta)?;
            }
        }
    }
}

/// Append a single already-formatted JSONL line (used to refresh the meta record).
fn append_line(companion: &Path, line: &str) -> Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(companion)
        .with_context(|| format!("append {}", companion.display()))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// `--html`: render to HTML and open it in the browser instead of the TUI.
///
/// Both modes serve over a tiny **loopback HTTP server** and run until Ctrl-C —
/// serving (not `file://`) is what lets a path click reveal the file in Finder
/// (`/__reveal`) and, with `-f`, lets the page `fetch` its companion (browsers
/// block those over `file://`). `-f` adds the append-only companion + live tail;
/// without it the page is a self-contained static snapshot.
pub fn serve(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    let fold = FoldPolicy::from_args(args);
    let (jsonl, turns) = snapshot(agent, path, args, &fold)?;
    // Files are named by the session id (unique); the page title is the repo name.
    let sid = session_id(path);
    let title = display_title(path);

    // A private temp dir keeps the two files together (the page fetches the
    // companion by basename) without cluttering the cwd.
    let dir = std::env::temp_dir().join("claude-replay").join(&sid);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let html_path = dir.join(format!("{sid}.html"));

    // Live mode also writes the append-only companion the page polls.
    let companion = if args.follow {
        let c = dir.join(format!("{sid}.jsonl"));
        std::fs::write(&c, format!("{jsonl}\n"))
            .with_context(|| format!("write {}", c.display()))?;
        Some(c)
    } else {
        None
    };
    let src = companion.as_ref().map(|_| format!("{sid}.jsonl"));
    std::fs::write(
        &html_path,
        build_html(&title, &jsonl, &turns, src.as_deref()),
    )
    .with_context(|| format!("write {}", html_path.display()))?;

    let port = spawn_http_server(dir.clone())?;
    let url = format!("http://127.0.0.1:{port}/{sid}.html");
    let kind = if args.follow { "live" } else { "static" };
    eprintln!(
        "serving {} at {url} ({kind} — Ctrl-C to stop)",
        dir.display()
    );
    eprintln!("  open in a browser, or copy the URL above");
    open_in_browser(&url);
    println!("{url}");

    match &companion {
        Some(c) => follow_and_append(agent, path, args, &fold, c, jsonl.lines().count()),
        // Static: nothing to tail, but keep serving so path-reveal keeps working.
        None => loop {
            std::thread::park();
        },
    }
}

/// Open `url` in the default browser (best-effort; never fails the run).
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let prog = "open";
    #[cfg(target_os = "windows")]
    let prog = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let prog = "xdg-open";
    let _ = std::process::Command::new(prog)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// A minimal read-only HTTP server bound to loopback on an ephemeral port,
/// serving files by basename out of `root`. Returns the chosen port; the accept
/// loop runs on a detached thread (dies with the process on Ctrl-C). Loopback +
/// basename-only paths keep it from exposing anything beyond the two export files.
fn spawn_http_server(root: std::path::PathBuf) -> Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").context("bind loopback HTTP server")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let root = root.clone();
            std::thread::spawn(move || {
                let _ = serve_connection(stream, &root);
            });
        }
    });
    Ok(port)
}

/// Decode a `%XX`-percent-encoded string (the reveal path arrives via
/// `encodeURIComponent`). Unknown/short escapes are passed through literally.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn serve_connection(mut stream: std::net::TcpStream, root: &Path) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    // `GET /name.html HTTP/1.1` → the requested basename.
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let name = target
        .trim_start_matches('/')
        .split('?')
        .next()
        .unwrap_or("");
    let respond = |stream: &mut std::net::TcpStream, code: &str, ct: &str, body: &[u8]| {
        let head = format!(
            "HTTP/1.1 {code}\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream
            .write_all(head.as_bytes())
            .and_then(|_| stream.write_all(body))
    };
    // `/__reveal?path=<url-encoded abs path>` — reveal a file in the OS file
    // manager (the served page can't follow a `file://` link: browsers block
    // http→file navigation). Reveal-only (`open -R` / folder open), never execute;
    // the path must exist. Loopback-bound, so only this machine can reach it.
    if name == "__reveal" {
        if let Some(p) = target
            .split_once("path=")
            .map(|(_, v)| percent_decode(v.split('&').next().unwrap_or("")))
        {
            let path = Path::new(&p);
            if path.exists() {
                crate::app::reveal_in_file_manager(path);
                return respond(&mut stream, "200 OK", "text/plain", b"revealed");
            }
        }
        return respond(&mut stream, "404 Not Found", "text/plain", b"no such path");
    }
    // Basename-only: no traversal, no subdirs — the export writes two flat files.
    if name.is_empty() || name.contains('/') || name.contains("..") {
        return respond(&mut stream, "403 Forbidden", "text/plain", b"forbidden");
    }
    match std::fs::read(root.join(name)) {
        Ok(bytes) => {
            let ct = if name.ends_with(".html") {
                "text/html; charset=utf-8"
            } else if name.ends_with(".jsonl") || name.ends_with(".json") {
                "application/json; charset=utf-8"
            } else {
                "application/octet-stream"
            };
            respond(&mut stream, "200 OK", ct, &bytes)
        }
        Err(_) => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Hunk;

    /// Emit `blocks` to the block-stream JSON (skipping the meta line) with the
    /// given fold policy and no timestamps — the shape the tests assert on.
    fn stream(blocks: &[Block], fold: &FoldPolicy) -> Vec<Value> {
        let times: Vec<Option<f64>> = Vec::new();
        let (jsonl, _turns) = build_jsonl(blocks, &times, fold, "/repo", json!({ "t": "meta" }));
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
    fn append_new_only_writes_the_fresh_suffix() {
        let dir = std::env::temp_dir().join(format!("cr-html-append-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("c.jsonl");
        std::fs::write(&f, "meta\nb1\n").unwrap();

        // First two lines already emitted; a third appears.
        let lines = vec!["meta", "b1", "b2"];
        let n = append_new(&f, 2, &lines).unwrap();
        assert_eq!(n, 3);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "meta\nb1\nb2\n");

        // Nothing new → no write, count unchanged.
        let n = append_new(&f, 3, &lines).unwrap();
        assert_eq!(n, 3);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "meta\nb1\nb2\n");

        // A shrunk stream (compaction) is skipped, not truncated.
        let shorter = vec!["meta", "b1"];
        let n = append_new(&f, 3, &shorter).unwrap();
        assert_eq!(n, 3);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "meta\nb1\nb2\n");

        std::fs::remove_dir_all(&dir).ok();
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
        let out = stream(&blocks, &FoldPolicy::none());
        // The Edit header carries the resolved absolute path (cwd + target).
        assert_eq!(out[0]["head"]["path"], json!("/repo/src/x.rs"));
        // Bash is a command, not a file — no path link.
        assert!(out[1]["head"].get("path").is_none(), "bash has no path");
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
        use clap::Parser;
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
}

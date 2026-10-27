//! **Search on the server (#s278, `design/slow-links.md` §7)** — the MATCHING half of the page's
//! `shared/search.js`, ported literally, so a reader over a slow link can search the records their
//! page has not fetched.
//!
//! The page keeps the grammar (the prefix letters, `o(…)`, the escape, the tool letters) and sends
//! what it reads from the box: the lowercased needle, whole words, the wanted class mask and the
//! resolved tools. What a record's searchable text IS, and how it is counted, is decided here exactly
//! as the page decides it, quirks included, because a difference is a count that changes when the
//! head lands: `stripTags`' five sequential entity replaces (`&amp;lt;` reads `<`), JavaScript
//! truthiness on a head's fields, `String(undefined)` for a part with no text, each part lowercased
//! before it is measured, and the unscoped total counted over the whole text rather than summed from
//! the parts. A parity test runs both over the rendering audit's corpus (`parity_with_the_page`).
//!
//! It quotes nothing back: a reply is record indices and counts, so secret masking has nothing to do.

use serde::Deserialize;
use serde_json::{json, Value};
use std::borrow::Cow;
use std::sync::OnceLock;

/// The scope classes (`CLASS_BIT`): your turns, the agent's replies, thinking, tools.
pub(crate) const U: u8 = 1;
pub(crate) const A: u8 = 2;
pub(crate) const T: u8 = 4;
pub(crate) const O: u8 = 8;
const CLASS_ORDER: [u8; 4] = [U, A, T, O];

/// A tool `o(…)` names, as `splitQuery` resolves it: a name, or a family's prefix.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub(crate) struct Tool {
    pub name: String,
    #[serde(default)]
    pub prefix: bool,
}

/// What the page read from the box (`splitQuery` with its session's letters).
#[derive(Deserialize, Debug, Clone, Default)]
pub(crate) struct Query {
    /// The needle, lowercased; empty for a query with tools and no text (the filter).
    #[serde(default)]
    pub lc: String,
    #[serde(default)]
    pub whole: bool,
    /// The wanted classes (`scopeMask`), 0 for everything.
    #[serde(default)]
    pub wanted: u8,
    #[serde(default)]
    pub tools: Vec<Tool>,
}

/// JavaScript truthiness of a JSON value (`if (h[k])`).
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// JavaScript's `String(x)` of a JSON value, `None` being `undefined`.
fn js_string(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::Null) => "null".into(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                let f = n.as_f64().unwrap_or(f64::NAN);
                if f.is_nan() {
                    "NaN".into()
                } else if f.fract() == 0.0 && f.abs() < 1e21 {
                    format!("{f:.0}")
                } else {
                    format!("{f}")
                }
            }
        }
        // An array's String is its elements' joined by commas, null and undefined as nothing.
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".into(),
    }
}

/// `directMask`: the scope classes a record kind belongs to directly.
fn direct_mask(kind: &str) -> u8 {
    match kind {
        "user" | "command" => U,
        "assistant" => A,
        "think" | "act" => T,
        "bash" | "edit" | "write" | "read" | "skill" | "tool" => O,
        _ => 0,
    }
}

/// `stripTags`: tags out (each `<…>` a space), then the five entities, in turn — so `&amp;lt;`
/// becomes `&lt;` and then `<`, as the page's sequential replaces leave it.
pub(crate) fn strip_tags(h: Option<&Value>) -> String {
    let s = match h {
        None | Some(Value::Null) => String::new(),
        other => js_string(other),
    };
    strip_str(&s)
}

fn strip_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('<') {
        match rest[open..].find('>') {
            Some(close) => {
                out.push_str(&rest[..open]);
                out.push(' ');
                rest = &rest[open + close + 1..];
            }
            None => break,
        }
    }
    out.push_str(rest);
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// A JSON array's items, or none (`x || []` over what the renderer writes).
fn items(v: Option<&Value>) -> &[Value] {
    v.and_then(Value::as_array).map_or(&[], Vec::as_slice)
}

/// `ownTextParts`: a record's own text parts (nested records excluded), in reading order.
fn own_text_parts(b: &Value) -> Vec<String> {
    let mut parts = Vec::new();
    if let Some(h) = b.get("head").filter(|h| h.is_object()) {
        for k in [
            "summary", "badge", "preview", "name", "target", "att_name", "from",
        ] {
            if let Some(v) = h.get(k).filter(|v| truthy(v)) {
                parts.push(js_string(Some(v)));
            }
        }
    }
    for p in items(b.get("body")) {
        match p.get("p").and_then(Value::as_str) {
            Some("md" | "think") => parts.push(strip_tags(p.get("h"))),
            Some("pre" | "note") => parts.push(js_string(p.get("x"))),
            Some("num") => {
                for r in items(p.get("rows")) {
                    parts.push(strip_str(&js_string(r.get(1))));
                }
            }
            Some("diff") => {
                for r in items(p.get("rows")) {
                    parts.push(js_string(r.get(2)));
                }
            }
            _ => {}
        }
    }
    parts
}

/// One own text inside a record's searchable text: its span (bytes of the lowercased text), the
/// scope classes of the record it belongs to, and that record's tool.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Part {
    pub start: usize,
    pub end: usize,
    pub mask: u8,
    pub tool: String,
}

/// `recordTextParts` with the app shell's `lower` (`toLowerCase`): the record's own text, then each
/// nested record's, newline-joined and lowercased part by part, with each part's span.
pub(crate) fn record_text_parts(b: &Value) -> (String, Vec<Part>) {
    fn walk(record: &Value, text: &mut String, parts: &mut Vec<Part>) {
        let own = own_text_parts(record).join("\n").to_lowercase();
        if !own.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            let start = text.len();
            text.push_str(&own);
            let tool = record
                .get("tool")
                .filter(|t| truthy(t))
                .map(|t| js_string(Some(t)))
                .unwrap_or_default();
            parts.push(Part {
                start,
                end: text.len(),
                mask: direct_mask(record.get("kind").and_then(Value::as_str).unwrap_or("")),
                tool,
            });
        }
        for p in items(record.get("body")) {
            if p.get("p").and_then(Value::as_str) == Some("blocks") {
                for item in items(p.get("items")) {
                    walk(item, text, parts);
                }
            }
        }
    }
    let (mut text, mut parts) = (String::new(), Vec::new());
    walk(b, &mut text, &mut parts);
    (text, parts)
}

/// The word characters of the whole-word rule (`WORD_LEFT`/`WORD_RIGHT`): `[\p{L}\p{N}\p{M}_]`,
/// as sorted ranges taken from the Unicode tables syntect's regex already carries.
fn word_ranges() -> &'static [(char, char)] {
    static RANGES: OnceLock<Vec<(char, char)>> = OnceLock::new();
    RANGES.get_or_init(|| {
        let hir = regex_syntax::Parser::new()
            .parse(r"[\p{L}\p{N}\p{M}_]")
            .expect("the word class parses");
        match hir.kind() {
            regex_syntax::hir::HirKind::Class(regex_syntax::hir::Class::Unicode(class)) => class
                .ranges()
                .iter()
                .map(|r| (r.start(), r.end()))
                .collect(),
            other => panic!("the word class is a Unicode class, not {other:?}"),
        }
    })
}

fn is_word(c: char) -> bool {
    let ranges = word_ranges();
    let at = ranges.partition_point(|(_, end)| *end < c);
    ranges.get(at).is_some_and(|(start, _)| *start <= c)
}

/// `wholeAt`: the match at `start` (bytes) of `len` bytes in `t` stands between non-word characters.
fn whole_at(t: &str, start: usize, len: usize) -> bool {
    !t[..start].chars().next_back().is_some_and(is_word)
        && !t[start + len..].chars().next().is_some_and(is_word)
}

/// `countOcc`: non-overlapping occurrences of the lowercase needle in `t`, whole words only when
/// `whole`. An empty needle counts nothing.
pub(crate) fn count_occ(t: &str, lc: &str, whole: bool) -> usize {
    if lc.is_empty() {
        return 0;
    }
    let (mut n, mut i) = (0, 0);
    while let Some(at) = t[i..].find(lc) {
        let start = i + at;
        if !whole || whole_at(t, start, lc.len()) {
            n += 1;
        }
        i = start + lc.len();
    }
    n
}

/// `toolMatches`: does a record's tool answer one of `tools`? None asks nothing.
pub(crate) fn tool_matches(tool: &str, tools: &[Tool]) -> bool {
    if tools.is_empty() {
        return true;
    }
    if tool.is_empty() {
        return false;
    }
    let lc = tool.to_lowercase();
    tools.iter().any(|t| {
        let name = t.name.to_lowercase();
        if t.prefix {
            lc.starts_with(&name)
        } else {
            lc == name
        }
    })
}

/// `partWanted`: is one text part inside the search?
fn part_wanted(part: &Part, wanted: u8, tools: &[Tool]) -> bool {
    if wanted == 0 {
        return true;
    }
    if part.mask & wanted == 0 {
        return false;
    }
    part.mask & O == 0 || tool_matches(&part.tool, tools)
}

/// `countRecord`: one record's hits in scope, with every part's hits added into `counts` by its
/// classes whatever the scope; unscoped, the whole text's count.
pub(crate) fn count_record(
    text: &str,
    parts: &[Part],
    q: &Query,
    counts: &mut [usize; 4],
) -> usize {
    let mut in_scope = 0;
    for part in parts {
        let n = count_occ(&text[part.start..part.end], &q.lc, q.whole);
        if n == 0 {
            continue;
        }
        for (k, bit) in CLASS_ORDER.iter().enumerate() {
            if part.mask & bit != 0 {
                counts[k] += n;
            }
        }
        if part_wanted(part, q.wanted, &q.tools) {
            in_scope += n;
        }
    }
    if q.wanted != 0 {
        in_scope
    } else {
        count_occ(text, &q.lc, q.whole)
    }
}

/// The app shell's filter (`filterChain`): does a record, or a record nested in it, hold a tool call
/// whose name (`toolNameOf`: its `tool`, else its head's `name`) answers one of `tools`?
pub(crate) fn filter_chain(b: &Value, tools: &[Tool]) -> bool {
    const TOOL_KINDS: [&str; 6] = ["bash", "read", "write", "edit", "skill", "tool"];
    let kind = b.get("kind").and_then(Value::as_str).unwrap_or("");
    let name = b
        .get("tool")
        .filter(|t| truthy(t))
        .or_else(|| {
            b.get("head")
                .and_then(|h| h.get("name"))
                .filter(|n| truthy(n))
        })
        .map(|n| js_string(Some(n)))
        .unwrap_or_default();
    if TOOL_KINDS.contains(&kind) && tool_matches(&name, tools) {
        return true;
    }
    items(b.get("body"))
        .iter()
        .filter(|p| p.get("p").and_then(Value::as_str) == Some("blocks"))
        .any(|p| {
            items(p.get("items"))
                .iter()
                .any(|item| filter_chain(item, tools))
        })
}

/// `replyContext`: a quoted reply's context, when it has exactly the shape the page draws.
fn reply_context(record: &Value) -> Option<&Value> {
    let value = record.get("reply_context")?;
    let object = |v: &Value| v.is_object();
    let keys = |v: &Value, allowed: &[&str]| {
        v.as_object()
            .is_some_and(|o| o.keys().all(|k| allowed.contains(&k.as_str())))
    };
    let text = |v: Option<&Value>| v.and_then(Value::as_str).is_some_and(|s| !s.is_empty());
    if !object(value) || !keys(value, &["version", "items", "request", "context"]) {
        return None;
    }
    if value.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let list = value.get("items").and_then(Value::as_array)?;
    if list.is_empty() {
        return None;
    }
    if value.get("request").is_some() && !text(value.get("request")) {
        return None;
    }
    if value.get("context").is_some_and(|c| !c.is_string()) {
        return None;
    }
    for item in list {
        if !object(item) || !keys(item, &["quote", "response"]) || !text(item.get("quote")) {
            return None;
        }
        if let Some(response) = item.get("response") {
            let kind = response.get("kind").and_then(Value::as_str);
            if !object(response)
                || !keys(response, &["kind", "text"])
                || !matches!(kind, Some("comment" | "following_text"))
                || !text(response.get("text"))
            {
                return None;
            }
        }
    }
    Some(value)
}

/// `recordForSearch`: a prompt that quotes a reply is searched by its quotes and responses (and the
/// context and request), as the page draws it, not by its typed body.
pub(crate) fn record_for_search(record: &Value) -> Cow<'_, Value> {
    if record.get("kind").and_then(Value::as_str) != Some("user") {
        return Cow::Borrowed(record);
    }
    let Some(reply) = reply_context(record) else {
        return Cow::Borrowed(record);
    };
    let s = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
    let mut texts = Vec::new();
    for item in items(reply.get("items")) {
        texts.push(s(item.get("quote")));
        texts.push(s(item.get("response").and_then(|r| r.get("text"))));
    }
    texts.push(s(reply.get("context")));
    texts.push(s(reply.get("request")));
    let body: Vec<Value> = texts
        .into_iter()
        .filter(|t| !t.is_empty())
        .map(|x| json!({"p": "pre", "x": x}))
        .collect();
    let mut out = record.clone();
    if let Some(o) = out.as_object_mut() {
        o.insert("body".into(), Value::Array(body));
    }
    Cow::Owned(out)
}

/// What one search found: the records with hits in scope (or, for a query with tools and no text,
/// the records holding such a call), the per-class counts, and the total in scope.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Found {
    pub records: Vec<(usize, usize)>,
    pub counts: [usize; 4],
    pub total: usize,
}

impl Found {
    /// Take record `r` into the search, as the page does in `updateSearch` (text) or
    /// `computeFilterHits` (tools alone: an empty list matches nothing there).
    pub(crate) fn take(&mut self, r: usize, record: &Value, q: &Query) {
        if q.lc.is_empty() {
            if !q.tools.is_empty() && filter_chain(record, &q.tools) {
                self.records.push((r, 1));
                self.total += 1;
            }
            return;
        }
        let searched = record_for_search(record);
        let (text, parts) = record_text_parts(&searched);
        let n = count_record(&text, &parts, q, &mut self.counts);
        if n > 0 {
            self.records.push((r, n));
            self.total += n;
        }
    }

    pub(crate) fn reply(&self, epoch: u64, upto: usize) -> Value {
        json!({
            "t": "search", "epoch": epoch, "upto": upto,
            "records": self.records.iter().map(|(r, n)| json!([r, n])).collect::<Vec<_>>(),
            "counts": {"u": self.counts[0], "a": self.counts[1], "t": self.counts[2], "o": self.counts[3]},
            "total": self.total,
        })
    }
}

/// Search the first `upto` records of the record log at `path` (no further than `log_len` bytes):
/// each committed record is one JSON line, read and matched in turn. A line that does not parse ends
/// the read, as the turn index's does (the page starts over on what it then cannot match). `masked`:
/// the client is sent masked records (#365), so each is masked before it is matched — the counts are
/// over what the page will hold, and never a way to read a masked secret back a character at a time.
pub(crate) fn search_log(
    path: &std::path::Path,
    log_len: u64,
    upto: usize,
    q: &Query,
    masked: bool,
) -> std::io::Result<Found> {
    use std::io::{BufRead, Read};
    let file = std::fs::File::open(path)?;
    let mut reader = std::io::BufReader::new(file.take(log_len));
    let mut found = Found::default();
    let mut line = Vec::new();
    for r in 0..upto {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 || line.last() != Some(&b'\n') {
            break;
        }
        line.pop();
        if masked {
            super::mask::mask_json_bytes(&mut line);
        }
        let Ok(record) = serde_json::from_slice::<Value>(&line) else {
            break;
        };
        found.take(r, &record, q);
    }
    Ok(found)
}

/// What a search costs on a real record log (`design/slow-links.md` §7.2: whether the server should
/// keep each record's lowercased text decides on this): `CR_SEARCH_PROBE=<records.jsonl> cargo test
/// --release -p claude-replay-html probe_search -- --ignored --nocapture`. Prints timings and counts,
/// never text.
#[test]
#[ignore = "reads a real record log named by CR_SEARCH_PROBE"]
fn probe_search_a_real_log() {
    let Ok(log) = std::env::var("CR_SEARCH_PROBE") else {
        return;
    };
    let path = std::path::Path::new(&log);
    let len = std::fs::metadata(path).unwrap().len();
    let probes = [
        (
            "the, everywhere",
            Query {
                lc: "the".into(),
                ..Query::default()
            },
        ),
        (
            "deploy, whole words",
            Query {
                lc: "deploy".into(),
                whole: true,
                ..Query::default()
            },
        ),
        (
            "error in tools",
            Query {
                lc: "error".into(),
                wanted: O,
                ..Query::default()
            },
        ),
        (
            "Bash calls (the filter)",
            Query {
                tools: vec![Tool {
                    name: "Bash".into(),
                    prefix: false,
                }],
                ..Query::default()
            },
        ),
    ];
    for (what, q) in probes {
        let t = std::time::Instant::now();
        let found = search_log(path, len, usize::MAX, &q, false).unwrap();
        eprintln!(
            "log {} MB, {what}: {:?}, {} records with hits, {} in all",
            len / 1_000_000,
            t.elapsed(),
            found.records.len(),
            found.total
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #s286: a masked search reads on past a record whose private key's span holds a JSON `\u`
    /// escape (terminal colour codes); the mask once broke that record's JSON, the read stopped
    /// there, and every hit after it went uncounted.
    #[test]
    fn a_masked_search_reads_past_a_key_span_holding_an_escape() {
        let tail = ["PRIVATE", "KEY-----"].join(" ");
        let out = format!(
            "\u{1b}[1m-----BEGIN OPENSSH {tail}\n{}\u{1b}[0m\n-----END OPENSSH {tail}",
            "k".repeat(40)
        );
        let log = [
            json!({"kind": "bash", "tool": "Bash", "body": [{"p": "pre", "x": out}]}),
            json!({"kind": "bash", "tool": "Bash", "body": [{"p": "pre", "x": "count the petrel nests"}]}),
        ]
        .iter()
        .map(|r| r.to_string() + "\n")
        .collect::<String>();
        let path =
            std::env::temp_dir().join(format!("cr-search-escape-{}.jsonl", std::process::id()));
        std::fs::write(&path, &log).unwrap();
        let len = log.len() as u64;
        for masked in [false, true] {
            let found = search_log(&path, len, 2, &q("petrel", false, 0, &[]), masked).unwrap();
            assert_eq!(found.total, 1, "the hit after the key (masked: {masked})");
        }
        let _ = std::fs::remove_file(&path);
    }

    fn q(lc: &str, whole: bool, wanted: u8, tools: &[(&str, bool)]) -> Query {
        Query {
            lc: lc.into(),
            whole,
            wanted,
            tools: tools
                .iter()
                .map(|(n, p)| Tool {
                    name: (*n).into(),
                    prefix: *p,
                })
                .collect(),
        }
    }

    /// The page's quirks, each as the page has it.
    #[test]
    fn the_text_is_the_pages_quirks_included() {
        assert_eq!(strip_str("a<b>c</b>d"), "a c d");
        assert_eq!(
            strip_str("&amp;lt;x&amp;gt;"),
            "<x>",
            "sequential replaces, as the page's"
        );
        assert_eq!(strip_str("1 < 2"), "1 < 2", "a < with no > is text");
        assert_eq!(strip_str("<a<b>c"), " c", "one tag up to the first >");
        let r = json!({"kind": "tool", "head": {"summary": "Run", "badge": 0, "name": "", "target": 7},
                        "body": [{"p": "pre"}, {"p": "num", "rows": [[1, "<i>x</i>"]]}, {"p": "md", "h": null}]});
        assert_eq!(
            own_text_parts(&r),
            ["Run", "7", "undefined", " x ", ""],
            "falsy head fields skipped, String(undefined) for a part with no text"
        );
        assert_eq!(js_string(Some(&json!(1.5))), "1.5");
        assert_eq!(js_string(Some(&json!([1, null, "a"]))), "1,,a");
    }

    #[test]
    fn counting_follows_the_page() {
        let r = json!({"kind": "act", "head": {"summary": "Deploy"}, "body": [{"p": "blocks", "items": [
            {"kind": "bash", "tool": "Bash", "body": [{"p": "pre", "x": "deploy deployment"}]},
            {"kind": "read", "tool": "Read", "body": [{"p": "pre", "x": "x_deploy"}]},
        ]}]});
        let (text, parts) = record_text_parts(&r);
        assert_eq!(text, "deploy\ndeploy deployment\nx_deploy");
        assert_eq!(parts.iter().map(|p| p.mask).collect::<Vec<_>>(), [T, O, O]);
        let mut counts = [0; 4];
        assert_eq!(
            count_record(&text, &parts, &q("deploy", false, 0, &[]), &mut counts),
            4
        );
        assert_eq!(
            counts,
            [0, 0, 1, 3],
            "every part counted by its class, scope or not"
        );
        let mut c = [0; 4];
        assert_eq!(
            count_record(&text, &parts, &q("deploy", true, 0, &[]), &mut c),
            2,
            "whole words: _ is a word character"
        );
        let mut c = [0; 4];
        assert_eq!(
            count_record(
                &text,
                &parts,
                &q("deploy", false, O, &[("Bash", false)]),
                &mut c
            ),
            2,
            "o(Bash) narrows the tools class"
        );
        let mut c = [0; 4];
        assert_eq!(
            count_record(
                &text,
                &parts,
                &q("deploy", false, T | O, &[("Read", false)]),
                &mut c
            ),
            2,
            "the thinking, and Read's"
        );
        assert!(!filter_chain(
            &r,
            &[Tool {
                name: "mcp__".into(),
                prefix: true
            }]
        ));
        assert!(
            filter_chain(
                &r,
                &[Tool {
                    name: "bash".into(),
                    prefix: false
                }]
            ),
            "names case-insensitively, nested calls included"
        );
        let named = json!({"kind": "tool", "head": {"name": "mcp__github__get"}, "body": []});
        assert!(
            filter_chain(
                &named,
                &[Tool {
                    name: "mcp__github__".into(),
                    prefix: true
                }]
            ),
            "a call named by its head"
        );
        let think = json!({"kind": "think", "tool": "Bash", "body": []});
        assert!(
            !filter_chain(
                &think,
                &[Tool {
                    name: "Bash".into(),
                    prefix: false
                }]
            ),
            "only a tool-call kind is a call"
        );
    }

    #[test]
    fn whole_words_read_unicode_classes() {
        assert_eq!(count_occ("café x", "caf", true), 0, "é is a letter");
        assert_eq!(
            count_occ("cafe\u{301} x", "cafe", true),
            0,
            "a combining mark is a word character"
        );
        assert_eq!(count_occ("漢字 x", "漢", true), 0);
        assert_eq!(count_occ("a-b", "a", true), 1);
        assert_eq!(count_occ("aaaa", "aa", false), 2, "non-overlapping");
        assert_eq!(count_occ("x", "", false), 0);
    }

    #[test]
    fn a_quoted_reply_is_searched_by_what_the_page_draws() {
        let r = json!({"kind": "user", "body": [{"p": "md", "h": "typed"}],
            "reply_context": {"version": 1, "items": [{"quote": "the quote", "response": {"kind": "comment", "text": "my note"}}], "request": "please"}});
        let (text, _) = record_text_parts(&record_for_search(&r));
        assert_eq!(text, "the quote\nmy note\nplease");
        let bad = json!({"kind": "user", "body": [{"p": "md", "h": "typed"}], "reply_context": {"version": 2, "items": []}});
        assert_eq!(
            record_text_parts(&record_for_search(&bad)).0,
            "typed",
            "a context of another shape is not drawn"
        );
    }

    /// Records the audit corpus may not carry: Unicode case and word edges, entities, numbers in a
    /// head, a part with no text, a quoted reply, calls named by their head, a call kind inside
    /// thinking.
    fn extra_records() -> Vec<Value> {
        vec![
            json!({"kind": "user", "id": "t1", "body": [{"p": "md", "h": "<p>ΟΔΟΣ and İstanbul, café, cafe\u{301}, naïve 漢字テスト</p>"}]}),
            json!({"kind": "assistant", "id": "b1", "head": {"summary": "&amp;lt;tag&amp;gt;", "badge": 3, "preview": false},
                   "body": [{"p": "md", "h": "x &lt;y&gt; &quot;z&quot; it&#39;s a_b a-b"}, {"p": "note"}]}),
            json!({"kind": "act", "id": "b2", "body": [{"p": "blocks", "items": [
                {"kind": "bash", "id": "b3", "tool": "Bash", "body": [{"p": "pre", "x": "deploy the_deploy deployment"}]},
                {"kind": "tool", "id": "b4", "head": {"name": "mcp__github__get_issue", "target": "deploy"}, "body": []},
                {"kind": "edit", "id": "b5", "tool": "Update", "body": [{"p": "diff", "rows": [[1, " ", "deploy here"], [2, "+"]]}]},
                {"kind": "read", "id": "b6", "tool": "Read", "body": [{"p": "num", "rows": [[1, "<span>deploy</span>"], [2]]}]},
            ]}]}),
            json!({"kind": "think", "id": "b7", "tool": "Bash", "body": [{"p": "think", "h": "think about deploy"}]}),
            json!({"kind": "user", "id": "t2", "body": [{"p": "md", "h": "typed text"}],
                   "reply_context": {"version": 1, "items": [{"quote": "Deploy the thing", "response": {"kind": "comment", "text": "why deploy"}}], "context": "", "request": "please deploy"}}),
            json!({"kind": "command", "id": "t3", "head": {"summary": "/deploy now"}, "body": []}),
            // Where the classes differ: a combining mark and a circled letter (a symbol, yet
            // `char::is_alphabetic`) against a match; an entity escaped twice inside Markdown.
            json!({"kind": "assistant", "id": "b8", "body": [{"p": "md", "h": "cafe\u{301} \u{24b6}deploy deploy\u{24b6} &amp;lt;b&amp;gt; &amp;amp;amp"}]}),
        ]
    }

    /// The queries: fixed needles, needles taken from the records' own text, each with whole words
    /// on and off, every class mask, and tools named and by family; and the filter's tools alone.
    fn parity_queries(records: &[Value]) -> Vec<Query> {
        let mut needles: Vec<String> = [
            "deploy",
            "the",
            "fn",
            "error",
            "x",
            "é",
            "e\u{301}",
            "漢字",
            "<",
            "&",
            "\"",
            "'",
            "a b",
            "a_b",
            "-",
            ".",
            "lorem",
            "bash",
            "οδος",
            "οδοσ",
            "i\u{307}",
            "caf",
            "naï",
            "テスト",
            "it's",
            "<y>",
            "/deploy",
            "cafe",
            "<b>",
            "&amp",
            "&amp;",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for record in records {
            let (text, _) = record_text_parts(&record_for_search(record));
            let chars: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
            if chars.len() >= 4 {
                needles.push(chars[..3].iter().collect());
                let mid = chars.len() / 2;
                needles.push(chars[mid..(mid + 4).min(chars.len())].iter().collect());
            }
            if let Some(word) = text.split_whitespace().nth(1) {
                needles.push(word.to_string());
            }
        }
        needles.sort();
        needles.dedup();
        let tool = |n: &str, p: bool| Tool {
            name: n.into(),
            prefix: p,
        };
        let tool_sets: Vec<Vec<Tool>> = vec![
            vec![],
            vec![tool("Bash", false)],
            vec![tool("Read", false), tool("bash", false)],
            vec![tool("mcp__github__", true)],
            vec![tool("Update", false)],
        ];
        let masks = [0, U, A, T, O, U | A, T | O, U | A | T | O];
        let mut out = Vec::new();
        for lc in &needles {
            for whole in [false, true] {
                for &wanted in &masks {
                    for tools in &tool_sets {
                        if wanted & O == 0 && !tools.is_empty() {
                            continue;
                        }
                        out.push(Query {
                            lc: lc.clone(),
                            whole,
                            wanted,
                            tools: tools.clone(),
                        });
                    }
                }
            }
        }
        for tools in &tool_sets {
            out.push(Query {
                lc: String::new(),
                whole: false,
                wanted: 0,
                tools: tools.clone(),
            });
        }
        out
    }

    /// The parity test (`design/slow-links.md` §7.4): the server's matcher and the page's own
    /// modules, under node, over the rendering audit's corpus and the records above, for every query
    /// above — any difference fails. The page's side is `recordTextParts(recordForSearch(r),
    /// stripTags, toLowerCase)` and `countRecord`, as `updateSearch` runs them, and the filter's
    /// chain as `filterChain` is written (it lives in app.js, which node cannot import). Skipped
    /// without node, as the frontend contract is; CI has it.
    #[test]
    fn parity_with_the_page() {
        let mut records =
            crate::html_export::audit::audit_records(&crate::html_export::audit::audit_corpus());
        records.extend(extra_records());
        let queries = parity_queries(&records);
        let ours: Vec<Value> = queries
            .iter()
            .map(|q| {
                let mut found = Found::default();
                for (r, record) in records.iter().enumerate() {
                    found.take(r, record, q);
                }
                let reply = found.reply(0, records.len());
                json!({"records": reply["records"], "counts": reply["counts"], "total": reply["total"]})
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("cr-search-parity-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let search = manifest.join("src/html/shared/search.js");
        let view_model = manifest.join("../claude-monitor/src/codex-ui/view-model.js");
        let queries_json: Vec<Value> = queries
            .iter()
            .map(|q| json!({"lc": q.lc, "whole": q.whole, "wanted": q.wanted,
                "tools": q.tools.iter().map(|t| json!({"name": t.name, "prefix": t.prefix})).collect::<Vec<_>>()}))
            .collect();
        std::fs::write(
            dir.join("records.json"),
            serde_json::to_vec(&records).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("queries.json"),
            serde_json::to_vec(&queries_json).unwrap(),
        )
        .unwrap();
        let script = format!(
            r#"import fs from "node:fs";
import {{ recordTextParts, countRecord, stripTags, zeroCounts, toolMatches }} from {search:?};
import {{ recordForSearch }} from {view_model:?};
const [records, queries] = [process.argv[2], process.argv[3]].map(p => JSON.parse(fs.readFileSync(p, "utf8")));
const toolKinds = new Set(["bash", "read", "write", "edit", "skill", "tool"]);
const toolNameOf = r => r.tool || r.head?.name || "";
const chain = (r, tools) => (toolKinds.has(r.kind) && toolMatches(toolNameOf(r), tools)) || (r.body || []).some(p => p.p === "blocks" && (p.items || []).some(i => chain(i, tools)));
const out = queries.map(q => {{
  const counts = zeroCounts(), recs = [];
  let total = 0;
  records.forEach((record, r) => {{
    if (!q.lc) {{ if (q.tools.length && chain(record, q.tools)) {{ recs.push([r, 1]); total += 1; }} return; }}
    const {{ text, parts }} = recordTextParts(recordForSearch(record), stripTags, s => s.toLowerCase());
    const n = countRecord(text, parts, q.lc, q.whole, q.wanted, counts, q.tools);
    if (n) {{ recs.push([r, n]); total += n; }}
  }});
  return {{ records: recs, counts: {{ u: counts.u, a: counts.a, t: counts.t, o: counts.o }}, total }};
}});
fs.writeFileSync(process.argv[4], JSON.stringify(out));
"#,
            search = search.to_string_lossy(),
            view_model = view_model.to_string_lossy(),
        );
        let script_path = dir.join("parity.mjs");
        std::fs::write(&script_path, script).unwrap();
        let theirs_path = dir.join("theirs.json");
        let run = std::process::Command::new("node")
            .arg(&script_path)
            .arg(dir.join("records.json"))
            .arg(dir.join("queries.json"))
            .arg(&theirs_path)
            .output();
        let run = match run {
            Ok(run) => run,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                eprintln!("skip: no `node` on PATH — CI runs this test where node is present");
                return;
            }
            Err(e) => panic!("could not run node: {e}"),
        };
        assert!(
            run.status.success(),
            "the page's side failed:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let theirs: Vec<Value> =
            serde_json::from_slice(&std::fs::read(&theirs_path).unwrap()).unwrap();
        assert_eq!(theirs.len(), ours.len());
        let differ: Vec<String> = ours
            .iter()
            .zip(&theirs)
            .zip(&queries_json)
            .filter(|((a, b), _)| a != b)
            .take(5)
            .map(|((a, b), q)| format!("query {q}\n  server {a}\n  page   {b}"))
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        eprintln!(
            "parity: {} records, {} queries, {} hits in all",
            records.len(),
            queries.len(),
            ours.iter()
                .map(|o| o["total"].as_u64().unwrap_or(0))
                .sum::<u64>()
        );
        assert!(
            differ.is_empty(),
            "the server and the page disagree:\n{}",
            differ.join("\n")
        );
    }
}

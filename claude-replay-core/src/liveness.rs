//! **Transcript-tree liveness signals** — agent-neutral file reads that answer "is this
//! session doing anything?", moved down from the `jdi` supervisor (#98 §10) so a consumer
//! that is not the viewer binary (the monitor, a separate repo later) can link them without
//! dragging in clap, ratatui and the whole TUI.
//!
//! Two signals, and they are deliberately BOTH needed (#82): mtime growth misses a session
//! blocked in a long tool call (transcripts are only written when a result *lands*, so a
//! `cargo build` leaves the whole tree untouched while the session is maximally busy), and
//! the in-flight check alone misses ordinary streaming growth between tool calls.

use std::path::Path;

/// The most recent write anywhere in a session's transcript TREE: the root transcript
/// plus every child transcript under its `<stem>/subagents/` dir — the signal that the
/// session is actively working even when the root is quiet (a sub-agent holds the turn).
/// `None` when nothing is readable.
pub fn latest_tree_activity(transcript: &Path) -> Option<std::time::SystemTime> {
    let mut latest = std::fs::metadata(transcript)
        .and_then(|m| m.modified())
        .ok();
    let subagents = transcript
        .parent()
        .zip(transcript.file_stem())
        .map(|(dir, stem)| dir.join(stem).join("subagents"));
    if let Some(entries) = subagents.and_then(|d| std::fs::read_dir(d).ok()) {
        for e in entries.flatten() {
            if let Ok(m) = e.metadata().and_then(|m| m.modified()) {
                latest = Some(latest.map_or(m, |cur| cur.max(m)));
            }
        }
    }
    latest
}

/// How far back the in-flight scan reads. Large enough that a tool call's `tool_use` line is
/// still inside the window when its result lands pages later; bounded so the check is O(1)
/// in transcript size.
const INFLIGHT_TAIL_BYTES: u64 = 262_144;

/// One Codex tool family that can leave a call dangling in the tail: a call record plus the
/// SEPARATE output record that closes it. Both fields are `payload.type` values, matched
/// exactly — never as free substrings, because a tool output can quote a transcript.
///
/// Self-contained record kinds are deliberately absent. The Codex reader also renders
/// `web_search_call` and `image_generation_call` as tool uses (`agents::codex::model`), but
/// those arrive as ONE complete record with no output counterpart — the search results and
/// the image bytes are inside the call record itself. Tracking them here could only ever
/// leave a permanently unmatched id, i.e. a finished session reported busy forever. That is
/// the mirror image of the #82 gap this check exists to close, and the worse half: #82 kills
/// working sessions, this would keep dead ones alive.
struct CodexToolPattern {
    /// `payload.type` of a call record.
    call: &'static str,
    /// `payload.type` of the record that closes it.
    result: &'static str,
    /// Keys that may carry the call identity, in precedence order — the FIRST key present on
    /// the record wins, exactly as the Codex reader's `specialized_call_id` pairs the two
    /// records. Taking every key instead is a bug on real rollouts: a `tool_search_call`
    /// carries both `id` (`tsc_…`) and `call_id` (`call_…`), and only the `call_id` repeats
    /// on the `tool_search_output` (whose own `id` is `tso_…`), so the call's `id` would
    /// dangle for the rest of the session.
    id_keys: &'static [&'static str],
}

const CODEX_TOOL_PATTERNS: &[CodexToolPattern] = &[
    CodexToolPattern {
        call: "function_call",
        result: "function_call_output",
        id_keys: &["call_id"],
    },
    CodexToolPattern {
        call: "custom_tool_call",
        result: "custom_tool_call_output",
        id_keys: &["call_id"],
    },
    CodexToolPattern {
        call: "tool_search_call",
        result: "tool_search_output",
        id_keys: &["call_id", "id"],
    },
];

/// Whether the transcript's TAIL holds an **in-flight tool call** — a `tool_use` (Claude)
/// or Codex call with no matching result yet. Mid-tool is BUSY BY CONSTRUCTION even when
/// every file mtime is stale: transcripts are only written when a result LANDS, so during a
/// long `cargo build`/test run the whole tree sits untouched (#82 — the gap in #32's
/// quiet-tree signal that got working sessions SIGKILLed).
///
/// Scans the last `INFLIGHT_TAIL_BYTES` (256 KiB) only; a use whose result predates the window
/// can't appear dangling (uses are collected from within the window, results may close
/// them from anywhere in it) — which holds only for the transcript's own WHOLE records, so a
/// COPY of a call (Claude Code's API-call log, #s329) and the line the window's edge cuts
/// (#s365) are never read. Covers sub-agent work too: while a child runs, the parent's spawning
/// `tool_use` is itself unresolved in the ROOT tail.
pub fn inflight_tool_in_tail(transcript: &Path) -> bool {
    !inflight_tools_in_tail(transcript).is_empty()
}

/// One unresolved call from the in-flight scan (#194): its pairing id and the tool's
/// name as the raw record carried it (best-effort — a name the scan cannot see is an
/// empty string, never a reason to drop the pending fact).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InflightTool {
    pub id: String,
    pub name: String,
}

/// [`inflight_tool_in_tail`], upgraded to say WHICH calls are unresolved — the S2
/// carrier of design/agent-states.md: the consumer joins `name` against the adapter's
/// `tool_is_interactive` (#21) to tell a modal wait from a running tool. Same window,
/// same lossy-decode rationale, same pairing rules as the boolean form.
pub fn inflight_tools_in_tail(transcript: &Path) -> Vec<InflightTool> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(transcript) else {
        return Vec::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    // One byte before the window, so a window opening exactly on a line's start keeps that line.
    let start = len.saturating_sub(INFLIGHT_TAIL_BYTES + 1);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut raw = Vec::new();
    if f.read_to_end(&mut raw).is_err() {
        return Vec::new();
    }
    // #s365: the window starts at a fixed byte, so its first line is usually the end of a record
    // whose start lies outside it, and nothing can be said about that record: not its type, so
    // the blob filter below never sees a cut `api-request-blob`, and not its calls' results, which
    // lie before the window. Read as a line, it made a finished `claude -p` worker read
    // `exited-mid-work` (12 of 364 that ended on `end_turn` on aries-black, 6 of 174 on
    // hong-devserver, measured 2026-11-05; none of those 174 once the line goes). A real call is
    // lost this way only if its own record reaches past the window, whose `tool_use` head was
    // then outside it already.
    let whole: &[u8] = if start == 0 {
        &raw
    } else {
        raw.iter()
            .position(|&b| b == b'\n')
            .map_or(&[], |i| &raw[i + 1..])
    };
    // LOSSY on purpose. A strict decode that meets one bad byte discards the ENTIRE signal and
    // answers "idle", which is the direction that gets a working session reaped (#82): the
    // window's edge did that on 7 of 558 real rollouts (~1.3%) by opening inside a character.
    // That character now goes with the cut line; whatever else is bad costs one replacement
    // char in one field, not the whole check.
    let buf = String::from_utf8_lossy(whole);
    // Insertion-ordered id → name (#194): the order calls were made is the order the
    // consumer wants to report them; a re-seen id keeps its first name.
    let mut uses: Vec<(String, String)> = Vec::new();
    let mut results: std::collections::HashSet<String> = std::collections::HashSet::new();
    fn note_use(uses: &mut Vec<(String, String)>, id: String, name: &str) {
        if !uses.iter().any(|(u, _)| *u == id) {
            uses.push((id, name.to_string()));
        }
    }
    // Field-level extraction: `key":"value"` occurrences on each line. Claude marks calls
    // as `"type":"tool_use"` with `"id":"toolu_…"` and results via `"tool_use_id"`; Codex
    // marks calls with `"type":"<call_type>"` and closes them with matching result records,
    // both carrying one of the id keys (`call_id` or `id`).
    fn field_values<'a>(line: &'a str, key: &str) -> impl Iterator<Item = &'a str> {
        let pat = format!("\"{key}\":\"");
        let mut rest = line;
        let mut out = Vec::new();
        while let Some(i) = rest.find(&pat) {
            let v = &rest[i + pat.len()..];
            if let Some(end) = v.find('"') {
                out.push(&v[..end]);
                rest = &v[end..];
            } else {
                break;
            }
        }
        out.into_iter()
    }
    /// A Codex record's own kind, read from the `payload` envelope it is always serialized
    /// with (`{"type":"response_item","payload":{"type":"…"`). Anchoring here rather than
    /// searching the line keeps a tool output that QUOTES a transcript — this repo's own
    /// sessions do exactly that — from being mistaken for the record it quotes.
    fn codex_payload_type(line: &str) -> Option<&str> {
        const ANCHOR: &str = "\"payload\":{\"type\":\"";
        let rest = &line[line.find(ANCHOR)? + ANCHOR.len()..];
        rest.find('"').map(|end| &rest[..end])
    }
    /// The identity a Codex call and its output are paired by: the first of `keys` present on
    /// the record, mirroring the reader's `specialized_call_id`. `None` when absent or empty —
    /// an id we cannot read is not an id we can wait for.
    fn call_identity(line: &str, keys: &[&str]) -> Option<String> {
        let raw = keys.iter().find_map(|k| field_values(line, k).next())?;
        (!raw.is_empty()).then(|| raw.to_string())
    }
    for line in buf.lines() {
        // #s329: Claude Code's log of its API calls (#s134) is never a call or a result. An
        // `api-request-blob` holds a message as sent, the ids of its calls or results included,
        // and lands on either side of the record it copies (over 270 transcripts on hong-devserver
        // on 2026-11-05: 92% of its calls and 38% of its results after the record, the rest
        // before). A window whose edge fell between a result and a blob after it read the call as
        // running: a finished `claude -p` worker whose process was gone read `exited-mid-work`
        // (11 of 131 that ended on `end_turn` on aries-black over three days, measured
        // 2026-10-30), and a live one could read as a permission wait. Each such line puts its
        // type first.
        if line.starts_with("{\"type\":\"api-request") {
            continue;
        }
        if line.contains("\"type\":\"tool_use\"") {
            // The tool's name rides the same record as its id; take the line's first
            // `name` field as the best-effort label for every use on the line.
            let name = field_values(line, "name").next().unwrap_or("").to_string();
            for id in field_values(line, "id").filter(|v| v.starts_with("toolu")) {
                note_use(&mut uses, id.to_string(), &name);
            }
        }
        if line.contains("\"tool_use_id\"") {
            results.extend(field_values(line, "tool_use_id").map(str::to_string));
        }
        let Some(kind) = codex_payload_type(line) else {
            continue;
        };
        for pat in CODEX_TOOL_PATTERNS {
            if kind == pat.call {
                if let Some(id) = call_identity(line, pat.id_keys) {
                    // Codex names function calls; the specialized shapes fall back to
                    // their call type, which is the honest label they have.
                    let name = field_values(line, "name").next().unwrap_or(pat.call);
                    note_use(&mut uses, id, name);
                }
            } else if kind == pat.result {
                results.extend(call_identity(line, pat.id_keys));
            }
        }
    }
    uses.retain(|(id, _)| !results.contains(id));
    uses.into_iter()
        .map(|(id, name)| InflightTool { id, name })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cr-liveness-{}-{}", name, std::process::id()))
    }

    fn write_tail(path: &std::path::Path, lines: &str) {
        std::fs::write(path, lines).unwrap();
    }

    /// #194: the upgraded scan names what is pending — the S2 carrier the state
    /// derivation joins against `tool_is_interactive`.
    #[test]
    fn inflight_tools_carry_names_in_call_order() {
        let p = fixture("names");
        write_tail(
            &p,
            concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_a","name":"Bash","input":{}}]}}"#,
                "
",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_b","name":"AskUserQuestion","input":{}}]}}"#,
                "
",
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"toolu_a","content":"ok"}]}}"#,
                "
",
            ),
        );
        let pending = inflight_tools_in_tail(&p);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, "toolu_b");
        assert_eq!(pending[0].name, "AskUserQuestion");
    }

    #[test]
    fn claude_tool_use_without_result_is_inflight() {
        let p = fixture("claude-inflight");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_use","id":"toolu_01","name":"bash","input":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn claude_tool_use_with_result_is_not_inflight() {
        let p = fixture("claude-done");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_use","id":"toolu_02","name":"bash","input":{}}}
{"type":"response_item","payload":{"type":"tool_result","tool_use_id":"toolu_02","content":"ok"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    /// #s329: the client's copy of a call in its API-call log is not the call. Here the window's
    /// edge has cut away the call's own record and its result, leaving the blob written after
    /// them; a call made after it, with no result, is still in flight.
    #[test]
    fn a_request_log_copy_of_a_call_is_not_inflight() {
        let p = fixture("claude-blob");
        write_tail(
            &p,
            concat!(
                r#"{"type":"api-request-blob","sessionId":"s","hash":"h1","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_old","name":"Bash","input":{}}]}}"#,
                "\n",
                r#"{"type":"api-request","sessionId":"s","id":"q1","keep":2,"tail":["h1"]}"#,
                "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_new","name":"Read","input":{}}]}}"#,
                "\n",
            ),
        );
        let pending = inflight_tools_in_tail(&p);
        assert_eq!(
            pending,
            vec![InflightTool {
                id: "toolu_new".into(),
                name: "Read".into()
            }]
        );
    }

    #[test]
    fn codex_function_call_without_output_is_inflight() {
        let p = fixture("codex-func");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-1","name":"bash","arguments":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_function_call_with_output_is_not_inflight() {
        let p = fixture("codex-func-done");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-2","name":"bash","arguments":{}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"fc-2","output":"ok"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_custom_tool_call_is_tracked() {
        let p = fixture("codex-custom");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"ct-1","name":"Read","arguments":{}}}
{"type":"response_item","payload":{"type":"custom_tool_call_output","call_id":"ct-1","output":"hi"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_tool_search_call_is_closed_by_its_call_id_not_its_own_id() {
        // Shape taken verbatim from a real rollout: the call carries BOTH `id` ("tsc_…") and
        // `call_id`, the output repeats only the `call_id` and has its own `id` ("tso_…").
        // Pairing on every id key would leave "tsc_…" dangling for the rest of the session.
        let p = fixture("codex-search");
        write_tail(
            &p,
            r#"{"timestamp":"2026-07-30T07:59:07.943Z","type":"response_item","payload":{"type":"tool_search_call","id":"tsc_057945","call_id":"call_Pfd96J","status":"completed","execution":"client","arguments":{"query":"board api"}}}
{"timestamp":"2026-07-30T07:59:08.001Z","type":"response_item","payload":{"type":"tool_search_output","id":"tso_019fb208","call_id":"call_Pfd96J","status":"completed","execution":"client","tools":[]}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_tool_search_call_dangling_is_inflight() {
        let p = fixture("codex-search-open");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"tool_search_call","id":"tsc_open","call_id":"call_open","status":"completed","arguments":{}}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn codex_self_contained_records_are_never_inflight() {
        // `web_search_call` and `image_generation_call` carry their own results and have no
        // output record to wait for, so they must not enter the dangling set at all —
        // otherwise every session that ever searched the web would read as busy forever.
        let p = fixture("codex-self-contained");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"web_search_call","id":"ws_1","status":"completed","action":{"type":"search","query":"rust"}}}
{"type":"response_item","payload":{"type":"image_generation_call","id":"ig_1","revised_prompt":"crab","result":null}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn a_tool_output_quoting_a_call_record_still_closes_it() {
        // Transcripts of work ON this repo embed transcript text in tool output. The record's
        // kind therefore has to come from its own `payload` envelope, not from a substring
        // search that the quoted text can satisfy.
        let p = fixture("codex-quoted");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"fc-3","name":"bash","arguments":{}}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"fc-3","output":"grep hit: {\"type\":\"function_call\",\"call_id\":\"fc-9\"}"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn only_unmatched_calls_count_as_inflight() {
        let p = fixture("codex-mixed");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"done-1","name":"a"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"done-1","output":"ok"}}
{"type":"response_item","payload":{"type":"function_call","call_id":"open-1","name":"b"}}
"#,
        );
        assert!(inflight_tool_in_tail(&p));
    }

    #[test]
    fn empty_call_ids_are_ignored() {
        let p = fixture("codex-empty-id");
        write_tail(
            &p,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"","name":"a"}}
"#,
        );
        assert!(!inflight_tool_in_tail(&p));
    }

    #[test]
    fn missing_file_is_not_inflight() {
        let p = fixture("missing");
        assert!(!inflight_tool_in_tail(&p));
    }

    /// The window opens at a FIXED byte offset, so on any non-ASCII transcript it can open in
    /// the middle of a character — the first bytes are then an invalid UTF-8 fragment. Decoding
    /// strictly used to throw the whole tail away and report "idle", hiding a live tool call;
    /// measured on 7 of 558 real rollouts. The split character must cost nothing but itself.
    #[test]
    fn a_window_opening_mid_character_still_sees_the_dangling_call() {
        let p = fixture("split-char");
        let call = br#"{"type":"response_item","payload":{"type":"custom_tool_call","call_id":"ct-split","name":"Read"}}
"#;
        let mut bytes = br#"{"text":""#.to_vec();
        bytes.extend_from_slice("语".as_bytes());
        // Sized so the window opens on the character's last byte.
        let rest = br#"","cwd":"/x"}
"#;
        let keep = INFLIGHT_TAIL_BYTES as usize - rest.len() - call.len();
        bytes.extend_from_slice(&vec![b'x'; keep - 1]);
        bytes.extend_from_slice(rest);
        bytes.extend_from_slice(call);
        std::fs::write(&p, &bytes).unwrap();
        let edge = bytes.len() - INFLIGHT_TAIL_BYTES as usize;
        assert_eq!(
            bytes[edge] & 0xC0,
            0x80,
            "the edge must open inside the character"
        );
        assert_eq!(
            inflight_tools_in_tail(&p),
            vec![InflightTool {
                id: "ct-split".into(),
                name: "Read".into()
            }]
        );
    }

    /// A blob padded so its head lies before the window and its ids inside it, then `after`, then
    /// filler text to bring the window's edge inside the blob's padding.
    fn cut_blob(after: &str) -> String {
        let head = format!(
            r#"{{"type":"api-request-blob","sessionId":"s","hash":"h1","message":{{"role":"user","content":[{{"type":"text","text":"{}"}},"#,
            "x".repeat(1_000)
        );
        let ids = r#"{"type":"tool_use","id":"toolu_cut","name":"Bash","input":{}}]}}"#;
        let filler = r#"{"type":"user","message":{"content":"FILL"}}"#;
        let room = INFLIGHT_TAIL_BYTES as usize - 500 - ids.len() - after.len() - filler.len();
        let filler = filler.replace("FILL", &"y".repeat(room));
        let text = format!("{head}{ids}\n{filler}\n{after}");
        let edge = text.len() - INFLIGHT_TAIL_BYTES as usize;
        assert!(
            edge > 0 && edge < head.len(),
            "the edge must fall in the blob"
        );
        assert!(
            text.find("toolu_cut").unwrap() > edge,
            "the ids must lie inside"
        );
        text
    }

    /// #s365: where the window's edge cuts a blob, its head is out of sight, so the blob filter
    /// cannot know it. The cut piece's ids are no calls; a real one after it still is.
    #[test]
    fn a_call_on_the_line_the_window_cuts_is_not_inflight() {
        let p = fixture("cut-blob");
        write_tail(&p, &cut_blob(""));
        assert_eq!(inflight_tools_in_tail(&p), vec![]);
        write_tail(
            &p,
            &cut_blob(concat!(
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_live","name":"Read","input":{}}]}}"#,
                "\n",
            )),
        );
        assert_eq!(
            inflight_tools_in_tail(&p),
            vec![InflightTool {
                id: "toolu_live".into(),
                name: "Read".into()
            }]
        );
    }

    /// The first line goes only when it is cut: a window opening exactly on a line's start keeps
    /// the line, and one byte further on it does not.
    #[test]
    fn a_window_opening_on_a_line_s_start_keeps_that_line() {
        let call = concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_edge","name":"Bash","input":{}}]}}"#,
            "\n",
        );
        let filler = format!(
            "{}\n",
            "z".repeat(INFLIGHT_TAIL_BYTES as usize - call.len() - 1)
        );
        let p = fixture("line-start");
        write_tail(&p, &format!("before\n{call}{filler}"));
        assert!(inflight_tool_in_tail(&p), "a whole first line is read");
        write_tail(&p, &format!("before\n{call}{filler}z"));
        assert!(!inflight_tool_in_tail(&p), "a cut one is not");
    }

    /// One record longer than the window leaves no whole line to read.
    #[test]
    fn a_window_inside_one_line_reads_nothing() {
        let p = fixture("one-line");
        let tail = r#"{"type":"tool_use","id":"toolu_tail","name":"Bash","input":{}}"#;
        write_tail(
            &p,
            &format!("{}{tail}", "x".repeat(INFLIGHT_TAIL_BYTES as usize)),
        );
        assert_eq!(inflight_tools_in_tail(&p), vec![]);
    }
}

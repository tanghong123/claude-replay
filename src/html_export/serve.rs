//! The `--html` live server: a loopback HTTP server + the per-agent live tailer.
//! Renders via `super`'s block/stream helpers; residency is delegated to core's
//! `SessionStore`. Split out so the HTTP/tailer machinery doesn't share a namespace
//! with the markdown/JSON renderer.

use super::{
    block_lines, build_shell, child_info, display_title, render_agent_stream, render_snapshot,
    session_id, AgentInfo, ChildRef, POLL_MS,
};
use crate::fold::FoldPolicy;
use crate::{discover, Agent, Args};
use anyhow::{Context, Result};
use serde_json::json;
use std::path::Path;

/// How long an agent keeps being tailed after its last request before it goes idle and is
/// dropped (its stream file stays on disk; a later request revives it).
const TAIL_TTL_MS: u128 = 30_000;

/// The live server's shared state. Only *requested* agents become resident and get folded
/// each cycle — the rest cost nothing (tier (c) in the store), which is the CPU fix vs
/// re-parsing the whole tree. The session bookkeeping (the id→source registry + the resident
/// follower set + idle reaping) lives in [`SessionStore`](crate::engine::store::SessionStore);
/// `Live` layers the HTML rendering, the materialized `<id>.jsonl` (tier (b)), and the
/// `/stream` byte cursor over it.
struct Live {
    dir: std::path::PathBuf,
    agent: Agent,
    fold: FoldPolicy,
    root_path: std::path::PathBuf,
    cwd: String,
    store: crate::engine::store::SessionStore<AgentInfo, Tailer>,
}

/// A resident agent's live payload: the block lines last written (the diff baseline for the
/// next delta) and the incremental follower (M16) — a persistent `Replayer` that folds only
/// the newly-appended lines each cycle. Its `poll` returning `None` when the source hasn't
/// grown IS the skip-if-unchanged (no whole-file re-parse). (Its idle clock lives in the
/// store, which owns residency.)
struct Tailer {
    prev: Vec<String>,
    follower: crate::follow::FollowParser,
}

impl Live {
    /// Ensure `<id>.jsonl` exists (generate it from the agent's own source on first
    /// request), register its children, and mark it recently-seen so the background tailer
    /// keeps it current. Cheap on the hot path (an already-tailing id just bumps its clock).
    /// Returns false for an unknown id (not in the registry).
    fn ensure_stream(&self, id: &str) -> bool {
        if self.store.see(id) {
            return true; // already resident (tier (a)) — just bumped its clock
        }
        // Tier-(c) lookup: the registry (populated from spawn events). Fall back to
        // resolving the source directly — every agent shares the flat `subagents/` dir, so
        // a valid id resolves even if its parent was never navigated (deep links) — with a
        // plain title until its parent's spawn supplies the description.
        let info = self.store.resolve(id).or_else(|| {
            discover::subagent_source(self.agent, &self.root_path, id).map(|source| AgentInfo {
                id: id.to_string(),
                source,
                title: id.to_string(),
                agent_type: String::new(),
                ancestors: Vec::new(), // unknown ancestry for an un-navigated deep link
            })
        });
        let Some(info) = info else {
            return false;
        };
        if !info.source.exists() {
            return false;
        }
        // Initial generation via the incremental follower's first poll (folds the whole
        // source once; subsequent polls in `run_tailer` fold only appended deltas).
        let mut follower = crate::follow::FollowParser::open(self.agent, &info.source);
        let (blocks, times, metrics) = match follower.poll() {
            Ok(Some(t)) => t,
            Ok(None) => (Vec::new(), Vec::new(), crate::metrics::Metrics::default()),
            Err(_) => return false,
        };
        let (jsonl, children) = render_agent_stream(
            self.agent, &self.fold, &self.cwd, true, &info, &blocks, &times, &metrics, None,
        );
        let _ = std::fs::write(self.dir.join(format!("{id}.jsonl")), format!("{jsonl}\n"));
        self.register_children(&info, children);
        // Promote to tier (a): resident with its follower + diff baseline.
        self.store.admit(
            id,
            Tailer {
                prev: block_lines(&jsonl),
                follower,
            },
        );
        true
    }

    /// Register `parent`'s discovered children so their `?session=` links resolve to a
    /// source later — carrying the ancestry (parent's + parent) for their breadcrumb.
    fn register_children(&self, parent: &AgentInfo, children: Vec<ChildRef>) {
        for c in children {
            if self.store.is_registered(&c.id) {
                continue;
            }
            if let Some(ci) = child_info(self.agent, &self.root_path, parent, c) {
                let id = ci.id.clone();
                self.store.register_new(&id, ci);
            }
        }
    }

    /// Serve `<id>.jsonl` bytes from byte offset `from` (clamped past-EOF → empty),
    /// truncated to the last newline so the client's cursor always lands on a line
    /// boundary — never mid-record, even if a tailer append is in flight.
    /// `(start, bytes)` — the served chunk AND the **absolute byte offset it begins at**
    /// (the requested `from` clamped to EOF). The client uses `start` to place the chunk
    /// idempotently: it discards any prefix it already has and sets its cursor to
    /// `start + bytes.len()`, so a re-fetch or a past-EOF request can't desync it.
    fn stream_bytes(
        &self,
        id: &str,
        from: crate::model::ByteOffset,
    ) -> (crate::model::ByteOffset, Vec<u8>) {
        match std::fs::read(self.dir.join(format!("{id}.jsonl"))) {
            Ok(bytes) => {
                let start = (from as usize).min(bytes.len());
                (
                    start as u64,
                    line_aligned_tail(&bytes, from as usize).to_vec(),
                )
            }
            Err(_) => (from, Vec::new()),
        }
    }

    /// The single background tailer thread: each cycle, re-parse ONLY the agents requested
    /// within the TTL (usually just the one on screen), diff, and append their deltas. Idle
    /// agents are dropped. No whole-tree re-parse — this is the CPU fix.
    fn run_tailer(self: std::sync::Arc<Self>) {
        loop {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            self.store.reap(TAIL_TTL_MS); // drop residents idle past the TTL → tier (c)
            for id in self.store.resident_ids() {
                let Some(info) = self.store.resolve(&id) else {
                    continue;
                };
                // Fold ONLY the newly-appended lines through this agent's persistent follower
                // (its `poll` returns `None` when the source hasn't grown — the skip that
                // turns a constant re-parse of a huge transcript into O(delta) work). The
                // poll runs under the residents lock only for the brief delta read.
                let polled = self
                    .store
                    .with_resident(&id, |tl| tl.follower.poll().ok().flatten())
                    .flatten();
                let Some((blocks, times, metrics)) = polled else {
                    continue; // reaped since enumeration, or nothing new this cycle
                };
                let (jsonl, children) = render_agent_stream(
                    self.agent, &self.fold, &self.cwd, true, &info, &blocks, &times, &metrics, None,
                );
                self.register_children(&info, children);
                let fresh = block_lines(&jsonl);
                let meta = jsonl.lines().next().unwrap_or("{}");
                self.store.with_resident(&id, |tl| {
                    if let Some(delta) = stream_delta(&tl.prev, &fresh, meta) {
                        let _ =
                            append_line(&self.dir.join(format!("{id}.jsonl")), delta.trim_end());
                        tl.prev = fresh;
                    }
                });
            }
        }
    }
}

/// The append chunk to bring a stream from `prev` block lines to `fresh`: a
/// `{t:"reset",from:N}` when an already-rendered block changed/vanished, the new tail,
/// and the refreshed `meta`. `None` when nothing changed (a pure no-op cycle). Mirrors
/// the single-file [`follow_and_append`] diff, per agent.
pub(super) fn stream_delta(prev: &[String], fresh: &[String], meta: &str) -> Option<String> {
    let diff = prev.iter().zip(fresh).take_while(|(a, b)| a == b).count();
    if diff >= prev.len() && diff >= fresh.len() {
        return None; // unchanged
    }
    let mut out = String::new();
    if diff < prev.len() {
        out.push_str(&json!({ "t": "reset", "from": diff }).to_string());
        out.push('\n');
    }
    for l in &fresh[diff..] {
        out.push_str(l);
        out.push('\n');
    }
    out.push_str(meta);
    out.push('\n');
    Some(out)
}

/// Poll the transcript forever, streaming changes to `companion`. Shared by
/// `--dump-html -f` and `--html -f`; returns only on error (the caller runs until
/// Ctrl-C). `prev` is the block lines already on the page (excluding the meta).
///
/// The tail of a live transcript is **rewritten**, not just appended to: a
/// thinking block finalizes, a tool result lands, an activity group coalesces. So
/// each cycle we diff the fresh block lines against `prev` and find the first that
/// differs. Blocks before it are stable → left alone (the common case is a pure
/// append: no divergence, just new lines). From the first divergence we emit a
/// `{"t":"reset","from":N}` record (the page drops its rendered blocks ≥ N) then
/// re-emit the fresh tail — so a rewritten/ coalesced tail re-renders correctly,
/// matching the TUI's full re-parse. `reveal` must match the initial snapshot.
pub(super) fn follow_and_append(
    agent: Agent,
    path: &Path,
    fold: &FoldPolicy,
    companion: &Path,
    mut prev: Vec<String>,
    reveal: bool,
) -> Result<()> {
    // Incremental follower (M16): fold only the newly-appended lines each poll instead of
    // re-parsing the whole file. `open` starts at byte 0, so the first poll folds the file
    // to the current state (== the initial export → no diff), then only deltas thereafter.
    let mut follower = crate::follow::FollowParser::open(agent, path);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
        let polled = match follower.poll() {
            Ok(p) => p,
            Err(_) => continue, // transient read error mid-write; retry next cycle
        };
        let Some((snap_blocks, times, metrics)) = polled else {
            continue; // nothing new this cycle
        };
        let cwd = crate::discover::session_cwd(path)
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let (fresh, _) = render_snapshot(
            agent,
            path,
            &snap_blocks,
            &times,
            &metrics,
            &cwd,
            fold,
            reveal,
        );
        let meta = fresh.lines().next().unwrap_or("{}").to_string();
        let blocks = block_lines(&fresh);
        // First index where the fresh stream diverges from what's on the page.
        let diff = prev.iter().zip(&blocks).take_while(|(a, b)| a == b).count();
        let changed = diff < prev.len() || diff < blocks.len();
        if !changed {
            continue;
        }
        let mut out = String::new();
        // Only when an already-rendered block changed/vanished — a pure append
        // (diff == prev.len()) needs no reset, keeping the common path append-only.
        if diff < prev.len() {
            out.push_str(&json!({ "t": "reset", "from": diff }).to_string());
            out.push('\n');
        }
        for line in &blocks[diff..] {
            out.push_str(line);
            out.push('\n');
        }
        // Refreshed meta so usage / cost / duration / tool counts keep up.
        out.push_str(&meta);
        out.push('\n');
        append_line(companion, out.trim_end())?;
        prev = blocks;
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

/// `--html`: render to HTML and open it in the browser instead of the TUI, as a
/// **multi-file bundle** — one shared shell + one `<id>.jsonl` per agent — so sub-agent
/// drill-down works (clicking an agent navigates to its own stream). Serves over a tiny
/// **loopback HTTP server** (not `file://`) so a path click can reveal the file in Finder
/// (`/__reveal`) and the page can `fetch` its streams. `-f` live-tails the whole tree,
/// keeping every agent's stream current (new spawns appear, children grow); without it
/// the bundle is a static snapshot.
pub fn serve(args: &Args, path: &Path) -> Result<()> {
    use std::sync::Arc;
    let agent = discover::detect_agent(path);
    let fold = FoldPolicy::from_args(args);
    let sid = session_id(path);
    let cwd = discover::session_cwd(path)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let title = display_title(agent, path);

    // A private temp dir holds the bundle (shell + per-agent streams). Fresh per run —
    // wipe any streams left by a previous run of this session so lazy materialization
    // starts clean (only the root exists until a child is requested).
    let dir = std::env::temp_dir().join("claude-replay").join(&sid);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    // The shared live state. The registry starts with just the root; children are
    // discovered + registered lazily as their parents' streams are generated. Streams are
    // generated ONLY on first request (`/stream?session=<id>`), and only *requested* agents
    // are re-parsed by the background tailer — so opening a huge tree costs one parse.
    let live = Arc::new(Live {
        dir: dir.clone(),
        agent,
        fold,
        root_path: path.to_path_buf(),
        cwd,
        store: crate::engine::store::SessionStore::new(),
    });
    live.store.register(
        &sid,
        AgentInfo {
            id: sid.clone(),
            source: path.to_path_buf(),
            title: title.clone(),
            agent_type: String::new(),
            ancestors: Vec::new(),
        },
    );
    // The shell + the root stream up-front so the first page load is instant.
    std::fs::write(
        dir.join("index.html"),
        build_shell(&title, &sid, args.follow),
    )
    .with_context(|| "write index.html")?;
    live.ensure_stream(&sid);

    let port = spawn_http_server(dir.clone(), Some(live.clone()))?;
    let url = format!("http://127.0.0.1:{port}/index.html?session={sid}");
    let kind = if args.follow { "live" } else { "static" };
    eprintln!(
        "serving {} at {url} ({kind} — Ctrl-C to stop)",
        dir.display()
    );
    eprintln!("  open in a browser, or copy the URL above");
    open_in_browser(&url);
    println!("{url}");

    if args.follow {
        live.run_tailer(); // background-tail the requested agents; runs until Ctrl-C
        Ok(())
    } else {
        // Static: no tailing, but keep serving so navigation + reveal keep working. Streams
        // are still generated lazily on first request (children on demand).
        loop {
            std::thread::park();
        }
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
fn spawn_http_server(root: std::path::PathBuf, live: Option<std::sync::Arc<Live>>) -> Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").context("bind loopback HTTP server")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let root = root.clone();
            let live = live.clone();
            std::thread::spawn(move || {
                let _ = serve_connection(stream, &root, live.as_deref());
            });
        }
    });
    Ok(port)
}

/// The bytes of `data` from byte offset `from` (clamped past-EOF → empty), truncated to
/// the last newline so a served chunk never ends mid-record — the client's byte cursor
/// stays line-aligned even if a tailer append is in flight.
pub(super) fn line_aligned_tail(data: &[u8], from: usize) -> &[u8] {
    let slice = &data[from.min(data.len())..];
    match slice.iter().rposition(|&b| b == b'\n') {
        Some(nl) => &slice[..=nl],
        None => &[], // no complete line past the cursor yet
    }
}

/// Parse a `k=v&…` query string value for `key` (already past the `?`).
pub(super) fn query_get<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Decode a `%XX`-percent-encoded string (the reveal path arrives via
/// `encodeURIComponent`). Unknown/short escapes are passed through literally.
pub(super) fn percent_decode(s: &str) -> String {
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

fn serve_connection(
    mut stream: std::net::TcpStream,
    root: &Path,
    live: Option<&Live>,
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    // `GET /name?query HTTP/1.1`
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path_part, query) = target.split_once('?').unwrap_or((target, ""));
    let name = path_part.trim_start_matches('/');
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
    // `/stream?session=<id>&from=<byte>` — the live feed. Generate the agent's stream on
    // first request (lazy), keep it tailed, and serve ONLY the bytes past the client's
    // cursor (so a poll transfers just the new delta, not the whole transcript).
    if name == "stream" {
        let Some(live) = live else {
            return respond(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"no live server",
            );
        };
        let id = query_get(query, "session").unwrap_or("");
        let from: u64 = query_get(query, "from")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if id.is_empty() || id.contains('/') || id.contains("..") || !live.ensure_stream(id) {
            return respond(&mut stream, "404 Not Found", "text/plain", b"no such agent");
        }
        // Include the delta's ABSOLUTE start offset so the client can place it
        // idempotently (discard overlap, snap its cursor to `start + len`).
        let (start, bytes) = live.stream_bytes(id, from);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\n\
             X-Offset: {start}\r\nContent-Length: {}\r\nAccess-Control-Expose-Headers: X-Offset\r\n\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            bytes.len()
        );
        return stream
            .write_all(head.as_bytes())
            .and_then(|_| stream.write_all(&bytes));
    }
    // `/__reveal?path=<url-encoded abs path>` — reveal a file in the OS file manager (the
    // served page can't follow a `file://` link: browsers block http→file navigation).
    if name == "__reveal" {
        if let Some(v) = query_get(query, "path") {
            let p = percent_decode(v);
            let path = Path::new(&p);
            if path.exists() {
                crate::tui::app::reveal_in_file_manager(path);
                return respond(&mut stream, "200 OK", "text/plain", b"revealed");
            }
        }
        return respond(&mut stream, "404 Not Found", "text/plain", b"no such path");
    }
    // Static files: the shell, per-agent `<id>.jsonl` (static bundle), and `assets/<file>`.
    // Allow a single `assets/` subdir; block any other traversal.
    let allowed = !name.is_empty()
        && !name.contains("..")
        && (!name.contains('/')
            || name
                .strip_prefix("assets/")
                .is_some_and(|r| !r.contains('/')));
    if !allowed {
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

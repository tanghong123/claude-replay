//! The `--html` live server: a loopback HTTP server + the per-agent live tailer.
//! Renders via `super`'s block/stream helpers; the session domain (the id→source registry, the
//! resident incremental followers, and the materialized `Session`s) is owned by core's
//! [`SessionCache`](crate::SessionCache) — the server keeps only *presentation* state (the
//! rendered-line diff baseline + titles). Split out so the HTTP/tailer machinery doesn't share
//! a namespace with the markdown/JSON renderer.

use super::{
    block_lines, build_shell, child_info, display_title, render_agent_stream, render_snapshot,
    session_id, AgentInfo, ChildRef, POLL_MS,
};
use crate::cache::{pull, pull_indices, Cursor, RenderSnapshot, SharedSession};
use crate::fold::FoldPolicy;
use crate::{discover, Agent, Args, SessionCache, Transcript};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// How long an agent keeps being tailed after its last request before it goes idle and is
/// dropped (its stream file stays on disk; a later request revives it).
const TAIL_TTL_MS: u128 = 30_000;

/// The live server's shared state. Only *requested* agents become resident and get folded
/// each cycle — the rest cost nothing (tier (c) in the cache), which is the CPU fix vs
/// re-parsing the whole tree. The session domain (the id→source registry + the resident
/// incremental followers + the materialized `Session`s + idle reaping) is owned by
/// [`SessionCache`](crate::SessionCache); `Live` keeps only the *presentation* state — the
/// per-agent titles, the rendered-line diff baseline (`prev`), the materialized `<id>.jsonl`
/// (tier (b)), and the `/stream` byte cursor — layered over it.
struct Live {
    dir: std::path::PathBuf,
    agent: Agent,
    fold: FoldPolicy,
    root_path: std::path::PathBuf,
    cwd: String,
    /// The session domain: id→source registry + resident followers + TTL reaping.
    cache: SessionCache,
    /// Presentation state, keyed by agent id. `prev`: the block lines last written (the diff
    /// baseline for the next delta); its presence also marks an agent as materialized (its
    /// `<id>.jsonl` exists). `titles`: the non-source half of the old descriptor.
    prev: Mutex<HashMap<String, Vec<String>>>,
    titles: Mutex<HashMap<String, TitleInfo>>,
    /// The pull-client path's per-id live state (`/pull`), materialized on first pull and lazily
    /// TTL-reaped (no background thread — folding happens on the pull request's own thread). Empty
    /// and untouched unless a client uses `/pull`, so the default `/stream` path is unaffected.
    shared: Mutex<HashMap<String, (std::time::Instant, std::sync::Arc<SharedSession>)>>,
}

/// The presentation half of an agent's descriptor — everything `render_agent_stream` needs for
/// the stream meta except the source (which the cache owns) and the id (the map key).
#[derive(Clone, Default)]
struct TitleInfo {
    title: String,
    agent_type: String,
    /// The ancestry from the root down to this agent's parent — `(id, title)` each — for the
    /// breadcrumb. Empty for the root.
    ancestors: Vec<(String, String)>,
}

impl Live {
    /// Reconstruct the `AgentInfo` that `render_agent_stream` / `child_info` expect from the
    /// split state: the source (owned by the cache) + the title (owned by `titles`) + the id.
    fn agent_info(&self, id: &str, source: std::path::PathBuf, t: &TitleInfo) -> AgentInfo {
        AgentInfo {
            id: id.to_string(),
            source,
            title: t.title.clone(),
            agent_type: t.agent_type.clone(),
            ancestors: t.ancestors.clone(),
        }
    }

    /// Resolve `id` to its source + title. Tier-(c) lookup first (the cache registry, populated
    /// from spawn events); else resolve the source directly — every agent shares the flat
    /// `subagents/` dir, so a valid id resolves even if its parent was never navigated (deep links)
    /// — with a plain title until its parent's spawn supplies the description, registering the
    /// fallback into the cache/titles so later lookups find it. `None` for an unknown id. Shared by
    /// [`ensure_stream`](Self::ensure_stream) (the `/stream` path) and
    /// [`pull_response`](Self::pull_response) (the `/pull` path).
    fn resolve_id(&self, id: &str) -> Option<(Transcript, TitleInfo)> {
        if let Some(src) = self.cache.resolve(id) {
            let t = self
                .titles
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .unwrap_or_default();
            return Some((src, t));
        }
        let source = discover::subagent_source(self.agent, &self.root_path, id)?;
        if !source.exists() {
            return None;
        }
        let src = Transcript::open(self.agent, source);
        let t = TitleInfo {
            title: id.to_string(),
            ..Default::default() // unknown ancestry/type for an un-navigated deep link
        };
        self.cache.register(id, src.clone());
        self.titles
            .lock()
            .unwrap()
            .insert(id.to_string(), t.clone());
        Some((src, t))
    }

    /// Ensure `<id>.jsonl` exists (generate it from the agent's own source on first request)
    /// and register its children. Cheap on the hot path (an already-materialized id short-
    /// circuits; the background tailer keeps it current). Returns false for an unknown id.
    fn ensure_stream(&self, id: &str) -> bool {
        if self.prev.lock().unwrap().contains_key(id) {
            return true; // already materialized — the tailer keeps its stream current
        }
        let Some((src, title)) = self.resolve_id(id) else {
            return false;
        };
        if !src.path().exists() {
            return false;
        }
        // Initial materialization via the cache's first poll (folds the whole source once;
        // subsequent polls in `run_tailer` fold only appended deltas). `None` == an empty
        // source; a read error drops the request.
        let session = match self.cache.poll(id) {
            Some(Ok(s)) => Some(s),
            Some(Err(_)) => return false,
            None => None,
        };
        let info = self.agent_info(id, src.path().to_path_buf(), &title);
        let empty_metrics = crate::metrics::Metrics::default();
        let blocks_owned = session.as_ref().map(|s| s.blocks());
        let (blocks, times, metrics) = match &session {
            Some(s) => (
                blocks_owned.as_deref().unwrap_or(&[]),
                s.user_times.as_slice(),
                &s.metrics,
            ),
            None => (&[][..], &[][..], &empty_metrics),
        };
        let (jsonl, children) = render_agent_stream(
            self.agent, &self.fold, &self.cwd, true, &info, blocks, times, metrics, None,
        );
        let _ = std::fs::write(self.dir.join(format!("{id}.jsonl")), format!("{jsonl}\n"));
        self.register_children(&info, children);
        // Record the diff baseline (also marks the id materialized for the fast path above).
        self.prev
            .lock()
            .unwrap()
            .insert(id.to_string(), block_lines(&jsonl));
        true
    }

    /// The `/pull` handler: serve the pull-client wire reply for `id` at `cursor`. Materialize the
    /// id's [`SharedSession`] on first pull (lazily TTL-reaped — no background thread, so a session
    /// nobody is pulling costs nothing), **borrow this request's thread to tail** it (fold any new
    /// source lines), render the current blocks, and slice by the cursor via [`pull_wire`]. `None`
    /// for an unknown/unreadable id. Independent of the `/stream` path (its own `shared` map).
    fn pull_response(&self, id: &str, cursor: Cursor) -> Option<String> {
        let (src, title) = self.resolve_id(id)?;
        if !src.path().exists() {
            return None;
        }
        let shared = {
            let mut map = self.shared.lock().unwrap();
            // Lazy reap (this path owns no background thread): drop sessions no one has pulled
            // within the TTL. A later pull re-materializes from the source.
            map.retain(|_, (t, _)| t.elapsed().as_millis() < TAIL_TTL_MS);
            let entry = map.entry(id.to_string()).or_insert_with(|| {
                (
                    std::time::Instant::now(),
                    std::sync::Arc::new(SharedSession::open(self.agent, src.path())),
                )
            });
            entry.0 = std::time::Instant::now();
            entry.1.clone()
        };
        // Borrow-to-tail: fold newly-appended source lines on this request's own thread.
        let _ = shared.advance();
        // Idle fast-path: decide from the counters alone (no block clone/render) whether this
        // cursor has anything to receive. The baseline `/stream` tailer skips all work on an idle
        // session; without this, an attached client polling a large quiet session would pay an
        // O(N) clone + render every tick — the opposite of the pull version's whole point.
        let (epoch, gen, nc, np) = shared.counters();
        let (cf, pf) = pull_indices(epoch, nc, np, gen, cursor);
        if cursor.epoch == epoch && cf == nc && pf == np {
            // Nothing to send: empty zones, no `meta` (the client ignores an idle reply's meta).
            return Some(
                json!({
                    "t": "pull", "epoch": epoch,
                    "committed_from": cf, "committed": [],
                    "provisional_gen": gen,
                    "provisional_from": pf, "provisional": [],
                    "meta": Value::Null,
                })
                .to_string(),
            );
        }
        let snap = shared.render_snapshot();
        let info = self.agent_info(id, src.path().to_path_buf(), &title);
        let (jsonl, children) = render_agent_stream(
            self.agent,
            &self.fold,
            &self.cwd,
            true,
            &info,
            &snap.blocks,
            &snap.user_times,
            &snap.metrics,
            None,
        );
        self.register_children(&info, children);
        let lines = block_lines(&jsonl);
        let meta = jsonl.lines().next().unwrap_or("{}");
        Some(pull_wire(&snap, &lines, meta, cursor))
    }

    /// Register `parent`'s discovered children so their `?session=` links resolve to a source
    /// later — carrying the ancestry (parent's + parent) for their breadcrumb. Splits each
    /// child's `AgentInfo` into a cache source + a `titles` entry.
    fn register_children(&self, parent: &AgentInfo, children: Vec<ChildRef>) {
        for c in children {
            if self.cache.is_registered(&c.id) {
                continue;
            }
            if let Some(ci) = child_info(self.agent, &self.root_path, parent, c) {
                let id = ci.id.clone();
                let src = Transcript::open(self.agent, ci.source);
                let t = TitleInfo {
                    title: ci.title,
                    agent_type: ci.agent_type,
                    ancestors: ci.ancestors,
                };
                self.cache.register_new(&id, src);
                self.titles.lock().unwrap().entry(id).or_insert(t);
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
            self.cache.reap(TAIL_TTL_MS); // drop residents idle past the TTL → tier (c)
            for id in self.cache.resident_ids() {
                let Some(src) = self.cache.resolve(&id) else {
                    continue;
                };
                // Fold ONLY the newly-appended lines through this agent's persistent follower
                // (the cache's `poll` returns `None` when the source hasn't grown — the skip
                // that turns a constant re-parse of a huge transcript into O(delta) work). The
                // follower read runs under the cache's residents lock; rendering is out here.
                let session = match self.cache.poll(&id) {
                    Some(Ok(s)) => s,
                    _ => continue, // reaped since enumeration, unreadable, or nothing new
                };
                let title = self
                    .titles
                    .lock()
                    .unwrap()
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                let info = self.agent_info(&id, src.path().to_path_buf(), &title);
                let blocks = session.blocks();
                let (jsonl, children) = render_agent_stream(
                    self.agent,
                    &self.fold,
                    &self.cwd,
                    true,
                    &info,
                    &blocks,
                    &session.user_times,
                    &session.metrics,
                    None,
                );
                self.register_children(&info, children);
                let fresh = block_lines(&jsonl);
                let meta = jsonl.lines().next().unwrap_or("{}");
                let mut prev = self.prev.lock().unwrap();
                let baseline = prev.get(&id).map(Vec::as_slice).unwrap_or(&[]);
                if let Some(delta) = stream_delta(baseline, &fresh, meta) {
                    let _ = append_line(&self.dir.join(format!("{id}.jsonl")), delta.trim_end());
                    prev.insert(id, fresh);
                }
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

/// Build the `/pull` wire reply from a [`RenderSnapshot`] and the client's [`Cursor`] (the
/// pull-client transport, vs `stream_delta`'s server-diffed byte stream). Runs the **tested**
/// block-level [`pull`] index math over the snapshot's committed/provisional zones, then slices
/// the parallel rendered block `lines` (1:1 with the snapshot's blocks) at the resulting `*_from`
/// indices. `committed` blocks are permanent appends; `provisional` is a truncate-from + append.
/// The content-blind client applies "truncate to `from`, then extend" per zone (see `export.js`).
fn pull_wire(snap: &RenderSnapshot, lines: &[String], meta: &str, cursor: Cursor) -> String {
    // One rendered line per block — the slicing below relies on it. A future renderer that emits a
    // multi-line record would silently misplace blocks (we clamp rather than panic); fail loudly in
    // debug so that regression is caught, not shipped.
    debug_assert_eq!(
        lines.len(),
        snap.blocks.len(),
        "pull_wire expects one rendered line per block"
    );
    // The committed/provisional split in line-space (clamped to the rendered lines defensively;
    // in practice `lines.len() == blocks.len()` — one rendered line per block).
    let n = snap.n_committed.min(lines.len());
    let reply = pull(
        snap.epoch,
        &snap.blocks[..snap.n_committed],
        &snap.blocks[snap.n_committed..],
        snap.provisional_gen,
        cursor,
    );
    let to_vals = |ls: &[String]| -> Vec<Value> {
        ls.iter()
            .map(|l| serde_json::from_str::<Value>(l).unwrap_or(Value::Null))
            .collect()
    };
    let committed = to_vals(&lines[reply.committed_from.min(n)..n]);
    let provisional = to_vals(&lines[(n + reply.provisional_from).min(lines.len())..]);
    json!({
        "t": "pull",
        "epoch": snap.epoch,
        "committed_from": reply.committed_from,
        "committed": committed,
        "provisional_gen": snap.provisional_gen,
        "provisional_from": reply.provisional_from,
        "provisional": provisional,
        "meta": serde_json::from_str::<Value>(meta).unwrap_or(Value::Null),
    })
    .to_string()
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
        cache: SessionCache::new(),
        prev: Mutex::new(HashMap::new()),
        titles: Mutex::new(HashMap::new()),
        shared: Mutex::new(HashMap::new()),
    });
    live.cache
        .register(&sid, Transcript::open(agent, path.to_path_buf()));
    live.titles.lock().unwrap().insert(
        sid.clone(),
        TitleInfo {
            title: title.clone(),
            ..Default::default()
        },
    );
    // Transport: the pull-client feed (`/pull`) is the DEFAULT for a live server — it costs nothing
    // when no browser is attached (no background tailer; folding rides each client request). Setting
    // `CR_STREAM=1` reverts to the baseline `/stream` byte-diff + `run_tailer` (kept for comparison).
    let pull_mode = args.follow && std::env::var_os("CR_STREAM").is_none();

    // The shell up-front so the first page load is instant. In pull mode the page carries
    // `data-pull` and drives `/pull`; the baseline pre-materializes the root `/stream` file.
    std::fs::write(
        dir.join("index.html"),
        build_shell(&title, &sid, args.follow, pull_mode),
    )
    .with_context(|| "write index.html")?;
    if !pull_mode {
        live.ensure_stream(&sid); // baseline: pre-render the root stream (the tailer keeps it current)
    }

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

    if args.follow && !pull_mode {
        live.run_tailer(); // baseline `/stream`: background-tail the requested agents until Ctrl-C
        Ok(())
    } else {
        // Pull mode (client-driven — no background tailer, zero cost when idle) OR static: keep
        // serving so navigation + reveal keep working. Streams are folded on demand (per `/pull`
        // request, or lazily on first `/stream` request for a static bundle).
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
    // `/pull?session=<id>&cursor=<epoch.committed.gen.index>` — the pull-client feed. Materialize
    // the id on first pull, borrow this thread to tail it, and return the self-describing PullReply
    // JSON (committed append + provisional truncate/extend). Costs nothing when no client pulls.
    if name == "pull" {
        let Some(live) = live else {
            return respond(
                &mut stream,
                "404 Not Found",
                "text/plain",
                b"no live server",
            );
        };
        let id = query_get(query, "session").unwrap_or("");
        let cursor = Cursor::from_query(query_get(query, "cursor").unwrap_or(""));
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return respond(&mut stream, "404 Not Found", "text/plain", b"no such agent");
        }
        return match live.pull_response(id, cursor) {
            Some(body) => respond(
                &mut stream,
                "200 OK",
                "application/json; charset=utf-8",
                body.as_bytes(),
            ),
            None => respond(&mut stream, "404 Not Found", "text/plain", b"no such agent"),
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Block;

    fn snap(n_committed: usize, total: usize, epoch: u64, gen: u64) -> RenderSnapshot {
        RenderSnapshot {
            epoch,
            provisional_gen: gen,
            n_committed,
            blocks: (0..total)
                .map(|i| Block::AssistantText(format!("b{i}")))
                .collect(),
            user_times: Vec::new(),
            metrics: Default::default(),
        }
    }
    fn lines(total: usize) -> Vec<String> {
        (0..total).map(|i| format!("{{\"i\":{i}}}")).collect()
    }

    /// `pull_wire` maps the block-level pull onto the rendered lines: a fresh cursor resyncs
    /// (all lines), an up-to-date cursor is idle (empty zones), and a catching-up cursor gets the
    /// committed slice + a provisional reset.
    #[test]
    fn pull_wire_resync_idle_and_catchup() {
        // 2 committed + 2 provisional, epoch 5, gen 3.
        let s = snap(2, 4, 5, 3);
        let ls = lines(4);

        // Fresh (epoch-0) cursor ⇒ resync: all committed + all provisional.
        let v: Value = serde_json::from_str(&pull_wire(&s, &ls, "{}", Cursor::default())).unwrap();
        assert_eq!(v["epoch"], 5);
        assert_eq!(v["committed_from"], 0);
        assert_eq!(v["committed"].as_array().unwrap().len(), 2);
        assert_eq!(v["provisional_gen"], 3);
        assert_eq!(v["provisional_from"], 0);
        assert_eq!(v["provisional"].as_array().unwrap().len(), 2);

        // Up-to-date cursor (epoch 5, committed 2, gen 3, index 2) ⇒ idle.
        let v: Value =
            serde_json::from_str(&pull_wire(&s, &ls, "{}", Cursor::from_query("5.2.3.2"))).unwrap();
        assert_eq!(v["committed"].as_array().unwrap().len(), 0);
        assert_eq!(v["provisional"].as_array().unwrap().len(), 0);

        // Catching-up cursor with a stale gen ⇒ committed slice + whole provisional (from 0).
        let v: Value =
            serde_json::from_str(&pull_wire(&s, &ls, "{}", Cursor::from_query("5.0.1.0"))).unwrap();
        assert_eq!(v["committed_from"], 0);
        assert_eq!(v["committed"].as_array().unwrap().len(), 2);
        assert_eq!(v["provisional_from"], 0);
        assert_eq!(v["provisional"].as_array().unwrap().len(), 2);
    }
}

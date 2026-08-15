//! The `--html` live server: a loopback HTTP server + the per-agent live tailer.
//! Renders via `super`'s block/stream helpers; the session domain (the id→source registry, the
//! resident incremental followers, and the materialized `Session`s) is owned by core's
//! [`SessionCache`] — the server keeps only *presentation* state (the
//! rendered-line diff baseline + titles). Split out so the HTTP/tailer machinery doesn't share
//! a namespace with the markdown/JSON renderer.

use super::record_store::{HtmlNote, RecordStore};
use super::{
    assemble_meta, build_shell, build_shell_chrome, display_title, render_blocks, session_id,
    AgentInfo, PageChrome,
};
use crate::cache::{self, Presentation};
use crate::cache::{
    lock, pull_indices, Admission, Cursor, Denial, Holder, SharedSession, Unavailable,
};
use crate::engine::meta_stream::Versions;
use crate::fold::FoldPolicy;
use crate::{discover, Agent, Args, SessionCache, Transcript};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

/// How long an agent keeps being tailed after its last request before it goes idle and is
/// dropped (its stream file stays on disk; a later request revives it).
const TAIL_TTL_MS: u128 = 30_000;

/// The live server's shared state. Only *requested* agents become resident and get folded
/// each cycle — the rest cost nothing (tier (c) in the cache), which is the CPU fix vs
/// re-parsing the whole tree. The session domain (the id→source registry + the resident
/// incremental followers + the materialized `Session`s + idle reaping) is owned by
/// [`SessionCache`]; `Live` keeps only the *presentation* state — the
/// per-agent titles, parent pointers, and cached open-turn renders — layered over it.
pub struct SessionService {
    fold: FoldPolicy,
    /// Every ROOT this server hosts. Usually one (`--html <session>`); the `-f --html`
    /// picker registers every discovered session so they are all live at once, each
    /// reachable at `?session=<id>` on this one server. Roots may span agents AND
    /// working directories, so agent/cwd are per-session, never server-wide.
    roots: std::sync::Mutex<Vec<Root>>,
    /// The session domain: id→source registry + resident followers + TTL reaping.
    cache: SessionCache<RecordStore, ServeAux>,
    /// This server's port, set once the listener binds.
    ///
    /// It exists so the lock's note can name where we serve. The note cannot be written at
    /// startup: sessions are admitted lazily, on their first `/pull`, so at bind time this
    /// process owns nothing and a publish would silently do nothing — which is exactly what
    /// used to happen, leaving every lock's note `null` and a peer with nowhere to redirect.
    port: std::sync::OnceLock<u16>,
}

/// Where an ALREADY-RUNNING server serves `sid`, if one does (#96's rendezvous).
///
/// The lock is a rendezvous record, not just a mutex: its holder writes the port it serves on,
/// so a second invocation can send the user to the existing server instead of standing up a
/// duplicate. Read-only — this never takes the lock.
///
/// Both checks matter. A pid alone is not enough, because pids are recycled; a port alone is
/// not enough, because some *other* program may now hold that port. A holder that has not
/// published a note yet has bound nothing to redirect to, so it does not count.
pub fn existing_server(root: Option<&std::path::Path>, sid: &str) -> Option<u16> {
    let dir = cache::admit::entry_dir(root?, Presentation::Html, sid);
    let h = lock::read::<HtmlNote>(&dir)?;
    if h.pid == std::process::id() {
        return None; // our own lock from earlier in this process
    }
    let port = h.note?.port;
    (lock::pid_alive(h.pid) && port_open(Some(port))).then_some(port)
}

/// The browser URL a hand-off points at.
pub fn handoff_url(port: u16, sid: &str) -> String {
    format!("http://127.0.0.1:{port}/index.html?session={sid}")
}

/// Why this server is not serving a session, when it is not (#163). One entry has one writer, so
/// a process that is not it never serves a second copy — it either routes the client to the
/// owner or says what went wrong.
enum Unserved {
    /// A live peer owns the entry and published where it serves. The client must NAVIGATE there.
    ///
    /// Deliberately not an HTTP 302: `fetch` follows a redirect transparently, so the pull loop
    /// would carry on polling the peer with a cursor (`epoch.committed.gen.index`) minted against
    /// THIS server's record stream — and #159 is the standing evidence that two folds of one
    /// transcript need not agree. A reply the client acts on is a reply it can act on correctly.
    Elsewhere(String),
    /// Nothing here, and nowhere to send anyone. The text says which, because the alternative is
    /// the failure mode this whole task exists to remove: a page that shows nothing and explains
    /// nothing.
    Nowhere(String),
}

/// A pull reply that routes the client elsewhere instead of feeding it.
fn redirect_reply(url: &str) -> String {
    json!({"t": "redirect", "url": url}).to_string()
}

/// Plain English for a denial that is nobody's fault but the machine's.
fn reason(why: Unavailable) -> &'static str {
    match why {
        Unavailable::NoCacheFlag => "this server has no cache root",
        Unavailable::UnwritableRoot => {
            "its cache directory cannot be written — set $CLAUDE_REPLAY_CACHE somewhere writable"
        }
        Unavailable::NoLivenessCheck => {
            "this platform cannot tell whether a lock's holder is still running, so entries \
             cannot be shared safely"
        }
        Unavailable::UnknownSession => "no transcript is registered under that id",
    }
}

/// The render fingerprint a durable stream is validated against: change what a wire record
/// contains, or how folding shapes it, and a resumed page would splice two schemas together.
///
/// It covers the fold policy and the record schema, and deliberately not the session's cwd —
/// which paths are rendered relative to. The cwd is a pure function of the transcript (a root's
/// is read from its own head; a child's is inherited from its single parent, whose own is), and
/// the transcript's identity is already pinned by the stream's anchor.
fn render_flavor(fold: &FoldPolicy) -> u64 {
    use std::hash::{Hash, Hasher};
    /// Bump when the wire record's shape changes.
    const RECORD_SCHEMA: u16 = 1;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    RECORD_SCHEMA.hash(&mut h);
    fold.folded_kinds().hash(&mut h);
    h.finish()
}

/// Whether a lock's published port still answers. A pid alone is not enough: pids are recycled,
/// and a stale lock naming a recycled pid would look live forever. `None` (a holder that took
/// the lock but has not bound yet) counts as live — it is a real window, not a dead process.
fn port_open(port: Option<u16>) -> bool {
    let Some(port) = port else { return true };
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .is_ok()
}

/// One root session this server hosts (its own agent, transcript, and cwd).
struct Root {
    id: String,
    agent: Agent,
    path: std::path::PathBuf,
    cwd: String,
}

/// The live server's per-session presentation sidecar, held in the cache's aux slot (#76 —
/// "the cache IS the data layer"). `title`: the non-source half of the descriptor (a child's
/// derives once, on its first resolve). `parent`: child → parent session id, recorded when
/// the parent's pull registers the child's source ([`derive_title`](SessionService::derive_title)
/// follows it).
#[derive(Default)]
struct ServeAux {
    title: Option<TitleInfo>,
    parent: Option<String>,
    /// This session's working directory — recorded once at registration (roots) or
    /// inherited from the parent (children, which run in the parent's cwd), so a pull
    /// never re-reads a transcript head just to render paths relative to it.
    cwd: Option<String>,
    /// Whether this session's "cannot serve it" reason has already been logged. A client that
    /// cannot be served keeps polling, and one line per poll would bury the first (#163).
    unserved: bool,
    /// Rendered open-turn records, keyed by `(epoch, gen, len)` (#85): within a gen the
    /// finalized provisional is append-only and the committed prefix frozen, so an equal
    /// key ⇒ identical records — concurrent clients (or fast re-pulls) reuse the render
    /// instead of re-running markdown + highlighting over the whole open turn.
    prov_render: Option<((u64, u64, usize), Vec<String>)>,
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

/// What a HOST decides for a [`SessionService`] (#98 §6.2) — everything `start_server`
/// used to hardcode. `--html` passes claude-replay's own values; the monitor passes its
/// root and scratch. One implementation either way, so the byte gate keeps covering it.
pub struct ServiceConfig {
    /// Durable cache root. `None` ⇒ ephemeral (exactly `--no-cache`).
    pub cache_root: Option<std::path::PathBuf>,
    /// Namespace within that root. Both known hosts pass [`Presentation::Html`] — they
    /// differ by ROOT, not by namespace (§10).
    pub presentation: Presentation,
    /// Render parameters — also the flavor the durable stream is validated against.
    pub fold: FoldPolicy,
    /// Where the host keeps this run's static shell. Created here so a host does not have to;
    /// the service itself writes nothing to it — it once held the cache-less fallback's private
    /// record logs, and that is gone (#163).
    pub scratch: std::path::PathBuf,
}

impl SessionService {
    /// Stand the session service up from a host's config. Creates the scratch dir; binds
    /// nothing (the host owns the listener; see [`spawn_listener`]).
    pub fn new(cfg: ServiceConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.scratch)
            .with_context(|| format!("create {}", cfg.scratch.display()))?;
        let cache = match cfg.cache_root {
            Some(root) => SessionCache::durable(
                cfg.presentation,
                root,
                Versions::current(Some(render_flavor(&cfg.fold))),
            ),
            None => SessionCache::ephemeral(),
        };
        Ok(Self {
            fold: cfg.fold,
            roots: std::sync::Mutex::new(Vec::new()),
            cache,
            port: std::sync::OnceLock::new(),
        })
    }

    /// Register a ROOT session this service hosts (children register themselves as their
    /// parents' pulls discover them). Safe to call any time — the monitor registers new
    /// sessions as its scans find them. Returns the session id.
    pub fn register_root(&self, path: &Path) -> String {
        let id = session_id(path);
        {
            // Idempotent and CHEAP for a known id: a monitor re-registers every session on
            // every scan, and the sniffing below reads transcript heads.
            let roots = self.roots.lock().unwrap_or_else(|e| e.into_inner());
            if roots.iter().any(|r| r.id == id) {
                return id;
            }
        }
        let agent = discover::detect_agent(path);
        let cwd = discover::session_cwd(path)
            .map(|c| c.display().to_string())
            .unwrap_or_default();
        {
            let mut roots = self.roots.lock().unwrap_or_else(|e| e.into_inner());
            if !roots.iter().any(|r| r.id == id) {
                roots.push(Root {
                    id: id.clone(),
                    agent,
                    path: path.to_path_buf(),
                    cwd: cwd.clone(),
                });
            }
        }
        self.cache
            .register_new(&id, Transcript::open(agent, path.to_path_buf()));
        let title = display_title(agent, path);
        // EVERY root needs its own title: `derive_title` follows a parent pointer, and a
        // root has none, so an untitled root would show as its bare session id.
        self.cache.aux_with(&id, |a| {
            if a.title.is_none() {
                a.title = Some(TitleInfo {
                    title,
                    ..Default::default()
                });
            }
            if a.cwd.is_none() {
                a.cwd = Some(cwd);
            }
        });
        id
    }

    /// The port this service is reachable on — published into each admitted session's lock
    /// as the #96 rendezvous note. Set once, after the host's listener binds.
    pub fn set_port(&self, port: u16) {
        let _ = self.port.set(port);
    }

    /// The complete session view for `id` at a URL (#98 §6.3) — exactly the page `--html`
    /// serves, with optional host [`PageChrome`]. `None` for an id this service cannot
    /// resolve.
    pub fn page(&self, id: &str, chrome: Option<&PageChrome>) -> Option<String> {
        let (_, t) = self.resolve_id(id)?;
        Some(match chrome {
            Some(c) => build_shell_chrome(&t.title, id, c),
            None => build_shell(&t.title, id, true, true),
        })
    }

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

    /// This session's working directory — paths render relative to it. Reads the aux note
    /// (written at registration for a root, inherited from the parent for a child) and only
    /// falls back to sniffing the transcript head for a cold deep link that had neither,
    /// memoizing the result so the sniff happens at most once per session.
    fn cwd_of(&self, id: &str, src: &Transcript) -> String {
        if let Some(cwd) = self.cache.aux_with(id, |a| a.cwd.clone()) {
            return cwd;
        }
        // A hosted root's cwd was resolved when the server started; anything else (a cold
        // deep link into a child) sniffs the transcript head, once.
        let cwd = self
            .roots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.cwd.clone())
            .unwrap_or_else(|| {
                discover::session_cwd(src.path())
                    .map(|p| p.display().to_string())
                    .unwrap_or_default()
            });
        self.cache.aux_with(id, |a| a.cwd = Some(cwd.clone()));
        cwd
    }

    /// Resolve `id` to its source + title. Tier-(c) lookup first (the cache registry, populated
    /// from spawn events); else resolve the source directly — every agent shares the flat
    /// `subagents/` dir, so a valid id resolves even if its parent was never navigated (deep links)
    /// — with a plain title until its parent's spawn supplies the description, registering the
    /// fallback into the cache/titles so later lookups find it. `None` for an unknown id. Used by
    /// [`pull_response`](Self::pull_response) (the `/pull` path).
    fn resolve_id(&self, id: &str) -> Option<(Transcript, TitleInfo)> {
        if let Some(src) = self.cache.resolve(id) {
            // A registered id with no title yet was registered source-only by its parent's pull
            // (`register_child_sources`): derive its title/breadcrumb ONCE from the parent's
            // maintained meta, now that this session is actually being retrieved.
            let cached = self.cache.aux_with(id, |a| a.title.clone());
            let t = cached.unwrap_or_else(|| self.derive_title(id));
            return Some((src, t));
        }
        // A deep link to a child whose parent was never pulled: no parent pointer exists
        // yet, so ask each root whether the id is in ITS subtree. Only reached on a cold
        // deep link (the common path is `register_child_sources`, one batch per parent),
        // and it stops at the first hit — but it is O(roots) resolves, so keep it off the
        // hot path.
        let (agent, source) = {
            let roots = self.roots.lock().unwrap_or_else(|e| e.into_inner());
            roots.iter().find_map(|r| {
                discover::subagent_source(r.agent, &r.path, id).map(|path| (r.agent, path))
            })?
        };
        if !source.exists() {
            return None;
        }
        let src = Transcript::open(agent, source);
        let t = TitleInfo {
            title: id.to_string(),
            ..Default::default() // unknown ancestry/type for an un-navigated deep link
        };
        self.cache.register(id, src.clone());
        self.cache.aux_with(id, |a| a.title = Some(t.clone()));
        Some((src, t))
    }

    /// Derive (and cache) a child session's title/breadcrumb ONCE, on its first resolve, from
    /// its **parent's maintained meta** — the child-side half of the pull path's nav inversion.
    /// Follows the parent pointer recorded at registration; reads the child's description off the
    /// parent's [`SessionMeta`](crate::engine::SessionMeta) and chains the parent's own ancestry.
    /// Falls back to the bare id when the parent isn't resident (a deep link before the parent
    /// was ever pulled, or the parent's live state was TTL-reaped) — matching the pre-existing
    /// deep-link fallback; whichever value is derived is cached one-time, as before.
    fn derive_title(&self, id: &str) -> TitleInfo {
        let parent_id = self.cache.aux_with(id, |a| a.parent.clone());
        let derived = parent_id.and_then(|pid| {
            let pmeta = self.cache.shared_peek(&pid)?.session_meta();
            let c = pmeta.children.iter().find(|c| c.id == id)?;
            let pt = self
                .cache
                .aux_with(&pid, |a| a.title.clone())
                .unwrap_or_default();
            let mut ancestors = pt.ancestors;
            ancestors.push((pid, pt.title));
            Some(TitleInfo {
                title: if c.description.is_empty() {
                    c.agent_type.clone()
                } else {
                    c.description.clone()
                },
                agent_type: c.agent_type.clone(),
                ancestors,
            })
        });
        let t = derived.unwrap_or_else(|| TitleInfo {
            title: id.to_string(),
            ..Default::default()
        });
        self.cache
            .aux_with(id, |a| a.title.get_or_insert(t).clone())
    }

    /// Record `parent_id`'s children in the id→source registry — a pure path derivation, one
    /// time per child (already-registered ids are skipped) — plus the parent pointer
    /// [`derive_title`](Self::derive_title) follows later. **No title writes**: the pull path's
    /// per-poll cross-session `register_children` is inverted into this one-time source note +
    /// the child's own lazy, one-time title derivation on its first pull.
    fn register_child_sources(&self, parent_id: &str, children: &[crate::engine::ChildMeta]) {
        let unregistered: Vec<&str> = children
            .iter()
            .filter(|c| !self.cache.is_registered(&c.id))
            .map(|c| c.id.as_str())
            .collect();
        if unregistered.is_empty() {
            return;
        }
        // One operation-scoped batch: an adapter backed by a relationship store (Codex)
        // scans it once for the whole child list, not once per child.
        // Resolve against the PARENT's own agent + transcript (not a server-wide root):
        // with several roots hosted at once they may be different agents entirely.
        let Some(parent) = self.cache.resolve(parent_id) else {
            return;
        };
        let parent_cwd = self.cache.aux_with(parent_id, |a| a.cwd.clone());
        let sources = discover::subagent_sources(parent.agent(), parent.path(), &unregistered);
        for (id, source) in unregistered.into_iter().zip(sources) {
            if let Some(source) = source {
                self.cache
                    .register_new(id, Transcript::open(parent.agent(), source));
                self.cache.aux_with(id, |a| {
                    a.parent = Some(parent_id.to_string());
                    // A child runs in its parent's directory — inherit rather than
                    // re-read the child transcript's head on its first pull.
                    a.cwd = parent_cwd.clone();
                });
            }
        }
    }

    /// This session's resident, admitting it into the durable cache on first use (#96).
    ///
    /// A denial ends here — it never becomes a second copy of the session (#163). There used to
    /// be a fallback that opened a private record log and re-folded from scratch, and it was the
    /// worst of both: it re-appended a whole render on every TTL cycle (#158) and disagreed with
    /// the owner about the tail (#159), with nothing on the page to say so. One entry has one
    /// writer, so a process that is not it has exactly two honest answers: send the client where
    /// the owner serves, or say why it cannot.
    fn session_for(
        &self,
        id: &str,
        src: &Transcript,
        cwd: &str,
    ) -> Result<Arc<SharedSession<RecordStore>>, Unserved> {
        if let Some(ss) = self.cache.touch(id) {
            return Ok(ss);
        }
        let agent = src.agent();
        let path = src.path().to_path_buf();
        // #74: the Session's own BlockStore renders each committed block to its wire record as
        // it commits (Bv = RecordLocator) — one storage, one serialization; there is no separate
        // block backing. A followed session's resident footprint is O(open turn) + the locators.
        let open = |at: &Path| {
            RecordStore::open_append(
                at,
                self.fold.clone(),
                cwd.to_string(),
                Transcript::open(agent, path.clone()),
            )
        };
        // A lock's note names where its holder serves — but a note naming OUR OWN port is not
        // evidence of a peer. A recycled pid plus our own listener answering would otherwise
        // satisfy both halves of this predicate forever, and we would deny ourselves an entry we
        // could take and then "redirect" the client to the server it is already talking to.
        let ours = self.port.get().copied();
        let alive = |h: &Holder<HtmlNote>| {
            let port = h.note.as_ref().map(|n| n.port);
            port != ours && lock::pid_alive(h.pid) && port_open(port)
        };
        match self
            .cache
            .admit(id, |dir| open(&dir.join("records.jsonl")), alive)
        {
            Admission::Owned { session, .. } => {
                // Now that the entry is ours, say where we serve it. This is the first moment
                // both facts are true: the lock is held AND the port is known.
                if let Some(&port) = self.port.get() {
                    let _ = self.cache.publish(id, HtmlNote { port });
                }
                Ok(session)
            }
            // A live peer owns it. If it has published a port, that is where this session is —
            // send the client there.
            Admission::Denied(Denial::Held(h)) => Err(match h.note {
                Some(n) => Unserved::Elsewhere(handoff_url(n.port, id)),
                // It took the lock but has not bound yet, so there is no URL to name. Say so
                // rather than inventing a target or quietly serving a second copy.
                None => Unserved::Nowhere(format!(
                    "session {id} is held by pid {} — it has not published where it serves yet",
                    h.pid
                )),
            }),
            // No peer, no entry: the cache root is unusable or the id is unknown. Nothing here is
            // recoverable by serving the session another way.
            Admission::Denied(Denial::Unavailable(why)) => {
                Err(Unserved::Nowhere(format!("session {id}: {}", reason(why))))
            }
        }
    }

    /// The `/pull` handler: serve the pull-client wire reply for `id` at `cursor`. The session
    /// domain lives in the [`SessionCache`]: it materializes the id's [`SharedSession`] on first
    /// pull and TTL-reaps idle residents (no background thread — folding rides this request's
    /// thread, so a session nobody is pulling costs nothing).
    ///
    /// `Err` for a session this server will not serve: [`Unserved::Elsewhere`] carries the owner's
    /// URL for the client to navigate to, [`Unserved::Nowhere`] the reason there is nothing to go
    /// to. Both reach the client — a blank page that says nothing is the bug (#163).
    fn pull_response_for(&self, id: &str, cursor: Cursor) -> Result<String, Unserved> {
        let unknown = || Unserved::Nowhere(format!("session {id}: no such transcript"));
        let (src, title) = self.resolve_id(id).ok_or_else(unknown)?;
        if !src.path().exists() {
            return Err(Unserved::Nowhere(format!(
                "session {id}: {} is gone",
                src.path().display()
            )));
        }
        // Per-SESSION context: this server can host several unrelated roots at once, so the
        // agent comes from the session's own `Transcript` and the cwd from its aux note
        // (recorded at registration, inherited by children) — never a server-wide field.
        let agent = src.agent();
        let cwd = self.cwd_of(id, &src);
        // Lazy reap (this path owns no background thread), then fetch-or-admit the resident —
        // both owned by the cache (one resident set, one policy).
        self.cache.reap(TAIL_TTL_MS);
        let mut shared = self.session_for(id, &src, &cwd)?;
        if shared.poisoned() {
            // A panic poisoned it mid-update (#56 — its state may be torn). Drop it and refold
            // fresh; the new epoch resyncs clients. Never serve torn state, never brick.
            //
            // Order matters (#168): `release` FIRST, while the session is still resident, because
            // it finds it through the map and quiesces it — that is what stops the old writer
            // before the re-admission below opens another store on the same log. Then let go of
            // it, so the store is closed rather than merely silent.
            self.cache.release(id);
            self.cache.remove_pull(id);
            drop(shared);
            shared = self.session_for(id, &src, &cwd)?;
        }
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
            return Ok(json!({
                "t": "pull", "epoch": epoch,
                "committed_from": cf, "committed_ext": Value::Null,
                "provisional_gen": gen,
                "provisional_from": pf, "provisional": [],
                "meta": Value::Null,
            })
            .to_string());
        }
        let info = self.agent_info(id, src.path().to_path_buf(), &title);
        // Attachments load from THIS agent's own transcript.
        let transcript = crate::Transcript::open(agent, info.source.clone());

        // #74: committed records were already rendered-once by the store's `put` as each
        // block crossed the durability frontier — the Session's committed table IS the wire
        // projection. One consistent read (a single lock) hands back the open-turn delta plus
        // the two store-derived facts this reply needs: the committed byte range for this
        // cursor and the render continuation the open turn resumes from.
        let (d, (committed_ext, mut open_emit)) = shared.open_delta_with(|store, committed, d| {
            let (cfx, _) = pull_indices(
                d.epoch,
                committed.len(),
                d.provisional.len(),
                d.provisional_gen,
                cursor,
            );
            let start = committed
                .get(cfx)
                .map(|l| l.offset)
                .unwrap_or_else(|| store.log_len());
            let ext = (store.log_len() > start).then_some((start, store.log_len() - start));
            (ext, store.emit_snapshot())
        });
        let prov_key = (d.epoch, d.provisional_gen, d.provisional.len());
        let cached = self.cache.aux_with(id, |a| {
            a.prov_render
                .as_ref()
                .filter(|(k, _)| *k == prov_key)
                .map(|(_, l)| l.clone())
        });
        let provisional_lines = cached.unwrap_or_else(|| {
            let lines = render_blocks(
                &d.provisional,
                &d.user_times,
                &self.fold,
                &cwd,
                true,
                true,
                None,
                Some(&transcript),
                &mut open_emit,
            );
            self.cache
                .aux_with(id, |a| a.prov_render = Some((prov_key, lines.clone())));
            lines
        });
        // Slice each zone at the cursor (via the tested pull_indices). The committed zone is a
        // POINTER `{offset, len}` into the on-disk `<id>.records` log — the client range-reads
        // it via `/records`: the reply never carries the committed bytes, so the server renders
        // and buffers none of them.
        let (cf, pf) = pull_indices(
            d.epoch,
            d.n_committed,
            provisional_lines.len(),
            d.provisional_gen,
            cursor,
        );
        // The meta wire record from the maintained header (no block scan) + this agent's
        // presentation info. Children get a one-time source+parent-pointer note so their
        // `?session=` links resolve; their titles derive lazily on THEIR first pull
        // (`derive_title`) — this pull touches no other session's presentation state.
        // Meta tasks (#15): the fold's op-log overlaid by a fresh read of the live task
        // files (small dir; the pull is already a per-second file poll).
        let tasks = crate::engine::tasks::merged(
            &d.tasks,
            crate::discover::session_tasks(agent, src.path()),
        );
        let meta = assemble_meta(agent, &cwd, &info, &d.meta, &d.metrics, &tasks);
        self.register_child_sources(id, &d.meta.children);
        let provisional_records: Vec<&str> = provisional_lines[pf.min(provisional_lines.len())..]
            .iter()
            .map(String::as_str)
            .collect();
        Ok(pull_reply_json(
            d.epoch,
            d.provisional_gen,
            cf,
            committed_ext,
            pf,
            &provisional_records,
            &meta,
        ))
    }

    /// The `/pull` body for `id`, whichever kind it is: a feed, a hand-off to the owner, or the
    /// reason there is neither. Every one of the three reaches the client — that is the whole
    /// point of #163, since the alternative was a page that showed nothing and said nothing.
    /// The STATUS still means what it always meant, because `/pull` is a public wire surface and
    /// not only the browser reads it: a feed and a hand-off are 200 (both are answers), a session
    /// that cannot be served at all is 404. The body is JSON either way, so a client that
    /// switches on `t` never has to look.
    pub fn pull_response(&self, id: &str, cursor: Cursor) -> HttpResponse {
        match self.pull_response_for(id, cursor) {
            Ok(body) => {
                // A session that recovers may fail again later, and THAT one deserves a line too.
                self.cache.aux_with(id, |a| a.unserved = false);
                HttpResponse::json(body)
            }
            Err(Unserved::Elsewhere(url)) => HttpResponse::json(redirect_reply(&url)),
            Err(Unserved::Nowhere(why)) => {
                // Once per spell of failure, not once per poll: a client that cannot be served
                // keeps asking, and a line a second would bury the one that matters.
                if self
                    .cache
                    .aux_with(id, |a| !std::mem::replace(&mut a.unserved, true))
                {
                    eprintln!("claude-replay: {why}");
                }
                HttpResponse {
                    code: "404 Not Found",
                    content_type: "application/json; charset=utf-8",
                    body: json!({"t": "error", "message": why})
                        .to_string()
                        .into_bytes(),
                }
            }
        }
    }

    /// Serve `[from, from+len)` off `<id>.records` — the client's committed range read (the
    /// second phase of a pull whose reply carried a `committed_ext` pointer). `Err(StaleEpoch)` → **409**
    /// when `epoch` doesn't match the log's current epoch: a reset recreated the log since the
    /// pointer was issued, so the bytes would be wrong — the client drops the whole reply and
    /// re-pulls with its old cursor (the epoch bump then resyncs it). Read under the session
    /// lock so a concurrent reset can't swap the log mid-read (#74: through the store).
    pub fn records_bytes(
        &self,
        id: &str,
        from: u64,
        len: u64,
        epoch: u64,
    ) -> Result<Vec<u8>, StaleEpoch> {
        // Through the resident store, under the session lock — the epoch check can't tear
        // against a concurrent reset. A reaped resident yields 409; the client's next pull
        // rematerializes it and reissues the pointer.
        let ss = self.cache.shared_peek(id).ok_or(StaleEpoch)?;
        ss.store_read(|cur_epoch, store| {
            if cur_epoch != epoch {
                return Err(StaleEpoch);
            }
            Ok(store.read_range(from, from.saturating_add(len)))
        })
    }
}

/// Build the `/pull` wire reply string. The **provisional** records are spliced inline (already
/// JSON objects, so `[rec1,rec2,…]` is a valid array — no per-record parse); the **committed**
/// zone is a pointer `committed_ext: {offset, len}` into `<id>.records` that the client
/// range-reads via `/records` (`null` when there is nothing new). The content-blind client
/// materializes the committed records from the range read, then applies "truncate to `from`,
/// then extend" per zone (see `export.js`).
fn pull_reply_json(
    epoch: u64,
    provisional_gen: u64,
    committed_from: usize,
    committed_ext: Option<(u64, u64)>,
    provisional_from: usize,
    provisional_records: &[&str],
    meta: &Value,
) -> String {
    let provisional = format!("[{}]", provisional_records.join(","));
    let ext = match committed_ext {
        Some((offset, len)) if len > 0 => format!("{{\"offset\":{offset},\"len\":{len}}}"),
        _ => "null".into(),
    };
    format!(
        "{{\"t\":\"pull\",\"epoch\":{epoch},\"committed_from\":{committed_from},\"committed_ext\":{ext},\"provisional_gen\":{provisional_gen},\"provisional_from\":{provisional_from},\"provisional\":{provisional},\"meta\":{meta}}}"
    )
}

/// `--html`: render to HTML and open it in the browser instead of the TUI, as a
/// **multi-file bundle** — one shared shell + one `<id>.jsonl` per agent — so sub-agent
/// drill-down works (clicking an agent navigates to its own stream). Serves over a tiny
/// **loopback HTTP server** (not `file://`) so a path click can reveal the file in Finder
/// (`/__reveal`) and the page can `fetch` its streams. It live-tails the whole tree, keeping
/// every agent's stream current as new spawns appear and children grow.
///
/// If another instance is **already serving this session**, this hands off to it — opens that
/// server's URL and returns, rather than standing up a second server, a second fold and a second
/// copy of the same session. Reuse beats duplication when the running server already serves what
/// was asked for (#96).
pub fn serve(args: &Args, path: &Path) -> Result<()> {
    let sid = session_id(path);
    let root = (!args.no_cache).then(cache::admit::default_root).flatten();
    if let Some(port) = existing_server(root.as_deref(), &sid) {
        let url = handoff_url(port, &sid);
        eprintln!("already served by another claude-replay at {url}");
        open_in_browser(&url);
        println!("{url}");
        return Ok(());
    }
    let server = start_server(args, std::slice::from_ref(&path.to_path_buf()))?;
    let url = server.url_for_root(0).expect("one root");
    eprintln!(
        "serving {} at {url} (live — Ctrl-C to stop)",
        server.dir.display()
    );
    eprintln!("  open in a browser, or copy the URL above");
    open_in_browser(&url);
    println!("{url}");

    // Client-driven (no background thread, zero cost when idle): keep serving so
    // navigation + reveal keep working; sessions materialize on their first `/pull`.
    loop {
        std::thread::park();
    }
}

/// A running live server, and the roots it hosts. Returned by [`start_server`] so a caller
/// can keep its own UI in the foreground (the `-f --html` session picker stays up and opens
/// a browser tab per pick) instead of the blocking [`serve`] loop.
pub struct LiveServer {
    /// The bundle directory (shell + per-session artifacts).
    pub dir: std::path::PathBuf,
    /// Loopback port the bundle is served on.
    pub port: u16,
    /// Session ids of the hosted roots, in the order they were passed in.
    pub root_ids: Vec<String>,
    /// The SHARED cache root, when this run uses one — what [`open`](Self::open) consults to see
    /// whether a session is already served elsewhere. `None` for `--no-cache`, whose whole
    /// purpose is a view that does not defer to the holder.
    shared_root: Option<std::path::PathBuf>,
    /// Keeps the server state alive for the process's lifetime.
    _live: std::sync::Arc<SessionService>,
}

impl LiveServer {
    /// The browser URL for hosted root `i` (its own `?session=` on the shared shell).
    pub fn url_for_root(&self, i: usize) -> Option<String> {
        self.root_ids.get(i).map(|sid| self.url_for(sid))
    }

    /// The browser URL for any hosted session id.
    pub fn url_for(&self, sid: &str) -> String {
        format!("http://127.0.0.1:{}/index.html?session={}", self.port, sid)
    }

    /// Open a hosted session in the default browser (best-effort).
    ///
    /// The PICKER stays up (#163) — it is not a session and defers to nobody — but a session it
    /// opens that another viewer already holds belongs to that viewer: send the browser straight
    /// there. Without this the tab would still arrive, one hop later, when its first `/pull`
    /// answered with a redirect; checking here spares the user a page that appears and leaves.
    pub fn open(&self, sid: &str) {
        match existing_server(self.shared_root.as_deref(), sid) {
            Some(port) => open_in_browser(&handoff_url(port, sid)),
            None => open_in_browser(&self.url_for(sid)),
        }
    }
}

/// Start the live server over one or MORE root sessions and return without blocking.
///
/// Every root is registered up front — a registry entry only, since streams and folds are
/// produced on a session's first `/pull` — so all of them are live simultaneously and each
/// is reachable at `?session=<id>` on the one shared shell. Roots may span agents and
/// working directories; the server's per-session agent/cwd plumbing is what makes that
/// safe. [`serve`] is the single-root special case of this.
pub fn start_server(args: &Args, paths: &[std::path::PathBuf]) -> Result<LiveServer> {
    use std::sync::Arc;
    anyhow::ensure!(!paths.is_empty(), "no sessions to serve");

    // Take back what dead runs left behind before adding this run's own directory (#165).
    crate::sys::reclaim();

    // This run's bundle (shell + per-agent artifacts), under the cache home rather than
    // `$TMPDIR` — everything the tool writes then lives in one place a person can find, and
    // `reclaim` above can take it back when the run is gone. Per-RUN, not per-session: a pid
    // is what makes concurrent runs unable to wipe each other's bundle, and it made the
    // one-root/several-roots split unnecessary. Fresh per run — wipe anything a previous run
    // with this pid left, so lazy materialization starts clean.
    let dir = crate::sys::run_dir();
    let _ = std::fs::remove_dir_all(&dir);

    // The service, from claude-replay's OWN config (#98 §6.2): the durable cache at
    // claude-replay's root, the CLI's fold policy, the bundle dir as scratch. The monitor is
    // the other caller, with its own values — one implementation either way, which is what
    // keeps the byte gate covering this path.
    //
    // `--no-cache` selects a private cache root, NOT the absence of one (#165): the same
    // implementation, at a root no other viewer coordinates over. That is what makes the flag
    // a genuine second view rather than a degraded mode — and it is why nothing here needs a
    // fallback for "the cache said no".
    // `None` under `--no-cache`: the flag opts out of the shared root AND of deferring to whoever
    // holds it, or it would send you back to the process you were trying to bypass.
    let shared_root = (!args.no_cache).then(cache::admit::default_root).flatten();
    let live = Arc::new(SessionService::new(ServiceConfig {
        cache_root: Some(match &shared_root {
            Some(root) => root.clone(),
            None => crate::sys::throwaway_root(),
        }),
        presentation: Presentation::Html,
        fold: args.fold_policy(),
        scratch: dir.clone(),
    })?);
    let root_ids: Vec<String> = paths.iter().map(|p| live.register_root(p)).collect();

    // ONE transport (#85): every server-backed page — static or live — is a pull client
    // (a static page pulls once; a live page keeps polling). One protocol exercised by all
    // traffic, protected by one test suite, folded on the requester's own thread.
    let first = paths[0].clone();
    let title = display_title(discover::detect_agent(&first), &first);
    std::fs::write(
        dir.join("index.html"),
        // Always live: a served page tails its session, full stop.
        build_shell(&title, &root_ids[0], true, true),
    )
    .with_context(|| "write index.html")?;

    let port = spawn_http_server(dir.clone(), Some(live.clone()))?;
    // Hand the port to the session path, which publishes it as each session is admitted. Not a
    // publish loop here: nothing is admitted yet.
    live.set_port(port);
    Ok(LiveServer {
        dir,
        port,
        root_ids,
        shared_root,
        _live: live,
    })
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
fn spawn_http_server(
    root: std::path::PathBuf,
    live: Option<std::sync::Arc<SessionService>>,
) -> Result<u16> {
    spawn_listener(
        0,
        std::sync::Arc::new(move |req: &Request| {
            service_routes(live.as_deref(), &root, req.name, req.query)
        }),
    )
}

/// A `/records` range read against an epoch the store has since left — the client drops
/// the reply whole and re-pulls (the 409 path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaleEpoch;

/// One HTTP reply — what a [`spawn_listener`] handler returns. Plain data so a host's own
/// routes and [`service_routes`] compose without either knowing the socket.
pub struct HttpResponse {
    /// e.g. `"200 OK"`.
    pub code: &'static str,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn ok(content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            code: "200 OK",
            content_type,
            body,
        }
    }
    pub fn html(body: String) -> Self {
        Self::ok("text/html; charset=utf-8", body.into_bytes())
    }
    pub fn json(body: String) -> Self {
        Self::ok("application/json; charset=utf-8", body.into_bytes())
    }
    pub fn not_found(msg: &'static str) -> Self {
        Self {
            code: "404 Not Found",
            content_type: "text/plain",
            body: msg.as_bytes().to_vec(),
        }
    }
    pub fn unauthorized(msg: &'static str) -> Self {
        Self {
            code: "401 Unauthorized",
            content_type: "text/plain",
            body: msg.as_bytes().to_vec(),
        }
    }
    pub fn forbidden(msg: &'static str) -> Self {
        Self {
            code: "403 Forbidden",
            content_type: "text/plain",
            body: msg.as_bytes().to_vec(),
        }
    }
    pub fn method_not_allowed(msg: &'static str) -> Self {
        Self {
            code: "405 Method Not Allowed",
            content_type: "text/plain",
            body: msg.as_bytes().to_vec(),
        }
    }
}

/// A parsed request handed to a route handler. Reads consume `name`/`query` exactly as
/// before; a WRITE route (#133) additionally gates on POST + [`authenticated`](Self::authenticated)
/// + [`origin_ok`](Self::origin_ok) via [`deny_write`](Self::deny_write).
pub struct Request<'a> {
    pub method: &'a str,
    pub name: &'a str,
    pub query: &'a str,
    pub body: &'a [u8],
    /// A valid TOKEN was presented — not merely a same-user loopback peer. This is the
    /// "authenticated" bar a write must clear (#133/#196): it is FALSE on an unpaired
    /// monitor, so a stock binary cannot inject until `claude-monitor --pair` — pairing is
    /// the master switch for the write capability.
    pub authenticated: bool,
    /// The `Host`/`Origin` headers are the monitor's own loopback origin (or absent) —
    /// §3.2 defense-in-depth over #196's auth. A cross-site request carries a foreign
    /// `Origin`; a DNS-rebound one carries a foreign `Host`; both are refused here even if
    /// they somehow held a token.
    pub origin_ok: bool,
}

impl Request<'_> {
    /// Gate a write route (#133 §3.2): `None` to proceed, else the exact 4xx to return. A
    /// write requires `POST`, a same-origin request, and an authenticated (token-bearing)
    /// client — in that order, so the reply names the first thing that is wrong.
    pub fn deny_write(&self) -> Option<HttpResponse> {
        if self.method != "POST" {
            return Some(HttpResponse::method_not_allowed("POST required"));
        }
        if !self.origin_ok {
            return Some(HttpResponse::forbidden("cross-origin request refused"));
        }
        if !self.authenticated {
            return Some(HttpResponse::unauthorized(
                "this action requires pairing — run `claude-monitor --pair`",
            ));
        }
        None
    }
}

/// A header's value by case-insensitive name (`headers` is the raw block, one per line).
fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

/// Whether the request's `Host`/`Origin` are the monitor's own loopback origin at `port`
/// (or absent) — §3.2. A foreign `Host` is DNS rebinding; a foreign `Origin` is a
/// cross-site fetch; either is refused.
fn origin_ok(headers: &str, port: u16) -> bool {
    let ours = |raw: &str| {
        let v = raw
            .trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        v == format!("127.0.0.1:{port}") || v == format!("localhost:{port}")
    };
    header_value(headers, "host").is_none_or(ours)
        && header_value(headers, "origin").is_none_or(ours)
}

/// The listener's access rule (#196 D3b + §4.2). Loopback is a MACHINE boundary, not a
/// USER one: on a shared dev server every local user can reach `127.0.0.1:<port>`, so an
/// unguarded loopback monitor is readable — and, once #195 lands writes, drivable — by
/// anyone with a login. (It is not even purely read-only from the OS's view: `/__reveal`
/// opens Finder on the server.) The gate admits a connection two ways:
///
/// 1. **Same-user loopback**, where the peer's uid is VERIFIABLE. On Linux — the
///    shared-server platform — the peer uid is read from `/proc/net/tcp`; `ssh -L`'s
///    remote end connects as the authenticated user's own sshd child, so a tunnel owner
///    passes with zero ceremony. Where no kernel mechanism exposes a TCP peer's uid
///    (macOS: no `SO_PEERCRED` for TCP, `pcblist` sysctl is fragile fail-open FFI) it
///    cannot verify — see the token.
/// 2. **A valid bearer token** (§4.2). A 256-bit secret in a 0600 file (`pair`), whose
///    same-user guarantee is the FILE PERMISSIONS — identical on every OS. This is what
///    makes a shared **Mac** safe: macOS cannot verify the peer, so a paired monitor
///    REQUIRES the token there and a stranger's request is refused. Comparison is
///    constant-time; the token rides `?token=`, `Authorization: Bearer`, or the
///    `cmauth` cookie.
///
/// Unpaired (no token), the gate is D3b exactly: Linux enforces same-user, macOS admits
/// same-machine (the single-user-Mac assumption). Pairing is what closes the multi-user
/// case. See `design/fleet-pairing.md` §4.2.
#[derive(Clone)]
pub struct AuthGate {
    /// Our effective uid; a loopback peer with this uid is same-user (Linux). `None` when
    /// it could not be read.
    euid: Option<u32>,
    /// The bearer token when paired; `None` = unpaired (D3b behavior).
    token: Option<std::sync::Arc<str>>,
}

/// The gate's ruling on one request.
pub(crate) enum Access {
    /// Admitted; no cookie to set (same-user, or the token already arrived as a cookie).
    Ok,
    /// Admitted via a `?token=`/header token — set the `cmauth` cookie so the browser's
    /// subsequent same-origin requests carry it without the secret in the URL.
    OkSetCookie,
    /// Refused.
    Denied,
}

impl AuthGate {
    /// The same-user gate for a loopback server, unpaired (D3b): read our euid at bind.
    pub fn same_user() -> Self {
        Self {
            euid: current_euid(),
            token: None,
        }
    }

    /// The paired gate: same-user OR the given bearer token (§4.2).
    pub fn with_token(token: impl Into<std::sync::Arc<str>>) -> Self {
        Self {
            euid: current_euid(),
            token: Some(token.into()),
        }
    }

    /// Test constructor: a FOREIGN euid, so the same-user leg is deterministically false
    /// on every platform (a real connection from the test process would otherwise pass
    /// same-user on Linux and make a deny assertion platform-dependent).
    #[cfg(test)]
    fn for_test(token: Option<&str>) -> Self {
        Self {
            euid: Some(u32::MAX), // never a real uid → same-user never matches
            token: token.map(std::sync::Arc::from),
        }
    }

    /// Whether a token is configured (the binary uses this to decide the pre-pair
    /// warning and whether to print a tokened URL).
    pub fn is_paired(&self) -> bool {
        self.token.is_some()
    }

    /// Whether `presented` is the configured token — the "authenticated" test a WRITE
    /// needs (#133), STRICTER than [`decide`](Self::decide): same-user loopback admits a
    /// READ but never a write. Always false on an unpaired monitor (no token).
    fn token_ok(&self, presented: Option<&str>) -> bool {
        matches!((self.token.as_deref(), presented),
            (Some(t), Some(p)) if ct_eq(t.as_bytes(), p.as_bytes()))
    }

    /// Rule on a request. `peer`/`local` are the accepted stream's ends; `presented` is the
    /// token extracted from the request (query/header/cookie), and `from_cookie` says it
    /// arrived as a cookie already (so no Set-Cookie is needed).
    fn decide(
        &self,
        peer: std::net::SocketAddr,
        local: std::net::SocketAddr,
        presented: Option<&str>,
        from_cookie: bool,
    ) -> Access {
        // A valid token admits regardless of peer identity (the phone/remote path, and
        // the shared-Mac path). Constant-time compare: loopback is where timing leaks.
        if let (Some(tok), Some(p)) = (self.token.as_deref(), presented) {
            if ct_eq(tok.as_bytes(), p.as_bytes()) {
                return if from_cookie {
                    Access::Ok
                } else {
                    Access::OkSetCookie
                };
            }
        }
        // Else the same-user loopback leg (D3b). Non-loopback never bypasses.
        if !peer.ip().is_loopback() {
            return Access::Denied;
        }
        let same_user = match (peer, local) {
            (std::net::SocketAddr::V4(p), std::net::SocketAddr::V4(l)) => {
                match peer_uid_v4(*p.ip(), p.port(), *l.ip(), l.port()) {
                    Some(uid) => self.euid == Some(uid),
                    // Unverifiable: same-machine ONLY when unpaired. A paired monitor on
                    // macOS refuses an unverifiable peer that lacks the token — the whole
                    // point of pairing a shared Mac.
                    None => self.token.is_none(),
                }
            }
            // v6 loopback: unverified; same-machine only when unpaired.
            _ => self.token.is_none(),
        };
        if same_user {
            Access::Ok
        } else {
            Access::Denied
        }
    }
}

/// Constant-time byte compare — no early exit, so a near-miss token cannot be timed out
/// character by character over loopback.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// A fresh 256-bit token as lowercase hex, from `/dev/urandom` (never time/pid-derived).
/// `None` if the OS RNG is unreadable — the caller then stays unpaired rather than mint a
/// weak secret.
pub fn mint_token() -> Option<String> {
    use std::io::Read;
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut buf)
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Our effective uid, dependency-free. Linux: the effective field of `/proc/self/status`'s
/// `Uid:` line (`real eff saved fs`). Elsewhere unused (the gate admits loopback there).
fn current_euid() -> Option<u32> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|l| {
        let rest = l.strip_prefix("Uid:")?;
        rest.split_whitespace().nth(1)?.parse().ok()
    })
}

/// The uid owning the loopback TCP socket whose local end is `peer` and remote end is
/// `local` (i.e. the CLIENT socket of a connection into our listener), from
/// `/proc/net/tcp`. `None` when the file is absent (non-Linux) or the row is not found.
fn peer_uid_v4(
    peer_ip: std::net::Ipv4Addr,
    peer_port: u16,
    local_ip: std::net::Ipv4Addr,
    local_port: u16,
) -> Option<u32> {
    let contents = std::fs::read_to_string("/proc/net/tcp").ok()?;
    find_peer_uid(&contents, peer_ip, peer_port, local_ip, local_port)
}

/// Pure `/proc/net/tcp` matcher — the client socket's row has `local_address` == the
/// connection's peer end and `rem_address` == our listener end; its `uid` column is the
/// connecting user. Split out so the byte-order parsing is unit-testable on synthetic
/// text (the address hex is the in-memory u32, little-endian per octet; the port is
/// big-endian hex).
fn find_peer_uid(
    contents: &str,
    peer_ip: std::net::Ipv4Addr,
    peer_port: u16,
    local_ip: std::net::Ipv4Addr,
    local_port: u16,
) -> Option<u32> {
    for line in contents.lines().skip(1) {
        let mut f = line.split_whitespace();
        let (Some(local_hex), Some(rem_hex)) = (f.nth(1), f.next()) else {
            continue;
        };
        // Fields after rem_address: st, tx:rx, tr:tm, retrnsmt, uid → uid is 4 past it.
        let uid = f.nth(4).and_then(|u| u.parse::<u32>().ok());
        if parse_proc_addr(local_hex) == Some((peer_ip, peer_port))
            && parse_proc_addr(rem_hex) == Some((local_ip, local_port))
        {
            return uid;
        }
    }
    None
}

/// `RRRRRRRR:PPPP` from `/proc/net/tcp` → (v4 addr, port). The address is a u32 whose
/// bytes are the octets in memory order (little-endian on the platforms Linux runs
/// here), so octet `i` is hex pair `3 - i`; the port is plain big-endian hex.
fn parse_proc_addr(field: &str) -> Option<(std::net::Ipv4Addr, u16)> {
    let (addr_hex, port_hex) = field.split_once(':')?;
    if addr_hex.len() != 8 {
        return None;
    }
    let mut octets = [0u8; 4];
    for i in 0..4 {
        let pair = &addr_hex[(3 - i) * 2..(3 - i) * 2 + 2];
        octets[i] = u8::from_str_radix(pair, 16).ok()?;
    }
    let port = u16::from_str_radix(port_hex, 16).ok()?;
    Some((std::net::Ipv4Addr::from(octets), port))
}

/// A minimal read-only loopback HTTP listener whose ROUTING is the caller's (#98 §6.6:
/// "the listener takes a handler"). `port` 0 picks an ephemeral port; a host wanting a
/// stable address passes its own. Returns the bound port; the accept loop runs on a
/// detached thread (dies with the process). One listener implementation for `--html` and
/// every host, so a header fix lands everywhere at once.
/// A route handler: `(path, query) -> reply`. Shared by the listener and any host chaining
/// its own routes in front of [`service_routes`].
pub type RouteHandler = std::sync::Arc<dyn Fn(&Request) -> HttpResponse + Send + Sync>;

pub fn spawn_listener(port: u16, handler: RouteHandler) -> Result<u16> {
    spawn_listener_gated(port, handler, AuthGate::same_user())
}

/// [`spawn_listener`] with an explicit access gate (#196 D3b). `spawn_listener` is this
/// with the same-user gate — the safe default every server wants on a shared machine.
pub fn spawn_listener_gated(port: u16, handler: RouteHandler, gate: AuthGate) -> Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", port)).context("bind loopback HTTP server")?;
    let port = listener.local_addr()?.port();
    let gate = std::sync::Arc::new(gate);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let _ = serve_connection(stream, &*handler, &gate);
            });
        }
    });
    Ok(port)
}

/// Parse a `k=v&…` query string value for `key` (already past the `?`).
pub fn query_get<'a>(query: &'a str, key: &str) -> Option<&'a str> {
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

/// The `cmauth` cookie's max age — the browser cap (400 days). A SESSION cookie would die
/// with the browser and the owner's plain bookmark would 401 the next morning, which reads
/// as "the monitor broke".
const COOKIE_MAX_AGE_SECS: u64 = 400 * 24 * 3600;

/// The largest POST body the server will read (#133): a prompt, not a payload.
const MAX_BODY_BYTES: usize = 64 * 1024;

fn serve_connection(
    mut stream: std::net::TcpStream,
    handler: &(dyn Fn(&Request) -> HttpResponse + Send + Sync),
    gate: &AuthGate,
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Read, Write};
    // Read the request line + headers (bounded — a monitor request is tiny; an 8 KiB cap
    // keeps a hostile peer from streaming forever).
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut headers = String::new();
    loop {
        let mut h = String::new();
        let n = reader.read_line(&mut h)?;
        if n == 0 || h == "\r\n" || h == "\n" || headers.len() > 8192 {
            break;
        }
        headers.push_str(&h);
    }
    // `METHOD /name?query HTTP/1.1`
    let method = line.split_whitespace().next().unwrap_or("GET").to_string();
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path_part, query) = target.split_once('?').unwrap_or((target, ""));
    let name = path_part.trim_start_matches('/');

    // The POST body, bounded — a write route (#133) reads its prompt/target from here.
    let body_len = header_value(&headers, "content-length")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(MAX_BODY_BYTES);
    let mut body = vec![0u8; body_len];
    if body_len > 0 {
        reader.read_exact(&mut body)?;
    }

    // #196 §4.2: same-user OR a valid token (query / Authorization: Bearer / cmauth cookie).
    let (presented, from_cookie) = extract_token(query, &headers);
    let peer = stream.peer_addr().ok();
    let local = stream.local_addr().ok();
    let access = match (peer, local) {
        (Some(p), Some(l)) => gate.decide(p, l, presented.as_deref(), from_cookie),
        // No socket identity to check — deny unless a token was presented and matches.
        _ => match presented.as_deref() {
            Some(_)
                if matches!(
                    gate.decide(
                        ([127, 0, 0, 1], 0).into(),
                        ([127, 0, 0, 1], 0).into(),
                        presented.as_deref(),
                        from_cookie
                    ),
                    Access::OkSetCookie | Access::Ok,
                ) =>
            {
                Access::OkSetCookie
            }
            _ => Access::Denied,
        },
    };

    // The write-route verdicts (#133): a valid TOKEN was presented (not merely same-user),
    // and the Host/Origin are ours. `deny_write` on the Request enforces both + POST.
    let local_port = local.map(|l| l.port()).unwrap_or(0);
    let req = Request {
        method: &method,
        name,
        query,
        body: &body,
        authenticated: gate.token_ok(presented.as_deref()),
        origin_ok: origin_ok(&headers, local_port),
    };

    let (r, set_cookie, redirect_root) = match access {
        Access::Denied => (
            // A 401 is a well-formed reply: the fleet's `status_code` probe reads a gated
            // remote monitor as "serving" (and its own tunnel passes same-user anyway).
            HttpResponse::unauthorized(
                "not paired — run `claude-monitor --pair` and open the printed URL",
            ),
            None,
            false,
        ),
        Access::Ok => (handler(&req), None, false),
        Access::OkSetCookie => {
            let tok = presented.clone().unwrap_or_default();
            // A ROOT navigation that admitted via a URL token gets a one-time 302 to the
            // bare path (+ cookie): the token never lingers in the address bar or history,
            // and NO page JS changes. This is a page-load redirect, NOT the pull-loop
            // redirect the design forbids — a fresh GET, not a cursor'd stream.
            let root_nav = name.is_empty() || name == "index.html" || name == "index";
            if root_nav && !from_cookie {
                (HttpResponse::ok("text/plain", Vec::new()), Some(tok), true)
            } else {
                (handler(&req), Some(tok), false)
            }
        }
    };

    let cookie = set_cookie
        .map(|t| {
            format!(
                "Set-Cookie: cmauth={t}; Path=/; Max-Age={COOKIE_MAX_AGE_SECS}; \
                 HttpOnly; SameSite=Strict\r\n"
            )
        })
        .unwrap_or_default();
    let head = if redirect_root {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: /\r\n{cookie}\
             Content-Length: 0\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        )
    } else {
        format!(
            "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{cookie}\
             Cache-Control: no-store\r\nConnection: close\r\n\r\n",
            r.code,
            r.content_type,
            r.body.len()
        )
    };
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(&r.body))
}

/// Pull the bearer token from a request: `?token=` (query), `Authorization: Bearer`, or the
/// `cmauth` cookie — in that precedence. Returns `(token, came_from_cookie)`.
fn extract_token(query: &str, headers: &str) -> (Option<String>, bool) {
    if let Some(t) = query_get(query, "token") {
        return (Some(percent_decode(t)), false);
    }
    for raw in headers.lines() {
        if let Some(v) = raw
            .strip_prefix("Authorization:")
            .or_else(|| raw.strip_prefix("authorization:"))
        {
            if let Some(bearer) = v.trim().strip_prefix("Bearer ") {
                return (Some(bearer.trim().to_string()), false);
            }
        }
        if let Some(v) = raw
            .strip_prefix("Cookie:")
            .or_else(|| raw.strip_prefix("cookie:"))
        {
            for kv in v.split(';') {
                if let Some((k, val)) = kv.split_once('=') {
                    if k.trim() == "cmauth" {
                        return (Some(val.trim().to_string()), true);
                    }
                }
            }
        }
    }
    (None, false)
}

/// The session service's wire surface as a ROUTE TABLE (#98 §6.3): `/session`, `/pull`,
/// `/records`, `/__reveal`, then static files out of `static_dir`. Returns a plain
/// [`HttpResponse`], so a host chains its own routes in front and falls back to this —
/// `--html`'s listener is exactly that with zero routes of its own.
pub fn service_routes(
    live: Option<&SessionService>,
    static_dir: &Path,
    name: &str,
    query: &str,
) -> HttpResponse {
    // `/session?id=<sid>[&chrome=embed][&theme=light|dark]` — the complete session view at
    // a URL, exactly as `--html` serves it, with optional host chrome (#98 §6.3).
    if name == "session" {
        let Some(live) = live else {
            return HttpResponse::not_found("no live server");
        };
        // Accept BOTH `id` (the documented monitor API, §6.3) and `session` — the served
        // page navigates BETWEEN sessions with a relative `?session=<child>` href
        // (export.js), so a sub-agent click on a page served at `/session?id=X` becomes
        // `/session?session=<child>`. Reading only `id` 404'd every drill-down under the
        // monitor (it worked under `--html`, whose shell lives at `index.html?session=`).
        let id = query_get(query, "id")
            .or_else(|| query_get(query, "session"))
            .unwrap_or("");
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return HttpResponse::not_found("no such session");
        }
        let chrome = PageChrome {
            embed: query_get(query, "chrome") == Some("embed"),
            theme: query_get(query, "theme").map(str::to_string),
        };
        let chrome = (chrome.embed || chrome.theme.is_some()).then_some(chrome);
        return match live.page(id, chrome.as_ref()) {
            Some(page) => HttpResponse::html(page),
            None => HttpResponse::not_found("no such session"),
        };
    }
    // `/pull?session=<id>&cursor=<epoch.committed.gen.index>` — the pull-client feed. Materialize
    // the id on first pull, borrow this thread to tail it, and return the self-describing PullReply
    // JSON (committed append + provisional truncate/extend). Costs nothing when no client pulls.
    if name == "pull" {
        let Some(live) = live else {
            return HttpResponse::not_found("no live server");
        };
        let id = query_get(query, "session").unwrap_or("");
        let cursor = Cursor::from_query(query_get(query, "cursor").unwrap_or(""));
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return HttpResponse::not_found("no such agent");
        }
        return live.pull_response(id, cursor);
    }
    // `/records?session=<id>&from=<off>&len=<n>&epoch=<e>` — the committed range read backing a
    // pull reply's `committed_ext` pointer. 409 on a stale epoch (the log was recreated by a
    // reset since the pointer was issued) — the client drops the reply and re-pulls.
    if name == "records" {
        let Some(live) = live else {
            return HttpResponse::not_found("no live server");
        };
        let id = query_get(query, "session").unwrap_or("");
        let num = |k| {
            query_get(query, k)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0)
        };
        if id.is_empty() || id.contains('/') || id.contains("..") {
            return HttpResponse::not_found("no such agent");
        }
        return match live.records_bytes(id, num("from"), num("len"), num("epoch")) {
            Ok(bytes) => HttpResponse::ok("application/json; charset=utf-8", bytes),
            Err(StaleEpoch) => HttpResponse {
                code: "409 Conflict",
                content_type: "text/plain",
                body: b"stale epoch".to_vec(),
            },
        };
    }
    // `/__reveal?path=<url-encoded abs path>` — reveal a file in the OS file manager (the
    // served page can't follow a `file://` link: browsers block http→file navigation).
    if name == "__reveal" {
        if let Some(v) = query_get(query, "path") {
            let p = percent_decode(v);
            let path = Path::new(&p);
            if path.exists() {
                crate::sys::reveal_in_file_manager(path);
                return HttpResponse::ok("text/plain", b"revealed".to_vec());
            }
        }
        return HttpResponse::not_found("no such path");
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
        return HttpResponse {
            code: "403 Forbidden",
            content_type: "text/plain",
            body: b"forbidden".to_vec(),
        };
    }
    match std::fs::read(static_dir.join(name)) {
        Ok(bytes) => {
            let ct = if name.ends_with(".html") {
                "text/html; charset=utf-8"
            } else if name.ends_with(".jsonl") || name.ends_with(".json") {
                "application/json; charset=utf-8"
            } else {
                "application/octet-stream"
            };
            HttpResponse::ok(ct, bytes)
        }
        Err(_) => HttpResponse::not_found("not found"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    /// #196 D3b: the `/proc/net/tcp` matcher finds the connecting user by the client
    /// socket's row (its `local_address` is the connection's peer end, `rem_address` our
    /// listener end), and decodes the little-endian address hex + big-endian port hex
    /// correctly. A row that does not match returns nothing.
    #[test]
    fn proc_net_tcp_matcher_reads_the_peer_uid() {
        // 127.0.0.1 = octets [127,0,0,1] → memory-order hex 0100007F; ports big-endian.
        // Peer (client) 127.0.0.1:54321 (0xD431), listener 127.0.0.1:2727 (0x0AA7), uid 501.
        let contents = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:D431 0100007F:0AA7 01 00000000:00000000 00:00000000 00000000   501        0 0\n\
   1: 0100007F:1234 0100007F:5678 01 00000000:00000000 00:00000000 00000000  1000        0 0\n";
        let lo = Ipv4Addr::LOCALHOST;
        assert_eq!(
            find_peer_uid(contents, lo, 54321, lo, 2727),
            Some(501),
            "the client row's uid, matched on peer=local / listener=rem"
        );
        // The reversed tuple (listener as peer) must NOT match — direction matters.
        assert_eq!(find_peer_uid(contents, lo, 2727, lo, 54321), None);
        // A tuple absent from the table is unknown, not a wrong uid.
        assert_eq!(find_peer_uid(contents, lo, 9999, lo, 2727), None);
    }

    /// The address/port hex decode in isolation — the endianness that bites.
    #[test]
    fn proc_addr_decodes_endianness() {
        assert_eq!(
            parse_proc_addr("0100007F:0AA7"),
            Some((Ipv4Addr::LOCALHOST, 2727))
        );
        assert_eq!(
            parse_proc_addr("0101A8C0:1F90"), // 192.168.1.1:8080
            Some((Ipv4Addr::new(192, 168, 1, 1), 8080))
        );
        assert_eq!(parse_proc_addr("short:1"), None);
    }

    /// §4.2: with a FOREIGN euid (so the same-user leg is off), the token decides.
    /// No token presented → denied; the right token → admitted (and Set-Cookie when it
    /// came by URL/header, plain OK when it was already a cookie); a wrong token → denied.
    /// Cross-user is exactly a foreign-euid peer, so this IS the shared-Mac deny path.
    #[test]
    fn the_token_gate_admits_only_the_right_token() {
        let lo = std::net::SocketAddr::from(([127, 0, 0, 1], 5000));
        let srv = std::net::SocketAddr::from(([127, 0, 0, 1], 2727));
        let gate = AuthGate::for_test(Some("secret-abc"));
        assert!(matches!(gate.decide(lo, srv, None, false), Access::Denied));
        assert!(matches!(
            gate.decide(lo, srv, Some("nope"), false),
            Access::Denied
        ));
        assert!(matches!(
            gate.decide(lo, srv, Some("secret-abc"), false),
            Access::OkSetCookie
        ));
        assert!(matches!(
            gate.decide(lo, srv, Some("secret-abc"), true),
            Access::Ok
        ));
        // A near-miss (same length, one byte off) is refused — the constant-time compare.
        assert!(matches!(
            gate.decide(lo, srv, Some("secret-abd"), false),
            Access::Denied
        ));
        // Unpaired with a foreign euid on unverifiable-loopback would admit (same-machine);
        // but a foreign euid on VERIFIABLE loopback (our matcher returns a uid) denies.
        // Here the matcher finds no /proc row → unverifiable → unpaired admits, paired denies.
        let unpaired = AuthGate::for_test(None);
        assert!(matches!(unpaired.decide(lo, srv, None, false), Access::Ok));
    }

    /// #133 wire hardening: `deny_write` gates a write on POST + same-origin + a real
    /// token, in that order (the reply names the first failing condition). A read request
    /// (GET, no token) is refused for a write even from the same user — writing requires
    /// authentication, not just loopback.
    #[test]
    fn deny_write_requires_post_same_origin_and_a_token() {
        let mk = |method, authenticated, origin_ok| Request {
            method,
            name: "api/compose",
            query: "",
            body: b"",
            authenticated,
            origin_ok,
        };
        // Wrong method → 405, before anything else is even checked.
        assert_eq!(
            mk("GET", true, true).deny_write().unwrap().code,
            "405 Method Not Allowed"
        );
        // Foreign origin → 403.
        assert_eq!(
            mk("POST", true, false).deny_write().unwrap().code,
            "403 Forbidden"
        );
        // Same-origin POST but not authenticated (same-user read, no token) → 401.
        assert_eq!(
            mk("POST", false, true).deny_write().unwrap().code,
            "401 Unauthorized"
        );
        // All three satisfied → proceed.
        assert!(mk("POST", true, true).deny_write().is_none());
    }

    /// The Origin/Host allowlist (§3.2): our own loopback origin (or absent) passes; a
    /// foreign Host (DNS rebinding) or Origin (cross-site fetch) is refused.
    #[test]
    fn origin_allowlist_admits_ours_refuses_foreign() {
        assert!(origin_ok("Host: 127.0.0.1:2727\r\n", 2727), "our host");
        assert!(
            origin_ok("Host: localhost:2727\r\n", 2727),
            "localhost alias"
        );
        assert!(
            origin_ok(
                "Host: 127.0.0.1:2727\r\nOrigin: http://127.0.0.1:2727\r\n",
                2727
            ),
            "our host + our origin (a same-origin POST)"
        );
        assert!(origin_ok("", 2727), "absent headers rely on the token");
        // DNS rebinding: the page is evil.com (rebound to 127.0.0.1), Host says evil.com.
        assert!(!origin_ok("Host: evil.com:2727\r\n", 2727), "foreign host");
        // Cross-site fetch: Host is ours, but the initiating page's Origin is evil.com.
        assert!(
            !origin_ok("Host: 127.0.0.1:2727\r\nOrigin: http://evil.com\r\n", 2727),
            "foreign origin"
        );
        // Right host, wrong port — a different local service, not us.
        assert!(!origin_ok("Host: 127.0.0.1:9999\r\n", 2727), "wrong port");
    }

    /// `extract_token` reads all three carriers with the right precedence and parses a
    /// `Cookie:` header without matching a look-alike key (`xcmauth`).
    #[test]
    fn extract_token_reads_query_header_and_cookie() {
        assert_eq!(extract_token("token=q1", "").0.as_deref(), Some("q1"));
        assert_eq!(
            extract_token("", "Authorization: Bearer h1\r\n")
                .0
                .as_deref(),
            Some("h1")
        );
        let (t, from_cookie) = extract_token("", "Cookie: foo=1; cmauth=c1; bar=2\r\n");
        assert_eq!((t.as_deref(), from_cookie), (Some("c1"), true));
        // A cookie whose name merely ends in cmauth must not match.
        assert_eq!(extract_token("", "Cookie: xcmauth=nope\r\n").0, None);
    }

    /// The gate admits our OWN loopback connection (this test process is the peer and the
    /// listener) and answers a bound gated listener with 200 — the same-user path,
    /// end to end, on whatever platform CI runs. (The cross-user DENY path cannot be
    /// exercised without a second uid; it is covered by the matcher tests above.)
    #[test]
    fn a_gated_listener_admits_its_own_user() {
        use std::io::{Read, Write};
        let handler: RouteHandler =
            std::sync::Arc::new(|_req: &Request| HttpResponse::ok("text/plain", b"ok".to_vec()));
        let port = spawn_listener_gated(0, handler, AuthGate::same_user()).unwrap();
        let mut s = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        s.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .unwrap();
        let mut resp = String::new();
        s.read_to_string(&mut resp).unwrap();
        assert!(
            resp.starts_with("HTTP/1.1 200 OK"),
            "same-user connection is admitted:\n{resp}"
        );
        assert!(resp.trim_end().ends_with("ok"));
    }

    /// `pull_reply_json` splices the provisional records inline (no re-parse) and carries the
    /// committed zone as a `{offset, len}` pointer into the on-disk record log (`null` when
    /// empty) — the Part-2 wire the client range-reads via `/records`.
    #[test]
    fn pull_reply_json_carries_pointer_and_spliced_provisional() {
        let meta = json!({ "t": "meta" });
        let s = pull_reply_json(5, 3, 2, Some((128, 4096)), 1, &[r#"{"id":"p"}"#], &meta);
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["epoch"], 5);
        assert_eq!(v["committed_from"], 2);
        assert_eq!(v["committed_ext"]["offset"], 128);
        assert_eq!(v["committed_ext"]["len"], 4096);
        assert_eq!(v["provisional_gen"], 3);
        assert_eq!(v["provisional_from"], 1);
        assert_eq!(v["provisional"].as_array().unwrap().len(), 1);
        assert_eq!(v["provisional"][0]["id"], "p");
        assert_eq!(v["meta"]["t"], "meta");

        // No committed delta (None or zero-length) ⇒ a null pointer, valid empty provisional.
        let v: Value =
            serde_json::from_str(&pull_reply_json(1, 0, 0, None, 0, &[], &meta)).unwrap();
        assert!(v["committed_ext"].is_null());
        assert_eq!(v["provisional"].as_array().unwrap().len(), 0);
        let v: Value =
            serde_json::from_str(&pull_reply_json(1, 0, 3, Some((9, 0)), 0, &[], &meta)).unwrap();
        assert!(v["committed_ext"].is_null(), "zero-length ⇒ null pointer");
    }

    /// A FAITHFUL port of the browser client's `consumePull` (src/html/export.js) — every rule,
    /// in the same order: the idle early-return, the epoch resync, the committed
    /// truncate-then-append, the provisional truncate-then-extend, and the cursor adoption.
    /// `dom` stands in for the `#stream` children (one entry per rendered block record).
    /// Drift between this port and the JS is itself a bug — keep them in lockstep.
    #[derive(Default)]
    struct SimClient {
        epoch: u64,
        committed: usize,
        gen: u64,
        index: usize,
        dom: Vec<Value>,
    }

    impl SimClient {
        fn cursor(&self) -> Cursor {
            Cursor {
                epoch: self.epoch,
                committed_id: self.committed,
                provisional_gen: self.gen,
                provisional_index: self.index,
            }
        }
        /// Apply one reply (`committed` already materialized from the `/records` range read,
        /// exactly as the browser driver does before calling consumePull).
        fn consume(&mut self, r: &Value, committed: &[Value]) {
            let provisional = r["provisional"].as_array().expect("provisional array");
            let repoch = r["epoch"].as_u64().expect("epoch");
            if repoch == self.epoch && committed.is_empty() && provisional.is_empty() {
                return; // idle tick
            }
            if repoch != self.epoch {
                self.dom.clear(); // resetFrom(0)
                self.committed = 0;
            }
            if !committed.is_empty() {
                let cf = r["committed_from"].as_u64().expect("committed_from") as usize;
                self.dom.truncate(cf);
                self.committed = cf;
                for b in committed {
                    self.dom.push(b.clone());
                    self.committed += 1;
                }
            }
            let pf = r["provisional_from"].as_u64().expect("provisional_from") as usize;
            self.dom.truncate(self.committed + pf);
            for b in provisional {
                self.dom.push(b.clone());
            }
            self.epoch = repoch;
            self.gen = r["provisional_gen"].as_u64().expect("gen");
            self.index = pf + provisional.len();
        }
        /// One full client poll against the live server: pull, then (phase two) range-read the
        /// committed pointer; a failed/stale range read drops the whole reply, like the browser.
        fn poll(&mut self, live: &SessionService, id: &str) {
            let reply = body(live.pull_response(id, self.cursor()));
            let r: Value = serde_json::from_str(&reply).expect("valid reply JSON");
            let committed: Vec<Value> = match &r["committed_ext"] {
                Value::Null => Vec::new(),
                ext => {
                    let (from, len, epoch) = (
                        ext["offset"].as_u64().unwrap(),
                        ext["len"].as_u64().unwrap(),
                        r["epoch"].as_u64().unwrap(),
                    );
                    match live.records_bytes(id, from, len, epoch) {
                        Err(StaleEpoch) => return, // 409: drop the reply whole; re-pull next tick
                        Ok(bytes) => std::str::from_utf8(&bytes)
                            .expect("utf8 records")
                            .lines()
                            .filter(|l| !l.is_empty())
                            .map(|l| serde_json::from_str(l).expect("valid record"))
                            .collect(),
                    }
                }
            };
            self.consume(&r, &committed);
        }
    }

    /// THE live-client invariant (#54): a long-lived incremental client's DOM must equal, after
    /// EVERY poll, the DOM a freshly-attached client (a page reload) builds from the same server
    /// — the user's "reloading fixes the duplicate" observation, promoted to the oracle. Drives
    /// the real pull_response/records_bytes through appends, a back-patch, activity grouping, a
    /// **The rendezvous probe** (#96): a second invocation finds the running server through the
    /// lock instead of standing up a duplicate. Both signals are required and each is tested for
    /// its own reason — a pid alone is not enough because pids are recycled, a port alone is not
    /// enough because another program may hold it, and a holder with no note has bound nothing to
    /// redirect to yet.
    #[test]
    fn existing_server_is_found_only_when_the_holder_is_alive_and_answering() {
        use crate::cache::{admit, lock, Holder, Presentation};

        let root = std::env::temp_dir().join(format!("cr-rdv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = admit::entry_dir(&root, Presentation::Html, "sid");
        std::fs::create_dir_all(&dir).unwrap();

        // A real listening port and a real live pid that is not ours.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let alive_pid = child.id();

        let write = |pid: u32, note: Option<HtmlNote>| {
            std::fs::write(
                lock::lock_path(&dir),
                serde_json::to_string(&Holder {
                    pid,
                    dir: dir.clone(),
                    note,
                })
                .unwrap(),
            )
            .unwrap();
        };

        write(alive_pid, Some(HtmlNote { port }));
        assert_eq!(
            existing_server(Some(&root), "sid"),
            Some(port),
            "a live holder that is answering IS the hand-off target"
        );

        write(alive_pid, None);
        assert_eq!(
            existing_server(Some(&root), "sid"),
            None,
            "a holder that has not bound yet has nothing to redirect to"
        );

        write(std::process::id(), Some(HtmlNote { port }));
        assert_eq!(
            existing_server(Some(&root), "sid"),
            None,
            "our own lock is not a hand-off target"
        );

        // A dead holder: the port may even still be open (another program), so the pid check is
        // what has to reject this.
        child.kill().ok();
        child.wait().ok();
        write(alive_pid, Some(HtmlNote { port }));
        assert_eq!(
            existing_server(Some(&root), "sid"),
            None,
            "a dead holder is not a hand-off target, whoever holds the port now"
        );

        // A closed port with a live pid: the port probe is what has to reject this.
        let mut child2 = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        drop(listener);
        write(child2.id(), Some(HtmlNote { port }));
        assert_eq!(
            existing_server(Some(&root), "sid"),
            None,
            "a live pid that stopped answering is not a hand-off target"
        );
        child2.kill().ok();
        child2.wait().ok();

        assert_eq!(
            existing_server(None, "sid"),
            None,
            "no cache root, no target"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **The lock's note must name where we serve.** A peer that finds the lock held reads it to
    /// redirect; a `null` note leaves it nowhere to go.
    ///
    /// The bug this pins was one of ordering: the note used to be published right after the
    /// listener bound, but sessions are admitted lazily on their first `/pull`, so at that moment
    /// the process owned nothing and the publish silently did nothing. It is now written when the
    /// entry is admitted — the first moment the lock is ours AND the port is known.
    #[test]
    fn the_lock_note_carries_the_serving_port_once_a_session_is_admitted() {
        use crate::cache::{admit, lock, Presentation};
        use crate::engine::meta_stream::Versions;
        use crate::{SessionCache, Transcript};

        let base = std::env::temp_dir().join(format!("cr-note-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("nid.jsonl");
        let bundle = base.join("bundle");
        let root = base.join("cache"); // never the developer's real cache
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(
            &sess,
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n",
        )
        .unwrap();

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "nid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: SessionCache::durable(
                Presentation::Html,
                root.clone(),
                Versions::current(Some(1)),
            ),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("nid", Transcript::open(Agent::CLAUDE, sess.clone()));
        let _ = live.port.set(4321);

        let entry = admit::entry_dir(&root, Presentation::Html, "nid");
        assert!(
            lock::read::<HtmlNote>(&entry).is_none(),
            "nothing is owned before the first pull"
        );

        let _ = live.pull_response("nid", Cursor::default());

        let held = lock::read::<HtmlNote>(&entry).expect("the pull admitted and locked it");
        assert_eq!(held.pid, std::process::id());
        assert_eq!(
            held.note.expect("the note must not be null").port,
            4321,
            "a peer reads this to redirect instead of standing up its own server"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// #163: a session a live peer owns is ROUTED, never served a second time.
    ///
    /// The pull answers with a `redirect` record the client acts on by NAVIGATING — deliberately
    /// not an HTTP 302, which `fetch` would follow transparently, leaving the page pulling
    /// another server with a cursor minted against this one's record stream. And when the holder
    /// has taken the lock but not yet published a port there is nowhere to send anyone, so the
    /// reply says that instead of inventing a target or quietly opening a second copy.
    #[test]
    fn a_session_a_peer_holds_is_redirected_not_served() {
        use crate::cache::{admit, lock};
        use crate::engine::meta_stream::Versions;
        use crate::{SessionCache, Transcript};

        let base = std::env::temp_dir().join(format!("cr-redirect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sid.jsonl");
        let root = base.join("cache"); // never the developer's real cache
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            &sess,
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n",
        )
        .unwrap();

        // A peer that really exists: a live pid that is not ours, and a port that really answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let peer_port = listener.local_addr().unwrap().port();
        let mut peer = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();

        let service = |port: u16| SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: SessionCache::durable(
                Presentation::Html,
                root.clone(),
                Versions::current(Some(render_flavor(&FoldPolicy::default()))),
            ),
            port: {
                let c = std::sync::OnceLock::new();
                let _ = c.set(port);
                c
            },
        };
        let entry = admit::entry_dir(&root, Presentation::Html, "sid");
        let hold = |note: Option<HtmlNote>| {
            std::fs::create_dir_all(&entry).unwrap();
            std::fs::write(
                lock::lock_path(&entry),
                serde_json::to_string(&Holder {
                    pid: peer.id(),
                    dir: entry.clone(),
                    note,
                })
                .unwrap(),
            )
            .unwrap();
        };

        hold(Some(HtmlNote { port: peer_port }));
        let live = service(9999);
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));
        let r = live.pull_response("sid", Cursor::default());
        assert_eq!(r.code, "200 OK", "a hand-off is an answer, not a failure");
        let v: Value = serde_json::from_str(&body(r)).unwrap();
        assert_eq!(v["t"], json!("redirect"), "held ⇒ route, never serve: {v}");
        assert_eq!(v["url"], json!(handoff_url(peer_port, "sid")));
        assert!(
            !entry.join("records.jsonl").exists(),
            "and nothing of ours was written into the peer's entry"
        );
        drop(live);

        // Same holder, no published port: a real window (it took the lock before it bound).
        hold(None);
        let live = service(9999);
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));
        let r = live.pull_response("sid", Cursor::default());
        assert_eq!(
            r.code, "404 Not Found",
            "`/pull` is a wire surface: a session that cannot be served is not a 200"
        );
        let v: Value = serde_json::from_str(&body(r)).unwrap();
        assert_eq!(v["t"], json!("error"), "nowhere to send anyone: {v}");
        let msg = v["message"].as_str().unwrap_or_default();
        assert!(
            msg.contains(&peer.id().to_string()),
            "the reason names the holder: {msg:?}"
        );
        drop(live);

        // THE SELF-GUARD. The note names OUR port — which a recycled pid plus our own listener
        // would produce — so it is not evidence of a peer. Take the entry; redirecting here would
        // send the page to the server it is already talking to.
        hold(Some(HtmlNote { port: peer_port }));
        let live = service(peer_port);
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));
        let v: Value =
            serde_json::from_str(&body(live.pull_response("sid", Cursor::default()))).unwrap();
        assert_eq!(
            v["t"],
            json!("pull"),
            "a note naming our own port is us: {v}"
        );
        assert_eq!(
            lock::read::<HtmlNote>(&entry).expect("locked").pid,
            std::process::id(),
            "and the entry is now genuinely ours"
        );

        peer.kill().ok();
        peer.wait().ok();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// sub-agent spawn + async completion (queue-op), commits, plus a lagging second client
    /// (missed ticks) and an interleaved third client (a second tab).
    #[test]
    fn incremental_client_always_equals_a_fresh_reload() {
        use crate::Transcript;
        let base = std::env::temp_dir().join(format!("cr-sim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sid.jsonl");
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(&sess, "").unwrap();

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: test_cache(&base),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));

        // A live session's growth, one appended chunk per tick: turns that commit, a tool call
        // whose result back-patches, activity runs that regroup, a spawn whose async completion
        // arrives via a queue-op notification, and a queued prompt that dequeues immediately.
        let chunks: Vec<String> = vec![
            r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-07-26T10:00:00Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b1","name":"Bash","input":{"command":"ls"}}]},"timestamp":"2026-07-26T10:00:01Z"}"#.into(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"b1","content":"out"}]},"timestamp":"2026-07-26T10:00:02Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"b2","name":"Grep","input":{"pattern":"x"}}]},"timestamp":"2026-07-26T10:00:03Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hm"},{"type":"text","text":"done part 1"}]},"timestamp":"2026-07-26T10:00:04Z"}"#.into(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"spawn one"}]},"timestamp":"2026-07-26T10:00:05Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_A","name":"Task","input":{"subagent_type":"gp","description":"child","prompt":"go"}}]},"timestamp":"2026-07-26T10:00:06Z"}"#.into(),
            r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"async_launched","outputFile":"/t/a.out"},"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"async_launched"}]},"timestamp":"2026-07-26T10:00:07Z"}"#.into(),
            "{\"type\":\"queue-operation\",\"operation\":\"enqueue\",\"content\":\"<task-notification>\\n<task-id>achild01</task-id>\\n<tool-use-id>toolu_A</tool-use-id>\\n<status>completed</status>\\n<summary>done</summary>\\n<result>ok</result>\\n</task-notification>\"}".into(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next question"}]},"timestamp":"2026-07-26T10:00:08Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"answer"}]},"timestamp":"2026-07-26T10:00:09Z"}"#.into(),
            r#"{"type":"queue-operation","operation":"enqueue","content":"typed ahead"}"#.into(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"typed ahead"}]},"timestamp":"2026-07-26T10:00:10Z"}"#.into(),
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"final"}]},"timestamp":"2026-07-26T10:00:11Z"}"#.into(),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"one more"}]},"timestamp":"2026-07-26T10:00:12Z"}"#.into(),
        ];

        let mut steady = SimClient::default(); // polls every tick
        let mut lagging = SimClient::default(); // polls every 3rd tick (missed intervals)
        let mut other_tab = SimClient::default(); // interleaved second client
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&sess)
            .unwrap();
        for (i, chunk) in chunks.iter().enumerate() {
            use std::io::Write;
            writeln!(file, "{chunk}").unwrap();
            file.flush().unwrap();

            steady.poll(&live, "sid");
            if i % 2 == 0 {
                other_tab.poll(&live, "sid"); // a second tab pulling on its own rhythm
            }
            if i % 3 == 2 {
                lagging.poll(&live, "sid");
            }

            // THE ORACLE: a fresh client (a reload) built from the same server state right now.
            let mut fresh = SimClient::default();
            fresh.poll(&live, "sid");
            assert_eq!(
                steady.dom, fresh.dom,
                "tick {i}: steady client diverged from a fresh reload"
            );
            if i % 2 == 0 {
                assert_eq!(
                    other_tab.dom, fresh.dom,
                    "tick {i}: second-tab client diverged from a fresh reload"
                );
            }
            if i % 3 == 2 {
                assert_eq!(
                    lagging.dom, fresh.dom,
                    "tick {i}: lagging client diverged from a fresh reload"
                );
            }
            // And no duplicates by construction: consecutive user turns must have distinct ids.
            let ids: Vec<&str> = steady.dom.iter().filter_map(|b| b["id"].as_str()).collect();
            let mut seen = std::collections::HashSet::new();
            for id in &ids {
                assert!(seen.insert(*id), "tick {i}: duplicate block id {id} in DOM");
            }
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Part 2 end-to-end (no HTTP): a pull whose reply carries a `committed_ext` pointer, the
    /// `/records` range read materializing exactly the committed records, the applied cursor
    /// round-tripping to an idle re-pull, and the stale-epoch 409 path.
    #[test]
    fn pull_committed_pointer_round_trips_via_records() {
        use crate::cache::Cursor;
        use crate::Transcript;
        let base = std::env::temp_dir().join(format!("cr-serve-p2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sid.jsonl");
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(&sess, concat!(
            r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-07-26T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"done"}]},"timestamp":"2026-07-26T10:00:01Z"}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"next"}]},"timestamp":"2026-07-26T10:00:02Z"}"#, "\n",
        )).unwrap();

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: test_cache(&base),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));

        // Fresh cursor: turn 1 committed (the second user turn opened turn 2) ⇒ the reply carries
        // a pointer, not inline committed records.
        let reply = body(live.pull_response("sid", Cursor::default()));
        let v: Value = serde_json::from_str(&reply).unwrap();
        let ext = &v["committed_ext"];
        assert!(!ext.is_null(), "committed delta ⇒ pointer present");
        let (from, len) = (
            ext["offset"].as_u64().unwrap(),
            ext["len"].as_u64().unwrap(),
        );
        let epoch = v["epoch"].as_u64().unwrap();

        // Phase two: the range read materializes exactly the committed records, in order.
        let bytes = live
            .records_bytes("sid", from, len, epoch)
            .expect("current epoch serves");
        let recs: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(!recs.is_empty(), "committed turn 1 delivered");
        assert_eq!(recs[0]["id"], "t1", "first committed record is user turn 1");

        // The cursor a client derives from the materialized records + reply counts round-trips
        // to an idle re-pull (null pointer, empty provisional).
        let next = Cursor {
            epoch,
            committed_id: v["committed_from"].as_u64().unwrap() as usize + recs.len(),
            provisional_gen: v["provisional_gen"].as_u64().unwrap(),
            provisional_index: v["provisional_from"].as_u64().unwrap() as usize
                + v["provisional"].as_array().unwrap().len(),
        };
        let idle: Value = serde_json::from_str(&body(live.pull_response("sid", next))).unwrap();
        assert!(idle["committed_ext"].is_null(), "idle ⇒ null pointer");
        assert_eq!(idle["provisional"].as_array().unwrap().len(), 0);

        // A pointer issued before a reset must not read a recreated log: stale epoch ⇒ Err (409).
        assert!(live.records_bytes("sid", from, len, epoch + 1).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The child-nav inversion end-to-end (no HTTP): a parent's pull registers its child's
    /// SOURCE + a parent pointer only (no title write — the parent's pull touches no other
    /// session's presentation state); the child then derives its title/breadcrumb ONCE from the
    /// parent's maintained meta on ITS first resolve, and the result is cached.
    #[test]
    fn pull_registers_child_source_only_and_child_derives_title_lazily() {
        use crate::cache::Cursor;
        use crate::Transcript;
        let base = std::env::temp_dir().join(format!("cr-serve-inv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("proj").join("sid.jsonl");
        let sadir = base.join("proj").join("sid").join("subagents");
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&sadir).unwrap();
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(&sess, concat!(
            r#"{"type":"user","cwd":"/r","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-07-26T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"review the auth module","prompt":"go"}}]}}"#, "\n",
            r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}"#, "\n",
        )).unwrap();
        std::fs::write(
            sadir.join("agent-achild01.jsonl"),
            concat!(r#"{"type":"user","message":{"content":"go"}}"#, "\n"),
        )
        .unwrap();

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: test_cache(&base),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));
        live.cache.aux_with("sid", |a| {
            a.title = Some(TitleInfo {
                title: "root title".into(),
                ..Default::default()
            });
        });

        // Parent's pull: reply carries the child in its meta; the side effects are ONLY a source
        // registration + the parent pointer — no title write for the child.
        let reply = body(live.pull_response("sid", Cursor::default()));
        let v: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(v["meta"]["children"][0]["id"], "achild01");
        assert_eq!(v["meta"]["children"][0]["title"], "review the auth module");
        assert!(live.cache.is_registered("achild01"), "source registered");
        assert_eq!(
            live.cache
                .aux_with("achild01", |a| a.parent.clone())
                .as_deref(),
            Some("sid"),
            "parent pointer recorded"
        );
        assert!(
            live.cache.aux_with("achild01", |a| a.title.is_none()),
            "no cross-session title write from the parent's pull"
        );

        // Child's first resolve: title/breadcrumb derived once from the parent's maintained meta.
        let (_src, t) = live.resolve_id("achild01").expect("child resolves");
        assert_eq!(t.title, "review the auth module");
        assert_eq!(t.agent_type, "general-purpose");
        assert_eq!(
            t.ancestors,
            vec![("sid".to_string(), "root title".to_string())]
        );
        assert!(
            live.cache.aux_with("achild01", |a| a.title.is_some()),
            "derived once, then cached"
        );

        let _ = std::fs::remove_dir_all(&base);
    }

    /// `$CLAUDE_REPLAY_CACHE` is a PROCESS-global override and cargo runs these tests on
    /// parallel threads in one binary, so any test that points it somewhere holds this for the
    /// whole window. Without it, a test could scan — or write into — the developer's REAL cache
    /// home, which is the one thing the isolation rule forbids.
    static CACHE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A durable cache at the TEST's own root. Since #163 there is no cache-less path: an
    /// ephemeral cache denies every session and the server answers with the reason instead of a
    /// feed, so a test that expects to be served needs a real entry. Its own root, never the
    /// developer's — the isolation rule is the same one the suite has always had.
    fn test_cache(base: &Path) -> SessionCache<RecordStore, ServeAux> {
        SessionCache::durable(
            Presentation::Html,
            base.join("cache"),
            Versions::current(Some(render_flavor(&FoldPolicy::default()))),
        )
    }

    /// A reply's body as text — `/pull` answers with a status now (#163), and every test here
    /// is about what the body says.
    fn body(r: HttpResponse) -> String {
        String::from_utf8(r.body).expect("replies are utf-8 json")
    }

    /// Where `test_cache` puts `sid`'s record log.
    fn test_records(base: &Path, sid: &str) -> std::path::PathBuf {
        cache::admit::entry_dir(&base.join("cache"), Presentation::Html, sid).join("records.jsonl")
    }

    /// `start_server` end-to-end over real HTTP: several roots on ONE port, each answering
    /// `/pull` for its own `?session=`  — the server half of "stay on the picker, open a tab
    /// per session". Also pins where a `--no-cache` run puts things (#165): a real cache at its
    /// OWN root, and a per-run bundle dir, both under the cache home rather than `$TMPDIR`.
    #[test]
    fn start_server_hosts_every_root_on_one_port() {
        use clap::Parser as _;
        let _env = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!(
            "cr-start-multi-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // The cache home for this run — the API/env override the design keeps for exactly this
        // (§ Decisions). Everything `--no-cache` writes then lands under `base`, so the suite
        // touches neither the shared root nor a running viewer's locks.
        std::env::set_var("CLAUDE_REPLAY_CACHE", &base);
        let mut paths = Vec::new();
        for (n, text) in [("one", "first root"), ("two", "second root")] {
            let p = base.join(format!("{n}.jsonl"));
            std::fs::write(&p, format!(
                concat!(
                    r#"{{"type":"user","cwd":"/w","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"timestamp":"2026-08-01T10:00:00Z"}}"#, "\n",
                    r#"{{"type":"assistant","message":{{"role":"assistant","content":[{{"type":"text","text":"ok"}}]}},"timestamp":"2026-08-01T10:00:01Z"}}"#, "\n",
                    r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"text","text":"more"}}]}},"timestamp":"2026-08-01T10:00:02Z"}}"#, "\n",
                ),
                text
            )).unwrap();
            paths.push(p);
        }

        // `--no-cache` is a different ROOT, not the absence of one (#165).
        let args = crate::Args::parse_from(["claude-replay", "--html", "--no-cache"]);
        let server = start_server(&args, &paths).expect("server starts");
        assert_eq!(server.root_ids, vec!["one".to_string(), "two".to_string()]);
        assert_eq!(
            server.dir,
            base.join("runs").join(std::process::id().to_string()),
            "the bundle is a per-RUN directory under the cache home, not $TMPDIR"
        );
        // Distinct URLs, one port.
        let (u0, u1) = (
            server.url_for_root(0).unwrap(),
            server.url_for_root(1).unwrap(),
        );
        assert_ne!(u0, u1);
        assert!(u0.contains("?session=one") && u1.contains("?session=two"));

        // Each root really answers for itself, over the wire — and its records land in its own
        // durable entry under the run's PRIVATE cache root, which is what `--no-cache` now
        // selects. Nothing is written to the shared root, and nothing is served from a store
        // opened outside a cache entry.
        let private = base.join("throwaway").join(std::process::id().to_string());
        for (sid, want) in [("one", "first root"), ("two", "second root")] {
            let body = http_post(server.port, &format!("/pull?session={sid}&cursor=0"))
                .unwrap_or_else(|| panic!("{sid} pull"));
            let v: Value = serde_json::from_str(&body).unwrap();
            assert_eq!(v["meta"]["sid"], json!(sid));
            let entry = private.join("html").join(sid);
            let records =
                std::fs::read_to_string(entry.join("records.jsonl")).unwrap_or_else(|e| {
                    panic!("{sid} has a durable entry at {}: {e}", entry.display())
                });
            assert!(
                body.contains(want) || records.contains(want),
                "{sid} serves its own transcript"
            );
            assert!(
                entry.join("LOCK").exists(),
                "{sid} is locked like any other entry — a private cache is still a cache"
            );
            assert!(
                !base.join("sessions").exists(),
                "--no-cache never touches the shared root"
            );
        }
        drop(server); // release the entry locks before the root goes
        std::env::remove_var("CLAUDE_REPLAY_CACHE");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Minimal loopback HTTP POST for the test above (the server speaks HTTP/1.0-style
    /// close-delimited replies).
    fn http_post(port: u16, path: &str) -> Option<String> {
        use std::io::{Read, Write};
        let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).ok()?;
        write!(
            s,
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
        .ok()?;
        let mut raw = String::new();
        s.read_to_string(&mut raw).ok()?;
        raw.split_once("\r\n\r\n").map(|(_, body)| body.to_string())
    }

    /// The `-f --html` picker hosts EVERY discovered session on one server. Roots may be
    /// different agents in different directories, so each must pull with its OWN agent and
    /// its OWN cwd — the thing the old server-wide `agent`/`cwd` fields could not express.
    #[test]
    fn multi_root_server_serves_each_root_with_its_own_agent_and_cwd() {
        use crate::cache::Cursor;
        use crate::Transcript;
        let base = std::env::temp_dir().join(format!(
            "cr-serve-multiroot-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();

        // A Claude session in /a and a Codex session in /b.
        let claude = base.join("claude.jsonl");
        std::fs::write(&claude, concat!(
            r#"{"type":"user","cwd":"/a","message":{"role":"user","content":[{"type":"text","text":"hello claude"}]},"timestamp":"2026-08-01T10:00:00Z"}"#, "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi"}]},"timestamp":"2026-08-01T10:00:01Z"}"#, "\n",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"again"}]},"timestamp":"2026-08-01T10:00:02Z"}"#, "\n",
        )).unwrap();
        let codex = base.join("codex.jsonl");
        std::fs::write(&codex, concat!(
            r#"{"type":"session_meta","payload":{"id":"cx","cwd":"/b","source":"cli"}}"#, "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello codex"}]}}"#, "\n",
        )).unwrap();

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![
                Root {
                    id: "c1".into(),
                    agent: Agent::CLAUDE,
                    path: claude.clone(),
                    cwd: "/a".into(),
                },
                Root {
                    id: "x1".into(),
                    agent: Agent::CODEX,
                    path: codex.clone(),
                    cwd: "/b".into(),
                },
            ]),
            cache: test_cache(&base),
            port: std::sync::OnceLock::new(),
        };
        for (id, agent, path) in [("c1", Agent::CLAUDE, &claude), ("x1", Agent::CODEX, &codex)] {
            live.cache
                .register(id, Transcript::open(agent, path.clone()));
        }

        for (id, want_agent, want_cwd, want_text) in [
            ("c1", "claude", "/a", "hello claude"),
            ("x1", "codex", "/b", "hello codex"),
        ] {
            let reply = body(live.pull_response(id, Cursor::default()));
            let v: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(v["meta"]["agent"], json!(want_agent), "{id} agent");
            assert_eq!(v["meta"]["cwd"], json!(want_cwd), "{id} cwd");
            assert_eq!(v["meta"]["sid"], json!(id));
            // …and it really served THAT transcript, not the other root's.
            let records = std::fs::read_to_string(test_records(&base, id)).unwrap_or_default();
            let inline = reply.contains(want_text);
            assert!(
                inline || records.contains(want_text),
                "{id} must serve its own content"
            );
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn codex_pull_registers_distinct_threads_with_the_same_agent_path() {
        use crate::cache::Cursor;
        use crate::Transcript;
        let base = std::env::temp_dir().join(format!(
            "cr-serve-codex-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        let sessions = base.join("sessions/2026/07/29");
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&bundle).unwrap();
        let parent = sessions.join("rollout-parent.jsonl");
        std::fs::write(
            &parent,
            concat!(
                r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"first\"}"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-1","agent_thread_id":"child-a","agent_path":"/root/review","kind":"started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","call_id":"spawn-2","arguments":"{\"task_name\":\"review\",\"message\":\"second\"}"}}"#,
                "\n",
                r#"{"type":"event_msg","payload":{"type":"sub_agent_activity","event_id":"spawn-2","agent_thread_id":"child-b","agent_path":"/root/review","kind":"started"}}"#,
                "\n",
                r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-2","output":"{\"task_name\":\"/root/review\"}"}}"#,
                "\n",
            ),
        )
        .unwrap();
        for child_id in ["child-a", "child-b"] {
            std::fs::write(
                sessions.join(format!("rollout-{child_id}.jsonl")),
                format!(
                    "{}\n",
                    json!({
                        "type": "session_meta",
                        "payload": {
                            "id": child_id,
                            "cwd": "/repo",
                            "source": {
                                "subagent": {
                                    "thread_spawn": {
                                        "parent_thread_id": "parent",
                                        "agent_path": "/root/review"
                                    }
                                }
                            }
                        }
                    })
                ),
            )
            .unwrap();
        }

        let live = SessionService {
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CODEX,
                path: parent.clone(),
                cwd: "/repo".into(),
            }]),
            cache: test_cache(&base),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("parent", Transcript::open(Agent::CODEX, parent));

        let reply = body(live.pull_response("parent", Cursor::default()));
        let value: Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(
            value["meta"]["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(|child| child["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["child-a", "child-b"]
        );
        for child_id in ["child-a", "child-b"] {
            assert!(live.cache.is_registered(child_id));
            let (source, _) = live.resolve_id(child_id).expect("child resolves");
            assert!(source
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .contains(child_id));
        }

        let _ = std::fs::remove_dir_all(&base);
    }

    /// RecordStore put → read_range round-trips (#74): each committed block renders to one
    /// on-disk record whose locator addresses exactly its bytes, and reading from a given
    /// committed offset returns exactly the records from there on (the pointer serve path).
    #[test]
    fn record_store_put_then_read_range_round_trips() {
        use crate::engine::BlockStore;
        let dir = std::env::temp_dir().join(format!("cr-records-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("t.records");
        let _ = std::fs::remove_file(&path);
        let mut store = super::super::record_store::RecordStore::open_append(
            &path,
            FoldPolicy::default(),
            "/r".into(),
            crate::Transcript::open(Agent::CLAUDE, path.clone()),
        )
        .unwrap();
        let blocks = [
            crate::model::Block::AssistantText("first".into()),
            crate::model::Block::AssistantText("second".into()),
            crate::model::Block::AssistantText("third".into()),
        ];
        let locs: Vec<_> = blocks
            .iter()
            .enumerate()
            .map(|(at, b)| store.put(b.clone(), at, &[]))
            .collect();
        assert_eq!(locs.len(), 3);
        // Read from record 1 to EOF → records 1 and 2 only, each valid JSON with its text.
        let bytes = store.read_range(locs[1].offset, store.log_len());
        let got: Vec<Value> = std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(got.len(), 2);
        assert!(got[0].to_string().contains("second"));
        assert!(got[1].to_string().contains("third"));
        // Locator lengths address exactly one record each (framing newline excluded).
        let one = store.read_range(locs[1].offset, locs[1].offset + locs[1].len as u64);
        assert!(
            serde_json::from_slice::<Value>(&one).is_ok(),
            "locator-exact read parses"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The `/session?id=` page (#98 §6.3): with no chrome it is byte-identical to the shell
    /// `--html` writes; `chrome=embed` swaps the brand for the session title and hides the
    /// theme toggle; `theme=` appends the post-boot stamp. The no-chrome equality is the
    /// whole reuse argument — one page, one renderer, zero drift.
    #[test]
    fn session_route_serves_the_shell_with_optional_chrome() {
        let dir = std::env::temp_dir().join(format!("cr-sess-route-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let sess = dir.join("s.jsonl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &sess,
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n",
        )
        .unwrap();
        let live = SessionService::new(ServiceConfig {
            cache_root: None,
            presentation: Presentation::Html,
            fold: FoldPolicy::default(),
            scratch: dir.join("scratch"),
        })
        .unwrap();
        let id = live.register_root(&sess);

        let plain = live.page(&id, None).expect("resolvable");
        let title = display_title(Agent::CLAUDE, &sess);
        assert_eq!(
            plain,
            build_shell(&title, &id, true, true),
            "no chrome ⇒ exactly the page --html serves"
        );
        assert!(plain.contains("claude-replay <span class=\"brand-sub\""));
        assert!(!plain.contains("data-theme\",\""), "no stamp by default");

        let embedded = live
            .page(
                &id,
                Some(&PageChrome {
                    embed: true,
                    theme: Some("light".into()),
                }),
            )
            .unwrap();
        assert!(
            embedded.contains(&format!(
                "id=\"embed-title\" title=\"{title}\">{title}</div>"
            )),
            "embed shows the session title where the brand was"
        );
        assert!(
            !embedded.contains("claude-replay <span class=\"brand-sub\""),
            "and the brand is gone"
        );
        assert!(
            embedded.contains("id=\"btn-theme\" class=\"tbtn\" style=\"display:none\""),
            "theme toggle hidden — the host owns the theme"
        );
        assert!(
            embedded.ends_with(
                "<script>document.documentElement.setAttribute(\"data-theme\",\"light\");</script>\n</body>\n</html>\n"
            ),
            "the stamp runs AFTER the page's own boot"
        );

        // The route itself: unknown ids 404; a valid id serves the page.
        let r = service_routes(
            Some(&live),
            &dir,
            "session",
            &format!("id={id}&chrome=embed"),
        );
        assert_eq!(r.code, "200 OK");
        let r = service_routes(Some(&live), &dir, "session", "id=nope");
        assert_eq!(r.code, "404 Not Found");
        // `session=` is accepted as an alias for `id` (#120): the embedded view navigates
        // sub-agents with a relative `?session=<child>` href, so the drill-down URL becomes
        // `/session?session=<child>` — reading only `id` 404'd every drill-down.
        let r = service_routes(Some(&live), &dir, "session", &format!("session={id}"));
        assert_eq!(r.code, "200 OK", "session= is an alias for id=");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

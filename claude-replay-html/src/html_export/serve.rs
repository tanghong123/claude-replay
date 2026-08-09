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
use crate::cache::{lock, pull_indices, Admission, Cursor, SharedSession};
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
    dir: std::path::PathBuf,
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
    /// Scratch directory for the cache-less fallback and the static shell.
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
            dir: cfg.scratch,
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
    /// A denial is never fatal here: partial success is normal for a multi-root server, so a
    /// session another process holds — or one with no durable slot at all — is simply served
    /// **cache-less**, out of the run's own temp bundle. The pick-time redirect a holder's note
    /// enables is a routing decision made before this point; by the time a client is pulling,
    /// the page is already open and cannot be handed off.
    fn session_for(
        &self,
        id: &str,
        src: &Transcript,
        cwd: &str,
    ) -> Option<Arc<SharedSession<RecordStore>>> {
        if let Some(ss) = self.cache.touch(id) {
            return Some(ss);
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
        match self.cache.admit(
            id,
            |dir| open(&dir.join("records.jsonl")),
            |h| lock::pid_alive(h.pid) && port_open(h.note.as_ref().map(|n| n.port)),
        ) {
            Admission::Owned { session, .. } => {
                // Now that the entry is ours, say where we serve it. This is the first moment
                // both facts are true: the lock is held AND the port is known.
                if let Some(&port) = self.port.get() {
                    let _ = self.cache.publish(id, HtmlNote { port });
                }
                Some(session)
            }
            Admission::Denied(_) => {
                // Cache-less: the run's own temp bundle, wiped at startup. Range reads go
                // through the store's own path, so this serves fully.
                let at = self.dir.join(format!("{id}.records"));
                let store = open(&at).or_else(|_| {
                    open(
                        &std::env::temp_dir()
                            .join(format!("cr-records-{}-{id}.records", std::process::id())),
                    )
                });
                self.cache.open_uncached(id, store.ok()?)
            }
        }
    }

    /// The `/pull` handler: serve the pull-client wire reply for `id` at `cursor`. The session
    /// domain lives in the [`SessionCache`]: it materializes the id's [`SharedSession`] on first
    /// pull and TTL-reaps idle residents (no background thread — folding rides this request's
    /// thread, so a session nobody is pulling costs nothing). `None` for an unknown/unreadable id.
    pub fn pull_response(&self, id: &str, cursor: Cursor) -> Option<String> {
        let (src, title) = self.resolve_id(id)?;
        if !src.path().exists() {
            return None;
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
            self.cache.remove_pull(id);
            self.cache.release(id);
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
            return Some(
                json!({
                    "t": "pull", "epoch": epoch,
                    "committed_from": cf, "committed_ext": Value::Null,
                    "provisional_gen": gen,
                    "provisional_from": pf, "provisional": [],
                    "meta": Value::Null,
                })
                .to_string(),
            );
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
        Some(pull_reply_json(
            d.epoch,
            d.provisional_gen,
            cf,
            committed_ext,
            pf,
            &provisional_records,
            &meta,
        ))
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
    pub fn open(&self, sid: &str) {
        open_in_browser(&self.url_for(sid));
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
    let live = Arc::new(SessionService::new(ServiceConfig {
        cache_root: Some(if args.no_cache {
            crate::sys::throwaway_root()
        } else {
            cache::admit::default_root().unwrap_or_else(crate::sys::throwaway_root)
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
        std::sync::Arc::new(move |name: &str, query: &str| {
            service_routes(live.as_deref(), &root, name, query)
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
}

/// A minimal read-only loopback HTTP listener whose ROUTING is the caller's (#98 §6.6:
/// "the listener takes a handler"). `port` 0 picks an ephemeral port; a host wanting a
/// stable address passes its own. Returns the bound port; the accept loop runs on a
/// detached thread (dies with the process). One listener implementation for `--html` and
/// every host, so a header fix lands everywhere at once.
/// A route handler: `(path, query) -> reply`. Shared by the listener and any host chaining
/// its own routes in front of [`service_routes`].
pub type RouteHandler = std::sync::Arc<dyn Fn(&str, &str) -> HttpResponse + Send + Sync>;

pub fn spawn_listener(port: u16, handler: RouteHandler) -> Result<u16> {
    use std::net::TcpListener;
    let listener = TcpListener::bind(("127.0.0.1", port)).context("bind loopback HTTP server")?;
    let port = listener.local_addr()?.port();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let handler = handler.clone();
            std::thread::spawn(move || {
                let _ = serve_connection(stream, &*handler);
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

fn serve_connection(
    mut stream: std::net::TcpStream,
    handler: &(dyn Fn(&str, &str) -> HttpResponse + Send + Sync),
) -> std::io::Result<()> {
    use std::io::{BufRead, BufReader, Write};
    let mut line = String::new();
    BufReader::new(stream.try_clone()?).read_line(&mut line)?;
    // `GET /name?query HTTP/1.1`
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let (path_part, query) = target.split_once('?').unwrap_or((target, ""));
    let name = path_part.trim_start_matches('/');
    let r = handler(name, query);
    let head = format!(
        "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        r.code,
        r.content_type,
        r.body.len()
    );
    stream
        .write_all(head.as_bytes())
        .and_then(|_| stream.write_all(&r.body))
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
        return match live.pull_response(id, cursor) {
            Some(body) => HttpResponse::json(body),
            None => HttpResponse::not_found("no such agent"),
        };
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
            let reply = live.pull_response(id, self.cursor()).expect("pull reply");
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
            dir: bundle,
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

        live.pull_response("nid", Cursor::default()).expect("pull");

        let held = lock::read::<HtmlNote>(&entry).expect("the pull admitted and locked it");
        assert_eq!(held.pid, std::process::id());
        assert_eq!(
            held.note.expect("the note must not be null").port,
            4321,
            "a peer reads this to redirect instead of standing up its own server"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    /// sub-agent spawn + async completion (queue-op), commits, plus a lagging second client
    /// (missed ticks) and an interleaved third client (a second tab).
    #[test]
    fn incremental_client_always_equals_a_fresh_reload() {
        use crate::{SessionCache, Transcript};
        let base = std::env::temp_dir().join(format!("cr-sim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sess = base.join("sid.jsonl");
        let bundle = base.join("bundle");
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(&sess, "").unwrap();

        let live = SessionService {
            dir: bundle,
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: SessionCache::new(),
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
        use crate::{SessionCache, Transcript};
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
            dir: bundle,
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: SessionCache::new(),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("sid", Transcript::open(Agent::CLAUDE, sess.clone()));

        // Fresh cursor: turn 1 committed (the second user turn opened turn 2) ⇒ the reply carries
        // a pointer, not inline committed records.
        let reply = live.pull_response("sid", Cursor::default()).expect("reply");
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
        let idle: Value = serde_json::from_str(&live.pull_response("sid", next).unwrap()).unwrap();
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
        use crate::{SessionCache, Transcript};
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
            dir: bundle,
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CLAUDE,
                path: sess.clone(),
                cwd: "/r".into(),
            }]),
            cache: SessionCache::new(),
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
        let reply = live
            .pull_response("sid", Cursor::default())
            .expect("parent reply");
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
        use crate::{SessionCache, Transcript};
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
            dir: bundle,
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
            cache: SessionCache::new(),
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
            let reply = live
                .pull_response(id, Cursor::default())
                .unwrap_or_else(|| panic!("{id} reply"));
            let v: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(v["meta"]["agent"], json!(want_agent), "{id} agent");
            assert_eq!(v["meta"]["cwd"], json!(want_cwd), "{id} cwd");
            assert_eq!(v["meta"]["sid"], json!(id));
            // …and it really served THAT transcript, not the other root's.
            let records =
                std::fs::read_to_string(live.dir.join(format!("{id}.records"))).unwrap_or_default();
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
        use crate::{SessionCache, Transcript};
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
            dir: bundle,
            fold: FoldPolicy::default(),
            roots: std::sync::Mutex::new(vec![Root {
                id: "sid".into(),
                agent: Agent::CODEX,
                path: parent.clone(),
                cwd: "/repo".into(),
            }]),
            cache: SessionCache::new(),
            port: std::sync::OnceLock::new(),
        };
        live.cache
            .register("parent", Transcript::open(Agent::CODEX, parent));

        let reply = live
            .pull_response("parent", Cursor::default())
            .expect("parent reply");
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

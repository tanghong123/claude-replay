# Design: taking persistence out of `SessionCache`

> **Status:** proposed (not built), **resolved to a shape — see §13**. Tracked as task **#167**.
> Design-only; no code has moved. §3–§5 explore the space; §13 is the decision.
> Builds on [`durable-session-cache.md`](durable-session-cache.md) (#96 — BUILT), which is the
> design of the thing being re-cut here. Read §1 and §3; the rest follows from them.
>
> For the API as it *is* — every type, field and method, always in sync because it is generated
> from the source — see the rustdoc: <https://tanghong123.github.io/claude-replay/>
> (`cargo apidoc --open` locally). This document is the argument for changing it; Appendix A is a
> map of which pages to open.

**The rule** (the user, 2026-08-08):

> *"session cache itself should have no knowledge of persistency. The persistency (and
> durability) are provided by the BlockStore and other interfaces. So persistent/durable cache
> directory should not be tied to the main cache API."*

---

## 1. The problem, in one sentence

`SessionCache` is two things wearing one name: a **keyed cache of live sessions** (ids → sources,
ids → resident folders, a residency policy) and a **client of the filesystem** (a root directory,
a lock per entry, a meta stream, a garbage collector). Only the first is what a cache *is*. The
second leaked in because #96 needed somewhere to put it, and the cache was the type that already
knew every session by id.

Concretely, today:

```rust
pub struct SessionCache<P: BlockStore, A = ()> {
    registry:       Mutex<HashMap<String, Transcript>>,      // ids → sources           — a cache
    pull_residents: Mutex<HashMap<String, PullResident<P>>>, // ids → LIVE SESSIONS     — a cache
    aux:            Mutex<HashMap<String, A>>,               // ids → sidecars          — a cache
    admitting:      Mutex<()>,                               // one admission at a time — a cache
    durable:        Option<Durable>,                         // ← everything below is NOT
}

struct Durable {
    presentation: Presentation,   // a FRONTEND enum: Tui | Html
    root:         PathBuf,        // a DIRECTORY
    versions:     Versions,       // a FOLD stamp
    owned:        Mutex<HashMap<String, Owned>>,  // which directories we hold locks on
}
```

**Where the session itself is**, since the type alias hides it — this is the resident everything
else in this doc talks about:

```rust
type PullResident<P> = (Instant, Arc<SharedSession<P>>);
//                      ▲         ▲
//                      │         └── the live session: `Arc` so any number of request threads
//                      │             share ONE of them (§7.1). This is what `admit` and `touch`
//                      │             hand back, what `reap` counts references to (#168), and
//                      │             what the `admitting` gate exists to keep singular (#169).
//                      └── the idle clock `reap` reads; stamped when a request TAKES the session,
//                          which is why a fold longer than the TTL once looked idle (#168).

pub struct SharedSession<S: BlockStore = InMemoryStore> {
    inner: Mutex<Inner<S>>,   // the thread-level EXCLUSION lock of §7.1 — every public
}                             // method takes it; `advance()` holds it for the whole fold

struct Inner<S: BlockStore> {
    follower: Box<FollowParser<S>>,  // the incremental fold — and, inside it, the store `S`
    epoch: u64,                      // the cursor-protocol facts the pull reply is built from
    provisional_gen: u64,
    n_provisional: usize,
    meta: Option<MetaWriter>,        // present ⇔ we are writing; `quiesce` sets it to None
    frozen: bool,                    //   …and this to true (#109 retention)
    …
}
```

Four layers, and it is worth keeping them straight because the doc moves between them:
`Transcript` (where the session's bytes come FROM) → `SharedSession` (the live fold, one per
session) → `S: BlockStore` (where folded blocks GO — `RecordStore` on disk, `ArcLog` in memory) →
`Bv` (what the store hands back per block: a locator, or an `Arc<Block>`).

The `meta: Option<MetaWriter>` field is the one §7.3 proposes to make own the process lock —
"present ⇔ we are writing" would become "present ⇔ we hold the entry", which is the same fact.

Four smells fall out of that one field:

1. **A constructor that takes a directory** — and does I/O as a side effect of *building a cache*:
   ```rust
   pub fn durable(presentation: Presentation, root: PathBuf, versions: Versions) -> Self {
       admit::gc(&root);   // a directory walk, from a constructor
       …
   }
   ```
2. **`Presentation` in a cache's signature.** `Tui | Html` is a *frontend* concept; the cache
   only ever uses it to build a path (`<root>/<tui|html>/<session>`).
3. **Filesystem words in the public vocabulary.** `Denial::Unavailable::UnwritableRoot`,
   `NoCacheFlag` — a cache telling you about `chmod` and CLI flags.
4. **A lock lifecycle** — `publish` / `release` / `release_all` / `Drop` all reach into
   `cache::lock`, which is `LOCK` files on disk.

None of this is *wrong* today; it works and it is well tested. It is in the wrong place, and the
cost of that shows up as: a third party cannot use the cache without accepting a directory layout,
tests reach for a real filesystem to exercise pure residency logic, and — see §9 — a missing lock
hid inside a function that does six unrelated things.

---

## 2. Who knows what (the target picture)

```
        ┌──────────────────────────────────────────────┐
CLIENT  │ claude-replay --html  ·  claude-monitor  ·   │   picks the ROOT, builds the provider,
        │ the TUI                                      │   owns `gc`, owns "where things live"
        └───────────────────┬──────────────────────────┘
                            │ builds
                            ▼
        ┌──────────────────────────────────────────────┐
PROVIDER│ FsEntries: root, Presentation, Versions,      │   locks, meta streams, entry dirs,
        │ liveness rule, Note type                     │   resume/align, gc, KEEP_FOR
        └───────────────────┬──────────────────────────┘
                            │ handed to
                            ▼
        ┌──────────────────────────────────────────────┐
CACHE   │ SessionCache: ids → sources, ids → residents,│   residency, TTL, sidecars,
        │ TTL/budget, one-admission-at-a-time          │   one tailer per session
        └───────────────────┬──────────────────────────┘
                            │ holds one Arc per session
                            ▼
        ┌──────────────────────────────────────────────┐
SESSION │ SharedSession: the follower + epoch/gen,      │   ONE per session, shared by every
        │ behind one Mutex                             │   request thread (§7.1)
        └───────────────────┬──────────────────────────┘
                            │ folds into
                            ▼
        ┌──────────────────────────────────────────────┐
STORE   │ RecordStore / ArcLog (BlockStore)            │   the bytes themselves
        └──────────────────────────────────────────────┘
```

The cache never names a directory. It asks the provider for a backing and is told either what it
got or who has it.

---

## 2b. End to end: one session, from a click to the disk and back

Everything below this point is about pieces. This is the whole path once, so the pieces have
somewhere to sit. Follow one session through a `claude-monitor` visit.

### First visit — nothing exists yet

```
 browser                server                    cache                provider          disk
    │  GET /session ──────▶ page(id)                                                       
    │  ◀── the shell ──────┤  (no fold, no admission: just HTML)                          
    │                      │                                                               
    │  GET /pull?cursor=0 ─▶ pull_response_for(id, 0)                                      
    │                      ├── reap(30s) ─────────▶ drop residents idle AND unreferenced   
    │                      ├── touch(id) ─────────▶ None — nobody has it                   
    │                      └── session_for ───────▶ admit(id, make_store)                  
    │                                              ├─ gate: one admission at a time  (#169)
    │                                              ├─ ours = None (no resident)      (#109)
    │                                              └─ claim ──────────▶ entry_dir ────▶ mkdir
    │                                                                   lock::acquire ─▶ LOCK
    │                                                                   make_store ────▶ open
    │                                                                                   records.jsonl
    │                                                                   recover ───────▶ read
    │                                                                    (no meta yet)   meta.jsonl
    │                                              ◀── Owned { store, Cold(NoPriorCache) }
    │                                              ├─ SharedSession::with_store         
    │                                              ├─ install → pull_residents[id]      
    │                                              └─ attach_writer(MetaWriter)         
    │                      ├── publish(id, {port}) ───────────────────▶ note into ─────▶ LOCK
    │                      ├── shared.advance() ──▶ FOLD the transcript
    │                      │                        each committed block: store.put ──▶ append
    │                      │                        each commit: meta record ─────────▶ append
    │                      └── build the reply: counters + committed_ext {offset, len}
    │  ◀── pull reply ─────┤
    │  GET /records?from&len ─▶ records_bytes ────▶ shared_peek → store.read_range ───▶ read
    │  ◀── the bytes ──────┘
```

Two things to notice, because the rest of the doc argues about them. The **lock is taken before
the store is opened** — a denial must leave nothing open, or "nothing was opened" is a lie. And
the **fold happens on the request's own thread**, holding that session's mutex: the requester
pays, and a session nobody is pulling costs nothing.

### Second pull, seconds later

`touch(id)` finds the resident and returns the same `Arc`. No admission, no lock, no provider.
`advance()` folds only what the transcript grew by; the reply's `committed_ext` points at the new
bytes. This is the steady state, and it is why a second browser tab is free: it is another reader
of one session (§7.1), not another tailer.

### Idle 30 seconds, then a pull

`reap(TAIL_TTL_MS)` drops the resident — **unless someone still holds it** (#168). The entry's
lock is NOT released: this process still owns it. The next pull re-admits:

```
admit → gate → ours = None (the resident is gone) → claim
                                   ├─ acquire: the LOCK is OURS already → granted (lock.rs:82)
                                   ├─ make_store: open_append (never truncates)
                                   ├─ load_from(0): walk the log, rebuild locators   ← a RESUME
                                   │                (and since #168, stop at a line
                                   │                 carrying two records)
                                   └─ recover: versions + anchor + align → Resumed
```

Nothing is re-folded and nothing is re-rendered — the blocks are already on disk, and this reads
them back as locators. That is what makes a 30-second TTL affordable, and it is the whole point of
the durable entry.

### A second process arrives

```
claude-replay --html <same session>
    └─ existing_server(root, sid) ──▶ read LOCK ──▶ note {port} ──▶ open that URL instead, quit
```

If it gets past that (a session admitted lazily after a page is already open), `admit` returns
`Denied(Held(holder))`, and the page is told to navigate rather than served a second copy (#163).
**One entry, one writer, always.**

### Tomorrow

The process is gone; the entry is not. A new run's `admit` finds a lock naming a dead pid,
reclaims it, and `recover` compares the meta stream's fold version and source anchor against
today's. Same → resume from `replay_from`. Different → `Cold(VersionChanged)`, and the log is
rebuilt from scratch. Untouched for 14 days → `gc` deletes it (30 if a lock is still there — §7).

### Where each layer's responsibility starts and stops

| layer | owns | does NOT know |
|---|---|---|
| **client** (viewer/monitor) | which root, which fold policy, when to gc | anything about locks or meta streams |
| **cache** | ids → sources, ids → residents, TTL, one-admission-at-a-time | where an entry lives on disk |
| **session** | the fold, the epoch/gen, one-thread-at-a-time | that its store is durable at all |
| **store** | how a block becomes bytes, and back | who else might want the file |
| **provider** (today: `admit`+`lock`) | entry dirs, locks, notes, resume/align, gc | what a `SharedSession` is |

The last column is the design. Today the middle three rows are one type — §1 shows the cache
holding the provider's whole job in a `durable` field, and §3 is about giving it back.

## 3. The seam

```rust
/// What a CLIENT provides so the cache can hand out sessions that outlive the process.
/// The cache never names a directory, a lock, or a file — it asks for a backing and is told
/// either what it got or who has it.
pub trait Entries<P: BlockStore> {
    /// What a holder publishes about itself for a peer that finds the entry taken:
    /// a port for the HTML server, a tmux pane for the TUI.
    type Note: Serialize + DeserializeOwned + Clone;

    /// Take exclusive ownership of `id`'s backing, or say who has it.
    ///
    /// `ours` is the CALLER's half of the #109 witness (§4): how many bytes the resident the
    /// cache already holds has written, or `None` when it holds nothing.
    ///
    /// `make_store` stays a per-call argument, exactly as it is today: only the caller knows
    /// this session's cwd and fold policy, and a server hosting several roots renders each
    /// against its own. The provider supplies the DIRECTORY; the caller supplies the context.
    fn open(
        &self,
        id: &str,
        src: &Transcript,
        ours: Option<u64>,
        make_store: &dyn Fn(&Path) -> std::io::Result<P>,
    ) -> Opened<P, Self::Note>;

    /// Say where we serve `id`, now that both facts are true (we hold it, and we are bound).
    fn publish(&self, id: &str, note: Self::Note) -> bool;

    /// Give `id` back. Nothing else may write it until someone opens it again.
    fn release(&self, id: &str);
}

pub enum Opened<P: BlockStore, N> {
    /// Ours. `origin` stays as rich as it is today — a denial is a support question (§8.4).
    Owned { store: P, loaded: Vec<P::Bv>, origin: Origin },
    /// Nothing was opened: held by a live peer, or nothing here to open.
    Denied(Denial<N>),
}
```

Today's `cache::admit` + `cache::lock` become **`FsEntries`** — the one implementation shipped,
and the only thing that knows `Presentation`, `Versions`, `gc`, `KEEP_FOR`, or a `PathBuf`.

### 3.1 Two axes, not one: persistence and sharing

The ceremony in §6.1 — lock, note, publish, deny, redirect — exists for exactly one reason:
**another process might want the same entry**. It has nothing to do with whether the bytes survive
this process. Those are independent:

| | not shared | shared |
|---|---|---|
| **not persistent** | in-memory cache — a library consumer, a test | *(no such thing: nothing to contend for)* |
| **persistent** | `--no-cache`'s private root; a single-tool bundle | the shared root: two viewers, a monitor |

Three of those four are real, and only ONE of them needs the protocol. A provider that cannot be
contended for should not be made to perform it:

- **`open` always returns `Owned`** — `Denied`, `Denial`, `Holder` and the redirect path never
  arise, because no peer exists.
- **`publish` is meaningless** — there is nobody to announce to. `Note` is `()`.
- **`release` still matters**, but only its quiesce half: stop writing. There is no lock to drop.

So `publish` and `release` get **default method bodies** on the trait, and a minimal provider is
one function:

```rust
struct Memory;

impl Entries<ArcLog> for Memory {
    type Note = ();                        // nothing to publish
    fn open(&self, _id, _src, _ours, make_store) -> Opened<ArcLog, ()> {
        // Nobody else can have it. There is nothing to align, resume, or refuse.
        Ok(Opened::Owned { store: make_store(…)?, loaded: vec![], origin: Origin::Cold(NoPriorCache) })
    }
    // publish → false, release → no-op: both defaulted.
}
```

This is worth stating because it is not hypothetical today. **`--no-cache` already pays the full
ceremony for entries nobody can ever see**: #165 gives it a pid-keyed private root, and every
session in it still takes a `LOCK`, writes a note, and can in principle be `Denied` — by a peer
that cannot exist. The protocol is not wrong there, it is simply unreachable, which is a good sign
it belongs to a *kind* of provider rather than to all of them.

**The wart this exposes.** `make_store: &dyn Fn(&Path) -> io::Result<P>` bakes a directory into
the seam — a memory provider has no path to hand it. Three ways out, undecided:

| | approach | cost |
|---|---|---|
| a | pass a meaningless path (`Path::new("")`) and let memory stores ignore it | one line, and exactly the sort of quiet lie this refactor exists to remove |
| b | an enum: `Where::Dir(&Path)` / `Where::Nowhere` | honest; one `match` in every store factory that does not need it |
| c | an associated `type Place<'a>` on the trait (GAT) | most precise; the heaviest to read, and it infects every signature |

Leaning **b**: it says the true thing without asking a reader to know what a GAT is, and the
`Nowhere` arm is a one-line `unreachable`/ignore in the two factories that would ever see it.

### 3.2 Granularity is the provider's choice too

Sharing is not one thing either: the two shipped clients exclude peers at **different
granularities**, and both are right.

| client | granularity | why |
|---|---|---|
| `claude-replay` (viewer) | per `<session, frontend>` | two viewers on different sessions must both work; only the same session collides |
| `claude-monitor` | the whole root, one lock (#160) | it serves *every* session on the machine, so "one monitor per root" is the useful statement |

That was a deliberate decision (DESIGN.md, Decisions — *"the former will have lock granularity of
`<session, frontend>`, the latter will just be a single entity (one big lock)"*). What was not
decided is that the monitor ends up paying **both**, because it reaches its entries through the
same `admit` path:

```
~/.cache/claude-monitor/LOCK                      ← the root lock (#160): one monitor per root
~/.cache/claude-monitor/html/<session>/LOCK       ← and a per-entry lock. 56 of 56 entries have one.
```

The per-entry locks there can never be contended. The root lock already guarantees no second
monitor, and nothing else on the machine reads that root — the viewer's is
`~/.cache/claude-replay/sessions`. So an entire protocol (take, note, publish, `Denied`, redirect)
runs for peers that cannot exist. Exactly the `--no-cache` observation above, arrived at from the
other direction.

**And it is not free.** The only way the monitor can be `Denied` its own entry is a FALSE
positive: a stale entry lock naming a pid that has since been recycled, whose published port
(2727) happens to answer — because the monitor itself is answering on it. That is why #163 had to
add a self-guard (*"a note naming OUR OWN port is not evidence of a peer"*). A protocol that
cannot succeed at its job here, but can still fail at it.

So the provider should say what its granularity is, and a root-locked provider's per-entry
protocol should be a no-op:

```rust
// the viewer: contention is per session
FsEntries::per_session(root, Presentation::Html)

// the monitor: one lock at the root, taken once at startup; entries need no protocol of their own
FsEntries::single_writer(root, Presentation::Html)   // open → always Owned; no note, no Denied
```

Note what this does NOT change: entries still get their own **directories**, meta streams and
record logs — that is layout, and the rail reads it lock-free (§10). What goes is the per-entry
*exclusion*, which the root lock has already provided.


### 3.3 One Rust wrinkle, named up front

`Entries` has an associated type (`Note`), so the cache cannot hold it as a bare
`Box<dyn Entries<P>>` — a trait object must name its associated types. Two ways out:

| | shape | cost |
|---|---|---|
| **A** (proposed) | `SessionCache<P, A, E: Entries<P>>` — generic over the provider | a third type parameter; each frontend already has a one-line alias, so the churn is one line each |
| B | `SessionCache<P, A, N>` + `Box<dyn Entries<P, Note = N>>` | still a third parameter, plus a `dyn` indirection, and the provider becomes harder to inline |

**A**, because it costs the same in signatures and less at runtime. Today's aliases absorb it:

```rust
// claude-replay-tui/src/app.rs  — before
pub(crate) type TuiCache = SessionCache<ArcLog, ViewSidecar>;
// after
pub(crate) type TuiCache = SessionCache<ArcLog, ViewSidecar, FsEntries<ArcLog, TuiNote>>;
```

---

## 4. The one factoring that makes it work

This is the subtle part, and the reason the seam is `ours: Option<u64>` rather than
`&SharedSession<P>`.

Re-admitting a session asks: *do I have to rebuild anything?* Answering needs three facts held by
three different parties:

| fact | held by | why it matters |
|---|---|---|
| how many bytes the **resident I already have** wrote | the **cache** | it is the cache's own `SharedSession` |
| how many bytes are **on disk now** | the **store** | `backing_len()` |
| whether the **stream is still mine** (fold version + source anchor) | the **provider** | it reads `meta.jsonl` |

Today all three meet inside one closure in `admit`, because the cache can see all three:

```rust
// BEFORE — cache/mod.rs, inside admit()
let on_disk = s.backing_len();                                     // store
let ours = resident.as_ref().map(|ss| ss.store_read(|_, st| st.backing_len())); // cache
if ours == Some(on_disk) && admit::stream_unchanged(dir, src.path(), &d.versions) {
    return admit::Backing::Retained;                               // provider's half, inline
}
```

The tempting refactor is to hand the provider the resident and let it decide:

```rust
// TEMPTING — and wrong
fn open(&self, id: &str, src: &Transcript, resident: Option<&SharedSession<P>>) -> Opened<..>;
```

That looks like separation and is not: `SharedSession` is the cache's central type, so the
provider now depends on the cache and nothing has been split — the code moved, the coupling
stayed. **Pass a number.** The cache computes its own half and sends the one fact the provider
needs:

```rust
// AFTER — the cache keeps its own fact and hands over a u64
let ours = self.shared_peek(id)
    .filter(|ss| ss.frozen())
    .map(|ss| ss.store_read(|_, st| st.backing_len()));

match self.entries.open(id, &src, ours, &make_store) {
    Opened::Owned { store, loaded, origin } => { /* build + install the session */ }
    Opened::Denied(d) => Admission::Denied(d),
}
```

`Retained` / `Resumed { at }` / `Cold(reason)` come back in `origin`, and the cache does what it
already does with them. The provider never learns what a `SharedSession` is.

---

## 5. Before and after, at every call site

### 5.1 Building the cache — the HTML server

```rust
// BEFORE — claude-replay-html/src/html_export/serve.rs
let cache = match cfg.cache_root {
    Some(root) => SessionCache::durable(
        cfg.presentation,
        root,
        Versions::current(Some(render_flavor(&cfg.fold))),
    ),
    None => SessionCache::ephemeral(),
};
```

```rust
// AFTER — the client builds the provider; the cache just holds it
let entries = FsEntries::new(cfg.cache_root, cfg.presentation)
    .versions(Versions::current(Some(render_flavor(&cfg.fold))))
    .liveness(html_liveness(port.clone()));   // pid + port probe + self-guard (§5.3)
entries.gc();                                 // explicit, where the root is known
let cache = SessionCache::new(entries);
```

Three things changed and each is the point: the **root** is named by the client, `gc()` is a
**call** rather than a constructor side effect, and the **liveness rule** sits with the thing that
owns locks instead of being threaded through every `admit`.

### 5.2 Building the cache — the TUI

```rust
// BEFORE — claude-replay-tui/src/app.rs
fn make_cache(args: &Args) -> TuiCache {
    crate::sys::reclaim();
    let root = match args.no_cache {
        true  => crate::sys::throwaway_root(),
        false => cache::admit::default_root().unwrap_or_else(crate::sys::throwaway_root),
    };
    TuiCache::durable(Presentation::Tui, root, Versions::current(None))
}
```

```rust
// AFTER — the same decision, one layer out
fn make_cache(args: &Args) -> TuiCache {
    crate::sys::reclaim();
    let root = match args.no_cache {
        true  => crate::sys::throwaway_root(),
        false => cache::admit::default_root().unwrap_or_else(crate::sys::throwaway_root),
    };
    let entries = FsEntries::new(root, Presentation::Tui)
        .versions(Versions::current(None))
        .liveness(|h: &Holder<TuiNote>| lock::pid_alive(h.pid));
    entries.gc();
    SessionCache::new(entries)
}
```

Note what did **not** move: `throwaway_root()` and `default_root()` were already the client's
choice (#165 moved them into `present::sys` for exactly this reason). This change extends the
same idea one step further in.

### 5.3 Admitting a session — the HTML server

```rust
// BEFORE — the liveness rule is rebuilt at every call, and the store factory
// carries the directory layout ("records.jsonl") at the call site
let ours = self.port.get().copied();
let alive = |h: &Holder<HtmlNote>| {
    let port = h.note.as_ref().map(|n| n.port);
    port != ours && lock::pid_alive(h.pid) && port_open(port)
};
match self.cache.admit(id, |dir| open(&dir.join("records.jsonl")), alive) {
    Admission::Owned { session, .. } => { … }
    Admission::Denied(Denial::Held(h)) => { … }
}
```

```rust
// AFTER — the call says what it wants, not where or under what rules
match self.cache.admit(id, |dir| open(&dir.join("records.jsonl"))) {
    Admission::Owned { session, .. } => { … }        // unchanged
    Admission::Denied(Denial::Held(h)) => { … }      // unchanged
}
```

`make_store` stays, and stays a closure taking `&Path`: only this caller knows the session's cwd
and fold policy, and a server hosting several roots renders each against its own. What leaves is
the **liveness rule** (provider config now, and it needs the server's bound port — which the
provider gets as a shared `Arc<OnceLock<u16>>` rather than by capturing `self`).

`Admission` and `Denial` are unchanged, so every `match` at every call site keeps compiling. That
is deliberate: it is what makes §11's steps small.

### 5.4 The provider, sketched

```rust
pub struct FsEntries<P, N> {
    root: PathBuf,
    presentation: Presentation,
    versions: Versions,
    alive: Box<dyn Fn(&Holder<N>) -> bool + Send + Sync>,
    owned: Mutex<HashMap<String, PathBuf>>,   // moved off the cache
    _store: PhantomData<P>,
}

impl<P: DurableStore, N> Entries<P> for FsEntries<P, N> {
    type Note = N;

    fn open(&self, id, src, ours, make_store) -> Opened<P, N> {
        let dir = entry_dir(&self.root, self.presentation, id);   // ← the only place a path is built
        match admit::claim(&dir, src.path(), &self.versions, ours, make_store, &self.alive) {
            Claim::Ours { store, loaded, origin } => { self.owned.lock().insert(id, dir); Opened::Owned { … } }
            Claim::Denied(d) => Opened::Denied(d),
        }
    }
    fn publish(&self, id, note) -> bool { … lock::publish … }
    fn release(&self, id)              { … lock::release_any … }
}
```

Almost all of this body already exists — it is today's `admit::claim`, `recover`, `align` and
`writer_for` with the cache's half of the witness arriving as `ours` instead of being read from a
`SharedSession`.

---

## 6. Using the cache: the whole lifecycle

**Read this first if the seam is what confused you.** Almost none of the lifecycle changes. A
client builds the cache differently (§5.1–5.2) and drops one argument from `admit` (§5.3);
everything below is the same before and after, and is shown as it works today.

There is exactly **one resident per session**, and both frontends share it. What differs is how
they read it: the HTML server serves any number of stateless clients from it over the cursor
**pull** protocol; the TUI ticks it in-process for a **`ViewDelta`** it splices into a view.

### 6.1 The order of operations

```
   register ──▶ admit ──▶ publish ─┬─▶ [ serve / tick ]  ──▶ release
   (know it)   (own it)  (announce)│      touch                (let go, stay resident)
                                   │      advance
                                   └──────┘ every request/tick
```

1. **`register(id, src)`** — *"this id exists, and here is its transcript."* Cheap: a map entry,
   nothing opened, nothing folded. Use `register_new` for a source that may already be known —
   it preserves the first, richest descriptor against a later bare one (a child registered by its
   parent's pull keeps its ancestry).
2. **`admit(id, make_store)`** — *"and I want to own it."* The only path that takes the entry's
   lock, and therefore the only way a durable session comes into being. Returns `Owned { session,
   origin }` or `Denied`. Registration alone is **not** enough: `poll_view` will not materialize a
   session `admit` never granted, so a registered-but-unadmitted id ticks forever without ever
   producing anything.
3. **`publish(id, note)`** — *"and here is where I serve it."* Separate from `admit` because the
   useful fact arrives later: a server has no port until it binds. Returns `false` if this process
   does not own `id` — worth checking, since publishing before admitting is silently a no-op.
4. **serve / tick** — §6.2.
5. **`release(id)`** — *"I am done writing."* The session stays **resident and readable**; what
   stops is every write. That is what lets a later re-admission keep the blocks instead of
   rebuilding them (#109).

### 6.2 Reading it: the two shapes

**HTML server — per `/pull` request:**

```rust
self.cache.reap(TAIL_TTL_MS);                      // lazy eviction; no background thread
let shared = match self.cache.touch(id) {          // resident? bump its clock, take it
    Some(ss) => ss,
    None => /* admit it — §5.3 */,
};
shared.advance()?;                                 // fold newly-appended source lines, HERE,
                                                   // on this request's thread
let (epoch, gen, n_committed, n_provisional) = shared.counters();   // idle fast-path: is there
                                                   // anything to send at all?
let (delta, extra) = shared.open_delta_with(|store, committed, d| { … });  // one consistent read
```

`advance()` is where folding actually happens, and it holds that session's lock for its duration —
a cold fold of a large transcript can hold it for a minute. That is by design (the requester pays,
an idle session costs nothing) and is why §9's concurrency rules matter.

**TUI — per tick:**

```rust
if let Some(Ok(delta)) = cache.poll_view(id, ArcLog::memory) {
    view.splice(delta);        // committed_delta + provisional, spliced at `changed_from`
}
```

`poll_view` returns `None` for *"nothing to do"* — either the id is not resident (on a durable
cache: never admitted) or the file has not changed. `Some(Ok(delta))` carries only what moved.

### 6.3 How a client holds a session (and never locks it itself)

The type looks alarming if you are new to Rust — a struct whose only field is a mutex — but the
shape is doing something specific, and it is what lets many threads share one session safely.

```rust
pub struct SharedSession<S: BlockStore = InMemoryStore> {
    inner: Mutex<Inner<S>>,     // private field, private type, zero public fields
}
```

**A client never sees the lock, and never mutates the state.** `Inner` is private, all of its
fields are private, and nothing outside `cache/shared.rs` so much as names `.inner`. Everything a
client can do is a method on `SharedSession`, and every one of them looks like this:

```rust
pub fn counters(&self) -> (u64, u64, usize, usize) {   // ← `&self`, NOT `&mut self`
    let g = super::lock_recover(&self.inner);          // lock taken HERE, inside
    (g.epoch, g.provisional_gen, g.follower.committed_len(), g.n_provisional)
}                                                      // guard dropped HERE, on return
```

Two things follow, and they are the whole reason for the design:

- **`&self`, not `&mut self`.** Rust would normally require `&mut` to change a struct, and
  `&mut` cannot be shared. Putting the `Mutex` *inside* means every method can take a plain shared
  reference and still mutate what it guards — the pattern is called *interior mutability*. That is
  what makes `Arc<SharedSession>` work: `Arc` gives you many owners but only shared (`&`) access,
  which without the inner mutex would be read-only and useless.
- **The lock is not part of the API.** A client cannot forget to lock, lock in the wrong order,
  or hold the guard too long, because it never holds one. Compare the alternative shape,
  `Mutex<Session>` handed to the client: every call site would then be responsible for the
  discipline, in a codebase where two request threads and a TUI event loop all touch the same
  session.

So a client's whole interaction is:

```rust
let session: Arc<SharedSession<RecordStore>> = /* from admit or touch */;
session.advance()?;                    // mutates: folds new bytes. No lock in sight.
let (epoch, gen, nc, np) = session.counters();   // reads. Same.
// …and when this Arc is dropped, this thread's claim on the session is over (§7.1).
```

**When you need two facts that must agree**, methods take a *closure* instead of returning the
guard — the work happens while the lock is held, and the lock still cannot escape:

```rust
pub fn store_read<R>(&self, f: impl FnOnce(u64, &S) -> R) -> R {
    let g = super::lock_recover(&self.inner);
    f(g.epoch, g.follower.store())      // your closure runs HERE, under the lock
}

// so a caller reads an epoch and the bytes it describes, consistently, without ever
// being able to keep either past the call:
let bytes = session.store_read(|epoch, store| {
    if epoch != want { return Err(StaleEpoch) }
    Ok(store.read_range(from, to))
});
```

Three practical notes, in rough order of how likely they are to bite:

1. **Do not call another `SharedSession` method from inside one of those closures.** Rust's
   `Mutex` is not re-entrant: the inner call would wait for a lock the outer call is still
   holding, and the thread deadlocks — forever, with no error.
2. **A long method blocks everyone else on that session.** `advance()` holds the lock for the
   whole fold (§7.1). That is deliberate — the alternative is two folds — but it is why a cold
   fold of a large transcript makes other pulls of that session wait.
3. **`lock_recover`, not `.lock().unwrap()`.** If a thread panics while holding a `Mutex`, Rust
   marks it *poisoned* and every later `.lock()` returns an error; `.unwrap()` would turn one
   panic into a permanent failure for that session. `lock_recover` takes the guard anyway,
   because the state it guards is either per-entry (and self-heals through the epoch/resync
   protocol) or rebuilt by the owner via `poisoned()`.

### 6.4 Getting blocks out

Three accessors, and picking the wrong one is the usual mistake:

| you want | call | cost |
|---|---|---|
| the blocks themselves, to render now | `committed_arcs()` → `Vec<Arc<Block>>` | `Arc` clones — cheap; the cache keeps the authoritative copy |
| what the STORE holds, to hand to a resume | `committed_bvs()` → `Vec<S::Bv>` | cheap; for `RecordStore` these are `{offset, len}` locators, not content |
| the open turn plus store facts, atomically | `open_delta_with(\|store, committed, d\| …)` | one lock, one consistent read — use this when you need two facts that must agree |

The rule behind the table: `Bv` is the *storage* projection and `Arc<Block>` is the *content* one.
The HTML store deliberately cannot read its `Bv`s back into blocks (a wire record is a one-way
projection), so a consumer that wants content must use `committed_arcs` or the pull protocol —
the type system enforces it.

### 6.5 Sidecars — per-session state the cache holds for you

The `A` type parameter is an opaque slot the cache stores per session and never interprets. Two
usage shapes, and they are not interchangeable:

```rust
// PARK-AND-TAKE — for state that survives an eviction and is re-adopted once.
// The TUI parks a child's measured heights + fold/scroll when the frame is evicted:
cache.aux_put(&key, view.into_sidecar());
if let Some(sc) = cache.aux_take(&key) { view.adopt_sidecar(sc); }   // move semantics: taking
                                                                    // it out is the point —
                                                                    // a sidecar is never
                                                                    // stale-shared

// ALWAYS-ON — for state read and mutated in place every request.
// The HTML server keeps titles, parent pointers, cwd and the cached open-turn render:
let title = cache.aux_with(id, |a| a.title.clone());
cache.aux_with(id, |a| a.parent = Some(parent_id.to_string()));
```

Two properties worth knowing: sidecars have **registry lifetime** — reaping a resident does *not*
drop its sidecar, which is exactly why park-and-take works across an eviction — and **the consumer
owns validity**.

#### There is no way to INVALIDATE a sidecar, by design

The cache never touches the contents, so it can never notice they went stale. Nothing says *"the
state you parked is wrong now"* — a producer's only lever is `aux_take` (take it out and drop it)
or `aux_put` (overwrite). Which means validity is decided **at adopt time, by the adopter, from
what it can see** — and the answer is always *discard and recompute*, never *update in place*.

That is defensible: the parked state is derived, so recomputing it is always available, and the
alternative (the cache learning enough about `A` to patch it) would put view geometry inside a
cache. But it puts the whole weight on the validity key, and **the two consumers key on very
different things**:

```rust
// HTML — a CONTENT key. `(epoch, gen, len)`: within a gen the finalized provisional is
// append-only and the committed prefix frozen, so an equal key means identical records.
// A tail reshape bumps `gen`, so the cached render is dropped exactly when it stops being true.
let prov_key = (d.epoch, d.provisional_gen, d.provisional.len());
let cached = self.cache.aux_with(id, |a| {
    a.prov_render.as_ref().filter(|(k, _)| *k == prov_key).map(|(_, l)| l.clone())
});

// TUI — a LENGTH key. `adopt_sidecar` compares nothing but counts:
if sc.heights.len() != self.blocks.len() || sc.collapsed.len() != self.blocks.len() {
    return false;   // "the session changed shape while evicted — fresh measure"
}
```

**The question this raises** (and the doc had glossed): what about a reshape that preserves the
count? A tool result back-patching its call, or a finalization that regroups the tail, can change
a block's rendered height with `blocks.len()` unchanged — and then stale `heights` are adopted
*and marked current* (`dirty_from = None`). Width is not the hole: `adopt_sidecar` takes the
parked width and `layout` re-measures when the real one differs. Content is.

In practice the TUI is mostly saved by two things, neither of them the key: a re-descended child
is usually **completed** (its blocks cannot change), and a **live** one gets a delta on the next
poll that sets `dirty_from` and re-measures from the change point. So this is a latent sharp edge
rather than a standing bug — but "mostly saved by the next event" is not a validity discipline,
and it is exactly the sort of thing that becomes a bug when someone parks a sidecar somewhere new.

Three ways to close it, for whoever picks this up:

| | approach | cost |
|---|---|---|
| a | give the TUI a content key like the HTML side's — park the session's `(epoch, gen, committed_len)` with the sidecar and compare on adopt | small, symmetric with the existing HTML discipline, and it makes the two consumers explicable as one rule |
| b | adopt conservatively: keep folds and scroll (position-keyed, cheap to re-apply), always re-measure heights | loses the measure-pass saving that is the sidecar's main point |
| c | let the cache invalidate on session change — a generation counter it bumps on every commit, stamped into the slot | the cache would be enforcing a validity rule for state it cannot read; rejected on the same grounds as everything else in this doc |

Leaning **a**. It is the one that makes `aux` a single concept rather than a slot two clients use
with different rigor — and it is a change to the TUI, not to the cache, which is the right side of
the seam for it.

### 6.6 Residency: keeping a lid on memory

| call | who | policy |
|---|---|---|
| `reap(ttl_ms)` | HTML server, lazily on each `/pull` | drop residents idle longer than `ttl_ms` — but never one still in use (#168) |
| `reap_over_budget(n, pinned)` | TUI, after a descend | keep at most `n` sub-agent residents, least-recently-touched go first; the pinned root never counts |
| `remove_pull(id)` | either, rarely | drop one resident immediately — used when it turns out poisoned |

An evicted resident is not a loss: its registry entry survives, so the next `admit`/`poll_view`
re-materializes it — and on a durable cache that is a **resume**, not a re-fold, which is what
makes an aggressive TTL affordable.

### 6.7 Letting go

```rust
cache.release(id);      // quiesce + unlock ONE session; it stays resident and readable
cache.release_all();    // everything — both `process::exit(0)` sites call this explicitly,
                        // because they skip destructors
// Drop does release_all() too: it covers every `?` on every error path.
```

`release` **quiesces** rather than merely flushing: a released session that kept its writer would
append to an entry this process no longer owns — two writers on one entry, which is the whole
thing the lock exists to prevent.

### 6.8 What there is deliberately NO API for

- **Deleting an entry.** Nothing exposes "forget this session's durable state". Entries go by age
  through `admit::gc` (`KEEP_FOR`, 14 days; a lock buys another 16), swept once when a durable
  cache is built. A client that wants an entry gone deletes the directory itself — which is one
  reason `entry_dir` stays public (§10).
- **Sharing an entry.** There is no read-only open, no shared mode. One writer, always; a second
  asker is `Denied` and routes to the holder (#163).
- **Asking whether an id is admitted.** `is_registered` answers *known*, not *owned*. The honest
  test is to call `admit` and read the outcome.
- **Invalidating a sidecar.** The cache cannot read `A`, so it can never know it went stale. See
  §6.5 — validity is the adopter's job, and today the two consumers do it with different rigor.

### 6.9 Which of these are durable-only (today: most of them)

A fair reading of §6.1 is *"this is the lifecycle of a **durable** cache"* — and today that is
exactly right, which is itself part of the problem:

| method | with durable wiring | without it |
|---|---|---|
| `register` / `resolve` / `aux_*` | works | works |
| `shared_session` / `poll_view` / `reap` | works | works |
| **`admit`** | the only path to a session | **always `Denied(NoCacheFlag)`** |
| **`publish`** | writes the note | **silently `false`** |
| **`release`** | quiesce **+** unlock | quiesce only |
| **`release_all`** (and `Drop`) | quiesce + unlock everything | **returns immediately — quiesces nothing** |

So there are really **two lifecycles** today, and `poll_view` picks between them with a branch on
the cache's own field:

```rust
let ss = if self.durable.is_some() {
    self.touch(id)?                       // durable: only `admit` may materialize
} else {
    self.shared_session(id, || …)         // otherwise: materialize on demand, no lock
};
```

That branch is the smell in one line. A type that behaves as two different things depending on
whether one `Option` field is `Some` is two types.

(Also visible in the table: `release` quiesces regardless, `release_all` does not quiesce at all
without durable wiring. Harmless today — a cache with no wiring has no writer attached, so there
is nothing to stop — but the two disagree, and only because the `Option` check sits in different
places in each.)

**In the target this collapses to one lifecycle** — but not by making everyone pay for it. The
provider is mandatory (§7.1), so `admit` is never a no-op; and a provider that cannot be contended
for implements one function and defaults the rest (§3.1), so an in-memory or private-root client
gets `open → Owned` and nothing else. `poll_view` loses its branch: every session comes into being
the same way, and what varies is how much the provider has to do about it.

The deeper point is that **`admit` is not a durability concept at all**. It reads as one because
its implementation is "claim a lock on a directory". What it actually establishes is *one owner,
therefore one tailer* — and #169 proved that matters with zero disk involved: two folds of one
session in one process means double the CPU, two tails that need not agree, and (only
incidentally, because this store happens to be durable) a corrupt log. Persistence made the
symptom loud; the rule was never about persistence.

That gap between what these three mean and what they are named after is the audit in miniature.

### 6.10 Every method, and who calls it

| method | HTML server | TUI | monitor |
|---|---|---|---|
| `register` / `register_new` | roots at startup, children on a parent's pull | root + each descended child | via the service, per scan |
| `admit` | first `/pull` of a session | opening a session, descending into a child | via the service |
| `publish` | after bind, on admission (`{port}`) | at startup (`{pane}`) | via the service |
| `touch` | every `/pull` | — | — |
| `shared_peek` | `/records` range reads (no clock bump) | — | — |
| `advance` | every `/pull` | via `poll_view` | — |
| `poll_view` | — | every tick | — |
| `open_delta_with` / `counters` | every `/pull` | — | — |
| `committed_arcs` | — | building a view | — |
| `aux_with` | titles, parents, cwd, render cache | — | — |
| `aux_put` / `aux_take` | — | park/adopt evicted frame state | — |
| `reap` | every `/pull` | — | — |
| `reap_over_budget` | — | after a descend | — |
| `release` | poisoned-session recovery | on switch | — |
| `release_all` | `Drop` | explicit at exit | — |
| `resident_tasks` | — | — | rail counters, without materializing |

## 7. Which lock is which, and why only one of them is a convention

### 7.1 The levels

Three different questions get answered by things all called "the lock", and keeping them apart
explains most of the last three bugs:

| level | question | acquired by | released by | who owns it | needed when |
|---|---|---|---|---|---|
| **process** | may this PROCESS write the entry? | `admit` (→ `LOCK` file) | `release` / `release_all` / `Drop` | the **provider** | only on a **shared** root (§3.1) |
| **thread — ownership** | may this THREAD keep the session alive? | holding the `Arc<SharedSession>` that `admit`/`touch` returns | **dropping it** — no call | the **cache** | **always** |
| **thread — exclusion** | may this THREAD touch it *right now*? | the session's own `inner: Mutex<Inner>`, taken inside every method | released when that method returns | the **session** | **always** |

**`admit` / `release` / `publish` are the PROCESS-level protocol.** Neither thread-level row has a
protocol to call: ownership is by value (hold the `Arc`, drop the `Arc`) and exclusion is taken and
released inside each call.

### So how is it guaranteed that only one thread pulls?

**It is not — and it does not need to be.** Any number of threads may pull one session at once.
What is guaranteed is that they all pull the *same* session, and that only one is inside it at a
time:

- **One resident per id** — the singleton in `pull_residents`, protected at creation by the
  `admitting` gate (#169). Every puller gets an `Arc` to the same `SharedSession`, so there is one
  follower, one store, one writer, however many callers.
- **One thread inside it at a time** — every public method on `SharedSession` takes `inner`
  (`advance`, `poll_view`, `open_delta_with`, `counters`, `store_read`, `quiesce`,
  `attach_writer`, `committed_arcs`; `pull` takes it twice). `advance()` holds it for **the whole
  fold**, so a second puller blocks, and then advances from where the first left off — usually
  finding nothing new, which is exactly right.

That is why several browser tabs on one session are fine, and always were: they are not N tailers
racing, they are N readers of one. The bugs were never concurrent *pulling* — they were concurrent
**creation** (#169: N callers each built a session because none had been installed yet) and
premature **destruction** (#168: a resident dropped while a thread was still folding into it).
Both are lifetime problems around the singleton, not exclusion problems inside it.

The property worth knowing, because it is a real cost rather than a bug: a cold fold holds that
session's mutex for its whole duration — a minute or more for a large transcript — and every other
puller of that session waits. They are not doing redundant work while they wait, which is the
point, but they are waiting.

The process-level lock is deliberately **blind to your own process**:

```rust
// lock.rs — acquire()
if h.pid != std::process::id() && alive(&h) {
    return Ok(Taken::Held(h));     // a peer holds it
}
// ours already, or the holder is gone — reclaim
```

That is correct — a process must be able to re-take an entry it already owns, or a reap-then-
re-admit would deadlock against itself — and it means the file lock offers **exactly zero
thread-level guarantee**. Every in-process bug this month lived in that gap: #168 (reap dropped a
resident still being folded) and #169 (N concurrent admissions each opened a store) were both
*thread*-level failures while the *process*-level lock was working perfectly. The durable path did
not cause them; it made them loud, by turning "two folds" into "a corrupt file".

Two consequences for the target:

- **Thread-level exclusion stays on the cache**, always, for every provider. It is not a
  durability feature: two folds of one session in RAM is still double the CPU and two tails that
  need not agree. This is why §9 puts the `admitting` gate on the cache rather than the provider.
- **Process-level exclusion is the provider's, and only some providers have one.** An in-memory or
  private-root provider (§3.1) has no `LOCK`, no note, no `Denied` — and loses nothing, because
  there is no other process to exclude.

The rest of this section is about the **process-level** lock, whose lifetime is managed by hand.

### 7.2 What is actually loose today

Everything about who holds an entry is enforced by **calling the right methods in the right
order**. `admit` takes the lock and records it in a side map; `release` drops it, keyed by a
string; `Drop` catches whatever was forgotten. Nothing in the type system ties *holding the lock*
to *being the thing that writes*. In a single-threaded client that is merely fragile. In a
multi-threaded one — which both frontends are — it is a live hazard.

```rust
// the lock lives HERE, keyed by a string, in a map beside the sessions
struct Durable { owned: Mutex<HashMap<String, Owned /* { dir: PathBuf } */>>, … }

// the writer lives HERE, inside the session
pub fn attach_writer(&self, w: MetaWriter) { g.meta = Some(w); g.frozen = false; }
pub fn quiesce(&self)                      { g.meta = None;    g.frozen = true;  }
```

Two facts that must agree — *we hold the entry* and *we are writing to it* — are stored in two
places and kept in step by hand. Three ways that goes wrong:

1. **`remove_pull(id)` alone is a footgun.** It removes the resident and does *nothing else*: no
   quiesce, no unlock. So the lock stays held, and any thread still holding an `Arc` to that
   session keeps **writing** — while the next `admit` finds nothing resident, is granted (same
   pid), and opens a **second store on the same backing**. That is #169 again, reachable through
   one public method called on its own. Today it is safe only because its one caller happens to
   call `release` first, and only since #163 put them in that order.
2. **`release(id)` from another thread stops a fold silently.** Thread A is inside `advance()`;
   thread B releases. A's session is frozen, so A's next `advance()` returns `Ok(false)` — *"no
   new data"*, indistinguishable from an idle session. Nothing errors. The session simply stops
   following, which is the exact failure `thaw`'s doc comment describes as "worse than serving it
   uncached".
3. **The two release paths disagree** (§6.9): `release` quiesces even with no durable wiring;
   `release_all` returns before quiescing anything. Harmless today, but the difference is
   accidental — the `Option` check sits in a different place in each.

`Drop` covers the honest mistake (forgetting to release). It cannot cover any of these, because
each is a *correctly compiling call sequence* that means something different from what the caller
intended.

### 7.3 The fix falls out of the refactor: the lock lives in the writer

`quiesce` already drops the `MetaWriter`. Make the writer **own the lock**, and the two facts
become one:

```rust
// The provider hands back a writer that HOLDS the entry. There is no other way to have one.
pub enum Opened<P: BlockStore, N> {
    Owned { store: P, loaded: Vec<P::Bv>, origin: Origin, writer: EntryWriter },
    Denied(Denial<N>),
}

/// Dropping this releases the entry. That is the whole mechanism.
pub struct EntryWriter { meta: MetaWriter, _lease: EntryLease }
impl Drop for EntryLease { fn drop(&mut self) { lock::release_any(&self.dir) } }
```

Then:

- **`release(id)` becomes "drop the writer"** — one operation instead of two that must agree, and
  the unlock happens at exactly the moment writing stops, not near it.
- **`remove_pull` stops being dangerous.** Dropping the last `Arc` drops the writer, which drops
  the lease. A thread still holding an `Arc` still holds the lock — which is *correct*: it is
  still writing.
- **The `release` / `release_all` asymmetry disappears**, because neither exists as a separate
  step.
- **`Durable::owned` disappears** — the map of "which directories we hold" was only ever a way to
  find the lock again from a string, and there is nothing left to find.
- **#109 retention still works.** The lock is tied to the WRITER, not to the session: `quiesce`
  drops the writer (unlocking) and the session stays resident and readable, which is precisely
  what retention means. Re-admission calls `attach_writer` with a fresh lease.

What this really does is make the two levels **symmetric**: the thread level is already ownership
by value (hold the `Arc`, drop the `Arc`), and this gives the process level the same shape (hold
the writer, drop the writer). `release(id)` — a string-keyed side-channel call that mutates state
somewhere else — stops existing, and with it the whole class of "called in the wrong order".

The remaining hazard is hazard 2, and it is not fixable by ownership — a caller that deliberately
stops another thread's fold is asking for what it gets. What ownership fixes is that it can no
longer happen *by accident*, through a method that looks unrelated.

### 7.4 Why this belongs to this refactor rather than after it

The lease has to be created where the lock is taken, and after this change that is the provider —
so `Entries::open` is the only place it can come from. Retrofitting RAII later would mean touching
the same `admit` body twice. Step 2 of §11 is where it lands.

## 8. Four reductions this is really made of

The value here is mostly **deletion**. It is not a new abstraction so much as putting one name on
a thing that is already there, and then dropping what it makes redundant.

1. **`Option<Durable>` goes away.** No production caller selects an ephemeral cache any more —
   #163 removed the fallback, #165 made `--no-cache` a real cache at its own root. The provider
   becomes mandatory, and `Unavailable::NoCacheFlag`, `Default`, and `ephemeral()` go with it. A
   host that genuinely wants nothing on disk implements a `Memory` provider that grants every
   request and locks nothing (§3.1 — it is one function) — a better answer than a `None` that
   makes every method check whether it is real.
2. **`Note` moves off `DurableStore` onto `Entries`.** A note describes the *process holding the
   lock* — a port, a tmux pane — not the store that writes blocks. Every use already proves it:
   `lock::read::<HtmlNote>`, `Holder<HtmlNote>`, `cache.publish(id, HtmlNote { port })`. It sits
   on the store today only because the store was the nearest per-frontend type.
3. **`gc` moves to the client.** It is a constructor side effect of `durable()` today; with no
   root on the cache it belongs where the provider is built. #165 moving `throwaway_root` /
   `run_dir` / `reclaim` out of `cache::admit` into `present::sys` — because the client picks its
   own directories — is the precedent.
4. **`Origin` / `ColdReason` survive intact.** They are diagnosable on purpose and the rejection
   tests assert on them; "the cache did not help, and here is which of five reasons" is a support
   answer. An opaque "here is a store" return would throw that away.

---

## 9. What #169 adds

`admit` turned out to have no mutual exclusion: between a caller finding no resident and the cache
installing one, every other caller also found none — so N concurrent first-pulls opened N stores
on one backing and folded into it at once. (On disk: record log lines carrying up to six records,
scrambled and duplicated.)

That is evidence **for** this refactor. The reason a missing lock stayed invisible is that `admit`
interleaves registry lookup, residency, lock acquisition, entry-dir computation, backing open and
stream alignment in a single function, where *"who may open a store"* is nobody's stated job.

Two consequences the migration must carry, both easy to lose when a body moves behind a trait:

- **The gate belongs to the CACHE, not the provider.** `admitting: Mutex<()>` guards the window
  between "no resident" and "installed", and residents are the cache's own state. A provider that
  serialized *itself* would still let two callers race to `install`. The provider's `open()` runs
  *under* the cache's gate and needs none of its own.
- **The double-check is part of `admit`'s contract.** A live (non-frozen) resident **is** the
  admission: the caller that loses the race takes the winner's session rather than opening a
  second one beside it. Drop that half and the bug returns with nothing failing except the test
  written for it.

---

## 10. What deliberately does NOT move behind the provider

`entry_dir`, `Presentation` and `lock::read` have consumers that never touch the cache and must
keep working:

- **`claude-monitor/src/index.rs`** reads each visited entry's `meta.jsonl` lock-free for the
  rail's counters (R7 — no fold on the index path).
- **`html_export::existing_server`** reads a lock's note to decide a hand-off *before* any session
  is admitted.

They stay public on the filesystem provider's own module. A provider is not a private box; it is
the layout, and other tools legitimately read that layout.

---

## 11. Migration

Four steps, each independently gateable. Two tests are the oracles at **every** step, not just the
last:

- `a_resumed_record_log_is_byte_identical_to_a_cold_one` — the byte gate never renders a *resumed*
  fold, so this is the only thing watching the code this refactor touches most.
- `concurrent_admissions_of_one_session_open_exactly_one_store` — step 2 moves the very body the
  #169 gate protects, and this is the only thing that notices if the protection is left behind.

| step | change | risk |
|---|---|---|
| 1 | Move `Note` from `DurableStore` to a standalone per-frontend type | none — a type moves, no behaviour |
| 2 | Extract `Entries` with one impl (`FsEntries`); `SessionCache::durable` keeps its signature and builds one internally | the witness re-factoring (§4) lands here; all tests unchanged |
| 3 | Flip to `SessionCache::new(entries)`; frontends and the monitor build their own; `gc` moves out | call-site churn, mechanical |
| 4 | Delete `Option<Durable>`, `ephemeral()`, `Default`, `NoCacheFlag`; add `Memory` for the tests that used `ephemeral()` as a type-level state | test-only |

## 12. Open questions

1. **Does `cache_home()` / `default_root()` belong to the cache at all?** They live in
   `cache::admit` today and are already called by clients. Candidates: move to `present::sys`
   beside `throwaway_root`, or keep them as the `FsEntries` default.
2. **Is `Presentation` the provider's business or the client's?** After this change the cache never
   sees it. It could equally be folded into the root the client passes (`<root>/html`), leaving
   the enum to the two places that need to *name* the namespace (the monitor's rail read).
3. **Does `admit` keep its name?** Once it is "become the single owner of this session's
   tailer" rather than "claim a lock on a directory", the word is doing less work than it looks
   like it is (§6.8). `own` / `claim` / `take` are candidates. Renaming touches every call site
   and every test that matches on `Admission`, so it is worth deciding at step 3 of §11 or not at
   all.
4. **Does `A` (the sidecar slot) stay on the cache?** It is view state keyed by session, so
   probably yes — but it is the third map on a type we are trying to narrow.

---

## 13. Resolution — one cache, three providers

Reviewed end to end (2026-08-10) against four requirements: crate responsibilities, the three
real use cases, the locking story, and an audit of every method. The shape that survives:

**One `SessionCache`, one `Entries` trait, three implementations.** Not two cache classes, not
three.

### 13.1 The three use cases are three providers

The deciding observation: every *thread-level* guarantee — one resident per id, one admission at
a time, the session's own mutex, reap-vs-`strong_count` — is **identical across all three use
cases** and sits above the seam. What differs is only what happens at the entry: whether there is
a lock, a meta stream, a resume. That is provider-sized, not class-sized.

| use case | provider | lock | meta/resume | on re-materialize |
|---|---|---|---|---|
| (a) `--no-cache`, one-off | `Transient` | none | none | `store.reset()` + cold refold |
| (b) viewer, per-session durable | `PerSession { root, versions, alive, note }` | one `LOCK` per `<session, frontend>` entry | yes | resume from `replay_from` |
| (c) monitor, whole-machine | `SingleWriter::claim(root)` | **one** root `LOCK`, taken at construction | yes | resume from `replay_from` |

```rust
pub trait Entries<P: BlockStore> {
    type Note: Serialize + DeserializeOwned + Clone;
    fn open(&self, id: &str, src: &Transcript, ours: Option<u64>,
            make_store: &dyn Fn(&Path) -> io::Result<P>) -> Opened<P, Self::Note>;
    fn publish(&self, id: &str, note: Self::Note) -> bool { false }   // defaulted
}

pub enum Opened<P: BlockStore, N> {
    /// `writer` owns the entry lock (§13.3). `None` for Transient — nothing to record, nothing held.
    Owned { store: P, loaded: Vec<P::Bv>, origin: Origin, writer: Option<EntryWriter> },
    Denied(Denial<N>),
}
```

**(a) revises #165, and is the better reading of the flag.** `--no-cache` means *no durable
cache* — not "a durable cache at a private root". The on-disk artifacts a `--html --no-cache` run
writes (the bundle, a `records.jsonl` the range reads serve from) are **serving artifacts in run
scratch**, wiped at the next start (`start_server` already wipes `run_dir`); they are not a cache
anyone resumes from. So `Transient` has no meta stream, no `LOCK`, no note, no `Denied` — `open`
always returns `Owned { origin: Cold(NoPriorCache), writer: None }`, and the cache's cold path
calls `store.reset()` first, which is what makes the old unbounded-growth path structurally
impossible (the growth was append-without-reset). `throwaway/<pid>` as a *durable* root is
retired; the `runs/<pid>` scratch and its `reclaim` sweep stay. A second `--no-cache` run of the
same session is two independent views by design — no rendezvous, no redirect, which is exactly
what the flag is for.

**(c) dissolves three standing warts at once.** The monitor's provider takes the root lock in its
constructor — `SingleWriter::claim(root)` returns `Ok(provider)` or `Served { url }` (#166's
redirect becomes a constructor outcome, and `claim_root` in `main.rs` becomes provider
construction). Per-entry `open` then never locks: process-level exclusivity is already
established, so the 56-of-56 uncontendable entry locks disappear (§3.2), the only-possible-denial
false positive disappears, and with it the #163 self-guard. Entries keep their directories and
meta streams — the rail's lock-free read is layout, not locking.

### 13.2 Why one cache class

Two- and three-class designs were explored and rejected on the same grounds:

- **The read path is the bulk of the API** — touch/peek, poll_view, reap, aux, the pull plumbing
  — and it is byte-identical across use cases. Separate classes either duplicate it or forward
  it; Rust has no inheritance, delegation is hand-written, and `Deref` abuse to fake it is worse
  than the generic. The consumers (`serve.rs`, `app.rs`) are each written once against one type.
- **A wrapper (`Durable<SessionCache>`) splits the admission from the residency it must guard.**
  The #169 gate has to close the window between "no resident" and "installed", and the residents
  map lives in the inner type — a wrapper serializing itself still races on `install`. The gate
  and the map must be one type's business.
- **Sharing at a lower level happens in the provider module, not in a cache hierarchy.**
  `PerSession` and `SingleWriter` share `lock.rs`, the meta stream, `entry_dir`, `gc` — all in
  `cache::fs`, which is also where `Presentation` retreats to and what the monitor's index and
  `existing_server` keep reading (§10).

A trait rather than an enum of providers because `Note` is deliberately typed per frontend (#96)
and `Transient` must exist without any fs dependency. The third type parameter is absorbed by the
per-frontend aliases exactly as §3.3 argued.

### 13.3 The locking story, complete

| moment | mechanism | level | owner |
|---|---|---|---|
| provider construction | `SingleWriter::claim` → root `LOCK` or `Served(url)` | process, whole cache | provider |
| admission | `admitting` gate (a critical section, not a claim) | thread | cache |
| admission | `PerSession` entry `LOCK` | process, per session | provider |
| resident lifetime | `Arc` singleton; `reap` spares `strong_count > 1` | thread | cache |
| serving | the session's `inner` mutex; `advance` holds it for the fold | thread | session |
| release | **RAII**: `EntryWriter` = `MetaWriter` + lease; drop ⇒ unlock | process | writer |

The last row is §7.3 adopted: `release(id)` stops being a string-keyed call that mutates state
somewhere else. `quiesce` drops the writer, dropping the writer releases the lock at exactly the
moment writing stops; `release_all` is "quiesce everything"; the `release`/`release_all`
asymmetry (§6.9) and the `remove_pull` footgun (§7.2) both cease to exist. **The note lives
wherever the lock lives**: on the entry for `PerSession`, on the root for `SingleWriter`, nowhere
for `Transient`.

### 13.4 The method audit — what survives, what dies

Deleted, with the reason:

- **`shared_session(id, open)`** — the get-or-create that bypasses admission. It has exactly one
  production caller (`poll_view`'s non-durable branch), and it is the two-writers hazard as an
  API. With `Transient`, `admit` is cheap for the cache-less case, so **`admit` becomes the only
  creator** and `poll_view` loses its `durable.is_some()` branch — the §6.9 smell, gone.
- **`admit`'s `alive` parameter** — a per-call closure rebuilding what is provider configuration.
  `PerSession` takes the liveness rule (and the self-port guard) once, at construction.
- **`ephemeral()` / `durable()` / `Default`** — one constructor, `SessionCache::new(entries)`.
- **`Denial::Unavailable::NoCacheFlag`** — no flag selects a cache that cannot serve.
  `NoLivenessCheck` and root-level `UnwritableRoot` move to provider *construction* errors — the
  machine's problems surface when the client builds the provider, not per session at admit time.
  `UnknownSession` stays (it is the registry's, i.e. the cache's). `Held` stays, `PerSession` only.
- **`release(id)` as an unlock** — RAII (§13.3). The name survives as "quiesce".
- **TUI's `publish` call** — its note (the tmux pane) is known at construction, so `PerSession`
  takes an initial note and writes it at acquire. `publish` remains only for facts that arrive
  late — the HTML port — which is the one caller that ever needed it.
- **`DurableStore::Note`** — moves to `Entries::Note`; the store trait drops its serde bounds and
  is again purely "how blocks become bytes".
- **`Presentation` and `Versions` in the cache signature** — provider config, in `cache::fs`.

Kept, deliberately: the registry four (`register`/`register_new`/`is_registered`/`resolve`);
`touch`/`shared_peek` (the two honest getters); `reap`/`reap_over_budget` (two policies, two
real owners); the `aux` trio (the TUI still parks view state; #170 moved only the *browser's*
equivalent client-side); `make_store` as a per-call closure (cwd and fold are per-session facts
only the caller has); `admit`'s name (it now means "become the sole owner of this session's
tailer", and the churn of renaming every call site buys no clarity that doc comments cannot).

### 13.5 The call sites, after

```rust
// TUI
let entries = PerSession::new(root, Presentation::Tui)
    .versions(Versions::current(None))
    .note(TuiNote::here());                       // known now; written at acquire
entries.gc();
let cache = TuiCache::new(entries);
…
match cache.admit(id, |dir| ArcLog::open_append(&dir.join("blocks.jsonl"))) { … }

// HTML server (shared root)
let entries = PerSession::new(root, Presentation::Html)
    .versions(Versions::current(Some(render_flavor(&fold))))
    .liveness(html_alive(port.clone()));          // pid + port probe + self-guard, once
…
match self.cache.admit(id, |dir| open(&dir.join("records.jsonl"))) { … }

// HTML server (--no-cache): same calls, different provider — and NOTHING else changes
let cache = SessionCache::new(Transient::in_dir(run_dir));   // wiped next start

// monitor
let entries = match SingleWriter::claim(&root)? {
    Claimed::Ours(e) => e,
    Claimed::Served { url } => { open_url(&url); return Ok(()); }   // #166, at construction
};
```

The reading test for requirement 4: every remaining call says *what* it wants — never how the
entry is found, guarded, or kept — and the same `admit`/`touch`/`poll_view` lines serve all three
use cases with only the constructor differing.

### 13.6 Open questions, closed

1. `cache_home()`/`default_root()` → stay client-side in `present::sys` (#165's precedent).
2. `Presentation` → lives in `cache::fs` as provider vocabulary; out of the cache's signature.
3. `admit` keeps its name (§13.4).
4. The sidecar slot `A` stays on the cache: registry-lifetime view state keyed by session is
   residency's business; its validity discipline is the adopter's (§6.5) and is unchanged here.

### 13.7 Migration, revised

§11's four steps hold with two amendments: step 2 extracts `cache::fs` with **two** impls
(`PerSession`, `SingleWriter`) and the `EntryWriter` lease lands there (§7.4 — same body, touch
it once); step 4 adds `Transient`, rewires `--no-cache`, deletes `shared_session`, and converts
the monitor — which is also the step that deletes its 56 entry locks and the self-guard. The two
per-step oracles (§11) stand, joined by a third: `--no-cache` growth stays bounded across reap
cycles (the test that would have caught #158 by construction).

---

## Appendix A. The key types at a glance

> **The authority is the rustdoc, not this table.** It is generated from the source on every push
> to main and denies warnings, so it cannot drift:
> **<https://tanghong123.github.io/claude-replay/>** — or `cargo apidoc --open` locally. It
> documents everything, `pub(crate)` and private included (`--document-private-items`).
>
> What follows is a *curated subset*: the handful of types this design turns on, grouped by what
> each group is FOR, with the private fields the argument is actually about. Read it as a map of
> which page to open, not as the definition. If the two ever disagree, the rustdoc is right and
> this is stale.

### [`SessionCache<P, A>`](https://tanghong123.github.io/claude-replay/claude_replay_present/cache/struct.SessionCache.html) — the thing being re-cut

```rust
registry:       Mutex<HashMap<String, Transcript>>        // ids → sources
pull_residents: Mutex<HashMap<String, (Instant, Arc<SharedSession<P>>)>>  // ids → live sessions
aux:            Mutex<HashMap<String, A>>                 // ids → sidecars (opaque)
admitting:      Mutex<()>                                 // the #169 admission critical section
durable:        Option<Durable>                           // ← what §3 moves out
```

| group | methods |
|---|---|
| know a session | `register`, `register_new`, `is_registered`, `resolve` |
| own a session | `admit`, `publish`, `release`, `release_all`, `Drop` |
| use a session | `touch`, `shared_peek`, `shared_session`, `poll_view`, `resident_tasks` |
| sidecars | `aux_put`, `aux_take`, `aux_with` |
| residency | `reap`, `reap_over_budget`, `remove_pull` |
| construction | `durable`, `ephemeral`, `new` |

### [`SharedSession<S>`](https://tanghong123.github.io/claude-replay/claude_replay_present/cache/struct.SharedSession.html) — the live session, one per id

```rust
inner: Mutex<Inner<S>>        // the ONLY field; the thread-level exclusion lock of §7.1

struct Inner<S> {             // private, zero public fields
    follower: Box<FollowParser<S>>,  // the incremental fold — and the store `S` inside it
    epoch: u64,                      // cursor-protocol validity token
    provisional_gen: u64,            // open-turn generation
    n_provisional: usize,            // finalized open-turn length (keeps `counters` O(1))
    prev_provisional: …,             // last tick's finalized open turn — the reshape check
    meta: Option<MetaWriter>,        // present ⇔ writing (§7.3 would make it ⇔ holding the entry)
    frozen: bool,                    // quiesced: resident and readable, no longer writing (#109)
}
```

| group | methods |
|---|---|
| advance the fold | `advance` (HTML), `poll_view` (TUI) |
| read content | `committed_arcs`, `committed_bvs`, `session_meta`, `tasks` |
| serve the cursor protocol | `pull`, `pull_delta`, `counters`, `end_cursor`, `epoch`, `provisional_gen` |
| read under the lock | `open_delta_with(closure)`, `store_read(closure)` |
| writer lifecycle | `attach_writer`, `quiesce`, `thaw`, `frozen`, `poisoned` |
| construction | `open`, `with_store`, `resume` |

Every one takes `&self` and locks internally (§6.3).

### The store traits — where persistence actually lives ([`DurableStore`](https://tanghong123.github.io/claude-replay/claude_replay_present/cache/trait.DurableStore.html))

```rust
pub trait BlockStore {                  // engine: what a fold writes into
    type Bv: Clone;                     // the stored form of a block
    fn put(&mut self, b: Block, at: BlockIndex, user_times: &[Option<EpochSeconds>]) -> Self::Bv;
    fn reset(&mut self);                // default no-op
}

pub trait DurableStore: BlockStore {    // …and what makes it survive a process
    type Note;                          // ← §7.2 moves this to the provider
    fn load_from(&mut self, at: u64) -> io::Result<Vec<Self::Bv>>;
    fn load(&mut self) -> io::Result<Vec<Self::Bv>>;   // = load_from(0)
    fn backing_len(&self) -> u64;       // the caller's half of the #109 witness
    fn adopt(&mut self, n: usize, meta: &SessionMeta) -> io::Result<()>;
}
```

### The two stores

```rust
pub struct RecordStore {          // HTML: Bv = RecordLocator { offset, len }
    log: Log,                     //   path + append handle + length
    cx:  RenderCx,                //   fold policy, cwd, transcript — the render context
    emit: EmitState,              //   the resumable render continuation
}
// own methods: open_append, cut_to, log_len, emit_snapshot, read_range
//   …plus one_record (#168's framing check) as a free function

pub struct ArcLog {               // TUI: Bv = Arc<Block>
    path: PathBuf,
    file: Option<File>,           //   None ⇒ memory-only; a session that works, slower next time
    len:  u64,
}
// own methods: memory, create, open_append, load_from, load, truncate_to, backing_len
```

`RecordStore` implements `BlockStore` but NOT `BlockRead`: a wire record is a one-way projection
of its `Block`, so the type system keeps every committed consumer on the pointer path (§6.4).

### `Transcript` — where a session's bytes come from

```rust
pub struct Transcript { agent: Agent, path: PathBuf }
// open, detect, agent, path, parse, parse_enriched, follow, card, load_attachment
```

Cheap and cloneable: the cache stores one per registered id and hands copies out.

### Proposed, not built (§3)

```rust
pub trait Entries<P: BlockStore> {
    type Note;
    fn open(&self, id: &str, src: &Transcript, ours: Option<u64>,
            make_store: &dyn Fn(&Path) -> io::Result<P>) -> Opened<P, Self::Note>;
    fn publish(&self, id: &str, note: Self::Note) -> bool;   // defaulted: false
    fn release(&self, id: &str);                             // defaulted: no-op
}

pub enum Opened<P: BlockStore, N> {
    Owned { store: P, loaded: Vec<P::Bv>, origin: Origin /*, writer: EntryWriter — §7.3 */ },
    Denied(Denial<N>),
}
```

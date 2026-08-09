# Design: taking persistence out of `SessionCache`

> **Status:** proposed (not built). Tracked as task **#167**. Design-only; no code has moved.
> Builds on [`durable-session-cache.md`](durable-session-cache.md) (#96 — BUILT), which is the
> design of the thing being re-cut here. Read §1 and §3; the rest follows from them.

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
    registry:       Mutex<HashMap<String, Transcript>>,   // ids → sources          — a cache
    pull_residents: Mutex<HashMap<String, PullResident<P>>>, // ids → live folders  — a cache
    aux:            Mutex<HashMap<String, A>>,            // ids → sidecars         — a cache
    admitting:      Mutex<()>,                            // one admission at a time — a cache
    durable:        Option<Durable>,                      // ← everything below is NOT
}

struct Durable {
    presentation: Presentation,   // a FRONTEND enum: Tui | Html
    root:         PathBuf,        // a DIRECTORY
    versions:     Versions,       // a FOLD stamp
    owned:        Mutex<HashMap<String, Owned>>,  // which directories we hold locks on
}
```

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
tests reach for a real filesystem to exercise pure residency logic, and — see §7 — a missing lock
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
                            │ owns
                            ▼
        ┌──────────────────────────────────────────────┐
STORE   │ RecordStore / ArcLog (BlockStore)            │   the bytes themselves
        └──────────────────────────────────────────────┘
```

The cache never names a directory. It asks the provider for a backing and is told either what it
got or who has it.

---

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
    /// Ours. `origin` stays as rich as it is today — a denial is a support question (§5.4).
    Owned { store: P, loaded: Vec<P::Bv>, origin: Origin },
    /// Nothing was opened: held by a live peer, or nothing here to open.
    Denied(Denial<N>),
}
```

Today's `cache::admit` + `cache::lock` become **`FsEntries`** — the one implementation shipped,
and the only thing that knows `Presentation`, `Versions`, `gc`, `KEEP_FOR`, or a `PathBuf`.

### 3.1 One Rust wrinkle, named up front

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
is deliberate: it is what makes §8's steps small.

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

## 6. Four reductions this is really made of

The value here is mostly **deletion**. It is not a new abstraction so much as putting one name on
a thing that is already there, and then dropping what it makes redundant.

1. **`Option<Durable>` goes away.** No production caller selects an ephemeral cache any more —
   #163 removed the fallback, #165 made `--no-cache` a real cache at its own root. The provider
   becomes mandatory, and `Unavailable::NoCacheFlag`, `Default`, and `ephemeral()` go with it. A
   host that genuinely wants nothing on disk implements a `Memory` provider that grants every
   request and locks nothing — which is a better answer than a `None` that makes every method
   check whether it is real.
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

## 7. What #169 adds

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

## 8. What deliberately does NOT move behind the provider

`entry_dir`, `Presentation` and `lock::read` have consumers that never touch the cache and must
keep working:

- **`claude-monitor/src/index.rs`** reads each visited entry's `meta.jsonl` lock-free for the
  rail's counters (R7 — no fold on the index path).
- **`html_export::existing_server`** reads a lock's note to decide a hand-off *before* any session
  is admitted.

They stay public on the filesystem provider's own module. A provider is not a private box; it is
the layout, and other tools legitimately read that layout.

---

## 9. Migration

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

## 10. Open questions

1. **Does `cache_home()` / `default_root()` belong to the cache at all?** They live in
   `cache::admit` today and are already called by clients. Candidates: move to `present::sys`
   beside `throwaway_root`, or keep them as the `FsEntries` default.
2. **Is `Presentation` the provider's business or the client's?** After this change the cache never
   sees it. It could equally be folded into the root the client passes (`<root>/html`), leaving
   the enum to the two places that need to *name* the namespace (the monitor's rail read).
3. **Does `A` (the sidecar slot) stay on the cache?** It is view state keyed by session, so
   probably yes — but it is the third map on a type we are trying to narrow.

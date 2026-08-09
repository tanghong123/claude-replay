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
tests reach for a real filesystem to exercise pure residency logic, and — see §8 — a missing lock
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
    /// Ours. `origin` stays as rich as it is today — a denial is a support question (§7.4).
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

### 3.2 One Rust wrinkle, named up front

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
is deliberate: it is what makes §10's steps small.

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
an idle session costs nothing) and is why §8's concurrency rules matter.

**TUI — per tick:**

```rust
if let Some(Ok(delta)) = cache.poll_view(id, ArcLog::memory) {
    view.splice(delta);        // committed_delta + provisional, spliced at `changed_from`
}
```

`poll_view` returns `None` for *"nothing to do"* — either the id is not resident (on a durable
cache: never admitted) or the file has not changed. `Some(Ok(delta))` carries only what moved.

### 6.3 Getting blocks out

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

### 6.4 Sidecars — per-session state the cache holds for you

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
owns validity**. The cache cannot know that a terminal resize invalidated measured heights, so a
sidecar carries its own validity key and the adopter discards on mismatch.

### 6.5 Residency: keeping a lid on memory

| call | who | policy |
|---|---|---|
| `reap(ttl_ms)` | HTML server, lazily on each `/pull` | drop residents idle longer than `ttl_ms` — but never one still in use (#168) |
| `reap_over_budget(n, pinned)` | TUI, after a descend | keep at most `n` sub-agent residents, least-recently-touched go first; the pinned root never counts |
| `remove_pull(id)` | either, rarely | drop one resident immediately — used when it turns out poisoned |

An evicted resident is not a loss: its registry entry survives, so the next `admit`/`poll_view`
re-materializes it — and on a durable cache that is a **resume**, not a re-fold, which is what
makes an aggressive TTL affordable.

### 6.6 Letting go

```rust
cache.release(id);      // quiesce + unlock ONE session; it stays resident and readable
cache.release_all();    // everything — both `process::exit(0)` sites call this explicitly,
                        // because they skip destructors
// Drop does release_all() too: it covers every `?` on every error path.
```

`release` **quiesces** rather than merely flushing: a released session that kept its writer would
append to an entry this process no longer owns — two writers on one entry, which is the whole
thing the lock exists to prevent.

### 6.7 What there is deliberately NO API for

- **Deleting an entry.** Nothing exposes "forget this session's durable state". Entries go by age
  through `admit::gc` (`KEEP_FOR`, 14 days; a lock buys another 16), swept once when a durable
  cache is built. A client that wants an entry gone deletes the directory itself — which is one
  reason `entry_dir` stays public (§9).
- **Sharing an entry.** There is no read-only open, no shared mode. One writer, always; a second
  asker is `Denied` and routes to the holder (#163).
- **Asking whether an id is admitted.** `is_registered` answers *known*, not *owned*. The honest
  test is to call `admit` and read the outcome.

### 6.8 Which of these are durable-only (today: most of them)

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

### 6.9 Every method, and who calls it

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

## 7. Four reductions this is really made of

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

## 8. What #169 adds

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

## 9. What deliberately does NOT move behind the provider

`entry_dir`, `Presentation` and `lock::read` have consumers that never touch the cache and must
keep working:

- **`claude-monitor/src/index.rs`** reads each visited entry's `meta.jsonl` lock-free for the
  rail's counters (R7 — no fold on the index path).
- **`html_export::existing_server`** reads a lock's note to decide a hand-off *before* any session
  is admitted.

They stay public on the filesystem provider's own module. A provider is not a private box; it is
the layout, and other tools legitimately read that layout.

---

## 10. Migration

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

## 11. Open questions

1. **Does `cache_home()` / `default_root()` belong to the cache at all?** They live in
   `cache::admit` today and are already called by clients. Candidates: move to `present::sys`
   beside `throwaway_root`, or keep them as the `FsEntries` default.
2. **Is `Presentation` the provider's business or the client's?** After this change the cache never
   sees it. It could equally be folded into the root the client passes (`<root>/html`), leaving
   the enum to the two places that need to *name* the namespace (the monitor's rail read).
3. **Does `admit` keep its name?** Once it is "become the single owner of this session's
   tailer" rather than "claim a lock on a directory", the word is doing less work than it looks
   like it is (§6.8). `own` / `claim` / `take` are candidates. Renaming touches every call site
   and every test that matches on `Admission`, so it is worth deciding at step 3 of §10 or not at
   all.
4. **Does `A` (the sidecar slot) stay on the cache?** It is view state keyed by session, so
   probably yes — but it is the third map on a type we are trying to narrow.

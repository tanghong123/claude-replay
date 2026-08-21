# The session cache, redesigned: one cache, three providers

> **Status:** the agreed target (task #167). Nothing is built yet; this is the spec to build
> against. The exploration that led here — every dead end, bug post-mortem and rejected
> alternative — is preserved in [`cache-persistence-seam.md`](cache-persistence-seam.md); read
> that when you want to know *why not something else*. Read this when you want to know *what
> we are building*.
>
> Written to be readable without knowing Rust or this codebase. Rust syntax is annotated where
> it appears; a short glossary is at the end (§10).

---

## 1. Background: what this cache is for

`claude-replay` is a viewer for AI-agent session transcripts. A transcript is a JSONL file (one JSON
object per line) that an agent appends to while it works — often for hours, sometimes growing past 100 MB. To display
one, the viewer must **fold** it: parse every line and build the sequence of renderable blocks
(user turns, tool calls, results). Folding a large transcript takes real time — up to a minute
cold — so the result is worth keeping.

Three programs display sessions, and they share one cache implementation. In every one of them
the cache holds **multiple entries at once** — what differs is how many a user *sees*
concurrently, not how many are cached:

| program | on screen at once | in the cache | how it reads the cache |
|---|---|---|---|
| `claude-replay` (terminal UI) | one session (it takes over the screen) | several: the viewed session, its descended sub-agents, and recently-switched-away sessions kept warm | polls each tick for a delta to splice into the view |
| `claude-replay --html` | any number — one browser tab per session | every session the server hosts | serves any number of tabs from one folded copy per session, over a cursor protocol |
| `claude-monitor` | one at a time in the rail's frame | every session on the machine, populated as visited | same HTML serving path, one long-running process |

The cache answers two different needs, and keeping them distinct is the whole subject of this
document:

1. **Residency** (in-memory): "give every caller the *same* live, folded copy of session X, keep
   it fresh as the transcript grows, and drop it when nobody needs it." One folded copy per
   session, shared — because two independent folds of one transcript waste CPU and can disagree.
   *"Keep it fresh" is the subtle half*: nothing in the cache does it on a timer, and a resident
   can trail both the durable entry and the transcript until a client drives it forward — §1.1.
2. **Durability** (on-disk): "when the process restarts, don't re-fold from byte 0 — resume from
   where the last run left off." A durable *entry* on disk holds the folded output plus a small
   metadata stream that says how far the fold got; a *lock file* ensures only one process writes
   an entry at a time; a *note* inside the lock tells a second process where the first one is
   serving, so it can redirect instead of fighting.

### 1.1 Three states, and the client that drives them into agreement

Naming residency and durability as two *jobs* invites a reading where each is simply "the" state
of a session. They are not. There are **three** states of the same session, and at any instant
they may disagree — each lagging the one above it:

```
  transcript on disk       the agent appends to it, on its own schedule
      ↑ lags
  durable entry            folded as far as the last record the cache wrote
      ↑ lags
  resident (in memory)     folded as far as the last advance a client drove
```

**And the cache has no background sync thread.** `SharedSession::advance` says so in its own
first line — it *borrows the caller's thread* to tail the source. Nothing in the cache wakes up
on its own, nothing polls, nothing catches up in the background. The three states converge only
when a client asks for something, which makes the per-request sequence in §5.5
(`reap → touch → (admit if evicted) → advance → reply`) not an implementation detail but **the
convergence itself**, in the only order that is cheap:

1. **Resident ← durable, first.** `SharedSession::resume` rebuilds the accumulator from the
   record stream and starts its reader at `resume.replay_from`, so the bytes below that offset
   are never read again. The cost is proportional to what the previous run already folded, not
   to the size of the file.
2. **Then transcript → both, in one pass.** The `advance()` that follows reads only the appended
   delta, and `record()` drains the fold's new meta records into the writer as it goes. One read
   of the transcript moves the resident and the durable entry forward together.

The order is the whole efficiency argument, and it is worth stating because the reverse is the
intuitive one. Read the transcript first and reconcile with the durable entry afterwards, and
the resident has to fold from byte 0 to discover what the durable entry already knew — precisely
the minute-long cold fold that durability exists to avoid. Sync the cheap hop first, and the
expensive hop is a delta rather than a file.

Two consequences, stated because both are easy to mistake for bugs:

- **A resident is only ever *behind* the durable entry at the moment it is created.** Once
  `attach_writer` has armed it, the fold and the record stream advance in lockstep under one
  lock. There is no mid-life drift to reconcile — which is why the resident's interface has a
  `resume` and no `resync`.
- **A released resident stops following on purpose.** `quiesce` flushes, detaches the writer and
  freezes; `advance` then returns `Ok(false)`. The session stays resident and readable while
  owning nothing on disk (#109), so it lags the transcript deliberately until a re-admission
  re-arms it. "Stale" and "broken" look identical from outside and are not the same thing.

This is also why §6's right-hand column can say the resident must never know "that its store is
durable at all": it does not participate in the reconciliation — it *is* one of the things being
reconciled, and the client's call order is what does the work.

Terms used throughout:

| term | meaning |
|---|---|
| **session** | one transcript file, identified by a session id |
| **fold** | parsing the transcript into renderable blocks |
| **resident** | the live, in-memory folded copy of one session (`SharedSession`) |
| **store** | where folded blocks go — RAM, or an append-only file (`BlockStore` implementations) |
| **entry** | a session's on-disk durable state: the store's file + metadata + lock |
| **admit** | ask the cache for exclusive ownership of a session's entry, getting the resident back |
| **reap** | evict idle residents from memory (their entries survive on disk) |

---

## 2. The problem

Today one class, `SessionCache`, does both jobs. Residency is its natural business. Durability
leaked in as an optional field:

```rust
// BEFORE — claude-replay-present/src/cache/mod.rs (annotated)
pub struct SessionCache<P: BlockStore, A = ()> {
    //                  ^ P = the store type   ^ A = an opaque per-session "sidecar" slot
    registry:       Mutex<HashMap<String, Transcript>>,   // ids → where the transcript is
    pull_residents: Mutex<HashMap<String, (Instant, Arc<SharedSession<P>>)>>,
    //              ids → the live folded copy (Arc = shared reference; Instant = idle clock)
    aux:            Mutex<HashMap<String, A>>,            // ids → sidecar state
    admitting:      Mutex<()>,                            // "one admission at a time" gate
    durable:        Option<Durable>,                      // ← THE PROBLEM: maybe-persistence
}

struct Durable {                       // everything below is filesystem business
    presentation: Presentation,        // a frontend enum (Tui | Html) — used only to build paths
    root:         PathBuf,             // a directory
    versions:     Versions,            // a fold-version stamp, for rejecting stale entries
    owned:        Mutex<HashMap<String, Owned>>,   // which entry locks this process holds
}
```

Because durability is an `Option` (a maybe-present field), every behavior forks on whether it is
there. Four concrete costs:

**1. Two lifecycles in one type.** With the field, `admit` is the only way to create a resident.
Without it, `admit` always refuses (`NoCacheFlag`) and a *different* method (`shared_session`)
creates residents with no gate at all. The fork is visible in the code:

```rust
// BEFORE — poll_view picks a lifecycle by inspecting the cache's own field
let ss = if self.durable.is_some() {
    self.touch(id)?               // durable: only `admit` may create — a missing resident is a bug
} else {
    self.shared_session(id, || …) // otherwise: create on demand, no lock, no gate
};
```

A type that behaves as two different things depending on one `Option` field is two types.

**2. The filesystem vocabulary leaks upward.** The cache's constructor takes a directory and
performs disk I/O (a garbage-collection sweep) as a side effect of *building a cache object*. Its
public error type names `chmod` problems (`UnwritableRoot`) and CLI flags (`NoCacheFlag`).
`Presentation` — which of two frontends you are — appears in the cache's signature purely to
build a path.

**3. Lock lifetime is managed by convention, cross-referenced across two structures.** "We hold
the entry" lives in `Durable::owned` (a map keyed by session-id strings); "we are writing to it"
lives inside the resident. They are kept in step by calling `release(id)` at the right times.
Called in the wrong order — or not at all on an error path — the two disagree. One public method
(`remove_pull`) removes a resident *without* releasing its lock or stopping its writer, and is
safe today only because its single caller happens to call `release` first.

**4. It has produced real corruption.** Two bugs this month were both lifecycle races around
this structure: a resident evicted while a thread was still folding into it (#168), and N
concurrent first-requests each creating a store on the same file because no gate existed yet
(#169). Both were *thread-level* failures that the *process-level* lock file could not prevent —
the lock deliberately never denies its own process. The durable machinery didn't cause them, but
its entanglement with residency is why nobody could see that "who may open a store" was no one's
stated job.

And one design-level mismatch, visible on any machine running the monitor:

**5. Everyone pays for the strictest case.** `claude-monitor` locks its whole cache directory
with ONE root lock (one monitor per machine). But because it reaches entries through the same
`admit` path as everyone else, it *also* takes a per-entry lock on every entry (56 of 56 on the machine where this was
diagnosed) — locks
that can never be contended, since the root lock already guarantees no second monitor exists. The
only thing those locks can do is misfire (a stale lock naming a recycled process id once forced a
special "is this actually me?" guard). Meanwhile `--no-cache` — which should mean *no durable
cache* — currently builds a full durable cache at a throwaway path, locks and all, for entries no
other process can ever see.

---

## 3. The three use cases, named precisely

Everything the redesign does follows from taking these three seriously as *different* cases:

**(a) One-off, no durability — `claude-replay --no-cache`.**
The user wants a second, independent view of a session (perhaps someone else's viewer already
holds it). Nothing should survive the run and nothing should coordinate with other processes.
Note: an `--html --no-cache` run *does* write a file (the rendered records the browser
range-reads) — but that file is a **serving artifact in run scratch, wiped at the next start**,
not a cache anyone resumes from. Writing to disk and being a durable cache are not the same
thing.

**(b) Per-session durable — plain `claude-replay` and `claude-replay --html`.**
Entries live under a shared root (`~/.cache/claude-replay/sessions/`). Two viewers on *different*
sessions must both work, so exclusion is per `<session, frontend>`: one lock file per entry. A
second process asking for a *held* session is denied and redirected to the holder (whose lock
note names its port or tmux pane).

**(c) Whole-machine durable — `claude-monitor`.**
One long-running process serves every session on the machine from its own root
(`~/.cache/claude-monitor/`). The useful exclusion statement is "one monitor per root": ONE lock,
taken at startup. A second monitor should be redirected to the first (via the root lock's note)
and exit. Per-entry locks add nothing here.

Two axes fall out — and they are independent:

|  | not shared with other processes | shared |
|---|---|---|
| **not persistent** | (a) `--no-cache` | — (nothing to contend for) |
| **persistent** | *(collapses into (a): scratch artifacts aren't a cache)* | (b) per-session · (c) per-root |

**Process-level locking is only needed on the shared side.** Thread-level safety — one resident
per id, one admission at a time, one thread inside a resident at a time — is needed *always*,
in all three cases, and is exactly the part that is identical across them.

---

## 4. The design

### 4.1 Shape: one cache class, one small interface, three implementations

The cache keeps everything that is the same across the three cases (all of residency, all
thread-level locking). Everything that differs — lock or no lock, metadata or none, resume or
cold — moves behind one interface the client chooses an implementation of. In Rust an interface
is a **trait**; an implementation of it here is called a **provider**.

```
        client (TUI / HTML server / monitor)
          │  picks a provider + root at startup; owns gc timing
          ▼
        SessionCache  ─────────────  residency, TTL eviction, sidecars,
          │                          one-resident-per-id, one-admission-at-a-time
          │ asks "open entry X for me"
          ▼
        Entries (trait)  ──────────  the ONLY seam
          ├─ Transient               (a): no lock, no metadata, no resume
          ├─ PerSession              (b): one lock file per entry, resume, redirect notes
          └─ SingleWriter            (c): one root lock at construction, resume, NO entry locks
          │
          ▼
        the store (BlockStore)  ───  how folded blocks become bytes (RAM or file) — unchanged
```

Why **one** cache class and not two or three (one per use case)? Because the read path —
getting the resident, polling it, serving pulls, reaping, sidecars — is the bulk of the API and
is *byte-identical* across the cases. Rust has no inheritance; separate classes would each
hand-copy or hand-forward ~20 methods for zero behavioral difference. Worse, the "one admission
at a time" gate must live in the same type as the residents map it protects — a durability
wrapper *around* the cache could serialize itself and still race on inserting the resident.
Code the providers do share (lock files, the metadata stream, path layout, gc) lives together in
one module, `cache::fs` — sharing happens *below* the seam, not via a class hierarchy.

### 4.2 The interface, before and after

**Before** there is no interface — the provider's whole job is inlined into the cache. The one
method that does everything:

```rust
// BEFORE — the cache's admission entry point
pub fn admit(
    &self,
    id: &str,
    make_store: impl FnOnce(&Path) -> io::Result<P>,   // caller builds the store, given a directory
    alive: impl Fn(&Holder<P::Note>) -> bool,          // caller re-supplies the liveness rule EVERY call
) -> Admission<P>

pub enum Admission<P> {
    Owned { session: Arc<SharedSession<P>>, origin: Origin },  // it's yours (origin: resumed/cold/…)
    Denied(Denial<P::Note>),                                   // someone else has it, or no cache
}
```

Inside that one function: registry lookup, the thread gate, building the entry's path, taking the
lock file, opening the store, reading the metadata stream, aligning/resuming, installing the
resident, attaching the metadata writer. Six concerns, one body — which is where the missing
gate hid for months.

**After** — the cache keeps the gate, the resident singleton, and installation; the provider gets
one method for the entry work:

```rust
// AFTER — the seam (proposed)
pub trait Entries<P: BlockStore> {
    /// What a lock-holder publishes about itself so a second process can redirect:
    /// the HTML server's port, the TUI's tmux pane. Typed per frontend.
    type Note;

    /// Take exclusive ownership of session `id`'s entry — or say who has it.
    /// `ours`: the byte length of the ENTRY'S BLOCK BACKING as our still-in-memory resident
    ///          last wrote it (`BlockStore::backing_len` — the #109 witness; `None` when no
    ///          resident survives). An ON-DISK-ARTIFACT length, never a transcript offset —
    ///          the transcript's identity/position is owned by the versions+anchor checks and
    ///          the resume machinery. It buys three outcomes with one stat: equal ⇒ nothing
    ///          touched the entry since we let go — a "retained" open, zero loading; ours <
    ///          on-disk ⇒ our blocks are a valid prefix, load only from `ours`; ours >
    ///          on-disk ⇒ a peer cut below us, the prefix is no longer ours to trust.
    /// `make_store`: the caller still builds the store (only it knows this session's
    ///          working directory and fold options); the provider supplies the *place*.
    fn open(&self, id: &str, src: &Transcript, ours: Option<u64>,
            make_store: &dyn Fn(&Path) -> io::Result<P>) -> Opened<P, Self::Note>;

    /// Publish a late-arriving fact into the lock note (only the HTML port needs this).
    fn publish(&self, id: &str, note: Self::Note) -> bool { false }   // default: nothing to publish
}

pub enum Opened<P, N> {
    Owned {
        store:  P,                    // opened, positioned
        loaded: Vec<P::Bv>,           // blocks recovered from disk (empty on a cold start)
        origin: Origin,               // Resumed / Retained / Cold(reason) — kept diagnosable
        writer: Option<EntryWriter>,  // owns BOTH the metadata stream and the lock — see §4.4
    },
    Denied(Denial<N>),                // a live peer holds it (with its note), redirect to them
}
```

**…and the cache around it, after** — the part the user of the cache actually holds:

```rust
// AFTER — the cache (proposed)
pub struct SessionCache<P: BlockStore, A = (), E: Entries<P>> {
    registry:   Mutex<HashMap<String, Transcript>>,      // unchanged
    residents:  Mutex<HashMap<String, (Instant, Arc<SharedSession<P>>)>>,  // unchanged
    aux:        Mutex<HashMap<String, A>>,               // unchanged
    admitting:  Mutex<()>,   // unchanged — the thread gate stays HERE. Not redundant with the
                             // per-map mutexes: those protect each map's bytes, this makes the
                             // admission SEQUENCE atomic (peek-miss → slow open → install).
                             // The entry LOCK cannot arbitrate threads (pid-based: our own pid
                             // reads as ours), and holding `residents` across the open would
                             // starve every unrelated reader — see #169, where N concurrent
                             // first-pulls each folded and wrote the same backing.
    entries:    E,                                       // ALWAYS present. No Option, no forked behavior.
}

impl SessionCache {
    pub fn new(entries: E) -> Self;                 // the ONLY constructor; no I/O side effects
    pub fn admit(&self, id: &str,
        make_store: impl Fn(&Path) -> io::Result<P>) -> Admission<P>;  // the ONLY creator of residents
    pub fn publish(&self, id: &str, note: E::Note) -> bool;  // forwards to the provider (HTML port only)
    pub fn quiesce(&self, id: &str);                // stop writing; the entry's PROCESS-level
                                                    // LOCK file releases automatically — the
                                                    // writer's drop IS the unlock (§4.4; thread
                                                    // locks are call-scoped and never held here)
    // Everything else is unchanged: touch, shared_peek, poll_view, reap, reap_over_budget,
    // register/register_new/is_registered/resolve, aux_put/aux_take/aux_with.
}

/// Owned by the resident while it writes. Dropping it releases the entry lock —
/// that is the entire release mechanism (§4.4).
pub struct EntryWriter { meta: MetaWriter, lease: Lease }
```

`Admission` keeps its two-outcome shape (`Owned`/`Denied`) so every existing `match` at every
call site keeps compiling — that is what makes the migration steps in §8 small.

Two deliberate details:

- `ours` is a **number**, not a reference to the resident. The tempting version — hand the
  provider the resident and let it decide — would make the provider depend on the cache's central
  type, and nothing would actually be decoupled. The cache computes its own half of the
  "has anything changed?" check and passes one integer across the seam.
- `Origin` survives unchanged. "The cache didn't help, and here is which of five reasons" is a
  support answer; the rejection tests assert on it.

### 4.3 The three providers

```rust
// (a) --no-cache: no lock, no metadata, no resume. `open` ALWAYS succeeds, cold.
//     The store may still write into run scratch (serving artifacts) — wiped next start.
let cache = SessionCache::new(Transient::in_dir(run_dir));

// (b) the viewer / HTML server: entry lock per <session, frontend>, resume, redirect notes.
//     No construction-time claim — several processes on one root are LEGITIMATE here (each
//     serving different sessions), so the redirect surfaces per ADMISSION instead:
//     `Admission::Denied(Denial::Held(h))` at the admit call site, where the frontend sends
//     the client to `h.note`'s port (serve.rs's existing match, kept compiling by design).
let entries = PerSession::new(root, Namespace::Html)     // Presentation retreats into cache::fs
    .versions(Versions::current(Some(render_flavor(&fold))))  // fold stamp: stale entries rebuild
    .liveness(alive_rule);                                // pid + port probe — configured ONCE
entries.gc();                                             // explicit, where the root is known
let cache = SessionCache::new(entries);

// (c) the monitor: ONE root lock, taken here. A second monitor gets the redirect
//     as a CONSTRUCTOR outcome and never builds a cache at all.
let entries = match SingleWriter::claim(&root)? {
    Claimed::Ours(e)        => e,
    Claimed::Served { url } => { open_in_browser(&url); return Ok(()); }
};
let cache = SessionCache::new(entries);
```

What each one's `open` does:

| | `Transient` | `PerSession` | `SingleWriter` |
|---|---|---|---|
| lock taken in `open` | none | this entry's `LOCK` file | none — the root lock (constructor) already covers it |
| can return `Denied` | never | yes: a live peer holds the entry | never |
| metadata / resume | none — always cold; the cache resets the store first | yes | yes |
| note (for redirects) | none | in the entry's lock | in the root lock |

Rule of thumb that falls out: **the note lives wherever the lock lives — and the redirect is
handled wherever `Denied` can surface**: at construction for (c) (a second monitor never builds
a cache), per admission for (b) (the request redirects to the entry's holder), nowhere for (a).

The `Transient` row is what fixes `--no-cache` for good: no metadata stream means a
re-materialized session always starts from a `store.reset()` (truncate) and refolds — so the
historical unbounded-growth bug (append-forever without reset) becomes structurally impossible,
and the monitor's 56 uncontendable entry locks — plus the "is this stale lock actually me?"
guard they necessitated — are simply deleted by the `SingleWriter` row.

### 4.4 Locking, the complete story

Two levels, three moments, and after the redesign each has exactly one owner:

| moment | mechanism | level | owner |
|---|---|---|---|
| provider construction | `SingleWriter::claim`: root `LOCK` or redirect | process (whole cache) | provider |
| admission | the `admitting` gate — a critical section, held only during `admit` | thread | cache |
| admission | `PerSession`: the entry's `LOCK` file | process (one session) | provider |
| resident lifetime | shared-reference count; reaping spares a resident still in use | thread | cache |
| serving | the resident's internal mutex; a fold holds it start to finish | thread | resident |
| release | **automatic**: dropping the `EntryWriter` releases the lock | process | writer |

The last row replaces convention with ownership (in Rust terms, RAII: releasing-on-drop). The
`EntryWriter` bundles the metadata stream and the lock lease into one object stored inside the
resident. "We are writing" and "we hold the entry" become the *same fact*, held in *one place*:

- `quiesce` (stop writing, stay readable) drops the writer → the lock releases at exactly the
  moment writing stops, not near it.
- Evicting a resident drops its last shared reference → writer drops → lock releases. The old
  footgun method that evicted without unlocking cannot exist.
- Every error path is covered for free; there is no "forgot to call release".
- The map of held locks (`Durable::owned`) disappears — it only ever existed to find a lock
  again from a string.

### 4.5 What gets deleted, and why

| deleted | why |
|---|---|
| `shared_session(id, factory)` — get-or-create bypassing admission | it *is* the two-writers hazard as an API, and it has exactly one production caller. `admit` becomes the only way a resident comes into being — `Transient` makes that cheap for the no-cache case — and `poll_view`'s two-lifecycle branch (§2, cost 1) disappears |
| `admit`'s `alive` parameter | a per-call closure rebuilding what is provider configuration; `PerSession` takes the liveness rule once |
| `ephemeral()` / `durable(…)` / `Default` constructors | one constructor: `SessionCache::new(provider)`; the gc side effect becomes an explicit client call |
| `Denial::NoCacheFlag` | no flag selects a cache that cannot serve anymore |
| `NoLivenessCheck`, root-level `UnwritableRoot` | machine problems surface when the client *builds the provider*, once — not per session at admit time |
| `release(id)` as an unlock | replaced by writer ownership (§4.4); the name survives meaning only "quiesce" |
| the TUI's `publish` call | its note (the tmux pane) is known at startup, so `PerSession` accepts it at construction and writes it on acquire; `publish` remains only for the HTML port, the one fact that genuinely arrives late |
| `DurableStore::Note` (on the store trait) | a note describes the *process holding the lock*, not the store; it moves to `Entries::Note`, and the store trait sheds its serialization bounds — back to purely "how blocks become bytes" |
| `Presentation`/`Versions` in the cache's signature | provider configuration, in `cache::fs` |

Kept, deliberately: the registry (`register`/`register_new`/`is_registered`/`resolve`); the two
honest getters (`touch` = get-and-bump-idle-clock, `shared_peek` = get-without); both eviction
policies (`reap(ttl)` for the server, `reap_over_budget` for the TUI); the sidecar slot
(`aux_put`/`aux_take`/`aux_with` — the TUI parks view state there); `make_store` as a per-call
closure (the working directory and fold options are per-session facts only the caller has); and
the name `admit` (it now means "become the sole owner of this session's tailer" — renaming every
call site buys nothing a doc comment can't).

---

## 5. Call sites, before and after

### 5.1 Building the cache — the terminal UI

```rust
// BEFORE — claude-replay-tui/src/app.rs
fn make_cache(args: &Args) -> TuiCache {
    crate::sys::reclaim();
    let root = match args.no_cache {
        true  => crate::sys::throwaway_root(),   // a full DURABLE cache at a private path…
        false => cache::admit::default_root().unwrap_or_else(crate::sys::throwaway_root),
    };
    TuiCache::durable(Presentation::Tui, root, Versions::current(None))
    //       ^ constructor does disk I/O (gc);  ^ frontend enum in a cache signature
}
```

```rust
// AFTER
fn make_cache(args: &Args) -> TuiCache {
    crate::sys::reclaim();
    if args.no_cache {
        return TuiCache::new(Transient::in_dir(crate::sys::run_dir()));  // truly no cache
    }
    let entries = PerSession::new(default_root(), Namespace::Tui)
        .versions(Versions::current(None))
        .note(TuiNote::here());          // the pane is known NOW — no publish call later
    entries.gc();                        // explicit, visible, client-owned
    TuiCache::new(entries)
}
```

### 5.2 Building the cache — the HTML server

```rust
// BEFORE — claude-replay-html/src/html_export/serve.rs
let cache = match cfg.cache_root {
    Some(root) => SessionCache::durable(
        cfg.presentation, root,
        Versions::current(Some(render_flavor(&cfg.fold))),
    ),
    None => SessionCache::ephemeral(),    // every admit will refuse; a fallback path once
                                          // hid behind this and silently double-folded
};
```

```rust
// AFTER — the same match, but both arms are honest
let cache = match cfg.cache_root {
    Some(root) => {
        let entries = PerSession::new(root, Namespace::Html)
            .versions(Versions::current(Some(render_flavor(&cfg.fold))))
            .liveness(html_alive(port.clone()));   // pid + port probe + self-guard, ONCE
        entries.gc();
        SessionCache::new(entries)
    }
    None => SessionCache::new(Transient::in_dir(cfg.scratch.clone())),
    //      --no-cache: serving artifacts land in run scratch, wiped next start.
    //      admit works; nothing is durable; no locks exist.
};
```

### 5.3 Admitting a session — the HTML server, per request

```rust
// BEFORE — the liveness rule is rebuilt at EVERY call site that admits
let ours = self.port.get().copied();
let alive = |h: &Holder<HtmlNote>| {
    let port = h.note.as_ref().map(|n| n.port);
    port != ours && lock::pid_alive(h.pid) && port_open(port)
};
match self.cache.admit(id, |dir| open(&dir.join("records.jsonl")), alive) {
    Admission::Owned { session, .. } => { /* publish port, serve */ }
    Admission::Denied(Denial::Held(h)) => { /* redirect to h.note */ }
    Admission::Denied(Denial::Unavailable(why)) => { /* explain */ }
}
```

```rust
// AFTER — the call says WHAT it wants, never how the entry is guarded
match self.cache.admit(id, |dir| open(&dir.join("records.jsonl"))) {
    Admission::Owned { session, .. } => { /* publish port, serve */ }
    Admission::Denied(Denial::Held(h)) => { /* redirect to h.note */ }
    // Unavailable is gone from this match: machine problems surfaced at construction.
}
```

The same line works against all three providers. Under `Transient` the `Denied` arm is simply
unreachable — no peer can exist.

### 5.4 The monitor's startup

```rust
// BEFORE — claude-monitor/src/main.rs: a hand-rolled root-lock dance,
// then a cache that ALSO takes 56 per-entry locks it can never need
claim_root(&root)?;                       // acquire-or-redirect, ~60 lines incl. port probing
let service = SessionService::new(ServiceConfig {
    cache_root: Some(root.clone()),       // → SessionCache::durable → per-entry LOCKs
    …
})?;
```

```rust
// AFTER — the root lock IS the provider's constructor; entries take no locks at all
let entries = match SingleWriter::claim(&root)? {
    Claimed::Ours(e)        => e,
    Claimed::Served { url } => { open_url(&url); println!("{url}"); return Ok(()); }
};
let service = SessionService::with_entries(entries, …)?;
```

### 5.5 The steady state — unchanged on purpose

Serving and ticking do not change at all. Per HTML request:
`reap → touch → (admit if evicted) → advance → reply`. Per TUI tick: `poll_view → splice delta`.
The redesign's success criterion for requirement 4 was exactly this: only the constructors know
which of the three worlds you are in.

---

## 6. What each layer knows — the one-table summary

| layer | owns | must never know |
|---|---|---|
| client (viewer / monitor) | which provider, which root, fold options, gc timing | lock files, metadata formats |
| `SessionCache` | ids → sources, ids → residents, thread-level exclusion, eviction, sidecars | where (or whether) an entry lives on disk |
| `Entries` provider | entry paths, lock files, notes, metadata, resume/align, gc | what a `SharedSession` is |
| store (`BlockStore`) | how blocks become bytes and come back | who else might want the file |
| resident (`SharedSession`) | the fold, its counters, one-thread-at-a-time | that its store is durable at all |

The right-hand column is the design. Today's code violates it in one place — the cache holds the
provider's entire job in an optional field — and every §2 cost traces back to that.

In crate terms (the workspace is layered engine → present → frontends): the store trait stays in
**`claude-replay-engine`**; the cache, the `Entries` trait, and `Transient` live in
**`claude-replay-present`** (`cache/`); the two filesystem providers and everything they share —
lock files, the metadata stream, path layout, `gc` — live together in **`cache::fs`** in that
same crate; the clients are **`claude-replay-tui`**, **`claude-replay-html`**, and
**`claude-monitor`**. Nothing in `present` gains a dependency; the frontends lose their
knowledge of lock liveness rules.

---

## 7. Correctness invariants the redesign must preserve

1. **One resident per id, ever** — creation only through `admit`, under the gate; the loser of a
   race receives the winner's resident (#169).
2. **A resident in use is never evicted** — reaping checks the reference count (#168).
3. **One writer per entry, ever** — process level by lock (per-entry or root), thread level by
   "admit is the only creator" + the resident's mutex.
4. **Resume equals cold** — a resumed record log must be byte-identical to one folded from
   scratch (the standing oracle test).
5. **`--no-cache` growth is bounded** — every re-materialization resets the store before folding
   (a new oracle; the old design's growth bug becomes untestable-because-impossible, and the test
   proves it stays that way).
6. **A denial opens nothing** — the lock is taken before the store is opened, so "someone else
   has it" leaves no handle behind.

## 8. Migration (each step lands green on all oracles)

1. Move `Note` off the store trait onto a standalone per-frontend type. No behavior change.
2. Extract `cache::fs` with `PerSession` + `SingleWriter` implementing `Entries`; the old
   constructors keep their signatures and build providers internally. The `EntryWriter` lease
   lands here (touch the admission body once, not twice). Pure re-plumbing; all tests unchanged.
3. Flip the constructors: clients build providers; `gc` moves to the client; the monitor adopts
   `SingleWriter` (deleting its per-entry locks and hand-rolled `claim_root`); the TUI's note
   moves to construction.
4. Add `Transient`; rewire `--no-cache`; delete `shared_session`, `ephemeral`, `NoCacheFlag`,
   and the `poll_view` branch; retire the throwaway durable root.

## 9. Out of scope

- The pull protocol, the fold, the record/store formats — untouched.
- The sidecar validity discipline (who decides parked view state is stale) — a real, documented
  gap, but orthogonal; see `cache-persistence-seam.md` §6.5.
- Public path-layout readers (`entry_dir`, the monitor's lock-free metadata reads,
  `existing_server`) — they stay public on `cache::fs`; the layout is an interface of its own.

## 10. Minimal Rust glossary

| term | meaning here |
|---|---|
| **trait** | an interface: a named set of methods a type promises to provide |
| **provider** | our word for a type implementing the `Entries` trait |
| **generic parameter** (`SessionCache<P, A, E>`) | a type slot filled in at compile time; each frontend defines one alias so the letters never appear at call sites |
| **`Arc<T>`** | a shared reference with a count; the value lives until the last holder drops it |
| **`Mutex<T>`** | a lock around a value; only one thread can touch the inside at a time |
| **`Option<T>`** | maybe-a-value; the "maybe" on the old `durable` field is what forked every behavior |
| **enum** (`Opened`, `Admission`) | a value that is exactly one of several named shapes; the compiler forces callers to handle each |
| **closure** | an inline function value, e.g. `make_store` — the caller's recipe the provider invokes with a path |
| **RAII / `Drop`** | cleanup runs automatically when a value goes out of scope; how the lock release becomes impossible to forget |

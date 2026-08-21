//! **The unified data layer** — one keyed cache owning every followed session's single
//! full in-memory presentation copy (#84/#85), shared by both frontends.
//!
//! [`SessionCache<P, A>`] holds, per session id:
//! - **registered** — a keyed [`Transcript`] source handle: we know where
//!   the session lives, but hold nothing else. Costs nothing; the common case for a large
//!   sub-agent tree whose children were discovered but never opened.
//! - **resident** — a [`SharedSession<P>`] (see [`shared_session`](SessionCache::shared_session)):
//!   an open incremental follower plus the committed store `P` — the ONE live tier every
//!   consumer shares. The TUI ticks it in-process via [`poll_view`](SessionCache::poll_view)
//!   (a [`ViewDelta`] splice against `P = ArcStore`, blocks shared by `Arc` — the cache keeps
//!   the authoritative copy, views hold clones of the pointers); the HTML server serves any
//!   number of stateless clients from the same resident via the cursor [`pull`] protocol
//!   (`P = RecordStore`, committed blocks living as wire-format pointers on disk).
//! - **durable** (#96) — a cache built with [`durable`](SessionCache::durable) keeps each owned
//!   session's committed blocks and meta records on disk under `root/<presentation>/<session>/`,
//!   so a LATER PROCESS resumes the fold instead of re-reading the transcript from byte 0. The
//!   frontend's whole view of it is [`admit`](SessionCache::admit) and its two outcomes.
//!
//! The `A` parameter is an opaque per-session **presentation sidecar** slot
//! ([`aux_put`](SessionCache::aux_put)/[`aux_take`](SessionCache::aux_take)): view-parameter-
//! dependent state (the TUI's measured heights, the server's titles/parents) lives with the
//! session it belongs to, with registry lifetime and consumer-owned validity.
//!
//! The maps are guarded by independent mutexes and never locked simultaneously, so the cache
//! can't self-deadlock. Rendering happens in the caller *between* cache calls; the only work
//! under a cache lock is the brief O(delta) follower advance.
// SharedSession: the one live tier — the follower + store both frontends share.
mod shared;
use crate::engine::meta_stream::Versions;
#[allow(unused_imports)]
pub use crate::engine::tier_b::{Deferred, TierBSession, TierBStore};
use crate::engine::BlockStore;
#[allow(unused_imports)]
pub mod admit;
pub mod fs; // #167: the entry providers — everything filesystem-shaped, behind one seam
pub mod lock;
pub mod stream;
pub use admit::{ColdReason, Denial, Origin, Presentation, Unavailable};
pub use fs::{Entries, EntryWriter, Opened, PerSession, SingleWriter, Witness};
pub use lock::Holder;
pub use shared::{DurableStore, PullDelta, SharedSession, ViewDelta};
pub use stream::{MetaReader, MetaWriter};
// The pull protocol moved to [`crate::pull`] (#87); these aliases keep the old paths.
pub use crate::pull::{pull, pull_indices, Applied, Cursor, PullClient, PullReply};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Instant;

/// Lock `m`, RECOVERING from poisoning instead of propagating the panic. A panic on one request
/// thread (e.g. a fold hitting a malformed transcript) used to poison the mutex and turn every
/// later request into a `PoisonError` panic — one bad line permanently bricking the session
/// (#56's cascade). The state these mutexes guard is either per-entry (a torn entry self-heals
/// through the pull protocol's epoch/resync) or rebuilt by the owner via
/// [`SharedSession::poisoned`], so recovering the guard is strictly better than the brick.
pub fn lock_recover<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

use crate::Transcript;

/// A keyed cache of sessions (see the module docs for the residency lifecycle). Owns the
/// session domain — every followed session's single full presentation copy, held by its
/// [`SharedSession`] — so consumers keep only presentation state.
pub struct SessionCache<P: BlockStore = TierBStore, A = ()> {
    /// Registered: every known session → its [`Transcript`] source handle.
    registry: Mutex<HashMap<String, Transcript>>,
    /// Resident: one [`SharedSession`] per id a consumer is following (`Arc` so any number
    /// of request threads share it). A resident kind of its own
    /// because it serves a different protocol (cursor pulls, borrow-to-tail) than the `poll`
    /// followers, but under the same owner and the same [`reap`](Self::reap) policy.
    pull_residents: Mutex<HashMap<String, PullResident<P>>>,
    /// The per-session **presentation sidecar** slot (#75): derived, view-parameter-DEPENDENT
    /// state a frontend associates with a session (e.g. the TUI's measured block heights +
    /// fold/scroll state) — the cache-level home for what `BlockStore::put`'s put-once
    /// contract forbids in `Bv`. Opaque to the cache; **the consumer owns validity** (the
    /// cache can't know about resizes — a sidecar carries its own validity key and the
    /// adopter discards on mismatch). Registry-lifetime: reaping a resident does NOT drop
    /// its sidecar.
    aux: Mutex<HashMap<String, A>>,
    /// The entry provider (#96/#167), absent on an [`ephemeral`](Self::ephemeral) cache. No
    /// flag selects `None` any more — `--no-cache` builds a real cache at its own root (#165)
    /// — so in production this is always `Some`, and a cache without it can only deny.
    /// (Becomes the always-present generic `E: Entries` at §8 step 2b.)
    entries: Option<fs::PerSession>,
    /// **One admission at a time** (#169). [`admit`](Self::admit) is not atomic: between a
    /// caller finding no resident and this cache installing one, every other caller finds no
    /// resident too. They all open a store on the same backing, and `lock::acquire` grants every
    /// one of them because none is a different process — so a session admitted by N concurrent
    /// requests is folded and WRITTEN by N of them at once. A gate rather than per-id state
    /// because an admission is short (open the backing, walk it) where a fold is long, and the
    /// long part happens outside it.
    admitting: Mutex<()>,
}

/// A pull-servable resident: its idle clock + the shared session. Tier-b-backed — the committed
/// block content of a followed session lives in the store's on-disk backing, not RAM.
type PullResident<P> = (Instant, std::sync::Arc<SharedSession<P>>);

impl<P: BlockStore, A> Default for SessionCache<P, A> {
    fn default() -> Self {
        Self {
            registry: Mutex::new(HashMap::new()),
            pull_residents: Mutex::new(HashMap::new()),
            aux: Mutex::new(HashMap::new()),
            entries: None,
            admitting: Mutex::new(()),
        }
    }
}

impl<P: BlockStore, A> SessionCache<P, A> {
    /// A cache that persists nothing. Every [`admit`](Self::admit) on one of these denies with
    /// `Unavailable(NoCacheFlag)`, and since #163 a denial has nothing behind it — so this is a
    /// cache that cannot hand out a session at all. `--no-cache` does NOT select it (#165): that
    /// flag builds a real cache at a root of its own.
    pub fn ephemeral() -> Self {
        Self::default()
    }

    pub fn new() -> Self {
        Self::default()
    }

    /// Store `id`'s presentation sidecar (see the field docs — consumer-owned validity).
    pub fn aux_put(&self, id: &str, a: A) {
        lock_recover(&self.aux).insert(id.to_string(), a);
    }

    /// Take `id`'s presentation sidecar out (move semantics: the adopter re-installs on its
    /// next eviction, so a sidecar is never stale-shared).
    pub fn aux_take(&self, id: &str) -> Option<A> {
        lock_recover(&self.aux).remove(id)
    }

    /// Read/mutate `id`'s sidecar in place (created default on first touch) — the shape for
    /// always-on per-session presentation state (the live server's titles/parents/diff
    /// baselines), as opposed to the park-and-take shape of eviction sidecars.
    pub fn aux_with<R>(&self, id: &str, f: impl FnOnce(&mut A) -> R) -> R
    where
        A: Default,
    {
        f(lock_recover(&self.aux).entry(id.to_string()).or_default())
    }

    /// Register (or overwrite) a session's tier-(c) source.
    pub fn register(&self, id: &str, src: Transcript) {
        lock_recover(&self.registry).insert(id.to_string(), src);
    }

    /// Register a session only if not already known — preserves the first (richest,
    /// ancestry-bearing) descriptor against a later bare fallback.
    pub fn register_new(&self, id: &str, src: Transcript) {
        lock_recover(&self.registry)
            .entry(id.to_string())
            .or_insert(src);
    }

    /// Whether `id` has a tier-(c) source.
    pub fn is_registered(&self, id: &str) -> bool {
        lock_recover(&self.registry).contains_key(id)
    }

    /// The tier-(c) source for `id`, if known.
    pub fn resolve(&self, id: &str) -> Option<Transcript> {
        lock_recover(&self.registry).get(id).cloned()
    }

    /// Evict follower residents beyond `budget` — least-recently-touched first — never evicting
    /// `pinned` (the session the viewer is anchored to, which doesn't count against the budget).
    /// The navigation-recency residency policy the TUI rides: evicted residents
    /// re-materialize from the registry on their next poll (a fresh whole-file fold).
    pub fn reap_over_budget(&self, budget: usize, pinned: &str) {
        let mut residents = lock_recover(&self.pull_residents);
        let mut others: Vec<(String, Instant)> = residents
            .iter()
            .filter(|(id, _)| id.as_str() != pinned)
            .map(|(id, (last_seen, _))| (id.clone(), *last_seen))
            .collect();
        if others.len() <= budget {
            return;
        }
        others.sort_by_key(|(_, last_seen)| *last_seen); // oldest first
        for (id, _) in &others[..others.len() - budget] {
            residents.remove(id);
        }
    }

    /// The pull-servable resident for `id`, materializing it via `open` on first use and bumping
    /// its idle clock on every call — the `/pull` handler's one entry point to the session domain.
    /// The returned `Arc` stays valid across a concurrent [`reap`](Self::reap) (the reap drops the
    /// cache's reference; in-flight requests finish on theirs).
    pub fn shared_session(
        &self,
        id: &str,
        open: impl FnOnce() -> SharedSession<P>,
    ) -> std::sync::Arc<SharedSession<P>> {
        let mut m = lock_recover(&self.pull_residents);
        let entry = m
            .entry(id.to_string())
            .or_insert_with(|| (Instant::now(), std::sync::Arc::new(open())));
        entry.0 = Instant::now();
        entry.1.clone()
    }

    /// Peek at an already-resident pull session **without** materializing or touching its idle
    /// clock — for a read that shouldn't keep the session alive (e.g. a child deriving its title
    /// from its parent's maintained meta iff the parent happens to be resident).
    pub fn shared_peek(&self, id: &str) -> Option<std::sync::Arc<SharedSession<P>>> {
        self.pull_residents
            .lock()
            .unwrap()
            .get(id)
            .map(|(_, ss)| ss.clone())
    }

    /// Evict every resident idle for longer than `ttl_ms` back down to tier (c). Their registry
    /// sources remain, so a later `shared_session` re-materializes them — and on a durable cache
    /// that re-materialization is a RESUME, not a re-fold, which is what makes an aggressive TTL
    /// affordable. Returns the evicted residents so the owner can act on the reference before it
    /// drops.
    ///
    /// **In use is not idle** (#168). The idle clock is stamped when a request TAKES the session,
    /// not when it finishes with it, so a fold longer than the TTL — a cold 132 MB transcript
    /// comfortably exceeds 30 s — leaves its own resident evictable while it is still writing to
    /// it. Evicting it there is not a lost cache entry, it is a lost *lock*: the next admission
    /// finds no resident, and `lock::acquire` never denies its own pid, so it opens a SECOND store
    /// on the same log. Two `SharedSession`s are two mutexes, so nothing serializes them, and
    /// `put`'s record-then-newline pair interleaves into `recA recB \n \n` — a line the browser's
    /// `JSON.parse` cannot read and the server never re-parses, i.e. a page that stays blank
    /// forever with nothing logged anywhere.
    ///
    /// `strong_count` is exact here, not a heuristic: every clone of a resident (`touch`,
    /// `shared_peek`, `shared_session`) is handed out under this same map lock, so no new
    /// reference can appear while the count is being read.
    pub fn reap(&self, ttl_ms: u128) -> Vec<(String, std::sync::Arc<SharedSession<P>>)> {
        let mut evicted = Vec::new();
        self.pull_residents
            .lock()
            .unwrap()
            .retain(|id, (last_seen, ss)| {
                let in_use = std::sync::Arc::strong_count(ss) > 1;
                let keep = in_use || last_seen.elapsed().as_millis() < ttl_ms;
                if !keep {
                    evicted.push((id.clone(), ss.clone()));
                }
                keep
            });
        evicted
    }

    /// A resident session's task op-log state (#15) without materializing or touching its
    /// idle clock — `None` when `id` has no live resident.
    pub fn resident_tasks(&self, id: &str) -> Option<crate::engine::TaskList> {
        self.shared_peek(id).map(|ss| ss.tasks())
    }

    /// The resident for `id`, bumping its idle clock — a read that keeps the session alive
    /// without being able to materialize one.
    pub fn touch(&self, id: &str) -> Option<std::sync::Arc<SharedSession<P>>> {
        let mut m = lock_recover(&self.pull_residents);
        let e = m.get_mut(id)?;
        e.0 = Instant::now();
        Some(e.1.clone())
    }

    /// Drop one pull resident immediately (regardless of idle time) — used when a resident turns
    /// out to be poisoned ([`SharedSession::poisoned`]) and must be replaced by a fresh session.
    pub fn remove_pull(&self, id: &str) {
        lock_recover(&self.pull_residents).remove(id);
    }
}

/// **The durable frontend API** (#96 §8). One call in, an exhaustive outcome out.
impl<P: DurableStore, A> SessionCache<P, A> {
    /// A cache whose entries live under `root/<presentation>/<session>/`.
    ///
    /// `versions` is what the fold is; a stream written by a different one is rejected rather
    /// than spliced.
    pub fn durable(presentation: Presentation, root: PathBuf, versions: Versions) -> Self {
        admit::gc(&root); // one directory walk per run keeps the cache from growing forever
        let mut c = Self::default();
        c.entries = Some(fs::PerSession::new(root, presentation, versions));
        c
    }

    /// Take exclusive ownership of `id`, or say why not. Never blocks on another holder.
    ///
    /// `make_store` is the ONE per-frontend piece, and it takes the entry's own directory: only
    /// HTML knows its fold policy and this session's cwd, only the TUI knows it wants
    /// `Arc<Block>`. It must open the backing **without truncating** — this needs to read what
    /// is there before deciding whether to keep it, and resets the store itself when the answer
    /// is no.
    ///
    /// It is a per-call argument rather than a field on the cache because the context a store
    /// needs is per-session: a server hosting several roots renders each against its own cwd,
    /// and a closure stored at construction could not see it.
    ///
    /// `alive` decides whether a lock's holder is still running. [`lock::pid_alive`] is right
    /// for the TUI; a server ANDs in a port probe, since a recycled pid would otherwise make a
    /// stale lock look live forever.
    pub fn admit<N: serde::Serialize + serde::de::DeserializeOwned + Clone>(
        &self,
        id: &str,
        make_store: impl FnOnce(&Path) -> std::io::Result<P>,
        alive: impl Fn(&Holder<N>) -> bool,
    ) -> Admission<P, N> {
        // Admissions of ANY session are serialized, and the resident is re-checked once this
        // caller is through the gate (#169). Without both halves, concurrent first-pulls of one
        // session each open their own store on the same backing and each fold into it — every
        // caller sees no resident because the winner has not installed one yet, and `acquire`
        // denies none of them because none is a different process. The evidence is a record log
        // whose lines carry up to six records, scrambled and duplicated: not two writers, N.
        let _gate = lock_recover(&self.admitting);
        if let Some(session) = self.shared_peek(id).filter(|ss| !ss.frozen()) {
            // Someone admitted it while we waited. A live resident IS the admission — take it
            // rather than opening a second one beside it.
            let committed = session.counters().2;
            let _ = self.touch(id);
            return Admission::Owned {
                session,
                origin: Origin::Retained { committed },
            };
        }
        let Some(entries) = &self.entries else {
            return Admission::Denied(Denial::Unavailable(Unavailable::NoCacheFlag));
        };
        let Some(src) = self.resolve(id) else {
            return Admission::Denied(Denial::Unavailable(Unavailable::UnknownSession));
        };
        // A session this process RELEASED but kept resident (#109). `frozen` is the precise test:
        // a quiesced session has stopped writing, so its backing length cannot drift while we
        // decide what to do with it. The cache computes its half of the witness and passes plain
        // data across the seam (#167 §4.2) — never the resident itself.
        let resident = self.shared_peek(id).filter(|ss| ss.frozen());
        let ours = resident.as_ref().map(|ss| fs::Witness {
            backing_len: ss.store_read(|_, st| st.backing_len()),
            committed: ss.counters().2,
        });
        let mut make_store = Some(make_store);
        let mut make_store = |dir: &Path| (make_store.take().expect("called once"))(dir);
        match entries.open::<N>(id, &src, ours, &mut make_store, &alive) {
            Opened::Denied(x) => Admission::Denied(x),
            Opened::Retained { writer } => {
                let session = resident.expect("Retained is only reachable with a resident");
                let origin = Origin::Retained {
                    committed: session.counters().2,
                };
                match writer {
                    // Re-arm and thaw in one step: `attach_writer` drains anything the fold
                    // authored before the freeze and clears it.
                    Some(w) => session.attach_writer(w),
                    // If the stream cannot be reopened the session must still THAW — a
                    // retained session left frozen would silently stop following its
                    // transcript, which is worse than serving it undurable. (The provider
                    // already handed the lock back.)
                    None => session.thaw(),
                }
                Admission::Owned { session, origin }
            }
            Opened::Owned {
                store,
                loaded: tail,
                prefix_reused,
                origin,
                resumed,
                writer,
            } => {
                // Join the resident's already-decoded prefix (when the provider reused it)
                // with the tail it loaded; a resume then cuts the join to what the records
                // corroborate (I6's vector half — the store half, `adopt`, ran provider-side).
                let mut loaded = match (&resident, prefix_reused) {
                    (Some(ss), true) => ss.committed_bvs(),
                    _ => Vec::new(),
                };
                loaded.extend(tail);
                let session = match resumed {
                    Some(r) => {
                        let (mm, resume, keep) = *r;
                        loaded.truncate(keep);
                        SharedSession::resume(src.agent(), src.path(), store, loaded, mm, &resume)
                    }
                    None => SharedSession::with_store(src.agent(), src.path(), store),
                };
                let session = self.install(id, session);
                if let Some(w) = writer {
                    session.attach_writer(w);
                }
                Admission::Owned { session, origin }
            }
        }
    }

    /// Install a freshly built session as `id`'s resident, replacing whatever was there.
    fn install(&self, id: &str, session: SharedSession<P>) -> std::sync::Arc<SharedSession<P>> {
        let session = std::sync::Arc::new(session);
        lock_recover(&self.pull_residents)
            .insert(id.to_string(), (Instant::now(), session.clone()));
        session
    }

    /// Publish this process's note for whoever finds the lock held — separate from
    /// [`admit`](Self::admit) because the useful facts arrive later (a server has no port until
    /// it binds). It can therefore only land on a session this process **already owns**.
    /// Returns whether it landed. `false` means this process does not own `id` — it was never
    /// admitted, admission was denied, or the entry has been released. That is worth checking:
    /// publishing before admitting is silently a no-op, and an HTML server that did exactly
    /// that left every lock's note `null`, so a peer finding it held had nowhere to redirect.
    #[must_use]
    pub fn publish<N: serde::Serialize + serde::de::DeserializeOwned + Clone>(
        &self,
        id: &str,
        note: N,
    ) -> bool {
        let Some(entries) = &self.entries else {
            return false;
        };
        fs::Entries::<P>::publish(entries, id, note)
    }
}

/// Releasing needs no [`DurableStore`] bound — only a pid comparison — which is what lets
/// [`Drop`] do it too. That matters: every `?` on an error path skips an explicit call, and a
/// lock outliving its process denies the session to the next run until the pid dies, which for a
/// recycled pid can be never.
impl<P: BlockStore, A> SessionCache<P, A> {
    /// **Quiesce** and unlock ONE session — the TUI's `Outcome::Switch`, or a server dropping a
    /// root. The session stays RESIDENT and readable; what stops is every write, so re-admitting
    /// it later can retain its blocks instead of rebuilding them (#109).
    ///
    /// Quiescing, not merely flushing: a released session that kept its writer would append to an
    /// entry this process no longer owns — two writers on one entry, which is what #96's
    /// single-writer coordination exists to prevent. See [`SharedSession::quiesce`].
    pub fn release(&self, id: &str) {
        if let Some(ss) = self.shared_peek(id) {
            // Quiescing drops the resident's `EntryWriter`; the writer's drop releases the
            // entry LOCK (#167 §4.4) — "we are writing" and "we hold the entry" are one fact.
            ss.quiesce();
        }
    }

    /// Flush and unlock EVERYTHING. Both `process::exit(0)` sites call this explicitly, because
    /// they skip destructors and `Drop` never runs.
    pub fn release_all(&self) {
        // Quiesce every resident: each drop of an `EntryWriter` releases its own lock
        // (#167 §4.4) — there is no owned-locks map to walk any more, because the writer
        // IS the ownership.
        let ids: Vec<String> = lock_recover(&self.pull_residents).keys().cloned().collect();
        for id in ids {
            self.release(&id);
        }
    }
}

impl<P: BlockStore, A> Drop for SessionCache<P, A> {
    fn drop(&mut self) {
        self.release_all();
    }
}

/// The outcome of asking a durable cache for a session (#96 §8.1).
///
/// **Two** outcomes, not three. A cache entry is never shared, so you either own it or you do
/// not — and on a denial *nothing was opened*.
///
/// There is no third answer and no way to ask for one (#163). A `open_uncached` escape hatch used
/// to sit beside this, handing back a session with no entry and no lock "explicitly at the call
/// site"; every caller reached for it on denial, which is how one transcript ended up with two
/// folds appending to one log. A session this process does not own is a session it does not
/// serve — it routes the client to the owner, or says why it cannot.
pub enum Admission<P: BlockStore, N> {
    /// Exclusive owner. Durable, and resumed when the cache was valid.
    Owned {
        session: std::sync::Arc<SharedSession<P>>,
        origin: Origin,
    },
    /// Not the owner. **Nothing was opened, nothing is shared.** `N` is the note a live
    /// peer published (#167 step 1: the note now names the PROCESS holding the lock, not
    /// the store — `DurableStore` is back to purely "how blocks become bytes").
    Denied(Denial<N>),
}

/// The **in-process view surface** (#85) — on any cache whose blocks are `Arc<Block>`: ONE call
/// per tick advances the resident borrow-to-tail and returns the splice-shaped [`ViewDelta`]
/// (Arc-clone blocks + times + metrics + tasks). The same resident serves the wire pull protocol;
/// there is exactly one live tier (#85).
///
/// Generic over the store rather than fixed to [`ArcStore`](crate::engine::ArcStore), because a
/// durable TUI keeps `Arc<Block>` blocks *and* a log behind them (#96) — the tick is the same
/// either way.
impl<P: BlockStore<Bv = std::sync::Arc<crate::model::Block>>, A> SessionCache<P, A> {
    /// `open` builds the store when `id` is not yet resident.
    ///
    /// On a **durable** cache it is never called: [`admit`](Self::admit) is the only way a
    /// durable session comes into being, because it is the only path that takes the lock. A tick
    /// on an id that was never admitted is idle, not a silently unlocked session.
    pub fn poll_view(
        &self,
        id: &str,
        open: impl FnOnce() -> P,
    ) -> Option<std::io::Result<crate::cache::ViewDelta>> {
        let ss = if self.entries.is_some() {
            self.touch(id)?
        } else {
            let src = self.resolve(id)?;
            self.shared_session(id, || {
                SharedSession::with_store(src.agent(), src.path(), open())
            })
        };
        ss.poll_view().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Agent;
    use claude_replay_core::parse_session_as;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp() -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        std::env::temp_dir().join(format!(
            "cr-cache-{}-{}.jsonl",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    const CLAUDE_1: &str = "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"go\"}]},\"timestamp\":\"2026-07-26T10:00:00Z\"}\n";
    const CLAUDE_2: &str = "{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"done\"}],\"usage\":{\"input_tokens\":5,\"output_tokens\":8}},\"timestamp\":\"2026-07-26T10:00:01Z\"}\n";

    fn append(path: &PathBuf, s: &str) {
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(s.as_bytes()).unwrap();
    }

    /// #168: a resident someone is STILL USING is not idle, whatever its clock says.
    ///
    /// The clock is stamped when a request takes the session, not when it finishes, so a fold
    /// longer than the TTL leaves its own resident evictable mid-write. Dropping it there costs
    /// the entry's lock, not just its cache: the next admission finds nothing resident and
    /// `lock::acquire` never denies its own pid, so it opens a SECOND store on the same log and
    /// the two interleave into records no client can parse.
    #[test]
    fn reap_spares_a_resident_that_is_still_held() {
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache: SessionCache<crate::engine::ArcStore> = SessionCache::new();
        let src = Transcript::open(Agent::CLAUDE, path.clone());
        cache.register("s", src.clone());

        let held = cache.shared_session("s", || {
            SharedSession::with_store(src.agent(), src.path(), crate::engine::ArcStore)
        });
        assert!(
            cache.reap(0).is_empty(),
            "a zero TTL still cannot evict a session a request is holding"
        );
        assert!(cache.shared_peek("s").is_some(), "and it stays resident");

        drop(held);
        let evicted = cache.reap(0);
        assert_eq!(evicted.len(), 1, "once nobody holds it, the TTL applies");
        assert!(cache.shared_peek("s").is_none());
    }

    /// The unified live tier's in-process surface (#85): registered → first `poll_view`
    /// materializes the resident and hands the WHOLE file as the delta (`changed_from` 0);
    /// an unchanged file is idle (`None`); an append hands only the delta with the boundary
    /// past the stable prefix; the joined view equals a full parse; `reap` evicts back to
    /// the registry and a later poll re-materializes. One tier, one lifecycle, both
    /// protocols on the same resident.
    #[test]
    fn poll_view_lifecycle_equals_full_parse() {
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache: SessionCache<crate::engine::ArcStore> = SessionCache::new();

        assert!(
            cache.poll_view("s", || crate::engine::ArcStore).is_none(),
            "unregistered"
        );
        cache.register("s", Transcript::open(Agent::CLAUDE, path.clone()));
        let d1 = cache
            .poll_view("s", || crate::engine::ArcStore)
            .expect("registered")
            .expect("readable");
        assert_eq!(d1.changed_from, 0, "first poll: everything is new");
        let n1 = d1.committed_len + d1.provisional.len();
        assert!(n1 > 0);
        assert!(
            cache.poll_view("s", || crate::engine::ArcStore).is_none(),
            "idle on an unchanged file"
        );

        append(&path, CLAUDE_2);
        let d2 = cache
            .poll_view("s", || crate::engine::ArcStore)
            .expect("registered")
            .expect("readable");
        assert!(
            d2.changed_from <= d1.committed_len + d1.provisional.len(),
            "boundary within the previously-seen view"
        );
        // Reconstruct the joined view the way a View splices, and compare to a full parse.
        let mut joined: Vec<crate::model::Block> = Vec::new();
        for d in [&d1, &d2] {
            joined.truncate(d.committed_len - d.committed_delta.len());
            joined.extend(d.committed_delta.iter().map(|a| a.as_ref().clone()));
            joined.extend(d.provisional.iter().map(|a| a.as_ref().clone()));
        }
        let full = parse_session_as(Agent::CLAUDE, &path).unwrap();
        assert_eq!(
            format!("{joined:?}"),
            format!("{:?}", full.blocks()),
            "spliced view == full parse"
        );

        // Reap evicts; the registry survives; a later poll re-materializes whole-file.
        cache.reap(0);
        assert!(cache.shared_peek("s").is_none());
        assert!(cache.is_registered("s"));
        let d3 = cache
            .poll_view("s", || crate::engine::ArcStore)
            .expect("registered")
            .expect("readable");
        assert_eq!(d3.changed_from, 0, "re-materialized from scratch");
        let _ = std::fs::remove_file(&path);
    }

    /// The pull-resident lifecycle: `shared_session` materializes once (same `Arc` back on every
    /// call), `shared_peek` sees it without materializing, `reap` evicts it alongside the follower
    /// residents **once nobody holds it** (#168), and a later `shared_session` re-materializes
    /// fresh — on a genuinely new session, the old one having been dropped and its store closed.
    #[test]
    fn pull_resident_lifecycle_materialize_peek_reap() {
        use std::sync::Arc;
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache: SessionCache = SessionCache::new();

        assert!(cache.shared_peek("s").is_none(), "nothing resident yet");
        let a = cache.shared_session("s", || {
            SharedSession::with_store(Agent::CLAUDE, &path, TierBStore::new())
        });
        let b = cache.shared_session("s", || panic!("must not re-open a resident session"));
        assert!(Arc::ptr_eq(&a, &b), "materialized once, shared");
        assert!(
            cache.shared_peek("s").is_some_and(|p| Arc::ptr_eq(&p, &a)),
            "peek sees the resident without materializing"
        );

        // Let go before reaping: a held resident is in USE, and eviction under a live reference
        // is what lets a second store open on the same log.
        let gone = Arc::downgrade(&a);
        drop(a);
        drop(b);
        cache.reap(0);
        assert!(cache.shared_peek("s").is_none(), "reaped with the rest");
        assert!(
            gone.upgrade().is_none(),
            "and really dropped — its store is closed before anything reopens the entry"
        );
        let c = cache.shared_session("s", || {
            SharedSession::with_store(Agent::CLAUDE, &path, TierBStore::new())
        });
        assert!(gone.upgrade().is_none(), "re-admit re-materializes");
        drop(c);
        let _ = std::fs::remove_file(&path);
    }

    /// `reap_over_budget` — the TUI's residency policy on the unified tier (#85): the
    /// pinned root never counts or evicts; beyond the budget, the least-recently-touched
    /// residents go; the registry survives so a later poll re-materializes.
    #[test]
    fn reap_over_budget_pins_root_and_evicts_lru() {
        let cache: SessionCache<crate::engine::ArcStore> = SessionCache::new();
        for id in ["root", "a", "b", "c"] {
            let path = tmp();
            std::fs::write(&path, CLAUDE_1).unwrap();
            cache.register(id, Transcript::open(Agent::CLAUDE, path));
        }
        // Materialize in a known touch order: root, then a (oldest child), b, c (newest).
        for id in ["root", "a", "b", "c"] {
            cache.poll_view(id, || crate::engine::ArcStore);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        cache.reap_over_budget(2, "root");
        let mut resident: Vec<&str> = ["root", "a", "b", "c"]
            .into_iter()
            .filter(|id| cache.shared_peek(id).is_some())
            .collect();
        resident.sort();
        assert_eq!(
            resident,
            vec!["b", "c", "root"],
            "root pinned; newest 2 children kept; oldest evicted"
        );
        assert!(cache.is_registered("a"), "eviction is residency-only");
    }

    /// `register_new` keeps the first descriptor against a later bare fallback.
    #[test]
    fn register_new_preserves_first_source() {
        let cache: SessionCache = SessionCache::new();
        cache.register_new("c", Transcript::open(Agent::CLAUDE, PathBuf::from("rich")));
        cache.register_new("c", Transcript::open(Agent::CODEX, PathBuf::from("bare")));
        let s = cache.resolve("c").expect("registered");
        assert_eq!(s.path(), PathBuf::from("rich").as_path());
        assert_eq!(s.agent(), Agent::CLAUDE);
    }
}

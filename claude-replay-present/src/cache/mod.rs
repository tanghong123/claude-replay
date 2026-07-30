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
//! - **hibernated** — [`reap`](SessionCache::reap) evicts residents idle past a TTL and hands
//!   them back for hibernation; a [`PersistentStore`]'s backing (plus its
//!   [`hibernate_state`](PersistentStore::hibernate_state) sidecar) survives, so a later open
//!   [`restore`](SharedSession::restore)s without re-folding the whole transcript.
//!
//! The `A` parameter is an opaque per-session **presentation sidecar** slot
//! ([`aux_put`](SessionCache::aux_put)/[`aux_take`](SessionCache::aux_take)): view-parameter-
//! dependent state (the TUI's measured heights, the server's titles/parents) lives with the
//! session it belongs to, with registry lifetime and consumer-owned validity.
//!
//! The maps are guarded by independent mutexes and never locked simultaneously, so the cache
//! can't self-deadlock. Rendering happens in the caller *between* cache calls; the only work
//! under a cache lock is the brief O(delta) follower advance.
// The 4-member-cursor pull protocol (Cursor/PullReply + the executable-spec PullClient).
mod stream;
// SharedSession: the one live tier — the follower + store both frontends share.
mod shared;
#[allow(unused_imports)]
pub use crate::engine::tier_b::{Deferred, TierBSession, TierBStore};
use crate::engine::BlockStore;
#[allow(unused_imports)]
pub use shared::{PersistentStore, PullDelta, SharedSession, ViewDelta};
#[allow(unused_imports)]
pub use stream::{pull, pull_indices, Applied, Cursor, PullClient, PullReply};

use std::collections::HashMap;
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
        }
    }
}

impl<P: BlockStore, A> SessionCache<P, A> {
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

    /// Evict every resident (follower **and** pull-servable) idle for longer than `ttl_ms` back
    /// down to tier (c). Their registry sources remain, so a later `poll`/`shared_session`
    /// re-materializes them. Returns the **evicted pull residents** so the owner can persist each
    /// one's serving state (see [`SharedSession::hibernate`]) before the reference drops — the
    /// cache stays policy-free about where materializations live.
    pub fn reap(&self, ttl_ms: u128) -> Vec<(String, std::sync::Arc<SharedSession<P>>)> {
        let mut evicted = Vec::new();
        self.pull_residents
            .lock()
            .unwrap()
            .retain(|id, (last_seen, ss)| {
                let keep = last_seen.elapsed().as_millis() < ttl_ms;
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

    /// Drop one pull resident immediately (regardless of idle time) — used when a restored
    /// materialization turns out stale ([`SharedSession::hibernation_stale`]) and must be replaced
    /// by a fresh live session.
    pub fn remove_pull(&self, id: &str) {
        lock_recover(&self.pull_residents).remove(id);
    }
}

/// The **in-process view surface** (#85) — on a cache whose live store is
/// [`ArcStore`](crate::engine::ArcStore): ONE call per tick materializes the resident on
/// first use (from the registry), advances it borrow-to-tail, and returns the
/// splice-shaped [`ViewDelta`] — Arc-clone blocks + times + metrics + tasks. The same
/// resident serves the wire pull protocol; there is exactly one live tier (#85).
impl<A> SessionCache<crate::engine::ArcStore, A> {
    pub fn poll_view(&self, id: &str) -> Option<std::io::Result<crate::cache::ViewDelta>> {
        let src = self.resolve(id)?;
        let ss = self.shared_session(id, || {
            SharedSession::with_store(src.agent(), src.path(), crate::engine::ArcStore)
        });
        ss.poll_view().transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::parse_session_as;
    use crate::Agent;
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

        assert!(cache.poll_view("s").is_none(), "unregistered");
        cache.register("s", Transcript::open(Agent::Claude, path.clone()));
        let d1 = cache.poll_view("s").expect("registered").expect("readable");
        assert_eq!(d1.changed_from, 0, "first poll: everything is new");
        let n1 = d1.committed_len + d1.provisional.len();
        assert!(n1 > 0);
        assert!(cache.poll_view("s").is_none(), "idle on an unchanged file");

        append(&path, CLAUDE_2);
        let d2 = cache.poll_view("s").expect("registered").expect("readable");
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
        let full = parse_session_as(Agent::Claude, &path).unwrap();
        assert_eq!(
            format!("{joined:?}"),
            format!("{:?}", full.blocks()),
            "spliced view == full parse"
        );

        // Reap evicts; the registry survives; a later poll re-materializes whole-file.
        cache.reap(0);
        assert!(cache.shared_peek("s").is_none());
        assert!(cache.is_registered("s"));
        let d3 = cache.poll_view("s").expect("registered").expect("readable");
        assert_eq!(d3.changed_from, 0, "re-materialized from scratch");
        let _ = std::fs::remove_file(&path);
    }

    /// The pull-resident lifecycle: `shared_session` materializes once (same `Arc` back on every
    /// call), `shared_peek` sees it without materializing, `reap` evicts it alongside the follower
    /// residents, and a later `shared_session` re-materializes fresh.
    #[test]
    fn pull_resident_lifecycle_materialize_peek_reap() {
        use std::sync::Arc;
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache: SessionCache = SessionCache::new();

        assert!(cache.shared_peek("s").is_none(), "nothing resident yet");
        let a = cache.shared_session("s", || {
            SharedSession::with_store(Agent::Claude, &path, TierBStore::new())
        });
        let b = cache.shared_session("s", || panic!("must not re-open a resident session"));
        assert!(Arc::ptr_eq(&a, &b), "materialized once, shared");
        assert!(
            cache.shared_peek("s").is_some_and(|p| Arc::ptr_eq(&p, &a)),
            "peek sees the resident without materializing"
        );

        cache.reap(0);
        assert!(cache.shared_peek("s").is_none(), "reaped with the rest");
        let c = cache.shared_session("s", || {
            SharedSession::with_store(Agent::Claude, &path, TierBStore::new())
        });
        assert!(!Arc::ptr_eq(&a, &c), "re-admit re-materializes");
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
            cache.register(id, Transcript::open(Agent::Claude, path));
        }
        // Materialize in a known touch order: root, then a (oldest child), b, c (newest).
        for id in ["root", "a", "b", "c"] {
            cache.poll_view(id);
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
        cache.register_new("c", Transcript::open(Agent::Claude, PathBuf::from("rich")));
        cache.register_new("c", Transcript::open(Agent::Codex, PathBuf::from("bare")));
        let s = cache.resolve("c").expect("registered");
        assert_eq!(s.path(), PathBuf::from("rich").as_path());
        assert_eq!(s.agent(), Agent::Claude);
    }
}

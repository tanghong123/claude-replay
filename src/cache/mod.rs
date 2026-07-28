//! **A residency cache for incrementally-followed sessions** — the concrete session-domain
//! owner the live HTML server sits on. It absorbs the retired generic `SessionStore`'s
//! mechanism (a keyed registry + a TTL-reaped resident set behind two independent mutexes) as
//! concrete types, and additionally **owns the session domain**: each resident holds an open
//! incremental [`FollowParser`], and [`poll`](SessionCache::poll) returns an OWNED, current
//! [`Session`] equal to a full [`parse_session_as`](crate::engine::parse_session_as) of the source's
//! current bytes. The caller (the server) keeps only presentation state (diff baselines,
//! titles) — the follower and the `Session` live here.
//!
//! ## Residency tiers
//! - **(c) registered** — a keyed [`Transcript`] (agent + transcript path): we know where a
//!   session lives, but hold no follower. Costs nothing; the common case for a large sub-agent
//!   tree whose children were discovered but never opened.
//! - **(a) resident** — a registered session [`poll`](SessionCache::poll)ed recently: it holds
//!   an open `FollowParser` and a `last_seen` clock. [`reap`](SessionCache::reap) evicts
//!   residents idle past a TTL back down to tier (c); a later `poll` re-materializes from the
//!   registry (a fresh follower folds the whole current file).
//!
//! - **(a′) pull-resident** — a [`SharedSession`] a `/pull` client is following (see
//!   [`shared_session`](SessionCache::shared_session)): the same registry + reap policy, serving
//!   the cursor-pull protocol instead of `poll`.
//!
//! The maps are guarded by independent mutexes and never locked simultaneously, so the
//! cache can't self-deadlock. The expensive work — rendering — happens in the caller *between*
//! cache calls; the only work under a cache lock is the brief O(delta) follower read in `poll`.

#[allow(dead_code)]
// wired into serve.rs when the pull path replaces stream_delta (Phase C step 4/5)
mod stream;
// SharedSession: the pull-servable live state (present-layer). Wired into serve.rs with the pull
// path; the Arc/lock-free concurrency wrapper joins it there (real concurrent clients).
#[allow(dead_code)]
mod shared;
#[allow(unused_imports)]
pub use crate::engine::tier_b::{Deferred, TierBSession, TierBStore};
#[allow(unused_imports)]
pub use shared::{PullDelta, SharedSession};
#[allow(unused_imports)]
pub use stream::{pull, pull_indices, Cursor, PullReply};

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::engine::Session;
use crate::follow::FollowParser;
use crate::Transcript;

/// A resident session: an open incremental follower over its source. Its idle clock lives in
/// the residents map alongside it.
struct Resident {
    follower: FollowParser,
}

/// A keyed cache of sessions in two residency tiers (see the module docs). Owns the session
/// domain — the followers, the materialized [`Session`]s, and the pull-servable
/// [`SharedSession`]s — so its consumer (the live server) keeps only presentation state.
pub struct SessionCache {
    /// Tier (c): every known session → its [`Transcript`] source handle.
    registry: Mutex<HashMap<String, Transcript>>,
    /// Tier (a): the currently-resident subset → (last polled, open follower).
    residents: Mutex<HashMap<String, (Instant, Resident)>>,
    /// Tier (a′): the **pull-servable** residents — one [`SharedSession`] per id a `/pull` client
    /// is following (`Arc` so any number of request threads share it). A resident kind of its own
    /// because it serves a different protocol (cursor pulls, borrow-to-tail) than the `poll`
    /// followers, but under the same owner and the same [`reap`](Self::reap) policy.
    pull_residents: Mutex<HashMap<String, PullResident>>,
}

/// A pull-servable resident: its idle clock + the shared session. Tier-b-backed — the committed
/// block content of a followed session lives in the store's on-disk backing, not RAM.
type PullResident = (Instant, std::sync::Arc<SharedSession<TierBStore>>);

impl Default for SessionCache {
    fn default() -> Self {
        Self {
            registry: Mutex::new(HashMap::new()),
            residents: Mutex::new(HashMap::new()),
            pull_residents: Mutex::new(HashMap::new()),
        }
    }
}

impl SessionCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or overwrite) a session's tier-(c) source.
    pub fn register(&self, id: &str, src: Transcript) {
        self.registry.lock().unwrap().insert(id.to_string(), src);
    }

    /// Register a session only if not already known — preserves the first (richest,
    /// ancestry-bearing) descriptor against a later bare fallback.
    pub fn register_new(&self, id: &str, src: Transcript) {
        self.registry
            .lock()
            .unwrap()
            .entry(id.to_string())
            .or_insert(src);
    }

    /// Whether `id` has a tier-(c) source.
    pub fn is_registered(&self, id: &str) -> bool {
        self.registry.lock().unwrap().contains_key(id)
    }

    /// The tier-(c) source for `id`, if known.
    pub fn resolve(&self, id: &str) -> Option<Transcript> {
        self.registry.lock().unwrap().get(id).cloned()
    }

    /// Materialize on the first call (open a [`FollowParser`] on the source and fold its current
    /// bytes), tail on later calls (fold only appended bytes), and return an OWNED current
    /// [`Session`] equal to a full [`parse_session_as`](crate::engine::parse_session_as) of the current
    /// file. Returns `None` when the source hasn't grown since the last poll (idle) or when `id`
    /// is unregistered; `Some(Err)` when the source is unreadable. Bumps the resident's idle
    /// clock.
    pub fn poll(&self, id: &str) -> Option<std::io::Result<Session>> {
        let src = self.resolve(id)?;
        let mut residents = self.residents.lock().unwrap();
        let (last_seen, resident) = residents.entry(id.to_string()).or_insert_with(|| {
            (
                Instant::now(),
                Resident {
                    follower: src.follow(),
                },
            )
        });
        *last_seen = Instant::now();
        // `poll_session` returns a fully-assembled Session (cwd + sub-agent transcripts filled),
        // so the cache needs no core internals — the step toward moving it into the present layer.
        resident.follower.poll_session().transpose()
    }

    /// Like [`poll`](Self::poll), but through the follower's **delta** surface: additionally
    /// returns `changed_from` — the first block index that differs from the previous poll — so a
    /// windowed/render-caching consumer (the TUI) keeps its fold state and rendered lines for the
    /// unchanged prefix instead of re-scanning the whole block list. Same lifecycle as `poll`:
    /// materialize on first call, `None` when idle/unregistered, bumps the idle clock.
    #[allow(clippy::type_complexity)]
    pub fn poll_delta(
        &self,
        id: &str,
    ) -> Option<
        std::io::Result<(
            Vec<crate::model::Block>,
            Vec<Option<crate::model::EpochSeconds>>,
            crate::metrics::Metrics,
            usize,
        )>,
    > {
        let src = self.resolve(id)?;
        let mut residents = self.residents.lock().unwrap();
        let (last_seen, resident) = residents.entry(id.to_string()).or_insert_with(|| {
            (
                Instant::now(),
                Resident {
                    follower: src.follow(),
                },
            )
        });
        *last_seen = Instant::now();
        resident.follower.poll_delta().transpose()
    }

    /// Evict follower residents beyond `budget` — least-recently-touched first — never evicting
    /// `pinned` (the session the viewer is anchored to, which doesn't count against the budget).
    /// The navigation-recency residency policy the TUI rides: evicted followers re-materialize
    /// from the registry on their next poll (a fresh whole-file fold).
    pub fn reap_over_budget(&self, budget: usize, pinned: &str) {
        let mut residents = self.residents.lock().unwrap();
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
        open: impl FnOnce() -> SharedSession<TierBStore>,
    ) -> std::sync::Arc<SharedSession<TierBStore>> {
        let mut m = self.pull_residents.lock().unwrap();
        let entry = m
            .entry(id.to_string())
            .or_insert_with(|| (Instant::now(), std::sync::Arc::new(open())));
        entry.0 = Instant::now();
        entry.1.clone()
    }

    /// Peek at an already-resident pull session **without** materializing or touching its idle
    /// clock — for a read that shouldn't keep the session alive (e.g. a child deriving its title
    /// from its parent's maintained meta iff the parent happens to be resident).
    pub fn shared_peek(&self, id: &str) -> Option<std::sync::Arc<SharedSession<TierBStore>>> {
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
    pub fn reap(&self, ttl_ms: u128) -> Vec<(String, std::sync::Arc<SharedSession<TierBStore>>)> {
        self.residents
            .lock()
            .unwrap()
            .retain(|_, (last_seen, _)| last_seen.elapsed().as_millis() < ttl_ms);
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

    /// Drop one pull resident immediately (regardless of idle time) — used when a restored
    /// materialization turns out stale ([`SharedSession::hibernation_stale`]) and must be replaced
    /// by a fresh live session.
    pub fn remove_pull(&self, id: &str) {
        self.pull_residents.lock().unwrap().remove(id);
    }

    /// The ids currently resident (tier (a)) — the set the caller polls each cycle.
    pub fn resident_ids(&self) -> Vec<String> {
        self.residents.lock().unwrap().keys().cloned().collect()
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

    /// A materialized `poll` must return byte-identical (`Debug`) to a full `parse_session_as`
    /// of the current file — the cache's Session assembly (builder snapshot + cwd + sub-agent
    /// transcripts) equals the whole-file parse. Mirrors `follow_matches_full_reparse`.
    #[test]
    fn poll_equals_full_parse() {
        let path = tmp();
        std::fs::write(&path, format!("{CLAUDE_1}{CLAUDE_2}")).unwrap();
        let cache = SessionCache::new();
        cache.register("s", Transcript::open(Agent::Claude, path.clone()));
        let polled = cache.poll("s").expect("registered").expect("readable");
        let full = parse_session_as(Agent::Claude, &path).unwrap();
        assert_eq!(
            format!("{polled:?}"),
            format!("{full:?}"),
            "cache.poll == parse_session_as"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// The residency lifecycle the live server rides on: registered → first `poll` materializes
    /// (tier a) → a second `poll` on an unchanged file returns `None` (idle) → append to the
    /// source → `poll` returns the grown Session → `reap` past the TTL frees the resident back
    /// to tier (c) → a later `poll` re-materializes from the registry. (Ports the retired
    /// `SessionStore::tier_lifecycle_see_admit_reap_readmit`.)
    #[test]
    fn tier_lifecycle_register_poll_reap_rematerialize() {
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache = SessionCache::new();

        // Unregistered → poll is None, nothing resident.
        assert!(cache.poll("s").is_none());
        assert!(!cache.is_registered("s"));
        assert!(cache.resident_ids().is_empty());

        // Register (tier c), then the first poll materializes it (tier a).
        cache.register("s", Transcript::open(Agent::Claude, path.clone()));
        assert!(cache.is_registered("s"));
        let s1 = cache.poll("s").expect("registered").expect("readable");
        assert_eq!(cache.resident_ids(), vec!["s".to_string()]);
        let blocks1 = s1.block_count();

        // A second poll on an unchanged file → None (idle, no growth).
        assert!(cache.poll("s").is_none());

        // Append → poll returns the grown session, still == a full parse.
        append(&path, CLAUDE_2);
        let s2 = cache.poll("s").expect("registered").expect("readable");
        assert!(s2.block_count() >= blocks1, "grew");
        assert_eq!(
            format!("{s2:?}"),
            format!("{:?}", parse_session_as(Agent::Claude, &path).unwrap())
        );

        // Reap past the TTL evicts the resident back to tier (c); the registry survives.
        cache.reap(0);
        assert!(cache.resident_ids().is_empty());
        assert!(cache.is_registered("s"));

        // A later poll re-materializes from the registry (fresh follower, whole file).
        let s3 = cache.poll("s").expect("registered").expect("readable");
        assert_eq!(cache.resident_ids(), vec!["s".to_string()]);
        assert_eq!(
            format!("{s3:?}"),
            format!("{:?}", parse_session_as(Agent::Claude, &path).unwrap())
        );

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
        let cache = SessionCache::new();

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

    /// `poll_delta` (the TUI's live surface): first poll folds the whole file with
    /// `changed_from == 0`; a pure append returns the grown list with the unchanged prefix intact
    /// (`changed_from` past it); an idle poll is `None`. Same lifecycle as `poll`.
    #[test]
    fn poll_delta_forwards_the_follower_delta_surface() {
        let path = tmp();
        std::fs::write(&path, CLAUDE_1).unwrap();
        let cache = SessionCache::new();
        cache.register("s", Transcript::open(Agent::Claude, path.clone()));

        let (blocks1, _t, _m, cf1) = cache.poll_delta("s").unwrap().unwrap();
        assert_eq!(cf1, 0, "first poll: everything is new");
        assert!(!blocks1.is_empty());
        assert!(cache.poll_delta("s").is_none(), "idle");

        append(&path, CLAUDE_2);
        let (blocks2, _t, _m, cf2) = cache.poll_delta("s").unwrap().unwrap();
        assert!(blocks2.len() >= blocks1.len(), "grew");
        assert_eq!(
            format!("{:?}", &blocks2[..cf2]),
            format!("{:?}", &blocks1[..cf2]),
            "the kept prefix is unchanged"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// `reap_over_budget` — the TUI's residency policy: the pinned root never counts or evicts;
    /// beyond the budget, the least-recently-touched followers go; the registry survives so a
    /// later poll re-materializes.
    #[test]
    fn reap_over_budget_pins_root_and_evicts_lru() {
        let cache = SessionCache::new();
        for id in ["root", "a", "b", "c"] {
            let path = tmp();
            std::fs::write(&path, CLAUDE_1).unwrap();
            cache.register(id, Transcript::open(Agent::Claude, path));
        }
        // Materialize in a known touch order: root, then a (oldest child), b, c (newest).
        for id in ["root", "a", "b", "c"] {
            cache.poll(id);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        cache.reap_over_budget(2, "root");
        let mut resident = cache.resident_ids();
        resident.sort();
        assert_eq!(
            resident,
            vec!["b".to_string(), "c".to_string(), "root".to_string()],
            "root pinned; newest 2 children kept; oldest evicted"
        );
        assert!(cache.is_registered("a"), "eviction is residency-only");
    }

    /// `register_new` keeps the first descriptor against a later bare fallback.
    #[test]
    fn register_new_preserves_first_source() {
        let cache = SessionCache::new();
        cache.register_new("c", Transcript::open(Agent::Claude, PathBuf::from("rich")));
        cache.register_new("c", Transcript::open(Agent::Codex, PathBuf::from("bare")));
        let s = cache.resolve("c").expect("registered");
        assert_eq!(s.path(), PathBuf::from("rich").as_path());
        assert_eq!(s.agent(), Agent::Claude);
    }
}

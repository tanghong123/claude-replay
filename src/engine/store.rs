//! **A residency store for incrementally-followed sessions** (design §Phase 8 — the
//! "SessionStore + tiers"). This is the agent- and transport-agnostic bookkeeping the
//! multi-agent live HTML server sits on: a keyed registry of *what sessions exist and
//! where to find them*, plus the subset currently *resident* (holding an open
//! incremental follower). It knows nothing about HTTP, HTML, or how a session renders —
//! that stays in the serving layer, so the two can be reasoned about (and tested)
//! separately.
//!
//! ## Residency tiers
//! - **(c) path-only** — an entry in [`register`](SessionStore::register)ed `registry`:
//!   we know the session's id and its `Info` (where its source is, how to title it), but
//!   we hold no follower. Costs nothing; the common case for a large sub-agent tree whose
//!   children were discovered but never opened.
//! - **(a) resident** — a session that's been [`see`](SessionStore::see)n recently: it
//!   holds a caller-owned `Res` (an open incremental follower + whatever diff baseline the
//!   caller keeps) and a `last_seen` clock. [`reap`](SessionStore::reap) evicts residents
//!   idle past a TTL back down to tier (c); a later `see` re-promotes them.
//!
//! (Tier **(b) materialized** — the on-disk `<id>.jsonl` the server serves bytes from — is
//! the serving layer's concern, layered *over* this store, not tracked here.)
//!
//! The two maps are guarded by independent mutexes and never locked simultaneously, so the
//! store can't self-deadlock; each method takes one lock briefly. The expensive work —
//! opening a follower, rendering — happens in the caller *between* store calls, never under
//! a store lock.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

/// A keyed store of sessions in two residency tiers (see the module docs). `Info` is the
/// path-only descriptor (cheap, `Clone`); `Res` is the caller-owned resident payload (an
/// open follower + its diff baseline) the store holds for the lifetime of residency.
pub(crate) struct SessionStore<Info, Res> {
    /// Tier (c): every known session → where to find it / how to title it.
    registry: Mutex<HashMap<String, Info>>,
    /// Tier (a): the currently-resident subset → (last requested, resident payload).
    residents: Mutex<HashMap<String, (Instant, Res)>>,
}

impl<Info: Clone, Res> SessionStore<Info, Res> {
    pub(crate) fn new() -> Self {
        Self {
            registry: Mutex::new(HashMap::new()),
            residents: Mutex::new(HashMap::new()),
        }
    }

    /// Register (or overwrite) a session's tier-(c) descriptor.
    pub(crate) fn register(&self, id: &str, info: Info) {
        self.registry.lock().unwrap().insert(id.to_string(), info);
    }

    /// Register a child only if not already known — preserves the first (richest,
    /// ancestry-bearing) descriptor against a later bare fallback.
    pub(crate) fn register_new(&self, id: &str, info: Info) {
        let mut reg = self.registry.lock().unwrap();
        reg.entry(id.to_string()).or_insert(info);
    }

    /// Whether `id` has a tier-(c) descriptor.
    pub(crate) fn is_registered(&self, id: &str) -> bool {
        self.registry.lock().unwrap().contains_key(id)
    }

    /// The tier-(c) descriptor for `id`, if known.
    pub(crate) fn resolve(&self, id: &str) -> Option<Info> {
        self.registry.lock().unwrap().get(id).cloned()
    }

    /// **See** a session: if it's already resident, bump its `last_seen` and report `true`
    /// (a tier-(a) hit — the caller need do nothing more). If not, report `false` — the
    /// caller then does the expensive open/render and calls [`admit`](Self::admit) to
    /// promote it to residency.
    pub(crate) fn see(&self, id: &str) -> bool {
        let mut res = self.residents.lock().unwrap();
        if let Some((last_seen, _)) = res.get_mut(id) {
            *last_seen = Instant::now();
            true
        } else {
            false
        }
    }

    /// Promote a session to tier (a) with a freshly-built resident payload, stamping it
    /// seen now. (Pairs with a `false` from [`see`](Self::see).)
    pub(crate) fn admit(&self, id: &str, resident: Res) {
        self.residents
            .lock()
            .unwrap()
            .insert(id.to_string(), (Instant::now(), resident));
    }

    /// Evict every resident idle for longer than `ttl_ms` back down to tier (c). Their
    /// registry descriptors remain, so a later `see` re-admits them.
    pub(crate) fn reap(&self, ttl_ms: u128) {
        self.residents
            .lock()
            .unwrap()
            .retain(|_, (last_seen, _)| last_seen.elapsed().as_millis() < ttl_ms);
    }

    /// The ids currently resident (tier (a)) — the set the caller polls each cycle.
    pub(crate) fn resident_ids(&self) -> Vec<String> {
        self.residents.lock().unwrap().keys().cloned().collect()
    }

    /// Run `f` against the resident payload for `id` under the residents lock, returning
    /// its result (or `None` if `id` isn't resident — e.g. reaped since enumeration). Keep
    /// `f` cheap: it holds the lock. Used both to poll a follower and to update a baseline.
    pub(crate) fn with_resident<R>(&self, id: &str, f: impl FnOnce(&mut Res) -> R) -> Option<R> {
        self.residents
            .lock()
            .unwrap()
            .get_mut(id)
            .map(|(_, res)| f(res))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The tier lifecycle the live server depends on: path-only → resident → idle-reaped →
    // re-resident, plus the see/admit hand-off and resident mutation. This locks in the
    // behavior M16's incremental followers ride on (previously only curl-tested end-to-end).
    #[test]
    fn tier_lifecycle_see_admit_reap_readmit() {
        let store: SessionStore<String, i32> = SessionStore::new();

        // Tier (c): registered but not resident.
        store.register("a", "src-a".to_string());
        assert!(store.is_registered("a"));
        assert_eq!(store.resolve("a").as_deref(), Some("src-a"));
        assert!(!store.see("a")); // miss → caller must open
        assert!(store.resident_ids().is_empty());

        // Promote to tier (a).
        store.admit("a", 1);
        assert!(store.see("a")); // now a hit
        assert_eq!(store.resident_ids(), vec!["a".to_string()]);

        // Mutate the resident payload (the "grow → fast-forward the baseline" step).
        let doubled = store.with_resident("a", |n| {
            *n += 41;
            *n
        });
        assert_eq!(doubled, Some(42));
        assert_eq!(store.with_resident("a", |n| *n), Some(42));

        // Reap with a 0ms TTL evicts it back to tier (c) — registry intact.
        store.reap(0);
        assert!(store.resident_ids().is_empty());
        assert!(!store.see("a"));
        assert!(store.is_registered("a")); // descriptor survives eviction

        // A later see-miss + admit re-promotes with a fresh payload.
        store.admit("a", 7);
        assert_eq!(store.with_resident("a", |n| *n), Some(7));

        // A generous TTL keeps a just-seen resident.
        store.reap(60_000);
        assert_eq!(store.resident_ids().len(), 1);

        // Unknown ids: see-miss, resolve-none, with_resident-none.
        assert!(!store.see("ghost"));
        assert!(store.resolve("ghost").is_none());
        assert!(store.with_resident("ghost", |n| *n).is_none());
    }

    #[test]
    fn register_new_preserves_first_descriptor() {
        let store: SessionStore<String, ()> = SessionStore::new();
        store.register_new("c", "rich".to_string());
        store.register_new("c", "bare".to_string()); // ignored — already known
        assert_eq!(store.resolve("c").as_deref(), Some("rich"));
    }
}

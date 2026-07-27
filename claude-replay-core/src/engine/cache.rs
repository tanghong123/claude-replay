//! **A residency cache for incrementally-followed sessions** — the concrete session-domain
//! owner the live HTML server sits on. It absorbs the retired generic `SessionStore`'s
//! mechanism (a keyed registry + a TTL-reaped resident set behind two independent mutexes) as
//! concrete types, and additionally **owns the session domain**: each resident holds an open
//! incremental [`FollowParser`], and [`poll`](SessionCache::poll) returns an OWNED, current
//! [`Session`] equal to a full [`parse_session_as`](crate::parse_session_as) of the source's
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
//! The two maps are guarded by independent mutexes and never locked simultaneously, so the
//! cache can't self-deadlock. The expensive work — rendering — happens in the caller *between*
//! cache calls; the only work under a cache lock is the brief O(delta) follower read in `poll`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::engine::session::{populate_sub_agent_transcripts, Session};
use crate::follow::FollowParser;
use crate::Transcript;

/// A resident session: an open incremental follower over its source. Its idle clock lives in
/// the residents map alongside it.
struct Resident {
    follower: FollowParser,
}

/// A keyed cache of sessions in two residency tiers (see the module docs). Owns the session
/// domain — the followers and the materialized [`Session`]s — so its consumer (the live server)
/// keeps only presentation state.
pub struct SessionCache {
    /// Tier (c): every known session → its [`Transcript`] source handle.
    registry: Mutex<HashMap<String, Transcript>>,
    /// Tier (a): the currently-resident subset → (last polled, open follower).
    residents: Mutex<HashMap<String, (Instant, Resident)>>,
}

impl Default for SessionCache {
    fn default() -> Self {
        Self {
            registry: Mutex::new(HashMap::new()),
            residents: Mutex::new(HashMap::new()),
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
    /// [`Session`] equal to a full [`parse_session_as`](crate::parse_session_as) of the current
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
        match resident.follower.poll_session() {
            Ok(Some(mut session)) => {
                // Assemble the full Session exactly as `parse_session_as` does: the builder
                // leaves `cwd` None and each sub-agent's `transcript` None (it has no file
                // path); fill both from the source path now that we know it.
                session.cwd = crate::discover::session_cwd(src.path());
                populate_sub_agent_transcripts(src.agent(), src.path(), &mut session.sub_agents);
                Some(Ok(session))
            }
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }

    /// Evict every resident idle for longer than `ttl_ms` back down to tier (c). Their registry
    /// sources remain, so a later `poll` re-materializes them.
    pub fn reap(&self, ttl_ms: u128) {
        self.residents
            .lock()
            .unwrap()
            .retain(|_, (last_seen, _)| last_seen.elapsed().as_millis() < ttl_ms);
    }

    /// The ids currently resident (tier (a)) — the set the caller polls each cycle.
    pub fn resident_ids(&self) -> Vec<String> {
        self.residents.lock().unwrap().keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_session_as, Agent};
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
        let blocks1 = s1.blocks.len();

        // A second poll on an unchanged file → None (idle, no growth).
        assert!(cache.poll("s").is_none());

        // Append → poll returns the grown session, still == a full parse.
        append(&path, CLAUDE_2);
        let s2 = cache.poll("s").expect("registered").expect("readable");
        assert!(s2.blocks.len() >= blocks1, "grew");
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

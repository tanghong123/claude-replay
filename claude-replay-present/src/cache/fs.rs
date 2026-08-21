//! **The entry providers** (#167 §4.1–§4.3): everything filesystem-shaped about a durable
//! entry — the lock, the metadata stream, resume alignment — behind one small seam, so the
//! cache itself keeps no knowledge of persistence.
//!
//! The cache keeps the thread gate, the resident singleton, and installation; a provider
//! gets one method for the entry work. Two providers live here:
//!
//! - [`PerSession`] — the viewer / HTML server: an entry `LOCK` per `<session, frontend>`,
//!   resume, redirect notes. `open` can return `Denied` (a live peer holds the entry).
//! - [`SingleWriter`] — the monitor: ONE root lock taken at construction; `open` takes no
//!   per-entry locks and can never deny (a second monitor was redirected before any cache
//!   existed).
//!
//! Step-2 shape note (the as-built §8 trail): the note type `N` rides as a METHOD generic —
//! exactly as it does on the cache after step 1 — and becomes the provider's associated
//! `Note` when the cache goes generic over its provider in step 2b.

use std::path::{Path, PathBuf};

use super::admit::{self, Denial, Origin, Presentation, Unavailable};
use super::lock::{self, Holder};
use super::shared::DurableStore;
use super::stream::MetaWriter;
use crate::engine::meta_stream::{MaterializedMeta, Resume, Versions};
use crate::engine::BlockStore;
use crate::Transcript;

/// Serde bounds every lock note carries — the note is persisted inside the entry's `LOCK`
/// file for a peer to read.
pub trait NoteBounds: serde::Serialize + serde::de::DeserializeOwned + Clone {}
impl<T: serde::Serialize + serde::de::DeserializeOwned + Clone> NoteBounds for T {}

/// Owned by the resident while it writes (#167 §4.4). Bundles the metadata stream and the
/// lock lease into one object: dropping it releases the entry — that is the entire release
/// mechanism. `lease` is `None` under [`SingleWriter`], whose root lock outlives any entry.
pub struct EntryWriter {
    pub(crate) meta: MetaWriter,
    _lease: Option<lock::Lease>,
}

/// The cache's half of the #109 witness, passed across the seam as plain data — never a
/// reference to the resident (§4.2): the byte length of the entry's backing as our
/// still-in-memory resident last wrote it, and how many committed blocks that was (the
/// provider's alignment math needs the count; `load_from` needs the bytes).
#[derive(Clone, Copy)]
pub struct Witness {
    pub backing_len: u64,
    pub committed: usize,
}

/// What [`Entries::open`] produced.
pub enum Opened<P: BlockStore, N> {
    /// The entry is ours again and the caller's resident still describes it exactly (#109):
    /// nothing was loaded, aligned or recovered. `writer` re-arms the resident (`None` when
    /// the stream could not be reopened — the caller must still thaw; the lock was already
    /// handed back).
    Retained { writer: Option<EntryWriter> },
    /// The entry is ours. The caller constructs and installs the session.
    Owned {
        /// Opened, positioned; already `reset` on a cold start.
        store: P,
        /// Blocks recovered from the entry. When `prefix_reused`, only the TAIL past the
        /// witness — the caller prepends its resident's already-decoded prefix.
        loaded: Vec<P::Bv>,
        prefix_reused: bool,
        /// Resumed / Retained / Cold(reason) — kept diagnosable.
        origin: Origin,
        /// A resume's restored state: the materialized meta, the resume point, and how many
        /// committed blocks the alignment corroborated (the caller truncates the joined
        /// prefix to it — the store side of that cut, `adopt`, already ran here).
        resumed: Option<Box<(MaterializedMeta, Resume, usize)>>,
        /// `None` = the stream could not be written; the session is served undurable and
        /// the lock was already handed back.
        writer: Option<EntryWriter>,
    },
    /// A live peer holds it (with its note), or no entry can exist here.
    Denied(Denial<N>),
}

/// Take exclusive ownership of a session's durable entry — or say who has it (#167 §4.2).
/// The provider owns everything filesystem-shaped; the cache computes its half of the
/// witness and hands one value across the seam.
pub trait Entries<P: BlockStore> {
    /// `make_store` is called at most once, with the entry's directory — only the caller
    /// knows the frontend's store and this session's fold context.
    fn open<N: NoteBounds>(
        &self,
        id: &str,
        src: &Transcript,
        ours: Option<Witness>,
        make_store: &mut dyn FnMut(&Path) -> std::io::Result<P>,
        alive: &dyn Fn(&Holder<N>) -> bool,
    ) -> Opened<P, N>;

    /// Publish a late-arriving fact into the entry's lock note (only the HTML port needs
    /// this). `false` = this process does not hold the entry.
    fn publish<N: NoteBounds>(&self, _id: &str, _note: N) -> bool {
        false
    }
}

/// The per-`<session, frontend>` provider (§4.3 b): entry lock, resume, redirect notes.
pub struct PerSession {
    pub(crate) presentation: Presentation,
    pub(crate) root: PathBuf,
    pub(crate) versions: Versions,
}

impl PerSession {
    pub fn new(root: PathBuf, presentation: Presentation, versions: Versions) -> Self {
        PerSession {
            presentation,
            root,
            versions,
        }
    }

    fn dir(&self, id: &str) -> PathBuf {
        admit::entry_dir(&self.root, self.presentation, id)
    }
}

/// Build the entry's writer, wrapping stream + lease into the RAII bundle. On failure the
/// lock is handed straight back (a session served undurable must not pin the entry).
fn armed_writer(
    dir: &Path,
    src: &Path,
    versions: Versions,
    how: admit::Rewind,
    lease: Option<lock::Lease>,
) -> Option<EntryWriter> {
    match admit::writer_for(dir, src, versions, how) {
        Ok(meta) => Some(EntryWriter {
            meta,
            _lease: lease,
        }),
        Err(_) => None, // dropping `lease` here releases the lock — RAII covers the error path
    }
}

impl<P: DurableStore> Entries<P> for PerSession {
    fn open<N: NoteBounds>(
        &self,
        id: &str,
        src: &Transcript,
        ours: Option<Witness>,
        make_store: &mut dyn FnMut(&Path) -> std::io::Result<P>,
        alive: &dyn Fn(&Holder<N>) -> bool,
    ) -> Opened<P, N> {
        // The store is opened INSIDE the claim, after the lock is ours — the ordering is
        // load-bearing (see `claim`'s docs). It comes back out through these slots.
        let mut store: Option<P> = None;
        let mut loaded: Vec<P::Bv> = Vec::new();
        let mut prefix_reused = false;
        let claimed = admit::claim::<N>(
            Some(&self.root),
            self.presentation,
            id,
            src.path(),
            self.versions.clone(),
            |dir| {
                let Ok(mut s) = make_store(dir) else {
                    return admit::Backing::Unusable;
                };
                let on_disk = s.backing_len();
                // THE WITNESS (#109). Our blocks describe this entry exactly when the
                // backing is still the length we left it at, the fold is ours, and the
                // source is the same file — nothing was written here since we let go.
                if ours.map(|w| w.backing_len) == Some(on_disk)
                    && admit::stream_unchanged(dir, src.path(), &self.versions)
                {
                    return admit::Backing::Retained;
                }
                // Otherwise rebuild — but a resident prefix the backing still BEGINS with
                // is already decoded, so read only from where it ends. `ours <= on_disk` is
                // what makes that sound: a shorter backing means a peer cut below us and
                // the prefix is no longer ours to trust.
                let reuse = ours.filter(|w| w.backing_len <= on_disk);
                let (tail, prefix) = match reuse {
                    Some(w) => match s.load_from(w.backing_len) {
                        Ok(tail) => (tail, Some(w.committed)),
                        // `at` is not a record boundary — a `put` whose write failed
                        // part-way left the store's length behind the file's. Reuse is off
                        // and the whole backing is read instead.
                        Err(_) => match s.load_from(0) {
                            Ok(all) => (all, None),
                            Err(_) => return admit::Backing::Unusable,
                        },
                    },
                    None => match s.load_from(0) {
                        Ok(all) => (all, None),
                        Err(_) => return admit::Backing::Unusable,
                    },
                };
                prefix_reused = prefix.is_some();
                loaded = tail;
                let committed_len = prefix.unwrap_or(0) + loaded.len();
                store = Some(s);
                admit::Backing::Committed(committed_len)
            },
            alive,
        );
        let (dir, origin, resumed) = match claimed {
            admit::Claim::Denied(x) => return Opened::Denied(x),
            admit::Claim::Retained { dir } => {
                let lease = lock::Lease::held(&dir);
                return Opened::Retained {
                    // Re-arm: drain-preserving reattach (`Rewind::All` — nothing is being
                    // re-authored; the fold only stopped writing).
                    writer: armed_writer(
                        &dir,
                        src.path(),
                        self.versions.clone(),
                        admit::Rewind::All,
                        Some(lease),
                    ),
                };
            }
            admit::Claim::Ours {
                dir,
                origin,
                resumed,
            } => (dir, origin, resumed),
        };
        let lease = lock::Lease::held(&dir);
        let mut store = store.expect("claim only returns Ours after the store callback ran");

        // I6, the store half: cut the content stream to what the records corroborate. (The
        // caller cuts its joined block vector to the same count.) A prefix that cannot be
        // adopted demotes the whole open to cold on the same entry.
        match resumed {
            Some(a) => {
                let a = *a;
                if store.adopt(a.committed, &a.meta.session_meta).is_err() {
                    store.reset();
                    return Opened::Owned {
                        store,
                        loaded: Vec::new(),
                        prefix_reused: false,
                        origin: Origin::Cold(super::admit::ColdReason::TornStream),
                        resumed: None,
                        writer: armed_writer(
                            &dir,
                            src.path(),
                            self.versions.clone(),
                            admit::Rewind::Fresh,
                            Some(lease),
                        ),
                    };
                }
                let keep_records = a.records;
                Opened::Owned {
                    store,
                    loaded,
                    prefix_reused,
                    origin,
                    resumed: Some(Box::new((a.meta, a.resume, a.committed))),
                    writer: armed_writer(
                        &dir,
                        src.path(),
                        self.versions.clone(),
                        admit::Rewind::Keep(keep_records),
                        Some(lease),
                    ),
                }
            }
            None => {
                store.reset(); // a rejected cache keeps nothing
                Opened::Owned {
                    store,
                    loaded: Vec::new(),
                    prefix_reused: false,
                    origin,
                    resumed: None,
                    writer: armed_writer(
                        &dir,
                        src.path(),
                        self.versions.clone(),
                        admit::Rewind::Fresh,
                        Some(lease),
                    ),
                }
            }
        }
    }

    /// Land the note iff this process still holds the entry's lock — the ownership check
    /// that used to need the `owned` map, answered by the lock file itself.
    fn publish<N: NoteBounds>(&self, id: &str, note: N) -> bool {
        let dir = self.dir(id);
        if !lock::held_by_us(&dir) {
            return false;
        }
        lock::publish(&dir, note).is_ok()
    }
}

/// The monitor's provider (§4.3 c): ONE root lock, taken at construction — a second monitor
/// was redirected before any cache existed — so `open` takes no per-entry locks and can
/// never deny. Adopted by the monitor in §8 step 3.
pub struct SingleWriter {
    pub(crate) presentation: Presentation,
    pub(crate) root: PathBuf,
    pub(crate) versions: Versions,
}

impl SingleWriter {
    /// Wrap a root this process has already claimed (the monitor's own root lock — its
    /// claim/redirect ceremony stays with the monitor, which owns the user-facing message).
    pub fn over_claimed_root(
        root: PathBuf,
        presentation: Presentation,
        versions: Versions,
    ) -> Self {
        SingleWriter {
            presentation,
            root,
            versions,
        }
    }
}

impl<P: DurableStore> Entries<P> for SingleWriter {
    fn open<N: NoteBounds>(
        &self,
        id: &str,
        src: &Transcript,
        ours: Option<Witness>,
        make_store: &mut dyn FnMut(&Path) -> std::io::Result<P>,
        _alive: &dyn Fn(&Holder<N>) -> bool,
    ) -> Opened<P, N> {
        let dir = admit::entry_dir(&self.root, self.presentation, id);
        if std::fs::create_dir_all(&dir).is_err() {
            return Opened::Denied(Denial::Unavailable(Unavailable::UnwritableRoot));
        }
        let Ok(mut s) = make_store(&dir) else {
            return Opened::Denied(Denial::Unavailable(Unavailable::UnwritableRoot));
        };
        let on_disk = s.backing_len();
        if ours.map(|w| w.backing_len) == Some(on_disk)
            && admit::stream_unchanged(&dir, src.path(), &self.versions)
        {
            return Opened::Retained {
                writer: armed_writer(
                    &dir,
                    src.path(),
                    self.versions.clone(),
                    admit::Rewind::All,
                    None, // the root lock covers every entry; there is no lease to hold
                ),
            };
        }
        let reuse = ours.filter(|w| w.backing_len <= on_disk);
        let (loaded, prefix) = match reuse {
            Some(w) => match s.load_from(w.backing_len) {
                Ok(tail) => (tail, Some(w.committed)),
                Err(_) => match s.load_from(0) {
                    Ok(all) => (all, None),
                    Err(_) => {
                        return Opened::Denied(Denial::Unavailable(Unavailable::UnwritableRoot))
                    }
                },
            },
            None => match s.load_from(0) {
                Ok(all) => (all, None),
                Err(_) => return Opened::Denied(Denial::Unavailable(Unavailable::UnwritableRoot)),
            },
        };
        let committed_len = prefix.unwrap_or(0) + loaded.len();
        let (origin, resumed) =
            match admit::recover(&dir, src.path(), &self.versions, committed_len) {
                Ok(Some(a)) => (
                    Origin::Resumed {
                        committed: a.committed,
                        replay_from: a.resume.replay_from,
                    },
                    Some(a),
                ),
                Ok(None) => (Origin::Cold(super::admit::ColdReason::TornStream), None),
                Err(r) => (Origin::Cold(r), None),
            };
        match resumed {
            Some(a) => {
                if s.adopt(a.committed, &a.meta.session_meta).is_err() {
                    s.reset();
                    return Opened::Owned {
                        store: s,
                        loaded: Vec::new(),
                        prefix_reused: false,
                        origin: Origin::Cold(super::admit::ColdReason::TornStream),
                        resumed: None,
                        writer: armed_writer(
                            &dir,
                            src.path(),
                            self.versions.clone(),
                            admit::Rewind::Fresh,
                            None,
                        ),
                    };
                }
                let keep = a.records;
                Opened::Owned {
                    store: s,
                    loaded,
                    prefix_reused: prefix.is_some(),
                    origin,
                    resumed: Some(Box::new((a.meta, a.resume, a.committed))),
                    writer: armed_writer(
                        &dir,
                        src.path(),
                        self.versions.clone(),
                        admit::Rewind::Keep(keep),
                        None,
                    ),
                }
            }
            None => {
                s.reset();
                Opened::Owned {
                    store: s,
                    loaded: Vec::new(),
                    prefix_reused: false,
                    origin,
                    resumed: None,
                    writer: armed_writer(
                        &dir,
                        src.path(),
                        self.versions.clone(),
                        admit::Rewind::Fresh,
                        None,
                    ),
                }
            }
        }
    }
}

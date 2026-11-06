//! A transcript's BYTES, whatever form its store keeps them in (#s383).
//!
//! Codex compresses a rollout nobody has written for seven days into `rollout-….jsonl.zst`
//! (zstd level 3, one frame with the decoded size in its header; the plain file is removed and
//! its mtime kept), and decompresses it back to the plain `.jsonl` when the thread is resumed,
//! removing the `.zst`. So a `.zst` never grows, and a plain file beside one is the one to read
//! (codex-rs `rollout/src/compression.rs`, read 2026-11-06, the feature already in codex-cli
//! 0.166.0 behind `local_thread_store_compression`). On a machine with it on, 3,416 of 4,502
//! sessions were `.zst` and every reader here skipped them.
//!
//! Every reader in this workspace works in byte offsets of the JSONL — a follower's cursor, an
//! elided span's offset, a fold's resume point — and Codex's own reader does too ("offsets
//! always address the original JSONL bytes"). So a compressed transcript is opened as its
//! DECODED bytes: [`open`] gives a [`Source`] that reads, seeks and reports the length of the
//! JSONL whichever form is on disk, [`stream`] reads one once from its start (decoding only as
//! far as it is read, so a sniff of the head stays cheap), and [`len`] is that length without a
//! reader. A path names the session in either form ([`resolve`]: a reader holding the plain path
//! of a rollout compressed since finds the `.zst`, and back), and [`Source::reads`] tells a
//! reader that keeps going when the file under its path is no longer the one it holds. Anything that
//! reads a transcript's CONTENT goes through here. Whatever only asks whether a file CHANGED may
//! keep reading the file's own metadata: a `.zst`'s length and mtime are as stable as the
//! plain file's.
//!
//! The decoded bytes go to an anonymous file in the temp directory (unlinked as it is opened,
//! as Codex's seekable reader does), so nothing is left behind and the store is never written;
//! where the temp directory is on disk (macOS's is), memory stays bounded however long the
//! session. The last few are kept open, so the probes a scan makes of one session in a row (its
//! tail, its title, its length) decode it once.
//!
//! A decode also keeps the file's first [`HEAD_KEEP`] and last [`TAIL_KEEP`] bytes (#s386): a
//! reader of the head (a scan's title, cwd, agent sniff and first event) or of the tail (its
//! activity clock, state pulse and in-flight tools, which read at most 256 KiB from the end) is
//! served from them without decoding the file again, and so is one after a restart,
//! where a process names a directory for them ([`keep_tails_in`]; the monitor does, under its
//! cache root). They are the file's own bytes, not anything derived from them, so a rule that
//! changes in an upgrade still reads them afresh; and a `.zst` never changes, so the version that
//! names them (path, compressed length, mtime) is their only invalidation. Measured on 1,000
//! synthetic compressed rollouts: the monitor's first list took 39 s on every start, decoding
//! each about twice.

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// The ending of a compressed transcript's file name.
pub const COMPRESSED_SUFFIX: &str = ".jsonl.zst";

/// How many decoded transcripts stay open for the next reader, and how many of their bytes: the
/// most recent always stays, however long.
const KEEP_DECODED: usize = 4;
const KEEP_BYTES: u64 = 512 * 1024 * 1024;

/// How much of a compressed transcript's end a decode keeps for the next reader of its tail:
/// the in-flight probe reads 256 KiB and a byte, the state pulse 64 KiB, the activity clock
/// 32 KiB; a read below it decodes the file as before.
pub const TAIL_KEEP: u64 = 320 * 1024;
/// And of its start: the scan's head readers (a title's first prompt, the cwd, the agent's
/// five-line sniff, the first event, a one-shot's brief) read the first lines, 64 KiB at most for
/// all but a first prompt that comes later than that, which decodes.
pub const HEAD_KEEP: u64 = 64 * 1024;
/// The kept ends' share of memory, and of the directory a process names, compressed there. A
/// record is 384 KiB of JSONL before compression; on synthetic rollouts the records averaged
/// 141 KB, so the 3,416 compressed rollouts one machine held need about 480 MB: a cap below what
/// a store needs would drop records every start and decode those rows again.
const TAILS_IN_MEMORY: u64 = 32 * 1024 * 1024;
const TAILS_ON_DISK: u64 = 1024 * 1024 * 1024;

/// Whether `path` names a zstd-compressed transcript (`*.jsonl.zst`).
pub fn is_compressed(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(COMPRESSED_SUFFIX))
}

/// The plain `.jsonl` path a compressed transcript stands for, and `path` itself otherwise.
/// It is Codex's own name for the rollout, the one its thread database keeps
/// (`threads.rollout_path`), and the name a resume brings back.
pub fn plain_path(path: &Path) -> PathBuf {
    if is_compressed(path) {
        path.with_extension("")
    } else {
        path.to_path_buf()
    }
}

/// A transcript file's stem as a session's identity: `rollout-X` for `rollout-X.jsonl` and for
/// `rollout-X.jsonl.zst` alike, so a session keeps one id as its file changes form;
/// [`Path::file_stem`] for every other name.
pub fn stem(path: &Path) -> Option<&str> {
    let name = path.file_name()?.to_str()?;
    match name.strip_suffix(COMPRESSED_SUFFIX) {
        Some(stem) if !stem.is_empty() => Some(stem),
        _ => Path::new(name).file_stem()?.to_str(),
    }
}

/// The other form of a transcript's name: `x.jsonl.zst` for `x.jsonl` and back, `None` for a
/// name that has no other form.
fn other_form(path: &Path) -> Option<PathBuf> {
    if is_compressed(path) {
        return Some(plain_path(path));
    }
    let name = path.file_name()?.to_str()?;
    name.ends_with(".jsonl")
        .then(|| path.with_file_name(format!("{name}.zst")))
}

/// The file on disk that holds `path`'s transcript: the plain file wherever it exists, as Codex's
/// own reader and discovery decide (a resume writes it back before it removes the `.zst`, then
/// appends to it); else the compressed one. So a path names the session in either form: a plain
/// path whose rollout was compressed since it was found reads the `.zst`, and a `.zst` whose
/// thread was resumed reads the plain file. `None` when neither exists.
pub fn resolve(path: &Path) -> Option<PathBuf> {
    let plain = plain_path(path);
    if plain.exists() {
        return Some(plain);
    }
    if path.exists() {
        return Some(path.to_path_buf());
    }
    other_form(path).filter(|p| p.exists())
}

/// A transcript opened for reading, in the offsets of its JSONL.
pub struct Source {
    inner: Inner,
}

enum Inner {
    Plain(File),
    Decoded {
        snap: Arc<Snapshot>,
        pos: u64,
    },
    /// A compressed transcript known only by its kept ends: reads there are served from them,
    /// and the first read between them decodes the file it was opened on (never the path again:
    /// a resume may have removed it, or put another version there) into a snapshot.
    Tail {
        tail: Arc<Tail>,
        file: Option<File>,
        pos: u64,
    },
}

/// The first and last bytes of one version of a compressed transcript, decoded.
struct Tail {
    key: Key,
    /// The JSONL's whole length.
    len: u64,
    /// Its first [`HEAD_KEEP`] bytes (all of it, for a short one).
    head: Vec<u8>,
    /// Where `bytes` start in it.
    start: u64,
    bytes: Vec<u8>,
}

impl Tail {
    fn held(&self) -> u64 {
        (self.head.len() + self.bytes.len()) as u64
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Inner::Plain(_) => f.write_str("Source::Plain"),
            Inner::Decoded { snap, pos } => write!(f, "Source::Decoded({pos}/{})", snap.len),
            Inner::Tail { tail, pos, .. } => write!(f, "Source::Tail({pos}/{})", tail.len),
        }
    }
}

/// One compressed transcript's decoded bytes, and the version of the file they came from.
struct Snapshot {
    file: File,
    len: u64,
    key: Key,
}

impl Source {
    /// The JSONL's length: the plain file's own, read afresh (it may be growing), or the decoded
    /// length of a compressed one.
    pub fn len(&self) -> io::Result<u64> {
        match &self.inner {
            Inner::Plain(f) => Ok(f.metadata()?.len()),
            Inner::Decoded { snap, .. } => Ok(snap.len),
            Inner::Tail { tail, .. } => Ok(tail.len),
        }
    }

    /// Whether the JSONL is empty.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Whether the bytes were decoded from a compressed file (which never grows).
    pub fn is_decoded(&self) -> bool {
        !matches!(self.inner, Inner::Plain(_))
    }

    /// Whether this reader still reads the transcript `path` names now. A plain file that has
    /// since been compressed still does (its bytes are the `.zst`'s, which never grows). A
    /// decoded `.zst` that was decompressed again (a resume, which may append), or a plain file
    /// that was removed and written anew, does not: a reader that keeps going must open again.
    /// `true` when neither form exists: what a missing transcript means is the caller's own
    /// length check to decide.
    pub fn reads(&self, path: &Path) -> bool {
        let Some(on_disk) = resolve(path) else {
            return true;
        };
        match &self.inner {
            Inner::Decoded { snap, .. } => Key::of(&on_disk).is_ok_and(|k| k == snap.key),
            Inner::Tail { tail, .. } => Key::of(&on_disk).is_ok_and(|k| k == tail.key),
            Inner::Plain(_) if is_compressed(&on_disk) => true,
            Inner::Plain(f) => same_file(f, &on_disk),
        }
    }
}

#[cfg(unix)]
fn same_file(held: &File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (held.metadata(), std::fs::metadata(path)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => true,
    }
}

#[cfg(not(unix))]
fn same_file(_held: &File, _path: &Path) -> bool {
    true
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut upgrade = None;
        if let Inner::Tail { tail, file, pos } = &mut self.inner {
            if *pos < tail.start && *pos >= tail.head.len() as u64 {
                // Between the kept ends: the whole file it was opened on, once.
                let snap = match kept(&tail.key) {
                    Some(snap) => snap,
                    None => {
                        let file = file
                            .take()
                            .ok_or_else(|| io::Error::other("the file is no longer held"))?;
                        snapshot_of(file, tail.key.clone())?
                    }
                };
                upgrade = Some((snap, *pos));
            }
        }
        if let Some((snap, pos)) = upgrade {
            self.inner = Inner::Decoded { snap, pos };
        }
        match &mut self.inner {
            Inner::Plain(f) => f.read(buf),
            Inner::Decoded { snap, pos } => {
                let n = read_at(&snap.file, buf, *pos)?;
                *pos += n as u64;
                Ok(n)
            }
            Inner::Tail { tail, pos, .. } => {
                let have = if *pos < tail.head.len() as u64 {
                    &tail.head[*pos as usize..]
                } else {
                    let at = usize::try_from(*pos - tail.start).unwrap_or(usize::MAX);
                    tail.bytes.get(at..).unwrap_or(&[])
                };
                let n = have.len().min(buf.len());
                buf[..n].copy_from_slice(&have[..n]);
                *pos += n as u64;
                Ok(n)
            }
        }
    }
}

impl Seek for Source {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        match &mut self.inner {
            Inner::Plain(f) => f.seek(to),
            Inner::Decoded { snap, pos } => {
                let next = match to {
                    SeekFrom::Start(n) => Some(n),
                    SeekFrom::End(d) => snap.len.checked_add_signed(d),
                    SeekFrom::Current(d) => pos.checked_add_signed(d),
                };
                let next = next.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")
                })?;
                *pos = next;
                Ok(next)
            }
            Inner::Tail { tail, pos, .. } => {
                let next = match to {
                    SeekFrom::Start(n) => Some(n),
                    SeekFrom::End(d) => tail.len.checked_add_signed(d),
                    SeekFrom::Current(d) => pos.checked_add_signed(d),
                };
                let next = next.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "seek before the start")
                })?;
                *pos = next;
                Ok(next)
            }
        }
    }
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> io::Result<usize> {
    std::os::unix::fs::FileExt::read_at(file, buf, pos)
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], pos: u64) -> io::Result<usize> {
    std::os::windows::fs::FileExt::seek_read(file, buf, pos)
}

/// Opens `path`'s transcript (see [`resolve`]) as its JSONL bytes. A plain file is the file
/// itself; a compressed one is decoded whole before this returns, once per version of it while
/// it is among the last few opened, unless its tail is kept: then it is decoded only when a read
/// goes below the tail.
pub fn open(path: &Path) -> io::Result<Source> {
    let Some(on_disk) = resolve(path) else {
        // The plain open's own error, as every caller already expects of a missing file.
        return File::open(path).map(|f| Source {
            inner: Inner::Plain(f),
        });
    };
    if !is_compressed(&on_disk) {
        return Ok(Source {
            inner: Inner::Plain(File::open(&on_disk)?),
        });
    }
    let file = File::open(&on_disk)?;
    let key = Key::of_file(&on_disk, &file)?;
    if kept(&key).is_none() {
        if let Some(tail) = kept_tail(&key) {
            return Ok(Source {
                inner: Inner::Tail {
                    tail,
                    file: Some(file),
                    pos: 0,
                },
            });
        }
    }
    Ok(Source {
        inner: Inner::Decoded {
            snap: snapshot_of(file, key)?,
            pos: 0,
        },
    })
}

/// The length of `path`'s JSONL (see [`resolve`]): a plain file's own length, or a compressed
/// one's decoded length, remembered per version of the file: read from its frame header where
/// that can be proved, or from its kept tail; else counted by decoding it into the kept
/// snapshots, since a reader that asks the length almost always opens it next.
pub fn len(path: &Path) -> io::Result<u64> {
    let on_disk = resolve(path).unwrap_or_else(|| path.to_path_buf());
    if !is_compressed(&on_disk) {
        return Ok(std::fs::metadata(&on_disk)?.len());
    }
    let key = Key::of(&on_disk)?;
    if let Some(len) = lengths().lock().ok().and_then(|m| m.get(&key).copied()) {
        return Ok(len);
    }
    // #s386: Codex pledges the decoded size in the frame header, so the length is read there
    // whenever the file can be proved to hold that one frame and nothing else; only a file that
    // states no size, or holds more, is decoded to count it.
    if let Some(len) = declared_len(&on_disk)? {
        remember_len(key, len);
        return Ok(len);
    }
    if let Some(tail) = kept_tail(&key) {
        remember_len(key, tail.len);
        return Ok(tail.len);
    }
    Ok(snapshot(&on_disk)?.len)
}

/// The decoded length a zstd file states without being decoded: one frame whose header gives
/// its content size, its blocks walked by their 3-byte headers (each body skipped, never read)
/// to end exactly at the end of the file. `None` for a file that states no size, holds another
/// frame (a skippable one too), or does not parse: the caller decodes it. A few hundred small
/// reads for a 40 MB rollout (blocks are at most 128 KiB). A size the encoder pledged is one
/// libzstd held it to; a snapshot that decodes to another length replaces it.
fn declared_len(path: &Path) -> io::Result<Option<u64>> {
    const MAGIC: u32 = 0xFD2F_B528;
    let f = File::open(path)?;
    let file_len = f.metadata()?.len();
    let mut head = [0u8; 18]; // magic 4, descriptor 1, window 1, dictionary ≤ 4, size ≤ 8
    let mut n = 0;
    while n < head.len() {
        match read_at(&f, &mut head[n..], n as u64)? {
            0 => break,
            k => n += k,
        }
    }
    if n < 5 || u32::from_le_bytes([head[0], head[1], head[2], head[3]]) != MAGIC {
        return Ok(None);
    }
    let descriptor = head[4];
    let single_segment = descriptor & 0x20 != 0;
    let checksum = descriptor & 0x04 != 0;
    if descriptor & 0x08 != 0 {
        return Ok(None); // the reserved bit: not a frame this reader knows
    }
    let mut at = 5 + usize::from(!single_segment) + [0, 1, 2, 4][usize::from(descriptor & 3)];
    let size_bytes = match descriptor >> 6 {
        0 if single_segment => 1,
        0 => return Ok(None), // no size stated
        1 => 2,
        2 => 4,
        _ => 8,
    };
    if at + size_bytes > n {
        return Ok(None);
    }
    let mut size = 0u64;
    for (i, b) in head[at..at + size_bytes].iter().enumerate() {
        size |= u64::from(*b) << (8 * i);
    }
    if size_bytes == 2 {
        size += 256; // the format's offset for a two-byte size
    }
    at += size_bytes;
    let mut pos = at as u64;
    loop {
        let mut block = [0u8; 3];
        let mut got = 0;
        while got < 3 {
            match read_at(&f, &mut block[got..], pos + got as u64)? {
                0 => return Ok(None),
                k => got += k,
            }
        }
        let header = u32::from(block[0]) | u32::from(block[1]) << 8 | u32::from(block[2]) << 16;
        let body = match (header >> 1) & 3 {
            0 | 2 => u64::from(header >> 3), // raw, compressed: the size is the body's
            1 => 1,                          // RLE: one byte, repeated
            _ => return Ok(None),            // reserved
        };
        pos += 3 + body;
        if pos > file_len {
            return Ok(None);
        }
        if header & 1 != 0 {
            break; // the last block
        }
    }
    if checksum {
        pos += 4;
    }
    Ok((pos == file_len).then_some(size))
}

/// A compressed file's identity for the caches: a new version of it is a new key.
#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    len: u64,
    mtime: Option<SystemTime>,
}

impl Key {
    fn of(path: &Path) -> io::Result<Self> {
        Self::from_meta(path, &std::fs::metadata(path)?)
    }

    /// The version a handle holds, which the path may no longer name.
    fn of_file(path: &Path, file: &File) -> io::Result<Self> {
        Self::from_meta(path, &file.metadata()?)
    }

    fn from_meta(path: &Path, meta: &std::fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            path: path.to_path_buf(),
            len: meta.len(),
            mtime: meta.modified().ok(),
        })
    }
}

type Kept = Mutex<Vec<(Key, Arc<Snapshot>)>>;

fn kept_snapshots() -> &'static Kept {
    static KEPT: OnceLock<Kept> = OnceLock::new();
    KEPT.get_or_init(|| Mutex::new(Vec::new()))
}

fn lengths() -> &'static Mutex<std::collections::HashMap<Key, u64>> {
    static LENGTHS: OnceLock<Mutex<std::collections::HashMap<Key, u64>>> = OnceLock::new();
    LENGTHS.get_or_init(Default::default)
}

fn remember_len(key: Key, len: u64) {
    if let Ok(mut m) = lengths().lock() {
        // One number per compressed file seen, and an older version of a file is dropped.
        m.retain(|k, _| k.path != key.path);
        m.insert(key, len);
    }
}

fn kept(key: &Key) -> Option<Arc<Snapshot>> {
    let mut kept = kept_snapshots().lock().ok()?;
    let at = kept.iter().position(|(k, _)| k == key)?;
    let entry = kept.remove(at);
    let snap = entry.1.clone();
    kept.push(entry); // the most recently used goes last
    Some(snap)
}

fn snapshot(path: &Path) -> io::Result<Arc<Snapshot>> {
    let source = File::open(path)?;
    let key = Key::of_file(path, &source)?;
    snapshot_of(source, key)
}

/// The snapshot of the version `key` names, decoded from `source`, a handle on that version.
fn snapshot_of(source: File, key: Key) -> io::Result<Arc<Snapshot>> {
    if let Some(snap) = kept(&key) {
        return Ok(snap);
    }
    #[cfg(test)]
    tests::DECODES.with(|d| d.set(d.get() + 1));
    let mut file = anonymous_file()?;
    let len = {
        let mut out = io::BufWriter::new(&mut file);
        let len = decode(&mut BufReader::new(source), &mut out)?;
        out.flush()?;
        len
    };
    let snap = Arc::new(Snapshot {
        file,
        len,
        key: key.clone(),
    });
    keep_tail(&snap);
    remember_len(key.clone(), len);
    if let Ok(mut kept) = kept_snapshots().lock() {
        kept.retain(|(k, _)| k.path != key.path);
        kept.push((key, snap.clone()));
        let held = |kept: &Vec<(Key, Arc<Snapshot>)>| kept.iter().map(|(_, s)| s.len).sum::<u64>();
        while kept.len() > KEEP_DECODED || (kept.len() > 1 && held(&kept) > KEEP_BYTES) {
            kept.remove(0);
        }
    }
    Ok(snap)
}

// ── Kept tails (#s386) ──────────────────────────────────────────────────────────────────────────

type KeptTails = Mutex<Vec<Arc<Tail>>>;

fn kept_tails() -> &'static KeptTails {
    static KEPT: OnceLock<KeptTails> = OnceLock::new();
    KEPT.get_or_init(Default::default)
}

fn tails_dir() -> &'static Mutex<Option<PathBuf>> {
    static DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
    DIR.get_or_init(Default::default)
}

/// Names the directory where this process keeps compressed transcripts' tails across restarts
/// (`None`, the default: in memory only), and sweeps it: a record whose transcript is gone in
/// both forms goes, and then the oldest while the rest come to more than 1 GiB. The
/// monitor names one under its cache root, since it reads the tail of every session it lists on
/// every start; nothing else needs to.
pub fn keep_tails_in(dir: Option<PathBuf>) {
    if let Some(dir) = &dir {
        sweep_tails(dir);
    }
    if let Ok(mut named) = tails_dir().lock() {
        *named = dir;
    }
}

/// Whether this process names a directory for its records (see [`keep_tails_in`]).
fn keeps_records() -> bool {
    tails_dir().lock().is_ok_and(|d| d.is_some())
}

fn remember_tail(tail: Arc<Tail>) {
    let Ok(mut kept) = kept_tails().lock() else {
        return;
    };
    kept.retain(|t| t.key.path != tail.key.path);
    kept.push(tail);
    let mut held: u64 = kept.iter().map(|t| t.held()).sum();
    while kept.len() > 1 && held > TAILS_IN_MEMORY {
        held -= kept[0].held();
        kept.remove(0);
    }
}

/// The kept tail of this version of a compressed transcript: in memory, else in the named
/// directory. `None` when no decode kept one, or the one kept is of another version.
fn kept_tail(key: &Key) -> Option<Arc<Tail>> {
    if let Ok(mut kept) = kept_tails().lock() {
        if let Some(at) = kept.iter().position(|t| &t.key == key) {
            let tail = kept.remove(at);
            kept.push(tail.clone());
            return Some(tail);
        }
    }
    let dir = tails_dir().lock().ok()?.clone()?;
    let tail = Arc::new(read_tail_record(&dir, key)?);
    remember_tail(tail.clone());
    Some(tail)
}

/// Keeps a fresh snapshot's first [`HEAD_KEEP`] and last [`TAIL_KEEP`] bytes, in memory and in
/// the named directory.
fn keep_tail(snap: &Snapshot) {
    let read = |from: u64, upto: u64| -> Option<Vec<u8>> {
        let mut bytes = vec![0u8; (upto - from) as usize];
        let mut n = 0;
        while n < bytes.len() {
            match read_at(&snap.file, &mut bytes[n..], from + n as u64) {
                Ok(0) | Err(_) => return None,
                Ok(k) => n += k,
            }
        }
        Some(bytes)
    };
    let start = snap.len.saturating_sub(TAIL_KEEP);
    let (Some(head), Some(bytes)) = (read(0, snap.len.min(HEAD_KEEP)), read(start, snap.len))
    else {
        return;
    };
    let tail = Arc::new(Tail {
        key: snap.key.clone(),
        len: snap.len,
        head,
        start,
        bytes,
    });
    if let Some(dir) = tails_dir().lock().ok().and_then(|d| d.clone()) {
        let _ = write_tail_record(&dir, &tail);
    }
    remember_tail(tail);
}

/// A record's file: one per transcript path (a new version replaces the old), named by a hash
/// of the path, which the record also holds and is checked against.
fn record_path(dir: &Path, transcript: &Path) -> PathBuf {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a
    for b in transcript.as_os_str().as_encoded_bytes() {
        h = (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3);
    }
    dir.join(format!("{h:016x}.tail"))
}

fn mtime_nanos(mtime: Option<SystemTime>) -> Option<String> {
    let since = mtime?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    Some(since.as_nanos().to_string())
}

/// What a record says it holds, on its first line; the compressed tail follows.
#[derive(serde::Serialize, serde::Deserialize)]
struct TailHead {
    v: u8,
    path: PathBuf,
    compressed_len: u64,
    mtime_ns: String,
    len: u64,
    /// The kept head's length; the compressed blob is the head and then the tail.
    head_len: u64,
    start: u64,
}

const TAIL_RECORD_V: u8 = 1;

fn write_tail_record(dir: &Path, tail: &Tail) -> io::Result<()> {
    let Some(mtime_ns) = mtime_nanos(tail.key.mtime) else {
        return Ok(());
    };
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)?;
    let head = TailHead {
        v: TAIL_RECORD_V,
        path: tail.key.path.clone(),
        compressed_len: tail.key.len,
        mtime_ns,
        len: tail.len,
        head_len: tail.head.len() as u64,
        start: tail.start,
    };
    let mut out = serde_json::to_vec(&head).map_err(io::Error::other)?;
    out.push(b'\n');
    out.extend(ruzstd::encoding::compress_to_vec(
        tail.head.chain(&tail.bytes[..]),
        ruzstd::encoding::CompressionLevel::Fastest,
    ));
    let target = record_path(dir, &tail.key.path);
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let temp = dir.join(format!(
        ".{}.{}-{}.tmp",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("tail"),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let written = opts.open(&temp).and_then(|mut f| f.write_all(&out));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
        return written;
    }
    std::fs::rename(&temp, &target).inspect_err(|_| {
        let _ = std::fs::remove_file(&temp);
    })
}

fn read_tail_record(dir: &Path, key: &Key) -> Option<Tail> {
    let record = record_path(dir, &key.path);
    let raw = std::fs::read(&record).ok()?;
    let line = raw.iter().position(|&b| b == b'\n')?;
    let head: TailHead = serde_json::from_slice(&raw[..line]).ok()?;
    let fits = head.v == TAIL_RECORD_V
        && head.path == key.path
        && head.compressed_len == key.len
        && Some(&head.mtime_ns) == mtime_nanos(key.mtime).as_ref()
        && head.start <= head.len
        && head.len - head.start <= TAIL_KEEP
        && head.head_len == head.len.min(HEAD_KEEP);
    if !fits {
        return None;
    }
    let head_len = head.head_len as usize;
    let mut both = unpack(
        &raw[line + 1..],
        head_len + (head.len - head.start) as usize,
    )?;
    let bytes = both.split_off(head_len);
    // Read now: the sweep keeps the records in use, oldest-read going first.
    let _ = File::options()
        .write(true)
        .open(&record)
        .and_then(|f| f.set_modified(SystemTime::now()));
    Some(Tail {
        key: key.clone(),
        len: head.len,
        head: both,
        start: head.start,
        bytes,
    })
}

/// A record's compressed tail, through one decoder per thread: a restart reads thousands.
fn unpack(packed: &[u8], raw_len: usize) -> Option<Vec<u8>> {
    thread_local! {
        static DECODER: std::cell::RefCell<ruzstd::decoding::FrameDecoder> =
            std::cell::RefCell::new(ruzstd::decoding::FrameDecoder::new());
    }
    DECODER.with(|decoder| {
        let mut out = Vec::with_capacity(raw_len);
        decoder
            .borrow_mut()
            .decode_all_to_vec(packed, &mut out)
            .ok()?;
        (out.len() == raw_len).then_some(out)
    })
}

/// The directory's records whose transcript is gone in both forms go, then the oldest while the
/// rest are over [`TAILS_ON_DISK`]; a leftover temporary goes too. Each record's first line only
/// is read.
fn sweep_tails(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut kept = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".tmp") {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if !name.ends_with(".tail") {
            continue;
        }
        let mut first = Vec::new();
        let head = File::open(&path).ok().and_then(|f| {
            let n = f.take(64 * 1024).read_to_end(&mut first).ok()?;
            let line = first[..n].iter().position(|&b| b == b'\n')?;
            serde_json::from_slice::<TailHead>(&first[..line]).ok()
        });
        match head {
            Some(head) if resolve(&head.path).is_some() => {
                if let Ok(meta) = entry.metadata() {
                    kept.push((meta.modified().ok(), meta.len(), path));
                }
            }
            _ => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    kept.sort_by_key(|k| k.0);
    let mut total: u64 = kept.iter().map(|k| k.1).sum();
    for (_, size, path) in kept {
        if total <= TAILS_ON_DISK {
            break;
        }
        if std::fs::remove_file(&path).is_ok() {
            total -= size;
        }
    }
}

/// A file only this process can reach: created under a fresh name in the temp directory and
/// unlinked at once, so it goes when the last handle closes, even after a crash.
fn anonymous_file() -> io::Result<File> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir();
    let mut last = None;
    for _ in 0..16 {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let name = dir.join(format!(
            ".cr-decoded-{}-{}-{nanos}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true).write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        match opts.open(&name) {
            Ok(f) => {
                let _ = std::fs::remove_file(&name);
                return Ok(f);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::other("no fresh temp name")))
}

/// Decodes every zstd frame in `input` into `out`, returning the decoded length.
fn decode(input: &mut impl BufRead, out: &mut impl Write) -> io::Result<u64> {
    io::copy(&mut Frames::new(input), out)
}

/// The JSONL of a zstd stream, decoded only as far as it is read: every frame in turn, a
/// skippable one skipped, as the format allows (Codex writes one frame; several are still the
/// whole file). Input that holds no frame at all, or ends inside one, is an error, never an
/// empty transcript.
struct Frames<R: BufRead> {
    input: R,
    frame: ruzstd::decoding::FrameDecoder,
    in_frame: bool,
    seen_frame: bool,
    done: bool,
}

impl<R: BufRead> Frames<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            frame: ruzstd::decoding::FrameDecoder::new(),
            in_frame: false,
            seen_frame: false,
            done: false,
        }
    }
}

fn invalid(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl<R: BufRead> Read for Frames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        use ruzstd::decoding::errors::{FrameDecoderError, ReadFrameHeaderError};
        use ruzstd::decoding::BlockDecodingStrategy;
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.done {
                return Ok(0);
            }
            if !self.in_frame {
                if self.input.fill_buf()?.is_empty() {
                    if !self.seen_frame {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "the file holds no zstd frame",
                        ));
                    }
                    self.done = true;
                    return Ok(0);
                }
                match self.frame.init(&mut self.input) {
                    Ok(()) => {
                        self.in_frame = true;
                        self.seen_frame = true;
                    }
                    Err(FrameDecoderError::ReadFrameHeaderError(
                        ReadFrameHeaderError::SkipFrame { length, .. },
                    )) => {
                        let want = u64::from(length);
                        let skipped = io::copy(&mut (&mut self.input).take(want), &mut io::sink())?;
                        if skipped != want {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "a skippable zstd frame ends early",
                            ));
                        }
                        continue;
                    }
                    Err(e) => return Err(invalid(e)),
                }
            }
            while self.frame.can_collect() == 0 && !self.frame.is_finished() {
                self.frame
                    .decode_blocks(&mut self.input, BlockDecodingStrategy::UptoBlocks(1))
                    .map_err(invalid)?;
            }
            let n = self.frame.read(buf)?;
            if n > 0 {
                return Ok(n);
            }
            if self.frame.is_finished() && self.frame.can_collect() == 0 {
                self.in_frame = false; // the next frame, or the end
            }
        }
    }
}

/// A transcript opened to be read once from its start, as its JSONL. A compressed one is
/// decoded as it is read, so a sniff of its first lines decodes only those; [`open`] is for a
/// reader that seeks.
pub struct Stream {
    inner: StreamInner,
}

enum StreamInner {
    Plain(File),
    Zstd(Box<Frames<BufReader<File>>>),
    /// A compressed transcript whose head is kept: read from it, and decoded from the start
    /// (the kept bytes skipped) only once a read goes past it.
    Kept {
        ends: Arc<Tail>,
        file: Option<File>,
        pos: u64,
    },
    /// A compressed transcript decoded whole: read from its snapshot.
    Snap {
        snap: Arc<Snapshot>,
        pos: u64,
    },
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if let StreamInner::Kept { ends, file, pos } = &mut self.inner {
            let head = &ends.head;
            if *pos < head.len() as u64 {
                let n = (head.len() - *pos as usize).min(buf.len());
                buf[..n].copy_from_slice(&head[*pos as usize..*pos as usize + n]);
                *pos += n as u64;
                return Ok(n);
            }
            if *pos >= ends.len {
                return Ok(0);
            }
            // Past the kept head: the snapshot while it is kept, else decode from the start, the
            // bytes already read skipped.
            if let Some(snap) = kept(&ends.key) {
                let pos = *pos;
                self.inner = StreamInner::Snap { snap, pos };
                return self.read(buf);
            }
            #[cfg(test)]
            tests::STREAM_DECODES.with(|d| d.set(d.get() + 1));
            let held = file
                .take()
                .ok_or_else(|| io::Error::other("the file is no longer held"))?;
            let mut frames = Frames::new(BufReader::new(held));
            let skip = *pos;
            if io::copy(&mut (&mut frames).take(skip), &mut io::sink())? != skip {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the file is shorter than its kept head",
                ));
            }
            self.inner = StreamInner::Zstd(Box::new(frames));
        }
        match &mut self.inner {
            StreamInner::Plain(f) => f.read(buf),
            StreamInner::Zstd(z) => z.read(buf),
            StreamInner::Snap { snap, pos } => {
                let n = read_at(&snap.file, buf, *pos)?;
                *pos += n as u64;
                Ok(n)
            }
            StreamInner::Kept { .. } => unreachable!("moved to a decoder above"),
        }
    }
}

/// Opens `path`'s transcript (see [`resolve`]) to read once from its start.
pub fn stream(path: &Path) -> io::Result<Stream> {
    let on_disk = resolve(path).unwrap_or_else(|| path.to_path_buf());
    let file = File::open(&on_disk)?;
    if !is_compressed(&on_disk) {
        return Ok(Stream {
            inner: StreamInner::Plain(file),
        });
    }
    // #s386: a kept head serves the first lines, which is all most readers of a stream want.
    let mut file = file;
    let key = Key::of_file(&on_disk, &file)?;
    if let Some(snap) = kept(&key) {
        return Ok(Stream {
            inner: StreamInner::Snap { snap, pos: 0 },
        });
    }
    if let Some(ends) = kept_tail(&key) {
        return Ok(Stream {
            inner: StreamInner::Kept {
                ends,
                file: Some(file),
                pos: 0,
            },
        });
    }
    // A process that keeps records (the monitor) reads both ends of every row it lists, so its
    // first read of either end decodes the file once and keeps both: the head readers that come
    // before a row's tail probe would each decode the first block again. Anywhere else a stream
    // stays lazy, since a sniff of thousands of heads (a picker, a sweep) reads no further.
    // A file that does not decode whole (a corrupt later block, a temp directory that is full)
    // still gives the head it can, as a lazy stream does.
    if keeps_records() {
        match file
            .try_clone()
            .and_then(|clone| snapshot_of(clone, key.clone()))
        {
            Ok(snap) => {
                return Ok(Stream {
                    inner: StreamInner::Snap { snap, pos: 0 },
                })
            }
            Err(_) => {
                // The clone shares the handle's offset: back to the start.
                file.seek(SeekFrom::Start(0))?;
            }
        }
    }
    #[cfg(test)]
    tests::STREAM_DECODES.with(|d| d.set(d.get() + 1));
    Ok(Stream {
        inner: StreamInner::Zstd(Box::new(Frames::new(BufReader::new(file)))),
    })
}

/// `bytes` as one zstd frame, for a test or fixture that needs a compressed transcript in any
/// crate (`#[doc(hidden)]` rather than `cfg(test)`, which a downstream crate's tests cannot
/// see). The same crate's encoder, so no test needs a C toolchain.
#[doc(hidden)]
pub fn compress_for_tests(bytes: &[u8]) -> Vec<u8> {
    ruzstd::encoding::compress_to_vec(bytes, ruzstd::encoding::CompressionLevel::Fastest)
}

/// Replaces the plain transcript at `plain` with `<plain>.zst`, keeping its mtime, as Codex's
/// compression does; returns the new path. For tests (see [`compress_for_tests`]).
#[doc(hidden)]
pub fn compress_in_place_for_tests(plain: &Path) -> io::Result<PathBuf> {
    let bytes = std::fs::read(plain)?;
    let mtime = std::fs::metadata(plain)?.modified()?;
    let z = plain.with_file_name(format!(
        "{}.zst",
        plain
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("transcript.jsonl")
    ));
    std::fs::write(&z, compress_for_tests(&bytes))?;
    File::options().write(true).open(&z)?.set_modified(mtime)?;
    std::fs::remove_file(plain)?;
    Ok(z)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    thread_local! {
        /// Whole-file decodes this thread made (#s386): what a length read must not do.
        pub(crate) static DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
        /// Streams this thread decoded, from the start or past a kept head (#s386).
        pub(crate) static STREAM_DECODES: std::cell::Cell<usize> =
            const { std::cell::Cell::new(0) };
    }

    /// The source cases run one at a time: naming a directory for records changes what every
    /// stream in the process does.
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        static ONE: Mutex<()> = Mutex::new(());
        ONE.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn decodes() -> usize {
        DECODES.with(|d| d.get())
    }

    /// What a restart forgets: every memo this process keeps in memory.
    fn forget_in_memory() {
        kept_snapshots().lock().unwrap().clear();
        lengths().lock().unwrap().clear();
        kept_tails().lock().unwrap().clear();
    }

    /// A decode keeps the file's tail (#s386), so a later read there, even after a restart,
    /// decodes nothing; a read below it decodes once; a new version of the file is not served
    /// the old one's tail; and a record whose transcript is gone is swept.
    #[test]
    fn a_kept_tail_serves_tail_reads_across_a_restart() {
        let _one = serial();
        let dir = scratch("tails");
        let store = dir.join("tails");
        // The directory is the process's: unnamed again however this case ends, or a failure
        // here would change what every later case's streams do.
        struct Unname;
        impl Drop for Unname {
            fn drop(&mut self) {
                keep_tails_in(None);
            }
        }
        let _unname = Unname;
        keep_tails_in(Some(store.clone()));
        let body: String = (0..20_000)
            .map(|i| format!("{{\"n\":{i},\"pad\":\"{}\"}}\n", "x".repeat(i % 50)))
            .collect();
        let total = body.len() as u64;
        assert!(total > 2 * TAIL_KEEP, "{total}");
        let z = dir.join("big.jsonl.zst");
        std::fs::write(&z, compress(body.as_bytes())).unwrap();
        let tail_of = |n: u64| {
            let mut src = open(&z).unwrap();
            src.seek(SeekFrom::Start(total - n)).unwrap();
            let mut out = Vec::new();
            src.read_to_end(&mut out).unwrap();
            out
        };
        let at = decodes();
        assert_eq!(
            tail_of(64 * 1024),
            &body.as_bytes()[(total - 64 * 1024) as usize..]
        );
        assert_eq!(decodes() - at, 1, "first sight decodes once");
        // (Other tests decode beside this one while the directory is named: look at this record.)
        assert!(record_path(&store, &z).is_file(), "its record");

        forget_in_memory(); // a restart: the memos go, the directory stays
        let probe = 256 * 1024 + 1;
        assert_eq!(tail_of(probe), &body.as_bytes()[(total - probe) as usize..]);
        assert_eq!(len(&z).unwrap(), total);
        assert_eq!(
            decodes() - at,
            1,
            "after the restart, no decode for the tail"
        );

        let mut whole = String::new();
        open(&z).unwrap().read_to_string(&mut whole).unwrap();
        assert_eq!(whole, body);
        assert_eq!(decodes() - at, 2, "a read below the tail decodes, once");

        // Its head too: a stream reads its first lines from the record, and decodes only to go
        // past it, from the start, the bytes already read skipped.
        forget_in_memory();
        let streams = STREAM_DECODES.with(|d| d.get());
        let mut first = String::new();
        let mut reader = BufReader::new(stream(&z).unwrap());
        reader.read_line(&mut first).unwrap();
        assert_eq!(first, body.lines().next().unwrap().to_string() + "\n");
        assert_eq!(
            STREAM_DECODES.with(|d| d.get()),
            streams,
            "the head, from the record"
        );
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        assert_eq!(first + &rest, body);
        assert_eq!(
            STREAM_DECODES.with(|d| d.get()),
            streams + 1,
            "past it: decoded once"
        );
        assert_eq!(
            decodes() - at,
            2,
            "no whole-file decode for a stream with its record"
        );
        // With no record in a process that keeps them, a stream's first read decodes the whole
        // file once and keeps both ends; without the directory it decodes only what it reads.
        forget_in_memory();
        let _ = std::fs::remove_file(record_path(&store, &z));
        let mut first = String::new();
        BufReader::new(stream(&z).unwrap())
            .read_line(&mut first)
            .unwrap();
        assert_eq!(decodes() - at, 3, "decoded whole, once");
        assert!(record_path(&store, &z).is_file(), "both ends kept");
        // And a Source's reads in the head, after a restart: from the record.
        forget_in_memory();
        let mut src = open(&z).unwrap();
        let mut head = vec![0u8; 100];
        src.read_exact(&mut head).unwrap();
        assert_eq!(&head[..], &body.as_bytes()[..100]);
        assert_eq!(decodes() - at, 3, "the head, from the record");

        // A reader holds the version it opened: when a resume removes the .zst and a new version
        // stands at the path, a read between the kept ends decodes the one it opened, not the
        // new one, and so does a stream going past its kept head.
        forget_in_memory();
        let mut held = open(&z).unwrap();
        let mut held_stream = stream(&z).unwrap();
        let mut skip = vec![0u8; HEAD_KEEP as usize];
        held_stream.read_exact(&mut skip).unwrap();
        std::fs::remove_file(&z).unwrap();
        let other: String = body.replace("pad", "PAD");
        std::fs::write(&z, compress(other.as_bytes())).unwrap();
        let middle = total / 2;
        held.seek(SeekFrom::Start(middle)).unwrap();
        let mut got = vec![0u8; 64];
        held.read_exact(&mut got).unwrap();
        assert_eq!(
            &got[..],
            &body.as_bytes()[middle as usize..middle as usize + 64]
        );
        let mut rest = String::new();
        held_stream.read_to_string(&mut rest).unwrap();
        assert_eq!(rest, &body[HEAD_KEEP as usize..], "the version it opened");
        std::fs::write(&z, compress(body.as_bytes())).unwrap();
        let at = decodes(); // the held reader decoded its own version: count from here

        // A new version of the file: the old version's tail is not served.
        forget_in_memory();
        let longer = format!("{body}{{\"n\":-1}}\n");
        std::fs::write(&z, compress(longer.as_bytes())).unwrap();
        let mut src = open(&z).unwrap();
        src.seek(SeekFrom::End(-9)).unwrap();
        let mut last = Vec::new();
        src.read_to_end(&mut last).unwrap();
        assert_eq!(last, b"{\"n\":-1}\n");
        assert_eq!(decodes() - at, 1, "a new version decodes, once");

        // Gone in both forms: its record is swept as the directory is named again.
        std::fs::remove_file(&z).unwrap();
        keep_tails_in(Some(store.clone()));
        assert!(!record_path(&store, &z).exists(), "swept");
        keep_tails_in(None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn compress(bytes: &[u8]) -> Vec<u8> {
        compress_for_tests(bytes)
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cr-source-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const JSONL: &str = "{\"type\":\"session_meta\",\"n\":1}\n{\"type\":\"event_msg\",\"n\":2}\n{\"type\":\"response_item\",\"n\":3}\n";

    #[test]
    fn a_compressed_transcript_reads_seeks_and_measures_as_its_jsonl() {
        let _one = serial();
        let dir = scratch("reads");
        let z = dir.join("rollout-2026-10-01T00-00-00-abc.jsonl.zst");
        std::fs::write(&z, compress(JSONL.as_bytes())).unwrap();

        let mut src = open(&z).unwrap();
        assert!(src.is_decoded());
        assert_eq!(src.len().unwrap(), JSONL.len() as u64);
        assert_eq!(len(&z).unwrap(), JSONL.len() as u64);
        let mut all = String::new();
        src.read_to_string(&mut all).unwrap();
        assert_eq!(all, JSONL);

        // Offsets are the JSONL's: the second line starts where the first ends.
        let second = JSONL.find('\n').unwrap() as u64 + 1;
        src.seek(SeekFrom::Start(second)).unwrap();
        let mut line = String::new();
        BufReader::new(&mut src).read_line(&mut line).unwrap();
        assert_eq!(line, "{\"type\":\"event_msg\",\"n\":2}\n");
        src.seek(SeekFrom::End(-1)).unwrap();
        let mut last = [0u8; 1];
        src.read_exact(&mut last).unwrap();
        assert_eq!(&last, b"\n");

        // Two readers of one snapshot keep their own positions.
        let mut a = open(&z).unwrap();
        let mut b = open(&z).unwrap();
        a.seek(SeekFrom::Start(second)).unwrap();
        let mut first = [0u8; 9];
        b.read_exact(&mut first).unwrap();
        assert_eq!(&first, b"{\"type\":\"");
        let mut from_a = [0u8; 9];
        a.read_exact(&mut from_a).unwrap();
        assert_eq!(&from_a, b"{\"type\":\"");
        assert_eq!(a.stream_position().unwrap(), second + 9);
        assert_eq!(b.stream_position().unwrap(), 9);

        // A plain file opens as itself.
        let p = dir.join("plain.jsonl");
        std::fs::write(&p, JSONL).unwrap();
        let plain = open(&p).unwrap();
        assert!(!plain.is_decoded());
        assert_eq!(plain.len().unwrap(), JSONL.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_transcript_that_changed_form_is_found_in_the_other() {
        let _one = serial();
        let dir = scratch("forms");
        let plain = dir.join("rollout-x.jsonl");
        let z = dir.join("rollout-x.jsonl.zst");
        // Compressed since it was found: the plain path reads the .zst.
        std::fs::write(&z, compress(JSONL.as_bytes())).unwrap();
        assert_eq!(resolve(&plain).as_deref(), Some(z.as_path()));
        let mut s = String::new();
        open(&plain).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, JSONL);
        assert_eq!(len(&plain).unwrap(), JSONL.len() as u64);
        // Resumed since: the plain file is back, the .zst gone, and the .zst path reads it.
        std::fs::remove_file(&z).unwrap();
        std::fs::write(&plain, format!("{JSONL}{{\"n\":4}}\n")).unwrap();
        let back = open(&z).unwrap();
        assert!(!back.is_decoded());
        assert_eq!(
            back.len().unwrap(),
            (JSONL.len() + "{\"n\":4}\n".len()) as u64
        );
        // Both, as a resume writes the plain file back before it removes the .zst: the plain one.
        std::fs::write(&z, compress(JSONL.as_bytes())).unwrap();
        assert_eq!(resolve(&z).as_deref(), Some(plain.as_path()));
        assert!(!open(&z).unwrap().is_decoded());
        std::fs::remove_file(&z).unwrap();
        // Neither: the plain open's own NotFound.
        let gone = dir.join("rollout-gone.jsonl");
        assert_eq!(open(&gone).unwrap_err().kind(), io::ErrorKind::NotFound);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn several_frames_and_a_skippable_one_decode_as_the_whole_file() {
        let _one = serial();
        let dir = scratch("frames");
        let z = dir.join("multi.jsonl.zst");
        let (head, tail) = JSONL.split_at(JSONL.find('\n').unwrap() + 1);
        let mut bytes = compress(head.as_bytes());
        // A skippable frame: magic 0x184D2A50, a 4-byte length, that many bytes.
        bytes.extend_from_slice(&0x184D_2A50u32.to_le_bytes());
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(b"abc");
        bytes.extend(compress(tail.as_bytes()));
        std::fs::write(&z, bytes).unwrap();
        let mut s = String::new();
        open(&z).unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, JSONL);
        assert_eq!(len(&z).unwrap(), JSONL.len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stream_decodes_only_as_far_as_it_is_read() {
        let _one = serial();
        let dir = scratch("stream");
        let z = dir.join("long.jsonl.zst");
        // 4 MiB of lines: a first-line read must not decode the rest.
        let line = format!(
            "{{\"type\":\"event_msg\",\"pad\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let body = line.repeat(4 * 1024);
        std::fs::write(&z, compress(body.as_bytes())).unwrap();
        let mut first = String::new();
        let mut r = BufReader::new(stream(&z).unwrap());
        r.read_line(&mut first).unwrap();
        assert_eq!(first, line);
        let Stream {
            inner: StreamInner::Zstd(frames),
        } = r.into_inner()
        else {
            panic!("a .zst streams through the decoder")
        };
        assert!(
            !frames.frame.is_finished(),
            "the frame was not decoded to its end"
        );
        // Read whole, it is the file.
        let mut all = String::new();
        stream(&z).unwrap().read_to_string(&mut all).unwrap();
        assert_eq!(all, body);
        let mut plain = String::new();
        let p = dir.join("p.jsonl");
        std::fs::write(&p, JSONL).unwrap();
        stream(&p).unwrap().read_to_string(&mut plain).unwrap();
        assert_eq!(plain, JSONL);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file that states its size in one frame (as Codex's do, and the CLI's) is measured from
    /// its headers, without a decode (#s386); one that states none, or holds a second frame, is
    /// decoded to count it, and gives the same length.
    #[test]
    fn a_length_the_frame_states_is_read_without_decoding() {
        let _one = serial();
        let dir = scratch("declared");
        let cli = dir.join("cli.jsonl.zst");
        std::fs::write(
            &cli,
            include_bytes!("../tests/fixtures/zstd-cli-3-check.jsonl.zst"),
        )
        .unwrap();
        let before = DECODES.with(|d| d.get());
        assert_eq!(declared_len(&cli).unwrap(), Some(269));
        assert_eq!(len(&cli).unwrap(), 269);
        assert_eq!(
            DECODES.with(|d| d.get()),
            before,
            "measured from the headers"
        );
        let no_check = dir.join("nc.jsonl.zst");
        std::fs::write(
            &no_check,
            include_bytes!("../tests/fixtures/zstd-cli-3-no-check.jsonl.zst"),
        )
        .unwrap();
        assert_eq!(declared_len(&no_check).unwrap(), Some(269));

        // Several blocks (468 KiB of synthetic lines, the CLI at level 3): walked to the end.
        let blocks = dir.join("blocks.jsonl.zst");
        std::fs::write(
            &blocks,
            include_bytes!("../tests/fixtures/zstd-cli-3-blocks.jsonl.zst"),
        )
        .unwrap();
        assert_eq!(declared_len(&blocks).unwrap(), Some(478_882));
        let mut whole = Vec::new();
        stream(&blocks).unwrap().read_to_end(&mut whole).unwrap();
        assert_eq!(
            whole.len(),
            478_882,
            "the size it states is the size it decodes to"
        );

        // A second frame after it: the header's size is a lower bound only, so it is decoded.
        let two = dir.join("two.jsonl.zst");
        let mut bytes = include_bytes!("../tests/fixtures/zstd-cli-3-no-check.jsonl.zst").to_vec();
        bytes.extend(compress(b"{\"n\":2}\n"));
        std::fs::write(&two, bytes).unwrap();
        assert_eq!(declared_len(&two).unwrap(), None);
        assert_eq!(len(&two).unwrap(), 269 + 8);
        assert_eq!(DECODES.with(|d| d.get()), before + 1, "decoded to count it");
        // Cut short, or not zstd at all: no stated length.
        let cut = dir.join("cut.jsonl.zst");
        std::fs::write(
            &cut,
            &include_bytes!("../tests/fixtures/zstd-cli-3-check.jsonl.zst")[..100],
        )
        .unwrap();
        assert_eq!(declared_len(&cut).unwrap(), None);
        let text = dir.join("text.jsonl.zst");
        std::fs::write(&text, JSONL).unwrap();
        assert_eq!(declared_len(&text).unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Files made by the reference encoder (zstd 1.5.7's CLI at level 3; `zstd -lv` shows the
    /// decoded size in each header), one as Codex writes them (the `zstd` crate over libzstd,
    /// level 3, the size pledged, no checksum) and one with the CLI's default checksum: both
    /// decode to the same hand-written lines.
    #[test]
    fn files_from_the_reference_zstd_encoder_decode() {
        let _one = serial();
        const LINES: &str = concat!(
            r#"{"timestamp":"2026-10-01T09:00:00.000Z","type":"session_meta","payload":{"id":"zstd-cli-fixture","cwd":"/repo"}}"#,
            "\n",
            r#"{"timestamp":"2026-10-01T09:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}"#,
            "\n",
        );
        let dir = scratch("reference");
        for (name, bytes) in [
            (
                "no-check.jsonl.zst",
                &include_bytes!("../tests/fixtures/zstd-cli-3-no-check.jsonl.zst")[..],
            ),
            (
                "check.jsonl.zst",
                &include_bytes!("../tests/fixtures/zstd-cli-3-check.jsonl.zst")[..],
            ),
        ] {
            let z = dir.join(name);
            std::fs::write(&z, bytes).unwrap();
            let mut s = String::new();
            open(&z).unwrap().read_to_string(&mut s).unwrap();
            assert_eq!(s, LINES, "{name}");
            let mut streamed = String::new();
            stream(&z).unwrap().read_to_string(&mut streamed).unwrap();
            assert_eq!(streamed, LINES, "{name}");
            assert_eq!(len(&z).unwrap(), LINES.len() as u64, "{name}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// In a process that keeps records, a file that does not decode whole (here, bytes after
    /// its frame that are no frame) still gives a stream its head, as a lazy stream does: a
    /// failed decode used to fail the stream, and a scan's head readers with it.
    #[test]
    fn a_file_that_does_not_decode_whole_still_streams_its_head() {
        let _one = serial();
        let dir = scratch("broken-tail");
        struct Unname;
        impl Drop for Unname {
            fn drop(&mut self) {
                keep_tails_in(None);
            }
        }
        let _unname = Unname;
        keep_tails_in(Some(dir.join("tails")));
        let z = dir.join("broken.jsonl.zst");
        let mut bytes = compress(JSONL.as_bytes());
        bytes.extend_from_slice(b"not a frame at all");
        std::fs::write(&z, bytes).unwrap();
        assert!(open(&z).is_err(), "the whole file does not decode");
        let mut first = String::new();
        BufReader::new(stream(&z).unwrap())
            .read_line(&mut first)
            .unwrap();
        assert_eq!(first, "{\"type\":\"session_meta\",\"n\":1}\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_is_not_zstd_is_an_error_not_empty() {
        let _one = serial();
        let dir = scratch("bad");
        let z = dir.join("bad.jsonl.zst");
        std::fs::write(&z, JSONL).unwrap();
        assert_eq!(open(&z).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let empty = dir.join("empty.jsonl.zst");
        std::fs::write(&empty, b"").unwrap();
        assert_eq!(open(&empty).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_keeps_one_stem_in_either_form() {
        let _one = serial();
        let p = Path::new("/s/2026/10/01/rollout-2026-10-01T00-00-00-abc.jsonl");
        let z = Path::new("/s/2026/10/01/rollout-2026-10-01T00-00-00-abc.jsonl.zst");
        assert_eq!(stem(p), Some("rollout-2026-10-01T00-00-00-abc"));
        assert_eq!(stem(z), stem(p));
        assert_eq!(plain_path(z), p);
        assert_eq!(plain_path(p), p);
        assert_eq!(stem(Path::new("/x/a.json")), Some("a"));
        assert!(is_compressed(z) && !is_compressed(p));
    }
}

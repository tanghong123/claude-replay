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
    Decoded { snap: Arc<Snapshot>, pos: u64 },
}

impl std::fmt::Debug for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Inner::Plain(_) => f.write_str("Source::Plain"),
            Inner::Decoded { snap, pos } => write!(f, "Source::Decoded({pos}/{})", snap.len),
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
        }
    }

    /// Whether the JSONL is empty.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Whether the bytes were decoded from a compressed file (which never grows).
    pub fn is_decoded(&self) -> bool {
        matches!(self.inner, Inner::Decoded { .. })
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
        match &mut self.inner {
            Inner::Plain(f) => f.read(buf),
            Inner::Decoded { snap, pos } => {
                let n = read_at(&snap.file, buf, *pos)?;
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
/// it is among the last few opened.
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
    Ok(Source {
        inner: Inner::Decoded {
            snap: snapshot(&on_disk)?,
            pos: 0,
        },
    })
}

/// The length of `path`'s JSONL (see [`resolve`]): a plain file's own length, or a compressed
/// one's decoded length, remembered per version of the file. Counting it decodes the file into
/// the kept snapshots, since a reader that asks the length almost always opens it next (a
/// follower's first poll, a tail read, a fold): one decode, not two.
pub fn len(path: &Path) -> io::Result<u64> {
    let on_disk = resolve(path).unwrap_or_else(|| path.to_path_buf());
    if !is_compressed(&on_disk) {
        return Ok(std::fs::metadata(&on_disk)?.len());
    }
    let key = Key::of(&on_disk)?;
    if let Some(len) = lengths().lock().ok().and_then(|m| m.get(&key).copied()) {
        return Ok(len);
    }
    Ok(snapshot(&on_disk)?.len)
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
        let meta = std::fs::metadata(path)?;
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
    let key = Key::of(path)?;
    if let Some(snap) = kept(&key) {
        return Ok(snap);
    }
    let mut file = anonymous_file()?;
    let len = {
        let mut out = io::BufWriter::new(&mut file);
        let len = decode(&mut BufReader::new(File::open(path)?), &mut out)?;
        out.flush()?;
        len
    };
    let snap = Arc::new(Snapshot {
        file,
        len,
        key: key.clone(),
    });
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
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.inner {
            StreamInner::Plain(f) => f.read(buf),
            StreamInner::Zstd(z) => z.read(buf),
        }
    }
}

/// Opens `path`'s transcript (see [`resolve`]) to read once from its start.
pub fn stream(path: &Path) -> io::Result<Stream> {
    let on_disk = resolve(path).unwrap_or_else(|| path.to_path_buf());
    let file = File::open(&on_disk)?;
    Ok(Stream {
        inner: if is_compressed(&on_disk) {
            StreamInner::Zstd(Box::new(Frames::new(BufReader::new(file))))
        } else {
            StreamInner::Plain(file)
        },
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

    /// Files made by the reference encoder (zstd 1.5.7's CLI at level 3; `zstd -lv` shows the
    /// decoded size in each header), one as Codex writes them (the `zstd` crate over libzstd,
    /// level 3, the size pledged, no checksum) and one with the CLI's default checksum: both
    /// decode to the same hand-written lines.
    #[test]
    fn files_from_the_reference_zstd_encoder_decode() {
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

    #[test]
    fn a_file_that_is_not_zstd_is_an_error_not_empty() {
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

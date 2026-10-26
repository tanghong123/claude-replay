//! **The turn index (#s276, `design/slow-links.md` stage 1)** — a map of a session's record log, so
//! a page that has only the log's tail can still list every turn and know what the rest holds.
//!
//! A tail-first open (#314) draws the last 512 KB and reads the head behind it; on a phone over a
//! weak link that head is minutes of transfer, and until it lands the Turns pane, the tool letters
//! and counts, the compaction marks, the artifact roster and the offered files all came from a scan
//! of the records the page had — the tail alone. The index answers those for the records the page
//! is still waiting for.
//!
//! It is a SIDECAR beside the log (`records.index`): one JSON line per committed record, in record
//! order, holding the record's place in the log and whatever in it those features read — taken
//! from the rendered record itself, by the same rules the page's scans apply, so the two cannot
//! disagree about what a record holds. It grows with the log (`RecordStore::put`) and is cut with it
//! (`cut_to`, the one seam every cut of the log goes through). A log from before it, or one that
//! grew while it lagged, is indexed by parsing the records it is missing, once.
//!
//! The reply (`/turn-index`) covers the records BELOW a boundary the page names (`upto`, the count
//! it is still waiting for), exactly: a turn can straddle that boundary, so a per-turn total would
//! count part of it twice.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The record kinds the page counts as tool calls (`toolNames` in the app shell).
const TOOL_KINDS: [&str; 6] = ["bash", "read", "write", "edit", "skill", "tool"];
/// The record kinds that open a turn (`buildUnits`).
const TURN_KINDS: [&str; 3] = ["user", "command", "peer"];

/// One line of the sidecar: one committed record.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub(crate) struct Entry {
    /// The record's offset in the log and its length (the framing newline excluded).
    pub o: u64,
    pub n: u64,
    /// FNV-1a of the record's bytes: what tells this line still describes the record at `o`.
    pub h: String,
    /// The turn it opens: the record trimmed to what a Turns pane row reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<Map<String, Value>>,
    /// Its tool calls by name, nested calls included.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, u64>,
    /// A compaction's head: how it happened and the context size from and to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compaction: Option<Map<String, Value>>,
    /// The paths it offers with their stamps, the best offer per path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Offer>,
    /// The artifacts it published, in the order the page's roster meets them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<Value>,
}

/// A path a record offers, with the stamps the page's `openReference` reads.
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub(crate) struct Offer {
    pub path: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fsig: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sig: String,
}

/// The first non-empty string among `keys` in `o` — JavaScript's `a || b` over strings.
fn first_str<'a>(o: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .filter_map(|k| o.get(*k).and_then(Value::as_str))
        .find(|s| !s.is_empty())
        .unwrap_or("")
}

/// A record's nested records: the items of its `blocks` parts.
fn children(record: &Value) -> impl Iterator<Item = &Value> {
    record
        .get("body")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("p").and_then(Value::as_str) == Some("blocks"))
        .filter_map(|part| part.get("items").and_then(Value::as_array))
        .flatten()
}

/// FNV-1a, 64-bit, as hex — a fingerprint, not a defence: it tells a line from a record that
/// replaced its own after a log was cut and grown again.
fn fingerprint(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

impl Entry {
    /// What one rendered record holds, by the page's own rules (`toolNames`, `artifactRoster`,
    /// `openReference`, the compaction ticks, `buildUnits`).
    pub(crate) fn of(record: &Value, o: u64, bytes: &[u8]) -> Self {
        let mut e = Entry {
            o,
            n: bytes.len() as u64,
            h: fingerprint(bytes),
            ..Entry::default()
        };
        let kind = record.get("kind").and_then(Value::as_str).unwrap_or("");
        if TURN_KINDS.contains(&kind) {
            let mut row = Map::new();
            for k in ["kind", "id", "turn", "label", "ts", "reply_context"] {
                if let Some(v) = record.get(k) {
                    row.insert(k.into(), v.clone());
                }
            }
            if kind == "peer" {
                let head = record.get("head").unwrap_or(&Value::Null);
                let mut h = Map::new();
                for k in ["from", "mode"] {
                    if let Some(v) = head.get(k) {
                        h.insert(k.into(), v.clone());
                    }
                }
                row.insert("head".into(), Value::Object(h));
            }
            e.turn = Some(row);
        }
        if kind == "compaction" {
            let head = record.get("head").unwrap_or(&Value::Null);
            let mut c = Map::new();
            for k in ["compact_trigger", "compact_pre", "compact_post"] {
                if let Some(v) = head.get(k) {
                    c.insert(k.into(), v.clone());
                }
            }
            e.compaction = Some(c);
        }
        e.visit(record);
        e
    }

    fn visit(&mut self, record: &Value) {
        let head = record.get("head").unwrap_or(&Value::Null);
        let kind = record.get("kind").and_then(Value::as_str).unwrap_or("");
        if TOOL_KINDS.contains(&kind) {
            let name = match first_str(record, &["tool"]) {
                "" => match first_str(head, &["name"]) {
                    "" => kind,
                    name => name,
                },
                name => name,
            };
            *self.tools.entry(name.to_string()).or_default() += 1;
        }
        if let Some(a) = head.get("artifact") {
            if !first_str(a, &["url"]).is_empty() {
                let mut item = Map::new();
                for k in ["url", "name", "icon", "desc"] {
                    if let Some(v) = a.get(k) {
                        item.insert(k.into(), v.clone());
                    }
                }
                self.artifacts.push(Value::Object(item));
            }
        }
        let path = first_str(head, &["att_path", "path"]);
        let fsig = first_str(head, &["att_fsig", "fsig"]);
        let sig = first_str(head, &["att_sig", "sig"]);
        if !path.is_empty() && (!fsig.is_empty() || !sig.is_empty()) {
            match self.files.iter_mut().find(|f| f.path == path) {
                // A file stamp beats a reveal stamp; otherwise the first offer stands.
                Some(f) if f.fsig.is_empty() && !fsig.is_empty() => {
                    f.fsig = fsig.into();
                    f.sig = sig.into();
                }
                Some(_) => {}
                None => self.files.push(Offer {
                    path: path.into(),
                    fsig: fsig.into(),
                    sig: sig.into(),
                }),
            }
        }
        for child in children(record) {
            self.visit(child);
        }
    }

    /// The log bytes this line covers through: its record and the framing newline.
    fn end(&self) -> u64 {
        self.o + self.n + 1
    }
}

/// The sidecar and how far it reaches. Its lines are records `[0, lines)`, contiguous; `mark` is
/// the log byte they cover through, so a line is appended only for the record that starts there;
/// `bytes` is the sidecar's own length as this store left it, so an append that finds the file
/// another length (a second process wrote it) starts it over rather than interleave with it.
pub(crate) struct IndexFile {
    path: PathBuf,
    lines: usize,
    mark: u64,
    bytes: u64,
    file: Option<std::fs::File>,
}

impl IndexFile {
    /// Open the sidecar beside a log of `log_len` bytes at `log`, keeping what still describes it:
    /// a torn last line is cut, and lines that reach past the log or whose last record is no
    /// longer the one they fingerprinted are dropped whole (they are rebuilt from the log). Every
    /// read checks the whole sidecar again before it is served (`entries`).
    pub(crate) fn open(path: PathBuf, log: &Path, log_len: u64) -> Self {
        let mut ix = IndexFile {
            path,
            lines: 0,
            mark: 0,
            bytes: 0,
            file: None,
        };
        let Ok(bytes) = std::fs::read(&ix.path) else {
            return ix;
        };
        let complete = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let lines = bytes[..complete].iter().filter(|b| **b == b'\n').count();
        let last = bytes[..complete.saturating_sub(1)]
            .iter()
            .rposition(|b| *b == b'\n')
            .map_or(0, |i| i + 1);
        let tail = (lines > 0)
            .then(|| serde_json::from_slice::<Entry>(&bytes[last..complete - 1]).ok())
            .flatten()
            .filter(|e| {
                e.end() <= log_len && read_at(log, e.o, e.n).is_some_and(|r| fingerprint(&r) == e.h)
            });
        match tail {
            Some(e) => {
                ix.lines = lines;
                ix.mark = e.end();
                ix.bytes = complete as u64;
                if complete < bytes.len() {
                    ix.truncate(complete as u64);
                }
            }
            None if !bytes.is_empty() => ix.truncate(0),
            None => {}
        }
        ix
    }

    fn truncate(&mut self, len: u64) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&self.path) {
            let _ = f.set_len(len);
        }
        self.bytes = len;
        if len == 0 {
            self.lines = 0;
            self.mark = 0;
        }
    }

    /// Append `entries` as lines; `false` when they were not written, and the sidecar has been
    /// started over (an I/O error, or a file another process changed under this store).
    fn write(&mut self, entries: &[Entry]) -> bool {
        if entries.is_empty() {
            return true;
        }
        let mut out = Vec::new();
        for e in entries {
            if serde_json::to_writer(&mut out, e).is_err() {
                return false;
            }
            out.push(b'\n');
        }
        if self.file.is_none() {
            self.file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        let ours = self
            .file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .is_some_and(|m| m.len() == self.bytes);
        let written = ours
            && self
                .file
                .as_mut()
                .is_some_and(|f| f.write_all(&out).is_ok());
        if written {
            self.lines += entries.len();
            self.mark = entries.last().map_or(self.mark, Entry::end);
            self.bytes += out.len() as u64;
        } else {
            self.file = None;
            self.truncate(0);
        }
        written
    }

    /// Whether a record appended at `o` would be the sidecar's next line.
    pub(crate) fn level_at(&self, o: u64) -> bool {
        o == self.mark
    }

    /// The record just appended to the log at `o`, already parsed. Written only while the sidecar
    /// is level with the log; a lagging one is caught up from the log when it is next read.
    pub(crate) fn push(&mut self, record: &Value, o: u64, bytes: &[u8]) {
        if o == self.mark {
            self.write(&[Entry::of(record, o, bytes)]);
        }
    }

    /// The log was cut to `log_len` bytes: drop the lines for records at or past it.
    pub(crate) fn cut_to(&mut self, log_len: u64) {
        if log_len >= self.mark {
            return;
        }
        if log_len == 0 {
            self.truncate(0);
            return;
        }
        let Ok(bytes) = std::fs::read(&self.path) else {
            self.truncate(0);
            return;
        };
        let (mut at, mut lines, mut mark) = (0usize, 0usize, 0u64);
        for line in bytes.split_inclusive(|b| *b == b'\n') {
            match serde_json::from_slice::<Entry>(&line[..line.len() - 1]) {
                Ok(e) if e.end() <= log_len => {
                    at += line.len();
                    lines += 1;
                    mark = e.end();
                }
                _ => break,
            }
        }
        self.truncate(at as u64);
        self.lines = lines;
        self.mark = mark;
    }

    /// Index the records in `[mark, log_len)` the sidecar does not hold yet.
    pub(crate) fn catch_up(&mut self, log: &Path, log_len: u64) {
        if self.mark >= log_len {
            return;
        }
        let Ok(mut f) = std::fs::File::open(log) else {
            return;
        };
        if f.seek(SeekFrom::Start(self.mark)).is_err() {
            return;
        }
        let mut reader = std::io::BufReader::new(f.take(log_len - self.mark));
        let mut line = Vec::new();
        let mut at = self.mark;
        let mut batch = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.last() != Some(&b'\n') {
                break;
            }
            let bytes = &line[..line.len() - 1];
            let Ok(record) = serde_json::from_slice::<Value>(bytes) else {
                break;
            };
            batch.push(Entry::of(&record, at, bytes));
            at += line.len() as u64;
            if batch.len() >= 512 && !self.write(&std::mem::take(&mut batch)) {
                return;
            }
        }
        self.write(&batch);
    }

    /// Every line, checked whole before it is served: as many as the sidecar claims, each parsing,
    /// each starting where the one before ended, from byte 0 to the log's end. A sidecar that fails
    /// any of it (a damaged line would shift every record index after it) is rebuilt from the log.
    fn entries(&mut self, log: &Path, log_len: u64) -> Vec<Entry> {
        for attempt in 0..2 {
            self.catch_up(log, log_len);
            let read: Option<Vec<Entry>> = std::fs::read(&self.path).ok().and_then(|bytes| {
                bytes
                    .split(|b| *b == b'\n')
                    .take(self.lines)
                    .map(|l| serde_json::from_slice::<Entry>(l).ok())
                    .collect()
            });
            if let Some(entries) = read {
                let chained = entries.len() == self.lines
                    && entries
                        .iter()
                        .try_fold(0u64, |at, e| (e.o == at).then(|| e.end()))
                        == Some(log_len);
                if chained {
                    return entries;
                }
            }
            if attempt == 0 {
                self.file = None;
                self.truncate(0);
            }
        }
        Vec::new()
    }

    /// The reply for a page still waiting for records `[0, upto)` of the log at `log` (`log_len`
    /// bytes): their turns (each with its record and byte range), compactions, tool counts,
    /// artifacts and offered files.
    pub(crate) fn reply(&mut self, log: &Path, log_len: u64, epoch: u64, upto: usize) -> Value {
        let entries = self.entries(log, log_len);
        let upto = upto.min(entries.len());
        let opens: Vec<usize> = (0..entries.len())
            .filter(|&i| entries[i].turn.is_some())
            .collect();
        let mut turns = Vec::new();
        for (k, &r) in opens.iter().enumerate().take_while(|(_, &r)| r < upto) {
            let next = opens.get(k + 1).copied();
            let mut row = entries[r].turn.clone().unwrap_or_default();
            row.insert("r".into(), json!(r));
            row.insert("last".into(), json!(next.unwrap_or(entries.len()) - 1));
            row.insert("o".into(), json!(entries[r].o));
            row.insert("end".into(), json!(next.map_or(log_len, |n| entries[n].o)));
            turns.push(Value::Object(row));
        }
        let mut tools: BTreeMap<&str, u64> = BTreeMap::new();
        let mut compactions = Vec::new();
        let mut artifacts = Vec::new();
        let mut files: Vec<Value> = Vec::new();
        let mut file_at: BTreeMap<&str, usize> = BTreeMap::new();
        for (r, e) in entries.iter().enumerate().take(upto) {
            for (name, n) in &e.tools {
                *tools.entry(name).or_default() += n;
            }
            if let Some(c) = &e.compaction {
                compactions.push(json!({"r": r, "head": c}));
            }
            for a in &e.artifacts {
                let mut a = a.clone();
                if let Value::Object(m) = &mut a {
                    m.insert("at".into(), json!(r));
                }
                artifacts.push(a);
            }
            for f in &e.files {
                match file_at.get(f.path.as_str()) {
                    Some(&i) if files[i]["fsig"] == "" && !f.fsig.is_empty() => {
                        files[i] = json!({"path": f.path, "fsig": f.fsig, "sig": f.sig, "r": r});
                    }
                    Some(_) => {}
                    None => {
                        file_at.insert(&f.path, files.len());
                        files.push(json!({"path": f.path, "fsig": f.fsig, "sig": f.sig, "r": r}));
                    }
                }
            }
        }
        json!({
            "t": "turn-index", "epoch": epoch, "upto": upto,
            "records": entries.len(), "bytes": log_len,
            "turns": turns, "compactions": compactions, "tools": tools,
            "artifacts": artifacts, "files": files,
        })
    }
}

/// `n` bytes of `log` at `o`, or `None` when they cannot be read.
fn read_at(log: &Path, o: u64, n: u64) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(log).ok()?;
    f.seek(SeekFrom::Start(o)).ok()?;
    let mut buf = vec![0u8; n as usize];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn tmp(n: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let d = std::env::temp_dir().join(format!(
            "cr-turnindex-{}-{n}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A hand-made record log: a turn, an act holding two calls, a compaction, a peer's turn with
    /// an artifact and a file offered twice (a reveal stamp, then a file stamp), a quoted reply.
    fn records() -> Vec<Value> {
        vec![
            json!({"t": "block", "kind": "user", "id": "t1", "turn": 1, "label": "first ask", "ts": 1.0, "head": {}, "body": []}),
            json!({"t": "block", "kind": "act", "id": "b1", "head": {}, "body": [{"p": "blocks", "items": [
                {"kind": "bash", "tool": "Bash", "head": {"name": "Bash"}},
                {"kind": "read", "head": {"name": "Read", "path": "/r/a.md", "sig": "rv1"}},
                {"kind": "tool", "head": {}},
            ]}]}),
            json!({"t": "block", "kind": "compaction", "id": "b2", "epoch": true, "head": {"compact_trigger": "manual", "compact_pre": 9000, "compact_post": 100, "summary": "long"}}),
            json!({"t": "block", "kind": "peer", "id": "t2", "turn": 2, "label": "tern: look", "head": {"from": "tern", "mode": "bypass", "other": 1}, "body": []}),
            json!({"t": "block", "kind": "tool", "tool": "Artifact", "id": "b3", "head": {"name": "Artifact", "artifact": {"url": "https://x/1", "name": "Deck", "icon": "chart"}, "path": "/r/a.md", "fsig": "fs1", "sig": "rv1"}}),
            json!({"t": "block", "kind": "user", "id": "t3", "turn": 3, "label": "about that", "reply_context": {"version": 1, "items": [{"quote": "q"}]}, "body": []}),
            json!({"t": "block", "kind": "edit", "tool": "Update", "id": "b4", "head": {"name": "Update"}}),
        ]
    }

    /// Write the records as a log; returns the log's path and each record's offset.
    fn log(dir: &Path, recs: &[Value]) -> (PathBuf, Vec<u64>) {
        let path = dir.join("records.jsonl");
        let mut bytes = Vec::new();
        let mut at = Vec::new();
        for r in recs {
            at.push(bytes.len() as u64);
            bytes.extend(r.to_string().into_bytes());
            bytes.push(b'\n');
        }
        std::fs::write(&path, &bytes).unwrap();
        (path, at)
    }

    /// The page's own reading of each record kind, case by case.
    #[test]
    fn an_entry_reads_a_record_as_the_page_does() {
        let r = records();
        let e = Entry::of(&r[1], 10, b"x");
        assert_eq!(
            e.tools,
            BTreeMap::from([("Bash".into(), 1), ("Read".into(), 1), ("tool".into(), 1)]),
            "nested calls count, named by `tool`, then `head.name`, then the kind"
        );
        assert_eq!(
            e.files,
            [Offer {
                path: "/r/a.md".into(),
                fsig: "".into(),
                sig: "rv1".into()
            }]
        );
        assert!(e.turn.is_none());
        let c = Entry::of(&r[2], 0, b"x").compaction.unwrap();
        assert_eq!(
            Value::Object(c),
            json!({"compact_trigger": "manual", "compact_pre": 9000, "compact_post": 100})
        );
        let peer = Entry::of(&r[3], 0, b"x").turn.unwrap();
        assert_eq!(
            peer["head"],
            json!({"from": "tern", "mode": "bypass"}),
            "the sender, no more"
        );
        assert_eq!(peer["label"], "tern: look");
        let quoted = Entry::of(&r[5], 0, b"x").turn.unwrap();
        assert_eq!(quoted["reply_context"]["items"][0]["quote"], "q");
        assert!(quoted.get("head").is_none());
        let art = Entry::of(&r[4], 0, b"x");
        assert_eq!(
            art.artifacts,
            [json!({"url": "https://x/1", "name": "Deck", "icon": "chart"})]
        );
        assert_eq!(art.tools, BTreeMap::from([("Artifact".into(), 1)]));
    }

    /// The reply covers the records below `upto` exactly — a turn straddling it is listed, but
    /// only what lies below counts — and each turn carries its records and bytes.
    #[test]
    fn the_reply_covers_exactly_the_records_below_upto() {
        let dir = tmp("upto");
        let recs = records();
        let (path, at) = log(&dir, &recs);
        let len = std::fs::metadata(&path).unwrap().len();
        let mut ix = IndexFile::open(dir.join("records.index"), &path, len);
        ix.catch_up(&path, len);
        assert_eq!((ix.lines, ix.mark), (recs.len(), len));

        let all = ix.reply(&path, len, 7, recs.len());
        assert_eq!(all["epoch"], 7);
        assert_eq!(all["records"], recs.len());
        let turns = all["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        assert_eq!(
            (turns[0]["r"].clone(), turns[0]["last"].clone()),
            (json!(0), json!(2))
        );
        assert_eq!(
            (turns[0]["o"].clone(), turns[0]["end"].clone()),
            (json!(at[0]), json!(at[3]))
        );
        assert_eq!(
            (
                turns[2]["r"].clone(),
                turns[2]["last"].clone(),
                turns[2]["end"].clone()
            ),
            (json!(5), json!(6), json!(len))
        );
        assert_eq!(
            all["tools"],
            json!({"Artifact": 1, "Bash": 1, "Read": 1, "Update": 1, "tool": 1})
        );
        assert_eq!(
            all["compactions"],
            json!([{"r": 2, "head": {"compact_trigger": "manual", "compact_pre": 9000, "compact_post": 100}}])
        );
        assert_eq!(
            all["artifacts"],
            json!([{"url": "https://x/1", "name": "Deck", "icon": "chart", "at": 4}])
        );
        assert_eq!(
            all["files"],
            json!([{"path": "/r/a.md", "fsig": "fs1", "sig": "rv1", "r": 4}]),
            "a later file stamp beats the first reveal stamp"
        );

        // Waiting for the first four records: turn 2 opens at 3 and runs on past the boundary.
        let head = ix.reply(&path, len, 7, 4);
        assert_eq!(head["upto"], 4);
        let turns = head["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2, "turns that open below the boundary");
        assert_eq!(turns[1]["last"], 4, "…with their whole extent");
        assert_eq!(
            head["tools"],
            json!({"Bash": 1, "Read": 1, "tool": 1}),
            "only the calls below it"
        );
        assert_eq!(head["artifacts"], json!([]));
        assert_eq!(
            head["files"],
            json!([{"path": "/r/a.md", "fsig": "", "sig": "rv1", "r": 1}])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The sidecar follows the log: grown a line per record, cut with it, a torn line cut, and a
    /// sidecar describing records the log no longer holds dropped and rebuilt.
    #[test]
    fn the_sidecar_follows_the_log() {
        let dir = tmp("follow");
        let recs = records();
        let (path, _) = log(&dir, &recs[..3]);
        let side = dir.join("records.index");
        let len3 = std::fs::metadata(&path).unwrap().len();

        // An older log with no sidecar is indexed once, then grows a line per record.
        let mut ix = IndexFile::open(side.clone(), &path, len3);
        assert!(!ix.level_at(len3), "nothing indexed yet");
        ix.catch_up(&path, len3);
        assert!(ix.level_at(len3));
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let mut o = len3;
        for r in &recs[3..] {
            let bytes = r.to_string();
            f.write_all(bytes.as_bytes()).unwrap();
            f.write_all(b"\n").unwrap();
            ix.push(r, o, bytes.as_bytes());
            o += bytes.len() as u64 + 1;
        }
        assert_eq!(ix.lines, recs.len());
        let full = ix.reply(&path, o, 1, recs.len());

        // Reopened, it is level with the log and answers the same.
        let mut reopened = IndexFile::open(side.clone(), &path, o);
        assert_eq!((reopened.lines, reopened.mark), (recs.len(), o));
        assert_eq!(reopened.reply(&path, o, 1, recs.len()), full);

        // Cut with the log to three records.
        let mut cut = IndexFile::open(side.clone(), &path, o);
        cut.cut_to(len3);
        assert_eq!((cut.lines, cut.mark), (3, len3));
        assert_eq!(std::fs::read_to_string(&side).unwrap().lines().count(), 3);

        // A torn last line is cut at open.
        let mut torn = std::fs::read(&side).unwrap();
        torn.extend(b"{\"o\":12");
        std::fs::write(&side, &torn).unwrap();
        let reopened = IndexFile::open(side.clone(), &path, o);
        assert_eq!((reopened.lines, reopened.mark), (3, len3));
        assert_eq!(std::fs::read(&side).unwrap().len(), torn.len() - 7);

        // A log cut and grown again without it (another build, say): the last line no longer
        // describes the record at its offset, so the sidecar is dropped and rebuilt from the log.
        let mut other = recs.clone();
        other[2] = json!({"t": "block", "kind": "assistant", "id": "b9", "head": {}, "body": [{"p": "md", "h": "a different record of a different size"}]});
        let (path, _) = log(&dir, &other);
        let len = std::fs::metadata(&path).unwrap().len();
        let mut ix = IndexFile::open(side.clone(), &path, len);
        assert_eq!((ix.lines, ix.mark), (0, 0), "dropped");
        ix.catch_up(&path, len);
        assert!(ix.reply(&path, len, 1, other.len())["compactions"]
            .as_array()
            .unwrap()
            .is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What is served is checked whole first: a damaged line in the middle (which would shift every
    /// record index after it) and lines another process appended under this store are both found,
    /// and the sidecar is rebuilt from the log rather than served.
    #[test]
    fn a_damaged_or_foreign_sidecar_is_rebuilt_not_served() {
        let dir = tmp("damaged");
        let recs = records();
        let (path, _) = log(&dir, &recs);
        let len = std::fs::metadata(&path).unwrap().len();
        let side = dir.join("records.index");
        let mut ix = IndexFile::open(side.clone(), &path, len);
        let good = ix.reply(&path, len, 1, recs.len());
        assert_eq!(good["turns"].as_array().unwrap().len(), 3);

        // A middle line no longer parses.
        let text = std::fs::read_to_string(&side).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines[2] = "{\"o\":";
        std::fs::write(&side, lines.join("\n") + "\n").unwrap();
        let mut damaged = IndexFile::open(side.clone(), &path, len);
        assert_eq!(
            damaged.reply(&path, len, 1, recs.len()),
            good,
            "rebuilt, not shifted"
        );

        // Another process appended a line: this store's next append finds the file longer than it
        // left it and starts over, and the next read rebuilds it.
        let mut ours = IndexFile::open(side.clone(), &path, len);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&side)
            .unwrap();
        f.write_all(text.lines().next().unwrap().as_bytes())
            .unwrap();
        f.write_all(b"\n").unwrap();
        assert!(
            !ours.write(&[Entry::of(&recs[0], len, b"x")]),
            "not appended after another writer's line"
        );
        assert_eq!(ours.reply(&path, len, 1, recs.len()), good);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// How long a log from before the index takes to index, and how large the index and a reply are:
/// `CR_INDEX_PROBE=<records.jsonl> cargo test --release -p claude-replay-html probe_ -- --ignored
/// --nocapture`. Reads the log; writes the sidecar to scratch only. Prints sizes and times, never content.
#[cfg(test)]
#[test]
#[ignore]
fn probe_index_a_real_log() {
    let Ok(log) = std::env::var("CR_INDEX_PROBE") else {
        return;
    };
    let log = PathBuf::from(log);
    let len = std::fs::metadata(&log).unwrap().len();
    let side = std::env::temp_dir().join(format!("cr-index-probe-{}", std::process::id()));
    let _ = std::fs::remove_file(&side);
    let t = std::time::Instant::now();
    let mut ix = IndexFile::open(side.clone(), &log, len);
    ix.catch_up(&log, len);
    let built = t.elapsed();
    let t = std::time::Instant::now();
    let lines = ix.lines;
    let reply = ix.reply(&log, len, 1, lines).to_string();
    let replied = t.elapsed();
    let t = std::time::Instant::now();
    let reopened = IndexFile::open(side.clone(), &log, len);
    let opened = t.elapsed();
    println!(
        "log {} MB, {} records: indexed in {built:?}, sidecar {} KB; reopened in {opened:?} (level: {}); reply {} KB in {replied:?}",
        len / 1_000_000,
        ix.lines,
        std::fs::metadata(&side).unwrap().len() / 1000,
        reopened.mark == len,
        reply.len() / 1000
    );
    let _ = std::fs::remove_file(&side);
}

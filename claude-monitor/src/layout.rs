//! The session lists the person keeps in other apps, read for the app shell's Arrange choice
//! (#s387, design/layout-sync.md): the Claude desktop app's Code sidebar, the Codex app's thread
//! list, and Herdr's workspaces and tabs. The monitor shows the SAME sessions each app shows, in
//! the app's sections and order, so the person can move between the app and the monitor without
//! hunting for a session (#s387: not only the layout, the same sessions).
//!
//! A layout is sections of entries; an entry names a monitor row id (the transcript's session id),
//! the title the app shows, and the entries the app nests under it. The page decides everything
//! that depends on the monitor's own rows (which ids it knows, where the rest go), so the server
//! never needs the page's filter or hides.
//!
//! Every reader is read-only and tolerant: it never writes the app's files, skips what it cannot
//! parse, ignores keys it does not know, and answers "unavailable, because …" rather than failing
//! the request. Each location has an environment override, so a test reads a hermetic store, and
//! a unit test never reads this machine's apps.
//!
//! A request never reads an app (the review of #s387): each source has a reader thread of its own
//! that runs only while a page follows that source, and the route answers with its last read
//! (`Layouts`). Asking which sources can be followed checks files only.

use crate::desktop_sessions::{self, RecordFiles};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

/// An app whose session list the monitor can follow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    ClaudeApp,
    CodexApp,
    Herdr,
}

impl Source {
    /// The order the Arrange choice lists them in.
    pub const ALL: [Source; 3] = [Source::ClaudeApp, Source::CodexApp, Source::Herdr];

    /// The source a route's `source=` names, or `None` for a name it does not know.
    pub fn parse(key: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.key() == key)
    }

    /// The name on the wire.
    pub fn key(self) -> &'static str {
        match self {
            Source::ClaudeApp => "claude-app",
            Source::CodexApp => "codex-app",
            Source::Herdr => "herdr",
        }
    }

    /// How a sentence names it: "Arranged like the Claude app".
    pub fn label(self) -> &'static str {
        match self {
            Source::ClaudeApp => "the Claude app",
            Source::CodexApp => "the Codex app",
            Source::Herdr => "Herdr",
        }
    }
}

/// One session in an app's list, and the sessions the app draws under it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// The monitor's row id: the transcript's session id.
    pub id: String,
    /// The title the app shows for it, when the app keeps one.
    pub title: Option<String>,
    /// For a Herdr pane, `<server>|<pane id>`: the page prefers the row the monitor itself links
    /// to that pane, because the session id a Herdr hook reported at the agent's start goes stale
    /// when the session moves (#s294).
    pub pane: Option<String>,
    pub children: Vec<Entry>,
}

/// A sub-head inside a section: a project in the Claude app's Sessions section or the Codex app's
/// Threads, a tab in a Herdr workspace.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    pub id: String,
    pub label: String,
    /// Whether the app shows it closed (a Codex project folder the person has not opened).
    pub collapsed: bool,
    /// Drawn even when it holds no session, because the app draws it (a Codex project folder).
    pub keep: bool,
    pub entries: Vec<Entry>,
}

/// One section of an app's list, in the app's order. Entries come before groups.
#[derive(Clone, Debug, PartialEq)]
pub struct Section {
    /// Stable across refreshes, so the page can remember a section the reader opened.
    pub id: String,
    pub label: String,
    /// Whether the app shows it collapsed.
    pub collapsed: bool,
    pub entries: Vec<Entry>,
    pub groups: Vec<Group>,
}

/// One app's list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub sections: Vec<Section>,
    /// Something the reader should know about how faithful the list is, in words the page shows.
    pub note: Option<String>,
}

/// A source's layout, or why it could not be read, in words the page shows.
pub type LayoutRead = Result<Layout, String>;

/// How often a followed source is read, and for how long after a page last asked for it.
const READ_EVERY: Duration = Duration::from_secs(3);
const WANTED_FOR: Duration = Duration::from_secs(30);

type ReadFn = dyn Fn(&mut Readers, Source) -> LayoutRead + Send + Sync;
type ProbeFn = dyn Fn(Source) -> Result<(), String> + Send + Sync;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Each source's last read, kept by a reader thread of its own, so a request never waits on herdr,
/// SQLite or a file and holds no lock while one is read (the review of #s387, as `HerdrReader`
/// asks herdr at its own pace, #s238). A source is read only while a page follows it: the first
/// ask starts its reader, which reads every `READ_EVERY` and ends `WANTED_FOR` after the last ask;
/// until its first read lands the route answers "pending", and a read kept from before its reader
/// last stopped is answered as "stale". A person who never chooses an app never has one read.
pub struct Layouts {
    slots: [Arc<Mutex<Slot>>; 3],
    readers: [Arc<Mutex<Readers>>; 3],
    read: Arc<ReadFn>,
    probe: Arc<ProbeFn>,
    every: Duration,
    wanted_for: Duration,
}

#[derive(Default)]
struct Slot {
    last: Option<LayoutRead>,
    /// Whether `last` was read by the reader that runs now. A reader that stopped (nobody asked
    /// for `WANTED_FOR`) leaves its last read behind, which can be an hour old when a phone comes
    /// back: it is answered as stale until the new reader's first read lands.
    fresh: bool,
    wanted: Option<Instant>,
    reading: bool,
}

/// Clears a slot's `reading` however its thread ends, a panic included, so the next ask starts
/// another.
struct ReadingDone(Arc<Mutex<Slot>>);

impl Drop for ReadingDone {
    fn drop(&mut self) {
        lock(&self.0).reading = false;
    }
}

impl Default for Layouts {
    fn default() -> Self {
        Layouts::with(
            Arc::new(|readers: &mut Readers, source| readers.read(source)),
            Arc::new(probe),
            READ_EVERY,
            WANTED_FOR,
        )
    }
}

impl Layouts {
    fn with(read: Arc<ReadFn>, probe: Arc<ProbeFn>, every: Duration, wanted_for: Duration) -> Self {
        Layouts {
            slots: Default::default(),
            readers: Default::default(),
            read,
            probe,
            every,
            wanted_for,
        }
    }

    fn index(source: Source) -> usize {
        Source::ALL
            .iter()
            .position(|s| *s == source)
            .expect("every source is in ALL")
    }

    /// `GET api/layout?source=<name>`: the source's last read as JSON, starting its reader if none
    /// runs. Never reads the app itself.
    pub fn layout_json(&self, source: Source) -> String {
        let i = Layouts::index(source);
        let mut slot = lock(&self.slots[i]);
        slot.wanted = Some(Instant::now());
        if !slot.reading {
            slot.reading = true;
            slot.fresh = false;
            let (shared, readers, read) = (
                self.slots[i].clone(),
                self.readers[i].clone(),
                self.read.clone(),
            );
            let (every, wanted_for) = (self.every, self.wanted_for);
            let spawned = std::thread::Builder::new()
                .name(format!("layout-{}", source.key()))
                .spawn(move || {
                    let _done = ReadingDone(shared.clone());
                    loop {
                        let fresh = read(&mut lock(&readers), source);
                        let mut slot = lock(&shared);
                        slot.last = Some(fresh);
                        slot.fresh = true;
                        if slot.wanted.is_none_or(|at| at.elapsed() > wanted_for) {
                            return;
                        }
                        drop(slot);
                        std::thread::sleep(every);
                    }
                });
            if spawned.is_err() {
                slot.reading = false;
            }
        }
        match &slot.last {
            Some(read) => {
                let mut v = layout_json(source, read);
                if !slot.fresh {
                    v["stale"] = json!(true);
                }
                v
            }
            None => pending_json(source),
        }
        .to_string()
    }

    /// `GET api/layout`: whether each source can be followed, and why not, for the Arrange choice.
    /// Checks files only; reads no app. Only the file check decides whether a source can be
    /// chosen (`available`, with `reason` when it cannot): a read that failed is said as
    /// `lastError` and never disables the choice, since choosing it is what reads it again (the
    /// second review of #s387: a source that failed once stayed disabled until a restart).
    pub fn sources_json(&self) -> String {
        let sources: Vec<Value> = Source::ALL
            .into_iter()
            .map(|source| {
                let last = lock(&self.slots[Layouts::index(source)])
                    .last
                    .as_ref()
                    .and_then(|read| read.as_ref().err().cloned());
                let reason = (self.probe)(source).err();
                json!({
                    "source": source.key(),
                    "label": source.label(),
                    "available": reason.is_none(),
                    "reason": reason,
                    "lastError": if reason.is_none() { last } else { None },
                })
            })
            .collect();
        json!({ "sources": sources }).to_string()
    }
}

/// Whether a source's files are where its reader looks, without reading them.
fn probe(source: Source) -> Result<(), String> {
    match source {
        Source::ClaudeApp => match (desktop_sessions::store_root(), claude_config_path()) {
            (Some(root), Some(config)) if root.is_dir() && config.is_file() => Ok(()),
            (Some(_), Some(_)) => {
                Err("The Claude app's session list was not found on this machine".into())
            }
            _ => Err("The Claude app is not installed on this machine".into()),
        },
        Source::CodexApp => match codex_db_path() {
            Some(db) if db.is_file() => Ok(()),
            Some(_) => Err("The Codex app's thread database was not found on this machine".into()),
            None => Err("The Codex app is not installed on this machine".into()),
        },
        Source::Herdr => herdr_installed(),
    }
}

/// Whether the herdr program can be found: `AGENT_MONITOR_HERDR`, else `herdr` on `PATH`. A unit
/// test never looks for the real one.
fn herdr_installed() -> Result<(), String> {
    let missing = || Err("Herdr is not installed on this machine".to_string());
    if cfg!(test) && std::env::var_os("AGENT_MONITOR_HERDR").is_none() {
        return missing();
    }
    let program = PathBuf::from(crate::control::herdr_program());
    let found = if program.components().count() > 1 {
        program.is_file()
    } else {
        std::env::var_os("PATH").is_some_and(|path| {
            std::env::split_paths(&path).any(|dir| dir.join(&program).is_file())
        })
    };
    if found {
        Ok(())
    } else {
        missing()
    }
}

/// The readers and what they keep between refreshes: the Claude app's session records (re-read
/// only when one changes), its settings file, and the Codex app's database while it is unchanged.
#[derive(Default)]
pub struct Readers {
    claude_records: ClaudeRecords,
    claude_config: Option<ConfigRead>,
    codex: Option<CodexRead>,
    codex_state: Option<CodexStateRead>,
    /// The last reason logged per source, so an unreadable source is logged once, not on every
    /// read (`READ_EVERY`).
    reported: HashMap<&'static str, String>,
}

struct ConfigRead {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    prefs: Value,
}

struct CodexRead {
    db: PathBuf,
    /// The database's and its catalogue's modification times, each with its write-ahead log.
    stamp: (
        Option<SystemTime>,
        Option<SystemTime>,
        Option<SystemTime>,
        Option<SystemTime>,
    ),
    data: CodexData,
}

struct CodexStateRead {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    sidebar: CodexSidebar,
}

impl Readers {
    /// One source's layout.
    pub fn read(&mut self, source: Source) -> LayoutRead {
        let read = match source {
            Source::ClaudeApp => self.claude_app(),
            Source::CodexApp => self.codex_app(),
            Source::Herdr => read_herdr().map(|servers| herdr_layout(&servers)),
        };
        self.report(source, &read);
        read
    }

    /// Log a source that cannot be read, once per reason: an app update that moves its list should
    /// be noticed in the monitor's log, not only in a disabled option.
    fn report(&mut self, source: Source, read: &LayoutRead) {
        match read {
            Err(reason) if self.reported.get(source.key()) != Some(reason) => {
                eprintln!(
                    "agent-monitor: cannot arrange like {}: {reason}",
                    source.label()
                );
                self.reported.insert(source.key(), reason.clone());
            }
            Err(_) => {}
            Ok(_) => {
                self.reported.remove(source.key());
            }
        }
    }

    fn claude_app(&mut self) -> LayoutRead {
        let (Some(root), Some(config)) = (desktop_sessions::store_root(), claude_config_path())
        else {
            return Err("The Claude app is not installed on this machine".into());
        };
        if !root.is_dir() {
            return Err("The Claude app's session list was not found on this machine".into());
        }
        let prefs = self.claude_prefs(&config)?;
        let records = self.claude_records.records(&root);
        claude_layout(&prefs, &records)
    }

    /// The app's layout settings (`preferences.epitaxyPrefs`), re-read only when the file changes.
    /// A file caught half-written keeps the last good read, so the list does not flicker.
    fn claude_prefs(&mut self, path: &Path) -> Result<Value, String> {
        let meta = std::fs::metadata(path)
            .map_err(|_| "The Claude app's settings file was not found".to_string())?;
        let (modified, len) = (meta.modified().ok(), meta.len());
        if let Some(read) = &self.claude_config {
            if read.path == path && read.modified == modified && read.len == len {
                return Ok(read.prefs.clone());
            }
        }
        let fresh = (len <= CONFIG_LIMIT)
            .then(|| std::fs::read(path).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let Some(whole) = fresh else {
            return match &self.claude_config {
                Some(read) if read.path == path => Ok(read.prefs.clone()),
                _ => Err("The Claude app's settings file could not be read".into()),
            };
        };
        let prefs = whole
            .pointer("/preferences/epitaxyPrefs")
            .filter(|p| p.is_object())
            .cloned()
            .ok_or_else(|| {
                "The Claude app's settings hold no sidebar layout (preferences.epitaxyPrefs): \
                 this version of the app may keep it elsewhere"
                    .to_string()
            })?;
        self.claude_config = Some(ConfigRead {
            path: path.to_path_buf(),
            modified,
            len,
            prefs: prefs.clone(),
        });
        Ok(prefs)
    }

    fn codex_app(&mut self) -> LayoutRead {
        let Some(db) = codex_db_path() else {
            return Err("The Codex app is not installed on this machine".into());
        };
        if !db.is_file() {
            return Err("The Codex app's thread database was not found on this machine".into());
        }
        let catalog = codex_catalog_path(&db);
        let stamp = (
            modified(&db),
            modified(&PathBuf::from(format!("{}-wal", db.display()))),
            modified(&catalog),
            modified(&PathBuf::from(format!("{}-wal", catalog.display()))),
        );
        let fresh = self
            .codex
            .as_ref()
            .is_some_and(|read| read.db == db && read.stamp == stamp);
        if !fresh {
            match read_codex(&db) {
                Ok(data) => {
                    self.codex = Some(CodexRead {
                        db: db.clone(),
                        stamp,
                        data,
                    })
                }
                // Busy: the last read stands, and the next refresh tries again.
                Err(reason) if reason == CODEX_BUSY && self.codex.is_some() => {}
                Err(reason) => return Err(reason),
            }
        }
        let data = &self.codex.as_ref().expect("read above").data;
        // The sidebar itself — its sections, what each holds, the pins, the project folders — is
        // in the app's state file; without it the database's sections are the best guess.
        match codex_state(&db, &mut self.codex_state) {
            Ok(sidebar) => Ok(codex_sidebar_layout(data, sidebar)),
            Err(reason) => {
                let mut layout = codex_layout(data);
                layout.note = Some(format!(
                    "{reason}; showing the sections its database holds{}",
                    layout.note.map(|n| format!(". {n}")).unwrap_or_default()
                ));
                Ok(layout)
            }
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A layout as the page reads it.
pub fn layout_json(source: Source, read: &LayoutRead) -> Value {
    let (available, reason, note, sections) = match read {
        Ok(layout) => (
            true,
            None,
            layout.note.clone(),
            layout.sections.iter().map(section_json).collect(),
        ),
        Err(reason) => (false, Some(reason.clone()), None, Vec::new()),
    };
    json!({
        "source": source.key(),
        "label": source.label(),
        "available": available,
        "reason": reason,
        "note": note,
        "sections": sections,
    })
}

fn section_json(section: &Section) -> Value {
    json!({
        "id": section.id,
        "label": section.label,
        "collapsed": section.collapsed,
        "entries": section.entries.iter().map(entry_json).collect::<Vec<_>>(),
        "groups": section.groups.iter().map(|group| json!({
            "id": group.id,
            "label": group.label,
            "collapsed": group.collapsed,
            "keep": group.keep,
            "entries": group.entries.iter().map(entry_json).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

fn entry_json(entry: &Entry) -> Value {
    json!({
        "id": entry.id,
        "title": entry.title,
        "pane": entry.pane,
        "children": entry.children.iter().map(entry_json).collect::<Vec<_>>(),
    })
}

/// A source whose first read has not landed yet: the page shows that it is reading.
pub fn pending_json(source: Source) -> Value {
    json!({
        "source": source.key(),
        "label": source.label(),
        "available": true,
        "pending": true,
        "reason": null,
        "note": null,
        "sections": [],
    })
}

// ---------------------------------------------------------------------------------------------
// The Claude app (design/layout-sync.md §3.1). Its Code sidebar is a list of sections in the
// person's order: Pinned, Routines, one per custom group, and Sessions (the rest, by project).
// A side session is drawn under its starter, in its starter's section; an archived session leaves
// the sidebar; a pinned session keeps its group but is drawn under Pinned. Measured against the
// app's own sidebar listing on 2026-11-06 (Claude desktop 4.16336.0).
// ---------------------------------------------------------------------------------------------

/// The settings file is a few hundred kilobytes; anything far larger is not it.
const CONFIG_LIMIT: u64 = 16 << 20;

/// Where the Claude app keeps its settings, the sidebar's layout among them:
/// `CLAUDE_DESKTOP_CONFIG` when set (a test's hermetic file), else
/// `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS.
pub fn claude_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CLAUDE_DESKTOP_CONFIG") {
        return Some(PathBuf::from(path));
    }
    if cfg!(test) || !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/Claude/claude_desktop_config.json"))
}

/// What the layout needs of one session record.
#[derive(Clone, Debug, PartialEq)]
struct AppRecord {
    /// The app's id, `local_<uuid>`.
    desktop: String,
    /// The transcript's id: the monitor's row id.
    cli: String,
    /// `<account>/<organization>`: the directories the record sits in, and the key the app's
    /// settings are kept under.
    scope: String,
    title: Option<String>,
    starter: Option<String>,
    starred: bool,
    archived: bool,
    /// The folder the session was started in, which names its project in the Sessions section.
    folder: Option<String>,
    /// Milliseconds since the epoch.
    last_activity: i64,
    /// Milliseconds since the epoch.
    created: i64,
}

/// The records read so far, through the store's one record cache (`RecordFiles`).
#[derive(Default)]
struct ClaudeRecords {
    files: RecordFiles<AppRecord>,
}

impl ClaudeRecords {
    fn records(&mut self, root: &Path) -> Vec<AppRecord> {
        self.files
            .refresh(root, |bytes, path| parse_app_record(bytes, scope_of(path)));
        let mut out: Vec<AppRecord> = self.files.iter().map(|(_, r)| r.clone()).collect();
        out.sort_by(|a, b| a.desktop.cmp(&b.desktop));
        out
    }
}

/// `<store>/<account>/<org>/local_x.json` → `<account>/<org>`.
fn scope_of(path: &Path) -> String {
    let name = |p: Option<&Path>| {
        p.and_then(Path::file_name)
            .and_then(|n| n.to_str())
            .map(str::to_string)
    };
    let org = path.parent();
    match (name(org.and_then(Path::parent)), name(org)) {
        (Some(account), Some(org)) => format!("{account}/{org}"),
        _ => String::new(),
    }
}

/// A record's layout facts, or `None` for a file that is not a session record or has no transcript.
fn parse_app_record(bytes: &[u8], scope: String) -> Option<AppRecord> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let text = |pointer: &str| {
        v.pointer(pointer)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    let flag = |key: &str| v.get(key).and_then(Value::as_bool).unwrap_or(false);
    Some(AppRecord {
        desktop: text("/sessionId")?,
        cli: text("/cliSessionId")?,
        scope,
        title: text("/title"),
        starter: text("/spawnedFrom/sessionId"),
        starred: flag("isStarred"),
        archived: flag("isArchived"),
        folder: text("/originCwd").or_else(|| text("/cwd")),
        last_activity: v
            .get("lastActivityAt")
            .or_else(|| v.get("createdAt"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        created: v.get("createdAt").and_then(Value::as_i64).unwrap_or(0),
    })
}

/// How a section orders its sessions (`sortBy`, `ascending`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Order {
    Recent {
        ascending: bool,
    },
    Title {
        ascending: bool,
    },
    Created {
        ascending: bool,
    },
    /// The person's own order (a group's `order` list, or the pins'), the rest by recency after.
    Manual,
}

fn order_of(section: &Value) -> Order {
    let ascending = section
        .get("ascending")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // The app's names are `recency`, `alpha` and `created` (its sidebar tool's `sort_by`).
    match section.get("sortBy").and_then(Value::as_str) {
        Some("manual" | "custom") => Order::Manual,
        Some("alpha" | "title" | "name") => Order::Title { ascending },
        Some("created") => Order::Created { ascending },
        _ => Order::Recent { ascending },
    }
}

fn sort_records(records: &mut [&AppRecord], order: Order, manual: &[String]) {
    let recent = |a: &AppRecord, b: &AppRecord| {
        b.last_activity
            .cmp(&a.last_activity)
            .then_with(|| a.desktop.cmp(&b.desktop))
    };
    match order {
        Order::Recent { ascending } => records.sort_by(|a, b| {
            let by = recent(a, b);
            if ascending {
                by.reverse()
            } else {
                by
            }
        }),
        Order::Title { ascending } => records.sort_by(|a, b| {
            let key = |r: &AppRecord| r.title.clone().unwrap_or_default().to_lowercase();
            let by = key(a).cmp(&key(b)).then_with(|| recent(a, b));
            if ascending {
                by
            } else {
                by.reverse()
            }
        }),
        Order::Created { ascending } => records.sort_by(|a, b| {
            let by = b.created.cmp(&a.created).then_with(|| recent(a, b));
            if ascending {
                by.reverse()
            } else {
                by
            }
        }),
        Order::Manual => {
            let at = |r: &AppRecord| manual.iter().position(|id| *id == r.desktop);
            records.sort_by(|a, b| match (at(a), at(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => recent(a, b),
            })
        }
    }
}

/// `code:local_x` (a group assignment's key and its order entries) → `local_x`.
fn desktop_id(key: &str) -> String {
    key.strip_prefix("code:").unwrap_or(key).to_string()
}

fn strings(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(desktop_id)
                .collect()
        })
        .unwrap_or_default()
}

/// The Claude app's sidebar from its settings and its session records.
fn claude_layout(prefs: &Value, records: &[AppRecord]) -> LayoutRead {
    let sections_by_scope = prefs.get("dframe-code-sections").and_then(Value::as_object);
    // The account and organization the app shows: the one the most recent session belongs to.
    let scope = records
        .iter()
        .max_by(|a, b| {
            a.last_activity
                .cmp(&b.last_activity)
                .then_with(|| b.desktop.cmp(&a.desktop))
        })
        .map(|r| r.scope.clone())
        .or_else(|| sections_by_scope.and_then(|m| m.keys().next().cloned()))
        .ok_or_else(|| "The Claude app has no Code sessions on this machine".to_string())?;
    let records: Vec<&AppRecord> = records.iter().filter(|r| r.scope == scope).collect();
    let mut sections: Vec<Value> = sections_by_scope
        .and_then(|m| m.get(&scope))
        .and_then(|s| s.get("sections"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            // An app that never saved a section list draws its defaults.
            vec![
                json!({"id": "pinned", "kind": "pinned", "order": 0}),
                json!({"id": "sessions", "kind": "sessions", "order": 1}),
            ]
        });
    sections.sort_by_key(|s| s.get("order").and_then(Value::as_i64).unwrap_or(i64::MAX));
    let groups = prefs.get("dframe-group-scopes").and_then(|g| g.get(&scope));
    let group_names: HashMap<String, String> = groups
        .and_then(|g| g.get("groups"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|g| {
                    Some((
                        g.get("id")?.as_str()?.to_string(),
                        g.get("name")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    let assignment: HashMap<String, String> = groups
        .and_then(|g| g.get("assignments"))
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((desktop_id(k), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let group_order = |group: &str| {
        strings(groups.and_then(|g| g.pointer(&format!("/order/{}", pointer_key(group)))))
    };
    let starred_list: HashSet<String> = strings(prefs.get("starred-local-code-sessions"))
        .into_iter()
        .collect();
    let pinned_order = strings(prefs.pointer("/dframe-local-slice/pinnedOrder"));

    // A side session is drawn under its starter, wherever the starter is filed (side sessions
    // follow their starter's group). A starter chain that loops is broken at the record that sees
    // itself again, so nothing is lost.
    let by_desktop: HashMap<&str, &AppRecord> =
        records.iter().map(|r| (r.desktop.as_str(), *r)).collect();
    let is_top = |r: &AppRecord| {
        let mut at = r.starter.as_deref();
        let mut seen = HashSet::new();
        while let Some(starter) = at {
            if starter == r.desktop || !seen.insert(starter) {
                return true;
            }
            match by_desktop.get(starter) {
                Some(next) => at = next.starter.as_deref(),
                None => return seen.len() == 1,
            }
        }
        r.starter.is_none()
    };
    let mut kids: HashMap<&str, Vec<&AppRecord>> = HashMap::new();
    let mut tops: Vec<&AppRecord> = Vec::new();
    for &r in &records {
        if is_top(r) {
            tops.push(r);
        } else if let Some(starter) = r.starter.as_deref() {
            kids.entry(starter).or_default().push(r);
        }
    }
    let starred = |r: &AppRecord| r.starred || starred_list.contains(&r.desktop);
    let pinned_shown = sections.iter().any(|s| {
        s.get("kind").and_then(Value::as_str) == Some("pinned")
            && s.get("shown").and_then(Value::as_bool) != Some(false)
    });
    let filed = |r: &AppRecord| {
        assignment
            .get(&r.desktop)
            .filter(|g| group_names.contains_key(*g))
            .cloned()
    };
    let entries = |mut chosen: Vec<&AppRecord>, order: Order, manual: &[String], archived: bool| {
        sort_records(&mut chosen, order, manual);
        chosen
            .into_iter()
            .map(|r| claude_entry(r, &kids, archived, &mut HashSet::new()))
            .collect::<Vec<Entry>>()
    };

    let mut out = Vec::new();
    let mut unknown = Vec::new();
    for section in &sections {
        if section.get("shown").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let kind = section.get("kind").and_then(Value::as_str).unwrap_or("");
        let id = section
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or(kind)
            .to_string();
        let collapsed = section
            .get("collapsed")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let order = order_of(section);
        let live: Vec<&AppRecord> = tops.iter().copied().filter(|r| !r.archived).collect();
        let (label, top_entries, sub_groups) = match kind {
            "pinned" => {
                let chosen = live.into_iter().filter(|r| starred(r)).collect();
                let order = if pinned_order.is_empty() {
                    order
                } else {
                    Order::Manual
                };
                (
                    "Pinned".to_string(),
                    entries(chosen, order, &pinned_order, false),
                    Vec::new(),
                )
            }
            "routines" => continue,
            "manual" => {
                let chosen = live
                    .into_iter()
                    .filter(|r| filed(r).as_deref() == Some(id.as_str()))
                    .filter(|r| !(pinned_shown && starred(r)))
                    .collect();
                let label = group_names
                    .get(&id)
                    .cloned()
                    .or_else(|| {
                        section
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|n| !n.is_empty())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| "Group".to_string());
                (
                    label,
                    entries(chosen, order, &group_order(&id), false),
                    Vec::new(),
                )
            }
            "sessions" => {
                let chosen: Vec<&AppRecord> = live
                    .into_iter()
                    .filter(|r| filed(r).is_none())
                    .filter(|r| !(pinned_shown && starred(r)))
                    .collect();
                let by_project = section
                    .get("groupBy")
                    .and_then(Value::as_str)
                    .unwrap_or("project")
                    == "project";
                if by_project {
                    (
                        "Sessions".to_string(),
                        Vec::new(),
                        project_groups(chosen, order, &kids),
                    )
                } else {
                    (
                        "Sessions".to_string(),
                        entries(chosen, order, &[], false),
                        Vec::new(),
                    )
                }
            }
            other => {
                unknown.push(other.to_string());
                continue;
            }
        };
        out.push(Section {
            id,
            label,
            collapsed,
            entries: top_entries,
            groups: sub_groups,
        });
    }
    let archived: Vec<&AppRecord> = tops.iter().copied().filter(|r| r.archived).collect();
    if !archived.is_empty() {
        out.push(Section {
            id: "archived".into(),
            label: "Archived".into(),
            collapsed: true,
            entries: entries(archived, Order::Recent { ascending: false }, &[], true),
            groups: Vec::new(),
        });
    }
    let note = (!unknown.is_empty()).then(|| {
        format!(
            "The Claude app has sections of a kind this monitor does not know ({}); they are not shown",
            unknown.join(", ")
        )
    });
    Ok(Layout {
        sections: out,
        note,
    })
}

/// A JSON pointer's escaping of one key.
fn pointer_key(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

/// The Sessions section's projects: one group per folder, the most recently active first.
fn project_groups(
    chosen: Vec<&AppRecord>,
    order: Order,
    kids: &HashMap<&str, Vec<&AppRecord>>,
) -> Vec<Group> {
    let mut by_folder: Vec<(String, Vec<&AppRecord>)> = Vec::new();
    for r in chosen {
        let folder = r.folder.clone().unwrap_or_default();
        match by_folder.iter_mut().find(|(f, _)| *f == folder) {
            Some((_, rs)) => rs.push(r),
            None => by_folder.push((folder, vec![r])),
        }
    }
    let newest = |rs: &[&AppRecord]| rs.iter().map(|r| r.last_activity).max().unwrap_or(0);
    by_folder.sort_by(|(fa, a), (fb, b)| newest(b).cmp(&newest(a)).then_with(|| fa.cmp(fb)));
    by_folder
        .into_iter()
        .map(|(folder, mut rs)| {
            sort_records(&mut rs, order, &[]);
            let label = Path::new(&folder)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|n| !n.is_empty())
                .unwrap_or("No folder")
                .to_string();
            Group {
                id: folder,
                label,
                collapsed: false,
                keep: false,
                entries: rs
                    .into_iter()
                    .map(|r| claude_entry(r, kids, false, &mut HashSet::new()))
                    .collect(),
            }
        })
        .collect()
}

/// A session and its side sessions, the most recent first. Archived side sessions leave the
/// sidebar with every other archived session, except under an archived starter.
fn claude_entry(
    r: &AppRecord,
    kids: &HashMap<&str, Vec<&AppRecord>>,
    archived: bool,
    seen: &mut HashSet<String>,
) -> Entry {
    seen.insert(r.desktop.clone());
    let mut children: Vec<&AppRecord> = kids
        .get(r.desktop.as_str())
        .map(|k| {
            k.iter()
                .copied()
                .filter(|c| (archived || !c.archived) && !seen.contains(&c.desktop))
                .collect()
        })
        .unwrap_or_default();
    sort_records(&mut children, Order::Recent { ascending: false }, &[]);
    Entry {
        id: r.cli.clone(),
        title: r.title.clone(),
        pane: None,
        children: children
            .into_iter()
            .map(|c| claude_entry(c, kids, archived, seen))
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// The Codex app (design/layout-sync.md §3.2), read from its app-server database: pinned threads,
// the person's sections, threads by project, archived threads, sub-agents under their parent.
// That app is moving each thread's project from its private state file into this database; the
// layout says how many threads have no project there yet.
// ---------------------------------------------------------------------------------------------

/// Where the Codex app keeps its threads: `AGENT_MONITOR_CODEX_APP_DB` when set (a test's
/// hermetic database), else `state_5.sqlite` under `$CODEX_HOME` (default `~/.codex`) on macOS.
pub fn codex_db_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("AGENT_MONITOR_CODEX_APP_DB") {
        return Some(PathBuf::from(path));
    }
    if cfg!(test) || !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex")))?;
    Some(home.join("state_5.sqlite"))
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CodexThread {
    /// The thread's id, which its sub-agent edges name.
    id: String,
    /// The monitor's row id: the rollout file's name without its extensions
    /// (`rollout-<time>-<id>`), or the thread's id where the database names no rollout.
    row: String,
    title: Option<String>,
    project: Option<String>,
    section: Option<String>,
    section_position: Option<i64>,
    pinned: bool,
    archived: bool,
    /// Milliseconds since the epoch.
    recency: i64,
    /// Milliseconds since the epoch.
    created: i64,
    parent: Option<String>,
    /// A `codex exec` run or a sub-agent, by the database's `source` (`exec`, or a `subagent`
    /// object): the app does not list it. Read for when its catalogue cannot be.
    headless: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct CodexData {
    /// `(id, name)` in the app's order.
    projects: Vec<(String, String)>,
    /// `(id, name)` in the order they were made: the database keeps no other.
    sections: Vec<(String, String)>,
    threads: Vec<CodexThread>,
    /// The threads the app lists (its own catalogue, `local_thread_catalog` in its `codex.db`),
    /// or `None` where that catalogue cannot be read. Measured 2026-11-06: `codex exec` runs and
    /// sub-agent threads are in the threads table but not in the catalogue, and the app does not
    /// list them, so neither does this view. Without the catalogue they are told apart by their
    /// `source` (`CodexThread::headless`) and `catalogue_note` says so.
    listed: Option<HashSet<String>>,
    /// Why the catalogue could not be read, when it could not.
    catalogue_note: Option<String>,
    /// Thread rows the database holds that did not read (a column of an unexpected type).
    unreadable: usize,
}

impl CodexData {
    /// Whether the app lists this thread: its catalogue says so, or without one, it is not a
    /// `codex exec` run or a sub-agent.
    fn lists(&self, t: &CodexThread) -> bool {
        match &self.listed {
            Some(listed) => listed.contains(&t.id),
            None => !t.headless,
        }
    }

    /// The note for a layout drawn from this data: a catalogue that could not be read, and threads
    /// that could not be.
    fn note(&self) -> Option<String> {
        let unreadable = (self.unreadable > 0).then(|| {
            format!(
                "{} thread{} in the Codex app's database could not be read and {} not shown",
                self.unreadable,
                if self.unreadable == 1 { "" } else { "s" },
                if self.unreadable == 1 { "is" } else { "are" },
            )
        });
        join_notes(self.catalogue_note.clone(), unreadable)
    }
}

/// Why a read of the Codex app's database failed: a database another process holds is busy, and
/// the last good read stands; anything else is said as it is.
const CODEX_BUSY: &str = "The Codex app's thread database is busy";

/// Where the Codex app keeps its own catalogue of the threads it lists:
/// `AGENT_MONITOR_CODEX_APP_CATALOG` when set, else `sqlite/codex.db` beside its database.
fn codex_catalog_path(db: &Path) -> PathBuf {
    std::env::var_os("AGENT_MONITOR_CODEX_APP_CATALOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| db.with_file_name("sqlite").join("codex.db"))
}

/// One of the Codex app's SQLite files, opened read-only so that no file of the app's is made.
///
/// A reader of a WAL database makes `-shm` and `-wal` beside it when they are not there, and they
/// stay after it closes (measured with SQLite 3.54, the second review of #s387). With no `-wal`,
/// every committed page of a WAL database is in the main file, which a writer changes only at a
/// checkpoint, so that file is opened immutable, which makes nothing; while the app, the Codex CLI
/// or the monitor's own title reader has the database open, the `-wal` and `-shm` are there and
/// are shared as any reader shares them. An immutable connection takes no lock, so a writer that
/// arrives and checkpoints during the read could hand it a mix of old and new pages: `moved` says
/// a writer reached the file, and the read is then treated as busy (the third review of #s387). A
/// database in the rollback-journal mode is opened as usual: its readers make no file, and
/// immutable would ignore a writer's lock. A busy database answers at once rather than after
/// SQLite's default five seconds: the reader thread then keeps its last read (`CODEX_BUSY`).
#[cfg(target_os = "macos")]
struct AppDb {
    conn: rusqlite::Connection,
    path: PathBuf,
    /// The file's modification time when it was opened immutable; `None` when opened as usual.
    immutable_at: Option<Option<SystemTime>>,
}

#[cfg(target_os = "macos")]
impl AppDb {
    fn open(path: &Path) -> rusqlite::Result<AppDb> {
        use rusqlite::{Connection, OpenFlags};
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let immutable = !AppDb::wal(path).exists() && sqlite_wal_mode(path);
        let at = modified(path);
        let conn = if immutable {
            Connection::open_with_flags(immutable_uri(path), flags)?
        } else {
            Connection::open_with_flags(path, flags)?
        };
        conn.busy_timeout(Duration::from_millis(250))?;
        Ok(AppDb {
            conn,
            path: path.to_path_buf(),
            immutable_at: immutable.then_some(at),
        })
    }

    fn wal(path: &Path) -> PathBuf {
        PathBuf::from(format!("{}-wal", path.display()))
    }

    /// Whether a writer reached a file read immutable while it was read.
    fn moved(&self) -> bool {
        self.immutable_at
            .is_some_and(|at| AppDb::wal(&self.path).exists() || modified(&self.path) != at)
    }
}

#[cfg(target_os = "macos")]
fn sqlite_busy(e: &rusqlite::Error) -> bool {
    matches!(e, rusqlite::Error::SqliteFailure(f, _)
        if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

/// A value of an unexpected type in one row: that row is counted and the rest still read. Any
/// other error ends SQLite's scan, so what was read before it is not the whole list.
#[cfg(target_os = "macos")]
fn sqlite_row_type(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::FromSqlConversionFailure(..)
            | rusqlite::Error::InvalidColumnType(..)
            | rusqlite::Error::IntegralValueOutOfRange(..)
    )
}

#[cfg(target_os = "macos")]
fn read_codex(db: &Path) -> Result<CodexData, String> {
    let app = AppDb::open(db).map_err(|e| {
        if sqlite_busy(&e) {
            CODEX_BUSY.to_string()
        } else {
            format!("The Codex app's thread database could not be opened: {e}")
        }
    })?;
    let read = read_codex_threads(&app.conn);
    if app.moved() {
        return Err(CODEX_BUSY.into());
    }
    let (mut data, has_source) = read?;
    // The app's own catalogue says which threads it lists. Without one (an app that keeps none,
    // or a file that does not read) the headless threads are told apart by their `source`, and
    // the note says the list is the monitor's guess.
    let catalogue = codex_catalog_path(db);
    let listed = AppDb::open(&catalogue).and_then(|c| {
        let mut stmt = c
            .conn
            .prepare("SELECT thread_id FROM local_thread_catalog")?;
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<String>, _>>();
        drop(stmt);
        if c.moved() {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
                None,
            ));
        }
        ids
    });
    match listed {
        Ok(ids) => data.listed = Some(ids),
        Err(e) if sqlite_busy(&e) => return Err(CODEX_BUSY.into()),
        Err(e) => {
            let why = if catalogue.is_file() {
                format!("could not be read ({e})")
            } else {
                "was not found".to_string()
            };
            let guess = if has_source {
                "so every thread but codex exec runs, sub-agents and other runs it does not list is \
                 shown"
            } else {
                "and its database names no thread's source, so every thread is shown"
            };
            data.catalogue_note = Some(format!(
                "The Codex app's own list of threads (sqlite/codex.db) {why}, {guess}"
            ));
        }
    }
    Ok(data)
}

/// The threads, projects and sections in the Codex app's database, and whether it names each
/// thread's `source` (an older database has no such column: every thread then counts as listed).
#[cfg(target_os = "macos")]
fn read_codex_threads(conn: &rusqlite::Connection) -> Result<(CodexData, bool), String> {
    let unknown = |e: rusqlite::Error| {
        if sqlite_busy(&e) {
            CODEX_BUSY.to_string()
        } else {
            format!("The Codex app's thread database has a shape this monitor does not know ({e})")
        }
    };
    let failed = |e: rusqlite::Error| {
        if sqlite_busy(&e) {
            CODEX_BUSY.to_string()
        } else {
            format!("The Codex app's thread database could not be read ({e})")
        }
    };
    let pairs = |sql: &str| -> Result<Vec<(String, String)>, String> {
        let mut stmt = conn.prepare(sql).map_err(unknown)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(failed)?;
        rows.collect::<Result<_, _>>().map_err(failed)
    };
    let projects = pairs("SELECT id, name FROM projects ORDER BY position, id")?;
    let sections = pairs("SELECT id, name FROM thread_sections ORDER BY rowid")?;
    // Sub-agent edges are optional: an older database has no table for them.
    let parents: HashMap<String, String> =
        pairs("SELECT child_thread_id, parent_thread_id FROM thread_spawn_edges")
            .unwrap_or_default()
            .into_iter()
            .collect();
    let threads_sql = |source: &str| {
        format!(
            "SELECT id, rollout_path, COALESCE(NULLIF(TRIM(name), ''), NULLIF(TRIM(title), '')), \
             NULLIF(project_id, ''), NULLIF(thread_section_id, ''), section_position, \
             is_pinned, archived, \
             COALESCE(NULLIF(recency_at_ms, 0), updated_at_ms, updated_at * 1000, 0), \
             COALESCE(created_at_ms, created_at * 1000, 0), {source} \
             FROM threads"
        )
    };
    // An older database names no thread's source: read without it (the third review of #s387).
    let (mut stmt, has_source) = match conn.prepare(&threads_sql("source")) {
        Ok(stmt) => (stmt, true),
        Err(e) if !sqlite_busy(&e) => (conn.prepare(&threads_sql("NULL")).map_err(unknown)?, false),
        Err(e) => return Err(unknown(e)),
    };
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let rollout: Option<String> = row.get(1)?;
            let source: Option<String> = row.get(10)?;
            Ok(CodexThread {
                row: rollout
                    .as_deref()
                    .and_then(rollout_row_id)
                    .unwrap_or_else(|| id.clone()),
                id,
                title: row.get(2)?,
                project: row.get(3)?,
                section: row.get(4)?,
                section_position: row.get(5)?,
                pinned: row.get::<_, i64>(6)? != 0,
                archived: row.get::<_, i64>(7)? != 0,
                recency: row.get(8)?,
                created: row.get(9)?,
                parent: None,
                headless: source.as_deref().is_some_and(codex_headless),
            })
        })
        .map_err(failed)?;
    // A row of an unexpected type is counted and said, never dropped in silence. Any other error
    // ends the scan part-way, and a part of the list is never kept as the whole (the third review
    // of #s387): the read fails, busy or not, and the last good read stands.
    let mut threads = Vec::new();
    let mut unreadable = 0;
    for row in rows {
        match row {
            Ok(mut t) => {
                t.parent = parents.get(&t.id).cloned();
                threads.push(t);
            }
            Err(e) if sqlite_row_type(&e) => unreadable += 1,
            Err(e) => return Err(failed(e)),
        }
    }
    Ok((
        CodexData {
            projects,
            sections,
            threads,
            listed: None,
            catalogue_note: None,
            unreadable,
        },
        has_source,
    ))
}

/// Whether an SQLite file is in the WAL mode, by its header: the file format's write and read
/// versions (bytes 18 and 19) are 2 in WAL mode and 1 in the rollback-journal mode.
#[cfg(target_os = "macos")]
fn sqlite_wal_mode(path: &Path) -> bool {
    use std::io::Read;
    let mut header = [0u8; 20];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .is_ok_and(|()| {
            header.starts_with(b"SQLite format 3\0") && header[18] == 2 && header[19] == 2
        })
}

/// A database path as an SQLite URI that opens it immutable: `%`, `?` and `#` are escaped, since
/// the URI gives them meaning.
#[cfg(target_os = "macos")]
fn immutable_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for c in path.to_string_lossy().chars() {
        match c {
            '%' => uri.push_str("%25"),
            '?' => uri.push_str("%3f"),
            '#' => uri.push_str("%23"),
            c => uri.push(c),
        }
    }
    uri.push_str("?immutable=1");
    uri
}

/// Whether a thread's `source` is one the Codex app does not list, by the app's own rule (its
/// catalogue builder, read in the app 26.1103.31037 on 2026-11-07, the third review of #s387): a
/// source that is `exec` (a `codex exec` run), or one that is not a string — a `subagent` object
/// (measured: `{"subagent":{"thread_spawn":…}}` and `{"subagent":{"other":"guardian"}}`) or any
/// other object — unless it is `{"custom": "<name>"}`. Every other string is listed: the app's own
/// threads (`vscode`), the CLI's (`cli`), an app-server or MCP run, and `unknown`. The app also
/// leaves out an ephemeral thread and one with a parent, which the database does not say alone, so
/// without the catalogue this is a guess, and the note says so.
#[cfg(target_os = "macos")]
fn codex_headless(source: &str) -> bool {
    if source == "exec" {
        return true;
    }
    match serde_json::from_str::<Value>(source) {
        Ok(Value::Object(object)) => !object.get("custom").is_some_and(Value::is_string),
        _ => false,
    }
}

/// Where the Codex app keeps its window state, the sidebar among it:
/// `AGENT_MONITOR_CODEX_APP_STATE` when set, else `.codex-global-state.json` beside its database.
fn codex_state_path(db: &Path) -> PathBuf {
    std::env::var_os("AGENT_MONITOR_CODEX_APP_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| db.with_file_name(".codex-global-state.json"))
}

/// The state file is a few megabytes (2.5 MB measured); anything far larger is not it.
const CODEX_STATE_LIMIT: u64 = 64 << 20;

/// The sidebar as the Codex app keeps it in its state file (measured with the Codex app
/// 26.1103.31037 on 2026-11-06): `electron-persisted-atom-state` holds `sidebar-custom-sections-v3`
/// (per account: the section order, each custom section's name and items — a project
/// `codex:project:<id>` or a thread `codex:thread:<host>:<id>` — and the collapsed custom
/// sections), `sidebar-collapsed-sections-v1` (the built-in sections' collapsed state) and one
/// `sidebar-project-expanded-v1-codex:<project id>` per project folder; the top level holds the
/// pins (`pinned-thread-ids`), the project order and names (`project-order`, `local-projects`),
/// the threads with no project (`projectless-thread-ids`), each thread's project while the app
/// has not yet moved it into its database (`thread-project-assignments`), and how the state file's
/// project ids map to the database's.
#[derive(Clone, Debug, Default, PartialEq)]
struct CodexSidebar {
    section_order: Vec<String>,
    custom: Vec<CustomSection>,
    collapsed_custom: HashSet<String>,
    collapsed_builtin: HashMap<String, bool>,
    pinned: Vec<String>,
    /// Project ids as the state file names them, in the app's order, with their names.
    projects: Vec<(String, String)>,
    /// The database's project id for a state-file project id.
    server_project: HashMap<String, String>,
    /// A thread's project (a state-file id) while the app has not moved it into its database.
    thread_project: HashMap<String, String>,
    projectless: Vec<String>,
    /// A project folder the person opened, by either of its ids.
    expanded: HashSet<String>,
    /// How threads are ordered in a project folder and in Chats (`chatSortMode` of
    /// `flat-project-sidebar-preferences-v1`): `updated_at` (the default), `created_at` or
    /// `manual` (the person's order, `sidebar-project-thread-orders`, then the newest).
    chat_sort: String,
    /// The person's thread order per project, for the `manual` mode.
    thread_orders: HashMap<String, Vec<String>>,
}

#[derive(Clone, Debug, PartialEq)]
struct CustomSection {
    id: String,
    name: String,
    items: Vec<CustomItem>,
}

#[derive(Clone, Debug, PartialEq)]
enum CustomItem {
    Project(String),
    Thread(String),
}

/// The sidebar from the state file, re-read only when the file changes. A file caught
/// half-written keeps the last good read.
fn codex_state<'a>(
    db: &Path,
    cache: &'a mut Option<CodexStateRead>,
) -> Result<&'a CodexSidebar, String> {
    let path = codex_state_path(db);
    let meta = std::fs::metadata(&path)
        .map_err(|_| "The Codex app's sidebar state was not found".to_string())?;
    let (modified, len) = (meta.modified().ok(), meta.len());
    let current = cache
        .as_ref()
        .is_some_and(|read| read.path == path && read.modified == modified && read.len == len);
    if !current {
        let parsed = (len <= CODEX_STATE_LIMIT)
            .then(|| std::fs::read(&path).ok())
            .flatten()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        match parsed {
            Some(state) => {
                let sidebar = parse_codex_sidebar(&state)?;
                *cache = Some(CodexStateRead {
                    path,
                    modified,
                    len,
                    sidebar,
                });
            }
            None if cache.as_ref().is_some_and(|read| read.path == path) => {}
            None => return Err("The Codex app's sidebar state could not be read".into()),
        }
    }
    Ok(&cache.as_ref().expect("read above").sidebar)
}

fn parse_codex_sidebar(state: &Value) -> Result<CodexSidebar, String> {
    let atoms = state
        .get("electron-persisted-atom-state")
        .and_then(Value::as_object);
    let unknown = || {
        "The Codex app's sidebar state is in a format this monitor does not know \
         (no sidebar-custom-sections-v3)"
            .to_string()
    };
    // One account's sections: the first that has a section order.
    let custom = atoms
        .and_then(|a| a.get("sidebar-custom-sections-v3"))
        .and_then(Value::as_object)
        .and_then(|accounts| accounts.values().find(|v| v.get("sectionOrder").is_some()))
        .ok_or_else(unknown)?;
    let strings = |v: Option<&Value>| -> Vec<String> {
        v.and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let sections = custom
        .get("sections")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|section| {
                    Some(CustomSection {
                        id: section.get("id")?.as_str()?.to_string(),
                        name: section
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("Section")
                            .to_string(),
                        items: strings(section.get("itemKeys"))
                            .into_iter()
                            .filter_map(|key| {
                                if let Some(id) = key.strip_prefix("codex:project:") {
                                    Some(CustomItem::Project(id.to_string()))
                                } else {
                                    // `codex:thread:<host>:<id>`: the id after the host.
                                    let rest = key.strip_prefix("codex:thread:")?;
                                    let id = rest.split_once(':').map_or(rest, |(_, id)| id);
                                    Some(CustomItem::Thread(id.to_string()))
                                }
                            })
                            .collect(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let collapsed_builtin = atoms
        .and_then(|a| a.get("sidebar-collapsed-sections-v1"))
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_bool()?)))
                .collect()
        })
        .unwrap_or_default();
    let expanded = atoms
        .map(|a| {
            a.iter()
                .filter(|(_, v)| v.as_bool() == Some(true))
                .filter_map(|(k, _)| k.strip_prefix("sidebar-project-expanded-v1-codex:"))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let names = state.get("local-projects").and_then(Value::as_object);
    let projects = strings(state.get("project-order"))
        .into_iter()
        .map(|id| {
            let name = names
                .and_then(|n| n.get(&id))
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or(&id)
                .to_string();
            (id, name)
        })
        .collect();
    // The database's project ids by the state file's, for this machine's host.
    let server_project = state
        .get("app-server-project-id-by-legacy-project-id-by-host")
        .and_then(Value::as_object)
        .and_then(|hosts| {
            hosts
                .iter()
                .find(|(host, _)| host.starts_with("local"))
                .map(|(_, v)| v)
        })
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                .collect()
        })
        .unwrap_or_default();
    let thread_project = state
        .get("thread-project-assignments")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(thread, a)| {
                    Some((thread.clone(), a.get("projectId")?.as_str()?.to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(CodexSidebar {
        section_order: strings(custom.get("sectionOrder")),
        custom: sections,
        collapsed_custom: strings(custom.get("collapsedSectionIds"))
            .into_iter()
            .collect(),
        collapsed_builtin,
        pinned: strings(state.get("pinned-thread-ids")),
        projects,
        server_project,
        thread_project,
        projectless: strings(state.get("projectless-thread-ids")),
        expanded,
        chat_sort: atoms
            .and_then(|a| a.get("flat-project-sidebar-preferences-v1"))
            .and_then(|p| p.get("chatSortMode"))
            .or_else(|| atoms.and_then(|a| a.get("thread-sort-key")))
            .and_then(Value::as_str)
            .unwrap_or("updated_at")
            .to_string(),
        thread_orders: state
            .get("sidebar-project-thread-orders")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(project, ids)| (project.clone(), strings(Some(ids))))
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// The Codex app's sidebar: its sections in its order — Pinned, each custom section with the
/// projects and threads the person put in it, Projects (every other project folder, in the app's
/// project order) and Chats (threads with no project) — each collapsed as the app has it, a
/// project folder closed unless the person opened it. Threads are the database's (titles,
/// recency, archive, sub-agents); an archived thread is not drawn, as the app does not draw it.
fn codex_sidebar_layout(data: &CodexData, sidebar: &CodexSidebar) -> Layout {
    // Only the threads the app lists (its catalogue): not `codex exec` runs, not sub-agents.
    let threads: Vec<&CodexThread> = data.threads.iter().filter(|t| data.lists(t)).collect();
    let by_id: HashMap<&str, &CodexThread> = threads.iter().map(|t| (t.id.as_str(), *t)).collect();
    let mut kids: HashMap<&str, Vec<&CodexThread>> = HashMap::new();
    for &t in &threads {
        if let Some(parent) = t
            .parent
            .as_deref()
            .filter(|p| *p != t.id && by_id.contains_key(p))
        {
            kids.entry(parent).or_default().push(t);
        }
    }
    let is_child = |t: &CodexThread| {
        t.parent
            .as_deref()
            .is_some_and(|p| p != t.id && by_id.contains_key(p))
    };
    // A project by the state file's id; the database names it by its own.
    let state_id: HashMap<&str, &str> = sidebar
        .server_project
        .iter()
        .map(|(state, server)| (server.as_str(), state.as_str()))
        .collect();
    let project_of = |t: &CodexThread| -> Option<String> {
        sidebar.thread_project.get(&t.id).cloned().or_else(|| {
            let server = t.project.as_deref()?;
            Some(state_id.get(server).copied().unwrap_or(server).to_string())
        })
    };
    let mut by_project: HashMap<String, Vec<&CodexThread>> = HashMap::new();
    for &t in &threads {
        if t.archived || is_child(t) {
            continue;
        }
        if let Some(project) = project_of(t) {
            by_project.entry(project).or_default().push(t);
        }
    }
    let recent = |a: &&CodexThread, b: &&CodexThread| {
        b.recency.cmp(&a.recency).then_with(|| a.id.cmp(&b.id))
    };
    // A thread the state file names: its database row, or its id alone where the database has none.
    let thread_entry = |id: &str| -> Option<Entry> {
        match by_id.get(id) {
            Some(t) if t.archived => None,
            Some(t) => Some(codex_entry(t, &kids, &mut HashSet::new())),
            // Not in the database: kept by its id, unless the app's catalogue does not list it.
            None if data.listed.as_ref().is_some_and(|l| !l.contains(id)) => None,
            None => Some(Entry {
                id: id.to_string(),
                ..Default::default()
            }),
        }
    };
    let name_of = |project: &str| {
        sidebar
            .projects
            .iter()
            .find(|(id, _)| id == project)
            .map(|(_, name)| name.clone())
            .or_else(|| {
                let server = sidebar
                    .server_project
                    .get(project)
                    .map_or(project, String::as_str);
                data.projects
                    .iter()
                    .find(|(id, _)| id == server)
                    .map(|(_, name)| name.clone())
            })
            .unwrap_or_else(|| "Project".to_string())
    };
    // Threads in a folder and in Chats, in the app's chats order.
    let arrange = |threads: &mut Vec<&CodexThread>, project: Option<&str>| {
        threads.sort_by(recent);
        match sidebar.chat_sort.as_str() {
            "created_at" => {
                threads.sort_by(|a, b| b.created.cmp(&a.created).then_with(|| recent(a, b)))
            }
            "manual" => {
                let order = project.and_then(|p| sidebar.thread_orders.get(p));
                let at = |t: &CodexThread| order.and_then(|o| o.iter().position(|id| *id == t.id));
                threads.sort_by_key(|t| at(t).unwrap_or(usize::MAX));
            }
            _ => {}
        }
    };
    let folder = |project: &str| {
        let mut threads = by_project.get(project).cloned().unwrap_or_default();
        arrange(&mut threads, Some(project));
        let server = sidebar.server_project.get(project);
        let open = sidebar.expanded.contains(project)
            || server.is_some_and(|s| sidebar.expanded.contains(s));
        Group {
            id: project.to_string(),
            label: name_of(project),
            collapsed: !open,
            keep: true,
            entries: threads
                .into_iter()
                .map(|t| codex_entry(t, &kids, &mut HashSet::new()))
                .collect(),
        }
    };
    let in_custom: HashSet<&str> = sidebar
        .custom
        .iter()
        .flat_map(|section| section.items.iter())
        .filter_map(|item| match item {
            CustomItem::Project(id) => Some(id.as_str()),
            CustomItem::Thread(_) => None,
        })
        .collect();
    let builtin = |key: &str| sidebar.collapsed_builtin.get(key).copied().unwrap_or(false);
    let mut out = Vec::new();
    let mut unknown = Vec::new();
    for key in &sidebar.section_order {
        match key.as_str() {
            "pinned" => out.push(Section {
                id: "pinned".into(),
                label: "Pinned".into(),
                collapsed: builtin("pinned"),
                entries: sidebar
                    .pinned
                    .iter()
                    .filter_map(|id| thread_entry(id))
                    .collect(),
                groups: Vec::new(),
            }),
            "threads" => out.push(Section {
                id: "threads".into(),
                label: "Projects".into(),
                collapsed: builtin("threads"),
                entries: Vec::new(),
                groups: sidebar
                    .projects
                    .iter()
                    .filter(|(id, _)| !in_custom.contains(id.as_str()))
                    .map(|(id, _)| folder(id))
                    .collect(),
            }),
            "chats" => {
                let mut chats: Vec<&CodexThread> = sidebar
                    .projectless
                    .iter()
                    .filter_map(|id| by_id.get(id.as_str()).copied())
                    .filter(|t| !t.archived && !is_child(t))
                    .collect();
                arrange(&mut chats, None);
                out.push(Section {
                    id: "chats".into(),
                    label: "Chats".into(),
                    collapsed: builtin("chats"),
                    entries: chats
                        .into_iter()
                        .map(|t| codex_entry(t, &kids, &mut HashSet::new()))
                        .collect(),
                    groups: Vec::new(),
                });
            }
            custom if custom.starts_with("custom:") => {
                let id = &custom["custom:".len()..];
                let Some(section) = sidebar.custom.iter().find(|s| s.id == id) else {
                    continue;
                };
                let mut entries = Vec::new();
                let mut groups = Vec::new();
                for item in &section.items {
                    match item {
                        CustomItem::Thread(thread) => entries.extend(thread_entry(thread)),
                        CustomItem::Project(project) => groups.push(folder(project)),
                    }
                }
                out.push(Section {
                    id: format!("custom:{id}"),
                    label: section.name.clone(),
                    collapsed: sidebar.collapsed_custom.contains(id),
                    entries,
                    groups,
                });
            }
            // Not session lists: the app's own home items.
            "orbit" | "agents" | "cloud" => {}
            other => unknown.push(other.to_string()),
        }
    }
    let note = (!unknown.is_empty()).then(|| {
        format!(
            "The Codex app has sidebar sections this monitor does not know ({}); they are not shown",
            unknown.join(", ")
        )
    });
    Layout {
        sections: out,
        note: join_notes(note, data.note()),
    }
}

/// Two notes as one sentence each, either possibly absent.
fn join_notes(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(format!("{a}. {b}")),
        (a, b) => a.or(b),
    }
}

/// A transcript path as the monitor's row id: its file name without `.jsonl` (or `.jsonl.zst`).
/// The monitor names a Codex session by its rollout's name, a Claude session by its transcript's.
fn rollout_row_id(path: &str) -> Option<String> {
    let name = Path::new(path.trim()).file_name()?.to_str()?;
    let name = name.strip_suffix(".zst").unwrap_or(name);
    let name = name.strip_suffix(".jsonl").unwrap_or(name);
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(not(target_os = "macos"))]
fn read_codex(_db: &Path) -> Result<CodexData, String> {
    Err("The Codex app runs on macOS only".into())
}

fn codex_layout(data: &CodexData) -> Layout {
    // Only the threads the app lists (its catalogue): not `codex exec` runs, not sub-agents.
    let threads: Vec<&CodexThread> = data.threads.iter().filter(|t| data.lists(t)).collect();
    let by_id: HashMap<&str, &CodexThread> = threads.iter().map(|t| (t.id.as_str(), *t)).collect();
    let mut kids: HashMap<&str, Vec<&CodexThread>> = HashMap::new();
    let mut tops: Vec<&CodexThread> = Vec::new();
    for &t in &threads {
        match t
            .parent
            .as_deref()
            .filter(|p| *p != t.id && by_id.contains_key(p))
        {
            Some(parent) => kids.entry(parent).or_default().push(t),
            None => tops.push(t),
        }
    }
    let recent = |a: &&CodexThread, b: &&CodexThread| {
        b.recency.cmp(&a.recency).then_with(|| a.id.cmp(&b.id))
    };
    let entries = |mut chosen: Vec<&CodexThread>| {
        chosen.sort_by(recent);
        chosen
            .into_iter()
            .map(|t| codex_entry(t, &kids, &mut HashSet::new()))
            .collect::<Vec<Entry>>()
    };
    let known_sections: HashSet<&str> = data.sections.iter().map(|(id, _)| id.as_str()).collect();
    let known_projects: HashSet<&str> = data.projects.iter().map(|(id, _)| id.as_str()).collect();
    let live: Vec<&CodexThread> = tops.iter().copied().filter(|t| !t.archived).collect();
    let in_section = |t: &CodexThread| {
        t.section
            .as_deref()
            .is_some_and(|s| known_sections.contains(s))
    };
    let mut out = Vec::new();
    out.push(Section {
        id: "pinned".into(),
        label: "Pinned".into(),
        collapsed: false,
        entries: entries(live.iter().copied().filter(|t| t.pinned).collect()),
        groups: Vec::new(),
    });
    for (id, name) in &data.sections {
        let mut chosen: Vec<&CodexThread> = live
            .iter()
            .copied()
            .filter(|t| !t.pinned && t.section.as_deref() == Some(id.as_str()))
            .collect();
        chosen.sort_by(|a, b| match (a.section_position, b.section_position) {
            (Some(x), Some(y)) => x.cmp(&y).then_with(|| recent(a, b)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => recent(a, b),
        });
        out.push(Section {
            id: format!("section:{id}"),
            label: name.clone(),
            collapsed: false,
            entries: chosen
                .into_iter()
                .map(|t| codex_entry(t, &kids, &mut HashSet::new()))
                .collect(),
            groups: Vec::new(),
        });
    }
    let groups = data
        .projects
        .iter()
        .map(|(id, name)| Group {
            id: id.clone(),
            label: name.clone(),
            collapsed: false,
            keep: false,
            entries: entries(
                live.iter()
                    .copied()
                    .filter(|t| {
                        !t.pinned && !in_section(t) && t.project.as_deref() == Some(id.as_str())
                    })
                    .collect(),
            ),
        })
        .collect();
    out.push(Section {
        id: "threads".into(),
        label: "Threads".into(),
        collapsed: false,
        entries: Vec::new(),
        groups,
    });
    out.push(Section {
        id: "archived".into(),
        label: "Archived".into(),
        collapsed: true,
        entries: entries(tops.iter().copied().filter(|t| t.archived).collect()),
        groups: Vec::new(),
    });
    let unplaced = live
        .iter()
        .filter(|t| {
            !t.pinned
                && !in_section(t)
                && !t
                    .project
                    .as_deref()
                    .is_some_and(|p| known_projects.contains(p))
        })
        .count();
    let note = (unplaced > 0).then(|| {
        format!(
            "The Codex app is moving thread projects into its database: {unplaced} thread{} \
             {} no project there yet and {} under “Not in the Codex app”",
            if unplaced == 1 { "" } else { "s" },
            if unplaced == 1 { "has" } else { "have" },
            if unplaced == 1 { "is" } else { "are" },
        )
    });
    Layout {
        sections: out,
        note: join_notes(note, data.note()),
    }
}

fn codex_entry(
    t: &CodexThread,
    kids: &HashMap<&str, Vec<&CodexThread>>,
    seen: &mut HashSet<String>,
) -> Entry {
    seen.insert(t.id.clone());
    let mut children: Vec<&CodexThread> = kids
        .get(t.id.as_str())
        .map(|k| {
            k.iter()
                .copied()
                .filter(|c| !seen.contains(&c.id))
                .collect()
        })
        .unwrap_or_default();
    children.sort_by(|a, b| b.recency.cmp(&a.recency).then_with(|| a.id.cmp(&b.id)));
    Entry {
        id: t.row.clone(),
        title: t.title.clone(),
        pane: None,
        children: children
            .into_iter()
            .map(|c| codex_entry(c, kids, seen))
            .collect(),
    }
}

// ---------------------------------------------------------------------------------------------
// Herdr (design/layout-sync.md §3.3), through its supported CLI: `herdr session list --json` for
// the servers, then `herdr api snapshot` against each running one. One section per workspace, a
// group per tab, a row per pane whose agent reported its session (Herdr's agent hooks).
// ---------------------------------------------------------------------------------------------

/// Each running Herdr server's name and snapshot. `AGENT_MONITOR_HERDR` names the program, as
/// for every other herdr step; a unit test never runs the real one.
fn read_herdr() -> Result<Vec<(String, Value)>, String> {
    if cfg!(test) && std::env::var_os("AGENT_MONITOR_HERDR").is_none() {
        return Err("Herdr is not installed on this machine".into());
    }
    let program = crate::control::herdr_program();
    let list =
        crate::control::herdr_run(&program, None, &["session", "list", "--json"]).map_err(|e| {
            let missing = e.chain().any(|cause| {
                cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
            });
            if missing {
                "Herdr is not installed on this machine".to_string()
            } else {
                format!("Herdr's server list could not be read: {e}")
            }
        })?;
    let list: Value = serde_json::from_str(&list)
        .map_err(|_| "Herdr's server list is in a format this monitor does not know".to_string())?;
    let servers: Vec<(String, String)> = list
        .get("sessions")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|s| s.get("running").and_then(Value::as_bool) == Some(true))
                .filter_map(|s| {
                    Some((
                        s.get("name")?.as_str()?.to_string(),
                        s.get("socket_path")?.as_str()?.to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if servers.is_empty() {
        return Err("No Herdr server is running".into());
    }
    let mut out = Vec::new();
    let mut failed = Vec::new();
    for (name, socket) in servers {
        let snapshot = crate::control::herdr_socket_ok(&socket)
            .and_then(|socket| {
                crate::control::herdr_run(&program, Some(&socket), &["api", "snapshot"])
            })
            .ok()
            .and_then(|reply| serde_json::from_str::<Value>(&reply).ok());
        match snapshot {
            Some(snapshot) => out.push((name, snapshot)),
            None => failed.push(name),
        }
    }
    if out.is_empty() {
        return Err(format!(
            "Herdr's running server{} could not be read ({})",
            if failed.len() == 1 { "" } else { "s" },
            failed.join(", ")
        ));
    }
    Ok(out)
}

/// A pane's agent session as the monitor's row id: Herdr's hooks report a transcript's path, named
/// as the monitor names the row, or a session id, which the page matches to a row whose id ends
/// with it (a Codex row is named by its rollout, `rollout-<time>-<id>`).
fn herdr_session_id(session: &Value) -> Option<String> {
    let value = session.get("value")?.as_str()?.trim();
    if value.is_empty() {
        return None;
    }
    if session.get("kind").and_then(Value::as_str) == Some("path") {
        return rollout_row_id(value);
    }
    Some(value.to_string())
}

fn herdr_layout(servers: &[(String, Value)]) -> Layout {
    let many = servers.len() > 1;
    let number = |v: &Value| v.get("number").and_then(Value::as_i64).unwrap_or(i64::MAX);
    let text = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
    };
    let list = |v: &Value, key: &str| {
        v.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut out = Vec::new();
    for (name, reply) in servers {
        let snap = reply
            .pointer("/result/snapshot")
            .or_else(|| reply.get("snapshot"))
            .unwrap_or(reply);
        // A pane's session, from the pane or else from the agents list naming it.
        let agents: HashMap<String, Value> = list(snap, "agents")
            .into_iter()
            .filter_map(|a| Some((text(&a, "pane_id")?, a.get("agent_session")?.clone())))
            .collect();
        let mut workspaces = list(snap, "workspaces");
        workspaces.sort_by_key(number);
        let mut tabs = list(snap, "tabs");
        tabs.sort_by_key(number);
        let panes = list(snap, "panes");
        for ws in &workspaces {
            let Some(ws_id) = text(ws, "workspace_id") else {
                continue;
            };
            let ws_label = text(ws, "label").unwrap_or_else(|| format!("Workspace {}", number(ws)));
            let groups = tabs
                .iter()
                .filter(|t| text(t, "workspace_id").as_deref() == Some(ws_id.as_str()))
                .filter_map(|tab| {
                    let tab_id = text(tab, "tab_id")?;
                    let entries = panes
                        .iter()
                        .filter(|p| text(p, "tab_id").as_deref() == Some(tab_id.as_str()))
                        .filter_map(|p| {
                            let session = p
                                .get("agent_session")
                                .filter(|s| !s.is_null())
                                .or_else(|| text(p, "pane_id").and_then(|id| agents.get(&id)))?;
                            Some(Entry {
                                id: herdr_session_id(session)?,
                                title: text(p, "label"),
                                pane: text(p, "pane_id").map(|pane| format!("{name}|{pane}")),
                                children: Vec::new(),
                            })
                        })
                        .collect();
                    Some(Group {
                        id: tab_id,
                        label: text(tab, "label").unwrap_or_else(|| format!("Tab {}", number(tab))),
                        collapsed: false,
                        keep: false,
                        entries,
                    })
                })
                .collect();
            out.push(Section {
                id: format!("{name}/{ws_id}"),
                label: if many {
                    format!("{name} · {ws_label}")
                } else {
                    ws_label
                },
                collapsed: false,
                entries: Vec::new(),
                groups,
            });
        }
    }
    Layout {
        sections: out,
        note: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(desktop: &str, cli: &str, minutes_ago: i64) -> AppRecord {
        AppRecord {
            desktop: desktop.into(),
            cli: cli.into(),
            scope: "acct/org".into(),
            title: Some(format!("title {cli}")),
            starter: None,
            starred: false,
            archived: false,
            folder: Some("/w/harbor".into()),
            last_activity: 1_000_000 - minutes_ago * 60_000,
            created: 0,
        }
    }

    fn ids(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|e| {
                if e.children.is_empty() {
                    e.id.clone()
                } else {
                    format!("{}[{}]", e.id, ids(&e.children).join(","))
                }
            })
            .collect()
    }

    /// The Claude app's settings in the measured shape (synthetic ids and names).
    fn prefs() -> Value {
        json!({
            "starred-local-code-sessions": ["local_pin"],
            "dframe-local-slice": { "pinnedOrder": [] },
            "dframe-group-scopes": { "acct/org": {
                "groups": [
                    { "id": "cg-plan", "name": "Planning", "mode": "code" },
                    { "id": "cg-rev", "name": "Reviews", "mode": "code" },
                ],
                "assignments": {
                    "code:local_pin": "cg-plan",
                    "code:local_plan1": "cg-plan",
                    "code:local_plan2": "cg-plan",
                    "code:local_rev": "cg-rev",
                    "code:local_side": "cg-rev",
                    "code:local_gone": "cg-deleted",
                },
                "order": { "cg-plan": ["code:local_plan2", "code:local_plan1"] },
            }},
            "dframe-code-sections": { "acct/org": { "sections": [
                { "id": "sessions", "kind": "sessions", "order": 6, "sortBy": "recency", "ascending": false, "collapsed": false, "shown": true },
                { "id": "pinned", "kind": "pinned", "order": 0, "sortBy": "recency", "ascending": false, "collapsed": false, "shown": true },
                { "id": "routines", "kind": "routines", "order": 1, "collapsed": true, "shown": false },
                { "id": "cg-plan", "kind": "manual", "name": "Planning", "order": 2, "sortBy": "manual", "ascending": false, "collapsed": false, "shown": true },
                { "id": "cg-rev", "kind": "manual", "name": "Reviews", "order": 3, "sortBy": "recency", "ascending": false, "collapsed": true, "shown": true },
                { "id": "cg-off", "kind": "manual", "name": "Off", "order": 4, "shown": false },
                { "id": "x-new", "kind": "smart", "order": 5, "shown": true },
            ]}},
        })
    }

    /// #s387: the Claude app's sidebar — Pinned first (a pinned session keeps its group but is
    /// drawn under Pinned), each group in the section order and its own order, side sessions under
    /// their starter whatever they are filed under, the unfiled by project, archived at the end,
    /// and a section of a kind not known reported rather than guessed.
    #[test]
    fn the_claude_app_layout_follows_its_sections_groups_pins_and_side_sessions() {
        let mut records = vec![
            record("local_pin", "pin", 50),
            record("local_plan1", "plan1", 1),
            record("local_plan2", "plan2", 30),
            record("local_rev", "rev", 5),
            record("local_loose", "loose", 2),
            record("local_old", "old", 900),
            record("local_gone", "gone", 3),
            record("local_arch", "arch", 4),
            record("local_side", "side", 0),
            record("local_btw", "btw", 7),
        ];
        records[5].folder = Some("/w/atlas".into());
        records[7].archived = true;
        // Filed under Reviews itself, but started from a Planning session: drawn under it.
        records[8].starter = Some("local_plan1".into());
        records[9].starter = Some("local_side".into());
        // Another account's session is not this sidebar's.
        let mut other = record("local_x", "x", 0);
        other.scope = "acct/other".into();
        records.push(other);
        let mut scoped = records.clone();
        scoped[10].last_activity = 0;
        let layout = claude_layout(&prefs(), &scoped).unwrap();
        let shown: Vec<(&str, &str, bool, Vec<String>)> = layout
            .sections
            .iter()
            .map(|s| {
                let mut all = ids(&s.entries);
                for g in &s.groups {
                    all.push(format!("{}:{}", g.label, ids(&g.entries).join(",")));
                }
                (s.id.as_str(), s.label.as_str(), s.collapsed, all)
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                ("pinned", "Pinned", false, vec!["pin".to_string()]),
                (
                    "cg-plan",
                    "Planning",
                    false,
                    vec!["plan2".into(), "plan1[side[btw]]".into()]
                ),
                ("cg-rev", "Reviews", true, vec!["rev".into()]),
                (
                    "sessions",
                    "Sessions",
                    false,
                    vec!["harbor:loose,gone".into(), "atlas:old".into()]
                ),
                ("archived", "Archived", true, vec!["arch".into()]),
            ]
        );
        assert_eq!(
            layout.sections[0].entries[0].title.as_deref(),
            Some("title pin")
        );
        assert!(
            layout.note.as_deref().unwrap().contains("(smart)"),
            "{:?}",
            layout.note
        );
        // With Pinned switched off, a pinned session stays in its group.
        let mut off = prefs();
        off["dframe-code-sections"]["acct/org"]["sections"][1]["shown"] = json!(false);
        let layout = claude_layout(&off, &scoped).unwrap();
        assert_eq!(layout.sections[0].id, "cg-plan");
        assert_eq!(
            ids(&layout.sections[0].entries),
            vec!["plan2", "plan1[side[btw]]", "pin"]
        );
        // The account shown is the one with the most recent session.
        let mut newest = records.clone();
        newest[10].last_activity = 2_000_000;
        assert!(claude_layout(&prefs(), &newest)
            .unwrap()
            .sections
            .iter()
            .all(|s| s.groups.is_empty() || ids(&s.groups[0].entries) == ["x"]));
        assert_eq!(
            claude_layout(&json!({}), &[]),
            Err("The Claude app has no Code sessions on this machine".to_string())
        );
    }

    /// #s387: a section sorted by name or by date created, as the app names them (`alpha`,
    /// `created`), ascending or descending.
    #[test]
    fn the_claude_app_layout_sorts_a_section_by_name_or_date_created() {
        let mut a = record("local_a", "a", 1);
        let mut b = record("local_b", "b", 3);
        let mut c = record("local_c", "c", 2);
        (a.title, b.title, c.title) = (
            Some("Beta".into()),
            Some("alpha".into()),
            Some("Gamma".into()),
        );
        (a.created, b.created, c.created) = (30, 10, 20);
        let records = vec![a, b, c];
        let prefs = |sort: &str, ascending: bool| {
            json!({
                "dframe-group-scopes": { "acct/org": {
                    "groups": [{ "id": "cg-x", "name": "X" }],
                    "assignments": { "code:local_a": "cg-x", "code:local_b": "cg-x", "code:local_c": "cg-x" },
                }},
                "dframe-code-sections": { "acct/org": { "sections": [
                    { "id": "cg-x", "kind": "manual", "order": 0, "sortBy": sort, "ascending": ascending },
                ]}},
            })
        };
        let order = |sort: &str, ascending: bool| {
            ids(&claude_layout(&prefs(sort, ascending), &records)
                .unwrap()
                .sections[0]
                .entries)
        };
        assert_eq!(order("recency", false), vec!["a", "c", "b"]);
        assert_eq!(order("alpha", true), vec!["b", "a", "c"]);
        assert_eq!(order("alpha", false), vec!["c", "a", "b"]);
        assert_eq!(order("created", false), vec!["a", "c", "b"]);
        assert_eq!(order("created", true), vec!["b", "c", "a"]);
    }

    /// #s387: records and settings are read from the app's files, read-only and tolerantly, and a
    /// settings file caught half-written keeps the last good read.
    #[test]
    fn the_claude_app_reader_reads_its_records_and_settings_from_disk() {
        let root = std::env::temp_dir().join(format!("cm-layout-claude-{}", std::process::id()));
        let store = root.join("store");
        let org = store.join("acct").join("org");
        std::fs::create_dir_all(&org).unwrap();
        let write_record = |id: &str, cli: &str, extra: Value| {
            let mut v = json!({
                "sessionId": id, "cliSessionId": cli, "title": format!("t {cli}"),
                "titleSource": "auto", "isArchived": false, "cwd": "/w/harbor",
                "lastActivityAt": 1_000, "unknownKey": [1, 2],
            });
            for (k, x) in extra.as_object().unwrap() {
                v[k] = x.clone();
            }
            std::fs::write(org.join(format!("{id}.json")), v.to_string()).unwrap();
        };
        write_record("local_a", "aaaa", json!({ "isStarred": true }));
        write_record(
            "local_b",
            "bbbb",
            json!({ "spawnedFrom": { "sessionId": "local_a" } }),
        );
        std::fs::write(org.join("local_bad.json"), "{nope").unwrap();
        let config = root.join("claude_desktop_config.json");
        let settings = json!({ "preferences": { "epitaxyPrefs": {
            "dframe-code-sections": { "acct/org": { "sections": [
                { "id": "pinned", "kind": "pinned", "order": 0 },
                { "id": "sessions", "kind": "sessions", "order": 1 },
            ]}},
        }}});
        std::fs::write(&config, settings.to_string()).unwrap();
        let mut readers = Readers::default();
        let layout = {
            let prefs = readers.claude_prefs(&config).unwrap();
            let records = readers.claude_records.records(&store);
            claude_layout(&prefs, &records).unwrap()
        };
        assert_eq!(ids(&layout.sections[0].entries), vec!["aaaa[bbbb]"]);
        // Half-written: the last good settings stand.
        std::fs::write(&config, "{\"preferences\": {").unwrap();
        assert!(readers.claude_prefs(&config).is_ok());
        // A settings file with no layout says so.
        let mut fresh = Readers::default();
        std::fs::write(&config, json!({ "preferences": {} }).to_string()).unwrap();
        assert!(fresh
            .claude_prefs(&config)
            .unwrap_err()
            .contains("epitaxyPrefs"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// #s387: the Codex app's pinned threads, its sections in their order, threads by project in
    /// the project order, archived last, sub-agents under their parent, and a count of the threads
    /// its migration has not yet given a project.
    #[test]
    fn the_codex_app_layout_follows_its_pins_sections_projects_and_sub_agents() {
        let t = |id: &str, recency: i64| CodexThread {
            id: id.into(),
            row: id.into(),
            title: Some(format!("thread {id}")),
            recency,
            ..Default::default()
        };
        let data = CodexData {
            projects: vec![
                ("p2".into(), "notes-cli".into()),
                ("p1".into(), "atlas".into()),
            ],
            sections: vec![("s1".into(), "Release".into())],
            threads: vec![
                CodexThread {
                    pinned: true,
                    project: Some("p1".into()),
                    ..t("pin", 1)
                },
                CodexThread {
                    section: Some("s1".into()),
                    section_position: Some(2),
                    ..t("rel-b", 9)
                },
                CodexThread {
                    section: Some("s1".into()),
                    section_position: Some(1),
                    ..t("rel-a", 1)
                },
                CodexThread {
                    project: Some("p1".into()),
                    ..t("a-old", 1)
                },
                CodexThread {
                    project: Some("p1".into()),
                    ..t("a-new", 5)
                },
                CodexThread {
                    project: Some("p2".into()),
                    ..t("n", 3)
                },
                CodexThread {
                    parent: Some("a-new".into()),
                    ..t("sub", 8)
                },
                CodexThread {
                    archived: true,
                    project: Some("p2".into()),
                    ..t("arch", 2)
                },
                t("loose", 4),
            ],
            ..Default::default()
        };
        let layout = codex_layout(&data);
        let shown: Vec<(String, Vec<String>)> = layout
            .sections
            .iter()
            .map(|s| {
                let mut all = ids(&s.entries);
                for g in &s.groups {
                    all.push(format!("{}:{}", g.label, ids(&g.entries).join(",")));
                }
                (s.label.clone(), all)
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                ("Pinned".to_string(), vec!["pin".to_string()]),
                ("Release".into(), vec!["rel-a".into(), "rel-b".into()]),
                (
                    "Threads".into(),
                    vec!["notes-cli:n".into(), "atlas:a-new[sub],a-old".into()]
                ),
                ("Archived".into(), vec!["arch".into()]),
            ]
        );
        assert!(layout.sections[3].collapsed);
        assert!(
            layout
                .note
                .as_deref()
                .unwrap()
                .contains("1 thread has no project"),
            "{:?}",
            layout.note
        );
    }

    /// #s387: the Codex reader reads the app-server database read-only, in its measured shape.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_codex_app_reader_reads_the_database_read_only() {
        let root = std::env::temp_dir().join(format!("cm-layout-codex-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL, metadata TEXT NOT NULL DEFAULT '{}', position INTEGER NOT NULL, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL);
             CREATE TABLE thread_sections (id TEXT PRIMARY KEY, name TEXT NOT NULL, appearance TEXT);
             CREATE TABLE thread_spawn_edges (parent_thread_id TEXT NOT NULL, child_thread_id TEXT NOT NULL PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, title TEXT NOT NULL, updated_at INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER, recency_at_ms INTEGER NOT NULL DEFAULT 0, name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0, thread_section_id TEXT, section_position INTEGER, project_id TEXT, created_at INTEGER NOT NULL DEFAULT 0, created_at_ms INTEGER, source TEXT NOT NULL);
             INSERT INTO projects VALUES ('p1', 'atlas', '{}', 0, 0, 0);
             INSERT INTO thread_sections VALUES ('s1', 'Release', NULL);
             INSERT INTO threads VALUES ('t1', '/h/.codex/sessions/2026/11/06/rollout-2026-11-06T01-02-03-t1.jsonl.zst', 'first prompt', 1, 0, NULL, 5000, 'Renamed', 0, NULL, NULL, 'p1', 1, NULL, 'vscode');
             INSERT INTO threads VALUES ('t2', '', 'child', 1, 0, NULL, 4000, NULL, 0, NULL, NULL, NULL, 1, NULL, '{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"t1\",\"depth\":1}}}');
             INSERT INTO threads VALUES ('t3', '/h/rollout-x-t3.jsonl', 'release', 1, 0, NULL, 0, '', 0, 's1', 1, NULL, 1, NULL, 'cli');
             INSERT INTO threads VALUES ('t4', '/h/rollout-y-t4.jsonl', 'a codex exec run', 1, 0, NULL, 3000, NULL, 0, NULL, NULL, 'p1', 1, NULL, 'exec');
             INSERT INTO thread_spawn_edges VALUES ('t1', 't2', 'open');",
        )
        .unwrap();
        drop(conn);
        let made = |root: &Path| -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(root)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with("-shm") || n.ends_with("-wal"))
                .collect();
            names.sort();
            names
        };
        assert!(made(&root).is_empty(), "the fixture left {:?}", made(&root));
        // No catalogue beside it (an app that keeps none): a `codex exec` run and a sub-agent are
        // told apart by their source, and the note says the list is a guess.
        let data = read_codex(&db).unwrap();
        // The second review of #s387: a WAL database is read without making its `-shm` or `-wal`.
        assert!(made(&root).is_empty(), "the read made {:?}", made(&root));
        let layout = codex_layout(&data);
        assert_eq!(
            ids(&layout.sections[2].groups[0].entries),
            vec!["rollout-2026-11-06T01-02-03-t1"]
        );
        assert!(
            layout.note.as_deref().unwrap().contains("was not found"),
            "{:?}",
            layout.note
        );
        // With the app's catalogue, it decides: here it lists the sub-agent too.
        std::fs::create_dir_all(root.join("sqlite")).unwrap();
        let catalogue = rusqlite::Connection::open(root.join("sqlite/codex.db")).unwrap();
        catalogue
            .execute_batch(
                "PRAGMA journal_mode=WAL;
                 CREATE TABLE local_thread_catalog (host_id TEXT NOT NULL, thread_id TEXT NOT NULL);
                 INSERT INTO local_thread_catalog VALUES ('local', 't1'), ('local', 't2'), ('local', 't3');",
            )
            .unwrap();
        drop(catalogue);
        let data = read_codex(&db).unwrap();
        assert!(made(&root.join("sqlite")).is_empty());
        let layout = codex_layout(&data);
        // A row is named by its rollout, as the monitor names it; a thread with none, by its id.
        assert_eq!(ids(&layout.sections[1].entries), vec!["rollout-x-t3"]);
        assert_eq!(
            ids(&layout.sections[2].groups[0].entries),
            vec!["rollout-2026-11-06T01-02-03-t1[t2]"]
        );
        assert_eq!(
            layout.sections[2].groups[0].entries[0].title.as_deref(),
            Some("Renamed")
        );
        assert_eq!(
            layout.sections[1].entries[0].title.as_deref(),
            Some("release")
        );
        assert!(layout.note.is_none(), "{:?}", layout.note);
        // A database of another shape says so rather than guessing.
        let other = root.join("other.sqlite");
        rusqlite::Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE projects (id TEXT, name TEXT, position INTEGER); CREATE TABLE thread_sections (id TEXT, name TEXT);")
            .unwrap();
        assert!(read_codex(&other).unwrap_err().contains("does not know"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The Codex app's state file in its measured shape (synthetic ids and names).
    fn codex_state_json() -> Value {
        json!({
            "electron-persisted-atom-state": {
                "sidebar-custom-sections-v3": { "acct-1": {
                    "sections": [
                        { "id": "cs-work", "name": "Work", "hostSectionIds": { "local": "db-s1" },
                          "itemKeys": ["codex:project:lp-api", "codex:project:lp-web"], "appearance": null },
                        { "id": "cs-agent", "name": "Agent chats", "hostSectionIds": { "local": "db-s2" },
                          "itemKeys": ["codex:thread:local:t-chat2", "codex:thread:local:t-gone", "codex:thread:local:t-arch"], "appearance": null },
                    ],
                    "collapsedSectionIds": ["cs-agent"],
                    "sectionOrder": ["pinned", "orbit", "agents", "custom:cs-work", "custom:cs-agent", "threads", "chats", "x-new"],
                }},
                "sidebar-collapsed-sections-v1": { "pinned": true, "threads": false, "chats": false },
                "sidebar-project-expanded-v1-codex:srv-web": true,
                "sidebar-project-expanded-v1-codex:lp-docs": false,
                "thread-client-id-v1:local%3Ax": true,
            },
            "pinned-thread-ids": ["t-pin"],
            "project-order": ["lp-api", "lp-web", "lp-docs", "lp-empty"],
            "local-projects": {
                "lp-api": { "id": "lp-api", "name": "api" },
                "lp-web": { "id": "lp-web", "name": "web" },
                "lp-docs": { "id": "lp-docs", "name": "docs" },
                "lp-empty": { "id": "lp-empty", "name": "empty" },
            },
            "app-server-project-id-by-legacy-project-id-by-host": {
                "local:/h/.codex": { "lp-api": "srv-api", "lp-web": "srv-web", "lp-docs": "srv-docs", "lp-empty": "srv-empty" },
            },
            "thread-project-assignments": { "t-moved": { "projectId": "lp-docs", "projectKind": "local" } },
            "projectless-thread-ids": ["t-chat"],
        })
    }

    /// #s387: the Codex app's sidebar as its state file keeps it — Pinned (collapsed as the app
    /// has it), each custom section with its project folders or threads, Projects (every other
    /// folder, in the app's order, empty ones too), Chats — a folder closed unless the person
    /// opened it, a thread's project from the state file while the app has not moved it into its
    /// database, archived threads not drawn, and a section kind not known reported.
    #[test]
    fn the_codex_app_layout_follows_its_sidebar_state() {
        let t = |id: &str, recency: i64| CodexThread {
            id: id.into(),
            row: format!("rollout-{id}"),
            title: Some(format!("thread {id}")),
            recency,
            ..Default::default()
        };
        let data = CodexData {
            projects: vec![
                ("srv-api".into(), "api".into()),
                ("srv-web".into(), "web".into()),
            ],
            sections: vec![],
            threads: vec![
                CodexThread {
                    project: Some("srv-api".into()),
                    ..t("t-pin", 9)
                },
                CodexThread {
                    project: Some("srv-api".into()),
                    ..t("t-a1", 3)
                },
                CodexThread {
                    project: Some("srv-api".into()),
                    ..t("t-a2", 5)
                },
                CodexThread {
                    project: Some("srv-web".into()),
                    ..t("t-w", 1)
                },
                CodexThread {
                    parent: Some("t-w".into()),
                    ..t("t-sub", 2)
                },
                // The database still names the old project; the state file has moved it.
                CodexThread {
                    project: Some("srv-api".into()),
                    ..t("t-moved", 4)
                },
                CodexThread {
                    archived: true,
                    project: Some("srv-web".into()),
                    ..t("t-old", 8)
                },
                CodexThread {
                    archived: true,
                    ..t("t-arch", 7)
                },
                t("t-chat", 6),
                t("t-chat2", 2),
            ],
            ..Default::default()
        };
        let sidebar = parse_codex_sidebar(&codex_state_json()).unwrap();
        let layout = codex_sidebar_layout(&data, &sidebar);
        let shown: Vec<(String, bool, Vec<String>)> = layout
            .sections
            .iter()
            .map(|s| {
                let mut all = ids(&s.entries);
                for g in &s.groups {
                    all.push(format!(
                        "{}{}:{}",
                        g.label,
                        if g.collapsed { "(closed)" } else { "" },
                        ids(&g.entries).join(",")
                    ));
                }
                (s.label.clone(), s.collapsed, all)
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (
                    "Pinned".to_string(),
                    true,
                    vec!["rollout-t-pin".to_string()]
                ),
                (
                    "Work".into(),
                    false,
                    vec![
                        "api(closed):rollout-t-pin,rollout-t-a2,rollout-t-a1".into(),
                        "web:rollout-t-w[rollout-t-sub]".into()
                    ]
                ),
                (
                    "Agent chats".into(),
                    true,
                    vec!["rollout-t-chat2".into(), "t-gone".into()]
                ),
                (
                    "Projects".into(),
                    false,
                    vec![
                        "docs(closed):rollout-t-moved".into(),
                        "empty(closed):".into()
                    ]
                ),
                ("Chats".into(), false, vec!["rollout-t-chat".into()]),
            ]
        );
        assert!(
            layout.sections[1].groups.iter().all(|g| g.keep),
            "a folder is drawn empty too"
        );
        assert!(
            layout.note.as_deref().unwrap().contains("(x-new)"),
            "{:?}",
            layout.note
        );
        // A state file of another shape says so.
        assert!(parse_codex_sidebar(&json!({ "project-order": [] }))
            .unwrap_err()
            .contains("sidebar-custom-sections-v3"));
    }

    /// #s387: threads in a project folder and in Chats follow the app's chats order: the newest
    /// first by default, by date created, or the person's own order.
    #[test]
    fn the_codex_app_layout_orders_threads_as_its_chats_sort_says() {
        let t = |id: &str, recency: i64, created: i64| CodexThread {
            id: id.into(),
            row: id.into(),
            recency,
            created,
            project: Some("srv-api".into()),
            ..Default::default()
        };
        let data = CodexData {
            threads: vec![t("old-new", 9, 1), t("mid", 5, 5), t("new-old", 1, 9)],
            ..Default::default()
        };
        let mut sidebar = parse_codex_sidebar(&codex_state_json()).unwrap();
        let folder = |sidebar: &CodexSidebar| {
            let layout = codex_sidebar_layout(&data, sidebar);
            let work = layout.sections.iter().find(|s| s.label == "Work").unwrap();
            ids(&work.groups[0].entries)
        };
        assert_eq!(sidebar.chat_sort, "updated_at");
        assert_eq!(folder(&sidebar), vec!["old-new", "mid", "new-old"]);
        sidebar.chat_sort = "created_at".into();
        assert_eq!(folder(&sidebar), vec!["new-old", "mid", "old-new"]);
        sidebar.chat_sort = "manual".into();
        sidebar
            .thread_orders
            .insert("lp-api".into(), vec!["mid".into(), "new-old".into()]);
        assert_eq!(folder(&sidebar), vec!["mid", "new-old", "old-new"]);
        // The state file's own key.
        let mut state = codex_state_json();
        state["electron-persisted-atom-state"]["flat-project-sidebar-preferences-v1"] =
            json!({ "chatSortMode": "created_at", "mode": "project" });
        assert_eq!(parse_codex_sidebar(&state).unwrap().chat_sort, "created_at");
    }

    /// #s387: the state file is read beside the database, re-read only when it changes, and a file
    /// caught half-written keeps the last good read.
    #[test]
    fn the_codex_app_state_file_is_read_beside_its_database() {
        let root =
            std::env::temp_dir().join(format!("cm-layout-codex-state-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("state_5.sqlite");
        let state = root.join(".codex-global-state.json");
        let mut cache = None;
        assert!(codex_state(&db, &mut cache)
            .unwrap_err()
            .contains("not found"));
        std::fs::write(&state, codex_state_json().to_string()).unwrap();
        assert_eq!(codex_state(&db, &mut cache).unwrap().pinned, vec!["t-pin"]);
        std::fs::write(&state, "{\"electron-persisted-atom-state\": {").unwrap();
        assert_eq!(codex_state(&db, &mut cache).unwrap().pinned, vec!["t-pin"]);
        let mut fresh = None;
        assert!(codex_state(&db, &mut fresh)
            .unwrap_err()
            .contains("could not be read"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// #s387: Herdr's workspaces and tabs in their numbered order, a row per pane whose agent
    /// reported its session (an id, or a transcript path), shell panes skipped, and the server's
    /// name on each workspace when more than one server runs.
    #[test]
    fn the_herdr_layout_follows_workspaces_tabs_and_agent_panes() {
        let snapshot = |label: &str| {
            json!({ "id": "1", "result": { "type": "snapshot", "snapshot": {
                "workspaces": [
                    { "workspace_id": "w2", "number": 2, "label": "web" },
                    { "workspace_id": "w1", "number": 1, "label": label },
                ],
                "tabs": [
                    { "tab_id": "t2", "workspace_id": "w1", "number": 2, "label": "" },
                    { "tab_id": "t1", "workspace_id": "w1", "number": 1, "label": "auth" },
                    { "tab_id": "t3", "workspace_id": "w2", "number": 1, "label": "tests" },
                ],
                "panes": [
                    { "pane_id": "p1", "tab_id": "t1", "agent_session": { "source": "hook", "agent": "claude", "kind": "id", "value": "c-auth" } },
                    { "pane_id": "p2", "tab_id": "t1", "agent_session": null },
                    { "pane_id": "p3", "tab_id": "t2", "label": "docs pane", "agent_session": { "source": "hook", "agent": "codex", "kind": "path", "value": "/h/.codex/sessions/2026/11/06/rollout-2026-11-06T01-02-03-0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b.jsonl" } },
                    { "pane_id": "p4", "tab_id": "t3" },
                ],
                "agents": [
                    { "pane_id": "p4", "agent_session": { "source": "hook", "agent": "claude", "kind": "path", "value": "/h/.claude/projects/-w-web/c-e2e.jsonl" } },
                ],
            }}})
        };
        let layout = herdr_layout(&[("work".into(), snapshot("api"))]);
        let shown: Vec<(String, Vec<String>)> = layout
            .sections
            .iter()
            .map(|s| {
                (
                    s.label.clone(),
                    s.groups
                        .iter()
                        .map(|g| format!("{}:{}", g.label, ids(&g.entries).join(",")))
                        .collect(),
                )
            })
            .collect();
        assert_eq!(
            shown,
            vec![
                (
                    "api".to_string(),
                    vec![
                        "auth:c-auth".to_string(),
                        "Tab 2:rollout-2026-11-06T01-02-03-0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b"
                            .into()
                    ]
                ),
                ("web".into(), vec!["tests:c-e2e".into()]),
            ]
        );
        assert_eq!(
            layout.sections[0].groups[1].entries[0].title.as_deref(),
            Some("docs pane")
        );
        // Each entry names its pane, so the page can prefer the row the monitor links to it.
        assert_eq!(
            layout.sections[0].groups[0].entries[0].pane.as_deref(),
            Some("work|p1")
        );
        let two = herdr_layout(&[
            ("work".into(), snapshot("api")),
            ("home".into(), snapshot("api")),
        ]);
        assert_eq!(two.sections[0].label, "work · api");
        assert_eq!(two.sections[2].label, "home · api");
        assert_ne!(two.sections[0].id, two.sections[2].id);
    }

    /// The review of #s387: a request never reads an app. The first ask answers "pending" and
    /// starts the source's reader; the next answers with its read; asking which sources can be
    /// followed reads nothing; the reader ends once nobody asks, and a later ask starts it again,
    /// answering with the old read marked stale until the new reader's first read lands.
    #[test]
    fn a_layout_request_never_waits_on_a_read() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let reads = Arc::new(AtomicUsize::new(0));
        // Each read waits until the case lets it go, so "the request did not wait" is a fact of
        // the order of events, not of a clock (#s300: a timing bound flakes on a loaded machine).
        let open = Arc::new(AtomicBool::new(false));
        let (counted, gate) = (reads.clone(), open.clone());
        let layouts = Layouts::with(
            Arc::new(move |_: &mut Readers, _| {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !gate.load(SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let n = counted.fetch_add(1, SeqCst) + 1;
                Ok(Layout {
                    note: Some(format!("read {n}")),
                    ..Default::default()
                })
            }),
            Arc::new(|_| Ok(())),
            Duration::from_millis(20),
            Duration::from_millis(200),
        );
        let first: Value = serde_json::from_str(&layouts.layout_json(Source::Herdr)).unwrap();
        assert_eq!(
            first["pending"], true,
            "the request waited on a read: {first}"
        );
        open.store(true, SeqCst);
        let until = |what: &str, done: &dyn Fn(&Value) -> bool| {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let v: Value = serde_json::from_str(&layouts.layout_json(Source::Herdr)).unwrap();
                if done(&v) {
                    return v;
                }
                assert!(Instant::now() < deadline, "{what}: {v}");
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        let read = until("no read landed", &|v| v["note"].as_str().is_some());
        assert_eq!(read["available"], true, "{read}");
        assert!(read.get("stale").is_none(), "{read}");
        // Asking which sources can be followed reads nothing.
        let before = reads.load(SeqCst);
        let _ = layouts.sources_json();
        assert_eq!(reads.load(SeqCst), before);
        // Nobody asks: the reader ends, and its count stops.
        let slot = &layouts.slots[Layouts::index(Source::Herdr)];
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(slot).reading {
            assert!(Instant::now() < deadline, "the reader kept reading");
            std::thread::sleep(Duration::from_millis(10));
        }
        let settled = reads.load(SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(reads.load(SeqCst), settled, "the reader kept reading");
        // The second review of #s387: an ask after that answers with the last read, marked stale
        // (it may be an hour old), until the new reader's first read lands.
        open.store(false, SeqCst);
        let back: Value = serde_json::from_str(&layouts.layout_json(Source::Herdr)).unwrap();
        assert_eq!(back["stale"], true, "{back}");
        assert_eq!(back["note"], format!("read {settled}"));
        open.store(true, SeqCst);
        until("the new reader's read did not land", &|v| {
            v.get("stale").is_none()
        });
    }

    /// The second review of #s387: only the file check decides whether a source can be chosen. A
    /// read that failed is said, and the source stays choosable — choosing it is what reads it
    /// again — so a source that failed once is not disabled until the monitor restarts.
    #[test]
    fn a_failed_read_is_said_and_never_disables_a_source() {
        let failing = Layouts::with(
            Arc::new(|_: &mut Readers, _| Err("No Herdr server is running".into())),
            Arc::new(|source| match source {
                Source::CodexApp => Err("The Codex app is not installed on this machine".into()),
                _ => Ok(()),
            }),
            Duration::from_millis(20),
            Duration::from_millis(100),
        );
        let _ = failing.layout_json(Source::Herdr);
        let deadline = Instant::now() + Duration::from_secs(5);
        while lock(&failing.slots[Layouts::index(Source::Herdr)])
            .last
            .is_none()
        {
            assert!(Instant::now() < deadline, "no read landed");
            std::thread::sleep(Duration::from_millis(10));
        }
        let sources: Value = serde_json::from_str(&failing.sources_json()).unwrap();
        let herdr = &sources["sources"][2];
        assert_eq!(herdr["source"], "herdr");
        assert_eq!(herdr["available"], true, "{herdr}");
        assert_eq!(herdr["reason"], Value::Null);
        assert_eq!(herdr["lastError"], "No Herdr server is running");
        // A source whose files are not there cannot be chosen, and says why.
        let codex = &sources["sources"][1];
        assert_eq!(codex["available"], false, "{codex}");
        assert_eq!(
            codex["reason"],
            "The Codex app is not installed on this machine"
        );
        assert_eq!(codex["lastError"], Value::Null);
    }

    /// The review of #s387: the Codex app lists only the threads in its own catalogue, so a
    /// `codex exec` run or a sub-agent the database holds is not drawn; a row that does not read is
    /// counted in the note, not dropped in silence.
    #[test]
    fn the_codex_app_layout_lists_only_the_threads_its_catalogue_lists() {
        let t = |id: &str, recency: i64| CodexThread {
            id: id.into(),
            row: id.into(),
            recency,
            project: Some("srv-api".into()),
            ..Default::default()
        };
        let mut data = CodexData {
            threads: vec![
                t("shown", 3),
                CodexThread {
                    headless: true,
                    ..t("exec-run", 2)
                },
                CodexThread {
                    parent: Some("shown".into()),
                    headless: true,
                    ..t("sub", 1)
                },
                t("cli-run", 0),
            ],
            listed: Some(["shown", "t-pin"].map(String::from).into_iter().collect()),
            catalogue_note: None,
            unreadable: 2,
            ..Default::default()
        };
        let sidebar = parse_codex_sidebar(&codex_state_json()).unwrap();
        let layout = codex_sidebar_layout(&data, &sidebar);
        let work = layout.sections.iter().find(|s| s.label == "Work").unwrap();
        assert_eq!(ids(&work.groups[0].entries), vec!["shown"]);
        // A thread the state file names that the catalogue does not list is not drawn either (the
        // section is sent empty, and the page draws no empty section).
        let agent = layout
            .sections
            .iter()
            .find(|s| s.label == "Agent chats")
            .unwrap();
        assert!(agent.entries.is_empty(), "{agent:?}");
        assert!(layout
            .note
            .as_deref()
            .unwrap()
            .contains("2 threads in the Codex app's database could not be read"));
        // The second review of #s387: without a catalogue, every thread but the headless ones
        // (`codex exec` runs and sub-agents, by their source) counts, and the note says why.
        data.listed = None;
        data.catalogue_note = Some("The Codex app's own list of threads was not found".into());
        let layout = codex_sidebar_layout(&data, &sidebar);
        let work = layout.sections.iter().find(|s| s.label == "Work").unwrap();
        assert_eq!(ids(&work.groups[0].entries), vec!["shown", "cli-run"]);
        let note = layout.note.unwrap();
        assert!(
            note.contains("The Codex app's own list of threads was not found. 2 threads"),
            "{note}"
        );
    }

    /// The second review of #s387: the Codex app's `source` names the threads it does not list.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_codex_exec_run_and_a_sub_agent_are_headless() {
        assert!(codex_headless("exec"));
        assert!(codex_headless(r#"{"subagent":{"other":"guardian"}}"#));
        assert!(codex_headless(
            r#"{"subagent":{"thread_spawn":{"parent_thread_id":"t","depth":1}}}"#
        ));
        assert!(!codex_headless("vscode"));
        assert!(!codex_headless("cli"));
        // The third review of #s387: the app lists an MCP or app-server run and `unknown`, and a
        // custom source; any other object it does not.
        assert!(!codex_headless("mcp"));
        assert!(!codex_headless("appServer"));
        assert!(!codex_headless("unknown"));
        assert!(!codex_headless(r#"{"custom":"my-tool"}"#));
        assert!(codex_headless(r#"{"other":{}}"#));
        assert!(codex_headless("{}"));
    }

    /// The third review of #s387: an immutable read that a writer reached is not trusted, and only a
    /// value of the wrong type in a row is counted; any other error mid-scan fails the read.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_codex_read_a_writer_reached_is_busy_and_a_failed_scan_is_no_list() {
        let root =
            std::env::temp_dir().join(format!("cm-layout-codex-moved-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("state_5.sqlite");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (x);")
            .unwrap();
        drop(conn);
        let app = AppDb::open(&db).unwrap();
        assert!(app.immutable_at.is_some(), "a WAL database with no -wal");
        assert!(!app.moved());
        // A writer arrives: its -wal appears beside the database.
        std::fs::write(root.join("state_5.sqlite-wal"), b"").unwrap();
        assert!(app.moved());
        drop(app);
        // Opened while a writer has it, the read is an ordinary one, and nothing moves it.
        let app = AppDb::open(&db).unwrap();
        assert!(app.immutable_at.is_none());
        assert!(!app.moved());
        drop(app);
        std::fs::remove_dir_all(&root).unwrap();
        assert!(sqlite_row_type(&rusqlite::Error::InvalidColumnType(
            0,
            "x".into(),
            rusqlite::types::Type::Text
        )));
        assert!(!sqlite_row_type(&rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            None
        )));
    }

    /// The third review of #s387: a database with no `source` column still reads; with no
    /// catalogue every thread then counts, and the note says the database names no source.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_codex_database_without_sources_still_reads() {
        let root =
            std::env::temp_dir().join(format!("cm-layout-codex-nosource-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("state_5.sqlite");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE projects (id TEXT, name TEXT, position INTEGER);
                 CREATE TABLE thread_sections (id TEXT, name TEXT);
                 CREATE TABLE threads (id TEXT PRIMARY KEY, rollout_path TEXT NOT NULL, title TEXT NOT NULL, updated_at INTEGER NOT NULL, archived INTEGER NOT NULL DEFAULT 0, updated_at_ms INTEGER, recency_at_ms INTEGER NOT NULL DEFAULT 0, name TEXT, is_pinned INTEGER NOT NULL DEFAULT 0, thread_section_id TEXT, section_position INTEGER, project_id TEXT, created_at INTEGER NOT NULL DEFAULT 0, created_at_ms INTEGER);
                 INSERT INTO threads (id, rollout_path, title, updated_at) VALUES ('a', '/h/rollout-x-a.jsonl', 'a', 1), ('b', '/h/rollout-x-b.jsonl', 'b', 1);",
            )
            .unwrap();
        let data = read_codex(&db).unwrap();
        assert_eq!(data.threads.len(), 2);
        assert!(data.threads.iter().all(|t| data.lists(t)));
        let note = data.note().unwrap();
        assert!(note.contains("names no thread's source"), "{note}");
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The review of #s387: a database another process holds says it is busy, at once, rather than
    /// after SQLite's default five seconds, and not as a format this monitor does not know.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_busy_codex_database_says_so_at_once() {
        let root =
            std::env::temp_dir().join(format!("cm-layout-codex-busy-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let db = root.join("state_5.sqlite");
        let writer = rusqlite::Connection::open(&db).unwrap();
        writer
            .execute_batch(
                "CREATE TABLE projects (id TEXT, name TEXT, position INTEGER);
                 CREATE TABLE thread_sections (id TEXT, name TEXT);
                 BEGIN EXCLUSIVE; INSERT INTO projects VALUES ('p', 'p', 0);",
            )
            .unwrap();
        let started = Instant::now();
        assert_eq!(read_codex(&db).unwrap_err(), CODEX_BUSY);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        drop(writer);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// #s387: the wire shape the page reads, for a layout and for a source that cannot be read.
    #[test]
    fn a_layout_answers_in_the_shape_the_page_reads() {
        let layout = Layout {
            sections: vec![Section {
                id: "s".into(),
                label: "S".into(),
                collapsed: true,
                entries: vec![Entry {
                    id: "a".into(),
                    title: Some("A".into()),
                    pane: Some("work|w1:p1".into()),
                    children: vec![Entry {
                        id: "b".into(),
                        ..Default::default()
                    }],
                }],
                groups: vec![Group {
                    id: "g".into(),
                    label: "G".into(),
                    collapsed: true,
                    keep: true,
                    entries: vec![],
                }],
            }],
            note: Some("n".into()),
        };
        let v = layout_json(Source::Herdr, &Ok(layout));
        assert_eq!(v["source"], "herdr");
        assert_eq!(v["available"], true);
        assert_eq!(v["note"], "n");
        assert_eq!(v["sections"][0]["entries"][0]["children"][0]["id"], "b");
        assert_eq!(v["sections"][0]["entries"][0]["pane"], "work|w1:p1");
        assert_eq!(pending_json(Source::Herdr)["pending"], true);
        assert_eq!(v["sections"][0]["groups"][0]["label"], "G");
        assert_eq!(v["sections"][0]["groups"][0]["collapsed"], true);
        assert_eq!(v["sections"][0]["groups"][0]["keep"], true);
        let v = layout_json(Source::CodexApp, &Err("why".into()));
        assert_eq!(
            (v["available"].clone(), v["reason"].clone()),
            (json!(false), json!("why"))
        );
        assert_eq!(Source::parse("claude-app"), Some(Source::ClaudeApp));
        assert_eq!(Source::parse("nope"), None);
        // In a unit test no source reads this machine's apps.
        let summary: Value = serde_json::from_str(&Layouts::default().sources_json()).unwrap();
        assert!(
            summary["sources"]
                .as_array()
                .unwrap()
                .iter()
                .all(|s| s["available"] == false),
            "{summary}"
        );
    }
}

//! mdrev's embedded viewer, as a guest in the app shell's preview pane (#270) — the monitor's half
//! of mdrev's embedding contract. `design/mdrev-in-the-preview-pane.md` is the design; mdrev's own
//! guide and contract are pinned with the release, at `vendor/mdrev/release/docs/`.
//!
//! Three things live here:
//!
//! * **the kit** — ONE mdrev release, PINNED (#274): the monitors build the vendored release
//!   (`vendor/mdrev`, the `mdrev` crate) into their binaries and [`install`] it when they start.
//!   Its `bundle/` is the guest the page mounts and its `mdrev-cli.js` is the note store, run with
//!   node; they come from one release because the guide requires it (§11: "they share the note
//!   record and the anchor format"). Nothing installed on the machine is looked at — the version
//!   in the repository is the dependency, and it moves only by `scripts/vendor-mdrev.sh`. This
//!   crate never names the kit's crate, so `agent-replay`, which serves no preview pane, carries
//!   none of it;
//! * **`mdrev/<version>/…`** — that bundle, from memory, under a prefix that names the version,
//!   since the two entries are not content-hashed and a page must not keep a stale one across a
//!   monitor upgrade that moved the pin;
//! * **`api/mdrev/…`** — the contract, for two collections. `held` is Markdown the transcript
//!   itself carries, which the page hands to us and we keep content-addressed in memory; any other
//!   `root` is a directory on this machine, and every route about a document in it re-applies the
//!   four guards `/file` applies — pairing and a same-origin request, the file's own `Cap::File`
//!   stamp (which is also mdrev's `cap` for it), containment, and the size cap.
//!
//! Why the contract and not mdrev's in-process `client` for the held text: that seam's `doc()`
//! returns the document PRE-RENDERED, which mdrev's own HTTP client does with its renderer, its
//! highlighter's language loader and its diagram and math hooks — none of them exported by the
//! released bundle (`HOST_CLASS`, `engine`, `mountMdrev`, and nothing else). It is a seam for hosts
//! that build mdrev from source. A host consuming the release has the contract, versioned with the
//! guide and checked by `mdrev-cli conform`.

use super::serve::{
    artifact_headers, download_name, percent_decode, query_get, raster_type, HttpResponse, Request,
    SessionService, MAX_ARTIFACT_BYTES,
};
use super::sig::{self, Cap};
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------------------- the kit

/// A pinned mdrev release, as a binary carries it (#274): its version, every file of its `bundle/`
/// by its path inside `bundle/` — sorted by that path, so a lookup is a binary search — and its
/// CLI with the `package.json` the CLI reads its version from.
#[derive(Clone, Copy)]
pub struct Kit {
    pub version: &'static str,
    pub bundle: &'static [(&'static str, &'static [u8])],
    pub cli: &'static [u8],
    pub package_json: &'static [u8],
}

/// The kit this process serves, and how its CLI runs here — found on first use, since that means
/// writing the CLI out and finding a node to run it.
pub(super) struct Release {
    version: &'static str,
    bundle: &'static [(&'static str, &'static [u8])],
    kit: Option<Kit>,
    dir: PathBuf,
    cli: OnceLock<Option<Vec<OsString>>>,
}

static INSTALLED: OnceLock<Release> = OnceLock::new();

/// Serve `kit`, writing its CLI under `dir` when a route first needs it. Once per process: a
/// binary carries exactly one kit, so a second call is ignored.
pub fn install(kit: Kit, dir: PathBuf) {
    let _ = INSTALLED.set(Release {
        version: kit.version,
        bundle: kit.bundle,
        kit: Some(kit),
        dir,
        cli: OnceLock::new(),
    });
}

/// The kit this process serves. `None` in a server that installed none (`agent-replay --html`),
/// where the routes answer 404 and the page keeps rendering Markdown as text.
fn release() -> Option<&'static Release> {
    INSTALLED.get()
}

impl Release {
    /// The command that runs the kit's CLI — `node <dir>/<version>/mdrev-cli.js` — or `None` when
    /// this machine has no node fit to run it. Then notes are not offered (`open` says so) rather
    /// than failing on every call, and history comes from git instead of the CLI.
    fn cli_argv(&self) -> Option<&[OsString]> {
        self.cli
            .get_or_init(|| {
                let kit = self.kit?;
                let explicit = std::env::var_os("MDREV_NODE");
                let Some(node) = find_node(explicit, std::env::var_os("PATH"), NODE_FALLBACKS, &node_fits)
                else {
                    eprintln!(
                        "mdrev: review notes are off — no node >= {NODE_MAJOR} (MDREV_NODE, PATH, {}); \
                         history comes from git, without following renames",
                        NODE_FALLBACKS.join(", ")
                    );
                    return None;
                };
                match write_cli(&kit, &self.dir) {
                    Ok(script) => Some(vec![node.into_os_string(), script.into_os_string()]),
                    Err(e) => {
                        eprintln!(
                            "mdrev: review notes are off — writing mdrev-cli into {}: {e}",
                            self.dir.display()
                        );
                        None
                    }
                }
            })
            .as_deref()
    }
}

/// The oldest node the kit runs on — its README: "Requires node ≥ 20".
const NODE_MAJOR: u64 = 20;

/// Where node is when it is not on the PATH the monitor was started with — a launchd agent, a
/// shell that never sourced brew's environment: Apple silicon, Intel, Linuxbrew.
const NODE_FALLBACKS: &[&str] = &[
    "/opt/homebrew/bin/node",
    "/usr/local/bin/node",
    "/home/linuxbrew/.linuxbrew/bin/node",
];

/// The node to run the CLI with. `MDREV_NODE` is mdrev's own name for "this node" and is FINAL when
/// set, as it is in mdrev's launchers (`${MDREV_NODE:-node}`, so an empty one is unset) — which is
/// also how a case takes node away. Otherwise the first fit `node` on `PATH`, then the fallbacks.
fn find_node(
    explicit: Option<OsString>,
    path: Option<OsString>,
    fallbacks: &[&str],
    fits: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if let Some(node) = explicit.filter(|n| !n.is_empty()).map(PathBuf::from) {
        return fits(&node).then_some(node);
    }
    let on_path: Vec<PathBuf> = path
        .map(|p| std::env::split_paths(&p).map(|d| d.join("node")).collect())
        .unwrap_or_default();
    on_path
        .into_iter()
        .chain(fallbacks.iter().map(PathBuf::from))
        .find(|n| n.is_file() && fits(n))
}

/// A node runs, and reports a version the kit runs on.
fn node_fits(node: &Path) -> bool {
    let Ok(out) = Command::new(node)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return false;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let major = text
        .trim()
        .strip_prefix('v')
        .and_then(|v| v.split('.').next())
        .and_then(|m| m.parse::<u64>().ok());
    out.status.success() && major.is_some_and(|m| m >= NODE_MAJOR)
}

/// The kit's CLI, written out where node can run it: `<dir>/<version>/mdrev-cli.js`, beside the
/// `package.json` it reads its version from. Rewritten once per process over whatever is there —
/// the binary's copy is the pin, a file left in a scratch directory is not — and each file lands
/// by rename, so a reader never sees half of one.
fn write_cli(kit: &Kit, dir: &Path) -> std::io::Result<PathBuf> {
    let at = dir.join(kit.version);
    std::fs::create_dir_all(&at)?;
    for (name, bytes) in [
        ("package.json", kit.package_json),
        ("mdrev-cli.js", kit.cli),
    ] {
        let tmp = at.join(format!(".{name}.{}", std::process::id()));
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, at.join(name))?;
    }
    Ok(at.join("mdrev-cli.js"))
}

// ------------------------------------------------------------------------------------ dispatch

/// Claims `mdrev/<version>/…` (the bundle) and `api/mdrev/…` (the contract); `None` for every other
/// name, so `service_routes` goes on as before.
pub(super) fn route(live: Option<&SessionService>, req: &Request) -> Option<HttpResponse> {
    if let Some(rest) = req.name.strip_prefix("mdrev/") {
        return Some(bundle_file(release(), rest));
    }
    let rest = req.name.strip_prefix("api/mdrev/")?;
    Some(contract(release(), live, req, rest, held()))
}

/// The largest body `hold` takes: a document at the artifact cap, plus its JSON framing.
pub(super) const HOLD_BODY_LIMIT: usize = MAX_ARTIFACT_BYTES as usize + 64 * 1024;

/// The collection name for Markdown the transcript carries. Not a path, so it can never be
/// mistaken for a directory.
const HELD_ROOT: &str = "held";

fn contract(
    rel: Option<&Release>,
    live: Option<&SessionService>,
    req: &Request,
    route: &str,
    held: &Mutex<Held>,
) -> HttpResponse {
    let Some(rel) = rel else {
        return HttpResponse::not_found("this server carries no mdrev");
    };
    if !req.origin_ok {
        return HttpResponse::forbidden("cross-origin request refused");
    }
    match route {
        "hold" => return hold(req, held),
        "open" => return open(rel, live, req),
        // Reveal is interim — it will be replaced by a web file browser (#272) — so mdrev's rail
        // does not get a new Finder path. 501 is the contract's "never": the entry leaves the menu.
        "reveal" | "forget" => {
            return status("501 Not Implemented", error("not offered by this host"));
        }
        // No push: 204 is the contract's "poll me".
        "events" => return status("204 No Content", Vec::new()),
        // A side pane has no file rail. Absent is the answer that hides each section; an empty
        // list would draw a section that can never fill.
        "documents" | "tree" | "changed" | "recents" | "recent-changes" => {
            return HttpResponse::not_found("not offered by this host");
        }
        _ => {}
    }
    let root = param(req, "root").unwrap_or_default();
    if root == HELD_ROOT {
        held_route(req, route, held)
    } else {
        local_route(rel, live, req, route, &root)
    }
}

// ----------------------------------------------------------------------------- the bundle

fn bundle_file(rel: Option<&Release>, rest: &str) -> HttpResponse {
    let Some(rel) = rel else {
        return HttpResponse::not_found("this server carries no mdrev");
    };
    let Some((version, file)) = rest.split_once('/') else {
        return HttpResponse::not_found("no such file");
    };
    // Another version is a page from before the monitor moved the pin: 404, and it reloads.
    if version != rel.version {
        return HttpResponse::not_found("no such mdrev version");
    }
    let ext = file.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let Some(ct) = bundle_type(&ext) else {
        return HttpResponse::not_found("no such file");
    };
    // The table IS the bundle: a name not in it — `..`, an absolute path, a directory — names
    // nothing, and there is no filesystem underneath to lead anywhere else.
    let Ok(i) = rel.bundle.binary_search_by(|(path, _)| (*path).cmp(file)) else {
        return HttpResponse::not_found("no such file");
    };
    let mut r = HttpResponse::ok(ct, rel.bundle[i].1.to_vec());
    // The prefix names the version, so nothing under it ever changes.
    r.headers
        .push("Cache-Control: public, max-age=31536000, immutable".to_string());
    r.headers
        .push("X-Content-Type-Options: nosniff".to_string());
    r
}

/// What the bundle is made of (measured on 1.1.12: js, css, woff2, woff, ttf). The favicon's SVG
/// is not among them: nothing inside a mount asks for it, and an SVG is a script host.
fn bundle_type(ext: &str) -> Option<&'static str> {
    Some(match ext {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "json" => "application/json",
        "wasm" => "application/wasm",
        _ => return None,
    })
}

// ----------------------------------------------------------------------- held documents

const HELD_MAX_DOCS: usize = 64;
const HELD_MAX_BYTES: usize = 32 * 1024 * 1024;

/// Markdown the transcript carries, handed to us by the page so mdrev can read it through the
/// contract. Content-addressed — the same text under the same name is one entry however often it
/// is opened — and bounded, least recently used first out. Never written to disk.
#[derive(Default)]
pub(super) struct Held {
    order: VecDeque<String>,
    docs: HashMap<String, String>,
    bytes: usize,
}

impl Held {
    fn hold(&mut self, name: &str, text: String) -> String {
        use sha2::{Digest, Sha256};
        let hash = Sha256::digest(text.as_bytes());
        let hex: String = hash.iter().take(8).map(|b| format!("{b:02x}")).collect();
        let path = format!("{hex}/{}", safe_name(name));
        if self.docs.contains_key(&path) {
            self.touch(&path);
            return path;
        }
        self.bytes += text.len();
        self.docs.insert(path.clone(), text);
        self.order.push_back(path.clone());
        while self.order.len() > HELD_MAX_DOCS || self.bytes > HELD_MAX_BYTES {
            // Never the one just held: a single document is bounded by the body limit, far
            // under the store's.
            if self.order.len() <= 1 {
                break;
            }
            if let Some(old) = self.order.pop_front() {
                if let Some(t) = self.docs.remove(&old) {
                    self.bytes -= t.len();
                }
            }
        }
        path
    }

    fn get(&mut self, path: &str) -> Option<String> {
        let text = self.docs.get(path)?.clone();
        self.touch(path);
        Some(text)
    }

    fn touch(&mut self, path: &str) {
        if let Some(i) = self.order.iter().position(|p| p == path) {
            if let Some(p) = self.order.remove(i) {
                self.order.push_back(p);
            }
        }
    }
}

fn held() -> &'static Mutex<Held> {
    static H: OnceLock<Mutex<Held>> = OnceLock::new();
    H.get_or_init(|| Mutex::new(Held::default()))
}

/// A held document's name, as the last segment of its path: the basename, with anything that is
/// not a plain filename character replaced. It is a label — the hash is the identity.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '-'
            }
        })
        .take(80)
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "document.md".to_string()
    } else {
        cleaned
    }
}

/// `POST hold {name, text}` → `{root: "held", path, cap, name}`. It neither reads nor writes the
/// disk, so it asks for no pairing — only a same-origin POST, which the page's fetch is.
fn hold(req: &Request, held: &Mutex<Held>) -> HttpResponse {
    if req.method != "POST" {
        return HttpResponse::method_not_allowed("POST required");
    }
    let Ok(body) = serde_json::from_slice::<Value>(req.body) else {
        return status("400 Bad Request", error("the body is not JSON"));
    };
    let (Some(name), Some(text)) = (
        body.get("name").and_then(Value::as_str),
        body.get("text").and_then(Value::as_str),
    ) else {
        return status("400 Bad Request", error("expected {name, text}"));
    };
    if text.len() as u64 > MAX_ARTIFACT_BYTES {
        return status("413 Content Too Large", error("too large"));
    }
    let path = held
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .hold(name, text.to_string());
    let Some(cap) = sig::sign(Cap::Held, &path) else {
        return status("503 Service Unavailable", error("no signing key"));
    };
    HttpResponse::json(
        json!({"root": HELD_ROOT, "path": path, "cap": cap, "name": safe_name(name)}).to_string(),
    )
}

fn held_route(req: &Request, route: &str, held: &Mutex<Held>) -> HttpResponse {
    if route == "resolve" {
        // A held document is text from a transcript: it has no folder, so nothing it points at
        // is relative to anything. Every reference is refused — after the referrer is checked.
        let Some((from, targets)) = resolve_body(req) else {
            return status(
                "400 Bad Request",
                error("expected {from: {path, cap}, targets}"),
            );
        };
        if !sig::verify(Cap::Held, &from.0, from.1.as_deref()) {
            return HttpResponse::forbidden("not offered");
        }
        return HttpResponse::json(json!({"caps": vec![Value::Null; targets.len()]}).to_string());
    }
    let path = param(req, "path").unwrap_or_default();
    if !sig::verify(Cap::Held, &path, param(req, "cap").as_deref()) {
        return HttpResponse::forbidden("not offered");
    }
    match (req.method, route) {
        ("GET", "text") => match held.lock().unwrap_or_else(|e| e.into_inner()).get(&path) {
            Some(text) => HttpResponse::ok("text/plain; charset=utf-8", text.into_bytes()),
            // Evicted, or the monitor restarted: the page falls back to its own copy.
            None => HttpResponse::not_found("no longer held"),
        },
        ("GET", "revisions") | ("GET", "annotations") => HttpResponse::json("[]".to_string()),
        ("GET", "stat") => HttpResponse::json(r#"{"mtimeMs":null}"#.to_string()),
        (_, r) if r.starts_with("annotations") => {
            HttpResponse::forbidden("a document from a transcript takes no notes")
        }
        _ => HttpResponse::not_found("not offered for a held document"),
    }
}

// ------------------------------------------------------------------------------ local files

/// The mount's facts for a file the page already holds a `/file` stamp for:
/// `{root, path, cap, isGit, name, review, annotate}`. The same four guards as `/file`.
fn open(rel: &Release, live: Option<&SessionService>, req: &Request) -> HttpResponse {
    if let Some(r) = refuse_unpaired(req) {
        return r;
    }
    let Some(live) = live else {
        return HttpResponse::not_found("no live server");
    };
    let abs = param(req, "path").unwrap_or_default();
    let stamp = param(req, "sig");
    if !sig::verify(Cap::File, &abs, stamp.as_deref()) {
        return HttpResponse::not_found("no such path");
    }
    if live.contained(Path::new(&abs)).is_none() {
        return HttpResponse::not_found("no such path");
    }
    if !is_markdown(&abs) {
        return status(
            "415 Unsupported Media Type",
            error("not a Markdown document"),
        );
    }
    let (root, is_git) = root_of(&abs);
    let path = abs[root.len()..].trim_start_matches('/').to_string();
    let name = path.rsplit('/').next().unwrap_or(&path).to_string();
    // Notes are mdrev's own format, written only through its CLI; without a node to run it this
    // host refuses them — the contract's leave, and a refusal is whole: the viewer never asks the
    // annotation routes. History is the host's store, served from git either way.
    let notes = rel.cli_argv().is_some();
    HttpResponse::json(
        json!({
            "root": root, "path": path, "cap": stamp, "isGit": is_git, "name": name,
            "review": true, "annotate": notes,
        })
        .to_string(),
    )
}

/// The collection a file belongs to: the nearest ancestor holding `.git` (a directory, or the
/// file a worktree has), found on the path AS A STRING — else the file's own directory, with no
/// history. Never a `realpath`: the stamp was minted for the path as the server rendered it, and
/// `root + "/" + path` must rebuild that string byte for byte, which a resolved `/tmp` →
/// `/private/tmp` would not.
fn root_of(abs: &str) -> (String, bool) {
    let file = Path::new(abs);
    let mut dir = file.parent();
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return (d.to_string_lossy().into_owned(), true);
        }
        dir = d.parent();
    }
    let parent = file.parent().map(|p| p.to_string_lossy().into_owned());
    (parent.unwrap_or_else(|| "/".to_string()), false)
}

/// What mdrev reads: the extensions mdrev's own `/documents` lists, and their common spellings.
fn is_markdown(path: &str) -> bool {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    path.contains('.') && matches!(ext.as_str(), "md" | "markdown" | "mdown" | "mkd")
}

/// `/file`'s first guard: reading the local disk over HTTP is a capability the owner opts into by
/// pairing, and unpaired it is absent. The status is `/file`'s, so the page reads it the same way.
fn refuse_unpaired(req: &Request) -> Option<HttpResponse> {
    (!req.authenticated).then(|| {
        HttpResponse::unauthorized(
            "reading local files requires pairing — run `agent-monitor --pair`",
        )
    })
}

/// A document in a local collection, with its stamp checked and its containment proved.
struct Doc {
    root: String,
    rel: String,
    real: PathBuf,
}

fn doc(live: &SessionService, req: &Request, root: &str) -> Result<Doc, HttpResponse> {
    if !Path::new(root).is_absolute() {
        return Err(status("400 Bad Request", error("root must be absolute")));
    }
    let rel = param(req, "path").unwrap_or_default();
    if !plain_relative(&rel) {
        return Err(HttpResponse::forbidden("not offered"));
    }
    let abs = join(root, &rel);
    if !sig::verify(Cap::File, &abs, param(req, "cap").as_deref()) {
        return Err(HttpResponse::forbidden("not offered"));
    }
    let Some(real) = live.contained(Path::new(&abs)) else {
        return Err(HttpResponse::not_found("no such document"));
    };
    Ok(Doc {
        root: root.to_string(),
        rel,
        real,
    })
}

/// A path the contract may name inside a collection: relative, and every segment a plain name.
fn plain_relative(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.starts_with('/')
        && !rel.contains('\\')
        && !rel.contains('\0')
        && rel
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

fn join(root: &str, rel: &str) -> String {
    if root == "/" {
        format!("/{rel}")
    } else {
        format!("{}/{rel}", root.trim_end_matches('/'))
    }
}

fn local_route(
    rel: &Release,
    live: Option<&SessionService>,
    req: &Request,
    route: &str,
    root: &str,
) -> HttpResponse {
    if let Some(r) = refuse_unpaired(req) {
        return r;
    }
    let Some(live) = live else {
        return HttpResponse::not_found("no live server");
    };
    if route == "resolve" {
        return resolve(live, req, root);
    }
    // The COLLECTION's revisions (no path): only asked to tell "a folder with no history" from "a
    // draft nobody has committed yet". The mount declares `isGit` itself, so the honest short
    // answer costs nothing: this host does not list a collection's history.
    if route == "revisions" && param(req, "path").is_none() {
        return HttpResponse::json("[]".to_string());
    }
    let d = match doc(live, req, root) {
        Ok(d) => d,
        Err(r) => return r,
    };
    match (req.method, route) {
        ("GET", "text") => text(rel, &d, req),
        ("GET", "revisions") => revisions(rel, &d),
        ("GET", "asset") => asset(&d),
        ("GET", "snapshot") => snapshot(&d, req),
        ("GET", "stat") => stat(&d),
        ("GET", "annotations") => notes_list(rel, &d),
        ("POST", "annotations") => notes_add(rel, &d, req),
        (_, r) if r.starts_with("annotations/") => {
            note_op(rel, &d, req, &r["annotations/".len()..])
        }
        _ => HttpResponse::not_found("no such route"),
    }
}

/// `rev=current` is the file as it stands; any other `rev` must be a commit id from `/revisions`
/// — hex, so it can never reach git as an option.
fn text(rel: &Release, d: &Doc, req: &Request) -> HttpResponse {
    let rev = param(req, "rev").unwrap_or_else(|| "current".to_string());
    if rev == "current" {
        return match read_text(&d.real) {
            Ok(t) => HttpResponse::ok("text/plain; charset=utf-8", t.into_bytes()),
            Err(r) => r,
        };
    }
    if !is_hex(&rev, 4) {
        return status(
            "400 Bad Request",
            error("rev must be a commit id or `current`"),
        );
    }
    // mdrev-v2's rule: a revision is read under the name the revisions list gives it; a document
    // whose every listed name is its own was never renamed and reads under that; the rest is the
    // CLI's, which follows renames exactly as mdrev does.
    let listed = revision_list(rel, d).unwrap_or_default();
    let name = listed
        .iter()
        .find(|r| {
            r.get("rev")
                .and_then(Value::as_str)
                .is_some_and(|id| id == rev || (rev.len() >= 7 && id.starts_with(&rev)))
        })
        .map(|r| {
            r.get("path")
                .and_then(Value::as_str)
                .unwrap_or(&d.rel)
                .to_string()
        })
        .or_else(|| {
            listed
                .iter()
                .all(|r| {
                    r.get("path")
                        .and_then(Value::as_str)
                        .is_none_or(|p| p == d.rel)
                })
                .then(|| d.rel.clone())
        });
    if let Some(name) = name {
        if plain_relative(&name) {
            if let Some(bytes) = git(&d.root, &["show", &format!("{rev}:{name}")]) {
                return text_reply(bytes);
            }
        }
    }
    let out = cli(
        rel,
        &d.root,
        &["text", "--path", &d.rel, "--rev", &rev],
        None,
    );
    match out.code {
        0 => text_reply(out.out),
        _ => HttpResponse::not_found("no such document at that revision"),
    }
}

fn text_reply(bytes: Vec<u8>) -> HttpResponse {
    if bytes.len() as u64 > MAX_ARTIFACT_BYTES {
        return status("413 Content Too Large", error("too large"));
    }
    match String::from_utf8(bytes) {
        Ok(t) => HttpResponse::ok("text/plain; charset=utf-8", t.into_bytes()),
        Err(_) => status("415 Unsupported Media Type", error("not text")),
    }
}

fn read_text(real: &Path) -> Result<String, HttpResponse> {
    let meta = std::fs::metadata(real).map_err(|_| HttpResponse::not_found("no such document"))?;
    if meta.len() > MAX_ARTIFACT_BYTES {
        return Err(status("413 Content Too Large", error("too large")));
    }
    let bytes = std::fs::read(real).map_err(|_| HttpResponse::not_found("no such document"))?;
    String::from_utf8(bytes).map_err(|_| status("415 Unsupported Media Type", error("not text")))
}

fn revisions(rel: &Release, d: &Doc) -> HttpResponse {
    match revision_list(rel, d) {
        Some(list) => HttpResponse::json(Value::Array(list).to_string()),
        None => status(
            "502 Bad Gateway",
            error("mdrev-cli could not list the revisions"),
        ),
    }
}

/// `mdrev-cli revisions --path P`, cached per `HEAD` (mdrev-v2's rule: a process per read was
/// measured at 650 ms there, ~90 ms here — and it is asked for on every text read). A collection
/// with no history has none to list and is not asked. Without a node for the CLI, the NAME's
/// history from git ([`name_history`]).
fn revision_list(rel: &Release, d: &Doc) -> Option<Vec<Value>> {
    if !Path::new(&d.root).join(".git").exists() {
        return Some(Vec::new());
    }
    let head = git(&d.root, &["rev-parse", "HEAD"])
        .map(|b| String::from_utf8_lossy(&b).trim().to_string())
        .unwrap_or_default();
    type Cache = HashMap<(String, String), (String, Vec<Value>)>;
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (d.root.clone(), d.rel.clone());
    if let Some((h, list)) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        if *h == head {
            return Some(list.clone());
        }
    }
    let list: Vec<Value> = if rel.cli_argv().is_some() {
        let out = cli(rel, &d.root, &["revisions", "--path", &d.rel], None);
        match out.code {
            0 => serde_json::from_slice(&out.out).ok()?,
            // "No such document" in history: a draft nobody has committed yet.
            2 => Vec::new(),
            _ => return None,
        }
    } else {
        name_history(&d.root, &d.rel)?
    };
    let mut g = cache.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() > 256 {
        g.clear();
    }
    g.insert(key, (head, list.clone()));
    Some(list)
}

/// The revisions that touched this NAME, newest first, from git — what the contract calls a host
/// that "lists the history of the name": each entry without `path`, so `/text` reads every one
/// under the document's own name, and nothing from before a rename is listed. Only the kit's CLI
/// follows renames exactly as mdrev does; this is the history a machine without node still has.
fn name_history(root: &str, rel: &str) -> Option<Vec<Value>> {
    const FIELD: char = '\u{1f}';
    const RECORD: char = '\u{1e}';
    let out = git(
        root,
        &[
            "log",
            "--no-color",
            "--format=%H%x1f%aI%x1f%an%x1f%s%x1f%b%x1e",
            "--",
            rel,
        ],
    )?;
    let text = String::from_utf8_lossy(&out);
    let list = text
        .split(RECORD)
        .filter_map(|record| {
            let f: Vec<&str> = record.trim_start_matches('\n').split(FIELD).collect();
            let [rev, date, author, subject, rest @ ..] = f.as_slice() else {
                return None;
            };
            let mut entry = json!({"rev": rev, "date": date, "author": author, "subject": subject});
            if let Some(body) = rest.first().map(|b| b.trim()).filter(|b| !b.is_empty()) {
                entry["body"] = json!(body);
            }
            Some(entry)
        })
        .collect();
    Some(list)
}

/// A file the document points at, with the stamp `/resolve` minted for it (or the document itself
/// — `mdrev-cli conform` asks for that). Served exactly as `/file` serves any file, because this is
/// the same origin holding the same cookie: a raster image by its type, text as `text/plain` (a
/// repository's `.html` or `.svg` is READ, never run), anything else as a download — and all of it
/// with `nosniff` and an empty sandbox.
fn asset(d: &Doc) -> HttpResponse {
    let Ok(meta) = std::fs::metadata(&d.real) else {
        return HttpResponse::not_found("no such asset");
    };
    if meta.len() > MAX_ARTIFACT_BYTES {
        return status("413 Content Too Large", error("too large"));
    }
    let Ok(bytes) = std::fs::read(&d.real) else {
        return HttpResponse::not_found("no such asset");
    };
    let ext = d.rel.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let mut r = match raster_type(&ext) {
        Some(ct) => HttpResponse::ok(ct, bytes),
        None if std::str::from_utf8(&bytes).is_ok() => {
            HttpResponse::ok("text/plain; charset=utf-8", bytes)
        }
        None => {
            let mut r = HttpResponse::ok("application/octet-stream", bytes);
            r.headers.push(format!(
                "Content-Disposition: attachment; filename=\"{}\"",
                download_name(&d.real)
            ));
            r
        }
    };
    r.headers.extend(artifact_headers());
    r
}

/// What a document points at, stamped for this reader: a `Cap::File` stamp for each target inside
/// the collection that containment explains and the render policy admits — the same two tests a
/// rendered link passes before the renderer stamps it — and `null` for everything else. The
/// referrer's own stamp is checked first.
fn resolve(live: &SessionService, req: &Request, root: &str) -> HttpResponse {
    if req.method != "POST" {
        return HttpResponse::method_not_allowed("POST required");
    }
    if !Path::new(root).is_absolute() {
        return status("400 Bad Request", error("root must be absolute"));
    }
    let Some(((from, from_cap), targets)) = resolve_body(req) else {
        return status(
            "400 Bad Request",
            error("expected {from: {path, cap}, targets}"),
        );
    };
    if !plain_relative(&from) || !sig::verify(Cap::File, &join(root, &from), from_cap.as_deref()) {
        return HttpResponse::forbidden("not offered");
    }
    let caps: Vec<Value> = targets
        .iter()
        .map(|t| {
            let t = t.as_str().unwrap_or("");
            if !plain_relative(t) {
                return Value::Null;
            }
            let abs = join(root, t);
            if live.contained(Path::new(&abs)).is_none() || !sig::may_render(&abs) {
                return Value::Null;
            }
            sig::sign(Cap::File, &abs).map_or(Value::Null, Value::String)
        })
        .collect();
    HttpResponse::json(json!({ "caps": caps }).to_string())
}

type Referrer = (String, Option<String>);

fn resolve_body(req: &Request) -> Option<(Referrer, Vec<Value>)> {
    let body: Value = serde_json::from_slice(req.body).ok()?;
    let from = body.get("from")?;
    let path = from.get("path")?.as_str()?.to_string();
    let cap = from.get("cap").and_then(Value::as_str).map(str::to_string);
    let targets = body.get("targets")?.as_array()?.clone();
    Some(((path, cap), targets))
}

/// The text a note was taken on, for placing it exactly. `blob` must be hex.
fn snapshot(d: &Doc, req: &Request) -> HttpResponse {
    let blob = param(req, "blob").unwrap_or_default();
    if !is_hex(&blob, 4) {
        return status("400 Bad Request", error("blob must be an object id"));
    }
    match git(&d.root, &["cat-file", "-p", &blob]) {
        Some(bytes) => text_reply(bytes),
        None => HttpResponse::not_found("no such blob"),
    }
}

fn stat(d: &Doc) -> HttpResponse {
    let ms = std::fs::metadata(&d.real)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64);
    HttpResponse::json(json!({ "mtimeMs": ms }).to_string())
}

// ------------------------------------------------------------------------------------ notes

/// `GET annotations`: the document's notes, closed ones included, as mdrev's records.
fn notes_list(rel: &Release, d: &Doc) -> HttpResponse {
    let out = cli(
        rel,
        &d.root,
        &["notes", "list", "--path", &d.rel, "--all"],
        None,
    );
    if out.code != 0 {
        return cli_error(&out);
    }
    let Ok(items) = serde_json::from_slice::<Vec<Value>>(&out.out) else {
        return status(
            "502 Bad Gateway",
            error("mdrev-cli answered something other than a list"),
        );
    };
    let notes: Vec<Value> = items
        .into_iter()
        .filter_map(|mut i| i.get_mut("annotation").map(Value::take))
        .collect();
    HttpResponse::json(Value::Array(notes).to_string())
}

/// `POST annotations`: file a note — the viewer's record on stdin, the stored note back, 201.
fn notes_add(rel: &Release, d: &Doc, req: &Request) -> HttpResponse {
    if let Some(r) = req.deny_mutation("POST") {
        return r;
    }
    let _one_at_a_time = document_lock(d);
    let out = cli(
        rel,
        &d.root,
        &["notes", "add", "--path", &d.rel],
        Some(req.body),
    );
    if out.code != 0 {
        return cli_error(&out);
    }
    status("201 Created", out.out)
}

/// `PATCH annotations/{id}` (close / reopen), `POST …/{id}/replies`, `DELETE …/{id}`,
/// `DELETE …/{id}/replies/{at}` — each one `mdrev-cli notes` verb.
fn note_op(rel: &Release, d: &Doc, req: &Request, rest: &str) -> HttpResponse {
    let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let id_ok = !id.is_empty()
        && id.len() <= 80
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !id_ok {
        return status("400 Bad Request", error("no such note id"));
    }
    let body: Value = serde_json::from_slice(req.body).unwrap_or(Value::Null);
    let args: Vec<String>;
    let (method, ok): (&'static str, &'static str) = match (req.method, tail) {
        ("PATCH", "") => {
            let verb = match body.get("status").and_then(Value::as_str) {
                Some("resolved") => "resolve",
                Some("wontfix") => "wontfix",
                Some("open") => "reopen",
                _ => {
                    return status(
                        "400 Bad Request",
                        error("status must be resolved, wontfix or open"),
                    )
                }
            };
            let mut a = vec!["notes".to_string(), verb.to_string(), id.to_string()];
            if let Some(note) = body
                .get("resolvedBy")
                .and_then(|r| r.get("note"))
                .and_then(Value::as_str)
                .filter(|n| !n.is_empty())
            {
                a.extend(["--note".to_string(), note.to_string()]);
            }
            args = a;
            ("PATCH", "200 OK")
        }
        ("POST", "replies") => {
            let Some(text) = body.get("body").and_then(Value::as_str) else {
                return status("400 Bad Request", error("expected {body}"));
            };
            args = ["notes", "reply", id, "--body", text]
                .map(String::from)
                .to_vec();
            ("POST", "200 OK")
        }
        ("DELETE", "") => {
            args = ["notes", "delete", id].map(String::from).to_vec();
            ("DELETE", "204 No Content")
        }
        ("DELETE", t) if t.starts_with("replies/") => {
            let at = percent_decode(&t["replies/".len()..]);
            args = vec![
                "notes".into(),
                "delete-reply".into(),
                id.into(),
                "--at".into(),
                at,
            ];
            ("DELETE", "200 OK")
        }
        _ => return HttpResponse::method_not_allowed("no such note operation"),
    };
    if let Some(r) = req.deny_mutation(method) {
        return r;
    }
    let _one_at_a_time = document_lock(d);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = cli(rel, &d.root, &argv, None);
    if out.code != 0 {
        return cli_error(&out);
    }
    if ok == "204 No Content" {
        return status(ok, Vec::new());
    }
    status(ok, out.out)
}

/// One document's note writes, one at a time: the CLI rewrites the sidecar on every change but
/// create, so two closes at the same instant could lose one another (guide §6).
fn document_lock(d: &Doc) -> std::sync::MutexGuard<'static, ()> {
    type Locks = Mutex<HashMap<String, &'static Mutex<()>>>;
    static LOCKS: OnceLock<Locks> = OnceLock::new();
    // One lock per DOCUMENT ever written, made once and kept for the life of the process — a
    // handful in practice — so the guard can outlive the map's own lock.
    let mut locks = LOCKS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let lock = *locks
        .entry(format!("{}\0{}", d.root, d.rel))
        .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))));
    drop(locks);
    lock.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------------------ processes

struct Out {
    code: i32,
    out: Vec<u8>,
    err: String,
}

/// How long one `mdrev-cli` call may take before the request gives up on it.
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the kit's `mdrev-cli` in `root`, with `--root root` as mdrev-v2 does. Output is read on
/// threads while the child runs — a note list larger than a pipe buffer would otherwise hold the
/// child and this request against each other until the timeout.
fn cli(rel: &Release, root: &str, args: &[&str], stdin: Option<&[u8]>) -> Out {
    let Some((program, lead)) = rel.cli_argv().and_then(|a| a.split_first()) else {
        return Out {
            code: -1,
            out: Vec::new(),
            err: format!("mdrev-cli needs node >= {NODE_MAJOR}, and this machine has none"),
        };
    };
    let spawned = Command::new(program)
        .args(lead)
        .args(args)
        .args(["--root", root])
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(c) => c,
        Err(e) => {
            return Out {
                code: -1,
                out: Vec::new(),
                err: format!("could not run mdrev-cli: {e}"),
            }
        }
    };
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin.unwrap_or_default());
    }
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = p {
                let _ = p.read_to_end(&mut buf);
            }
            buf
        })
    };
    let out_t = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err_t = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + CLI_TIMEOUT;
    let code = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s.code().unwrap_or(-1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break -2;
            }
        }
    };
    let out = out_t.join().unwrap_or_default();
    let err = String::from_utf8_lossy(&err_t.join().unwrap_or_default()).into_owned();
    Out { code, out, err }
}

/// The CLI's exit code, as the contract's status: 1 a bad request, 2 no such note or document.
fn cli_error(out: &Out) -> HttpResponse {
    let message = out.err.trim().trim_start_matches("mdrev-cli: ").to_string();
    let message = if message.is_empty() {
        "mdrev-cli failed".to_string()
    } else {
        message
    };
    let code = match out.code {
        1 => "400 Bad Request",
        2 => "404 Not Found",
        -2 => "504 Gateway Timeout",
        _ => "502 Bad Gateway",
    };
    status(code, json!({ "error": message }).to_string().into_bytes())
}

fn git(root: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        // A document named `:(glob)*.md` is a name, never pathspec magic.
        .env("GIT_LITERAL_PATHSPECS", "1")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

// ---------------------------------------------------------------------------------- helpers

fn param(req: &Request, key: &str) -> Option<String> {
    query_get(req.query, key).map(percent_decode)
}

fn is_hex(s: &str, min: usize) -> bool {
    (min..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn error(message: &str) -> Vec<u8> {
    json!({ "error": message }).to_string().into_bytes()
}

fn status(code: &'static str, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        code,
        content_type: "application/json",
        body,
        headers: Vec::new(),
    }
}

/// The version the app shell names in its page, so it knows whether — and where — to load mdrev.
pub fn version() -> Option<&'static str> {
    release().map(|r| r.version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::Presentation;
    use crate::fold::FoldPolicy;
    use crate::html_export::serve::{RootLock, ServiceConfig};

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-mdrev-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A bundle as a binary carries one: both entries, a lazy chunk, and an SVG the route must
    /// refuse — sorted by path, as the kit's build writes it.
    static TEST_BUNDLE: &[(&str, &[u8])] = &[
        ("chunks/c-1a2b.js", b"export {};"),
        ("favicon.svg", b"<svg/>"),
        ("mdrev.css", b".mdrev-host{}"),
        ("mdrev.js", b"export const mountMdrev = 1;"),
    ];

    impl Release {
        /// A release whose CLI is already resolved: `argv` runs it, `None` is a machine without node.
        fn fixed(argv: Option<Vec<OsString>>) -> Release {
            Release {
                version: "9.9.9",
                bundle: TEST_BUNDLE,
                kit: None,
                dir: PathBuf::new(),
                cli: OnceLock::from(argv),
            }
        }
    }

    /// An executable script at `at` — the stand-in CLI, or a stand-in node.
    fn script(at: &Path, body: &str) -> PathBuf {
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(at, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(at, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        at.to_path_buf()
    }

    /// A stand-in `mdrev-cli`: records every call (argv, and stdin for `add`), answers
    /// `revisions` with the fixture repository's REAL commits in the contract's shape, and
    /// fails the way the real one does — exit 2 for a missing note, 1 for anything unknown.
    const FAKE_CLI: &str = r#"#!/bin/sh
here="$(cd "$(dirname "$0")" && pwd)"
printf '%s\n' "$*" >> "$here/calls.log"
case "$1" in
  revisions)
    git log --format='{"rev":"%H","date":"%aI","author":"%an","subject":"%s","path":"'"$3"'"}' -- "$3" | paste -sd, - | sed 's/^/[/; s/$/]/' ;;
  text) printf 'from the cli' ;;
  notes)
    case "$2" in
      list) echo '[{"path":"docs/doc.md","annotation":{"id":"ann-1","body":"hi"},"state":"exact"}]' ;;
      add) cat > "$here/stdin.json"; echo '{"id":"ann-2","body":"filed"}' ;;
      delete) if [ "$3" = "ann-missing" ]; then echo "mdrev-cli: no such note" >&2; exit 2; fi; echo '{"deleted":true}' ;;
      *) echo '{"id":"'"$3"'","status":"'"$2"'"}' ;;
    esac ;;
  *) echo "mdrev-cli: unknown command" >&2; exit 1 ;;
esac
"#;

    fn git_in(repo: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    struct Fx {
        _dir: PathBuf,
        repo: PathBuf,
        live: SessionService,
        rel: Release,
        kit: PathBuf,
        first: String,
    }

    /// A checkout with `docs/doc.md` committed twice ("# One", then "# Two"), an image and a text
    /// file beside it, a session whose cwd is the checkout (so containment explains it), and a
    /// release whose CLI is the stand-in.
    fn fx(name: &str) -> Fx {
        let dir = scratch(name);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join("docs")).unwrap();
        git_in(&repo, &["init", "-q"]);
        std::fs::write(repo.join("docs/doc.md"), "# One\n").unwrap();
        git_in(&repo, &["add", "."]);
        git_in(&repo, &["commit", "-qm", "one"]);
        let first = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();
        std::fs::write(repo.join("docs/doc.md"), "# Two\n").unwrap();
        git_in(&repo, &["commit", "-qam", "two"]);
        std::fs::write(repo.join("docs/img.png"), [0x89, b'P', b'N', b'G']).unwrap();
        std::fs::write(repo.join("docs/notes.txt"), "plain").unwrap();
        std::fs::write(repo.join("docs/other.md"), "# Other").unwrap();
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        let sess = store.join("s.jsonl");
        std::fs::write(
            &sess,
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"hi\"}}]}},\"timestamp\":\"2026-09-24T10:00:00Z\"}}\n",
                repo.display()
            ),
        )
        .unwrap();
        let live = SessionService::new(ServiceConfig {
            cache_root: None,
            presentation: Presentation::Html,
            fold: FoldPolicy::default(),
            scratch: dir.join("scratch"),
            root_lock: RootLock::PerSession,
        })
        .unwrap();
        live.register_root(&sess);
        let kit = dir.join("kit");
        let cli = script(&kit.join("mdrev-cli"), FAKE_CLI);
        let rel = Release::fixed(Some(vec![cli.into_os_string()]));
        Fx {
            _dir: dir,
            repo,
            live,
            rel,
            kit,
            first,
        }
    }

    fn stamp(abs: &str) -> String {
        sig::sign(Cap::File, abs).unwrap()
    }

    fn enc(s: &str) -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect()
    }

    /// The query every document route carries: the collection, the path in it, its stamp.
    fn doc_query(f: &Fx, rel: &str) -> String {
        let root = f.repo.display().to_string();
        let abs = format!("{root}/{rel}");
        format!("root={}&path={}&cap={}", enc(&root), enc(rel), stamp(&abs))
    }

    fn call(f: &Fx, held: &Mutex<Held>, route: &str, req: &Request) -> HttpResponse {
        contract(Some(&f.rel), Some(&f.live), req, route, held)
    }

    fn req<'a>(method: &'a str, query: &'a str, body: &'a [u8], paired: bool) -> Request<'a> {
        Request {
            method,
            name: "api/mdrev/x",
            query,
            body,
            authenticated: paired,
            origin_ok: true,
        }
    }

    fn calls(f: &Fx) -> String {
        std::fs::read_to_string(f.kit.join("calls.log")).unwrap_or_default()
    }

    // ---------------------------------------------------------------------- the kit

    #[test]
    fn the_bundle_is_served_from_the_binary_under_its_version_and_nothing_else_is() {
        let rel = Release::fixed(None);
        let r = bundle_file(Some(&rel), "9.9.9/mdrev.js");
        assert_eq!(
            (r.code, r.content_type, r.body.as_slice()),
            (
                "200 OK",
                "text/javascript; charset=utf-8",
                b"export const mountMdrev = 1;".as_slice()
            )
        );
        assert!(
            r.headers.iter().any(|h| h.contains("immutable")),
            "the prefix is versioned"
        );
        assert_eq!(
            bundle_file(Some(&rel), "9.9.9/chunks/c-1a2b.js").code,
            "200 OK"
        );
        assert_eq!(
            bundle_file(Some(&rel), "9.9.9/mdrev.css").content_type,
            "text/css; charset=utf-8"
        );
        for refused in [
            "9.9.8/mdrev.js",    // another version: a page from before the pin moved
            "9.9.9/../mdrev.js", // the table names files, never a way out of it
            "9.9.9/./mdrev.js",
            "9.9.9//mdrev.js",
            "9.9.9/chunks",      // a directory is not a file
            "9.9.9/favicon.svg", // in the bundle, but an SVG is a script host on this origin
            "9.9.9/missing.js",
            "9.9.9",
        ] {
            assert_eq!(
                bundle_file(Some(&rel), refused).code,
                "404 Not Found",
                "{refused}"
            );
        }
        assert_eq!(bundle_file(None, "9.9.9/mdrev.js").code, "404 Not Found");
    }

    #[test]
    fn node_is_mdrev_s_own_choice_else_the_path_s_else_a_brew_prefix() {
        let d = scratch("node");
        let at = |p: &str| script(&d.join(p), "");
        let (old, new, brew) = (at("old/node"), at("new/node"), at("brew/bin/node"));
        // "Fits" stands for `node --version` >= 20 (held below): here, anything but the old one.
        let fits = |n: &Path| !n.starts_with(d.join("old"));
        let path = std::env::join_paths([d.join("missing"), d.join("old"), d.join("new")]).unwrap();
        let fallbacks = [brew.to_str().unwrap()];
        assert_eq!(
            find_node(None, Some(path.clone()), &fallbacks, &fits),
            Some(new.clone()),
            "the first node on PATH that fits"
        );
        assert_eq!(
            find_node(None, None, &fallbacks, &fits),
            Some(brew.clone()),
            "a brew prefix, for a monitor started without brew on its PATH"
        );
        assert_eq!(
            find_node(Some(new.clone().into()), None, &fallbacks, &fits),
            Some(new),
            "MDREV_NODE"
        );
        assert_eq!(
            find_node(Some(old.into()), Some(path), &fallbacks, &fits),
            None,
            "MDREV_NODE is final, as in mdrev's own launchers — how a case takes node away"
        );
        assert_eq!(
            find_node(Some(OsString::new()), None, &fallbacks, &fits),
            Some(brew),
            "an empty MDREV_NODE is unset, as `${{MDREV_NODE:-node}}` reads it"
        );
        assert_eq!(find_node(None, None, &[], &fits), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_node_fits_when_it_runs_and_is_20_or_later() {
        let d = scratch("fits");
        for (i, (body, fits)) in [
            ("echo v24.15.0", true),
            ("echo v20.0.0", true),
            ("echo v18.19.1", false),
            ("echo nonsense", false),
            ("echo v22.1.0; exit 1", false),
        ]
        .into_iter()
        .enumerate()
        {
            let node = script(&d.join(format!("n{i}")), &format!("#!/bin/sh\n{body}\n"));
            assert_eq!(node_fits(&node), fits, "{body}");
        }
        assert!(!node_fits(&d.join("absent")));
    }

    #[test]
    fn the_cli_is_written_beside_its_package_json_and_over_anything_already_there() {
        let d = scratch("write");
        let kit = Kit {
            version: "9.9.9",
            bundle: TEST_BUNDLE,
            cli: b"// the pinned cli",
            package_json: br#"{"version":"9.9.9"}"#,
        };
        let cli = write_cli(&kit, &d).unwrap();
        assert_eq!(cli, d.join("9.9.9/mdrev-cli.js"));
        assert_eq!(std::fs::read(&cli).unwrap(), kit.cli);
        assert_eq!(
            std::fs::read(d.join("9.9.9/package.json")).unwrap(),
            kit.package_json,
            "`mdrev-cli --version` reads it beside itself"
        );
        std::fs::write(&cli, "tampered").unwrap();
        write_cli(&kit, &d).unwrap();
        assert_eq!(
            std::fs::read(&cli).unwrap(),
            kit.cli,
            "the binary's copy is the pin, not what a scratch directory held"
        );
    }

    // ------------------------------------------------------------- held documents

    #[test]
    fn held_text_is_content_addressed_stamped_and_bounded() {
        let f = fx("held");
        let held = Mutex::new(Held::default());
        let body = br##"{"name":"../odd name?.md","text":"# Hi\n"}"##;
        let r = call(&f, &held, "hold", &req("POST", "", body, false));
        assert_eq!(r.code, "200 OK", "{}", String::from_utf8_lossy(&r.body));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["root"], "held");
        let path = v["path"].as_str().unwrap().to_string();
        assert!(
            path.ends_with("/odd-name-.md"),
            "the name is a label, cleaned: {path}"
        );
        let again: Value =
            serde_json::from_slice(&call(&f, &held, "hold", &req("POST", "", body, false)).body)
                .unwrap();
        assert_eq!(again["path"], json!(path), "the same text is one entry");

        let cap = v["cap"].as_str().unwrap();
        let q = format!("root=held&path={}&cap={cap}", enc(&path));
        let text = call(&f, &held, "text", &req("GET", &q, b"", false));
        assert_eq!(
            (text.code, text.body.as_slice()),
            ("200 OK", b"# Hi\n".as_slice()),
            "held text needs no pairing: it never touches the disk"
        );
        for (route, want) in [("revisions", "[]"), ("annotations", "[]")] {
            assert_eq!(
                call(&f, &held, route, &req("GET", &q, b"", false)).body,
                want.as_bytes()
            );
        }
        assert_eq!(
            call(&f, &held, "annotations", &req("POST", &q, b"{}", true)).code,
            "403 Forbidden",
            "a document from a transcript takes no notes"
        );
        let wrong = format!("root=held&path={}&cap={}", enc(&path), "0".repeat(64));
        assert_eq!(
            call(&f, &held, "text", &req("GET", &wrong, b"", false)).code,
            "403 Forbidden"
        );
        // A stamp names one CAPABILITY: a file stamp for the same string opens nothing held.
        let as_file = format!("root=held&path={}&cap={}", enc(&path), stamp(&path));
        assert_eq!(
            call(&f, &held, "text", &req("GET", &as_file, b"", false)).code,
            "403 Forbidden"
        );

        let resolve =
            format!(r#"{{"from":{{"path":"{path}","cap":"{cap}"}},"targets":["a.png","b.md"]}}"#);
        let r = call(
            &f,
            &held,
            "resolve",
            &req("POST", "root=held", resolve.as_bytes(), false),
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&r.body).unwrap(),
            json!({"caps": [null, null]}),
            "text from a transcript has no folder for a reference to be relative to"
        );
        assert_eq!(
            call(&f, &held, "hold", &req("GET", "", b"", false)).code,
            "405 Method Not Allowed"
        );

        let mut store = Held::default();
        for i in 0..(HELD_MAX_DOCS + 6) {
            store.hold("d.md", format!("doc {i}"));
        }
        assert_eq!(store.docs.len(), HELD_MAX_DOCS, "bounded, oldest out");
        assert!(store.docs.values().all(|t| t != "doc 0"));
    }

    #[test]
    fn the_contract_is_absent_without_a_release_and_refuses_a_foreign_origin() {
        let f = fx("absent");
        let held = Mutex::new(Held::default());
        let r = contract(
            None,
            Some(&f.live),
            &req("GET", "", b"", true),
            "text",
            &held,
        );
        assert_eq!(r.code, "404 Not Found");
        let q = doc_query(&f, "docs/doc.md");
        let mut foreign = req("GET", &q, b"", true);
        foreign.origin_ok = false;
        assert_eq!(call(&f, &held, "text", &foreign).code, "403 Forbidden");
        assert_eq!(
            call(&f, &held, "events", &req("GET", "", b"", true)).code,
            "204 No Content"
        );
        assert_eq!(
            call(&f, &held, "reveal", &req("POST", "", b"{}", true)).code,
            "501 Not Implemented"
        );
        assert_eq!(
            call(&f, &held, "tree", &req("GET", "", b"", true)).code,
            "404 Not Found"
        );
    }

    // --------------------------------------------------------------- local files

    #[test]
    fn open_answers_the_mount_facts_for_a_stamped_markdown_file() {
        let f = fx("open");
        let held = Mutex::new(Held::default());
        let abs = format!("{}/docs/doc.md", f.repo.display());
        let q = format!("path={}&sig={}", enc(&abs), stamp(&abs));
        let r = call(&f, &held, "open", &req("GET", &q, b"", true));
        assert_eq!(r.code, "200 OK", "{}", String::from_utf8_lossy(&r.body));
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(
            v["root"],
            json!(f.repo.display().to_string()),
            "the nearest .git"
        );
        assert_eq!(v["path"], "docs/doc.md");
        assert_eq!(v["isGit"], true);
        assert_eq!(
            (&v["review"], &v["annotate"]),
            (&json!(true), &json!(true)),
            "history and notes are offered: the CLI runs"
        );
        assert_eq!(
            v["cap"],
            json!(stamp(&abs)),
            "mdrev's cap IS the file's own stamp"
        );
        assert_eq!(
            format!(
                "{}/{}",
                v["root"].as_str().unwrap(),
                v["path"].as_str().unwrap()
            ),
            abs,
            "root + path rebuilds the stamped string byte for byte"
        );

        assert_eq!(
            call(&f, &held, "open", &req("GET", &q, b"", false)).code,
            "401 Unauthorized"
        );
        let unstamped = format!("path={}&sig={}", enc(&abs), "0".repeat(64));
        assert_eq!(
            call(&f, &held, "open", &req("GET", &unstamped, b"", true)).code,
            "404 Not Found"
        );
        let txt = format!("{}/docs/notes.txt", f.repo.display());
        let q = format!("path={}&sig={}", enc(&txt), stamp(&txt));
        assert_eq!(
            call(&f, &held, "open", &req("GET", &q, b"", true)).code,
            "415 Unsupported Media Type"
        );
    }

    /// No node, no CLI: the host refuses notes — mdrev's own format, written only through its CLI —
    /// and serves history from git instead: the name's revisions, each read under that name.
    #[test]
    fn without_node_a_local_file_keeps_its_history_from_git_and_takes_no_notes() {
        let f = fx("nonode");
        let held = Mutex::new(Held::default());
        let bare = Release::fixed(None);
        let ask = |route: &str, q: &str| {
            contract(
                Some(&bare),
                Some(&f.live),
                &req("GET", q, b"", true),
                route,
                &held,
            )
        };
        let abs = format!("{}/docs/doc.md", f.repo.display());
        let open = ask("open", &format!("path={}&sig={}", enc(&abs), stamp(&abs)));
        let v: Value = serde_json::from_slice(&open.body).unwrap();
        assert_eq!(
            (&v["review"], &v["annotate"]),
            (&json!(true), &json!(false)),
            "history offered, notes refused — a refusal the viewer honours by never asking"
        );

        let q = doc_query(&f, "docs/doc.md");
        let revs = ask("revisions", &q);
        assert_eq!(
            revs.code,
            "200 OK",
            "{}",
            String::from_utf8_lossy(&revs.body)
        );
        let list: Vec<Value> = serde_json::from_slice(&revs.body).unwrap();
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!(list[1]["rev"], json!(f.first), "newest first");
        assert_eq!(list[1]["subject"], "one");
        assert!(
            list.iter().all(|r| r.get("path").is_none()),
            "the NAME's history: no `path`, as the contract has such a host answer"
        );
        assert_eq!(
            ask("text", &format!("{q}&rev={}", f.first)).body,
            b"# One\n",
            "a listed revision reads under the document's own name"
        );

        let notes = ask("annotations", &q);
        assert_eq!(notes.code, "502 Bad Gateway");
        assert!(
            String::from_utf8_lossy(&notes.body).contains("needs node"),
            "a route the viewer no longer asks for says why: {}",
            String::from_utf8_lossy(&notes.body)
        );
    }

    #[test]
    fn a_local_document_is_read_now_and_at_a_revision_behind_the_file_guards() {
        let f = fx("text");
        let held = Mutex::new(Held::default());
        let q = doc_query(&f, "docs/doc.md");
        let now = call(&f, &held, "text", &req("GET", &q, b"", true));
        assert_eq!(
            (now.code, now.body.as_slice()),
            ("200 OK", b"# Two\n".as_slice())
        );
        let then = format!("{q}&rev={}", f.first);
        let old = call(&f, &held, "text", &req("GET", &then, b"", true));
        assert_eq!(
            (old.code, old.body.as_slice()),
            ("200 OK", b"# One\n".as_slice()),
            "git show at the listed revision"
        );

        let revs = call(&f, &held, "revisions", &req("GET", &q, b"", true));
        let list: Vec<Value> = serde_json::from_slice(&revs.body).unwrap();
        assert_eq!(list.len(), 2, "{}", String::from_utf8_lossy(&revs.body));
        assert_eq!(list[1]["rev"], json!(f.first), "newest first");
        let asked = calls(&f).matches("revisions --path docs/doc.md").count();
        call(&f, &held, "revisions", &req("GET", &q, b"", true));
        assert_eq!(
            calls(&f).matches("revisions --path docs/doc.md").count(),
            asked,
            "cached per HEAD — the text read above already asked once"
        );
        assert!(
            calls(&f).contains(&format!("--root {}", f.repo.display())),
            "every call names the collection, as mdrev-v2 does"
        );

        // The guards, each on its own.
        assert_eq!(
            call(&f, &held, "text", &req("GET", &q, b"", false)).code,
            "401 Unauthorized"
        );
        let other = doc_query(&f, "docs/other.md");
        let swapped = format!(
            "{}&cap={}",
            &q[..q.find("&cap=").unwrap()],
            &other[other.find("&cap=").unwrap() + 5..]
        );
        assert_eq!(
            call(&f, &held, "text", &req("GET", &swapped, b"", true)).code,
            "403 Forbidden",
            "a stamp names one path"
        );
        let root = f.repo.display().to_string();
        let up = format!(
            "root={}&path=..%2Fstore%2Fs.jsonl&cap={}",
            enc(&root),
            stamp(&format!("{root}/../store/s.jsonl"))
        );
        assert_eq!(
            call(&f, &held, "text", &req("GET", &up, b"", true)).code,
            "403 Forbidden",
            "no `..`"
        );
        for bad in ["--output=/tmp/x", "HEAD", "abc"] {
            let r = call(
                &f,
                &held,
                "text",
                &req("GET", &format!("{q}&rev={}", enc(bad)), b"", true),
            );
            assert_eq!(r.code, "400 Bad Request", "rev {bad:?} never reaches git");
        }
    }

    #[test]
    fn resolve_stamps_only_what_the_collection_explains_and_assets_are_images() {
        let f = fx("resolve");
        let held = Mutex::new(Held::default());
        let root = f.repo.display().to_string();
        let from = format!("{root}/docs/doc.md");
        let body = format!(
            r#"{{"from":{{"path":"docs/doc.md","cap":"{}"}},"targets":["docs/img.png","docs/missing.png","../store/s.jsonl","docs/other.md"]}}"#,
            stamp(&from)
        );
        let q = format!("root={}", enc(&root));
        let r = call(
            &f,
            &held,
            "resolve",
            &req("POST", &q, body.as_bytes(), true),
        );
        let caps = serde_json::from_slice::<Value>(&r.body).unwrap()["caps"].clone();
        assert_eq!(caps[0], json!(stamp(&format!("{root}/docs/img.png"))));
        assert_eq!(caps[1], Value::Null, "nothing there");
        assert_eq!(caps[2], Value::Null, "not inside the collection");
        assert_eq!(
            caps[3],
            json!(stamp(&format!("{root}/docs/other.md"))),
            "a link to follow"
        );
        let forged = body.replace(&stamp(&from), &"0".repeat(64));
        assert_eq!(
            call(
                &f,
                &held,
                "resolve",
                &req("POST", &q, forged.as_bytes(), true)
            )
            .code,
            "403 Forbidden",
            "the referrer is checked first"
        );

        let img = call(
            &f,
            &held,
            "asset",
            &req("GET", &doc_query(&f, "docs/img.png"), b"", true),
        );
        assert_eq!((img.code, img.content_type), ("200 OK", "image/png"));
        assert!(img
            .headers
            .iter()
            .any(|h| h == "X-Content-Type-Options: nosniff"));
        // Everything else exactly as `/file` serves it: text is READ as text/plain, never run.
        let txt = call(
            &f,
            &held,
            "asset",
            &req("GET", &doc_query(&f, "docs/notes.txt"), b"", true),
        );
        assert_eq!(
            (txt.code, txt.content_type),
            ("200 OK", "text/plain; charset=utf-8")
        );
        let md = call(
            &f,
            &held,
            "asset",
            &req("GET", &doc_query(&f, "docs/doc.md"), b"", true),
        );
        assert_eq!(
            md.code, "200 OK",
            "conform asks /asset for the document itself"
        );
        assert!(md
            .headers
            .iter()
            .any(|h| h == "Content-Security-Policy: sandbox"));
    }

    #[test]
    fn notes_go_through_mdrev_cli_and_writes_clear_the_write_bar() {
        let f = fx("notes");
        let held = Mutex::new(Held::default());
        let q = doc_query(&f, "docs/doc.md");
        let list = call(&f, &held, "annotations", &req("GET", &q, b"", true));
        assert_eq!(
            serde_json::from_slice::<Value>(&list.body).unwrap(),
            json!([{"id":"ann-1","body":"hi"}]),
            "the records, not the CLI's wrapper"
        );

        let note = br#"{"body":"tighten","anchor":{"exact":"Two"}}"#;
        assert_eq!(
            call(&f, &held, "annotations", &req("POST", &q, note, false)).code,
            "401 Unauthorized",
            "unpaired, the local collection is absent altogether"
        );
        let added = call(&f, &held, "annotations", &req("POST", &q, note, true));
        assert_eq!(added.code, "201 Created");
        assert_eq!(
            std::fs::read(f.kit.join("stdin.json")).unwrap(),
            note.to_vec(),
            "the record on stdin"
        );

        let patch = br#"{"status":"resolved","resolvedBy":{"note":"done"}}"#;
        let r = call(
            &f,
            &held,
            "annotations/ann-1",
            &req("PATCH", &q, patch, true),
        );
        assert_eq!(r.code, "200 OK", "{}", String::from_utf8_lossy(&r.body));
        assert!(
            calls(&f).contains("notes resolve ann-1 --note done --root"),
            "{}",
            calls(&f)
        );
        let reopen = call(
            &f,
            &held,
            "annotations/ann-1",
            &req("PATCH", &q, br#"{"status":"open"}"#, true),
        );
        assert_eq!(reopen.code, "200 OK");
        assert!(calls(&f).contains("notes reopen ann-1 --root"));
        let reply = call(
            &f,
            &held,
            "annotations/ann-1/replies",
            &req("POST", &q, br#"{"body":"ok"}"#, true),
        );
        assert_eq!(reply.code, "200 OK");
        assert!(calls(&f).contains("notes reply ann-1 --body ok --root"));
        let gone = call(
            &f,
            &held,
            "annotations/ann-1",
            &req("DELETE", &q, b"", true),
        );
        assert_eq!(gone.code, "204 No Content");
        let at = call(
            &f,
            &held,
            "annotations/ann-1/replies/2026-09-24T10%3A00%3A00Z",
            &req("DELETE", &q, b"", true),
        );
        assert_eq!(at.code, "200 OK");
        assert!(calls(&f).contains("notes delete-reply ann-1 --at 2026-09-24T10:00:00Z"));

        let missing = call(
            &f,
            &held,
            "annotations/ann-missing",
            &req("DELETE", &q, b"", true),
        );
        assert_eq!(missing.code, "404 Not Found", "exit 2 is no such note");
        assert!(String::from_utf8_lossy(&missing.body).contains("no such note"));
        assert_eq!(
            call(
                &f,
                &held,
                "annotations/ann-1",
                &req("PATCH", &q, br#"{"status":"gone"}"#, true)
            )
            .code,
            "400 Bad Request"
        );
        assert_eq!(
            call(
                &f,
                &held,
                "annotations/..%2Fx",
                &req("DELETE", &q, b"", true)
            )
            .code,
            "400 Bad Request",
            "an id is a plain token"
        );
        assert_eq!(
            call(&f, &held, "annotations/ann-1", &req("GET", &q, b"", true)).code,
            "405 Method Not Allowed"
        );
    }
}

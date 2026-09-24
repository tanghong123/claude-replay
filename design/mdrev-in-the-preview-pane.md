# mdrev in the preview pane (#270)

*Status: built in v1.296.0 (#270); mdrev PINNED in v1.297.0 (#274). The record of what was decided
and why.*

The owner, 2026-09-24: render the Markdown the app shell's right-most pane shows through
**mdrev's embedded viewer, as a guest** — a clean reader for Markdown the transcript itself
carries, and the viewer with its toolbar (redlines against history, review notes) for a Markdown
file read from the local disk. And: **use mdrev's released artifacts**.

mdrev's embedding guide and contract ship inside every mdrev release, and the pinned release's are
in this repository at `vendor/mdrev/release/docs/`. This page assumes them and says only what the
monitor does.

## Two modes, decided by where the text came from

| The pane shows | mdrev is mounted as | Declared |
|---|---|---|
| Markdown the transcript CARRIES — an attachment's `att_text` (`item.text != null`) | a plain reader | `review: false`, `annotate: false`, `isGit: false`, `toolbar: 'none'` |
| a Markdown FILE, read through its capability-stamped `/file` link | the whole viewer | `review: true`; `annotate: true` where the monitor has node to run mdrev's CLI, else `false`; history and notes from the file's own checkout |

"Markdown" is the tab's name ending in `.md`, `.markdown`, `.mdown` or `.mkd`. Everything else
the pane shows is unchanged: images through the shared image view, `.html` in its sandbox,
other text in a `<pre>`.

`toolbar: 'none'` is mdrev's own word for "a document rendered inside a page that has chrome of
its own": the chrome goes, the keys stay. The moment `review` or `annotate` is effective the
toolbar is always present and its controls are not the host's to hide — mdrev's rule, not ours.

## One release, pinned

The owner, 2026-09-25, after v1.296.0 had shipped a monitor that FOUND an installed mdrev when it
started: "only depend on static version of mdrev, similar to how agent-monitor depends on crates in
claude-replay. Future upgrades will be triggered explicitly and manually". And: pin **1.1.12**,
"the latest version today and has all the embedding features we need".

So mdrev is a crate in this workspace, `vendor/mdrev` (#274), and `claude-monitor` depends on it the
way it depends on the html crate. The crate holds the embedding kit of one PUBLIC mdrev release,
unmodified, under `release/`: `bundle/` (the guest: `mdrev.js`, `mdrev.css`, lazy `chunks/`,
`assets/`), `mdrev-cli.js` with its `package.json`, and `docs/*.md`. Its version is the release's,
so `Cargo.lock` names the pin. Its build script turns `bundle/` into a table of `include_bytes!`,
and both monitors serve that from memory. Nothing installed on the machine is looked at, and
`agent-replay`, which has no preview pane, carries none of it — the html crate defines the kit's
shape (`MdrevKit`) and never names the crate, and `claude_monitor::routes::handler`, the one
constructor both binaries go through, installs it.

- **Provenance.** The source is the public release's `mdrev-embed-<version>.tar.gz` — "the kit
  alone", in mdrev's release notes — checked against the sha256 digest the release publishes for
  it. For 1.1.12 the application tarball's kit and the corp tap's kit matched it byte for byte
  (bundle, CLI, `package.json`, contract); the corp edition's guides differ only in their install
  instructions, which is also why nothing is taken from it. `vendor/mdrev/README.md` has the URL,
  the checksum and the date; `release.sha256` has every file's checksum, and `cargo test -p mdrev`
  holds the tree to it, so a hand edit fails a gate.
- **Moving it.** `scripts/vendor-mdrev.sh <version>` is the only way: download, digest check, unpack,
  check (both entries, `package.json` naming the version, the CLI reporting the same version under
  node — the guide's §11: bundle and CLI from ONE release), replace `release/` whole, rewrite the
  manifest, the crate's version and the README's provenance rows, then `cargo check -p mdrev`. It
  refuses a kit older than **1.1.6**: `toolbar` arrived in 1.1.6-dev9 and the `review`/`annotate`
  ceilings in 1.1.6-dev11, and an older guest takes those options as nothing and would draw its
  toolbar over Markdown the owner asked to be read clean. A test holds the pin to the same floor. The
  bump is then reviewed (`git diff vendor/mdrev/release/docs/contract.md` says what the host must now
  honour), put through the gates and the full browser suite, and committed as a change of its own.
- **The CLI.** The binary writes `mdrev-cli.js` and its `package.json` into its scratch directory
  (`<scratch>/mdrev/<version>/`) the first time a route needs it, over whatever is there, and runs it
  as `node mdrev-cli.js … --root <root>`. node is found once: `MDREV_NODE` first and FINAL when set
  (mdrev's own launchers read it that way, and it is how a case takes node away), else the first
  `node` on `PATH`, else `/opt/homebrew/bin`, `/usr/local/bin` or Linuxbrew — a monitor started by
  launchd has no brew on its `PATH`. It must report 20 or later (the kit's README).
- **Without node.** The guest needs no node, and neither does history: the contract makes the
  host's store the source of revisions, so the monitor lists the NAME's history from `git log`
  (entries without `path`, the contract's shape for a host that does not follow renames). Notes are
  mdrev's own format, written only through its CLI, so without node `open` declares
  `annotate: false`, and mdrev hides its notes control and never asks the annotation routes. The
  monitor says so once, on stderr, when it first looks.

The bundle is served at `/mdrev/<version>/…` — a **versioned prefix**, because the two entries are
not content-hashed and a page must not keep a stale `mdrev.js` across a monitor upgrade that moved
the pin. Everything under it is cacheable forever. A page that asks for another version gets 404 and
reloads.

A server that installed no kit (`agent-replay --html`) answers every mdrev route 404 and names no
version, and the pane renders what it rendered before mdrev: the text in a `<pre>`. So does a
document mdrev cannot mount.

**What v1.296.0 did instead**, and why the pin is better on its own terms: the monitor looked for an
installed release at startup (`AGENT_MONITOR_MDREV`, else the `mdrev`/`mdrev-embed` kegs), because a
bundle compiled in beside a CLI that `brew upgrade` moves would break §11. The pin meets §11 by
construction — bundle and CLI come from the same vendored release, and neither moves unless the
repository does. It also removed the class of bug discovery had: a running monitor upgraded
underneath served the new tree's files under the old version's immutable prefix (#273, cancelled
with discovery). And the viewer now works on a machine with no mdrev installed, CI included.

## Both modes go through the contract

mdrev offers two seams: its HTTP contract (fifteen routes under a prefix the host chooses, versioned
with the guide and checked by `mdrev-cli conform`), and an in-process `client` for hosts that
already hold the documents. The second looks made for transcript text — the page has it — and is
not usable from a release: its interface is the viewer's whole I/O surface (thirteen required
methods), and `doc()` must return the document PRE-RENDERED, which the HTTP client does by calling
the renderer, the highlighter's language loader and the diagram and math hooks. The released
bundle exports three names — `HOST_CLASS`, `engine`, `mountMdrev` — and none of those internals. That
seam is for hosts that build mdrev from source, as its Obsidian plugin does. A host consuming the
release has the contract, so both modes use it, under `api/mdrev/`.

### Held documents — the transcript's own text

The page hands the text to the monitor — `POST api/mdrev/hold` with `{name, text}` — and gets back
a document it can mount: `{root: "held", path, cap}`. The monitor keeps held text
**content-addressed** (the same text is one entry however often it is opened) in a bounded
in-memory store, least recently used first out, and mints a capability for the path it chose. It
never touches the disk. A held document has no history (`/revisions` answers `[]`), no notes, and no
assets: a reference it makes resolves to nothing, because text that lives in a transcript has no
folder for a relative path to be relative to.

This is also what gives #271 (open the pane's document in a tab of its own) something to open.

### Local files — the `/file` guards, reused exactly

A local file reaches the pane already stamped: the server signed the link it rendered
(`Cap::File`, `claude-replay-html/src/html_export/sig.rs`). `GET api/mdrev/open?path=&sig=` checks
that stamp with the same four guards `/file` applies — pairing and a same-origin request, the
stamp, containment (a hosted session must explain the path), and the size cap — and answers the
mount's facts:

- `root` — the nearest ancestor directory holding `.git`, found on the path **as a string**, else
  the file's own directory with `isGit: false`. A string prefix, never a `realpath`: the stamp was
  minted for the path as the server rendered it, and `root + "/" + path` must reconstruct that
  string byte for byte, which a resolved `/tmp` → `/private/tmp` would not.
- `path` — the file relative to `root`.
- `cap` — **the file's own stamp**. mdrev's capability is an opaque string minted per (collection,
  path); the monitor already has one, so no new kind is minted for a file.

Every route about a local document rebuilds `root/path`, verifies the stamp against it, and applies
the guards again before touching the disk. The table:

| Route | The monitor answers | |
|---|---|---|
| `GET text?rev=current` | the file, as `/file` would read it | |
| `GET text?rev=<commit>` | `git show <commit>:<name>`, the name taken from the revisions below | `rev` must be hex — never an option git would parse |
| `GET revisions` | `mdrev-cli revisions --path P`, cached per `HEAD`; without node, the name's `git log` | what mdrev-v2 does: renames followed exactly as mdrev follows them — by the CLI only |
| `GET revisions` (no `path`) | `git log` of the collection | only for a root some hosted session explains |
| `GET asset` | a raster image, as `/file` serves one | SVG is script-bearing; not served as an image |
| `POST resolve` | a `Cap::File` stamp per target inside the root that passes containment and the render policy; `null` otherwise | the viewer draws a refusal and never asks |
| `GET/POST/PATCH/DELETE annotations…` | `mdrev-cli notes list/add/resolve/wontfix/reopen/reply/delete/delete-reply --root R` | writes are gated like the monitor's other writes; one document's calls are serialised; without node, refused (`annotate: false`, so never asked) |
| `GET snapshot?blob=` | `git cat-file -p <blob>` | `blob` must be hex |
| `GET stat` | `{mtimeMs}` | |
| `GET events` | 204 — the viewer polls | |
| `POST reveal`, `POST forget` | 501 | reveal is interim and will be replaced by a web file browser (#272); no new Finder path |
| `documents`, `tree`, `changed`, `recents`, `recent-changes` | 404 | a side pane has no file rail; absence hides the section |

**Writes.** A note is a write into the reader's checkout (`.mdrev/annotations/…`, self-ignored), so
it clears the bar every monitor write clears: the method the route names, a same-origin request, and
a paired monitor. `deny_write` grew a method set for `PATCH` and `DELETE`; the request parser
already read the method and the body for every verb.

**Bodies.** The parser used to cut every request body at 64 KB **silently** — a declared 100 KB body
arrived as its first 64 KB. That would corrupt a held document, and was never right for any route:
a body over its route's bound is now refused with 413, and `hold` has the artifact cap as its
bound.

## Keys belong to whoever the reader is engaged with

mdrev acts on `a`, `n`, `p`, `[`, `]`, `/` and `Escape` only while the reader is ENGAGED with the
mount — its own `scopeToElement`: the last mousedown or focusin landed inside it. The shell's keymap
listens on the document and knew nothing of that, so a single `n` would step the transcript's search
AND mdrev's next change. `shared/keymap.js` now tracks the same engagement in `bindKeymap` — the last
mousedown or focusin inside a `[data-guest-keys]` region — and yields every key while it holds, the
`when: "any"` bindings included; `resolveKey` also yields for a key whose target is inside one.

Engagement, not the keydown's target, because a click on a guest's prose leaves the focus on
`<body>`. Making the host focusable to change that was tried and is worse: a `tabindex` on mdrev's
host was measured turning a key press into thousands of keydowns in the test browser — which turned
out to be the INSTRUMENT (headless Chrome over CDP repeats any key the page does not prevent, with
nothing on the page at all), but the host stays unfocusable: there is no reason for it to be, and the
engagement rule is mdrev's own.

## Theme

The shell owns the theme (`documentElement.dataset.theme`), so the mount is told it — `theme:
'light' | 'dark'` — and mdrev drops its own theme control rather than offer a second switch that can
disagree. mdrev has no way to change the theme of a live mount, so a toggle remounts it; the
document, the range and the notes are all re-read from the same routes.

## Not in scope

**The classic page.** The owner named the right-most pane, which only the app shell has, and the
classic page is also the html crate's offline export, which has no server to answer a contract.

**A narrower toolbar.** The pane is 340–680 px and mdrev's one layout breakpoint is 820 px, so the
pane always gets mdrev's narrow layout. The owner declined a narrower toolbar inside mdrev
(`what-a-host-takes.md`, Decided §1); #271 opens a document in a tab of its own for the room.

## How it is held

- **The pin** (`vendor/mdrev`): the tree matches `release.sha256` byte for byte and holds nothing
  else; the embedded table is the `bundle/` directory as it stands; the crate's version is the
  release's `package.json`; the version is at least 1.1.6 and fit for a URL segment. Each was
  checked by mutation — a byte appended to `mdrev.css`, a stray file in `docs/`.
- **Unit** (`html_export/mdrev.rs`): the bundle from memory under its version (another version, `..`,
  a directory, the SVG, a missing name — all 404); finding node (`MDREV_NODE` final, `PATH`, the
  prefixes, 20 or later); writing the CLI beside its `package.json` and over a tampered copy; the
  held store (content addressing, the bound, the stamp); every local route's guards (unpaired,
  cross-origin, a wrong stamp, `..`, a non-hex `rev`); `revisions` and the note routes against a
  stand-in `mdrev-cli` that records its arguments; without node, history from git and notes
  refused; text at a revision against a fixture repository; 413 for an oversized body. And in
  `claude-monitor`: the handler both binaries build installs the pin — the page names its version
  and `/mdrev/<version>/mdrev.js` is the vendored file, byte for byte.
- **Browser** (app shell, real Chrome, the pinned guest in the binary under test): an attachment
  carrying Markdown mounts a reader with a rendered heading and no toolbar, on a page that names the
  pin; a committed file mounts the viewer with its toolbar and its notes control; a key pressed
  inside the mount leaves the transcript's search where it was (red before the keymap rule);
  without node (`MDREV_NODE` pointed at nothing) the file keeps its toolbar and its two revisions
  and mdrev hides its notes control — red when `open` grants notes regardless.
- **The contract itself**: the pinned `mdrev-cli conform`, run with node against a live paired
  monitor — every route, every shape, and a note filed, found in the sidecar, closed and deleted.
  The guide: when your host passes it, you are done.
- **CI** runs all of it: the guest is in the binary, so nothing needs installing; the browser job
  sets up node for the CLI.

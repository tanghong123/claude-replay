# mdrev in the preview pane (#270)

*Status: built in v1.296.0 (#270). The record of what was decided and why.*

The owner, 2026-09-24: render the Markdown the app shell's right-most pane shows through
**mdrev's embedded viewer, as a guest** — a clean reader for Markdown the transcript itself
carries, and the viewer with its toolbar (redlines against history, review notes) for a Markdown
file read from the local disk. And: **use mdrev's released artifacts**.

mdrev's embedding guide and contract ship inside every mdrev release, at
`<prefix>/opt/mdrev/libexec/docs/`; this page assumes them and says only what the monitor does.

## Two modes, decided by where the text came from

| The pane shows | mdrev is mounted as | Declared |
|---|---|---|
| Markdown the transcript CARRIES — an attachment's `att_text` (`item.text != null`) | a plain reader | `review: false`, `annotate: false`, `isGit: false`, `toolbar: 'none'` |
| a Markdown FILE, read through its capability-stamped `/file` link | the whole viewer | `review: true`, `annotate: true`; history and notes from the file's own checkout |

"Markdown" is the tab's name ending in `.md`, `.markdown`, `.mdown` or `.mkd`. Everything else
the pane shows is unchanged: images through the shared image view, `.html` in its sandbox,
other text in a `<pre>`.

`toolbar: 'none'` is mdrev's own word for "a document rendered inside a page that has chrome of
its own": the chrome goes, the keys stay. The moment `review` or `annotate` is effective the
toolbar is always present and its controls are not the host's to hide — mdrev's rule, not ours.

## The release is found, not built in

mdrev is public, in the same tap as agent-monitor (`tanghong123/tap/mdrev`), and its release tree
is a keg: `bundle/` (the guest: `mdrev.js`, `mdrev.css`, lazy `chunks/`, `assets/`), `mdrev-cli`
and a `package.json` naming the version. The monitor looks for ONE tree when it starts:

1. `AGENT_MONITOR_MDREV=<tree>` — explicit, and final: an explicit tree that is not valid means
   mdrev is off, never "try the next one" (that is how a case makes it absent on purpose);
2. `opt/mdrev/libexec`, then `opt/mdrev-embed/libexec` (the corp tap's kit-only keg), under each of
   `/opt/homebrew`, `/usr/local` and `/home/linuxbrew/.linuxbrew`.

A tree is valid when it has both bundle entries, a version, and a `mdrev-cli` — the keg's
`bin/mdrev-cli` wrapper first (it resolves node), else the tree's own launcher — and the version is
at least **1.1.6**. `toolbar` arrived in 1.1.6-dev9 and the `review`/`annotate` ceilings in
1.1.6-dev11; an older guest takes options it does not know as nothing and would draw its toolbar over
Markdown the owner asked to be read clean, so an older tree counts as none. Measured 2026-09-24: the
public tap's mdrev is 0.16.45, the corp tap's 1.1.12 — so today the viewer lights up for readers on
the corp tap, and for everyone else the day a 1.1.6+ is public.

Why not compile the bundle into the binary: it is 7.5 MB, and the guide's §11 requires the bundle
and `mdrev-cli` to come from **one release** ("they share the note record and the anchor format").
The CLI needs node and git on the machine anyway; a bundle frozen into agent-monitor beside a CLI
that `brew upgrade` moves is exactly the pairing §11 forbids. Found at runtime, the two are one keg
and upgrade together.

The bundle is served at `/mdrev/<version>/…` — a **versioned prefix**, because the two entries are
not content-hashed and a page must not keep a stale `mdrev.js` across an upgrade. Everything under
it is cacheable forever. A page that asks for another version gets 404 and reloads.

**Absent**, the pane renders exactly what it rendered before: the text in a `<pre>`. No mdrev, no
regression.

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
| `GET revisions` | `mdrev-cli revisions --path P`, cached per `HEAD` | what mdrev-v2 does: renames followed exactly as mdrev follows them |
| `GET revisions` (no `path`) | `git log` of the collection | only for a root some hosted session explains |
| `GET asset` | a raster image, as `/file` serves one | SVG is script-bearing; not served as an image |
| `POST resolve` | a `Cap::File` stamp per target inside the root that passes containment and the render policy; `null` otherwise | the viewer draws a refusal and never asks |
| `GET/POST/PATCH/DELETE annotations…` | `mdrev-cli notes list/add/resolve/wontfix/reopen/reply/delete/delete-reply --root R` | writes are gated like the monitor's other writes; one document's calls are serialised |
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

- **Unit** (`html_export/mdrev.rs`): discovery over fixture trees; the static prefix (version,
  traversal, media types); the held store (content addressing, the bound, the stamp); every local
  route's guards (unpaired, cross-origin, a wrong stamp, `..`, a non-hex `rev`); `revisions` and the
  note routes against a stand-in `mdrev-cli` that records its arguments; text at a revision against
  a fixture repository; 413 for an oversized body.
- **Browser** (app shell, real Chrome, the real release): an attachment carrying Markdown mounts a
  reader with a rendered heading and no toolbar; a committed file mounts the viewer with its toolbar;
  a key pressed inside the mount leaves the transcript's search where it was (red before the keymap
  rule); with mdrev absent the text is in a `<pre>` as before.
- **The contract itself**: `mdrev-cli conform` against a live paired monitor — every route, every
  shape, and a note filed, found in the sidecar, closed and deleted. The guide: when your host
  passes it, you are done.
- **CI** cannot provision a guest: no mdrev >= 1.1.6 is public yet, and the corp tap is out of its
  reach. The cases that need one are named `mdrev_guest_…` and the workflow skips that prefix by name,
  with the reason beside it — the `known_red_` convention, never a silent pass. The fallback case and
  the contract's unit tests (a stand-in `mdrev-cli`) run there; the guest cases run wherever mdrev is
  installed, and `harness::mdrev_release` panics naming the fix when it is not.

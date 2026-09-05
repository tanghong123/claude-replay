# The browser harness drives the developer's own Google Chrome

_Filed 2026-09-05. This repo's `--ignored` browser tests are the **dominant**
producer of a machine-wide problem: they launch the **installed** Google Chrome
(`com.google.Chrome`) — the same app bundle the developer browses with — which
(1) can steal the developer's interactive Chrome window, and (2) floods the disk
with macOS code-sign clones. A note, not a fix: proposed change below._

## Root cause

`claude-replay-browser-tests/tests/harness/mod.rs` launches Chrome via the
`headless_chrome` crate with **no explicit binary path**:

```rust
// tests/harness/mod.rs:590-604
pub fn chrome() -> headless_chrome::Browser {
    headless_chrome::Browser::new(
        headless_chrome::LaunchOptions::default_builder()
            .headless(true)
            .window_size(Some((1400, 900)))
            .args(vec![ /* throttling off */ ])
            .build()
            .unwrap(),
    )
    .expect("chrome launches")
}
```

With `path` unset, `headless_chrome` auto-detects a browser and on macOS resolves
to **`/Applications/Google Chrome.app`** (bundle id `com.google.Chrome`) — the
developer's daily browser. macOS registers exactly one app per bundle id, so:

- **Window theft.** Launching a second instance of `com.google.Chrome` reconciles
  to the single "Google Chrome" application; the developer's interactive window is
  the casualty (it vanishes; a normal relaunch only re-activates the automation
  instance). Intermittent — only at test browser start/teardown.
- **Code-sign clone flood.** macOS makes a full copy-on-write clone of the entire
  `Google Chrome.app` bundle for each second-instance launch, into the per-user
  temp dir (`$TMPDIR/../X/com.google.Chrome.code_sign_clone/`), and never reaps
  them. One launch per verify run × two runners in parallel = the bulk of the pile.

## Evidence (2026-09-05)

Watching the clone directory with crux-web's deck idle, **15 clones appeared in 3
minutes**, each within seconds of a fresh:

```
/Applications/Google Chrome.app/Contents/MacOS/Google Chrome \
  --remote-debugging-port=NNNN \
  --user-data-dir=~/personal/claude-replay/target/rust-headless-chrome-profileXXXX
    ↑ parent: claude-replay/target/debug/deps/scenarios-… --ignored
```

`rust-headless-chrome-profileXXXX` is the `headless_chrome` crate's default temp
profile name; the `scenarios-… --ignored` parent is
`cargo test -p claude-replay-browser-tests -- --ignored`. At the time the machine
held **~4,500 clones spanning ~2.3 days**, the large majority attributable here.
(Cross-repo write-up of the same mechanism: `~/code/crux-web/tools/webtape/BROWSER-COLLISION.md`,
which fixed its side by moving off `com.google.Chrome`.)

## Proposed fix: launch a browser whose bundle id is NOT com.google.Chrome

Point `chrome()` at an isolated browser binary. Then the harness never touches
`com.google.Chrome` — no stolen window, no clones — and real Chrome fidelity is
kept if you use Chrome for Testing.

**Preferred — explicit path to Chrome for Testing** (bundle id
`com.google.chrome.for.testing`, coexists with stable). A Chrome-for-Testing
binary already exists on this machine (Playwright installed one for crux-web):

```rust
// tests/harness/mod.rs — in chrome(), on default_builder():
    .path(Some(std::path::PathBuf::from(
        std::env::var("CLAUDE_REPLAY_CHROME")
            .expect("set CLAUDE_REPLAY_CHROME to a Chrome-for-Testing binary"),
    )))
```

Make it opt-in-with-fallback if you prefer CI ergonomics: use the env var when
set, else fall back to the crate's `default_executable()` (documenting that the
fallback re-enables the collision).

**Lightest to try — the `CHROME` env var.** `headless_chrome`'s
`default_executable()` consults `$CHROME` before probing bundle paths (verify for
crate v1.0.22), so exporting `CHROME=/path/to/chrome-for-testing` in the test
environment may isolate it with no code change. Confirm against the pinned crate
version before relying on it.

**Alternative — bundled Chromium.** Enable the `headless_chrome` fetcher to
download and run its own Chromium (`org.chromium.Chromium`, distinct bundle).
Heavier (~150 MB) and drops branded-Chrome fidelity, but fully self-contained.

## Meanwhile (machine-level, already installed)

The symptom is mopped up by `~/.local/bin/code-sign-clone-reaper` (a per-user
LaunchAgent that deletes clone bundles older than a day; source:
`~/personal/claude-toolbox/macos-cleanup/`). It only bounds the mess — it does
not stop production. The fix above is what stops it at the source for this repo.

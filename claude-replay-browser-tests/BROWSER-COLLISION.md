# The browser harness drives an ISOLATED browser, not the developer's Chrome

_Filed 2026-09-05, fixed the same day (#u21). Kept as the record of WHY `chrome()` names its
binary, so nobody "simplifies" the path away._

`headless_chrome` with no `path` calls its own `default_executable()`, which on macOS falls
through to `/Applications/Google Chrome.app` — the bundle the developer browses with
(`com.google.Chrome`). macOS registers one app per bundle id, so launching a second instance
reconciles to the single application, which can take the developer's window, and mints a
copy-on-write clone of the whole bundle under
`$TMPDIR/../X/com.google.Chrome.code_sign_clone/` that nothing reaps.

Measured before the fix: 4,405 clones on this machine, 481 of them inside three hours of
running these suites — one per browser launch, and this suite launches one per case (~110 in
a full gate). Copy-on-write, so the cost is inode and metadata pressure, not bytes; `du` on
that directory took minutes.

`harness::browser_path` now names the binary: `CLAUDE_REPLAY_CHROME` when set, else a Chrome
for Testing (`com.google.chrome.for.testing`) from `/Applications` or Playwright's cache, else
the crate's own detection — CI has its own Chrome and no developer window to lose. Measured
after: 33 launches, zero new clones.

The machine-level mop-up (`~/.local/bin/code-sign-clone-reaper`, a LaunchAgent that deletes
clones older than a day) bounds what other producers leave behind; this stops this repo's.

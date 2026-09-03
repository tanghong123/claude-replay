# claude-replay-browser-tests

The real-Chrome harness for the HTML frontend and the monitor's shells. Every case is
`#[ignore]`d and runs only on request:

```sh
cargo build --release -p claude-monitor -p claude-monitor-v2
cargo test -p claude-replay-browser-tests -- --ignored --skip known_red
```

It needs a local Chrome (headless_chrome finds it on PATH). CI runs the same command in its
`browser` job.

## Layout

- `tests/harness/mod.rs` — the kit. Scratch roots and serialization (`base`, `serial`,
  `Reap`); Claude-format record builders (`user`, `assistant`, `tool_open`/`tool_result`,
  `thinking`, `agent_spawn`/`agent_result`, the now-relative `*_at` variants for records that
  must read as live) and `long_session`; `Stores`, a hermetic world of agent stores under the
  case's scratch root (`claude_session`, `claude_child`, `qoderwork_family`, `claude_finished`,
  and `envs()` — every store variable the monitors and adapters honour); `Monitor::spawn(kind,
  port, base, stores, paired)` (reaped on drop, `pair()`/`open()`, a PANIC naming the build when
  the binary is missing); `chrome()` with timer throttling off; `eval`/`probe`/`until`; and the
  two-surface vocabulary — `Surface::{Classic, AppShell}`, `turn_at_top`, `at_tail`,
  `scroll_by`, `jump_to_end`, `open_last_fold`, `mounted_turns`, `LiveGrowth`.
- `tests/browser_follow.rs` — the structural cases: the html server's viewport contract
  (follow/anchor, search stepping, resize, artifacts), the app shell (`the_app_shell_*`,
  ports 2831–2836), the classic rail on v1 (`the_classic_rail_*`, 2837–2838), the v2 splice,
  the compose affordance (2841–2842).
- `tests/scenarios.rs` — scenarios written once and run against BOTH pages. The classic page
  (the html server's `export.js`) is the reference; the app shell is held to the same
  assertions. Ports 2851 and up.

## Adding a scenario

1. Write it once: `fn scenario_x(tab: &Tab, surface: Surface, fx: &Fixture)`, speaking in the
   vocabulary — user-turn ordinals (`turn_at_top`), the tail (`at_tail`), the reader's scroll
   (`scroll_by`, which sends the wheel intent first — a bare programmatic scroll reads as the
   renderer's own and the follow logic re-pins), folds (`open_last_fold`), growth
   (`LiveGrowth::start(path, script, interval)` — the interval must exceed the slower
   consumer's poll; drop the driver before the case ends).
2. Add one `#[test]` per surface: `open(Surface::Classic, &fx, 0)` (the html server,
   in-process) and `open(Surface::AppShell, &fx, <port>)` (a paired v2 monitor on a hermetic
   store; pick an unused port from 2851 up).
3. If a surface's result is a QUEUED bug, name that test with `known_red_<task>`: the gate
   skips it, the fix removes the marker, and the assertion is never weakened. The classic page
   is the reference for the app shell, not an oracle — a scenario can find a classic-page bug
   too (#71 did), and then both tests carry the marker.

## Rules the cases keep

- Every case takes `serial()` first — fixed ports, a shared state dir.
- Nothing a case measures comes from this machine's sessions: build the world in `Stores`.
- A wait is `until(…)`, which panics with what it last saw. A skip is a failure.
- When a shell times out with nothing rendered, read Chrome's console before diagnosing:
  `chrome --headless=new --enable-logging=stderr --v=0 <url>` prints `CONSOLE … Uncaught …`
  with the file and line.

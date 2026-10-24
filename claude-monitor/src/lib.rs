//! `claude-monitor` as a LIBRARY — the machine-wide session index (#98) plus the control
//! plane (#133), so a second front-end can reuse them instead of forking them.
//!
//! The binary (`agent-monitor`, `src/main.rs`) is the v1 rail over these modules;
//! `agent-monitor-v2` composes the same session view without an iframe and drives the same
//! `Index`, `ConsentStore`, `Passcode` and send transports through here. Splitting the crate
//! this way is what keeps ONE implementation of "may this prompt be injected into that pane".

#[cfg(feature = "codex-native")]
pub use claude_monitor_codex as codex_gateway;
#[cfg(feature = "codex-native")]
pub mod codex_workspace;
pub mod consent;
pub mod control;
pub mod cost;
pub mod index;
pub mod pair_phone;
pub mod routes;
pub mod state;
pub mod ui;

/// An executable script for a test to run: `body` at `path`, mode 0755 (#s241).
///
/// The FINAL file is written by a `cp` child, never by this process. On Linux, exec'ing a file
/// that any process holds open for writing fails with ETXTBSY, and a test thread that forks while
/// another is writing hands its child a copy of that write descriptor until the child execs — so
/// write-then-run in a parallel test binary fails at random, and only on Linux (macOS does not
/// refuse). CI met it in the herdr send test; the root crate's `jdi::write_script` and the html
/// crate's mdrev tests learned it first. Only the draft is ours to write, and nothing runs it.
#[cfg(all(test, unix))]
pub(crate) fn write_script(path: &std::path::Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let draft = path.with_extension("draft");
    std::fs::write(&draft, body).unwrap();
    let copied = std::process::Command::new("cp")
        .arg(&draft)
        .arg(path)
        .status()
        .unwrap();
    assert!(
        copied.success(),
        "cp {} {}",
        draft.display(),
        path.display()
    );
    std::fs::remove_file(&draft).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

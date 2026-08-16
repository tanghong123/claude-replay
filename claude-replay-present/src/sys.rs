//! Small OS/path helpers shared by BOTH frontends (the TUI's dump path and the
//! HTML exporters/server) — no terminal or HTML dependencies.
//!
//! Including where a RUN puts its own directories (#165). That is deliberately here and not in
//! `cache`: the cache owns where the SHARED durable entries live
//! ([`cache::admit::default_root`](crate::cache::admit::default_root)), and a client that wants a
//! private cache of its own hands one in. A cache implementation that also invented private roots
//! for its callers would be two policies in one place.

use claude_replay_core::discover;
use std::path::{Path, PathBuf};

/// Reveal a path in the OS file manager (a benign, read-only side effect from an
/// explicit click). macOS: `open -R <file>` selects it in Finder / `open <dir>`
/// for a directory. Linux: `xdg-open` the containing directory. Spawned detached
/// so it never blocks or disturbs the TUI; failures are ignored (no file manager).
pub fn reveal_in_file_manager(path: &Path) {
    let is_dir = path.is_dir();
    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        if is_dir {
            cmd.arg(path);
        } else {
            cmd.arg("-R").arg(path);
        }
        let _ = cmd.spawn();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No generic reveal-and-select on Linux/other; open the folder itself.
        let dir = if is_dir {
            path
        } else {
            path.parent().unwrap_or(path)
        };
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
}

/// Re-root an absolute `path` from under `old_root` to `new_root`, component-aware.
/// A path not under `old_root` is returned unchanged — so an out-of-repo target
/// (`/etc/hosts`) survives a re-root untouched, and `/old/repofoo` is never matched
/// by `/old/repo`.
///
/// Pure: no disk access. It is the reveal ACTION's companion to
/// [`project_path`](discover::project_path) — when a repo moved, a tool/attachment path was
/// recorded under the now-dead `old_root` (its [`first_cwd`](discover::first_cwd)); this
/// swaps that prefix for the live project path so the file opens at its real location.
/// The caller decides whether the result exists — this only rewrites the string.
pub fn relocate(path: &Path, old_root: &Path, new_root: &Path) -> PathBuf {
    match path.strip_prefix(old_root) {
        Ok(rest) => new_root.join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// The root a `--no-cache` run hands to [`SessionCache::durable`](crate::SessionCache::durable)
/// as its own private cache: `<cache home>/throwaway/<pid>/`.
///
/// `--no-cache` does not mean "no cache": it means *not the shared one*. The run gets the same
/// implementation at a private root, so folding, locking and resume all work exactly as they
/// always do — what it gives up is coordination with other viewers, which is the entire point of
/// the flag (a second, independent view of a session someone else is holding).
///
/// Pid-keyed, and that does not break the "no pid in a durable path" rule: this root is not
/// durable. Two `--no-cache` runs have no root lock making them single-entity, so a *shared*
/// throwaway root would have them denying each other every session — the flag defeating itself.
/// The pid is also what lets [`reclaim`] tell a crashed run's leftovers from a live run's cache.
///
/// Infallible where the durable root is not: a throw-away root may be guessed at.
pub fn throwaway_root() -> PathBuf {
    run_space().join("throwaway").join(pid())
}

/// This run's bundle directory: `<cache home>/runs/<pid>/` — the served page's static shell and
/// per-session artifacts. Per-RUN, so concurrent runs cannot wipe each other's, and [`reclaim`]
/// can take it back once the run is gone.
pub fn run_dir() -> PathBuf {
    run_space().join("runs").join(pid())
}

/// Take back what dead runs left behind: `throwaway/<pid>` and `runs/<pid>` whose pid is gone,
/// plus the legacy `$TMPDIR/claude-replay` tree both used to live in.
///
/// Called from the CLI entry point and from each frontend's startup — whichever comes first, and
/// only ONCE per process: the walk costs a `readdir` and a liveness probe per candidate, and
/// nothing it finds can appear again mid-run. A crashed run leaves its directory and the next
/// start reclaims it — the same shape as the monitor's scratch (#162), and the reason these trees
/// need no cleanup logic of their own.
pub fn reclaim() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(reclaim_now);
}

fn reclaim_now() {
    for kind in ["throwaway", "runs"] {
        let Ok(entries) = std::fs::read_dir(run_space().join(kind)) else {
            continue;
        };
        for e in entries.flatten() {
            let Some(owner) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
                continue; // not ours to judge
            };
            if owner != std::process::id() && !crate::cache::lock::pid_alive(owner) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    reclaim_legacy_temp();
}

/// Where this run's own directories go. The cache home when there is one — everything the tool
/// writes then lives in one place a person can find, inspect and delete — and temp only when
/// nothing resolves, since a throw-away tree would rather land somewhere than nowhere.
fn run_space() -> PathBuf {
    crate::cache::admit::cache_home().unwrap_or_else(|| std::env::temp_dir().join("claude-replay"))
}

fn pid() -> String {
    std::process::id().to_string()
}

/// The pre-#165 bundle location, `$TMPDIR/claude-replay/{<session>|multi-<pid>}`. Nothing writes
/// there any more, but an OLDER build on the same machine still might — so only entries that have
/// gone quiet for an hour are taken, and the parent goes with a plain `remove_dir`, which
/// succeeds only once it is empty.
fn reclaim_legacy_temp() {
    const QUIET: std::time::Duration = std::time::Duration::from_secs(3600);
    let legacy = std::env::temp_dir().join("claude-replay");
    if legacy == run_space() {
        return; // no cache home resolved: this IS where we write
    }
    let Ok(entries) = std::fs::read_dir(&legacy) else {
        return;
    };
    let now = std::time::SystemTime::now();
    for e in entries.flatten() {
        let idle = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok());
        if matches!(idle, Some(d) if d >= QUIET) {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    let _ = std::fs::remove_dir(&legacy);
}

/// Deduce the default dump stem: `<basename>-<pathhash>-<sessionid>-<width>` where
/// basename/pathhash come from the session's project cwd, sessionid is its first 6
/// chars, and width is the render width. cwd/sessionId are read from the transcript via
/// the agent-neutral `discover` helpers (so a Codex rollout — whose cwd/id live under
/// `payload` — deduces a correct stem, not the `"session"` fallback a Claude-only scan gave).
pub fn deduce_stem(path: &Path, width: Option<usize>) -> String {
    use std::hash::{Hash, Hasher};
    let cwd = discover::first_cwd(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let basename = Path::new(&cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("session");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    let pathhash: String = format!("{:016x}", h.finish())[..6].to_string();
    let sid = discover::session_id(path).unwrap_or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string()
    });
    let sid6: String = sid.chars().take(6).collect();
    // `--dump` suffixes the render width (its output is width-specific); the HTML
    // export reflows in the browser, so it passes `None` and omits it.
    match width {
        Some(w) => format!("{basename}-{pathhash}-{sid6}-{w}"),
        None => format!("{basename}-{pathhash}-{sid6}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relocate_swaps_a_dead_root_prefix_but_leaves_outsiders_alone() {
        let old = Path::new("/old/repo");
        let new = Path::new("/new/repo");
        // A file recorded under the dead root re-roots to the live one.
        assert_eq!(
            relocate(Path::new("/old/repo/src/a.rs"), old, new),
            PathBuf::from("/new/repo/src/a.rs")
        );
        // The root itself re-roots to the new root.
        assert_eq!(relocate(old, old, new), PathBuf::from("/new/repo"));
        // Out-of-repo target: not under old_root → unchanged (a move doesn't touch it).
        assert_eq!(
            relocate(Path::new("/etc/hosts"), old, new),
            PathBuf::from("/etc/hosts")
        );
        // Component-aware: `/old/repofoo` is NOT under `/old/repo`.
        assert_eq!(
            relocate(Path::new("/old/repofoo/x"), old, new),
            PathBuf::from("/old/repofoo/x")
        );
    }

    /// `$CLAUDE_REPLAY_CACHE` is process-global and cargo runs these on parallel threads: hold
    /// this for the whole env-scoped window, or a test points the home somewhere and another
    /// reads the developer's REAL one mid-assertion.
    static CACHE_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn home(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-runspace-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::env::set_var("CLAUDE_REPLAY_CACHE", &d);
        d
    }

    /// The layout invariant #165 rests on: the two throw-away trees are SIBLINGS of the shared
    /// durable root, never inside it. `admit::gc` walks `<root>/<presentation>/<entry>`, so a
    /// run directory nested under `sessions/` would read as a presentation namespace and its
    /// contents as entries to reap — the sweep would eat a live run's cache.
    #[test]
    fn run_directories_are_siblings_of_the_shared_root() {
        let _env = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = home("siblings");
        let shared = crate::cache::admit::default_root().expect("home resolves");
        assert_eq!(shared, d.join("sessions"));
        for run in [throwaway_root(), run_dir()] {
            assert!(
                !run.starts_with(&shared),
                "{} must not be inside {}",
                run.display(),
                shared.display()
            );
            assert!(run.starts_with(&d), "…but still under the one cache home");
            assert_eq!(
                run.file_name().unwrap().to_string_lossy(),
                std::process::id().to_string(),
                "keyed by the run that owns it, so `reclaim` can tell the dead from the live"
            );
        }
        assert_ne!(throwaway_root(), run_dir());
        std::env::remove_var("CLAUDE_REPLAY_CACHE");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Reclaim takes a crashed run's directory and leaves a live one's — the whole cleanup
    /// story for both trees, and why neither needs logic of its own.
    #[test]
    fn reclaim_takes_dead_runs_and_spares_live_ones() {
        let _env = CACHE_ENV.lock().unwrap_or_else(|e| e.into_inner());
        let d = home("reclaim");
        // 999_999 is not a running process (the same stand-in `admit`'s gc tests use); our own
        // pid always is.
        assert!(!crate::cache::lock::pid_alive(999_999));
        let dead = d.join("throwaway").join("999999");
        let mine = throwaway_root();
        let bundle_dead = d.join("runs").join("999999");
        let named = d.join("runs").join("not-a-pid"); // nothing we may judge
        for p in [&dead, &mine, &bundle_dead, &named] {
            std::fs::create_dir_all(p).unwrap();
            std::fs::write(p.join("x"), b"x").unwrap();
        }
        reclaim_now(); // not `reclaim`: its once-per-process guard would make this order-dependent
        assert!(!dead.exists(), "a dead run's cache is taken back");
        assert!(!bundle_dead.exists(), "and its bundle with it");
        assert!(mine.exists(), "a live run's is not");
        assert!(
            named.exists(),
            "and a directory we cannot judge is left alone"
        );
        std::env::remove_var("CLAUDE_REPLAY_CACHE");
        let _ = std::fs::remove_dir_all(&d);
    }
}

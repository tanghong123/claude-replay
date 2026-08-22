//! Reading the user's own SSH config — the only honest source of "what machines are there".
//!
//! Discovery must not invent names. A tool that goes looking for `dev`, or `prod`, or the
//! author's two boxes, finds them on exactly one laptop. So the candidate list comes from the
//! file the user already maintains for `ssh` itself, and nothing is added to it here.
//!
//! Only literal `Host` names are candidates. `Host *`, `Host build-*` and `Host !bad` are
//! patterns, not destinations — `ssh build-*` connects to nothing — so they are skipped. The
//! parse is deliberately shallow: `Host` and `Include`, nothing else. Everything that makes a
//! host reachable (user, port, jump host, proxy command, keys) stays where it already works, in
//! that file, applied by `ssh` when we invoke it.

use std::path::{Path, PathBuf};

/// The SSH config to enumerate: `$AGENT_MONITOR_FLEET_SSH_CONFIG`, else `~/.ssh/config`.
///
/// The override is not a nicety — it is how the tests enumerate a config with no relation to the
/// machine they run on, which is the only way to prove nothing is seeded.
pub fn default_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_MONITOR_FLEET_SSH_CONFIG")
        .or_else(|| std::env::var_os("CLAUDE_MONITOR_FLEET_SSH_CONFIG"))
        .map(PathBuf::from)
    {
        return Some(p);
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".ssh").join("config"))
}

/// Literal `Host` names from `at`, following `Include`, in file order, first occurrence kept.
///
/// A missing or unreadable file yields no candidates — a machine with no SSH config has no
/// remote environments, which is a legitimate state and not an error.
pub fn candidates(at: &Path) -> Vec<String> {
    let mut out = Vec::new();
    collect(at, 0, &mut out);
    // Not `dedup()`: that collapses only ADJACENT repeats, and a file that includes itself — or two
    // files that both include a third — repeats hosts far apart, which would probe one machine
    // several times and list it several times.
    let mut seen = std::collections::HashSet::new();
    out.retain(|h| seen.insert(h.clone()));
    out
}

/// `Include` can nest; a cycle would otherwise recurse forever. Eight is far past any real
/// config and cheap to bound.
const MAX_DEPTH: u8 = 8;

fn collect(at: &Path, depth: u8, out: &mut Vec<String>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(text) = std::fs::read_to_string(at) else {
        return;
    };
    for line in text.lines() {
        // OpenSSH allows `Key = value`; treat `=` as whitespace so `Host = foo` still parses.
        let line = line.trim();
        let line = line.split('#').next().unwrap_or("").replace('=', " ");
        let mut words = line.split_whitespace();
        let Some(key) = words.next() else { continue };
        match key.to_ascii_lowercase().as_str() {
            "host" => out.extend(words.filter(|w| is_literal(w)).map(str::to_string)),
            "include" => {
                for token in words {
                    for path in expand(token, at) {
                        collect(&path, depth + 1, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Whether a `Host` token names one destination rather than matching a set of them.
fn is_literal(word: &str) -> bool {
    !word.is_empty() && !word.contains(['*', '?', '!'])
}

/// Resolve an `Include` token the way `ssh` does, minus the parts nobody writes: `~` is the
/// user's home, a relative path is relative to the including file's directory, and `*` in the
/// final component is expanded against that directory.
fn expand(token: &str, including: &Path) -> Vec<PathBuf> {
    let base = including.parent().unwrap_or(Path::new("."));
    let raw = match token.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(rest),
            None => return Vec::new(),
        },
        None if Path::new(token).is_absolute() => PathBuf::from(token),
        None => base.join(token),
    };
    let Some(name) = raw.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    if !name.contains('*') {
        return vec![raw];
    }
    let dir = raw.parent().unwrap_or(Path::new(".")).to_path_buf();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut hits: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| e.file_name().to_str().is_some_and(|n| glob_match(name, n)))
        .map(|e| e.path())
        .collect();
    // read_dir order is the filesystem's; sort so the candidate list is the same on every run.
    hits.sort();
    hits
}

/// Whether `name` matches a `*`-only glob. Several stars are normal in these includes
/// (`conf.d/*-*`), and a matcher that only handled one silently expanded to nothing — the failure
/// mode being a host the user has configured and this tool never mentions.
fn glob_match(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let Some(first) = parts.next() else {
        return true;
    };
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail = parts.collect::<Vec<_>>();
    let Some((last, middle)) = tail.split_last() else {
        // No star at all: the whole pattern had to be a prefix, and an exact match.
        return rest.is_empty();
    };
    for part in middle {
        // Leftmost match is enough: the parts are literals, so a later window can only match if an
        // earlier one already did.
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.len() >= last.len() && rest.ends_with(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cmf-ssh-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The anti-seeding guard: with no config there are no candidates. Not "the usual suspects",
    /// not localhost — nothing.
    #[test]
    fn no_config_means_no_candidates() {
        let missing = scratch("absent").join("config");
        assert!(candidates(&missing).is_empty());
    }

    /// Patterns are not destinations. This is the difference between offering the user their
    /// machines and offering them `*`.
    #[test]
    fn patterns_are_not_candidates() {
        let dir = scratch("patterns");
        let cfg = write(
            &dir,
            "config",
            "Host *\n  ServerAliveInterval 30\n\nHost alpha\n  HostName 10.0.0.1\n\
             \nHost tier-*\n  User deploy\n\nHost !nope beta\n  Port 2222\n\
             \nhost = gamma\n  # lowercase key, `=` form\n",
        );
        assert_eq!(candidates(&cfg), ["alpha", "beta", "gamma"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Split configs are normal (`Include ~/.orbstack/ssh/config`, `Include conf.d/*`), and a
    /// host we cannot see is a host the user has to add by hand.
    #[test]
    fn includes_are_followed_and_globs_expanded() {
        let dir = scratch("include");
        write(&dir.join("conf.d"), "10-one", "Host one\n");
        write(&dir.join("conf.d"), "20-two", "Host two\n");
        write(&dir.join("conf.d"), "ignored.bak", "Host nope\n");
        write(&dir, "extra", "Host three\n");
        let cfg = write(
            &dir,
            "config",
            "Include conf.d/*-*\nInclude extra\nHost four\nInclude config\n",
        );
        // `Include config` is a cycle: the depth bound makes it finite and the dedup makes the
        // result stable rather than one copy of every host per pass.
        assert_eq!(candidates(&cfg), ["one", "two", "three", "four"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The matcher these includes actually need. `conf.d/*-*` is a shape people write, and one that
    /// silently expanded to nothing would hide a machine the user HAS configured.
    #[test]
    fn globs_may_have_several_stars() {
        assert!(glob_match("*-*", "10-one"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("conf-*.cfg", "conf-a.cfg"));
        assert!(!glob_match("*-*", "nodash"));
        assert!(!glob_match("*.bak", "10-one"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "exactly"));
        // A star must not let two parts match the same text: `b*a` needs an `a` AFTER a `b`.
        assert!(!glob_match("b*a", "ab"));
    }

    /// Comments must not become host names — `#Host old-box` is a host the user turned OFF.
    #[test]
    fn comments_are_not_hosts() {
        let dir = scratch("comments");
        let cfg = write(&dir, "config", "#Host commented\nHost real # trailing\n");
        assert_eq!(candidates(&cfg), ["real"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

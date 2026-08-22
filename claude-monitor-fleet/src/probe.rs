//! Finding a monitor without being told where it is.
//!
//! A monitor's port is **not** a constant. `claude-monitor` defaults to 2727 but takes `--port`,
//! and a second monitor on the same machine needs a second port and its own `$CLAUDE_MONITOR_CACHE`
//! (one monitor per cache root, #160/#166). So a fleet that forwards to 2727 forwards to whichever
//! monitor happens to hold that port — or to nothing.
//!
//! It does not have to guess: a bound monitor PUBLISHES where it serves. `claim_root` writes
//! `{"pid":…,"note":{"port":…}}` to `<cache root>/LOCK` the moment the listener binds
//! (`claude-monitor/src/main.rs`). Reading that file is the whole discovery mechanism — it
//! reports the port the monitor actually took, on the machine it actually runs on.
//!
//! The same shell program runs locally and remotely, over `sh -s` with the snippet on stdin.
//! One implementation means the local tab cannot drift from the remote ones, and passing the
//! optional cache root as `$1` instead of interpolating it keeps a path with a space or a quote
//! in it from becoming a shell injection.

use anyhow::{Context, Result};
use std::path::Path;

/// A monitor found at a cache root.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    /// The cache root whose lock named it — also how two monitors on one host are told apart.
    pub root: String,
    /// The port it published when it bound.
    pub port: u16,
    /// The pid in the lock.
    pub pid: u32,
    /// Whether that pid answered `kill -0` **on its own machine**. Advisory only: a monitor run
    /// by another user is alive but not signalable, so this being `Some(false)` is a hint, never
    /// a reason to skip the host. The real check is whether the forwarded port serves HTTP.
    pub alive: Option<bool>,
    /// What that machine calls itself — `/etc/machine-id` where there is one, its hostname
    /// otherwise, empty if it would say neither. This is how ONE machine reached through two
    /// `Host` aliases is recognised as one machine instead of offered as two identical tabs.
    pub machine: String,
}

/// Resolve monitor cache roots and print one line per lock found, preceded by who this machine is.
///
/// Roots considered, in order: `$1` (an explicit root the user configured), `$CLAUDE_MONITOR_CACHE`,
/// then every `claude-monitor*` directory under `$XDG_CACHE_HOME` or `~/.cache`. That last glob is
/// what finds a second monitor without being told its name — `claude-monitor-next`, `-staging`,
/// whatever the user called it — and it is a heuristic about THIS TOOL's own directory naming, not
/// about the user's machines. A root somewhere else entirely is reachable with `--cache-root`.
///
/// A non-interactive `ssh host sh` does not read `.zshrc`/`.bashrc`, so `$CLAUDE_MONITOR_CACHE`
/// will usually be unset remotely even when the user's shell sets it. The glob covers the common
/// case; `--cache-root` covers the rest. Printing to stdout with a fixed prefix keeps MOTD banners
/// and shell noise from being parsed as results.
const SNIPPET: &str = r#"
set -u
explicit=${1:-}
base=${XDG_CACHE_HOME:-}
[ -n "$base" ] || base=${HOME:-}/.cache
id=$(cat /etc/machine-id 2>/dev/null | head -n1)
[ -n "$id" ] || id=$(hostname 2>/dev/null | head -n1)
printf 'ID\t%s\n' "${id:-}"
roots=$explicit
if [ -n "${CLAUDE_MONITOR_CACHE:-}" ]; then roots="$roots
$CLAUDE_MONITOR_CACHE"; fi
for d in "$base"/claude-monitor*; do
  if [ -d "$d" ]; then roots="$roots
$d"; fi
done
printf '%s\n' "$roots" | awk 'length && !seen[$0]++' | while IFS= read -r r; do
  [ -f "$r/LOCK" ] || continue
  lock=$(tr -d '\n' < "$r/LOCK")
  pid=$(printf '%s' "$lock" | sed -n 's/.*"pid":[[:space:]]*\([0-9][0-9]*\).*/\1/p')
  alive='?'
  if [ -n "$pid" ]; then
    if kill -0 "$pid" 2>/dev/null; then alive=1; else alive=0; fi
  fi
  printf 'MON\t%s\t%s\t%s\n' "$r" "$alive" "$lock"
done
"#;

/// Ask a machine which monitors it is running. `ssh` `None` ⇒ this machine.
///
/// `interactive` decides whether `ssh` may ask the user anything. A deliberate connection to a
/// configured environment may (that is the user's own machine, and a passphrase prompt is the
/// expected way in), but a survey of every host in an SSH config must not: one host waiting for a
/// passphrase would stall a scan the user did not target at it. So discovery passes `false` and
/// skips whatever cannot answer unattended.
///
/// stderr is inherited on purpose: an SSH key passphrase prompt, a host-key confirmation or an
/// auth failure is something the user needs to see and answer, and swallowing it to make an
/// error message tidier would turn "type your passphrase" into "timed out".
pub fn probe(
    ssh: Option<&str>,
    ssh_options: &[String],
    cache_root: Option<&str>,
    interactive: bool,
) -> Result<Vec<Found>> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let root = cache_root.unwrap_or("");
    let mut cmd = match ssh {
        None => {
            let mut c = Command::new("sh");
            c.arg("-s").arg("--").arg(root);
            c
        }
        Some(host) => {
            let mut c = Command::new("ssh");
            c.arg("-T")
                .args([
                    "-o",
                    if interactive {
                        "BatchMode=no"
                    } else {
                        "BatchMode=yes"
                    },
                ])
                .args(["-o", "ConnectTimeout=10"])
                .args(ssh_options)
                .arg(host)
                // The remote shell re-splits what ssh sends, so the root is quoted for it.
                .arg(format!("sh -s -- {}", single_quote(root)));
            c
        }
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| match ssh {
            None => "run sh".to_string(),
            Some(h) => format!("run ssh {h}"),
        })?;
    child
        .stdin
        .take()
        .context("probe stdin")?
        .write_all(SNIPPET.as_bytes())
        .context("send the probe program")?;
    let out = child.wait_with_output().context("probe")?;
    if !out.status.success() {
        anyhow::bail!(
            "probe failed on {} ({})",
            ssh.unwrap_or("this machine"),
            out.status
        );
    }
    Ok(parse(&String::from_utf8_lossy(&out.stdout)))
}

/// Turn the probe's stdout into results, ignoring everything that is not one of its own lines.
pub fn parse(stdout: &str) -> Vec<Found> {
    let mut machine = String::new();
    let mut out = Vec::new();
    for line in stdout.lines() {
        let mut f = line.split('\t');
        match f.next() {
            Some("ID") => {
                machine = f.next().unwrap_or("").trim().to_string();
                continue;
            }
            Some("MON") => {}
            _ => continue,
        }
        let (Some(root), Some(alive), Some(lock)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        let Some((pid, port)) = lock_fields(lock) else {
            // A lock with no port is a monitor that took the root but has not bound yet — a real
            // state (`claude-monitor` documents it), and nothing to forward to.
            continue;
        };
        out.push(Found {
            root: root.to_string(),
            port,
            pid,
            alive: match alive {
                "1" => Some(true),
                "0" => Some(false),
                _ => None,
            },
            machine: machine.clone(),
        });
    }
    out
}

/// What makes two findings the SAME monitor: one machine, one cache root, one process.
///
/// Two `Host` aliases for one box (a short name and a fully-qualified one, a direct route and one
/// through a jump host) are two destinations and one machine, and forwarding to both would give the
/// user two tabs showing the identical page. When a machine will not say who it is, the destination
/// name stands in — an unknown identity must never merge two hosts that are genuinely different.
pub fn machine_key(dest: Option<&str>, f: &Found) -> (String, String, u32) {
    let who = if f.machine.is_empty() {
        format!("dest:{}", dest.unwrap_or("local"))
    } else {
        f.machine.clone()
    };
    (who, f.root.clone(), f.pid)
}

/// `{"pid":…,"dir":…,"note":{"port":…}}` → `(pid, port)`. The lock is written by the monitor with
/// `serde_json`, so it is parsed as JSON rather than pattern-matched.
fn lock_fields(lock: &str) -> Option<(u32, u16)> {
    let v: serde_json::Value = serde_json::from_str(lock).ok()?;
    let pid = v.get("pid")?.as_u64()? as u32;
    let port = v.get("note")?.get("port")?.as_u64()?;
    (port > 0 && port <= u16::MAX as u64).then_some((pid, port as u16))
}

/// Wrap a string so a POSIX shell sees it as one literal word.
fn single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// A label for an environment found at `root` on `host`, derived from what was found rather than
/// chosen for the user: the host's own name, plus the root's distinguishing suffix when there is
/// more than one monitor there (`…/claude-monitor-next` ⇒ `-next`). Names are only ever
/// suggestions — `add --name` overrides, and the config is editable.
pub fn suggest_name(host: Option<&str>, root: &str) -> String {
    let base = host.unwrap_or("local");
    let leaf = Path::new(root)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    match leaf.strip_prefix("claude-monitor") {
        // The default root: the host name alone is unambiguous.
        Some("") => base.to_string(),
        // `…/claude-monitor-next` ⇒ `<host>-next`.
        Some(rest) => format!("{base}{rest}"),
        // A root named something else entirely (`--cache-root /opt/mon`). Still distinct, because
        // two entries with one name would silently overwrite each other in the config.
        None if leaf.is_empty() => base.to_string(),
        None => format!("{base}-{leaf}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe's output survives whatever the login shell prints around it — banners, MOTD,
    /// `Last login:` — because only its own prefixed lines are read.
    #[test]
    fn only_the_probes_own_lines_are_parsed() {
        let stdout = "Last login: Tue\n\
             ID\tf00dcafe\n\
             MON\t/home/u/.cache/claude-monitor\t1\t{\"pid\":42,\"dir\":\"/x\",\"note\":{\"port\":2727}}\n\
             some banner text\n\
             MON\t/home/u/.cache/claude-monitor-next\t0\t{\"pid\":7,\"note\":{\"port\":31999}}\n\
             MON\t/broken\t?\tnot json\n\
             MON\t/starting\t1\t{\"pid\":9,\"note\":null}\n";
        assert_eq!(
            parse(stdout),
            [
                Found {
                    root: "/home/u/.cache/claude-monitor".into(),
                    port: 2727,
                    pid: 42,
                    alive: Some(true),
                    machine: "f00dcafe".into(),
                },
                Found {
                    root: "/home/u/.cache/claude-monitor-next".into(),
                    port: 31999,
                    pid: 7,
                    alive: Some(false),
                    machine: "f00dcafe".into(),
                },
            ],
            "a lock with no published port is a monitor that has not bound — not a target"
        );
    }

    /// A machine that will not name itself (no `/etc/machine-id`, no `hostname`) must still be
    /// probed — it just cannot take part in dedup, which `machine_key` handles by falling back to
    /// the destination.
    #[test]
    fn an_unidentified_machine_is_still_reported() {
        let found =
            parse("ID\t\nMON\t/c/claude-monitor\t1\t{\"pid\":1,\"note\":{\"port\":9001}}\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].machine, "");
    }

    /// The reason machine identity is collected at all: one box reached by two `Host` aliases is
    /// one monitor, and two boxes that cannot identify themselves are still two.
    #[test]
    fn two_aliases_for_one_machine_share_a_key() {
        let via_short = parse(
            "ID\tsame-box\nMON\t/c/claude-monitor\t1\t{\"pid\":5,\"note\":{\"port\":2727}}\n",
        );
        // The same monitor seen through a different alias: another destination, another local
        // forward, but the same machine id, root and pid.
        let via_long = parse(
            "ID\tsame-box\nMON\t/c/claude-monitor\t1\t{\"pid\":5,\"note\":{\"port\":2727}}\n",
        );
        assert_eq!(
            machine_key(Some("build"), &via_short[0]),
            machine_key(Some("build.example.internal"), &via_long[0]),
            "two aliases for one box must collapse to one environment"
        );

        let anon = parse("MON\t/c/claude-monitor\t1\t{\"pid\":5,\"note\":{\"port\":2727}}\n");
        assert_ne!(
            machine_key(Some("boxA"), &anon[0]),
            machine_key(Some("boxB"), &anon[0]),
            "without an identity, distinct destinations stay distinct"
        );
    }

    /// Two monitors on one machine is the case a fixed port cannot express, and the case that
    /// proves the port is read rather than assumed: neither of these is the default.
    #[test]
    fn a_second_monitor_is_found_at_its_own_port() {
        let found = parse(
            "MON\t/c/claude-monitor\t1\t{\"pid\":1,\"note\":{\"port\":10001}}\n\
             MON\t/c/claude-monitor-staging\t1\t{\"pid\":2,\"note\":{\"port\":10002}}\n",
        );
        assert_eq!(found.len(), 2);
        assert_ne!(found[0].port, found[1].port);
        assert_eq!(suggest_name(Some("box"), &found[0].root), "box");
        assert_eq!(suggest_name(Some("box"), &found[1].root), "box-staging");
        assert_eq!(suggest_name(None, &found[1].root), "local-staging");
    }

    #[test]
    fn a_root_path_reaches_the_shell_as_one_word() {
        assert_eq!(single_quote("/a b/c"), "'/a b/c'");
        assert_eq!(single_quote("it's"), r"'it'\''s'");
        assert_eq!(single_quote(""), "''");
    }

    /// The local probe runs the same program the remote one does, so this exercises the snippet
    /// itself: a lock written into a temp root must come back with its port.
    #[test]
    fn the_local_probe_reads_a_lock_it_is_pointed_at() {
        let root =
            std::env::temp_dir().join(format!("cmf-probe-{}/claude-monitor", std::process::id()));
        let _ = std::fs::remove_dir_all(root.parent().unwrap());
        std::fs::create_dir_all(&root).unwrap();
        let me = std::process::id();
        std::fs::write(
            root.join("LOCK"),
            format!(
                r#"{{"pid":{me},"dir":"{}","note":{{"port":65123}}}}"#,
                root.display()
            ),
        )
        .unwrap();

        let root_str = root.to_str().unwrap();
        let found = probe(None, &[], Some(root_str), true).unwrap();
        let mine = found
            .iter()
            .find(|f| f.root == root_str)
            .expect("the root we pointed it at");
        assert_eq!(mine.port, 65123);
        assert_eq!(mine.pid, me);
        assert_eq!(mine.alive, Some(true), "our own pid is alive");

        let _ = std::fs::remove_dir_all(root.parent().unwrap());
    }
}

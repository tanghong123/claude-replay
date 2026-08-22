//! The fleet is **data, not code**. Everything about a user's topology — how many machines,
//! what they are called, how to reach them, which port a monitor happens to serve on — lives
//! in one JSON file that this crate ships EMPTY.
//!
//! That is the whole point of the file existing. The obvious way to aggregate several monitors
//! is a script with the hosts written into it, and that script is correct exactly once, on the
//! machine of the person who wrote it: a second user's hosts have different names, their
//! monitors bind different ports, and reconfiguring means editing source. So there is no
//! default host list here, no default port ladder, and no host name anywhere in this crate.
//! [`Fleet::default`] is empty and the tests hold it to that.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The schema version written to disk. A file from the future is refused rather than
/// half-understood — misreading a field silently would point tunnels at wrong ports.
pub const VERSION: u32 = 1;

/// One place a monitor runs. Named by the user, because only the user knows what to call it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Env {
    /// Display label and the key for `add`/`remove`. Arbitrary — a hostname, a role, a nickname.
    pub name: String,
    /// The SSH destination to tunnel through, exactly as `ssh` would take it. `None` means
    /// **this machine**: no tunnel, the iframe points straight at the local monitor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,
    /// Extra arguments handed to `ssh` verbatim (`-J bastion`, `-p 2222`, `-o …`). The user's
    /// `~/.ssh/config` already covers most of this; this is for what it does not. Nothing about
    /// jump hosts, proxies or ports is assumed on the user's behalf.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_options: Vec<String>,
    /// The monitor cache root to read the served port out of, if the default resolution
    /// ([`crate::probe`]) does not find it — e.g. a monitor started with an explicit
    /// `$CLAUDE_MONITOR_CACHE` somewhere unusual. Resolved **on that machine**, not here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_root: Option<String>,
    /// Pin the monitor's port instead of discovering it. Discovery is the norm; this exists for
    /// the case where the lock file cannot be read (a container, a different user's monitor)
    /// but the port is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl Env {
    /// Whether this entry describes the machine the fleet page itself runs on.
    pub fn is_local(&self) -> bool {
        self.ssh.is_none()
    }
}

/// The persisted fleet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fleet {
    pub version: u32,
    /// Empty by construction. A shipped default list would be somebody else's machines.
    #[serde(default)]
    pub environments: Vec<Env>,
}

impl Default for Fleet {
    fn default() -> Self {
        Self {
            version: VERSION,
            environments: Vec::new(),
        }
    }
}

impl Fleet {
    /// Add `env`, replacing any entry with the same name. Returns whether it replaced one — the
    /// caller says "added" or "updated" rather than making the user diff the file to find out.
    pub fn upsert(&mut self, env: Env) -> bool {
        match self.environments.iter_mut().find(|e| e.name == env.name) {
            Some(slot) => {
                *slot = env;
                true
            }
            None => {
                self.environments.push(env);
                false
            }
        }
    }

    /// Drop the entry called `name`. Returns whether there was one.
    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.environments.len();
        self.environments.retain(|e| e.name != name);
        self.environments.len() != before
    }

    /// Parse a config file's bytes. A file from a newer schema is an error, not a guess.
    pub fn parse(text: &str) -> Result<Self> {
        let fleet: Self = serde_json::from_str(text).context("parse fleet config JSON")?;
        anyhow::ensure!(
            fleet.version <= VERSION,
            "fleet config is version {} but this build understands at most {VERSION} — upgrade \
             claude-monitor-fleet",
            fleet.version
        );
        Ok(fleet)
    }
}

/// Where the fleet config lives: `$CLAUDE_MONITOR_FLEET_CONFIG` (absolute only), else
/// `$XDG_CONFIG_HOME/claude-monitor/fleet.json`, else `~/.config/claude-monitor/fleet.json`.
///
/// Config, not cache: this is a thing a person edits and keeps, so it does not belong under the
/// monitor's cache root — which the monitor is free to wipe, and which `--port`/`$CLAUDE_MONITOR_CACHE`
/// can move out from under it. The env var comes first so a test, or a second fleet, can have
/// its own file without touching the user's.
pub fn path() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_MONITOR_FLEET_CONFIG")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return Ok(p);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .ok_or_else(|| anyhow::anyhow!("no $HOME — nowhere to keep the fleet config"))?;
    Ok(base.join("claude-monitor").join("fleet.json"))
}

/// Read the fleet. **A missing file is an empty fleet, not an error**: the first run of any
/// command must work on a machine that has never seen this tool.
pub fn load(at: &Path) -> Result<Fleet> {
    match std::fs::read_to_string(at) {
        Ok(text) => Fleet::parse(&text).with_context(|| format!("read {}", at.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Fleet::default()),
        Err(e) => Err(e).with_context(|| format!("read {}", at.display())),
    }
}

/// Write the fleet, atomically — a torn config would be indistinguishable from a corrupt one.
/// Pretty-printed with a trailing newline, because this file is meant to be opened and edited
/// by hand as much as by this tool.
pub fn save(at: &Path, fleet: &Fleet) -> Result<()> {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut body = serde_json::to_string_pretty(fleet).context("serialize fleet config")?;
    body.push('\n');
    let tmp = at.with_extension("json.tmp");
    std::fs::write(&tmp, &body).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, at).with_context(|| format!("replace {}", at.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guard that keeps this crate honest: **nothing is assumed about the user's machines.**
    /// A default fleet is empty, so every host, every port and every hop is something the user
    /// (or discovery, which reads the user's own SSH config) put there.
    #[test]
    fn a_default_fleet_has_no_hosts() {
        let fleet = Fleet::default();
        assert!(
            fleet.environments.is_empty(),
            "a shipped host list would be someone else's machines"
        );
        assert_eq!(fleet.version, VERSION);
    }

    /// A machine that has never run this tool has no config file, and every command must still
    /// work there — `list` prints nothing, `discover` still probes, `up` explains itself.
    #[test]
    fn a_missing_file_reads_as_an_empty_fleet() {
        let missing = std::env::temp_dir().join("cmf-nonexistent-dir/fleet.json");
        assert_eq!(load(&missing).unwrap(), Fleet::default());
    }

    #[test]
    fn a_fleet_round_trips_through_the_file() {
        let dir = std::env::temp_dir().join(format!("cmf-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let at = dir.join("nested").join("fleet.json");

        let mut fleet = Fleet::default();
        assert!(!fleet.upsert(Env {
            name: "here".into(),
            ..Default::default()
        }));
        assert!(!fleet.upsert(Env {
            name: "over-there".into(),
            ssh: Some("some-host".into()),
            ssh_options: vec!["-J".into(), "some-bastion".into()],
            port: Some(31234),
            ..Default::default()
        }));
        save(&at, &fleet).unwrap();
        assert_eq!(load(&at).unwrap(), fleet, "what we wrote is what we read");

        // Same name ⇒ replaced, not duplicated: re-running `discover --add` must converge.
        assert!(fleet.upsert(Env {
            name: "over-there".into(),
            ssh: Some("some-host".into()),
            ..Default::default()
        }));
        assert_eq!(fleet.environments.len(), 2);
        assert_eq!(fleet.environments[1].port, None, "the new entry won");

        assert!(fleet.remove("here"));
        assert!(!fleet.remove("here"), "removing twice is not an error");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Absent optional fields must not be written, so a hand-edited file stays readable and a
    /// future field can be added without rewriting everyone's config.
    #[test]
    fn a_local_entry_serializes_to_just_its_name() {
        let fleet = Fleet {
            version: VERSION,
            environments: vec![Env {
                name: "here".into(),
                ..Default::default()
            }],
        };
        let text = serde_json::to_string(&fleet).unwrap();
        assert_eq!(text, r#"{"version":1,"environments":[{"name":"here"}]}"#);
        assert_eq!(Fleet::parse(&text).unwrap(), fleet);
    }

    #[test]
    fn a_config_from_the_future_is_refused() {
        let e = Fleet::parse(r#"{"version":99,"environments":[]}"#).expect_err("must refuse");
        assert!(e.to_string().contains("version 99"), "{e}");
    }

    /// `ssh: null` is not a missing value to be filled in with a guess — it IS the answer:
    /// this machine, no tunnel.
    #[test]
    fn no_ssh_destination_means_this_machine() {
        assert!(Env {
            name: "here".into(),
            ..Default::default()
        }
        .is_local());
        assert!(!Env {
            name: "there".into(),
            ssh: Some("h".into()),
            ..Default::default()
        }
        .is_local());
    }
}

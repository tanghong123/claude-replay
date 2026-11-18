//! One writer per cache root (#160, #166, #s426): the claim the monitor takes before it opens
//! its root, and the one a host takes before it mounts the route table over a root of its own.
//!
//! The cache root is single-writer by construction (#96): every entry a peer holds is one this
//! process would be DENIED, and two writers would corrupt the record logs. Binding 2727 twice
//! fails, but `--port` walks past it, so the lock is what actually enforces it.
//!
//! The lock's note says who holds it (`holder`): a MONITOR, whose published port a second
//! monitor hands off to, or a HOST, which serves the table inside a server of its own and is
//! never handed off to. A note without `holder` is a monitor's from before #s426. A host never
//! hands off either: it mounts the table in its own server, so a held root is an error naming
//! the holder, and the way out is a root of its own.

use anyhow::Result;
use claude_replay_present::cache::lock;
use std::path::Path;

/// Who takes the root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Holder {
    /// The `agent-monitor` binary: a second one hands off to the first.
    Monitor,
    /// A server that mounts the route table (`design/backend-mount.md`).
    Host,
}

impl Holder {
    fn word(self) -> &'static str {
        match self {
            Holder::Monitor => "monitor",
            Holder::Host => "host",
        }
    }
}

/// What claiming the cache root came to.
#[derive(Debug)]
pub enum Claimed {
    /// The root is ours: stand the server up.
    Ours,
    /// A live monitor already serves this root, here. Only a monitor is handed off.
    Served(String),
}

/// Take the cache root as `who`, or learn where the monitor that already has it serves.
///
/// Being second is not an error for a monitor: it means the thing you asked for is already
/// running. `claude-replay --html` has always handed a second invocation to the first
/// ([`claude_replay_html::existing_server`]), and a second monitor does the same.
///
/// The URL comes from the note, published once the listener binds ([`publish`]). A holder that
/// has taken the lock but not bound yet is a real window, not a dead process: it counts as live,
/// and it has no URL, so that one IS an error rather than a hand-off to nowhere. A dead holder is
/// reclaimed by `acquire`, which is what makes a killed monitor's lock harmless. A lock we cannot
/// WRITE is reported and ignored: a temp I/O fault should not stop the tool.
///
/// A host's claim writes its note at once (it publishes no port), so a monitor started over the
/// same root is told a host holds it, never sent to the host's server. With no port to probe, a
/// host's claim lasts as long as its pid is alive: if a host dies and another process of the same
/// user is given its pid, the root stays refused until that process exits too, and the refusal
/// names the pid.
pub fn claim(root: &Path, who: Holder) -> Result<Claimed> {
    let holder = match lock::acquire::<serde_json::Value>(root, |h| {
        lock::pid_alive(h.pid) && port_answers(note_port(h.note.as_ref()))
    }) {
        Ok(lock::Taken::Owned) => {
            if who == Holder::Host {
                publish(root, Holder::Host, serde_json::json!({}));
            }
            return Ok(Claimed::Ours);
        }
        Ok(lock::Taken::Held(h)) => h,
        Err(e) => {
            eprintln!(
                "warning: could not take the root lock at {}: {e}",
                root.display()
            );
            return Ok(Claimed::Ours);
        }
    };
    let by_host = note_holder(holder.note.as_ref()) == Some("host");
    if by_host || who == Holder::Host {
        let what = if by_host {
            "a host serving agent-monitor's table"
        } else {
            "agent-monitor"
        };
        anyhow::bail!(
            "{what} (pid {}) holds the cache root {}: give this {} a root of its own.",
            holder.pid,
            root.display(),
            who.word()
        );
    }
    match note_port(holder.note.as_ref()) {
        Some(port) => Ok(Claimed::Served(format!("http://127.0.0.1:{port}/"))),
        None => anyhow::bail!(
            "agent-monitor is starting up in another process (pid {}) — it holds this cache \
             root but has not published a port yet. Try again in a moment, or stop it first.",
            holder.pid
        ),
    }
}

/// Write the holder's note into the root lock: `note`'s fields and `holder`. A monitor publishes
/// its port once the listener has bound. The note is advisory after that: a write failure is one
/// stderr line and never turns a working server into a failed start.
pub fn publish(root: &Path, who: Holder, mut note: serde_json::Value) {
    if let Some(fields) = note.as_object_mut() {
        fields.insert("holder".into(), who.word().into());
    }
    if let Err(error) = lock::publish(root, note) {
        eprintln!(
            "warning: publish the {} endpoint and runtime identity to {}: {error:#}",
            who.word(),
            root.join("LOCK").display()
        );
    }
}

/// The port out of a root lock's note, which is plain JSON here.
fn note_port(note: Option<&serde_json::Value>) -> Option<u16> {
    note?.get("port")?.as_u64().map(|p| p as u16)
}

fn note_holder(note: Option<&serde_json::Value>) -> Option<&str> {
    note?.get("holder")?.as_str()
}

/// Whether a holder's published port still answers. Same rule the html server's rendezvous
/// uses: a pid alone is not enough (pids are recycled), and a holder that has taken the lock
/// but not yet bound (`None`), or a host (which publishes none), counts as live.
fn port_answers(port: Option<u16>) -> bool {
    let Some(port) = port else { return true };
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_runtime_note_failure_does_not_abort_a_bound_listener() {
        let root = std::env::temp_dir().join(format!("monitor-note-file-{}", std::process::id()));
        std::fs::write(&root, "a file cannot contain LOCK").unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        publish(
            &root,
            Holder::Monitor,
            serde_json::json!({"port":address.port()}),
        );
        assert!(std::net::TcpStream::connect(address).is_ok());
        assert!(root.is_file());
        std::fs::remove_file(root).unwrap();
    }

    /// A live pid that is not ours, and a lock written as that pid with `note`.
    struct Peer {
        child: std::process::Child,
    }

    impl Peer {
        fn spawn() -> Self {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .unwrap();
            Self { child }
        }

        fn hold(&self, root: &Path, note: serde_json::Value) {
            std::fs::write(
                lock::lock_path(root),
                serde_json::json!({"pid": self.child.id(), "dir": root, "note": note}).to_string(),
            )
            .unwrap();
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            self.child.kill().ok();
            self.child.wait().ok();
        }
    }

    /// #166: being second is not an error — it means what you asked for is already running.
    ///
    /// Covers all four ways the root lock can read: free, held by a live monitor that published
    /// where it serves (hand off), held by one that has taken the lock but not bound (no target,
    /// so this one really is an error), and held by a dead pid (reclaimed).
    #[test]
    fn a_second_monitor_hands_off_instead_of_failing() {
        let root = std::env::temp_dir().join(format!("cm-claim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(
            matches!(claim(&root, Holder::Monitor).unwrap(), Claimed::Ours),
            "a free root is ours"
        );
        // Our own lock does not lock us out (`acquire` never denies its own pid).
        assert!(matches!(
            claim(&root, Holder::Monitor).unwrap(),
            Claimed::Ours
        ));

        // A peer that really exists: a live pid that is not ours, on a port that really answers.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = Peer::spawn();
        let peer_pid = peer.child.id();

        // A monitor from before #s426 names no holder; one since names itself.
        for note in [
            serde_json::json!({ "port": port }),
            serde_json::json!({ "port": port, "holder": "monitor" }),
        ] {
            peer.hold(&root, note);
            match claim(&root, Holder::Monitor).unwrap() {
                Claimed::Served(url) => assert_eq!(url, format!("http://127.0.0.1:{port}/")),
                Claimed::Ours => panic!("a live monitor's root must not be taken from it"),
            }
        }

        // It holds the lock but has not bound: a real window, and no URL to send anyone to.
        peer.hold(&root, serde_json::Value::Null);
        let e = claim(&root, Holder::Monitor).expect_err("no port ⇒ nowhere to hand off to");
        assert!(
            e.to_string().contains(&peer_pid.to_string()),
            "the refusal names the holder: {e}"
        );

        // A dead holder is reclaimed, whoever holds that port now.
        drop(peer);
        std::fs::write(
            lock::lock_path(&root),
            serde_json::json!({"pid": peer_pid, "dir": root, "note": {"port": port}}).to_string(),
        )
        .unwrap();
        assert!(
            matches!(claim(&root, Holder::Monitor).unwrap(), Claimed::Ours),
            "a dead monitor does not keep the root"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// #s426: a host takes a root of its own. A monitor over a host's root is refused, never
    /// handed off to the host's server, and a host over a monitor's root is refused too, even
    /// one whose port answers. A host's note names it, so a monitor started next can tell.
    #[test]
    fn a_host_and_a_monitor_never_share_a_root_or_hand_off_to_each_other() {
        let root = std::env::temp_dir().join(format!("cm-claim-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();

        assert!(matches!(claim(&root, Holder::Host).unwrap(), Claimed::Ours));
        let ours: serde_json::Value =
            serde_json::from_slice(&std::fs::read(lock::lock_path(&root)).unwrap()).unwrap();
        assert_eq!(ours["note"]["holder"], "host", "{ours}");

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let peer = Peer::spawn();
        let peer_pid = peer.child.id().to_string();

        // A host holds it, even one whose note carries a port that answers.
        for note in [
            serde_json::json!({ "holder": "host" }),
            serde_json::json!({ "holder": "host", "port": port }),
        ] {
            peer.hold(&root, note);
            let e = claim(&root, Holder::Monitor).expect_err("a host's root is not handed off");
            let e = e.to_string();
            assert!(e.contains("a host") && e.contains(&peer_pid), "{e}");
            let e = claim(&root, Holder::Host).expect_err("nor shared with another host");
            assert!(e.to_string().contains(&peer_pid), "{e}");
        }

        // A monitor holds it and serves: a host is refused, not sent there.
        peer.hold(
            &root,
            serde_json::json!({ "port": port, "holder": "monitor" }),
        );
        let e = claim(&root, Holder::Host).expect_err("a monitor's root is not a host's");
        let e = e.to_string();
        assert!(
            e.contains("agent-monitor") && e.contains(&peer_pid) && e.contains("host"),
            "{e}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}

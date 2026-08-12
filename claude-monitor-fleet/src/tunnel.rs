//! The tunnel: `ssh -L <free local port>:127.0.0.1:<the port the monitor published>`.
//!
//! Two things here are deliberate, and both are places a hand-rolled aggregator goes wrong.
//!
//! **The remote port comes from discovery, never from a constant.** A forward whose remote side
//! is a literal 2727 lands on whichever monitor holds 2727 — possibly an older build, possibly
//! nothing. [`Tunnel::open`] takes the port [`crate::probe`] read out of that machine's own lock.
//!
//! **The local port is allocated, not assigned.** A ladder (first host gets N, second N+1, …)
//! collides with whatever else the user is running, and the collision surfaces as an empty tab.
//! Here the kernel picks a free port, and the tunnel is not reported up until the forward answers
//! HTTP — so a failure is a failure at start, with ssh's own diagnostics on the terminal, rather
//! than a blank iframe later.
//!
//! **The tunnel is tied to this process by a pipe, not only by `Drop`.** Rust runs no destructor
//! when the process is signalled, so `Drop` alone loses to `Ctrl-C`, `pkill` and `kill -9` — and a
//! leaked `ssh` keeps a forward open on a port the user thinks is free, reparented to init, where
//! only a manual `pgrep` finds it. So ssh is given [`KEEPALIVE`] instead of `-N` and a pipe for its
//! stdin, whose write end this process holds: however this process dies, the kernel closes that
//! end, the far side reads EOF and exits, and ssh follows it. See [`Tunnel::open`].
//!
//! The remote side is `127.0.0.1` because a monitor binds loopback only, by design. The tunnel is
//! what makes it reachable, which is exactly the mechanism `design/claude-monitor.md` names for
//! remote access: "an SSH tunnel, not a flag".

use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// What the far end runs instead of `ssh -N`: a reader that ends when its input does.
///
/// This is the whole orphan-prevention mechanism, so it must stay a command that (a) terminates on
/// stdin EOF and (b) needs nothing but a POSIX shell — the same assumption [`crate::probe`] already
/// makes. `sleep`, `-N`, or anything that ignores stdin would compile, run, and quietly restore the
/// leak.
const KEEPALIVE: &str = "cat >/dev/null";

/// A live forward, held for as long as it should exist.
///
/// Dropping it takes the tunnel down — the child is killed and reaped — and the `stdin` pipe covers
/// the paths where `Drop` never runs: a signal, or the process being killed outright.
#[derive(Debug)]
pub struct Tunnel {
    child: Child,
    /// The write end of ssh's stdin. Never written to; its purpose is to be open, and to be closed
    /// by the kernel when this process ends however it ends.
    _stdin: ChildStdin,
    local_port: u16,
}

impl Tunnel {
    /// Open a forward to `remote_port` on `host` and return once it serves HTTP.
    ///
    /// `attempts` covers the unavoidable race in "ask the kernel for a free port, then hand that
    /// number to another process": between the two, something else can take it. `ExitOnForwardFailure`
    /// turns that into an immediate ssh exit instead of a tunnel that exists but forwards nothing,
    /// and a retry gets a different port.
    pub fn open(
        host: &str,
        ssh_options: &[String],
        remote_port: u16,
        attempts: u8,
    ) -> Result<Self> {
        let mut last = None;
        for _ in 0..attempts.max(1) {
            let local_port = free_port()?;
            let mut child = Command::new("ssh")
                .arg("-T")
                .args(["-o", "ExitOnForwardFailure=yes"])
                .args(["-o", "ServerAliveInterval=15"])
                .args(["-o", "ServerAliveCountMax=3"])
                .args(["-o", "ConnectTimeout=10"])
                .args(ssh_options)
                .arg("-L")
                .arg(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"))
                .arg(host)
                // Not `-N`: this command is what makes the far end hang up when our stdin pipe
                // closes, which is what stops a signalled exit from leaking the forward.
                .arg(KEEPALIVE)
                // stdin is a pipe we keep, NOT the terminal — that is the orphan guard, and it
                // costs the user nothing: OpenSSH asks for passphrases and host-key confirmations
                // on `/dev/tty`, not on stdin, and stderr stays attached so those prompts and any
                // auth failure are still the user's to read and answer.
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .with_context(|| format!("spawn ssh for {host}"))?;
            let stdin = child.stdin.take().context("ssh stdin")?;

            let mut tunnel = Self {
                child,
                _stdin: stdin,
                local_port,
            };
            if tunnel.wait_until_serving(Duration::from_secs(20)) {
                return Ok(tunnel);
            }
            // Distinguish "ssh gave up" (bad forward, auth, unreachable) from "the far side is not
            // a monitor" — the second is not worth retrying a different local port for.
            let exited = tunnel.child.try_wait().ok().flatten().is_some();
            drop(tunnel);
            last = Some(if exited {
                anyhow::anyhow!("ssh exited before the forward to {host}:{remote_port} came up")
            } else {
                anyhow::anyhow!(
                    "the forward to {host}:{remote_port} is up but nothing answered HTTP there \
                     — is that monitor still running?"
                )
            });
            if !exited {
                break;
            }
        }
        Err(last.unwrap_or_else(|| anyhow::anyhow!("no attempt was made")))
    }

    /// The loopback port on THIS machine that now reaches that monitor.
    pub fn local_port(&self) -> u16 {
        self.local_port
    }

    /// Whether the ssh child is still running. A tunnel can die under a laptop lid; the page
    /// shows it rather than pretending.
    pub fn is_up(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn wait_until_serving(&mut self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return false;
            }
            if serves_http(self.local_port) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(120));
        }
        false
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A free loopback port, chosen by the kernel. The listener is closed immediately: the point is
/// the number, and the process that will use it is `ssh`.
pub fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).context("find a free port")?;
    let port = l.local_addr()?.port();
    Ok(port)
}

/// Whether something on this loopback port answers an HTTP request with a status line.
///
/// A TCP connect is not enough: with a forward open, `ssh` accepts the connection locally and only
/// then discovers the far side is closed, so connect succeeds against a dead monitor. The reply is
/// what settles it.
pub fn serves_http(port: u16) -> bool {
    status_code(port).is_some()
}

/// The status code from `GET /` on a loopback port, if it replies like HTTP at all.
pub fn status_code(port: u16) -> Option<u16> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(400)).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(1500))).ok()?;
    s.set_write_timeout(Some(Duration::from_millis(500))).ok()?;
    // HTTP/1.0 with `Connection: close`: no keep-alive to unwind, and the monitor's listener
    // parses only the request line anyway.
    s.write_all(b"GET / HTTP/1.0\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut head = [0u8; 64];
    let n = s.read(&mut head).ok()?;
    let line = String::from_utf8_lossy(&head[..n]);
    let code = line.strip_prefix("HTTP/")?.split(' ').nth(1)?;
    code.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The orphan guard, tested on the command itself: run [`KEEPALIVE`] the way ssh will, close the
    /// pipe the way a killed process does, and it must end. If someone replaces it with `sleep`,
    /// `-N`, or anything else that outlives its input, this fails instead of leaking a forward on
    /// every `Ctrl-C`.
    #[test]
    fn the_keepalive_ends_when_its_input_does() {
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(KEEPALIVE)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the keepalive");
        let stdin = child.stdin.take().expect("its stdin");
        assert!(
            matches!(child.try_wait(), Ok(None)),
            "it holds the tunnel open while the pipe is held"
        );

        drop(stdin); // exactly what the kernel does to a signalled process's fds
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut ended = false;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                ended = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !ended {
            let _ = child.kill();
            let _ = child.wait();
        }
        assert!(ended, "the far end must hang up when its input closes");
    }

    #[test]
    fn a_free_port_is_actually_free() {
        let p = free_port().unwrap();
        assert!(p > 0);
        // Nothing is holding it, so it can be bound — which is the property `ssh -L` needs.
        std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, p)).expect("the port is free");
    }

    /// The distinction the whole health column rests on: a socket that accepts and says nothing
    /// is not a monitor. `ssh -L` produces exactly that when the far side is gone.
    #[test]
    fn accepting_a_connection_is_not_serving_http() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            // Accept, then hold the connection open saying nothing, as a stalled forward does.
            if let Ok((stream, _)) = listener.accept() {
                std::thread::sleep(Duration::from_millis(300));
                drop(stream);
            }
        });
        assert!(!serves_http(port), "silence is not a reply");
        t.join().unwrap();
    }

    #[test]
    fn a_status_line_is_read() {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        assert_eq!(status_code(port), Some(200));
        t.join().unwrap();
        assert_eq!(
            status_code(free_port().unwrap()),
            None,
            "nothing is listening"
        );
    }
}

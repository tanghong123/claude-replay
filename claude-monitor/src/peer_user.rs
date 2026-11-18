//! Whether a loopback TCP peer runs as this user, where no kernel interface says so (#s577).
//!
//! The page pairs its own browser (`POST /api/pair/self`, `claude_replay_html::SelfPairing`) only
//! for a program running as the user the monitor runs as. Linux answers from `/proc/net/tcp`, in
//! the gate itself. macOS has no such table and no `SO_PEERCRED` for TCP, so this asks `lsof`,
//! which an ordinary user runs with an ordinary user's sight: macOS gives another user's file
//! descriptors to root alone, so `lsof` lists only this user's processes' sockets (measured on
//! macOS 26: 128 root processes running, and every one of the 118 TCP sockets `lsof -iTCP` listed
//! was uid 501's). Finding the connection's CLIENT end there is therefore the proof, and its
//! process's uid is checked as well. Anything else — no `lsof`, no answer within the bound, no
//! such socket, another uid — is a no.

use std::net::SocketAddr;
use std::time::Duration;

/// How long `lsof` may take: a pairing click waits on it, and a probe stuck in the kernel must not
/// hold the page (index.rs's probes allow 15 s for the same reason).
const LSOF_BOUND: Duration = Duration::from_secs(5);

/// Whether the loopback connection from `peer` into this server's `local` end is held by a process
/// of this user (see the module).
pub fn peer_is_this_user(peer: SocketAddr, local: SocketAddr) -> bool {
    if !peer.ip().is_loopback() {
        return false;
    }
    let selector = format!("-iTCP@{}:{}", lsof_host(&peer), peer.port());
    let out = crate::index::run_bounded_env(
        &["lsof", "-nP", "-a", &selector, "-sTCP:ESTABLISHED", "-Fpun"],
        &[],
        LSOF_BOUND,
    );
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    client_held_by(&out, peer, local, euid, std::process::id())
}

/// The host as `lsof -i` takes it: an IPv6 address in brackets.
fn lsof_host(addr: &SocketAddr) -> String {
    match addr {
        SocketAddr::V4(a) => a.ip().to_string(),
        SocketAddr::V6(a) => format!("[{}]", a.ip()),
    }
}

/// Whether `lsof -F pun` output names a process of `euid`, other than this one, holding the socket
/// whose local end is `peer` and remote end `local` (the client's side of the connection; the
/// server's own accepted socket names the two the other way round). Pure, for the tests.
fn client_held_by(out: &str, peer: SocketAddr, local: SocketAddr, euid: u32, own_pid: u32) -> bool {
    let want = format!("{peer}->{local}");
    let (mut pid, mut uid) = (None::<u32>, None::<u32>);
    for line in out.lines() {
        let (tag, value) = line.split_at(line.len().min(1));
        match tag {
            "p" => {
                pid = value.parse().ok();
                uid = None;
            }
            "u" => uid = value.parse().ok(),
            "n" if value == want && uid == Some(euid) && pid.is_some_and(|p| p != own_pid) => {
                return true;
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    /// The client's end, held by a process of this user, is the proof; the server's own accepted
    /// socket (the same two addresses the other way round), another uid, or this process are not.
    #[test]
    fn the_client_end_of_the_connection_held_by_this_user_proves_it() {
        let (peer, local) = (addr("127.0.0.1:52345"), addr("127.0.0.1:2727"));
        let browser = "p4100\nu501\nf27\nn127.0.0.1:52345->127.0.0.1:2727\n";
        let server = "p900\nu501\nf9\nn127.0.0.1:2727->127.0.0.1:52345\n";
        assert!(client_held_by(
            &format!("{server}{browser}"),
            peer,
            local,
            501,
            900
        ));
        assert!(
            !client_held_by(server, peer, local, 501, 900),
            "only the server's own socket"
        );
        assert!(
            !client_held_by(browser, peer, local, 502, 900),
            "another user's process"
        );
        assert!(
            !client_held_by(browser, peer, local, 501, 4100),
            "this process itself"
        );
        assert!(
            !client_held_by("", peer, local, 501, 900),
            "lsof said nothing"
        );
        let v6 = "p4100\nu501\nf27\nn[::1]:52345->[::1]:2727\n";
        assert!(client_held_by(
            v6,
            addr("[::1]:52345"),
            addr("[::1]:2727"),
            501,
            900
        ));
    }

    #[test]
    fn a_peer_off_the_loopback_is_never_this_user() {
        assert!(!peer_is_this_user(
            addr("100.64.0.7:52345"),
            addr("127.0.0.1:2727")
        ));
    }
}

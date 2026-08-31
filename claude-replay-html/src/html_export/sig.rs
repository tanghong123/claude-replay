//! **Capability signatures for the local-file routes** — `/file` and `/__reveal` act only on
//! paths this server itself OFFERED, not on any path a caller can name.
//!
//! Containment already asks "does a session shown here explain that path". That is a real
//! check, but it answers at the granularity of a REPOSITORY: everything inside a project the
//! monitor has indexed passes, `.git/config` and `.env` included, and one load of the session
//! list registers every project the user has ever run an agent in. So a token holder could
//! name a file the page never showed and read it.
//!
//! The signature closes that. Every path the renderer puts in a record is stamped with a MAC,
//! and the routes refuse a path whose MAC does not check out. What is reachable becomes
//! exactly what was rendered, which is the property the containment rule was standing in for.
//!
//! **The key is persisted, and it has to be.** The JSON record stream is CACHED (see
//! `render_flavor`), so a signature minted today is replayed tomorrow; a per-process key would
//! invalidate every cached page on restart. It lives beside the pairing token in the monitor's
//! state dir at 0600 — the same "same-user guarantee is the file permissions" argument, and
//! the same directory, so an installation has one key across `agent-replay --html`,
//! `agent-monitor` and `agent-monitor-v2`. A key fingerprint is folded into `render_flavor`,
//! so replacing the key re-renders rather than silently breaking every link.
//!
//! This is defence in depth, NOT a replacement for the other gates: `/file` still requires the
//! pairing token and a same-origin request, and still applies containment. A signature says
//! only "this server offered this path".

use sha2::{Digest, Sha256};
#[cfg(not(test))]
use std::path::PathBuf;
#[cfg(not(test))]
use std::sync::OnceLock;

/// HMAC-SHA256 (RFC 2104) — written out rather than pulled in, because `sha2` is already in
/// this workspace's tree and `hmac` is not, and the construction is fifteen lines of
/// specification. The nesting is what matters: a bare `H(key ‖ msg)` is length-extendable, so
/// an attacker holding one signature could mint one for a LONGER path.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let inner = Sha256::new()
        .chain_update(ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// Where the monitor keeps its 0600 secrets. Resolved by the same rule
/// `claude_monitor::index::state_dir` uses — deliberately duplicated rather than depended on,
/// since this crate sits BELOW the monitor and must not reach up. If the two ever disagreed
/// the signature would simply fail to verify, which fails closed.
#[cfg(not(test))]
fn state_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("AGENT_MONITOR_STATE")
        .or_else(|| std::env::var_os("CLAUDE_MONITOR_STATE"))
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return p;
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(|| PathBuf::from(".").join(".local").join("state"));
    // Both names, older first: the monitor renamed its directory and kept reading the old one,
    // so whichever exists is the installation's real state dir.
    for name in ["claude-monitor", "agent-monitor"] {
        let p = base.join(name);
        if p.is_dir() {
            return p;
        }
    }
    base.join("agent-monitor")
}

/// The signing key, minted once and persisted at 0600. `None` when it can neither be read nor
/// written, which makes every path unsigned and therefore unclickable — a degraded page, not
/// an open one.
fn key() -> Option<&'static [u8; 32]> {
    // Tests get a FIXED key and never touch the disk. A unit test that minted the real key
    // would write into the developer's own state directory, and one that read it would make
    // the suite depend on whether this machine has ever run the monitor.
    #[cfg(test)]
    {
        static TEST_KEY: [u8; 32] = [7u8; 32];
        return Some(&TEST_KEY);
    }
    #[cfg(not(test))]
    {
        static KEY: OnceLock<Option<[u8; 32]>> = OnceLock::new();
        KEY.get_or_init(|| {
            let path = state_dir().join("file-sig-key");
            if let Ok(hex) = std::fs::read_to_string(&path) {
                if let Some(k) = from_hex(hex.trim()) {
                    return Some(k);
                }
            }
            let mut buf = [0u8; 32];
            {
                use std::io::Read;
                std::fs::File::open("/dev/urandom")
                    .ok()?
                    .read_exact(&mut buf)
                    .ok()?;
            }
            // Mode set AT OPEN, never write-then-chmod: a world-readable window is exactly the
            // hole the permission is there to close (`ensure_token` makes the same argument).
            std::fs::create_dir_all(path.parent()?).ok()?;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.mode(0o600);
            }
            use std::io::Write;
            opts.open(&path)
                .ok()?
                .write_all(to_hex(&buf).as_bytes())
                .ok()?;
            Some(buf)
        })
        .as_ref()
    }
}

/// A short, non-secret fingerprint of the key, folded into `render_flavor` so that replacing
/// the key invalidates the cached records that carry its signatures instead of leaving a page
/// full of links that verify against nothing.
pub(crate) fn key_fingerprint() -> u64 {
    let Some(k) = key() else { return 0 };
    let d = Sha256::digest(k);
    u64::from_le_bytes(d[..8].try_into().unwrap_or_default())
}

/// Stamp a path the renderer is about to offer. `None` when there is no key.
pub(crate) fn sign(path: &str) -> Option<String> {
    Some(to_hex(&hmac_sha256(key()?, path.as_bytes())))
}

/// Whether `sig` is this server's stamp for `path`. Constant-time, and false without a key —
/// no key, nothing offered, nothing served.
pub(crate) fn verify(path: &str, sig: Option<&str>) -> bool {
    let (Some(k), Some(sig)) = (key(), sig) else {
        return false;
    };
    let want = to_hex(&hmac_sha256(k, path.as_bytes()));
    super::serve::ct_eq(want.as_bytes(), sig.as_bytes())
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stamp is deterministic, bound to its exact path, and survives the hex round trip.
    ///
    /// The length case is the one worth naming: a bare `H(key ‖ msg)` would let a holder of
    /// one signature extend the message and mint another, which for a PATH means turning a
    /// stamp for `/repo/a` into one for `/repo/a…/etc/shadow`. HMAC's nesting is what makes
    /// that impossible, so an extended path must not verify under the shorter one's stamp.
    #[test]
    fn a_stamp_names_exactly_one_path() {
        let p = "/repo/src/lib.rs";
        let s = sign(p).expect("a key");
        assert_eq!(s.len(), 64, "hex-encoded SHA-256");
        assert_eq!(sign(p).as_deref(), Some(s.as_str()), "deterministic");
        assert!(verify(p, Some(&s)));
        for other in [
            "/repo/src/lib.rs2",     // extended — the length-extension case
            "/repo/src/lib.r",       // truncated
            "/repo/src/../../etc/x", // a different path entirely
            "",
        ] {
            assert!(!verify(other, Some(&s)), "{other} must not verify");
        }
        assert!(!verify(p, None), "no stamp is not a stamp");
        assert!(!verify(p, Some("")), "nor is an empty one");
        assert!(!verify(p, Some(&"f".repeat(64))), "nor a made-up one");
    }

    /// Hex is the wire form; it must round-trip a key exactly, and refuse anything else.
    #[test]
    fn the_key_hex_round_trips() {
        let k = [0x00u8, 0xff, 0x10, 0x9a]
            .iter()
            .cycle()
            .take(32)
            .copied()
            .collect::<Vec<_>>();
        let k: [u8; 32] = k.try_into().unwrap();
        assert_eq!(from_hex(&to_hex(&k)), Some(k));
        assert_eq!(from_hex("short"), None);
        assert_eq!(from_hex(&"z".repeat(64)), None, "not hex");
    }
}

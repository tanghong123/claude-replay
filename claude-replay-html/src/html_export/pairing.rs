//! One-time pairing codes (#11): how a phone joins a PAIRED monitor without anyone typing, copying
//! or showing the long-lived token.
//!
//! The owner serves the monitor to their tailnet with `tailscale serve` on the monitor's own port
//! (`tailscale serve --bg --https 2727 2727`: the tailnet name's port 2727, proxied to the loopback
//! listener). `tailscaled` runs as root, so a request through it is never
//! the same-user loopback bypass: a paired monitor demands the token from every tailnet device.
//! Handing a phone that token was the problem — "typing the address won't work", and "it would be
//! awkward to copy the raw url with pairing code on it (and not secure)" (the owner, 2026-09-28).
//!
//! So `agent-monitor --pair-phone` mints a SHORT, SINGLE-USE code that expires in
//! [`PAIR_CODE_TTL_SECS`] and prints it with a QR code for `<base>pair#code=<CODE>`. The phone's
//! `/pair` page reads the code from the fragment (which never reaches the server or the proxy) or
//! from what the reader types, and POSTs it to `/api/pair`, which swaps it for the ordinary
//! `cmauth` cookie. A used or expired code is worthless; [`PAIR_CODE_TRIES`] wrong attempts burn
//! every outstanding code. The long-lived token is never displayed.
//!
//! The codes live in a 0600 file in the monitor's state directory, one `code <CODE> <expires>`
//! line each plus a `fails <n>` line: the CLI mints into it, the server redeems from it, and the
//! two are different processes.

use std::io::{Read, Write};
use std::path::Path;

/// The characters a code is drawn from: no 0/O, 1/I/L — a code is read off a screen and typed.
pub const PAIR_CODE_ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// A code's length: 31^8 ≈ 2^39.6, against [`PAIR_CODE_TRIES`] guesses in [`PAIR_CODE_TTL_SECS`].
pub const PAIR_CODE_LEN: usize = 8;
/// How long a code is good for.
pub const PAIR_CODE_TTL_SECS: u64 = 300;
/// Wrong attempts, across all codes, before every outstanding code is burned.
pub const PAIR_CODE_TRIES: u32 = 5;

/// Serializes redeems inside one server (requests are served on threads).
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Store {
    codes: Vec<(String, u64)>,
    fails: u32,
}

fn read_store(path: &Path) -> Store {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut text);
    }
    let mut store = Store {
        codes: Vec::new(),
        fails: 0,
    };
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some("code"), Some(code), Some(exp)) => {
                if let Ok(exp) = exp.parse() {
                    store.codes.push((code.to_string(), exp));
                }
            }
            (Some("fails"), Some(n), None) => store.fails = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    store
}

/// Write the store at 0600: to a sibling temp file with the mode set AT OPEN, then renamed over,
/// so a reader never sees half a file and the codes are never world-readable for an instant.
fn write_store(path: &Path, store: &Store) -> std::io::Result<()> {
    let mut text = String::new();
    for (code, exp) in &store.codes {
        text.push_str(&format!("code {code} {exp}\n"));
    }
    text.push_str(&format!("fails {}\n", store.fails));
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
    }
    std::fs::rename(&tmp, path)
}

/// A typed code, normalized: the reader may type it lowercase, with the dash the terminal prints
/// between its halves, or with spaces.
pub fn normalize_code(typed: &str) -> String {
    typed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// How a code is shown: two halves of four, easier to read off a screen and type.
pub fn display_code(code: &str) -> String {
    if code.len() == PAIR_CODE_LEN {
        format!("{}-{}", &code[..4], &code[4..])
    } else {
        code.to_string()
    }
}

/// Mint a code valid until `now + PAIR_CODE_TTL_SECS`, keeping the other unexpired ones, and
/// return it. Random from `/dev/urandom`; `None` when that cannot be read or the file written.
pub fn mint_pair_code(path: &Path, now: u64) -> Option<String> {
    let mut raw = [0u8; PAIR_CODE_LEN * 2];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut raw)
        .ok()?;
    // Rejection sampling keeps the draw uniform over the alphabet (256 is not a multiple of 31).
    let n = PAIR_CODE_ALPHABET.len();
    let limit = 256 - (256 % n);
    let mut code = String::new();
    let mut i = 0;
    while code.len() < PAIR_CODE_LEN {
        if i == raw.len() {
            std::fs::File::open("/dev/urandom")
                .ok()?
                .read_exact(&mut raw)
                .ok()?;
            i = 0;
        }
        let b = raw[i] as usize;
        i += 1;
        if b < limit {
            code.push(PAIR_CODE_ALPHABET[b % n] as char);
        }
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = read_store(path);
    store.codes.retain(|(_, exp)| *exp > now);
    store.codes.push((code.clone(), now + PAIR_CODE_TTL_SECS));
    write_store(path, &store).ok()?;
    Some(code)
}

/// Redeem `typed` at `now`: true exactly once for an unexpired code, which is then gone. A miss
/// counts toward [`PAIR_CODE_TRIES`], at which every outstanding code is burned.
pub fn redeem_pair_code(path: &Path, typed: &str, now: u64) -> bool {
    let code = normalize_code(typed);
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = read_store(path);
    store.codes.retain(|(_, exp)| *exp > now);
    // Compared in constant time against every entry, so how long a miss takes says nothing.
    let mut hit = None;
    for (i, (c, _)) in store.codes.iter().enumerate() {
        if super::serve::ct_eq(c.as_bytes(), code.as_bytes()) {
            hit = Some(i);
        }
    }
    let ok = match hit {
        Some(i) if !code.is_empty() => {
            store.codes.remove(i);
            store.fails = 0;
            true
        }
        _ => {
            store.fails += 1;
            if store.fails >= PAIR_CODE_TRIES {
                store.codes.clear();
                store.fails = 0;
            }
            false
        }
    };
    let _ = write_store(path, &store);
    ok
}

/// Seconds since the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `/pair` page: a code box that fills itself from `#code=…`, and a button. On success it
/// replaces itself with `/`, so neither the code nor this page stays in the history.
pub const PAIR_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pair this device</title>
<style>
:root{--bg:#f6f7f9;--card:#fff;--ink:#1d2330;--sub:#5b6475;--line:#d9dde5;--accent:#3f5bd9;--bad:#b3261e}
@media (prefers-color-scheme:dark){:root{--bg:#14171d;--card:#1c2029;--ink:#e6e9ef;--sub:#9aa3b2;--line:#303644;--accent:#8ea2ff;--bad:#ff8a80}}
*{box-sizing:border-box}body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--ink);font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;padding:16px}
main{width:min(380px,100%);background:var(--card);border:1px solid var(--line);border-radius:14px;padding:24px}
h1{margin:0 0 6px;font-size:20px}p{margin:0 0 16px;color:var(--sub);font-size:14px}
input{width:100%;font:600 24px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.12em;text-align:center;text-transform:uppercase;padding:12px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--ink)}
button{width:100%;margin-top:12px;padding:12px;border:0;border-radius:10px;background:var(--accent);color:#fff;font-size:16px;font-weight:600}
button:disabled{opacity:.6}#msg{min-height:1.5em;margin:12px 0 0;color:var(--bad);font-size:14px}
</style></head><body><main>
<h1>Pair this device</h1>
<p>Enter the code <code>agent-monitor --pair-phone</code> printed. It works once, for five minutes.</p>
<form id="f"><input id="code" autocomplete="one-time-code" autocapitalize="characters" spellcheck="false" maxlength="12" placeholder="XXXX-XXXX" aria-label="Pairing code"><button id="go" type="submit">Pair</button></form>
<p id="msg" role="status"></p>
</main><script>
(function(){
  var box = document.getElementById("code"), go = document.getElementById("go"), msg = document.getElementById("msg");
  function pair(){
    var code = box.value.trim();
    if (!code) return;
    go.disabled = true; msg.textContent = "";
    fetch("/api/pair", { method: "POST", body: code, credentials: "same-origin", cache: "no-store" })
      .then(function(r){ if (r.ok) { location.replace("/"); return; } go.disabled = false;
        msg.textContent = "That code is wrong, used or expired. Run agent-monitor --pair-phone again."; })
      .catch(function(){ go.disabled = false; msg.textContent = "The monitor did not answer."; });
  }
  document.getElementById("f").addEventListener("submit", function(e){ e.preventDefault(); pair(); });
  var m = /(?:^#|&)code=([A-Za-z0-9-]+)/.exec(location.hash);
  if (m) { box.value = m[1]; history.replaceState(null, "", location.pathname); pair(); }
})();
</script></body></html>
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn store_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cr-pair-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("pair-codes")
    }

    #[test]
    fn a_code_is_good_once_and_only_until_it_expires() {
        let path = store_path("once");
        let code = mint_pair_code(&path, 1_000).unwrap();
        assert_eq!(code.len(), PAIR_CODE_LEN);
        assert!(code.bytes().all(|b| PAIR_CODE_ALPHABET.contains(&b)));
        assert!(
            redeem_pair_code(&path, &code, 1_001),
            "a fresh code redeems"
        );
        assert!(!redeem_pair_code(&path, &code, 1_002), "…exactly once");
        let late = mint_pair_code(&path, 2_000).unwrap();
        assert!(
            !redeem_pair_code(&path, &late, 2_000 + PAIR_CODE_TTL_SECS),
            "an expired code is worthless"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the codes file is the owner's alone");
        }
    }

    #[test]
    fn a_typed_code_may_be_lowercase_with_its_dash() {
        let path = store_path("typed");
        let code = mint_pair_code(&path, 10).unwrap();
        let typed = format!(" {} ", display_code(&code).to_lowercase());
        assert!(typed.contains('-'), "shown in two halves: {typed}");
        assert!(redeem_pair_code(&path, &typed, 11), "{typed}");
    }

    #[test]
    fn five_wrong_attempts_burn_every_outstanding_code() {
        let path = store_path("burn");
        let code = mint_pair_code(&path, 10).unwrap();
        for i in 0..PAIR_CODE_TRIES - 1 {
            assert!(!redeem_pair_code(&path, "ZZZZZZZZ", 11 + u64::from(i)));
        }
        // One more miss reaches the limit…
        assert!(!redeem_pair_code(&path, "ZZZZZZZZ", 20));
        // …and the real code is gone with it.
        assert!(!redeem_pair_code(&path, &code, 21), "burned");
        // A success resets the count, so a later honest pairing is not held against earlier typos.
        let again = mint_pair_code(&path, 30).unwrap();
        assert!(!redeem_pair_code(&path, "ZZZZZZZZ", 31));
        assert!(redeem_pair_code(&path, &again, 32));
    }

    #[test]
    fn an_empty_or_missing_store_redeems_nothing() {
        let path = store_path("empty");
        assert!(!redeem_pair_code(&path, "", 1));
        assert!(!redeem_pair_code(&path, "ABCDEFGH", 1));
    }
}

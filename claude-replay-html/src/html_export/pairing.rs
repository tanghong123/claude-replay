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
    /// Page codes only (#s618): when the last one was asked for, how many in a row went unused,
    /// and until when asks rest. A phone store never sets them, so its file is as it was.
    asked: u64,
    unused: u32,
    rest: u64,
}

fn read_store(path: &Path) -> Store {
    let mut text = String::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let _ = f.read_to_string(&mut text);
    }
    let mut store = Store {
        codes: Vec::new(),
        fails: 0,
        asked: 0,
        unused: 0,
        rest: 0,
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
            (Some("asked"), Some(n), None) => store.asked = n.parse().unwrap_or(0),
            (Some("unused"), Some(n), None) => store.unused = n.parse().unwrap_or(0),
            (Some("rest"), Some(n), None) => store.rest = n.parse().unwrap_or(0),
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
    for (key, n) in [
        ("asked", store.asked),
        ("unused", u64::from(store.unused)),
        ("rest", store.rest),
    ] {
        if n > 0 {
            text.push_str(&format!("{key} {n}\n"));
        }
    }
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
    let code = draw_code()?;
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = read_store(path);
    store.codes.retain(|(_, exp)| *exp > now);
    store.codes.push((code.clone(), now + PAIR_CODE_TTL_SECS));
    write_store(path, &store).ok()?;
    Some(code)
}

/// A fresh code, uniform over [`PAIR_CODE_ALPHABET`], from `/dev/urandom`.
fn draw_code() -> Option<String> {
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
    // A code pairs only once its use is saved (#s618 review): otherwise it stays good, and while
    // the file cannot be written no miss is counted either. Unsaved, a hit reads as a miss, so a
    // guesser cannot tell which guess was right.
    write_store(path, &store).is_ok() && ok
}

// Page codes (#s618, design/pair-from-the-page.md): a browser on this machine asks, the host
// shows a code only on this machine (a dialog, or `--pair-code` in a terminal), and the person
// types it into the page. They live in a file of their own, apart from phone codes, with their own
// count of misses: a code shown on this machine's screen never pairs a phone, and misses at one
// kind never burn the other. The host redeems one only from a request that came to the loopback
// as the machine's own (`serve.rs`, `local_asker`).

/// Hold the page codes while `f` reads and writes them. Besides this process's own lock, a file
/// lock on `<path>.lock`: `--pair-code` runs as a second process beside the monitor, and a redeem
/// landing between its read and its write would otherwise put a used code back. None, and `f` not
/// run, when that file lock cannot be taken (#s618 review: a lock file this user cannot open, as
/// one a `sudo agent-monitor --pair-code` left): every caller then fails closed, minting nothing,
/// pairing nothing and clearing nothing.
fn with_page_lock<T>(path: &Path, f: impl FnOnce() -> T) -> Option<T> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let mut open = std::fs::OpenOptions::new();
    open.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let file = open.open(&lock_path).ok()?;
    file.lock().ok()?;
    let out = f();
    let _ = file.unlock();
    Some(out)
}

/// The seconds an ask waits after the last one.
pub const PAGE_ASK_SPACING_SECS: u64 = 10;
/// Page codes in a row that expire, burn or are ignored before asks rest.
pub const PAGE_UNUSED_LIMIT: u32 = 3;
/// How long asks rest after [`PAGE_UNUSED_LIMIT`] codes went unused. A terminal's
/// `--pair-code` still mints one meanwhile: its shell is the proof.
pub const PAGE_REST_SECS: u64 = 3600;

/// What an ask for a page code came to.
#[derive(Debug, PartialEq, Eq)]
pub enum PageAsk {
    /// A new code, to be shown on this machine and nowhere else. If it cannot be shown there, it
    /// waits for `--pair-code` in a terminal on the machine.
    Minted(String),
    /// One is waiting already: one page code at a time.
    Waiting,
    /// Asked less than [`PAGE_ASK_SPACING_SECS`] ago: try again in this many seconds.
    TooSoon(u64),
    /// Asks rest until this time (seconds since the epoch).
    Resting(u64),
    /// No random bytes, or the file could not be written.
    Failed,
}

/// What a typed page code came to.
#[derive(Debug, PartialEq, Eq)]
pub enum PageRedeem {
    /// No page code waits: the code is not a page code's to judge.
    NoneWaiting,
    /// The code was right: it is gone, and the browser that typed it pairs.
    Paired,
    /// The code was wrong. `burned` when that miss took every waiting code with it, so whatever
    /// shows one should close.
    Missed { burned: bool },
}

/// Drop the expired codes, each counting as one gone unused; at [`PAGE_UNUSED_LIMIT`] asks rest.
fn settle_page(store: &mut Store, now: u64) {
    let before = store.codes.len();
    store.codes.retain(|(_, exp)| *exp > now);
    for _ in store.codes.len()..before {
        note_unused(store, now);
    }
}

fn note_unused(store: &mut Store, now: u64) {
    store.unused += 1;
    if store.unused >= PAGE_UNUSED_LIMIT {
        store.unused = 0;
        store.rest = now + PAGE_REST_SECS;
    }
}

/// A page asks for a code at `now`. At most one waits; asks are spaced; and after
/// [`PAGE_UNUSED_LIMIT`] codes in a row go unused, they rest for [`PAGE_REST_SECS`].
pub fn ask_page_code(path: &Path, now: u64) -> PageAsk {
    with_page_lock(path, || ask_page_code_held(path, now)).unwrap_or(PageAsk::Failed)
}

fn ask_page_code_held(path: &Path, now: u64) -> PageAsk {
    let mut store = read_store(path);
    settle_page(&mut store, now);
    let answer = if store.rest > now {
        PageAsk::Resting(store.rest)
    } else if !store.codes.is_empty() {
        PageAsk::Waiting
    } else if store.asked > 0 && now < store.asked + PAGE_ASK_SPACING_SECS {
        PageAsk::TooSoon(store.asked + PAGE_ASK_SPACING_SECS - now)
    } else {
        match draw_code() {
            Some(code) => {
                store.asked = now;
                store.codes.push((code.clone(), now + PAIR_CODE_TTL_SECS));
                PageAsk::Minted(code)
            }
            None => PageAsk::Failed,
        }
    };
    if write_store(path, &store).is_err() {
        return PageAsk::Failed;
    }
    answer
}

/// The page code waiting at `now`, or a new one: what `--pair-code` prints in a terminal. It
/// keeps neither the spacing nor the rest, since the terminal is the proof; and it never adds a
/// second code beside one waiting.
pub fn terminal_page_code(path: &Path, now: u64) -> Option<String> {
    with_page_lock(path, || {
        let mut store = read_store(path);
        let held = store.codes.len();
        settle_page(&mut store, now);
        let code = match store.codes.first() {
            Some((code, _)) if store.codes.len() == held => {
                // Nothing changed: write nothing.
                return Some(code.clone());
            }
            Some((code, _)) => code.clone(),
            None => {
                let code = draw_code()?;
                store.codes.push((code.clone(), now + PAIR_CODE_TTL_SECS));
                code
            }
        };
        write_store(path, &store).ok()?;
        Some(code)
    })
    .flatten()
}

/// The person said they did not ask for `code` (the dialog's Ignore): it is burned, and counts as
/// one gone unused. Another code is left alone. False when that could not be saved: the code then
/// stays good until it expires, though while the file cannot be written no page code redeems.
pub fn ignore_page_code(path: &Path, code: &str, now: u64) -> bool {
    with_page_lock(path, || {
        let mut store = read_store(path);
        settle_page(&mut store, now);
        let before = store.codes.len();
        store.codes.retain(|(c, _)| c != code);
        if store.codes.len() < before {
            note_unused(&mut store, now);
        }
        write_store(path, &store).is_ok()
    })
    .unwrap_or(false)
}

/// Whether `code` is a page code still waiting at `now`. The host asks after it has shown a code,
/// since the code may have been used or burned while the display was starting (#s618 review).
pub fn page_code_waits(path: &Path, code: &str, now: u64) -> bool {
    with_page_lock(path, || {
        read_store(path)
            .codes
            .iter()
            .any(|(c, exp)| *exp > now && c == code)
    })
    .unwrap_or(false)
}

/// Redeem a typed page code at `now`. Exactly once for a waiting code; a miss counts here only,
/// and [`PAIR_CODE_TRIES`] of them burn every waiting page code, which counts as one gone unused.
pub fn redeem_page_code(path: &Path, typed: &str, now: u64) -> PageRedeem {
    let code = normalize_code(typed);
    with_page_lock(path, || redeem_page_code_held(path, &code, now))
        .unwrap_or(PageRedeem::Missed { burned: false })
}

fn redeem_page_code_held(path: &Path, code: &str, now: u64) -> PageRedeem {
    let mut store = read_store(path);
    settle_page(&mut store, now);
    if store.codes.is_empty() {
        let _ = write_store(path, &store);
        return PageRedeem::NoneWaiting;
    }
    let mut hit = None;
    for (i, (c, _)) in store.codes.iter().enumerate() {
        if super::serve::ct_eq(c.as_bytes(), code.as_bytes()) {
            hit = Some(i);
        }
    }
    let answer = match hit {
        Some(i) if !code.is_empty() => {
            store.codes.remove(i);
            store.fails = 0;
            store.unused = 0;
            PageRedeem::Paired
        }
        _ => {
            store.fails += 1;
            let burned = store.fails >= PAIR_CODE_TRIES;
            if burned {
                store.codes.clear();
                store.fails = 0;
                note_unused(&mut store, now);
            }
            PageRedeem::Missed { burned }
        }
    };
    // As for a phone code: a hit pairs only once its use is saved, and unsaved it reads as a miss.
    // Nor does an unsaved miss burn anything (#s618 review): the codes are still good on disk, so
    // whatever shows one stays up.
    match write_store(path, &store) {
        Ok(()) => answer,
        Err(_) => PageRedeem::Missed { burned: false },
    }
}

/// Clear every waiting page code: the host does this as it starts, since nothing shows them any
/// more. The count of codes gone unused and any rest are kept. False when they could not be
/// cleared; the host then offers no page pairing for the run.
pub fn clear_page_codes(path: &Path) -> bool {
    with_page_lock(path, || {
        if !path.exists() {
            return true;
        }
        let mut store = read_store(path);
        store.codes.clear();
        store.fails = 0;
        write_store(path, &store).is_ok()
    })
    .unwrap_or(false)
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
/// It names [`DEFAULT_PAIR_COMMAND`]; [`pair_page`] names another host's (#376).
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

/// The pairing page a browser on this machine gets (#s618): it asks the monitor to show a code on
/// this machine, then takes the code the person reads there. A phone's code still pairs from it.
const PAIR_HERE_PAGE: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Pair this browser</title>
<style>
:root{--bg:#f6f7f9;--card:#fff;--ink:#1d2330;--sub:#5b6475;--line:#d9dde5;--accent:#3f5bd9;--bad:#b3261e;--soft:#e4eaf4;--softink:#193858}
@media (prefers-color-scheme:dark){:root{--bg:#14171d;--card:#1c2029;--ink:#e6e9ef;--sub:#9aa3b2;--line:#303644;--accent:#8ea2ff;--bad:#ff8a80;--soft:#293c53;--softink:#e7f0ff}}
*{box-sizing:border-box}body{margin:0;min-height:100vh;display:grid;place-items:center;background:var(--bg);color:var(--ink);font:16px/1.5 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;padding:16px}
main{width:min(400px,100%);background:var(--card);border:1px solid var(--line);border-radius:14px;padding:24px}
h1{margin:0 0 6px;font-size:20px}p{margin:0 0 16px;color:var(--sub);font-size:14px}
input{width:100%;font:600 24px/1.2 ui-monospace,SFMono-Regular,Menlo,monospace;letter-spacing:.12em;text-align:center;text-transform:uppercase;padding:12px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--ink)}
button{width:100%;margin-top:12px;min-height:44px;padding:12px;border:0;border-radius:10px;background:var(--accent);color:#fff;font-size:16px;font-weight:600}
button.ask{background:var(--soft);color:var(--softink);margin:0 0 16px}
button:disabled{opacity:.6}#msg{min-height:1.5em;margin:12px 0 0;font-size:14px}#msg.bad{color:var(--bad)}
</style></head><body><main>
<h1>Pair this browser</h1>
<p>Pairing lets this browser read the monitor and write to your sessions. The monitor shows a code on this __HOST__ itself; type it here.</p>
<button id="ask" class="ask" type="button">Show a code on this __HOST__</button>
<form id="f"><input id="code" autocomplete="one-time-code" autocapitalize="characters" spellcheck="false" maxlength="12" placeholder="XXXX-XXXX" aria-label="Pairing code"><button id="go" type="submit">Pair</button></form>
<p id="msg" role="status"></p>
</main><script>
(function(){
  var box = document.getElementById("code"), go = document.getElementById("go"), ask = document.getElementById("ask"), msg = document.getElementById("msg");
  function say(text, bad){ msg.textContent = text; msg.className = bad ? "bad" : ""; }
  function said(r, fallback){ return r.json().then(function(j){ return (j && j.message) || fallback; }, function(){ return fallback; }); }
  ask.addEventListener("click", function(){
    ask.disabled = true; say("");
    fetch("/api/pair/ask", { method: "POST", credentials: "same-origin", cache: "no-store" })
      .then(function(r){
        if (!r.ok) return said(r, "The monitor could not show a code.").then(function(t){ ask.disabled = false; say(t, true); });
        return r.json().then(function(j){
          setTimeout(function(){ ask.disabled = false; }, 10000);
          say(j.shown === "dialog" ? "Type the code the dialog on this __HOST__ shows." : (j.message || "Run __CODE__ in a terminal on this __HOST__, then type the code it prints."), false);
          box.focus();
        });
      })
      .catch(function(){ ask.disabled = false; say("The monitor did not answer.", true); });
  });
  // A code a link carries (#code=, what --pair-phone's link holds) is a phone code: it is sent
  // marked so, and the monitor never judges it against a code shown on this machine, so a site that
  // opens this page with a made-up code cannot spend that code's guesses.
  function pair(q){
    var code = box.value.trim();
    if (!code) return;
    go.disabled = true; say("");
    fetch("/api/pair" + (q || ""), { method: "POST", body: code, credentials: "same-origin", cache: "no-store" })
      .then(function(r){ if (r.ok) { location.replace("/"); return; } go.disabled = false;
        say("That code is wrong, used or expired. Ask for a new one.", true); })
      .catch(function(){ go.disabled = false; say("The monitor did not answer.", true); });
  }
  document.getElementById("f").addEventListener("submit", function(e){ e.preventDefault(); pair(""); });
  var m = /(?:^#|&)code=([A-Za-z0-9-]+)/.exec(location.hash);
  if (m) { box.value = m[1]; history.replaceState(null, "", location.pathname); pair("?from=link"); }
})();
</script></body></html>
"#;

/// What a machine is called in the page and the dialog: a Mac on macOS.
pub const HOST_WORD: &str = if cfg!(target_os = "macos") {
    "Mac"
} else {
    "machine"
};

/// The pairing page for a browser on this machine (#s618), naming `code_command`, the host's
/// command that prints a page code in a terminal.
pub fn pair_page_here(code_command: &str) -> String {
    let command = if plain_command(code_command) {
        code_command
    } else {
        DEFAULT_CODE_COMMAND
    };
    PAIR_HERE_PAGE
        .replace("__HOST__", HOST_WORD)
        .replace("__CODE__", command)
}

/// The command that prints a page code in a terminal: the monitor's own, unless the host names
/// its own with [`AuthGate::with_page_pairing`](super::serve::AuthGate::with_page_pairing).
pub const DEFAULT_CODE_COMMAND: &str = "agent-monitor --pair-code";

/// The command the pairing page tells a person to run: the monitor's own, unless the host
/// names its own with [`AuthGate::with_pair_command`](super::serve::AuthGate::with_pair_command).
pub const DEFAULT_PAIR_COMMAND: &str = "agent-monitor --pair-phone";

/// Whether `command` can stand in the page as it is: plain words (letters, digits, space and
/// `. _ / -`), which need no escaping in the page's HTML or in its script's string.
pub fn plain_command(command: &str) -> bool {
    !command.is_empty()
        && command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._/-".contains(c))
}

/// The pairing page naming `command` (#376): [`PAIR_PAGE`] itself for the default.
pub fn pair_page(command: &str) -> String {
    if command == DEFAULT_PAIR_COMMAND || !plain_command(command) {
        return PAIR_PAGE.to_string();
    }
    PAIR_PAGE.replace(DEFAULT_PAIR_COMMAND, command)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host's command replaces both of the page's mentions; the default leaves the page as
    /// it always was (#376).
    #[test]
    fn the_pairing_page_names_the_hosts_command() {
        assert_eq!(pair_page(DEFAULT_PAIR_COMMAND), PAIR_PAGE);
        assert_eq!(
            PAIR_PAGE.matches(DEFAULT_PAIR_COMMAND).count(),
            2,
            "the lead and the wrong-code message"
        );
        let other = pair_page("agent-metrics pair-phone");
        assert_eq!(other.matches("agent-metrics pair-phone").count(), 2);
        assert!(!other.contains("agent-monitor"));
        // anything that would need escaping is not taken
        assert!(!plain_command("x</code><script>"));
        assert!(!plain_command("say \"hi\""));
        assert!(!plain_command(""));
        assert_eq!(pair_page("x\"y"), PAIR_PAGE);
    }

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

    /// #s618: one page code at a time, asks spaced, and the code good once.
    #[test]
    fn a_page_code_is_one_at_a_time_spaced_and_good_once() {
        let path = store_path("page-once");
        let PageAsk::Minted(code) = ask_page_code(&path, 1_000) else {
            panic!("the first ask mints");
        };
        assert_eq!(code.len(), PAIR_CODE_LEN);
        assert_eq!(
            ask_page_code(&path, 1_020),
            PageAsk::Waiting,
            "one at a time"
        );
        assert_eq!(
            terminal_page_code(&path, 1_021).as_deref(),
            Some(code.as_str()),
            "the terminal prints the code waiting, never a second"
        );
        assert_eq!(
            redeem_page_code(&path, &display_code(&code), 1_030),
            PageRedeem::Paired
        );
        assert_eq!(
            redeem_page_code(&path, &code, 1_031),
            PageRedeem::NoneWaiting,
            "once"
        );
        let PageAsk::Minted(_) = ask_page_code(&path, 1_040) else {
            panic!("a used code lets the next ask through");
        };
        let path = store_path("page-spacing");
        let PageAsk::Minted(first) = ask_page_code(&path, 2_000) else {
            panic!("mints");
        };
        assert_eq!(redeem_page_code(&path, &first, 2_003), PageRedeem::Paired);
        assert_eq!(
            ask_page_code(&path, 2_005),
            PageAsk::TooSoon(PAGE_ASK_SPACING_SECS - 5),
            "asks are spaced from the last ask, used or not"
        );
        assert!(matches!(ask_page_code(&path, 2_010), PageAsk::Minted(_)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the page codes file is the owner's alone");
        }
    }

    /// #s618: three page codes in a row gone unused (expired, burned by misses, or ignored) rest
    /// the asks for an hour; a terminal still gets one; a pairing resets the count.
    #[test]
    fn three_unused_page_codes_rest_the_asks_for_an_hour() {
        let path = store_path("page-rest");
        let mut now = 10_000;
        // 1: expires.
        assert!(matches!(ask_page_code(&path, now), PageAsk::Minted(_)));
        now += PAIR_CODE_TTL_SECS + 1;
        // 2: burned by five misses.
        assert!(matches!(ask_page_code(&path, now), PageAsk::Minted(_)));
        for i in 0..PAIR_CODE_TRIES {
            let burned = i + 1 == PAIR_CODE_TRIES;
            assert_eq!(
                redeem_page_code(&path, "ZZZZZZZZ", now + 1),
                PageRedeem::Missed { burned }
            );
        }
        now += PAGE_ASK_SPACING_SECS;
        // 3: ignored on the screen.
        let PageAsk::Minted(third) = ask_page_code(&path, now) else {
            panic!("mints");
        };
        ignore_page_code(&path, &third, now + 1);
        now += PAGE_ASK_SPACING_SECS;
        assert_eq!(
            ask_page_code(&path, now),
            PageAsk::Resting(now - PAGE_ASK_SPACING_SECS + 1 + PAGE_REST_SECS)
        );
        let code = terminal_page_code(&path, now).expect("a terminal is not held by the rest");
        assert_eq!(redeem_page_code(&path, &code, now + 1), PageRedeem::Paired);
        assert!(matches!(
            ask_page_code(&path, now + PAGE_REST_SECS + 2),
            PageAsk::Minted(_)
        ));
    }

    /// #s618: page codes and phone codes are apart. A miss at one kind never burns the other,
    /// and neither redeems as the other.
    #[test]
    fn page_codes_and_phone_codes_never_mix() {
        let dir = store_path("mix");
        let (phone, page) = (
            dir.with_file_name("pair-codes"),
            dir.with_file_name("page-codes"),
        );
        let phone_code = mint_pair_code(&phone, 100).unwrap();
        let PageAsk::Minted(page_code) = ask_page_code(&page, 100) else {
            panic!("mints");
        };
        assert_eq!(
            redeem_page_code(&page, &phone_code, 101),
            PageRedeem::Missed { burned: false },
            "a phone code is no page code"
        );
        assert!(
            !redeem_pair_code(&phone, &page_code, 102),
            "a page code is no phone code"
        );
        for _ in 0..PAIR_CODE_TRIES - 2 {
            assert!(!redeem_pair_code(&phone, "ZZZZZZZZ", 103));
        }
        assert_eq!(
            redeem_page_code(&page, &page_code, 104),
            PageRedeem::Paired,
            "misses at the phone codes left the page code waiting"
        );
        assert!(
            redeem_pair_code(&phone, &phone_code, 105),
            "and a page miss burned no phone code"
        );
        clear_page_codes(&page);
        assert_eq!(
            redeem_page_code(&page, &page_code, 106),
            PageRedeem::NoneWaiting
        );
    }

    #[test]
    fn an_empty_or_missing_store_redeems_nothing() {
        let path = store_path("empty");
        assert!(!redeem_pair_code(&path, "", 1));
        assert!(!redeem_pair_code(&path, "ABCDEFGH", 1));
    }

    /// `path`'s directory made read-only, so no store can be written there: false where that does
    /// not stop a write (a test run as root), with the directory given back.
    #[cfg(unix)]
    fn freeze(path: &std::path::Path, frozen: bool) -> bool {
        use std::os::unix::fs::PermissionsExt;
        let dir = path.parent().unwrap();
        let mode = if frozen { 0o500 } else { 0o700 };
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).unwrap();
        if !frozen {
            return true;
        }
        let probe = dir.join("probe");
        if std::fs::write(&probe, "x").is_ok() {
            let _ = std::fs::remove_file(&probe);
            freeze(path, false);
            return false;
        }
        true
    }

    /// #s618 review: a code pairs only once its use is saved. While the store cannot be written a
    /// right code reads as a miss (so a guesser learns nothing), and it is still good once the
    /// store can be written again; Ignore and the start's clear say they could not.
    #[cfg(unix)]
    #[test]
    fn a_code_whose_use_cannot_be_saved_pairs_nothing_and_stays_good() {
        let page = store_path("unsaved-page");
        let PageAsk::Minted(code) = ask_page_code(&page, 1_000) else {
            panic!("the first ask mints");
        };
        let phone = store_path("unsaved-phone");
        let phone_code = mint_pair_code(&phone, 1_000).expect("a phone code");
        if !freeze(&page, true) {
            eprintln!("skipped: a read-only directory does not stop this user's writes (root)");
            return;
        }
        assert!(freeze(&phone, true));
        assert_eq!(
            redeem_page_code(&page, &code, 1_001),
            PageRedeem::Missed { burned: false },
            "unsaved, a right page code reads as a miss"
        );
        assert!(
            !redeem_pair_code(&phone, &phone_code, 1_001),
            "unsaved, a right phone code reads as a miss"
        );
        assert!(
            !ignore_page_code(&page, &code, 1_001),
            "Ignore says it could not"
        );
        assert!(
            !clear_page_codes(&page),
            "the start's clear says it could not"
        );
        assert_eq!(ask_page_code(&page, 1_100), PageAsk::Failed);
        freeze(&page, false);
        freeze(&phone, false);
        assert_eq!(
            redeem_page_code(&page, &code, 1_002),
            PageRedeem::Paired,
            "the page code was never used"
        );
        assert!(
            redeem_pair_code(&phone, &phone_code, 1_002),
            "the phone code was never used"
        );
        assert!(clear_page_codes(&page));
    }

    /// #s618 review: a miss that cannot be saved burns nothing, so whatever shows the code stays
    /// up, and the code is still good once the store can be written again.
    #[cfg(unix)]
    #[test]
    fn a_miss_that_cannot_be_saved_burns_nothing() {
        let page = store_path("unsaved-burn");
        let PageAsk::Minted(code) = ask_page_code(&page, 1_000) else {
            panic!("the first ask mints");
        };
        let wrong = if code == "ZZZZZZZZ" {
            "YYYYYYYY"
        } else {
            "ZZZZZZZZ"
        };
        for _ in 1..PAIR_CODE_TRIES {
            assert_eq!(
                redeem_page_code(&page, wrong, 1_001),
                PageRedeem::Missed { burned: false }
            );
        }
        if !freeze(&page, true) {
            eprintln!("skipped: a read-only directory does not stop this user's writes (root)");
            return;
        }
        assert_eq!(
            redeem_page_code(&page, wrong, 1_002),
            PageRedeem::Missed { burned: false },
            "the miss that would burn, unsaved, burns nothing"
        );
        freeze(&page, false);
        assert!(page_code_waits(&page, &code, 1_002), "the code still waits");
        assert_eq!(redeem_page_code(&page, &code, 1_003), PageRedeem::Paired);
        assert!(
            !page_code_waits(&page, &code, 1_003),
            "used, it waits no more"
        );
    }

    /// #s618 review: where the lock file cannot be opened (one a `sudo agent-monitor --pair-code`
    /// left, here a directory in its place), nothing about the page codes changes: no ask mints,
    /// no code pairs or is ignored, the start's clear says it could not, and nothing is written.
    #[test]
    fn a_lock_file_that_cannot_be_opened_changes_nothing() {
        let path = store_path("page-no-lock");
        let PageAsk::Minted(code) = ask_page_code(&path, 1_000) else {
            panic!("the first ask mints");
        };
        assert!(page_code_waits(&path, &code, 1_001));
        assert!(
            !page_code_waits(&path, &code, 1_000 + PAIR_CODE_TTL_SECS),
            "expired"
        );
        let mut lock = path.as_os_str().to_owned();
        lock.push(".lock");
        let lock = std::path::PathBuf::from(lock);
        std::fs::remove_file(&lock).unwrap();
        std::fs::create_dir(&lock).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            redeem_page_code(&path, &code, 1_001),
            PageRedeem::Missed { burned: false },
            "a right code pairs nothing"
        );
        assert_eq!(ask_page_code(&path, 1_100), PageAsk::Failed);
        assert_eq!(terminal_page_code(&path, 1_100), None);
        assert!(!ignore_page_code(&path, &code, 1_001));
        assert!(!clear_page_codes(&path));
        assert!(
            !page_code_waits(&path, &code, 1_001),
            "unknown, so not waiting"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before, "nothing was written");
        std::fs::remove_dir(&lock).unwrap();
        assert_eq!(redeem_page_code(&path, &code, 1_001), PageRedeem::Paired);
    }

    /// #s618 review: the page this machine's browser gets sends a code a link carried (`#code=…`)
    /// as a phone code, and a typed one as typed. (The browser case opens such a link.)
    #[test]
    fn the_here_page_sends_a_linked_code_as_a_phone_code() {
        let page = pair_page_here("agent-monitor --pair-code");
        assert!(page.contains(r#"fetch("/api/pair" + (q || ""),"#));
        let hash = page
            .lines()
            .find(|l| l.contains("box.value = m[1]"))
            .expect("the #code= line");
        assert!(hash.contains(r#"pair("?from=link")"#), "{hash}");
        let submit = page
            .lines()
            .find(|l| l.contains(r#"addEventListener("submit""#))
            .expect("the submit line");
        assert!(submit.contains(r#"pair("")"#), "{submit}");
    }

    /// #s618 review: the file lock, not only this process's own, holds off a change to the page
    /// codes: `--pair-code` is a second process. A second open of the lock file stands in for it
    /// (a file lock belongs to the open file, so it conflicts within one process too). And the
    /// lock file is the owner's alone, as the codes are.
    #[test]
    fn another_holder_of_the_lock_file_holds_off_every_page_code_change() {
        let path = store_path("page-flock");
        let PageAsk::Minted(code) = ask_page_code(&path, 1_000) else {
            panic!("the first ask mints");
        };
        let mut lock = path.as_os_str().to_owned();
        lock.push(".lock");
        let lock = std::path::PathBuf::from(lock);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for p in [&path, &lock] {
                let mode = std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{}", p.display());
            }
        }
        let held = std::fs::OpenOptions::new().write(true).open(&lock).unwrap();
        held.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (p2, c2) = (path.clone(), code.clone());
        let redeem = std::thread::spawn(move || {
            tx.send(redeem_page_code(&p2, &c2, 1_001)).unwrap();
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(400))
                .is_err(),
            "a redeem waits while another holds the lock file"
        );
        held.unlock().unwrap();
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap(),
            PageRedeem::Paired
        );
        redeem.join().unwrap();
    }
}

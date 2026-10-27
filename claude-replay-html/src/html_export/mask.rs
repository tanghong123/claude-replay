//! Opt-in secret masking for what leaves the machine (#365).
//!
//! Two surfaces carry transcript text off this machine: a `--dump-html` file someone sends on, and
//! the pages a paired phone reads over the tailnet. A transcript is full of keys an agent printed
//! or read — an `.env` it opened, a `curl -H "Authorization: Bearer …"` it ran — and nothing was
//! redacted on either. This replaces the known secret SHAPES, at render or serve time and never in
//! the transcript, with `*` of the SAME LENGTH: the bytes a page range-reads by offset (`/records`)
//! stay where they were, and a JSON string stays a JSON string.
//!
//! The rule set is small on purpose, after loongsuite-pilot's (a cheap prefilter, then rules):
//! provider keys by their published prefixes, JWTs, bearer tokens, the body of a private key, a
//! password in a URL. A generic "anything after `password=`" rule would mask half a transcript's
//! prose; a shape with a prefix is either a secret or a deliberate fake.
//!
//! Inside a JSON string it never touches a backslash or the byte after it, so an escape — and the
//! string's end — survives every replacement; and it leaves a string that is a `data:` URI or a long
//! run of base64 alone (a pasted image), where a key's letters could occur by chance.

/// What the monitor masks, from `<state>/mask-policy.json` beside `render-policy.json`:
/// `{"mode": "remote"}`. `off` (the default when the file is absent), `remote` — only for a client
/// that is not on this machine, the phone over the tailnet (the local desktop is untouched) — or
/// `always`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskPolicy {
    Off,
    Remote,
    Always,
}

impl MaskPolicy {
    /// Read from a policy file's text; anything unreadable is `Off` — masking is opt-in.
    pub fn parse(raw: &str) -> Self {
        let v: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        match v.get("mode").and_then(|m| m.as_str()) {
            Some("remote") => Self::Remote,
            Some("always") => Self::Always,
            _ => Self::Off,
        }
    }

    /// Whether a response to this client is masked.
    pub fn masks(self, client_is_local: bool) -> bool {
        match self {
            Self::Off => false,
            Self::Remote => !client_is_local,
            Self::Always => true,
        }
    }
}

/// The monitor's policy, read once from `<state>/mask-policy.json`. Tests get `Off` and never read
/// the disk, as the render policy's do (#153).
pub fn policy() -> MaskPolicy {
    #[cfg(test)]
    {
        MaskPolicy::Off
    }
    #[cfg(not(test))]
    {
        static POLICY: std::sync::OnceLock<MaskPolicy> = std::sync::OnceLock::new();
        *POLICY.get_or_init(|| {
            std::fs::read_to_string(super::sig::state_dir().join("mask-policy.json"))
                .map(|raw| MaskPolicy::parse(&raw))
                .unwrap_or(MaskPolicy::Off)
        })
    }
}

/// Mask a response body as the policy says for this client (#365): a JSON body string by string,
/// a plain-text one whole. Same length, so `Content-Length` and every offset hold.
pub fn mask_body(content_type: &str, body: &mut [u8]) -> usize {
    if content_type.starts_with("application/json") {
        mask_json_bytes(body)
    } else if content_type.starts_with("text/plain") {
        mask_text_bytes(body)
    } else {
        0
    }
}

const WORD: fn(u8) -> bool = |b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';

/// A token rule: a literal prefix, the bytes the token may carry after it, and how many at least.
struct Token {
    prefix: &'static [u8],
    body: fn(u8) -> bool,
    min: usize,
    /// At most this many body bytes, when the shape is fixed (an AWS key id is exactly 16).
    max: usize,
}

fn key_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}
fn alnum(b: u8) -> bool {
    b.is_ascii_alphanumeric()
}
fn upper_digit(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit()
}
fn alnum_underscore(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The provider keys, by their published prefixes. Longer prefixes first, so `sk-ant-` is
/// Anthropic's and not the OpenAI rule's.
const TOKENS: &[Token] = &[
    Token {
        prefix: b"sk-ant-",
        body: key_char,
        min: 20,
        max: usize::MAX,
    },
    Token {
        prefix: b"sk-proj-",
        body: key_char,
        min: 20,
        max: usize::MAX,
    },
    Token {
        prefix: b"sk-",
        body: key_char,
        min: 20,
        max: usize::MAX,
    },
    Token {
        prefix: b"github_pat_",
        body: alnum_underscore,
        min: 40,
        max: usize::MAX,
    },
    Token {
        prefix: b"ghp_",
        body: alnum,
        min: 30,
        max: usize::MAX,
    },
    Token {
        prefix: b"gho_",
        body: alnum,
        min: 30,
        max: usize::MAX,
    },
    Token {
        prefix: b"ghu_",
        body: alnum,
        min: 30,
        max: usize::MAX,
    },
    Token {
        prefix: b"ghs_",
        body: alnum,
        min: 30,
        max: usize::MAX,
    },
    Token {
        prefix: b"ghr_",
        body: alnum,
        min: 30,
        max: usize::MAX,
    },
    Token {
        prefix: b"glpat-",
        body: key_char,
        min: 20,
        max: usize::MAX,
    },
    Token {
        prefix: b"xoxb-",
        body: key_char,
        min: 10,
        max: usize::MAX,
    },
    Token {
        prefix: b"xoxp-",
        body: key_char,
        min: 10,
        max: usize::MAX,
    },
    Token {
        prefix: b"xoxa-",
        body: key_char,
        min: 10,
        max: usize::MAX,
    },
    Token {
        prefix: b"xoxs-",
        body: key_char,
        min: 10,
        max: usize::MAX,
    },
    Token {
        prefix: b"AKIA",
        body: upper_digit,
        min: 16,
        max: 16,
    },
    Token {
        prefix: b"ASIA",
        body: upper_digit,
        min: 16,
        max: 16,
    },
    Token {
        prefix: b"AIza",
        body: key_char,
        min: 35,
        max: 35,
    },
];

/// Mask every secret shape in `bytes`, in place, with `*` of the same length. Returns how many
/// were masked. The bytes may be a JSON string's raw contents: escapes are never touched.
pub fn mask_text_bytes(bytes: &mut [u8]) -> usize {
    let mut masked = 0;
    masked += mask_private_keys(bytes);
    let mut i = 0;
    while i < bytes.len() {
        if !may_start(bytes[i]) {
            i += 1;
            continue;
        }
        if let Some(end) = token_at(bytes, i).or_else(|| jwt_at(bytes, i)) {
            star(bytes, i, end);
            masked += 1;
            i = end;
            continue;
        }
        if let Some((from, end)) = bearer_at(bytes, i).or_else(|| url_password_at(bytes, i)) {
            star(bytes, from, end);
            masked += 1;
            i = end;
            continue;
        }
        i += 1;
    }
    masked
}

/// Whether a secret's shape can start with `b`: a token's prefix, a JWT's `e`, a bearer's `b`, a URL's
/// `:`. Every other byte is passed over at once (ordinary text masked 1.5 to 2 times slower when each
/// byte tried every prefix, #s288).
fn may_start(b: u8) -> bool {
    static STARTS: std::sync::OnceLock<[bool; 256]> = std::sync::OnceLock::new();
    STARTS.get_or_init(|| {
        let mut t = [false; 256];
        for tok in TOKENS {
            t[usize::from(tok.prefix[0])] = true;
        }
        for &b in b"ebB:" {
            t[usize::from(b)] = true;
        }
        t
    })[usize::from(b)]
}

/// Mask the secrets inside every JSON string of `body` (a JSON document or JSON lines), leaving
/// the structure — quotes, escapes, keys' names — exactly where it was. Returns how many.
pub fn mask_json_bytes(body: &mut [u8]) -> usize {
    let mut masked = 0;
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'"' {
            i += 1;
            continue;
        }
        let start = i + 1;
        let mut j = start;
        while j < body.len() && body[j] != b'"' {
            j += if body[j] == b'\\' { 2 } else { 1 };
        }
        let end = j.min(body.len());
        if !is_blob(&body[start..end]) {
            masked += mask_text_bytes(&mut body[start..end]);
        }
        i = end + 1;
    }
    masked
}

/// A string that is a `data:` URI or a long unbroken run of base64 — a pasted image's bytes.
fn is_blob(s: &[u8]) -> bool {
    s.starts_with(b"data:")
        || (s.len() > 2048
            && s.iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=')))
}

/// Replace `bytes[from..to]` with `*`, leaving every escape whole: a backslash and the byte after
/// it, or a `\uXXXX` and its four hex digits (#s286: a control character inside a private key's span,
/// as an ANSI colour code in terminal output is, read `\u****` and the record stopped being JSON).
fn star(bytes: &mut [u8], from: usize, to: usize) {
    let mut k = from;
    while k < to {
        if bytes[k] == b'\\' {
            k += if unicode_escape_at(bytes, k) { 6 } else { 2 };
            continue;
        }
        // A non-ASCII byte is part of a multi-byte character: left whole, so UTF-8 stays UTF-8.
        if bytes[k] >= 0x80 {
            k += 1;
            continue;
        }
        bytes[k] = b'*';
        k += 1;
    }
}

/// A JSON `\uXXXX` escape starts at `k`: a backslash, a `u` and four hex digits.
fn unicode_escape_at(bytes: &[u8], k: usize) -> bool {
    bytes.get(k) == Some(&b'\\')
        && bytes.get(k + 1) == Some(&b'u')
        && bytes
            .get(k + 2..k + 6)
            .is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit))
}

/// A word boundary before `i`: a token never starts inside another word (`risk-` is not `sk-`).
/// The masker reads a JSON string's RAW bytes, so the end of an escape is a boundary too: a key alone
/// on a line follows the `n` of `\n`, and a coloured one the `m` of `\u001b[32m`; both read as inside
/// a word and went out unmasked (#s288). The backslash is not counted: JSON a tool printed carries
/// its line breaks as `\\n`, and a key after one is as much a key. Dashes that themselves start a
/// word are a boundary as well: a diff's removed line (`\n-ghp_…`), a flag (`--`). The matchers ask
/// only where their prefix stands, and no prefix starts with a dash, so each walk back covers its own
/// run alone and the whole pass stays linear (it once recursed at every byte: a tool's 100,000-dash
/// rule cost 21 s, and a debug build's stack).
fn starts_word(bytes: &[u8], i: usize) -> bool {
    if boundary(bytes, i) {
        return true;
    }
    let mut at = i;
    while at > 0 && bytes[at - 1] == b'-' {
        at -= 1;
    }
    at < i && boundary(bytes, at)
}

/// The byte before `at` ends a word: none, a non-word byte, an escape, a terminal code.
fn boundary(bytes: &[u8], at: usize) -> bool {
    at == 0 || !WORD(bytes[at - 1]) || after_escape(bytes, at) || after_terminal_code(bytes, at)
}

/// An escape ends right before `i`: `\n`, `\t`, `\r`, `\b`, `\f`, or a whole `\uXXXX`.
fn after_escape(bytes: &[u8], i: usize) -> bool {
    (i >= 2 && bytes[i - 2] == b'\\' && b"ntrbf".contains(&bytes[i - 1]))
        || (i >= 6 && unicode_escape_at(bytes, i - 6))
}

/// A terminal control sequence ends right before `i`. Either a CSI — ESC `[`, its parameter and
/// intermediate bytes (space to `?`: digits, `;`, `:`, `?`), a final byte (`@` to `~`): a colour
/// (`m`), an erased line (`K`, a progress bar's `\r\u001b[K`), a cursor shown or hidden — or a short
/// escape: ESC, intermediate bytes (space to `/`), a final byte (`0` to `~`), as `ESC ( B` and
/// `ESC 7`. The ESC is any of its spellings (`esc_ends_at`).
fn after_terminal_code(bytes: &[u8], i: usize) -> bool {
    if i < 3 || bytes[i - 1] == b'\\' {
        return false;
    }
    let last = bytes[i - 1];
    if (0x40..=0x7e).contains(&last) {
        let mut k = i - 1;
        while k > 0 && (0x20..=0x3f).contains(&bytes[k - 1]) {
            k -= 1;
        }
        if k > 0 && bytes[k - 1] == b'[' && esc_ends_at(bytes, k - 1) {
            return true;
        }
    }
    if (0x30..=0x7e).contains(&last) && last != b'[' {
        let mut k = i - 1;
        while k > 0 && (0x20..=0x2f).contains(&bytes[k - 1]) {
            k -= 1;
        }
        return esc_ends_at(bytes, k);
    }
    false
}

/// An ESC ends right before `e`: the byte itself (plain text), JSON's `\u001b`, or one a script
/// printed as text (`\x1b`, `\033`, `\0033`, `\e`).
fn esc_ends_at(bytes: &[u8], e: usize) -> bool {
    (e >= 1 && bytes[e - 1] == 0x1b)
        || (e >= 6
            && unicode_escape_at(bytes, e - 6)
            && bytes[e - 4..e].eq_ignore_ascii_case(b"001b"))
        || (e >= 4
            && bytes[e - 4] == b'\\'
            && (bytes[e - 3..e].eq_ignore_ascii_case(b"x1b") || &bytes[e - 3..e] == b"033"))
        || (e >= 5 && bytes[e - 5] == b'\\' && &bytes[e - 4..e] == b"0033")
        || (e >= 2 && bytes[e - 2] == b'\\' && matches!(bytes[e - 1], b'e' | b'E'))
}

fn token_at(bytes: &[u8], i: usize) -> Option<usize> {
    for t in TOKENS {
        if !bytes[i..].starts_with(t.prefix) || !starts_word(bytes, i) {
            continue;
        }
        let from = i + t.prefix.len();
        let mut end = from;
        while end < bytes.len() && (t.body)(bytes[end]) && end - from < t.max {
            end += 1;
        }
        let n = end - from;
        // A fixed-length shape that runs on is some other word.
        let runs_on = t.max != usize::MAX && end < bytes.len() && (t.body)(bytes[end]);
        if n >= t.min && !runs_on {
            return Some(end);
        }
    }
    None
}

/// `eyJ…`.`eyJ…`.`…`: a JSON web token, three base64url segments of some length.
fn jwt_at(bytes: &[u8], i: usize) -> Option<usize> {
    if !bytes[i..].starts_with(b"eyJ") || !starts_word(bytes, i) {
        return None;
    }
    let seg = |from: usize| {
        let mut e = from;
        while e < bytes.len() && key_char(bytes[e]) {
            e += 1;
        }
        e
    };
    let a = seg(i);
    if a - i < 10 || bytes.get(a) != Some(&b'.') {
        return None;
    }
    let b = seg(a + 1);
    if b - a - 1 < 10 || bytes.get(b) != Some(&b'.') {
        return None;
    }
    let c = seg(b + 1);
    (c - b > 10).then_some(c)
}

/// `Bearer <token>`, any case: the token, not the word.
fn bearer_at(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    const WORD_B: &[u8] = b"bearer ";
    if i + WORD_B.len() > bytes.len()
        || !bytes[i..i + WORD_B.len()].eq_ignore_ascii_case(WORD_B)
        || !starts_word(bytes, i)
    {
        return None;
    }
    let from = i + WORD_B.len();
    let mut end = from;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphanumeric() || b"._~+/=-".contains(&bytes[end]))
    {
        end += 1;
    }
    (end - from >= 20).then_some((from, end))
}

/// `scheme://user:password@host`: the password.
fn url_password_at(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    if !bytes[i..].starts_with(b"://") {
        return None;
    }
    let user = i + 3;
    let mut colon = user;
    while colon < bytes.len() && colon - user <= 64 {
        match bytes[colon] {
            b':' => break,
            b'/' | b'@' | b'"' | b'\\' | b' ' | b'\n' => return None,
            _ => colon += 1,
        }
    }
    if colon == user || bytes.get(colon) != Some(&b':') {
        return None;
    }
    let from = colon + 1;
    let mut at = from;
    while at < bytes.len() && at - from <= 128 {
        match bytes[at] {
            b'@' => break,
            b'/' | b'"' | b'\\' | b' ' | b'\n' => return None,
            _ => at += 1,
        }
    }
    (at > from && bytes.get(at) == Some(&b'@')).then_some((from, at))
}

/// The body of every `-----BEGIN … PRIVATE KEY-----` block, its header and footer left so the
/// reader still sees that a key was there.
fn mask_private_keys(bytes: &mut [u8]) -> usize {
    const BEGIN: &[u8] = b"-----BEGIN ";
    const KEY: &[u8] = b"PRIVATE KEY-----";
    const END: &[u8] = b"-----END ";
    let find = |hay: &[u8], needle: &[u8], from: usize| {
        hay[from.min(hay.len())..]
            .windows(needle.len())
            .position(|w| w == needle)
            .map(|p| p + from)
    };
    let mut masked = 0;
    let mut i = 0;
    while let Some(b) = find(bytes, BEGIN, i) {
        // The header is one short line: `PRIVATE KEY-----` closes it within 64 bytes, so it is looked
        // for there alone (looked for to the end, a flood of headers with one key far after them
        // scanned the rest of the text once per header: a megabyte took 24 s, #s288).
        let window = &bytes[b..bytes.len().min(b + 64 + KEY.len())];
        let Some(k) = find(window, KEY, 0).map(|p| p + b) else {
            i = b + BEGIN.len();
            continue;
        };
        if bytes[b..k].contains(&b'\n') {
            i = b + BEGIN.len();
            continue;
        }
        let body = k + KEY.len();
        let Some(e) = find(bytes, END, body) else {
            break;
        };
        star(bytes, body, e);
        masked += 1;
        i = e + END.len();
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn masked(s: &str) -> String {
        let mut b = s.as_bytes().to_vec();
        mask_text_bytes(&mut b);
        String::from_utf8(b).unwrap()
    }

    // Fakes, assembled so this source never holds a real-looking key whole — CI's gitleaks reads
    // every line of it.
    fn fake(prefix: &str, n: usize, c: char) -> String {
        format!("{prefix}{}", c.to_string().repeat(n))
    }

    /// A private-key block's header and footer for `kind`, built from parts for the same reason.
    fn pem_lines(kind: &str) -> (String, String) {
        let tail = ["PRIVATE", "KEY-----"].join(" ");
        (
            format!("-----BEGIN {kind} {tail}"),
            format!("-----END {kind} {tail}"),
        )
    }

    #[test]
    fn every_provider_shape_is_masked_to_its_own_length() {
        for key in [
            fake("sk-ant-api03-", 40, 'A'),
            fake("sk-proj-", 40, 'b'),
            fake("sk-", 40, 'c'),
            fake("github_pat_", 60, 'D'),
            fake("ghp_", 36, 'e'),
            fake("glpat-", 20, 'f'),
            fake("xoxb-", 30, '1'),
            fake("AKIA", 16, 'Z'),
            fake("AIza", 35, 'g'),
        ] {
            let line = format!("export TOKEN={key} # set");
            let out = masked(&line);
            assert_eq!(out.len(), line.len(), "{key}: same length");
            assert!(!out.contains(&key), "{key} is masked: {out}");
            assert!(
                out.starts_with("export TOKEN=*") && out.ends_with(" # set"),
                "only the key: {out}"
            );
        }
    }

    #[test]
    fn jwts_bearer_tokens_url_passwords_and_private_keys_are_masked() {
        let jwt = format!(
            "{}.{}.{}",
            fake("eyJ", 20, 'h'),
            fake("eyJ", 20, 'i'),
            fake("", 20, 'j')
        );
        assert!(!masked(&format!("token={jwt}")).contains("eyJhh"), "a JWT");
        let bearer = masked(&format!(
            "curl -H 'Authorization: Bearer {}'",
            fake("", 32, 'k')
        ));
        assert!(
            bearer.contains("Bearer ****") && !bearer.contains("kkkk"),
            "the token, not the word: {bearer}"
        );
        let url = masked(&format!(
            "git clone https://hong:{}@example.test/r.git",
            "s3cretpass"
        ));
        assert_eq!(
            url, "git clone https://hong:**********@example.test/r.git",
            "the password only"
        );
        let (begin, end) = pem_lines("OPENSSH");
        let pem = format!("{begin}\n{}\n{end}", fake("", 64, 'l'));
        let out = masked(&pem);
        assert!(
            out.starts_with(&begin) && out.ends_with(&end),
            "header and footer stay: {out}"
        );
        assert!(!out.contains("llll"), "the body is masked: {out}");
    }

    #[test]
    fn ordinary_words_and_short_shapes_are_left_alone() {
        for s in [
            "a risk-free task-force plan",
            "sk-short",
            "the Bearer of bad news",
            "AKIA is not enough",
            "https://example.test/path:with@nothing",
            "no keys here at all",
            "https://user@host.test/",
        ] {
            assert_eq!(masked(s), s, "{s}");
        }
        // A fixed-length shape that runs on is some other word.
        let long = fake("AKIA", 20, 'Q');
        assert_eq!(masked(&long), long);
    }

    #[test]
    fn json_stays_json_and_an_image_is_left_alone() {
        let key = fake("sk-ant-api03-", 40, 'A');
        let (begin, end) = pem_lines("RSA");
        let pem = format!("{begin}\\n{}\\n{end}", fake("", 48, 'm'));
        let image = format!("data:image/png;base64,{}", fake("AKIA", 16, 'Z'));
        let doc = format!(
            "{{\"text\":\"use {key} here\\n\",\"pem\":\"{pem}\",\"img\":\"{image}\",\"q\":\"say \\\"hi\\\"\"}}\n{{\"x\":\"https://u:pw@h.test\"}}\n"
        );
        let mut b = doc.clone().into_bytes();
        let n = mask_json_bytes(&mut b);
        let out = String::from_utf8(b).unwrap();
        assert_eq!(out.len(), doc.len(), "same length, so offsets hold");
        assert_eq!(n, 3, "the key, the private key and the password: {out}");
        for line in out.lines() {
            serde_json::from_str::<serde_json::Value>(line)
                .unwrap_or_else(|e| panic!("{e}: {line}"));
        }
        let v: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert!(!v["text"].as_str().unwrap().contains(&key));
        assert!(
            v["text"].as_str().unwrap().ends_with("here\n"),
            "the escape survived"
        );
        assert_eq!(
            v["img"].as_str().unwrap(),
            image,
            "an image's bytes are never touched"
        );
        assert_eq!(v["q"].as_str().unwrap(), "say \"hi\"");
    }

    /// #s286: a private key's span in terminal output carries colour codes, which JSON writes as
    /// `\u001b`; the mask leaves each escape whole, so the record is still JSON and the escape still
    /// the character it was, while every character of the key's body is starred.
    #[test]
    fn an_escape_inside_a_masked_span_stays_whole() {
        let (begin, end) = pem_lines("OPENSSH");
        let body = fake("", 40, 'n');
        let text = format!("\u{1b}[1m{begin}\n{body}\u{1b}[0m\n\u{7}{end}\u{1b}[0m");
        let doc = serde_json::json!({ "out": text }).to_string() + "\n";
        assert!(
            doc.contains("\\u001b"),
            "serde writes the colour code as an escape: {doc}"
        );
        let mut b = doc.clone().into_bytes();
        assert_eq!(mask_json_bytes(&mut b), 1, "the key's body");
        let out = String::from_utf8(b).unwrap();
        assert_eq!(out.len(), doc.len(), "same length, so offsets hold");
        let v: serde_json::Value =
            serde_json::from_str(out.trim_end()).unwrap_or_else(|e| panic!("{e}: {out}"));
        let got = v["out"].as_str().unwrap();
        assert!(!got.contains("nnnn"), "the body is masked: {got}");
        assert!(got.contains(&begin) && got.contains(&end), "{got}");
        assert_eq!(
            got.matches('\u{1b}').count(),
            3,
            "every colour code survives as itself: {got:?}"
        );
        assert!(got.contains('\u{7}'), "and the bell: {got:?}");
        // A backslash that starts no `\u` escape is still skipped with the byte after it alone.
        let mut b = br#"{"t":"\uZZ ok"}"#.to_vec();
        star(&mut b, 6, 10);
        assert_eq!(&b, br#"{"t":"\u** ok"}"#);
    }

    /// #s288: a key after an escape (a line break, a tab) or a colour code starts a word. The
    /// masker reads raw JSON bytes, where such a key follows a letter, and every one of these went
    /// out whole.
    #[test]
    fn a_key_after_an_escape_or_a_colour_code_is_masked() {
        let key = fake("sk-ant-api03-", 40, 'A');
        let ghp = fake("ghp_", 36, 'e');
        let jwt = format!(
            "{}.{}.{}",
            fake("eyJ", 20, 'h'),
            fake("eyJ", 20, 'i'),
            fake("", 20, 'j')
        );
        let bearer = format!("Bearer {}", fake("", 32, 'k'));
        let texts = [
            format!("export it:\n{key}\n"),
            format!("a\t{key}"),
            format!("a\r\n{jwt}"),
            format!("headers:\n{bearer}"),
            format!("\u{1b}[32m{ghp}\u{1b}[0m"),
            format!("\u{1b}[1;31m{key}"),
            // A progress bar redraws its line: CR, erase the line, then the text.
            format!("50%\r\u{1b}[K{ghp}"),
            format!("x\u{1b}[2K{key}"),
            format!("x\u{1b}[?25h{ghp}"),
            format!("x\u{1b}[38:5:214m{ghp}"),
            // A diff's removed line, and a colour code a script printed as text.
            format!("@@ -1 +1 @@\n-{ghp}\n+REDACTED"),
            format!("echo '\\x1b[32m{ghp}'"),
            format!("echo -e '\\033[32m{ghp}'"),
            format!("echo -e '\\0033[32m{ghp}'"),
            format!("(---{key}"),
            format!("printf '\\e[1m{key}'"),
            format!("x\u{1b}(B{ghp}"),
            format!("x\u{1b}7{ghp}"),
            // JSON a tool printed: its own escapes arrive as literal `\n` and `\u001b` text.
            format!("{{\"k\":\"line\\n{key}\"}}"),
            format!("{{\"k\":\"\\u001b[33m{ghp}\"}}"),
        ];
        for text in texts {
            let doc = serde_json::json!({ "out": text }).to_string();
            let mut b = doc.clone().into_bytes();
            assert!(mask_json_bytes(&mut b) >= 1, "nothing masked in {doc}");
            let out = String::from_utf8(b).unwrap();
            assert_eq!(out.len(), doc.len(), "same length: {out}");
            let v: serde_json::Value =
                serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
            let got = v["out"].as_str().unwrap();
            for secret in ["AAAA", "eeee", "hhhh", "kkkk"] {
                assert!(!got.contains(secret), "{secret} left in {got:?}");
            }
        }
        // Plain text with the colour code's own ESC byte.
        assert!(!masked(&format!("\u{1b}[32m{ghp}")).contains("eeee"));
        // Still a word, with bodies long enough to be keys: a prefix inside a longer word after an
        // escape, a `u` and four hex digits that are no escape, a word ending in a dash.
        for s in [
            "\\nrisk-assessment-for-the-quarter-review".to_string(),
            format!("a\\nx{} end", fake("AKIA", 16, 'B')),
            "menu1234sk-abcdefghijklmnopqrstuvwxyz".to_string(),
            "pre-sk-abcdefghijklmnopqrstuvwxyz0123".to_string(),
        ] {
            assert_eq!(masked(&s), s, "{s}");
        }
    }

    /// A long run of dashes (a tool's horizontal rule) costs a linear pass: the word-start test walks
    /// back over dashes only where a prefix stands, each walk over its own run. It used to recurse once per dash
    /// at every byte: 100,000 dashes took 21 s, and overflowed a debug build's stack.
    #[test]
    fn a_long_rule_of_dashes_masks_in_linear_time() {
        let rule = "-".repeat(200_000);
        let started = std::time::Instant::now();
        assert_eq!(masked(&rule), rule);
        // A flood of private-key headers with one key far after them.
        let (begin, end) = pem_lines("RSA");
        let flood = format!(
            "{}{begin}\n{}\n{end}",
            "-----BEGIN x\n".repeat(80_000),
            fake("", 48, 'p')
        );
        assert!(!masked(&flood).contains("pppp"), "the key after the flood");
        let doc =
            serde_json::json!({ "out": format!("{rule}\n{}", fake("ghp_", 36, 'e')) }).to_string();
        let mut b = doc.into_bytes();
        assert_eq!(mask_json_bytes(&mut b), 1);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn the_policy_is_off_unless_asked() {
        assert_eq!(MaskPolicy::parse(""), MaskPolicy::Off);
        assert_eq!(
            MaskPolicy::parse(r#"{"mode":"remote"}"#),
            MaskPolicy::Remote
        );
        assert_eq!(
            MaskPolicy::parse(r#"{"mode":"always"}"#),
            MaskPolicy::Always
        );
        assert!(!MaskPolicy::Remote.masks(true) && MaskPolicy::Remote.masks(false));
        assert!(MaskPolicy::Always.masks(true) && !MaskPolicy::Off.masks(false));
    }
}

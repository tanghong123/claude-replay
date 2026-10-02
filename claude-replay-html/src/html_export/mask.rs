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

/// Replace `bytes[from..to]` with `*`, leaving every escape (a backslash and the byte after it).
fn star(bytes: &mut [u8], from: usize, to: usize) {
    let mut k = from;
    while k < to {
        if bytes[k] == b'\\' {
            k += 2;
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

/// A word boundary before `i`: a token never starts inside another word (`risk-` is not `sk-`).
fn starts_word(bytes: &[u8], i: usize) -> bool {
    i == 0 || !WORD(bytes[i - 1])
}

fn token_at(bytes: &[u8], i: usize) -> Option<usize> {
    if !starts_word(bytes, i) {
        return None;
    }
    for t in TOKENS {
        if !bytes[i..].starts_with(t.prefix) {
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
    if !starts_word(bytes, i) || !bytes[i..].starts_with(b"eyJ") {
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
    if i + WORD_B.len() > bytes.len() || !starts_word(bytes, i) {
        return None;
    }
    if !bytes[i..i + WORD_B.len()].eq_ignore_ascii_case(WORD_B) {
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
        let Some(k) = find(bytes, KEY, b) else { break };
        // The header is one line: `PRIVATE KEY-----` must close it.
        if bytes[b..k].contains(&b'\n') || k - b > 64 {
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

//! Copy text to the system clipboard, with a portable fallback.
//!
//! Prefers the OS clipboard via `arboard` (works in any local terminal, including
//! macOS Terminal.app). When that's unavailable — e.g. a headless / SSH session
//! with no display server — it falls back to an **OSC 52** escape written to the
//! terminal, which modern terminals (iTerm2 / kitty / WezTerm) and tmux honor.

use std::cell::RefCell;
use std::io::Write;

thread_local! {
    // Kept alive for the whole session: on X11 the clipboard owner must outlive the
    // `set_text` call to serve paste requests. `None` if no OS clipboard is reachable.
    static OS: RefCell<Option<arboard::Clipboard>> =
        RefCell::new(arboard::Clipboard::new().ok());
}

/// Copy `text` to the clipboard. Tries the OS clipboard first, then OSC 52.
/// Best-effort: silently no-ops if neither path is available (nothing to surface
/// to a TUI mid-render).
pub fn copy(text: &str) {
    if text.is_empty() {
        return;
    }
    let via_os = OS.with(|c| {
        c.borrow_mut()
            .as_mut()
            .map(|cb| cb.set_text(text.to_owned()).is_ok())
            .unwrap_or(false)
    });
    if !via_os {
        let _ = osc52(text);
    }
}

/// Write the OSC 52 set-clipboard escape (`ESC ] 52 ; c ; <base64> BEL`) to stdout.
fn osc52(text: &str) -> std::io::Result<()> {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let mut out = std::io::stdout().lock();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Standard base64 (RFC 4648, with `=` padding) — small enough to avoid a dep.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decode standard base64 (RFC 4648), tolerating `=` padding and embedded
/// whitespace/newlines (transcript base64 is often line-wrapped). Returns `None` on
/// an invalid character. Used to write embedded image attachments to disk.
pub(crate) fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        let val = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | val;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::{base64, base64_decode};

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_decode_roundtrips_and_tolerates_whitespace() {
        for v in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foobar",
            &[0u8, 255, 16, 128, 3][..],
        ] {
            assert_eq!(base64_decode(&base64(v)).as_deref(), Some(v));
        }
        // Line-wrapped input (as transcripts store image data) decodes the same.
        assert_eq!(base64_decode("Zm9v\nYmFy").as_deref(), Some(&b"foobar"[..]));
        assert_eq!(base64_decode("!!bad"), None);
    }
}

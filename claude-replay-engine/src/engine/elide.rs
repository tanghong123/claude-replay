//! The bounded eliding line reader — the only component that touches raw transcript bytes
//! (#193, `design/bounded-line-reads.md`).
//!
//! Adopted from `agent-metrics/src/elide.rs` (the seed), with the engine's deltas:
//!
//! - **The framed, substitution-exact marker.** An oversized string value is emitted as
//!   `{prefix}<elided:{off},{len}>{postfix}` — the first [`PREFIX_KEEP`] and last
//!   [`POSTFIX_KEEP`] bytes of the value kept in place, the marker standing for exactly the
//!   dropped middle. `off` is the **absolute file offset** of the first dropped byte and
//!   `len` the dropped count, so splicing `file[off, off + len)` over the marker substring
//!   reconstructs the original line **byte for byte** — elision is invertible, and the round
//!   trip is this module's own test oracle.
//! - **Policy through the seam** (design α, α-lite form): [`Elision`] decides *which* values
//!   may be elided. `Aggressive` is the seed's size-only rule (provably metric-neutral —
//!   nothing metric-bearing exceeds the threshold); `Keys` elides only values whose enclosing
//!   key chain ends in an adapter-listed suffix (attachment bodies the fold defers), keeping
//!   the sans-io fold invariant `fold(elide(line)) ≡ fold(line)`; `None` elides nothing and
//!   is for identity-sensitive reads (a CRC over elided text would churn with the constants).
//! - **Two hardenings the seed wants back** (its `pending` buffered a non-elidable string
//!   without bound): a key or policy-denied value that outgrows the threshold switches to
//!   **passthrough** — streamed to the output, governed by [`ELIDE_CEILING`] — instead of
//!   accumulating in `pending`; and both frame cuts land on escape-sequence boundaries so
//!   per-part unescaping of prefix / dropped / postfix stays exact.
//!
//! Two invariants above all (the seed's, unchanged):
//!
//! 1. **Offsets count RAW bytes.** `raw_len` is always the true consumed length; the marker's
//!    `off`/`len` index the file on disk, never the elided text.
//! 2. **Only values are elided, never keys** — eliding a key changes the document's shape.
//!    The scanner falls back to copying whenever it is unsure.
//!
//! Torn lines (no newline yet — a write in progress) never emit a marker: a marker's span is
//! validated against the value's closing quote, which a torn value does not have, and the
//! line will be re-read whole once complete. `finish` emits the kept prefix only.

use std::io::BufRead;

/// A string value longer than this is elided (policy permitting). Comfortably above every
/// metric-bearing field and far below every observed attachment.
pub const ELIDE_STRING_BYTES: usize = 64 * 1024;

/// Lines up to this size skip the scanner entirely — ~99.9 % of lines take this path.
pub const SCAN_THRESHOLD: usize = 256 * 1024;

/// Last resort: a line whose non-elidable output alone exceeds this is skipped rather than
/// buffered — consumed to its newline, `out` cleared, and **counted** by the caller.
/// Unreachable in practice with elision in place; never a routine data-loss policy.
pub const ELIDE_CEILING: usize = 64 * 1024 * 1024;

/// Kept head of an elided value (`K`): covers Codex's `data:<mime>;base64,` header (~22 B)
/// and every magic-byte signature, so payload-shape recognizers survive elision.
pub const PREFIX_KEEP: usize = 64;

/// Kept tail of an elided value (`J`): the value's own last bytes — needed to reconstruct
/// it, shown as the tail of an elided text, and the load-time content check's comparand.
pub const POSTFIX_KEEP: usize = 64;

/// Longest tracked key; a longer key becomes an unmatchable sentinel (fail-safe: no elision).
const KEY_CAP: usize = 48;
/// Deepest tracked nesting; deeper containers count overflow and never match (fail-safe).
const DEPTH_CAP: usize = 8;
/// Ring capacity: [`POSTFIX_KEEP`] plus the longest escape unit (`\uXXXX`), so trimming the
/// head to a unit boundary still leaves ≥ `POSTFIX_KEEP − 6` bytes of postfix.
const RING_CAP: usize = POSTFIX_KEEP + 6;

/// Which values may be elided — the α-lite policy (`design/bounded-line-reads.md` §6).
/// Adapters hand this out through the seam exactly as they hand out
/// [`Shaping`](crate::engine::replay::Shaping): a `'static` describing the agent's
/// attachment-body nodes.
#[derive(Clone, Copy)]
pub enum Elision {
    /// Elide nothing. For identity-sensitive reads (CRC/dedup) — output is verbatim,
    /// bounded by [`ELIDE_CEILING`] alone.
    None,
    /// The seed's size-only rule: any value over the threshold. Provably metric-neutral;
    /// the metrics folds' policy.
    Aggressive,
    /// Elide only values whose enclosing object-key chain ends in one of these suffixes
    /// (e.g. `&[&["file", "base64"], &["source", "data"]]`). Array levels contribute no
    /// key; a keyless container, an oversized key, or nesting past the tracked depth
    /// never matches — fail-safe toward keeping bytes.
    Keys(&'static [&'static [&'static str]]),
}

/// What one read consumed. `raw_len` is always the true byte length, including the newline,
/// and is what the caller adds to its file offset.
#[derive(Debug, PartialEq)]
pub enum LineOutcome {
    Eof,
    /// A final line with no newline: a write in progress. A durable cursor must stay
    /// unadvanced so the next run re-reads the line whole (`raw_len` is reported for the
    /// one-shot caller that chooses to consume it). `out` holds a marker-free preview.
    Torn {
        raw_len: u64,
    },
    Complete {
        raw_len: u64,
        /// Bytes dropped by elision across the line (0 for the overwhelming majority).
        elided: u64,
        /// The line blew past [`ELIDE_CEILING`]; `out` holds nothing usable and the caller
        /// must count it as skipped.
        skipped: bool,
    },
}

/// Where the scanner is in the document, so that only *values* are elided.
#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Object,
    Array,
}

/// A tracked enclosing key: its raw bytes, or the unmatchable sentinel (too long, or absent
/// — a keyless container such as an array element object). The sentinel never matches.
#[derive(Clone)]
enum TrackedKey {
    Bytes(Vec<u8>),
    Unmatchable,
}

/// Streaming JSON filter. Fed one byte at a time; appends to `out`.
///
/// Not a validator — malformed input passes through as faithfully as possible so the parser
/// downstream reports the problem, not this.
struct Elider {
    policy: Elision,
    /// Absolute file offset of the line's first byte — the base every marker indexes.
    line_start: u64,
    /// Intra-line position of the NEXT byte to be fed.
    pos: u64,
    ctx: Vec<Ctx>,
    /// True when the next string encountered is a value (so elidable) rather than a key.
    expecting_value: bool,
    in_string: bool,
    /// The previous byte was a backslash inside a string (drives quote semantics).
    escaped: bool,
    /// Bytes remaining in the current escape unit (`\x` = 1, `\uXXXX` = 5 after the `\`) —
    /// the frame cuts land only where this is 0.
    esc_unit: u8,
    /// This string began where a value was expected.
    string_is_value: bool,
    /// Intra-line position of the current string's first content byte.
    content_start: u64,
    /// Bytes of the current string held back pending the size decision (opening quote
    /// included as its first byte).
    pending: Vec<u8>,
    /// The current string outgrew the threshold and may NOT be elided (a key, or the policy
    /// said no): stream straight to `out` instead of buffering without bound.
    passthrough: bool,
    /// The current string is being elided.
    eliding: bool,
    /// The kept, boundary-aligned head of the eliding string.
    prefix: Vec<u8>,
    /// Absolute file offset of the first dropped byte.
    drop_off: u64,
    /// The eliding string's last bytes as `(byte, starts_a_unit)` — trimmed at the close to
    /// a unit boundary and kept as the postfix.
    ring: std::collections::VecDeque<(u8, bool)>,
    /// Content bytes of the eliding string so far (prefix + dropped + ring).
    content_len: u64,
    /// Enclosing keys for `Elision::Keys` (containers opened by no key push the sentinel),
    /// capped at [`DEPTH_CAP`].
    key_stack: Vec<TrackedKey>,
    /// Containers opened beyond the cap — while > 0, nothing matches (fail-safe).
    depth_overflow: u32,
    /// The most recently completed key string — names the next value or container.
    last_key: Option<TrackedKey>,
    /// Capture buffer for the current string while it may turn out to be a key.
    key_capture: Vec<u8>,
    key_oversized: bool,
    /// Total bytes dropped across the line.
    removed: u64,
}

impl Elider {
    fn new(line_start: u64, policy: Elision) -> Self {
        Elider {
            policy,
            line_start,
            pos: 0,
            ctx: Vec::new(),
            // A bare top-level string is a value.
            expecting_value: true,
            in_string: false,
            escaped: false,
            esc_unit: 0,
            string_is_value: false,
            content_start: 0,
            pending: Vec::new(),
            passthrough: false,
            eliding: false,
            prefix: Vec::new(),
            drop_off: 0,
            ring: std::collections::VecDeque::with_capacity(RING_CAP),
            content_len: 0,
            key_stack: Vec::new(),
            depth_overflow: 0,
            last_key: None,
            key_capture: Vec::new(),
            key_oversized: false,
            removed: 0,
        }
    }

    fn push(&mut self, b: u8, out: &mut Vec<u8>) {
        let p = self.pos;
        self.pos += 1;
        if self.in_string {
            self.in_string_byte(b, out);
            return;
        }
        match b {
            b'"' => {
                self.in_string = true;
                self.escaped = false;
                self.esc_unit = 0;
                self.string_is_value = self.expecting_value;
                self.eliding = false;
                self.passthrough = false;
                self.content_start = p + 1;
                self.content_len = 0;
                self.pending.clear();
                self.pending.push(b);
                self.key_capture.clear();
                self.key_oversized = false;
            }
            b'{' | b'[' => {
                self.ctx
                    .push(if b == b'{' { Ctx::Object } else { Ctx::Array });
                // The next string inside an object is a key; array elements are values.
                self.expecting_value = b == b'[';
                let opener = self.last_key.take().unwrap_or(TrackedKey::Unmatchable);
                if self.key_stack.len() < DEPTH_CAP {
                    self.key_stack.push(opener);
                } else {
                    self.depth_overflow += 1;
                }
                out.push(b);
            }
            b'}' | b']' => {
                self.ctx.pop();
                if self.depth_overflow > 0 {
                    self.depth_overflow -= 1;
                } else {
                    self.key_stack.pop();
                }
                self.expecting_value = false;
                self.last_key = None;
                out.push(b);
            }
            b':' => {
                self.expecting_value = true;
                out.push(b);
            }
            b',' => {
                // Back to a key in an object; still values in an array.
                self.expecting_value = self.ctx.last() != Some(&Ctx::Object);
                out.push(b);
            }
            _ => out.push(b),
        }
    }

    fn in_string_byte(&mut self, b: u8, out: &mut Vec<u8>) {
        // A frame cut may land before this byte iff no escape unit is in flight.
        let starts_unit = self.esc_unit == 0;
        if self.escaped {
            self.escaped = false;
            self.esc_unit = if b == b'u' { 4 } else { 0 };
            self.take_string_byte(b, starts_unit, out);
            return;
        }
        if self.esc_unit > 0 {
            self.esc_unit -= 1;
            self.take_string_byte(b, starts_unit, out);
            return;
        }
        match b {
            b'\\' => {
                self.escaped = true;
                self.esc_unit = 1;
                self.take_string_byte(b, starts_unit, out);
            }
            b'"' => {
                self.in_string = false;
                if self.eliding {
                    self.emit_elided(out);
                } else if self.passthrough {
                    out.push(b);
                } else {
                    out.extend_from_slice(&self.pending);
                    out.push(b);
                    if !self.string_is_value {
                        // A completed key: remember it for the value or container it names.
                        self.last_key = Some(if self.key_oversized {
                            TrackedKey::Unmatchable
                        } else {
                            TrackedKey::Bytes(std::mem::take(&mut self.key_capture))
                        });
                    }
                }
                self.pending.clear();
                // A finished string is never itself followed by a value.
                self.expecting_value = false;
            }
            _ => self.take_string_byte(b, starts_unit, out),
        }
    }

    /// One content byte of the current string. `starts_unit` = a frame cut may land before
    /// this byte.
    fn take_string_byte(&mut self, b: u8, starts_unit: bool, out: &mut Vec<u8>) {
        if self.passthrough {
            out.push(b);
            return;
        }
        if self.eliding {
            self.content_len += 1;
            if self.ring.len() == RING_CAP {
                self.ring.pop_front();
            }
            self.ring.push_back((b, starts_unit));
            return;
        }
        self.pending.push(b);
        if !self.string_is_value {
            if self.key_capture.len() < KEY_CAP {
                self.key_capture.push(b);
            } else {
                self.key_oversized = true;
            }
        }
        if self.pending.len() - 1 > ELIDE_STRING_BYTES {
            if self.string_is_value && self.policy_allows() {
                self.begin_eliding();
            } else {
                // A key, or a policy-protected value: stream it through — `pending` must
                // not buffer without bound (the ceiling on `out` is the governor). The
                // seed buffered here; this is the hardening the design names.
                out.extend_from_slice(&self.pending);
                self.pending.clear();
                self.pending.shrink_to_fit();
                self.passthrough = true;
            }
        }
    }

    /// May the value string currently buffering be elided, per the α-lite key-suffix rule?
    fn policy_allows(&self) -> bool {
        let pats = match self.policy {
            Elision::None => return false,
            Elision::Aggressive => return true,
            Elision::Keys(p) => p,
        };
        if self.depth_overflow > 0 {
            return false; // fail-safe: we no longer know where we are
        }
        // Innermost-first chain: the key naming this value, then the enclosing keys. The
        // sentinel ends the usable suffix — anything beyond it cannot be matched honestly.
        let mut chain: Vec<&[u8]> = Vec::new();
        match &self.last_key {
            Some(TrackedKey::Bytes(k)) => chain.push(k),
            Some(TrackedKey::Unmatchable) => return false,
            None => {}
        }
        for k in self.key_stack.iter().rev() {
            match k {
                TrackedKey::Bytes(b) => chain.push(b),
                TrackedKey::Unmatchable => break,
            }
        }
        pats.iter().any(|pat| {
            pat.len() <= chain.len()
                && pat
                    .iter()
                    .rev()
                    .zip(chain.iter())
                    .all(|(want, have)| *have == want.as_bytes())
        })
    }

    /// The buffered value crossed the threshold and the policy said yes: fix the prefix at
    /// the last unit boundary ≤ [`PREFIX_KEEP`], record the absolute drop offset, seed the
    /// postfix ring from the buffered tail, and drop the middle from here on.
    fn begin_eliding(&mut self) {
        let content = &self.pending[1..]; // pending[0] is the opening quote
                                          // One boundary walk over the buffered content: the prefix cut, and unit flags for
                                          // the tail bytes that seed the ring.
        let mut esc: u8 = 0;
        let mut prefix_len = 0usize;
        let tail_from = content.len().saturating_sub(RING_CAP);
        let mut tail_flags = [false; RING_CAP];
        for (i, &cb) in content.iter().enumerate() {
            let starts_unit = esc == 0;
            if starts_unit && i <= PREFIX_KEEP {
                prefix_len = i;
            }
            if i >= tail_from {
                tail_flags[i - tail_from] = starts_unit;
            }
            if esc == 0 {
                if cb == b'\\' {
                    esc = 1;
                }
            } else if esc == 1 {
                esc = if cb == b'u' { 4 } else { 0 };
            } else {
                esc -= 1;
            }
        }
        self.prefix.clear();
        self.prefix.extend_from_slice(&content[..prefix_len]);
        self.drop_off = self.line_start + self.content_start + prefix_len as u64;
        self.ring.clear();
        for i in tail_from..content.len() {
            self.ring.push_back((content[i], tail_flags[i - tail_from]));
        }
        self.content_len = content.len() as u64;
        self.eliding = true;
        self.pending.clear();
        self.pending.shrink_to_fit();
    }

    /// Close an elided value: trim the ring head to a unit boundary (what remains is the
    /// postfix), compute the dropped span, and emit
    /// `"{prefix}<elided:{off},{len}>{postfix}"`.
    fn emit_elided(&mut self, out: &mut Vec<u8>) {
        while let Some(&(_, starts_unit)) = self.ring.front() {
            if starts_unit {
                break;
            }
            self.ring.pop_front();
        }
        // content > threshold ⇒ dropped ≥ threshold − prefix − ring > 0, always.
        let dropped = self.content_len - self.prefix.len() as u64 - self.ring.len() as u64;
        out.push(b'"');
        out.extend_from_slice(&self.prefix);
        out.extend_from_slice(format!("<elided:{},{}>", self.drop_off, dropped).as_bytes());
        for &(rb, _) in self.ring.iter() {
            out.push(rb);
        }
        out.push(b'"');
        self.removed += dropped;
        self.eliding = false;
        self.ring.clear();
        self.prefix.clear();
    }

    /// Flush a string still open at end of input (a torn line). **Never emits a marker**: a
    /// torn value has no closing quote to validate against and will be re-read whole once
    /// the writer finishes it — only the kept prefix (or the small buffered string) is
    /// emitted as a preview.
    fn finish(&mut self, out: &mut Vec<u8>) {
        if self.in_string {
            if self.eliding {
                out.push(b'"');
                out.extend_from_slice(&self.prefix);
            } else if !self.passthrough {
                out.extend_from_slice(&self.pending);
            }
        }
        self.pending.clear();
    }
}

/// Read one line, eliding oversized string values per `policy`. `out` is cleared and filled
/// with the (possibly elided) line **including** its trailing newline. `line_start` is the
/// absolute file offset of the line's first byte — the base every emitted marker indexes.
///
/// Lines up to [`SCAN_THRESHOLD`] are copied verbatim with no scanning. Past that, the
/// buffered prefix is re-fed through the filter (positions and key state rebuild exactly,
/// since the same bytes replay in order) and the rest streams through it, so memory stays
/// bounded by the threshold plus the elided output, backstopped by [`ELIDE_CEILING`].
pub fn read_line_elided<R: BufRead>(
    reader: &mut R,
    out: &mut Vec<u8>,
    line_start: u64,
    policy: Elision,
) -> std::io::Result<LineOutcome> {
    out.clear();
    let mut raw_len: u64 = 0;
    let mut elider: Option<Elider> = None;
    let mut skipped = false;

    loop {
        // Decide from the buffer, then release the borrow before consuming.
        let (chunk_len, found_newline) = {
            let avail = reader.fill_buf()?;
            if avail.is_empty() {
                // EOF. Anything buffered is a line with no terminator.
                if raw_len == 0 {
                    return Ok(LineOutcome::Eof);
                }
                if let Some(e) = elider.as_mut() {
                    e.finish(out);
                }
                return Ok(LineOutcome::Torn { raw_len });
            }
            match avail.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (avail.len(), false),
            }
        };

        {
            let avail = reader.fill_buf()?;
            let chunk = &avail[..chunk_len];
            if skipped {
                // Past the ceiling: consume to the newline, keep nothing.
            } else if let Some(e) = elider.as_mut() {
                for &b in chunk {
                    e.push(b, out);
                }
            } else if raw_len as usize + chunk_len > SCAN_THRESHOLD {
                // Threshold crossed: switch on the filter and re-feed the verbatim prefix
                // through it so its state — positions and key stack included — is correct.
                let prefix = std::mem::take(out);
                let mut e = Elider::new(line_start, policy);
                for &b in &prefix {
                    e.push(b, out);
                }
                for &b in chunk {
                    e.push(b, out);
                }
                elider = Some(e);
            } else {
                out.extend_from_slice(chunk);
            }
        }
        reader.consume(chunk_len);
        raw_len += chunk_len as u64;

        if !skipped && out.len() > ELIDE_CEILING {
            out.clear();
            out.shrink_to_fit();
            skipped = true;
        }

        if found_newline {
            let elided = elider.as_ref().map(|e| e.removed).unwrap_or(0);
            return Ok(LineOutcome::Complete {
                raw_len,
                elided,
                skipped,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_one_with(input: &str, policy: Elision) -> (String, LineOutcome) {
        let mut r = Cursor::new(input.as_bytes().to_vec());
        let mut out = Vec::new();
        let outcome = read_line_elided(&mut r, &mut out, 0, policy).unwrap();
        (String::from_utf8_lossy(&out).into_owned(), outcome)
    }

    fn read_one(input: &str) -> (String, LineOutcome) {
        read_one_with(input, Elision::Aggressive)
    }

    /// The reference implementation of the splice contract: replace every marker substring
    /// with `file[off, off + len)`. Written against the PUBLIC marker format — step 3's
    /// oracle reuses it. Only for fixtures whose small values contain no literal markers.
    fn unelide(elided: &[u8], file: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(file.len());
        let mut i = 0;
        while i < elided.len() {
            if elided[i..].starts_with(b"<elided:") {
                if let Some(end) = elided[i..].iter().position(|&b| b == b'>') {
                    let body = &elided[i + 8..i + end];
                    let txt = std::str::from_utf8(body).unwrap();
                    let (off, len) = txt.split_once(',').unwrap();
                    let (off, len): (usize, usize) = (off.parse().unwrap(), len.parse().unwrap());
                    out.extend_from_slice(&file[off..off + len]);
                    i += end + 1;
                    continue;
                }
            }
            out.push(elided[i]);
            i += 1;
        }
        out
    }

    /// The §4 invariant as an executable property, over one whole input.
    fn assert_round_trip(input: &str) {
        let (got, _) = read_one(input);
        assert_eq!(
            String::from_utf8_lossy(&unelide(got.as_bytes(), input.as_bytes())),
            input,
            "unelide(elide(line)) must reconstruct the line byte for byte"
        );
    }

    #[test]
    fn a_small_line_passes_through_byte_for_byte() {
        let line = "{\"a\":1,\"b\":\"hi\",\"c\":[1,2,{\"d\":null}]}\n";
        let (got, outcome) = read_one(line);
        assert_eq!(got, line);
        assert!(matches!(
            outcome,
            LineOutcome::Complete {
                elided: 0,
                skipped: false,
                ..
            }
        ));
    }

    #[test]
    fn raw_len_counts_real_bytes_even_when_elided() {
        let blob = "x".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"pad\":\"{}\",\"b\":\"{}\"}}\n",
            "y".repeat(SCAN_THRESHOLD),
            blob
        );
        let (got, outcome) = read_one(&line);
        match outcome {
            LineOutcome::Complete {
                raw_len,
                elided,
                skipped,
            } => {
                assert_eq!(raw_len, line.len() as u64, "offset must track the FILE");
                assert!(elided > 0);
                assert!(!skipped);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(got.len() < line.len() / 2, "output should be far smaller");
        // Still parses, still the same shape.
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert!(v.get("pad").is_some() && v.get("b").is_some());
        assert_round_trip(&line);
    }

    /// The framed marker: prefix and postfix are the value's own head and tail, and the
    /// marker's span is exactly the dropped middle — checked by splicing it back.
    #[test]
    fn the_marker_is_framed_and_substitution_exact() {
        let blob = format!("HEAD{}TAIL", "m".repeat(ELIDE_STRING_BYTES * 2));
        let line = format!(
            "{{\"pad\":\"{}\",\"big\":\"{blob}\"}}\n",
            "w".repeat(SCAN_THRESHOLD)
        );
        let (got, _) = read_one(&line);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        let big = v["big"].as_str().unwrap();
        assert!(big.starts_with("HEAD"), "prefix keeps the value's head");
        assert!(big.ends_with("TAIL"), "postfix keeps the value's tail");
        assert!(big.contains("<elided:"), "the dropped middle is a marker");
        assert_round_trip(&line);
    }

    /// A value that OPENS before the scan threshold and crosses the elide threshold after
    /// the re-feed — `off` must be correct across the fresh-Elider replay (the interaction
    /// most likely to regress under maintenance).
    #[test]
    fn off_is_correct_when_the_value_straddles_the_scan_threshold() {
        let blob = "s".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES);
        let line = format!("{{\"k\":1,\"big\":\"{blob}\"}}\n");
        assert_round_trip(&line);
        let (got, _) = read_one(&line);
        // And the marker's off points at real 's' bytes: splice-check a sample.
        let m = got.find("<elided:").unwrap();
        let end = got[m..].find('>').unwrap() + m;
        let (off, len): (usize, usize) = {
            let body = &got[m + 8..end];
            let (o, l) = body.split_once(',').unwrap();
            (o.parse().unwrap(), l.parse().unwrap())
        };
        assert!(line.as_bytes()[off] == b's' && line.as_bytes()[off + len - 1] == b's');
        assert_eq!(
            line.as_bytes()[off + len],
            b's',
            "postfix continues the value"
        );
    }

    /// Metric-bearing fields must survive untouched next to an elided blob, so an elided
    /// fold and a raw fold agree.
    #[test]
    fn usage_survives_beside_an_elided_attachment() {
        let blob = "A".repeat(ELIDE_STRING_BYTES * 4);
        let line = format!(
            "{{\"type\":\"assistant\",\"requestId\":\"req_1\",\"message\":{{\"id\":\"msg_1\",\
             \"model\":\"claude-opus-4-8\",\"usage\":{{\"input_tokens\":2,\"output_tokens\":1400,\
             \"cache_creation_input_tokens\":945,\"cache_read_input_tokens\":45461}}}},\
             \"toolUseResult\":{{\"file\":{{\"base64\":\"{blob}\"}}}}}}\n"
        );
        let (got, _) = read_one(&line);
        let raw: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        let el: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(raw.pointer("/message/usage"), el.pointer("/message/usage"));
        assert_eq!(raw.pointer("/message/id"), el.pointer("/message/id"));
        assert_eq!(raw.pointer("/message/model"), el.pointer("/message/model"));
        assert_eq!(raw.get("requestId"), el.get("requestId"));
        assert_eq!(raw.get("type"), el.get("type"));
        // The blob is framed: its own head survives, then the marker.
        let b = el
            .pointer("/toolUseResult/file/base64")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(b.starts_with('A') && b.contains("<elided:"));
        assert_round_trip(&line);
    }

    /// A key must never be elided — that would change the document's shape. It must also
    /// not buffer without bound: past the threshold it streams (the seed hardening).
    #[test]
    fn an_oversized_key_is_left_alone() {
        let key = "k".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"{key}\":1,\"pad\":\"{}\"}}\n",
            "z".repeat(SCAN_THRESHOLD)
        );
        let (got, _) = read_one(&line);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert!(v.get(&key).is_some(), "the long key survived intact");
    }

    /// Escapes must not confuse the string scanner into ending early.
    #[test]
    fn escaped_quotes_and_backslashes_survive() {
        let line = "{\"a\":\"he said \\\"hi\\\" and \\\\ left\",\"b\":2}\n";
        let (got, outcome) = read_one(line);
        assert_eq!(got, line);
        assert!(matches!(outcome, LineOutcome::Complete { elided: 0, .. }));
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(v["a"], "he said \"hi\" and \\ left");
    }

    /// A quote inside an elided value must not terminate it early — and the round trip
    /// must hold with escapes adjacent to both frame cuts.
    #[test]
    fn escapes_inside_an_elided_value_do_not_end_it() {
        let blob = format!(
            "\\\\\\u0041{}\\\"{}\\u00e9",
            "p".repeat(ELIDE_STRING_BYTES),
            "q".repeat(1000)
        );
        let line = format!("{{\"big\":\"{blob}\",\"after\":42}}\n");
        let (got, _) = read_one(&line);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(v["after"], 42, "parsing continued past the elided value");
        assert_round_trip(&line);
    }

    /// Frame cuts land on escape-unit boundaries: per-part unescaping of the emitted
    /// prefix/postfix never sees half an escape sequence.
    #[test]
    fn frame_cuts_never_split_an_escape() {
        // Escapes packed around byte 64 of the content and through the tail.
        let head: String = "\\u0041".repeat(30);
        let tail: String = "\\\\".repeat(60);
        let blob = format!("{head}{}{tail}", "r".repeat(ELIDE_STRING_BYTES * 2));
        let line = format!("{{\"big\":\"{blob}\"}}\n");
        let (got, _) = read_one(&line);
        // The emitted value must itself parse: serde rejects a torn escape.
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert!(v["big"].as_str().is_some());
        assert_round_trip(&line);
    }

    #[test]
    fn multibyte_utf8_is_preserved() {
        let line = "{\"a\":\"héllo → 世界 🎉\",\"b\":1}\n";
        let (got, _) = read_one(line);
        assert_eq!(got, line);
    }

    /// Torn lines never emit a marker: a torn value has no closing quote to validate
    /// against and is re-read whole once complete.
    #[test]
    fn a_torn_final_line_is_reported_not_completed_and_carries_no_marker() {
        let (_, outcome) = read_one("{\"a\":1}");
        assert!(matches!(outcome, LineOutcome::Torn { .. }));
        let torn_big = format!("{{\"big\":\"{}", "t".repeat(SCAN_THRESHOLD * 2));
        let (got, outcome) = read_one(&torn_big);
        assert!(matches!(outcome, LineOutcome::Torn { .. }));
        assert!(
            !got.contains("<elided:"),
            "no marker on a torn value: {got}"
        );
    }

    #[test]
    fn empty_input_is_eof() {
        let (_, outcome) = read_one("");
        assert_eq!(outcome, LineOutcome::Eof);
    }

    #[test]
    fn successive_lines_each_report_their_own_raw_length() {
        let a = "{\"a\":1}\n";
        let b = "{\"b\":2}\n";
        let mut r = Cursor::new(format!("{a}{b}").into_bytes());
        let mut out = Vec::new();
        for expect in [a, b] {
            let o = read_line_elided(&mut r, &mut out, 0, Elision::Aggressive).unwrap();
            assert_eq!(String::from_utf8_lossy(&out), expect);
            match o {
                LineOutcome::Complete { raw_len, .. } => assert_eq!(raw_len, expect.len() as u64),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(
            read_line_elided(&mut r, &mut out, 0, Elision::Aggressive).unwrap(),
            LineOutcome::Eof
        );
    }

    /// `line_start` bases the marker offsets: the same line at a nonzero file position
    /// emits offsets shifted by exactly that base.
    #[test]
    fn marker_offsets_are_absolute_via_line_start() {
        let blob = "n".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"pad\":\"{}\",\"big\":\"{blob}\"}}\n",
            "v".repeat(SCAN_THRESHOLD)
        );
        let base = 1_000_000u64;
        let mut r = Cursor::new(line.as_bytes().to_vec());
        let mut out = Vec::new();
        read_line_elided(&mut r, &mut out, base, Elision::Aggressive).unwrap();
        let got = String::from_utf8_lossy(&out);
        let m = got.find("<elided:").unwrap();
        let off: u64 = got[m + 8..].split(',').next().unwrap().parse().unwrap();
        assert!(off >= base, "absolute offset carries the line base");
        // Splicing against a virtual file placed at `base` reconstructs the line.
        let mut file = vec![0u8; base as usize];
        file.extend_from_slice(line.as_bytes());
        assert_eq!(
            String::from_utf8_lossy(&unelide(out.as_slice(), &file)),
            line
        );
    }

    /// Arrays hold values, so a big string directly inside one is elidable.
    #[test]
    fn a_big_string_in_an_array_is_elided() {
        let blob = "m".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"pad\":\"{}\",\"xs\":[\"{blob}\"]}}\n",
            "w".repeat(SCAN_THRESHOLD)
        );
        let (got, _) = read_one(&line);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert!(v["xs"][0].as_str().unwrap().contains("<elided:"));
        assert_round_trip(&line);
    }

    /// Content that LOOKS like a marker is content: a small value containing literal
    /// marker text passes through verbatim, no matter the policy.
    #[test]
    fn literal_marker_text_in_a_small_value_is_untouched() {
        let line = format!(
            "{{\"pad\":\"{}\",\"prose\":\"the doc quotes <elided:0,999999999> here\"}}\n",
            "q".repeat(SCAN_THRESHOLD)
        );
        let (got, _) = read_one(&line);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(
            v["prose"].as_str().unwrap(),
            "the doc quotes <elided:0,999999999> here"
        );
    }

    // ── the α-lite policy ────────────────────────────────────────────────────────

    const CLAUDE_ISH: Elision = Elision::Keys(&[&["file", "base64"], &["source", "data"]]);

    /// The sans-io invariant, α-side: a listed attachment body elides; an unlisted giant
    /// value — rendered content — passes through INTACT (streamed, not buffered).
    #[test]
    fn keys_policy_elides_listed_nodes_and_streams_the_rest() {
        let blob = "B".repeat(ELIDE_STRING_BYTES * 2);
        let text = "T".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"toolUseResult\":{{\"file\":{{\"base64\":\"{blob}\"}}}},\"message\":{{\"text\":\"{text}\"}}}}\n"
        );
        let (got, _) = read_one_with(&line, CLAUDE_ISH);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        let b = v
            .pointer("/toolUseResult/file/base64")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(b.contains("<elided:"), "listed node elides");
        let t = v.pointer("/message/text").unwrap().as_str().unwrap();
        assert_eq!(t, text, "unlisted giant text survives byte for byte");
        assert_round_trip(&line);
    }

    /// The tool_result twin rides inside arrays: `content[].content[].source.data` matches
    /// the ["source","data"] suffix — array levels contribute no key and break nothing.
    #[test]
    fn keys_policy_matches_through_array_levels() {
        let blob = "C".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES);
        let line = format!(
            "{{\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"content\":[{{\"type\":\"image\",\
             \"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{blob}\"}}}}]}}]}}}}\n"
        );
        let (got, _) = read_one_with(&line, CLAUDE_ISH);
        let d = serde_json::from_str::<serde_json::Value>(got.trim_end())
            .unwrap()
            .pointer("/message/content/0/content/0/source/data")
            .cloned()
            .unwrap();
        assert!(d.as_str().unwrap().contains("<elided:"));
        assert_round_trip(&line);
    }

    /// A one-key suffix must not over-match a DIFFERENT parent: ["file","base64"] does not
    /// elide `other.base64`… but a listed single-key pattern would. Pin both directions.
    #[test]
    fn keys_policy_suffix_is_exact_over_the_named_parents() {
        let blob = "D".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES);
        let line = format!("{{\"other\":{{\"base64\":\"{blob}\"}}}}\n");
        let (got, _) = read_one_with(&line, CLAUDE_ISH);
        let v: serde_json::Value = serde_json::from_str(got.trim_end()).unwrap();
        assert_eq!(
            v.pointer("/other/base64").unwrap().as_str().unwrap(),
            blob,
            "wrong parent: the two-key suffix must not match"
        );
    }

    /// `Elision::None`: nothing elides, ever — the identity-read policy. Output is the
    /// input, whatever its size (ceiling aside).
    #[test]
    fn none_policy_is_verbatim() {
        let blob = "E".repeat(ELIDE_STRING_BYTES * 2);
        let line = format!(
            "{{\"pad\":\"{}\",\"file\":{{\"base64\":\"{blob}\"}}}}\n",
            "u".repeat(SCAN_THRESHOLD)
        );
        let (got, outcome) = read_one_with(&line, Elision::None);
        assert_eq!(got, line);
        assert!(matches!(outcome, LineOutcome::Complete { elided: 0, .. }));
    }

    /// Nesting beyond the tracked depth fails SAFE: the listed suffix sits too deep to be
    /// known, so the value stays (boundedness degrades to the ceiling, correctness holds).
    #[test]
    fn keys_policy_depth_overflow_never_matches() {
        let blob = "F".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES);
        let mut open = String::new();
        let mut close = String::new();
        for i in 0..12 {
            open.push_str(&format!("{{\"w{i}\":"));
            close.push('}');
        }
        let line = format!("{open}{{\"file\":{{\"base64\":\"{blob}\"}}}}{close}\n");
        let (got, _) = read_one_with(&line, CLAUDE_ISH);
        assert!(
            !got.contains("<elided:"),
            "too deep to know where we are → keep the bytes"
        );
    }

    /// The ceiling is the last-resort bound: a line whose NON-elidable output exceeds it
    /// (here: a policy-protected giant under `None`) is consumed to its newline, `out` is
    /// cleared, and the outcome says skipped — counted, never buffered, never silent.
    #[test]
    fn past_the_ceiling_the_line_is_skipped_and_counted() {
        let blob = "H".repeat(ELIDE_CEILING + ELIDE_STRING_BYTES);
        let line = format!("{{\"big\":\"{blob}\"}}\n");
        let mut r = Cursor::new(line.as_bytes().to_vec());
        let mut out = Vec::new();
        let outcome = read_line_elided(&mut r, &mut out, 0, Elision::None).unwrap();
        match outcome {
            LineOutcome::Complete {
                raw_len, skipped, ..
            } => {
                assert_eq!(raw_len, line.len() as u64, "consumed to the newline");
                assert!(skipped);
                assert!(out.is_empty(), "nothing usable is kept");
            }
            other => panic!("unexpected {other:?}"),
        }
        // The reader is positioned after the newline: a following line reads normally.
    }

    /// A 10 MB base64 field — the design's headline case — elides to a small line and
    /// round-trips exactly.
    #[test]
    fn a_ten_megabyte_field_elides_and_round_trips() {
        let blob = "G".repeat(10 * 1024 * 1024);
        let line = format!("{{\"toolUseResult\":{{\"file\":{{\"base64\":\"{blob}\"}}}}}}\n");
        let (got, outcome) = read_one(&line);
        assert!(
            got.len() < 1024,
            "10 MB in, a few hundred bytes out: {}",
            got.len()
        );
        match outcome {
            LineOutcome::Complete {
                raw_len,
                elided,
                skipped,
            } => {
                assert_eq!(raw_len, line.len() as u64);
                assert!(elided > 10 * 1024 * 1024 - (PREFIX_KEEP + POSTFIX_KEEP + 8) as u64);
                assert!(!skipped);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_round_trip(&line);
    }
}

/// A marker parsed back out of an elided value by DECODE (sans-io): the span and the kept
/// frame, destined for the locator hint. The three recognition tests (§4 of the design)
/// all run here, with no IO: visible length under the threshold, exactly one marker, and a
/// plausible `len` — a genuine dropped middle is necessarily larger than
/// `ELIDE_STRING_BYTES − PREFIX_KEEP − POSTFIX_KEEP`, so an innocent literal marker in
/// prose (quoting the design doc, say) is dismissed without ever becoming a hint.
#[derive(Debug, Clone, PartialEq)]
pub struct MarkerSpan {
    /// Absolute file offset of the first dropped byte.
    pub off: u64,
    /// Dropped byte count.
    pub len: u64,
    /// The kept head of the value (unescaped, as decode received it).
    pub prefix: String,
    /// The kept tail of the value (unescaped).
    pub postfix: String,
}

/// Recognize an elided value and parse its marker. `None` = an ordinary value (content
/// that merely looks like a marker is content).
pub fn parse_marker(value: &str) -> Option<MarkerSpan> {
    if value.len() >= ELIDE_STRING_BYTES {
        return None; // a real elided value is ~K + marker + J; an oversized one is content
    }
    let start = value.find("<elided:")?;
    let rest = &value[start + 8..];
    let end = rest.find('>')?;
    let body = &rest[..end];
    let (off, len) = body.split_once(',')?;
    let (off, len): (u64, u64) = (off.parse().ok()?, len.parse().ok()?);
    let after = &rest[end + 1..];
    if after.contains("<elided:") {
        return None; // exactly one marker — several is prose about markers
    }
    if len <= (ELIDE_STRING_BYTES - PREFIX_KEEP - POSTFIX_KEEP) as u64 || len > ELIDE_CEILING as u64
    {
        return None; // implausible dropped middle — dismissed sans-io
    }
    Some(MarkerSpan {
        off,
        len,
        prefix: value[..start].to_string(),
        postfix: after.to_string(),
    })
}

#[cfg(test)]
mod marker_tests {
    use super::*;

    #[test]
    fn a_genuine_marker_parses_with_its_frame() {
        let v = format!(
            "data:image/png;base64,iVBOR<elided:52981042,{}>SuQmCC",
            ELIDE_STRING_BYTES * 2
        );
        let m = parse_marker(&v).unwrap();
        assert_eq!(m.off, 52_981_042);
        assert_eq!(m.len, (ELIDE_STRING_BYTES * 2) as u64);
        assert_eq!(m.prefix, "data:image/png;base64,iVBOR");
        assert_eq!(m.postfix, "SuQmCC");
    }

    #[test]
    fn innocent_literal_markers_are_dismissed_sans_io() {
        // Small numbers: implausible dropped middle.
        assert_eq!(parse_marker("the doc quotes <elided:0,999> here"), None);
        // Giant claimed len: past the ceiling.
        assert_eq!(
            parse_marker(&format!("x<elided:0,{}>y", ELIDE_CEILING as u64 + 1)),
            None
        );
        // Two markers: prose about markers.
        let n = (ELIDE_STRING_BYTES * 2) as u64;
        assert_eq!(
            parse_marker(&format!("a<elided:0,{n}>b<elided:9,{n}>c")),
            None
        );
        // No marker at all.
        assert_eq!(parse_marker("just text"), None);
        // An oversized VALUE that happens to end marker-shaped is content.
        let big = format!(
            "{}{}",
            "z".repeat(ELIDE_STRING_BYTES + 1),
            "<elided:1,999999>"
        );
        assert_eq!(parse_marker(&big), None);
    }

    /// The scanner's own emissions always parse back — the two halves agree.
    #[test]
    fn scanner_emissions_round_trip_through_parse_marker() {
        use std::io::Cursor;
        let blob = format!(
            "HEAD{}TAIL",
            "m".repeat(SCAN_THRESHOLD + ELIDE_STRING_BYTES)
        );
        let line = format!("{{\"big\":\"{blob}\"}}\n");
        let mut out = Vec::new();
        read_line_elided(
            &mut Cursor::new(line.as_bytes().to_vec()),
            &mut out,
            7_000,
            Elision::Aggressive,
        )
        .unwrap();
        let v: serde_json::Value =
            serde_json::from_str(std::str::from_utf8(&out).unwrap().trim_end()).unwrap();
        let m = parse_marker(v["big"].as_str().unwrap()).unwrap();
        assert!(m.off >= 7_000, "absolute, based on the line start");
        assert!(m.prefix.starts_with("HEAD"));
        assert!(m.postfix.ends_with("TAIL"));
        // And the frame + span cover the whole value.
        assert_eq!(
            m.prefix.len() as u64 + m.len + m.postfix.len() as u64,
            blob.len() as u64
        );
    }
}

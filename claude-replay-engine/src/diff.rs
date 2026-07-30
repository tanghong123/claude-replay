//! Agent-neutral Edit-diff row model: the classifier + line-numberer shared by every
//! frontend (the TUI renderer and the HTML exporter) so their gutter numbering can never
//! drift. Pure data — no ratatui/theme — over `crate::model`.

/// One step of a line-level diff.
pub enum LineOp<'a> {
    Eq(&'a str),
    Del(&'a str),
    Ins(&'a str),
}

/// Line-level LCS → an ordered op sequence (unchanged lines stay as context,
/// only genuinely changed runs become -/+). Avoids the old index-zip that
/// mispaired every line after an insertion/deletion.
pub fn line_diff<'a>(ol: &[&'a str], nl: &[&'a str]) -> Vec<LineOp<'a>> {
    let (n, m) = (ol.len(), nl.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if ol[i] == nl[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if ol[i] == nl[j] {
            ops.push(LineOp::Eq(ol[i]));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(LineOp::Del(ol[i]));
            i += 1;
        } else {
            ops.push(LineOp::Ins(nl[j]));
            j += 1;
        }
    }
    while i < n {
        ops.push(LineOp::Del(ol[i]));
        i += 1;
    }
    while j < m {
        ops.push(LineOp::Ins(nl[j]));
        j += 1;
    }
    ops
}

/// One classified row of an Edit diff: its kind, the gutter line number to show (`None` =
/// blank gutter — a deletion in the local-numbering fallback, which has no new-side position),
/// and the row text.
pub enum DiffKind {
    Ctx,
    Add,
    Del,
}
pub struct DiffRow {
    pub kind: DiffKind,
    pub num: Option<usize>,
    pub text: String,
}
/// A gutter-alignment group of diff rows: one `structuredPatch` hunk, or one `(old, new)`
/// pair in the local-numbering fallback. `max_line` is the largest line number either side's
/// numbering reaches — including a trailing-context position counted on the *old* side even
/// though that row shows its new number — so the TUI sizes its gutter exactly as it always has.
pub struct DiffGroup {
    pub rows: Vec<DiffRow>,
    pub max_line: usize,
}

/// **The single Edit-diff classifier + line-numberer**, shared by the TUI (`tui::render`'s
/// `render_diff`) and the HTML exporter (`html_export::diff_part`) so their numbering can never
/// drift. Rows are grouped for gutter alignment: one group per `structuredPatch` hunk (real file
/// line numbers on both sides), or — when the transcript carried no patch — one group per
/// `(old, new)` diff pair with a local 1..N numbering over the new side (via [`line_diff`]).
/// Context/added rows carry a new-side number; a deletion carries its OLD-side number in the
/// patch branch and `None` in the fallback. Empty diff pairs are dropped.
pub fn diff_row_groups(
    diffs: &[(String, String)],
    patch: Option<&[crate::model::Hunk]>,
) -> Vec<DiffGroup> {
    let mut groups = Vec::new();
    if let Some(hunks) = patch {
        for h in hunks {
            // Gutter extent counts BOTH sides fully (added/context on the new side, removed/
            // context on the old side) — a trailing-context run advances the old side past any
            // old number actually shown, so size to `max(new_last, old_last)`.
            let new_lines = h.lines.iter().filter(|l| !l.starts_with('-')).count();
            let old_lines = h.lines.iter().filter(|l| !l.starts_with('+')).count();
            let max_line = (h.new_start + new_lines.saturating_sub(1))
                .max(h.old_start + old_lines.saturating_sub(1));
            let mut rows = Vec::new();
            let (mut n, mut o) = (h.new_start, h.old_start);
            for line in &h.lines {
                let marker = line.chars().next().unwrap_or(' ');
                let text = line.get(marker.len_utf8()..).unwrap_or("").to_string();
                match marker {
                    '+' => {
                        rows.push(DiffRow {
                            kind: DiffKind::Add,
                            num: Some(n),
                            text,
                        });
                        n += 1;
                    }
                    '-' => {
                        rows.push(DiffRow {
                            kind: DiffKind::Del,
                            num: Some(o),
                            text,
                        });
                        o += 1;
                    }
                    _ => {
                        rows.push(DiffRow {
                            kind: DiffKind::Ctx,
                            num: Some(n),
                            text,
                        });
                        n += 1;
                        o += 1;
                    }
                }
            }
            groups.push(DiffGroup { rows, max_line });
        }
    } else {
        for (old, new) in diffs
            .iter()
            .filter(|(o, n)| !(o.is_empty() && n.is_empty()))
        {
            let ol: Vec<&str> = old.lines().collect();
            let nl: Vec<&str> = new.lines().collect();
            let ops = line_diff(&ol, &nl);
            // Local numbering over the NEW side only (deletions get a blank gutter); the
            // gutter sizes to the count of new-side lines (context + insertions).
            let max_line = ops
                .iter()
                .filter(|op| matches!(op, LineOp::Eq(_) | LineOp::Ins(_)))
                .count();
            let mut rows = Vec::new();
            let mut n = 0usize;
            for op in ops {
                match op {
                    LineOp::Eq(l) => {
                        n += 1;
                        rows.push(DiffRow {
                            kind: DiffKind::Ctx,
                            num: Some(n),
                            text: l.to_string(),
                        });
                    }
                    LineOp::Del(l) => {
                        rows.push(DiffRow {
                            kind: DiffKind::Del,
                            num: None,
                            text: l.to_string(),
                        });
                    }
                    LineOp::Ins(l) => {
                        n += 1;
                        rows.push(DiffRow {
                            kind: DiffKind::Add,
                            num: Some(n),
                            text: l.to_string(),
                        });
                    }
                }
            }
            groups.push(DiffGroup { rows, max_line });
        }
    }
    groups
}

/// Decode standard base64 (RFC 4648), tolerating `=` padding and embedded
/// whitespace/newlines (transcript base64 is often line-wrapped). Returns `None` on
/// an invalid character. Used to write embedded image attachments to disk.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
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
    use super::base64_decode;

    /// Standard base64 encoder — test-only helper so the decoder roundtrip stays
    /// self-contained (the production encoder lives in the viewer's `clipboard`).
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

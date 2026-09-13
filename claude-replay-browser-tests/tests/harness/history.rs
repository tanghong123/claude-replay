//! The sandbox (#197 stage B, design/viewport-history.md §5): an export of the viewport history —
//! kinds, heights, indices, turns and timings, never content — rebuilt as a synthetic transcript
//! with the same shape, grown by the same delta timeline, and replayed with the recorded actions
//! on either surface, so a report from a real session is a reproduction, and a short one.
//!
//! Nothing here reads a real session: the profile comes from the export, the text from a fixed
//! sentence sized by a calibration the case measures on the surface it replays on.

use super::{
    command_at, compaction_at, edit_tool_at, now_minus, queued_at, read_tool_at, thinking_at,
    tool_open_at, tool_result_lines, user_at, write_tool_at, Surface,
};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Duration;

/// One record of the session's shape: its kind in the page's vocabulary, its measured height
/// (null where it was never mounted) and its turn.
#[derive(Debug, Clone)]
pub struct Rec {
    pub kind: String,
    pub height: Option<f64>,
    pub turn: Option<i64>,
}

/// The export, loaded. `session.items` is one row per ENGINE index; `profile` flattens it to
/// records: on the classic page the items are the records; on the shell a process unit spans
/// records, listed in `session.records`, and its height is spread over them.
/// One row of an export's `session.items`: the kind, the measured height (`None` when the record
/// was never mounted), the turn, and the record range `from..=to` the row spans (a shell unit
/// spans several records; a classic record spans itself).
pub type Item = (Option<String>, Option<f64>, Option<i64>, i64, i64);

pub struct Export {
    pub page: String,
    pub items: Vec<Item>,
    pub records: Option<Vec<Option<String>>>,
    pub actions: Vec<Value>,
    pub states: Vec<Value>,
    pub deltas: Vec<Value>,
    pub raw: Value,
}

impl Export {
    pub fn load(path: &std::path::Path) -> Export {
        let raw: Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("read the export"))
                .expect("the export parses");
        Export::from_value(raw)
    }

    pub fn from_value(raw: Value) -> Export {
        assert_eq!(raw["format"], "viewport-history/1", "the export's format");
        let items = raw["session"]["items"]
            .as_array()
            .expect("session.items")
            .iter()
            .map(|row| {
                let r = row.as_array().expect("an item row");
                (
                    r[0].as_str().map(str::to_string),
                    r[1].as_f64(),
                    r[2].as_i64(),
                    r[3].as_i64().unwrap_or(0),
                    r[4].as_i64().unwrap_or(0),
                )
            })
            .collect();
        let records = raw["session"]["records"]
            .as_array()
            .map(|r| r.iter().map(|k| k.as_str().map(str::to_string)).collect());
        let list = |key: &str| raw[key].as_array().cloned().unwrap_or_default();
        Export {
            page: raw["page"].as_str().unwrap_or("").to_string(),
            items,
            records,
            actions: list("actions"),
            states: list("states"),
            deltas: list("deltas"),
            raw,
        }
    }

    /// The record-level shape.
    pub fn profile(&self) -> Vec<Rec> {
        let mut out = Vec::new();
        for (i, (kind, height, turn, from, to)) in self.items.iter().enumerate() {
            let n = (to - from + 1).max(1) as usize;
            match &self.records {
                Some(records) if n > 1 || kind.as_deref() == Some("process") => {
                    for j in 0..n {
                        let k = records
                            .get(*from as usize + j)
                            .cloned()
                            .flatten()
                            .unwrap_or_else(|| "act".to_string());
                        out.push(Rec {
                            kind: k,
                            height: height.map(|h| h / n as f64),
                            turn: *turn,
                        });
                    }
                }
                _ => out.push(Rec {
                    kind: match kind.as_deref() {
                        Some("process") => "act".to_string(),
                        Some(k) => k.to_string(),
                        None => "assistant".to_string(),
                    },
                    height: *height,
                    turn: *turn,
                }),
            }
            let _ = i;
        }
        out
    }

    /// The engine index the open ended at (the first delta that brought records in), in
    /// RECORD terms: everything before it is the initial transcript, the rest is growth.
    pub fn opened_records(&self) -> usize {
        let count1 = self
            .deltas
            .iter()
            .find(|d| d["count1"].as_i64().unwrap_or(0) > 0)
            .and_then(|d| d["count1"].as_i64())
            .unwrap_or(self.items.len() as i64) as usize;
        self.record_index(count1)
    }

    /// Engine index → record index (the first record of that item; the count past the end).
    pub fn record_index(&self, engine_index: usize) -> usize {
        if let Some(item) = self.items.get(engine_index) {
            return item.3 as usize;
        }
        self.items.last().map_or(0, |last| last.4 as usize + 1)
    }

    /// The turn of engine index `i`, for a replayed jump.
    pub fn turn_of(&self, engine_index: usize) -> Option<i64> {
        self.items.get(engine_index).and_then(|item| item.2)
    }
}

/// Per kind of prose, height ≈ `base` + `per_char` × chars, measured on the surface (§5's
/// calibration): the model the synthetic text is sized by.
#[derive(Debug, Clone)]
pub struct Calib {
    pub user: (f64, f64),
    pub assistant: (f64, f64),
    /// The folded heights of the non-prose kinds, as measured.
    pub think: f64,
    pub act: f64,
    /// A pasted image's card: pixels per source row once the page has scaled the image to its
    /// column, and what the card adds around the image.
    pub image_scale: f64,
    pub image_chrome: f64,
}

impl Calib {
    /// Source rows for a card of `height` pixels.
    pub fn rows_for(&self, height: f64) -> usize {
        if self.image_scale <= 0.0 {
            return 1;
        }
        (((height - self.image_chrome) / self.image_scale).max(1.0)).round() as usize
    }

    fn chars_for(&self, model: (f64, f64), height: f64) -> usize {
        let (base, per_char) = model;
        if per_char <= 0.0 {
            return 40;
        }
        (((height - base) / per_char).max(20.0)).round() as usize
    }
}

/// The calibration session: prose of known lengths, one turn per length, both kinds.
pub const CALIB_CHARS: [usize; 5] = [40, 200, 600, 1400, 3000];
/// The calibration images: 32 source pixels wide (the classic page scales a pasted image to
/// its column, so a source ROW is worth `Calib::image_scale` pixels on the page), 10 and 30 rows
/// tall, on the two longest turns (which the user fit does not use).
pub const CALIB_IMAGE_W: usize = 32;
pub const CALIB_IMAGE_ROWS: [usize; 2] = [10, 30];
/// The classic page clamps a long prompt (`clampBatch`), so the user model is fitted on the
/// lengths below it.
pub const USER_FIT_MAX_CHARS: usize = 600;

pub fn calibration_session() -> String {
    let mut out = String::new();
    for (i, n) in CALIB_CHARS.iter().enumerate() {
        out += &user_at(&prose(*n, i as u64), &now_minus(600 - i as u64 * 10));
        if i >= 3 {
            out += &image_only_at(CALIB_IMAGE_ROWS[i - 3], &now_minus(599 - i as u64 * 10));
        }
        out += &thinking_at(&prose(120, 7), &now_minus(598 - i as u64 * 10));
        out += &tool_open_at(&format!("c{i}"), &now_minus(597 - i as u64 * 10));
        out += &tool_result_lines(&format!("c{i}"), 6, &now_minus(596 - i as u64 * 10));
        out += &assistant_phased_at(
            &prose(*n, i as u64 + 3),
            &now_minus(595 - i as u64 * 10),
            true,
        );
    }
    out
}

/// Fit the model from the calibration session's measured item heights (the export's rows), in
/// the order the session was written: user, think, act, assistant per turn.
pub fn fit(items: &[Item], page: &str) -> Calib {
    let mut user = Vec::new();
    let mut assistant = Vec::new();
    let mut think = Vec::new();
    let mut act = Vec::new();
    let mut attachment = Vec::new();
    let mut user_units: Vec<(usize, f64)> = Vec::new();
    let mut turn = 0usize;
    for (kind, height, _, from, to) in items {
        let Some(h) = height else { continue };
        match (kind.as_deref(), page) {
            (Some("user"), _) => {
                if let Some(n) = CALIB_CHARS.get(turn) {
                    if *n <= USER_FIT_MAX_CHARS {
                        user.push((*n as f64, *h));
                    }
                }
                user_units.push((turn, *h));
            }
            (Some("attachment"), _) => attachment.push(*h),
            (Some("assistant"), _) => {
                if let Some(n) = CALIB_CHARS.get(turn) {
                    assistant.push((*n as f64, *h));
                }
                turn += 1;
            }
            (Some("think"), _) => think.push(*h),
            (Some("act"), _) => act.push(*h),
            // The shell's process unit spans the think and the act: split evenly.
            (Some("process"), _) => {
                let n = (to - from + 1).max(1) as f64;
                think.push(*h / n);
                act.push(*h / n);
            }
            _ => {}
        }
    }
    let line = |pts: &[(f64, f64)]| -> (f64, f64) {
        if pts.len() < 2 {
            return (0.0, 0.0);
        }
        let n = pts.len() as f64;
        let (sx, sy) = pts.iter().fold((0.0, 0.0), |(a, b), (x, y)| (a + x, b + y));
        let (mx, my) = (sx / n, sy / n);
        let sxx: f64 = pts.iter().map(|(x, _)| (x - mx) * (x - mx)).sum();
        let sxy: f64 = pts.iter().map(|(x, y)| (x - mx) * (y - my)).sum();
        let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
        (my - slope * mx, slope)
    };
    let mean = |v: &[f64]| {
        if v.is_empty() {
            40.0
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let user_model = line(&user);
    // The image model: on the classic page the two cards are records of their own; on the shell
    // they ride inside the user unit, so their share is the unit's height past the prose model.
    let (rows0, rows1) = (CALIB_IMAGE_ROWS[0] as f64, CALIB_IMAGE_ROWS[1] as f64);
    let cards: Option<(f64, f64)> = if attachment.len() >= 2 {
        Some((attachment[0], attachment[1]))
    } else {
        let unit = |t: usize| {
            user_units
                .iter()
                .find(|(turn, _)| *turn == t)
                .map(|(_, h)| *h)
        };
        match (unit(3), unit(4)) {
            (Some(u3), Some(u4)) => Some((
                u3 - (user_model.0 + user_model.1 * CALIB_CHARS[3] as f64),
                u4 - (user_model.0 + user_model.1 * CALIB_CHARS[4] as f64),
            )),
            _ => None,
        }
    };
    let (image_scale, image_chrome) = match cards {
        Some((h0, h1)) if h1 > h0 => {
            let scale = (h1 - h0) / (rows1 - rows0);
            (scale, (h0 - rows0 * scale).max(0.0))
        }
        _ => (0.0, 0.0),
    };
    Calib {
        user: user_model,
        assistant: line(&assistant),
        think: mean(&think),
        act: mean(&act),
        image_scale,
        image_chrome,
    }
}

/// An assistant text with the phase the engine reads off `stop_reason` (the claude adapter's
/// `assistant_phase`): `end_turn` closes the turn — the shell's own assistant unit — and
/// `tool_use` is commentary, which the shell files among the process rows.
pub fn assistant_phased_at(t: &str, ts: &str, final_answer: bool) -> String {
    let stop = if final_answer { "end_turn" } else { "tool_use" };
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"text\",\"text\":\"{t}\"}}],\"stop_reason\":\"{stop}\",\"usage\":{{\"input_tokens\":10,\"output_tokens\":5}}}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A tool call by name with its result — the NAME decides the record kind the engine shapes
/// (measured with `--dump-html` on 2026-09-13): `Bash`, `Read`, `Glob` and thinking coalesce into
/// one `act` run; `Edit` → edit, `Write`/`NotebookEdit` → write, `Skill` → skill, `Task`/`Agent`
/// → agent; `WebFetch`, `WebSearch`, `TodoWrite`, `ToolSearch` stand alone as `tool`.
pub fn call_at(id: &str, name: &str, input: &str, ts: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"{id}\",\"name\":\"{name}\",\"input\":{input}}}]}},\"timestamp\":\"{ts}\"}}\n{{\"type\":\"user\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"tool_use_id\":\"{id}\",\"content\":\"line 1\\nline 2\\nline 3\\n\"}}]}},\"timestamp\":\"{ts}\"}}\n"
    )
}

/// A pasted image as its own record — a user message whose only block is the image, which the
/// adapter files as an attachment of the prompt before it (no turn) — `rows` source pixels tall
/// and `CALIB_IMAGE_W` wide; the page scales it to its column, so the card's height is
/// `Calib::image_chrome + rows × Calib::image_scale`. The PNG is written here (stored deflate
/// blocks, grayscale) so the harness needs no image crate.
pub fn image_only_at(rows: usize, ts: &str) -> String {
    let png = tall_png(CALIB_IMAGE_W, rows.clamp(1, 4000));
    format!(
        "{{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"image\",\"source\":{{\"type\":\"base64\",\"media_type\":\"image/png\",\"data\":\"{}\"}}}}]}},\"timestamp\":\"{ts}\"}}\n",
        base64(&png)
    )
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A `width`×`height` grayscale PNG, mid-grey, zlib with stored (uncompressed) deflate blocks.
pub fn tall_png(width: usize, height: usize) -> Vec<u8> {
    let mut raw = Vec::with_capacity((width + 1) * height);
    for _ in 0..height {
        raw.push(0); // filter: none
        raw.extend(std::iter::repeat_n(0x99u8, width));
    }
    let mut z = vec![0x78, 0x01];
    let mut adler = (1u32, 0u32);
    for &b in &raw {
        adler.0 = (adler.0 + b as u32) % 65521;
        adler.1 = (adler.1 + adler.0) % 65521;
    }
    let blocks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, block) in blocks.iter().enumerate() {
        z.push(if i + 1 == blocks.len() { 1 } else { 0 });
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&((adler.1 << 16) | adler.0).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // 8-bit grayscale
    png_chunk(&mut out, b"IHDR", &ihdr);
    png_chunk(&mut out, b"IDAT", &z);
    png_chunk(&mut out, b"IEND", &[]);
    out
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.len();
        let v = (chunk[0] as u32) << 16
            | (if n > 1 { chunk[1] as u32 } else { 0 }) << 8
            | (if n > 2 { chunk[2] as u32 } else { 0 });
        out.push(T[(v >> 18) as usize & 63] as char);
        out.push(T[(v >> 12) as usize & 63] as char);
        out.push(if n > 1 {
            T[(v >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if n > 2 {
            T[v as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Prose of about `chars` characters, from one sentence, no newline (a raw newline drops the
/// record); `seed` varies the start so two records never read identical.
pub fn prose(chars: usize, seed: u64) -> String {
    const WORDS: [&str; 12] = [
        "the", "reader", "scrolled", "past", "a", "long", "answer", "while", "growth", "arrived",
        "below", "quietly",
    ];
    let mut out = String::new();
    let mut i = seed as usize;
    while out.len() < chars {
        if !out.is_empty() {
            out.push(' ');
        }
        out += WORDS[i % WORDS.len()];
        i += 1;
    }
    out
}

/// The mean measured height per kind: what an unmeasured record of that kind is given (the
/// export measures only what was mounted — 226 of 8 954 records on the walk — and the kind's
/// own mean keeps the rest of the page at a plausible size).
pub fn means(profile: &[Rec]) -> HashMap<String, f64> {
    let mut sums: HashMap<String, (f64, f64)> = HashMap::new();
    for rec in profile {
        if let Some(h) = rec.height {
            let e = sums.entry(rec.kind.clone()).or_insert((0.0, 0.0));
            e.0 += h;
            e.1 += 1.0;
        }
    }
    sums.into_iter().map(|(k, (s, n))| (k, s / n)).collect()
}

/// One record of the synthetic transcript for a profile record: the prose kinds sized by the
/// model to the recorded (or the kind's mean) height, the folded kinds by the builders whose
/// NAMES the engine shapes into the same record kinds (`call_at`), and an assistant with the
/// phase its kind carries (`commentary` is `stop_reason: tool_use`; `assistant` closes the turn
/// — the recording's own phase, not a guess from position: a turn can end and be continued).
pub fn synthetic_record(
    rec: &Rec,
    calib: &Calib,
    means: &HashMap<String, f64>,
    index: usize,
    ts: &str,
) -> String {
    let seed = index as u64;
    let height = |fallback: f64| {
        rec.height
            .or_else(|| means.get(&rec.kind).copied())
            .unwrap_or(fallback)
    };
    let id = format!("s{index}");
    match rec.kind.as_str() {
        "user" => user_at(
            &prose(calib.chars_for(calib.user, height(calib.user.0)), seed),
            ts,
        ),
        "assistant" | "commentary" | "plan" => assistant_phased_at(
            &prose(
                calib.chars_for(calib.assistant, height(calib.assistant.0)),
                seed,
            ),
            ts,
            rec.kind != "commentary",
        ),
        "think" => thinking_at(&prose(160, seed), ts),
        "queue" => queued_at(&prose(40, seed), ts),
        "command" => command_at("/context", "", "", ts),
        "compaction" => compaction_at(ts),
        "attachment" | "image" => image_only_at(
            calib.rows_for(height(calib.image_chrome + calib.image_scale)),
            ts,
        ),
        "edit" => edit_tool_at(&id, "/r/src/lib.rs", ts),
        "write" => write_tool_at(&id, "/r/src/new.rs", 8, ts),
        "read" => read_tool_at(&id, "/r/src/lib.rs", ts),
        "tool" => call_at(
            &id,
            "WebFetch",
            "{\"url\":\"https://example.invalid/page\"}",
            ts,
        ),
        "skill" => call_at(&id, "Skill", "{\"skill\":\"deploy\"}", ts),
        "agent" => call_at(
            &id,
            "Task",
            "{\"prompt\":\"look\",\"subagent_type\":\"general-purpose\"}",
            ts,
        ),
        // `act`: a Bash call with its result — folded by default, so its height is the head's.
        _ => format!("{}{}", tool_open_at(&id, ts), tool_result_lines(&id, 4, ts)),
    }
}

/// The synthetic transcript for records `[0, upto)` of the profile.
pub fn synthetic(profile: &[Rec], upto: usize, calib: &Calib) -> String {
    let means = means(profile);
    let mut out = String::new();
    let total = profile.len().max(1) as u64;
    for (i, rec) in profile.iter().take(upto).enumerate() {
        let ts = now_minus(3600 + total - i as u64);
        out += &synthetic_record(rec, calib, &means, i, &ts);
    }
    out
}

/// The growth after the open: per recorded delta that grew the count, the records it brought
/// in (the profile's final kinds at those indices — a rewrite is approximated by an append) and
/// the recorded gap since the previous delta.
pub fn growth(export: &Export, profile: &[Rec], calib: &Calib) -> Vec<(Duration, Vec<String>)> {
    let mut out = Vec::new();
    let mut seen_open = false;
    let mut last_t = 0.0;
    let mut cursor = export.opened_records();
    let means = means(profile);
    for delta in &export.deltas {
        let (count0, count1) = (
            delta["count0"].as_i64().unwrap_or(0),
            delta["count1"].as_i64().unwrap_or(0),
        );
        let t = delta["t"].as_f64().unwrap_or(0.0);
        if !seen_open {
            if count1 > 0 {
                seen_open = true;
                last_t = t;
            }
            continue;
        }
        if count1 <= count0 {
            last_t = t;
            continue;
        }
        let to = export.record_index(count1 as usize).max(cursor);
        let records: Vec<String> = (cursor..to)
            .filter_map(|i| {
                profile
                    .get(i)
                    .map(|rec| synthetic_record(rec, calib, &means, i, &now_minus(1)))
            })
            .collect();
        cursor = to;
        out.push((
            Duration::from_millis(((t - last_t).max(200.0)) as u64),
            records,
        ));
        last_t = t;
    }
    out
}

/// A recorded action, reduced to what the replay does with it.
#[derive(Debug, Clone)]
pub enum Step {
    Wheel(i64),
    Key(String),
    JumpTo(i64),
    End,
    Fold,
    Skip(String),
}

/// The offset the engine believed just before `t` and the first it recorded at or after
/// `until`: what a wheel gesture actually moved, which is not its `dy` (the sum of the deltas
/// the frame heard — the harness's `scroll_by` dispatches its wheel twice on a retry, and a
/// browser scrolls a real wheel by its own curve).
pub fn moved_between(states: &[Value], t: f64, until: f64) -> Option<i64> {
    let before = states
        .iter()
        .rev()
        .find(|s| s["t"].as_f64().unwrap_or(0.0) < t)
        .and_then(|s| s["top"].as_f64())?;
    let after = states
        .iter()
        .find(|s| s["t"].as_f64().unwrap_or(0.0) >= until)
        .and_then(|s| s["top"].as_f64())?;
    Some((after - before).round() as i64)
}

/// The recorded actions as steps with the recorded gap before each (capped so a replay of an
/// hour stays minutes).
pub fn steps(export: &Export, cap: Duration) -> Vec<(Duration, Step, f64)> {
    let mut out = Vec::new();
    // The clock starts at the open (the first delta that brought records in), which is where the
    // growth timeline starts too, so the two stay in step.
    let mut last_t: Option<f64> = export
        .deltas
        .iter()
        .find(|d| d["count1"].as_i64().unwrap_or(0) > 0)
        .and_then(|d| d["t"].as_f64());
    for action in &export.actions {
        let t = action["t"].as_f64().unwrap_or(0.0);
        let until = action["until"].as_f64().unwrap_or(t);
        let gap = last_t.map_or(Duration::from_millis(0), |p| {
            Duration::from_millis((t - p).max(0.0) as u64).min(cap)
        });
        last_t = Some(action["until"].as_f64().unwrap_or(t));
        let kind = action["kind"].as_str().unwrap_or("");
        let step = match kind {
            "wheel" => Step::Wheel(
                moved_between(&export.states, t, until)
                    .filter(|moved| *moved != 0)
                    .unwrap_or_else(|| action["dy"].as_i64().unwrap_or(0)),
            ),
            "key" => Step::Key(action["key"].as_str().unwrap_or("").to_string()),
            "jump" | "reveal" | "move" | "hold" => match action["index"]
                .as_u64()
                .and_then(|i| export.turn_of(i as usize))
            {
                Some(turn) => Step::JumpTo(turn),
                None => Step::Skip(kind.to_string()),
            },
            "follow" => Step::End,
            "fold" => Step::Fold,
            other => Step::Skip(other.to_string()),
        };
        out.push((gap, step, t));
    }
    out
}

/// The recorded state right after time `t`: its turn and follow flag.
pub fn state_after(states: &[Value], t: f64) -> Option<(Option<i64>, bool)> {
    states
        .iter()
        .find(|s| s["t"].as_f64().unwrap_or(0.0) >= t)
        .map(|s| {
            (
                s["turn"].as_i64(),
                s["following"].as_bool().unwrap_or(false),
            )
        })
}

/// One replayed step's record: what was done, what the recording saw after it, what the replay
/// saw after it.
#[derive(Debug, Clone)]
pub struct Diff {
    pub step: Step,
    pub recorded: Option<(Option<i64>, bool)>,
    pub replayed: (Option<i64>, bool),
}

impl Diff {
    /// The turn difference, when both sides name a turn.
    pub fn turn_delta(&self) -> Option<i64> {
        match (self.recorded, self.replayed.0) {
            (Some((Some(a), _)), Some(b)) => Some(b - a),
            _ => None,
        }
    }
}

/// The surface's own word for the turn under P after the last transaction, from the history.
pub fn replayed_state(tab: &headless_chrome::Tab) -> (Option<i64>, bool) {
    let last = super::probe(
        tab,
        "(function(){ var s = window.__viewportHistory.states; var l = s[s.length - 1] || {}; return { turn: l.turn == null ? null : l.turn, following: !!l.following }; })()",
    );
    (
        last["turn"].as_i64(),
        last["following"].as_bool().unwrap_or(false),
    )
}

/// A one-line summary of a diff list: how many steps, how many named a turn on both sides, the
/// largest turn difference and where it first exceeded `tolerance`.
pub fn summarize(diffs: &[Diff], tolerance: i64) -> String {
    let named: Vec<(usize, i64)> = diffs
        .iter()
        .enumerate()
        .filter_map(|(i, d)| d.turn_delta().map(|x| (i, x)))
        .collect();
    let worst = named.iter().map(|(_, x)| x.abs()).max().unwrap_or(0);
    let first = named.iter().find(|(_, x)| x.abs() > tolerance);
    format!(
        "{} steps, {} compared, worst turn delta {}, first past {}: {}",
        diffs.len(),
        named.len(),
        worst,
        tolerance,
        first.map_or("none".to_string(), |(i, x)| format!(
            "step {i} ({:?}) delta {x}",
            diffs[*i].step
        ))
    )
}

/// Which surface an export was recorded on.
pub fn surface_of(export: &Export) -> Surface {
    if export.page == "app" {
        Surface::AppShell
    } else {
        Surface::Classic
    }
}

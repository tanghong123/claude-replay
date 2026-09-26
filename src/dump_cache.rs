//! **`--dump --json --cache`** (#10) — the structured block stream a collector asks for every day,
//! resumed from where its previous dump of the session stopped instead of folded from byte zero.
//!
//! A plain dump reads the whole transcript to write it, and a daily sweep writes the same sessions
//! day after day: measured on 2026-09-26, whid's one-day window folded 1.1 GB to pick up 121 MB of
//! growth, 8.4 s of its 18 s run. The #167 cache already knows how to resume a fold — a resume
//! point, a CRC of the bytes below it, the session facts the fold carries — so this is that cache
//! with a namespace of its own ([`Presentation::Json`]), which a sweep can hold without ever
//! waiting on, or denying, a person reading the same session in the TUI.
//!
//! **The entry holds BLOCKS, not the stream's lines.** Storing the projected lines would make a
//! resume a byte copy, but it would also make the projection a second thing a cached entry can go
//! stale on: a block field added to the stream, with no version bumped, would keep being served
//! without it from every entry written before — the failure `render_flavor` records twice for the
//! HTML presentation. Stored blocks are the TUI's own format ([`ArcLog`]), so the entry goes stale
//! on exactly what the TUI's does, `FOLD_VERSION`, and the stream is projected at emission by the
//! one function a plain dump uses. That the two write the same bytes is then a claim about the
//! fold alone, which is what the resume principle already guarantees and the tests below check.
//!
//! The entry is trusted exactly as far as any durable entry is (#222): the transcript's first
//! line must still be the one it was written from, and the transcript must still reach the
//! resume point. A rewrite in place that keeps both is not looked for — an agent only appends to
//! its transcript, and a check whose cost grows with the prefix is the cost a resume exists to
//! avoid.
//!
//! What a cached dump will not do is WAIT, or fail where a plain dump would succeed. A peer
//! holding the entry (another sweep of the same session), a cache it cannot write, or a transcript
//! that does not end at a line boundary all get the plain dump — the last because a plain dump
//! folds a final line that parses even without its newline, while a resumable fold must stop
//! before it (the line may still be growing), so the two would differ by that one record.

use crate::cache::{Admission, Denial, Origin, PerSession, Presentation, SessionCache};
use crate::{Agent, Transcript};
use claude_replay_core::engine::meta_stream::Versions;
use claude_replay_tui::store::ArcLog;
use std::io::Write;
use std::path::Path;

/// What a cached dump served — for the tests and for nothing else: stdout is the stream, and a
/// consumer that cares can time it.
#[derive(Debug, PartialEq)]
pub(crate) enum Served {
    /// From the entry: resumed from it, or folded cold into it — the origin says which, and why.
    Entry(Origin),
    /// The plain dump.
    Plain(Fallback),
}

/// Why a cached dump served the plain one.
#[derive(Debug, PartialEq)]
pub(crate) enum Fallback {
    /// Another live process holds the entry.
    Held,
    /// No entry can exist: the cache directory cannot be written, or the transcript's name
    /// cannot key one.
    Unavailable,
    /// The transcript ends mid-line. The entry still advanced over every complete line.
    TornTail,
}

/// The plain dump: fold the whole transcript and write its stream.
pub(crate) fn write_plain<W: Write + ?Sized>(
    agent: Agent,
    path: &Path,
    out: &mut W,
) -> anyhow::Result<()> {
    // The flat parse: top-level blocks are identical to the enriched one's — a `SubAgent`
    // emits spawn facts and its `agent_id`, and the child transcript is its own session
    // (discoverable via `--paths --all`), not an inline sub-stream.
    let session = claude_replay_core::parse_session_as(agent, path)?;
    claude_replay_core::block_json::write_block_stream(&session, out)?;
    Ok(())
}

/// The cached dump of `path` into `out`, its entry under `root` (the durable cache's `sessions/`,
/// or a test's own). Same bytes as [`write_plain`].
pub(crate) fn write_cached<W: Write + ?Sized>(
    root: &Path,
    agent: Agent,
    path: &Path,
    out: &mut W,
) -> anyhow::Result<Served> {
    // The entry's key is the transcript's stem, as the TUI's is: a session id, never a title.
    let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
        write_plain(agent, path, out)?;
        return Ok(Served::Plain(Fallback::Unavailable));
    };
    let entries = PerSession::<()>::new(
        root.to_path_buf(),
        Presentation::Json,
        // Blocks, stored exactly as the TUI stores them: no render parameters, so no flavor.
        Versions::current(None),
    );
    // Take back entries nothing has touched for a fortnight — the TUI does this once per run,
    // and a machine that only ever sweeps would otherwise keep every entry it ever wrote.
    entries.gc();
    let cache: SessionCache<ArcLog, (), PerSession<()>> = SessionCache::new(entries);
    cache.register(id, Transcript::open(agent, path));
    let (session, origin) =
        match cache.admit(id, |dir| ArcLog::open_append(&dir.join("blocks.jsonl"))) {
            Admission::Owned { session, origin } => (session, origin),
            Admission::Denied(why) => {
                write_plain(agent, path, out)?;
                return Ok(Served::Plain(match why {
                    Denial::Held(_) => Fallback::Held,
                    Denial::Unavailable(_) => Fallback::Unavailable,
                }));
            }
        };
    // Fold what the entry does not hold yet: from its resume point, or from byte zero into a
    // fresh entry. Committed blocks are appended to the entry as they commit.
    session.advance()?;
    if !ends_at_a_line_boundary(path)? {
        cache.release(id);
        write_plain(agent, path, out)?;
        return Ok(Served::Plain(Fallback::TornTail));
    }
    let committed = session.committed_arcs();
    let (open, ()) = session.open_delta_with(|_, _, _| ());
    // Unlocked BEFORE the stream is written: a slow reader on the other end of the pipe must not
    // hold the entry from the next sweep. What was read above is ours to write regardless.
    cache.release(id);
    // A `Workflow` call's fleet (#38) is runtime state beside the transcript, which a ONE-SHOT
    // read merges into its stream and a resumable fold never does — the entry stays a function of
    // the transcript, as the live path's must. This dump is one-shot however its fold began, so it
    // merges the same members at the same places, zone by zone as the plain parse does, from
    // the rosters as they stand now.
    let adapter = claude_replay_core::adapter(agent);
    let rosters = adapter.spawn_rosters(path);
    if rosters.is_empty() {
        claude_replay_core::block_json::write_blocks(
            committed
                .iter()
                .map(|b| &**b)
                .chain(open.provisional.iter()),
            &open.user_times,
            out,
        )?;
    } else {
        let committed = claude_replay_core::expand_spawn_rosters(
            adapter,
            committed.iter().map(|b| (**b).clone()).collect(),
            &rosters,
        );
        let open_zone =
            claude_replay_core::expand_spawn_rosters(adapter, open.provisional, &rosters);
        claude_replay_core::block_json::write_blocks(
            committed.iter().chain(open_zone.iter()),
            &open.user_times,
            out,
        )?;
    }
    Ok(Served::Entry(origin))
}

/// Whether the transcript ends with a newline (or is empty) — no final line in progress.
fn ends_at_a_line_boundary(path: &Path) -> std::io::Result<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    if len == 0 {
        return Ok(true);
    }
    f.seek(SeekFrom::Start(len - 1))?;
    let mut last = [0u8; 1];
    f.read_exact(&mut last)?;
    Ok(last[0] == b'\n')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::ColdReason;
    use serde_json::json;
    use std::path::PathBuf;

    fn scratch(case: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("cr-dump-cache-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn plain(path: &Path) -> Vec<u8> {
        let mut out = Vec::new();
        write_plain(Agent::CLAUDE, path, &mut out).unwrap();
        out
    }

    fn cached(root: &Path, path: &Path) -> (Vec<u8>, Served) {
        let mut out = Vec::new();
        let served = write_cached(root, Agent::CLAUDE, path, &mut out).unwrap();
        (out, served)
    }

    fn append(path: &Path, bytes: &[u8]) {
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }

    /// Every position a sweep can find a growing transcript at, in order: for each line, half-way
    /// through it, one byte short of its newline (the record complete, the newline not yet
    /// written), and just past it.
    fn growth_points(corpus: &str) -> Vec<usize> {
        let mut points = Vec::new();
        let mut start = 0;
        for line in corpus.split_inclusive('\n') {
            let end = start + line.len();
            points.extend([start + line.len() / 2, end - 1, end]);
            start = end;
        }
        points.dedup();
        points
    }

    /// What one sweep of a growing transcript saw, dump by dump.
    #[derive(Default)]
    struct Sweep {
        /// Resume offsets, in order, of the dumps that resumed.
        resumes: Vec<u64>,
        /// Dumps that met a final line in progress.
        torn: usize,
        /// Dumps at a line boundary that folded cold, after one had already resumed.
        lost: Vec<String>,
    }

    /// Grow `path` to `corpus` through every growth point, dumping cached after each step and
    /// asserting the plain dump's bytes every time. `between` runs before each step's dump —
    /// what else changes beside the transcript while it grows. The entry is never cleared, so
    /// each dump resumes from what the one before left — the only way a resume point is used in
    /// production.
    fn sweep(root: &Path, path: &Path, corpus: &str, mut between: impl FnMut(usize)) -> Sweep {
        let mut seen = Sweep::default();
        let mut written = 0;
        for point in growth_points(corpus) {
            append(path, &corpus.as_bytes()[written..point]);
            written = point;
            between(point);
            let (got, served) = cached(root, path);
            assert_eq!(
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(&plain(path)),
                "the cached stream after {point} of {} bytes ({served:?})",
                corpus.len()
            );
            match served {
                Served::Entry(Origin::Resumed { replay_from, .. }) => {
                    assert!(replay_from > 0, "a resume starts past byte zero");
                    seen.resumes.push(replay_from);
                }
                Served::Plain(Fallback::TornTail) => seen.torn += 1,
                Served::Entry(cold) if !seen.resumes.is_empty() => {
                    seen.lost.push(format!("{point}: {cold:?}"))
                }
                Served::Entry(_) => {}
                other => panic!("nothing else holds this entry: {other:?}"),
            }
        }
        seen
    }

    /// The corpus that exercises every block kind the page can draw (#174): if the cached stream
    /// matches the plain one over this, it matches over every projection arm. It parks a prompt
    /// in the queue for good, which pins every resume point to its first line — nothing commits
    /// while a prompt waits, and rightly — so it tests the stream, not the depth of a resume.
    #[test]
    fn a_growing_transcript_dumps_the_same_bytes_cached_or_not() {
        let dir = scratch("growth");
        let root = dir.join("cache");
        let path = dir.join("s.jsonl");
        let corpus = crate::html_export::audit::audit_jsonl();
        let seen = sweep(&root, &path, &corpus, |_| {});
        assert!(
            seen.torn > 0 && !seen.resumes.is_empty(),
            "{:?}",
            seen.resumes
        );
        assert!(
            seen.lost.is_empty(),
            "a resume point, once laid, is kept as the transcript grows: {:?}",
            seen.lost
        );
        // And the finished transcript, dumped again untouched, resumes rather than refolds.
        let (got, served) = cached(&root, &path);
        assert_eq!(got, plain(&path));
        assert!(
            matches!(served, Served::Entry(Origin::Resumed { .. })),
            "{served:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session whose turns COMMIT, with the shapes a real sweep meets: tool calls and their
    /// results, a prompt queued mid-turn and picked up, and a `Workflow` call whose fleet lives
    /// beside the transcript in `run` (#38).
    fn working_session(run: &Path) -> String {
        let mut out = String::new();
        let mut n = 0u32;
        let mut stamp = || {
            n += 1;
            format!("2026-09-26T10:{:02}:{:02}.000Z", n / 60, n % 60)
        };
        let mut push = |v: serde_json::Value| {
            out.push_str(&v.to_string());
            out.push('\n');
        };
        for t in 0..12 {
            push(
                json!({"type":"user","cwd":"/w","sessionId":"s","timestamp":stamp(),
                "message":{"role":"user","content":[{"type":"text","text":format!("turn {t}: where is fold{t}?")}]}}),
            );
            let call = format!("toolu_{t}");
            push(json!({"type":"assistant","timestamp":stamp(),
                "message":{"role":"assistant","content":[{"type":"tool_use","id":call,"name":"Bash","input":{"command":format!("grep -n fold{t} src")}}]}}));
            if t == 7 {
                // A prompt typed while the agent works, then picked up (#165's rewrite).
                push(
                    json!({"type":"queue-operation","operation":"enqueue","timestamp":stamp(),
                    "content":"and check the tests too"}),
                );
            }
            push(json!({"type":"user","timestamp":stamp(),
                "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":call,"content":format!("src/fold.rs:{t}: fn fold{t}()\n")}]}}));
            if t == 7 {
                push(json!({"type":"queue-operation","operation":"dequeue","timestamp":stamp()}));
                push(json!({"type":"attachment","timestamp":stamp(),
                    "attachment":{"type":"queued_command","commandMode":"prompt","prompt":"and check the tests too"}}));
            }
            if t == 4 {
                let wf = "toolu_wf";
                push(json!({"type":"assistant","timestamp":stamp(),
                    "message":{"role":"assistant","content":[{"type":"tool_use","id":wf,"name":"Workflow","input":{"script":"export const meta = {}"}}]}}));
                push(json!({"type":"user","timestamp":stamp(),
                    "message":{"role":"user","content":[{"type":"tool_result","tool_use_id":wf,
                        "content":format!("Workflow launched in background. Task ID: w1\nTranscript dir: {}", run.display())}]}}));
            }
            push(json!({"type":"assistant","timestamp":stamp(),
                "message":{"role":"assistant","content":[{"type":"text","text":format!("fold{t} is in src/fold.rs.")}]}}));
        }
        out
    }

    /// #10 as the daily sweep meets it: a working session grows between dumps and each dump
    /// resumes from further along, while the `Workflow` run's roster grows beside it — a member
    /// arrives half-way. The stream is the plain dump's at every step, fleet included: a
    /// resumable fold never folds a roster in, and a one-shot dump always does.
    #[test]
    fn a_working_session_resumes_further_each_time_and_keeps_its_fleet() {
        let dir = scratch("fleet");
        let root = dir.join("cache");
        let path = dir.join("s.jsonl");
        let run = dir
            .join("s")
            .join("subagents")
            .join("workflows")
            .join("run1");
        std::fs::create_dir_all(&run).unwrap();
        let journal = run.join("journal.jsonl");
        append(
            &journal,
            b"{\"type\":\"started\",\"agentId\":\"afleet1\",\"label\":\"scan:parser\",\"phase\":\"Scan\"}\n\
              {\"type\":\"result\",\"agentId\":\"afleet1\",\"result\":\"The parser folds once.\"}\n",
        );
        let corpus = working_session(&run);
        let mut second = false;
        let seen = sweep(&root, &path, &corpus, |point| {
            if !second && point > corpus.len() / 2 {
                second = true;
                append(
                    &journal,
                    b"{\"type\":\"started\",\"agentId\":\"afleet2\",\"label\":\"scan:tests\",\"phase\":\"Scan\"}\n",
                );
            }
        });
        let stream = String::from_utf8(plain(&path)).unwrap();
        assert!(
            stream.contains("\"agent_id\":\"afleet1\"")
                && stream.contains("\"agent_id\":\"afleet2\""),
            "the fleet is in the stream, or this case tests nothing"
        );
        assert!(seen.lost.is_empty(), "{:?}", seen.lost);
        let last = *seen.resumes.last().expect("the sweep resumed");
        assert!(
            last as usize > corpus.len() * 3 / 4,
            "the resume point follows the transcript: the last dump resumed at {last} of {}",
            corpus.len()
        );
        assert!(
            seen.resumes.windows(2).all(|w| w[0] <= w[1]),
            "and never moves back: {:?}",
            seen.resumes
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same, from a cold entry at every growth point: whatever a transcript looks like the
    /// first time a sweep meets it, the first cached dump writes the plain dump's bytes.
    #[test]
    fn a_first_cached_dump_matches_the_plain_one_wherever_the_transcript_ends() {
        let dir = scratch("first");
        let path = dir.join("s.jsonl");
        let corpus = crate::html_export::audit::audit_jsonl();
        for point in growth_points(&corpus) {
            std::fs::write(&path, &corpus.as_bytes()[..point]).unwrap();
            let root = dir.join(format!("cache-{point}"));
            let (got, served) = cached(&root, &path);
            assert_eq!(
                String::from_utf8_lossy(&got),
                String::from_utf8_lossy(&plain(&path)),
                "a first dump at {point} bytes ({served:?})"
            );
            let _ = std::fs::remove_dir_all(&root);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A transcript replaced under the entry — another with the same name, or one cut back and
    /// grown again — is never spliced onto what the entry holds: the dump folds what the entry
    /// cannot vouch for and writes the plain dump's bytes. What vouches is #222's pair, the
    /// first line's identity and a length that never shrinks below the resume point; an in-place
    /// rewrite that keeps both is not looked for, since an agent only appends to its transcript.
    #[test]
    fn a_rewritten_transcript_is_folded_cold_not_spliced() {
        let dir = scratch("rewrite");
        let root = dir.join("cache");
        let path = dir.join("s.jsonl");
        let run = dir.join("unused");
        let corpus = working_session(&run);
        std::fs::write(&path, &corpus).unwrap();
        let (_, served) = cached(&root, &path);
        assert_eq!(
            served,
            Served::Entry(Origin::Cold(ColdReason::NoPriorCache))
        );

        // Same length, one character different in the first prompt.
        let rewritten = corpus.replacen("where is fold0?", "where is Fold0?", 1);
        assert_ne!(rewritten, corpus, "the fixture moved: pick another word");
        std::fs::write(&path, &rewritten).unwrap();
        let (got, served) = cached(&root, &path);
        assert_eq!(
            served,
            Served::Entry(Origin::Cold(ColdReason::SourceRewritten))
        );
        assert_eq!(got, plain(&path));

        // Cut back to a prefix of itself, and regrown by a different second half.
        let cut = rewritten[..rewritten.len() / 2].rfind('\n').unwrap() + 1;
        std::fs::write(&path, &rewritten[..cut]).unwrap();
        let (got, served) = cached(&root, &path);
        assert_eq!(got, plain(&path), "{served:?}");
        if let Served::Entry(Origin::Resumed { replay_from, .. }) = served {
            assert!(
                replay_from as usize <= cut,
                "{replay_from} past the cut at {cut}"
            );
        }
        append(&path, corpus[cut..].replace("fold", "unfold").as_bytes());
        let (got, served) = cached(&root, &path);
        assert_eq!(got, plain(&path), "{served:?}");
        if let Served::Entry(Origin::Resumed { replay_from, .. }) = served {
            assert!(
                replay_from as usize <= cut,
                "{replay_from} past the cut at {cut}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A peer holding the entry — another sweep of the same session — is not waited for: this
    /// dump writes the plain stream, and leaves the peer's entry alone.
    #[test]
    fn a_held_entry_gets_the_plain_dump_and_is_left_alone() {
        let dir = scratch("held");
        let root = dir.join("cache");
        let path = dir.join("s.jsonl");
        std::fs::write(&path, working_session(&dir.join("unused"))).unwrap();
        let entry = crate::cache::admit::entry_dir(&root, Presentation::Json, "s");
        std::fs::create_dir_all(&entry).unwrap();
        // A live process that is not us: the one that ran the tests.
        let peer = std::os::unix::process::parent_id();
        let lock = format!(r#"{{"pid":{peer},"dir":{:?},"note":null}}"#, entry);
        std::fs::write(entry.join("LOCK"), &lock).unwrap();

        let (got, served) = cached(&root, &path);
        assert_eq!(served, Served::Plain(Fallback::Held));
        assert_eq!(got, plain(&path));
        assert_eq!(
            std::fs::read_to_string(entry.join("LOCK")).unwrap(),
            lock,
            "the peer's lock is untouched"
        );
        assert!(
            !entry.join("blocks.jsonl").exists(),
            "and nothing was written into its entry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

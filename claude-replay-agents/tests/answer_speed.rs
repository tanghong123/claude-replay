//! Synthetic timing fixtures for turn attribution and durable resume.
use claude_replay_agents::ClaudeAdapter;
use claude_replay_engine::engine::meta_stream::{MaterializedMeta, FOLD_VERSION};
use claude_replay_engine::engine::session::{AnswerTotals, InMemoryStore, LastAnswer};
use claude_replay_engine::metrics::AnswerTiming;
use claude_replay_engine::SessionAccumulator;
use serde_json::{json, Value};

fn user(at: &str, text: &str) -> Value {
    json!({"type":"user", "timestamp":at, "message":{"role":"user","content":text}})
}
fn answer(at: &str, id: &str, model: &str, tok: u64) -> Value {
    json!({"type":"assistant", "timestamp":at, "message":{"role":"assistant", "id":id,
        "model":model, "usage":{"input_tokens":1,"output_tokens":tok},
        "content":[{"type":"text","text":"synthetic reply"}]}})
}
fn line(v: Value) -> String {
    v.to_string()
}
fn fold(lines: &[String]) -> SessionAccumulator {
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut at = 0;
    for l in lines {
        acc.advance_at(at, l);
        at += l.len() as u64 + 1;
    }
    acc
}

#[test]
fn single_open_answer_and_cross_turn_answer_have_distinct_ownership() {
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "first")),
        line(answer("2026-10-30T10:00:01Z", "a", "claude-opus-4-8", 10)),
        line(user("2026-10-30T10:00:02Z", "second")),
        line(answer("2026-10-30T10:00:03Z", "a", "claude-opus-4-8", 30)),
        line(answer("2026-10-30T10:00:04Z", "b", "gpt-example", 20)),
    ];
    let acc = fold(&lines);
    let got = acc.answer_readings();
    assert_eq!(got.turns.len(), 2);
    let first = got.turns[0].as_ref().unwrap();
    assert_eq!(
        (first.tok, first.ms, first.n, first.model.as_str()),
        (10, 1000, 1, "opus4.8")
    );
    let second = got.turns[1].as_ref().unwrap();
    assert_eq!(
        (second.tok, second.ms, second.n, second.model.as_str()),
        (20, 2000, 1, "gpt-example")
    );
    assert_eq!(
        got.totals["claude-opus-4-8"],
        AnswerTotals {
            tok: 30,
            ms: 3000,
            n: 1
        }
    );
    assert_eq!(got.last.as_ref().unwrap().model, "gpt-example");
}

#[test]
fn no_timing_or_tokens_never_fabricates_a_speed_and_positive_submillisecond_is_one_ms() {
    let mut timing = AnswerTiming {
        id: "a".into(),
        model: "m".into(),
        provider: None,
        sent: Some(1.0),
        first: Some(1.0),
        last: Some(1.0001),
        lines: 1,
        output_tokens: Some(5),
    };
    assert_eq!(LastAnswer::from_timing(&timing).unwrap().ms, 1);
    timing.last = timing.sent;
    assert!(LastAnswer::from_timing(&timing).is_none());
    timing.last = Some(2.0);
    timing.output_tokens = Some(0);
    assert!(LastAnswer::from_timing(&timing).is_none());
    timing.output_tokens = Some(1);
    timing.sent = None;
    assert!(LastAnswer::from_timing(&timing).is_none());
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "hi")),
        line(answer("2026-10-30T10:00:01Z", "a", "m", 0)),
    ];
    let got = fold(&lines).answer_readings();
    assert_eq!(got.turns, vec![None]);
    assert!(got.totals.is_empty() && got.last.is_none());
}

#[test]
fn pre_first_turn_answer_is_dropped() {
    let lines = vec![
        line(json!({"type":"user","timestamp":"2026-10-30T10:00:00Z"})),
        line(answer("2026-10-30T10:00:01Z", "before", "m", 100)),
        line(user("2026-10-30T10:00:02Z", "first")),
        line(answer("2026-10-30T10:00:03Z", "after", "m", 5)),
    ];
    let got = fold(&lines).answer_readings();
    assert_eq!(got.totals["m"].tok, 5);
    assert_eq!(got.turns[0].as_ref().unwrap().tok, 5);
}

#[test]
fn each_resume_and_checkpoint_matches_a_cold_fold_across_pinned_turns() {
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "one")),
        line(answer("2026-10-30T10:00:01Z", "a", "claude-opus-4-8", 10)),
        line(user("2026-10-30T10:00:02Z", "two")),
        line(answer("2026-10-30T10:00:03Z", "b", "claude-opus-4-8", 20)),
        line(json!({"type":"queue-operation","operation":"enqueue","content":"waiting"})),
        line(user("2026-10-30T10:00:04Z", "three")),
        line(answer("2026-10-30T10:00:05Z", "c", "other", 30)),
        line(user("2026-10-30T10:00:06Z", "four")),
        line(answer("2026-10-30T10:00:07Z", "d", "other", 40)),
        line(json!({"type":"queue-operation","operation":"dequeue"})),
        line(user("2026-10-30T10:00:08Z", "five")),
        line(answer("2026-10-30T10:00:09Z", "e", "other", 50)),
        line(user("2026-10-30T10:00:10Z", "six")),
    ];
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut materialized = MaterializedMeta::default();
    let mut offset = 0u64;
    let mut resumes = Vec::new();
    for (index, l) in lines.iter().enumerate() {
        acc.advance_at(offset, l);
        offset += l.len() as u64 + 1;
        if index == 7 {
            let readings = acc.answer_readings();
            assert!(
                readings.turns.len() - readings.committed_turns >= 3,
                "the queued marker must pin several turns"
            );
        }
        for rec in acc.drain_meta() {
            materialized.push(&rec);
            if let Some(resume) = rec.resume {
                assert_eq!(
                    materialized,
                    acc.materialized(),
                    "independent checkpoint equals folded deltas"
                );
                assert!(materialized
                    .answer_totals
                    .values()
                    .all(|t| *t != AnswerTotals::default()));
                resumes.push((acc.committed().to_vec(), materialized.clone(), resume));
            }
        }
    }
    assert!(resumes.len() >= 2);
    assert_eq!(FOLD_VERSION, 102);
    let expected = acc.answer_readings();
    for (committed, mm, resume) in resumes {
        let mut restored =
            SessionAccumulator::restore(&ClaudeAdapter, InMemoryStore, committed, mm, &resume);
        let mut at = 0u64;
        for l in &lines {
            if at >= resume.replay_from {
                restored.advance_at(at, l);
            }
            at += l.len() as u64 + 1;
        }
        assert_eq!(
            restored.answer_readings(),
            expected,
            "resume at {}",
            resume.replay_from
        );
    }
}

#[test]
fn display_model_variants_merge_before_turn_and_session_rates() {
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "one")),
        line(answer("2026-10-30T10:00:01Z", "a", "claude-opus-4-8", 10)),
        line(answer(
            "2026-10-30T10:00:02Z",
            "b",
            "claude-opus-4-8[1m]",
            20,
        )),
        line(answer("2026-10-30T10:00:03Z", "c", "gpt-example", 25)),
    ];
    let got = fold(&lines).answer_readings();
    let speeds = got.speeds();
    assert_eq!(speeds.len(), 2);
    let opus = speeds.iter().find(|a| a.model == "opus4.8").unwrap();
    assert_eq!((opus.tok, opus.ms, opus.n), (30, 3000, 2));
    assert_eq!(got.turns[0].as_ref().unwrap().model, "opus4.8");
}

#[test]
fn pre_turn_open_answer_remains_excluded_across_resume() {
    let lines = vec![
        line(json!({"type":"user","timestamp":"2026-10-30T10:00:00Z"})),
        line(answer("2026-10-30T10:00:01Z", "before", "m", 100)),
        line(user("2026-10-30T10:00:02Z", "first")),
        line(user("2026-10-30T10:00:03Z", "second")),
        line(answer("2026-10-30T10:00:04Z", "after", "m", 5)),
    ];
    let mut acc = SessionAccumulator::new(&ClaudeAdapter);
    let mut mm = MaterializedMeta::default();
    let mut saved = None;
    let mut at = 0;
    for l in &lines {
        acc.advance_at(at, l);
        at += l.len() as u64 + 1;
        for rec in acc.drain_meta() {
            mm.push(&rec);
            if mm.excluded_open_answer {
                saved = rec
                    .resume
                    .map(|r| (acc.committed().to_vec(), mm.clone(), r));
            }
        }
    }
    let expected = acc.answer_readings();
    let (committed, mm, resume) = saved.expect("the pre-turn answer survives a resume boundary");
    let mut restored =
        SessionAccumulator::restore(&ClaudeAdapter, InMemoryStore, committed, mm, &resume);
    let mut offset = 0;
    for l in &lines {
        if offset >= resume.replay_from {
            restored.advance_at(offset, l);
        }
        offset += l.len() as u64 + 1;
    }
    assert_eq!(restored.answer_readings(), expected);
    assert_eq!(expected.totals["m"].tok, 5);
}

#[test]
fn qoder_uses_the_same_timing_and_antigravity_stays_silent() {
    use claude_replay_agents::{AntigravityAdapter, QoderWorkAdapter};
    let lines = [
        line(user("2026-10-30T10:00:00Z", "one")),
        line(answer("2026-10-30T10:00:02Z", "a", "qoder-model", 64)),
    ];
    let mut qoder = SessionAccumulator::new(&QoderWorkAdapter);
    for l in &lines {
        qoder.advance_at(0, l);
    }
    let reading = qoder.answer_readings().turns[0].clone().unwrap();
    assert_eq!(
        (reading.tok, reading.ms, reading.model.as_str()),
        (64, 2000, "qoder-model")
    );
    let mut ag = SessionAccumulator::new(&AntigravityAdapter);
    for v in [
        json!({"step_index":0,"type":"USER_INPUT","source":"USER_EXPLICIT","status":"DONE","created_at":"2026-10-30T10:00:00Z","content":"hi"}),
        json!({"step_index":1,"type":"PLANNER_RESPONSE","source":"MODEL","status":"DONE","created_at":"2026-10-30T10:00:02Z","content":"hello","model":"gemini-test","output_tokens":64}),
    ] {
        ag.advance_at(0, &line(v));
    }
    assert!(ag.answer_readings().totals.is_empty());
    assert!(ag.answer_readings().turns.iter().all(Option::is_none));
}

#[test]
fn cross_turn_answer_resumes_through_two_overlapping_queue_pins() {
    use claude_replay_engine::engine::meta_stream::MetaRecord;
    let queue = |operation: &str, text: &str| {
        line(json!({"type":"queue-operation", "operation":operation, "content":text}))
    };
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "turn T")),
        queue("enqueue", "Q1"),
        queue("enqueue", "Q2"),
        line(answer("2026-10-30T10:00:01Z", "msg_cross", "m", 10)),
        queue("dequeue", "Q1"),
        line(user("2026-10-30T10:00:02Z", "Q1")),
        line(answer("2026-10-30T10:00:03Z", "msg_cross", "m", 30)),
        queue("enqueue", "Q3"),
        queue("dequeue", "Q2"),
        line(user("2026-10-30T10:00:04Z", "Q2")),
        line(answer("2026-10-30T10:00:05Z", "next", "m", 20)),
        queue("dequeue", "Q3"),
        line(user("2026-10-30T10:00:06Z", "Q3")),
        line(answer("2026-10-30T10:00:07Z", "third", "m", 12)),
        line(user("2026-10-30T10:00:08Z", "fourth")),
    ];
    let mut cold = SessionAccumulator::new(&ClaudeAdapter);
    let mut stream = Vec::<MetaRecord>::new();
    let mut saved = None;
    let mut offset = 0;
    for (index, l) in lines.iter().enumerate() {
        cold.advance_at(offset, l);
        offset += l.len() as u64 + 1;
        let recs = cold.drain_meta();
        stream.extend(recs);
        if index == 6 {
            assert_eq!(
                cold.committed_meta().turns,
                0,
                "Q2 pins turn T across the continuation"
            );
            assert_eq!(cold.answer_readings().turns.len(), 2);
        }
        if index == 9 {
            assert_eq!(
                cold.committed_meta().turns,
                1,
                "Q2 releases T while Q3 pins T+1"
            );
            let resume = stream.last().unwrap().resume.clone().unwrap();
            saved = Some((
                cold.committed().to_vec(),
                cold.materialized(),
                resume,
                stream.len(),
            ));
        }
    }
    let expected_readings = cold.answer_readings();
    let expected_materialized = cold.materialized();
    let (committed, mm, resume, prefix) = saved.unwrap();
    let mut resumed =
        SessionAccumulator::restore(&ClaudeAdapter, InMemoryStore, committed, mm, &resume);
    let mut resumed_records = Vec::new();
    let mut at = 0;
    for l in &lines {
        if at >= resume.replay_from {
            resumed.advance_at(at, l);
            resumed_records.extend(resumed.drain_meta());
        }
        at += l.len() as u64 + 1;
    }
    assert_eq!(
        resumed_records,
        stream[prefix..],
        "durable deltas after the resume boundary"
    );
    assert_eq!(resumed.materialized(), expected_materialized);
    assert_eq!(resumed.answer_readings(), expected_readings);
    assert_eq!(expected_readings.turns[0].as_ref().unwrap().tok, 30);
    assert_eq!(
        expected_readings.turns[1], None,
        "continuation stays owned by T"
    );
    assert_eq!(expected_readings.totals["m"].n, 3);
}

#[test]
fn real_checkpoint_round_trips_validates_and_resumes_without_zero_entries() {
    use claude_replay_engine::engine::meta_stream::{
        align, AlignError, MetaRecord, Versions, CHECKPOINT_EVERY,
    };
    use claude_replay_present::cache::stream::{MetaReader, MetaWriter};
    let mut lines = Vec::new();
    for turn in 0..(CHECKPOINT_EVERY + 8) {
        let minute = turn / 60;
        let second = turn % 60;
        let at = format!("2026-10-30T10:{minute:02}:{second:02}Z");
        let after = format!("2026-10-30T11:{minute:02}:{second:02}Z");
        lines.push(line(user(&at, &format!("prompt {turn}"))));
        if turn == 0 {
            lines.push(line(json!({"type":"assistant", "timestamp":after, "message":{"role":"assistant", "id":"zero", "model":"zero-model", "usage":{"input_tokens":0,"output_tokens":0}, "content":[{"type":"text","text":"empty usage"}]}})));
        } else {
            lines.push(line(answer(&after, &format!("answer-{turn}"), "m", 10)));
        }
    }
    let mut cold = SessionAccumulator::new(&ClaudeAdapter);
    let mut records = Vec::<MetaRecord>::new();
    let mut saved = None;
    let mut offset = 0;
    for l in &lines {
        cold.advance_at(offset, l);
        offset += l.len() as u64 + 1;
        for rec in cold.drain_meta() {
            if rec.checkpoint.is_some() {
                assert!(saved.is_none(), "fixture crosses exactly one checkpoint");
                saved = Some((cold.committed().to_vec(), records.len()));
            }
            records.push(rec);
        }
    }
    let (committed, checkpoint_index) = saved.expect("64 resumable drains write a real checkpoint");
    assert!(checkpoint_index + 1 >= CHECKPOINT_EVERY);
    assert_eq!(
        cold.fold().2.per_model.get("zero-model"),
        Some(&claude_replay_engine::metrics::TokenCounts::default()),
        "the raw metrics really contain a zero bucket to omit"
    );
    let dir = std::env::temp_dir().join(format!("answer-speed-checkpoint-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.jsonl");
    std::fs::write(&source, lines.join("\n") + "\n").unwrap();
    let versions = Versions::current(None);
    let mut writer = MetaWriter::create(&dir, &source, versions.clone()).unwrap();
    for record in &records {
        writer.append(record).unwrap();
    }
    writer.flush().unwrap();
    drop(writer);
    let (header, reader) = MetaReader::open(&dir).unwrap().unwrap();
    assert_eq!(header.versions, versions);
    let loaded: Vec<MetaRecord> = reader.collect();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(loaded, records);
    let checkpoint = loaded[checkpoint_index].checkpoint.as_ref().unwrap();
    assert!(!checkpoint.answer_totals.contains_key("zero-model"));
    assert!(!checkpoint.tokens.contains_key("zero-model"));
    assert!(checkpoint
        .answer_totals
        .values()
        .all(|v| *v != AnswerTotals::default()));
    let aligned = align(&loaded[..=checkpoint_index], committed.len())
        .unwrap()
        .unwrap();
    assert_eq!(&aligned.meta, checkpoint);
    let compacted = align(
        &loaded[checkpoint_index..=checkpoint_index],
        committed.len(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(compacted.meta, aligned.meta);
    let mut corrupt = loaded.clone();
    corrupt[checkpoint_index]
        .checkpoint
        .as_mut()
        .unwrap()
        .answer_totals
        .get_mut("m")
        .unwrap()
        .tok += 1;
    assert!(matches!(
        align(&corrupt, cold.committed_len()),
        Err(AlignError::CheckpointMismatch)
    ));
    let mut resumed = SessionAccumulator::restore(
        &ClaudeAdapter,
        InMemoryStore,
        committed,
        aligned.meta,
        &aligned.resume,
    );
    let mut resumed_records = Vec::new();
    let mut at = 0;
    for l in &lines {
        if at >= aligned.resume.replay_from {
            resumed.advance_at(at, l);
            resumed_records.extend(resumed.drain_meta());
        }
        at += l.len() as u64 + 1;
    }
    assert_eq!(resumed_records, loaded[checkpoint_index + 1..]);
    assert_eq!(resumed.materialized(), cold.materialized());
    assert_eq!(resumed.answer_readings(), cold.answer_readings());
}

#[test]
fn elapsed_includes_a_tool_between_lines_but_not_between_answers() {
    let tool_result = |at: &str| {
        line(
            json!({"type":"user", "timestamp":at, "message":{"role":"user", "content":[{"type":"tool_result", "tool_use_id":"tool", "content":"synthetic result"}]}}),
        )
    };
    let lines = vec![
        line(user("2026-10-30T10:00:00Z", "go")),
        line(answer("2026-10-30T10:00:01Z", "first", "m", 10)),
        tool_result("2026-10-30T10:00:02Z"),
        line(answer("2026-10-30T10:00:05Z", "first", "m", 10)),
        tool_result("2026-10-30T10:00:09Z"),
        line(answer("2026-10-30T10:00:10Z", "second", "m", 20)),
    ];
    let reading = fold(&lines).answer_readings();
    assert_eq!(
        reading.totals["m"],
        AnswerTotals {
            tok: 30,
            ms: 6000,
            n: 2
        }
    );
    assert_eq!(reading.last.unwrap().ms, 1000);
}

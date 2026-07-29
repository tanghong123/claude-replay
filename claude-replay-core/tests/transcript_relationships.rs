use claude_replay_core::{Agent, AgentStatus, Block, Transcript};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "claude-replay-public-api-{name}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn write(&self, path: impl AsRef<Path>, body: &str) -> PathBuf {
        let path = self.root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

fn assert_child_contract(transcript: &Transcript, child_id: &str, child_path: &Path) {
    let session = transcript.parse().unwrap();
    let child_meta = session
        .sub_agents
        .get(child_id)
        .expect("parse exposes the resolved child entity");
    assert_eq!(child_meta.transcript.as_deref(), Some(child_path));

    let child = transcript
        .subagent(child_id)
        .expect("the same stable id opens the child transcript");
    assert_eq!(child.path(), child_path);
    assert!(
        child
            .parse()
            .unwrap()
            .blocks()
            .iter()
            .any(|block| matches!(block, Block::UserText(text) if text == "child body")),
        "the resolved child handle reads the child source"
    );

    let mut follower = transcript.follow();
    let followed = follower
        .poll_session()
        .unwrap()
        .expect("the first public follow poll folds the existing parent");
    let followed_child = followed
        .sub_agents
        .get(child_id)
        .expect("follow exposes the same resolved child entity");
    assert_eq!(followed_child.transcript.as_deref(), Some(child_path));
}

#[test]
fn codex_parent_child_navigation_uses_only_the_transcript_api() {
    let fixture = Fixture::new("codex");
    let parent = fixture.write(
        "sessions/2026/07/29/rollout-parent.jsonl",
        concat!(
            r#"{"type":"session_meta","payload":{"id":"parent","cwd":"/repo","source":"cli"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call","name":"spawn_agent","namespace":"collaboration","call_id":"spawn-1","arguments":"{\"task_name\":\"review\",\"message\":\"inspect\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"spawn-1","output":"{\"task_name\":\"/root/review\"}"}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"agent_message","author":"/root/review","content":[{"type":"input_text","text":"Message Type: FINAL_ANSWER\nPayload:\nPASS"}]}}"#,
            "\n",
        ),
    );
    let child = fixture.write(
        "sessions/2026/07/29/rollout-child.jsonl",
        concat!(
            r#"{"type":"session_meta","payload":{"id":"child","cwd":"/repo","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","agent_path":"/root/review","agent_nickname":"Nash"}}}}}"#,
            "\n",
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"child body"}]}}"#,
            "\n",
        ),
    );

    let transcript = Transcript::open(Agent::Codex, &parent);
    assert_child_contract(&transcript, "child", &child);

    let session = transcript.parse().unwrap();
    assert_eq!(session.sub_agents["child"].status, AgentStatus::Completed);
}

#[test]
fn claude_parent_child_navigation_uses_the_same_transcript_api() {
    let fixture = Fixture::new("claude");
    let parent = fixture.write(
        "project/session.jsonl",
        concat!(
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"reviewer","description":"review","prompt":"inspect"}}]}}"#,
            "\n",
            r#"{"type":"user","toolUseResult":{"agentId":"child","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]}}"#,
            "\n",
        ),
    );
    let child = fixture.write(
        "project/session/subagents/agent-child.jsonl",
        concat!(
            r#"{"type":"user","message":{"content":"child body"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"done"}]}}"#,
            "\n",
        ),
    );

    assert_child_contract(&Transcript::open(Agent::Claude, parent), "child", &child);
}

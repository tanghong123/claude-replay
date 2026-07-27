use crate::model::Block;
use crate::Agent;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Agent-neutral, operation-scoped view of transcript relationships.
///
/// Clones deliberately share one backend so a batch parse, a live follower, and
/// an HTML traversal observe the same relationship cache and refresh budget.
#[derive(Clone)]
pub struct SessionGraph {
    backend: Arc<Mutex<Box<dyn SessionGraphBackend>>>,
}

pub(crate) trait SessionGraphBackend: Send {
    fn enrich(&mut self, source: &Path, blocks: &mut [Block]);
    fn subagent_source(&mut self, root: &Path, child_id: &str) -> Option<PathBuf>;
}

impl SessionGraph {
    pub fn open(agent: Agent, anchor: &Path) -> Self {
        crate::adapter::adapter(agent).session_graph(anchor)
    }

    pub(crate) fn from_backend(backend: Box<dyn SessionGraphBackend>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }

    pub fn enrich(&self, source: &Path, blocks: &mut [Block]) {
        self.with_backend(|backend| backend.enrich(source, blocks));
        synchronize_terminal_status(blocks);
    }

    pub fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
        self.with_backend(|backend| backend.subagent_source(root, child_id))
    }

    fn with_backend<T>(&self, f: impl FnOnce(&mut dyn SessionGraphBackend) -> T) -> T {
        let mut backend = self
            .backend
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(backend.as_mut())
    }
}

fn synchronize_terminal_status(blocks: &mut [Block]) {
    let terminal: Vec<_> = blocks
        .iter()
        .filter_map(|block| match block {
            Block::AgentDone {
                agent_id,
                status,
                result,
                ..
            } if !agent_id.is_empty() && status.is_terminal() => {
                Some((agent_id.clone(), *status, result.clone()))
            }
            _ => None,
        })
        .collect();

    for block in blocks {
        let Block::SubAgent(agent) = block else {
            continue;
        };
        if let Some((_, status, result)) = terminal
            .iter()
            .rev()
            .find(|(id, _, _)| id == &agent.agent_id)
        {
            agent.status = *status;
            if agent.result.is_none() {
                agent.result.clone_from(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SessionGraph, SessionGraphBackend};
    use crate::{Agent, AgentStatus, Block, SubAgent};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingBackend {
        calls: Arc<AtomicUsize>,
    }

    impl SessionGraphBackend for CountingBackend {
        fn enrich(&mut self, _source: &Path, _blocks: &mut [Block]) {
            self.calls.fetch_add(1, Ordering::SeqCst);
        }

        fn subagent_source(&mut self, _root: &Path, _child_id: &str) -> Option<PathBuf> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            None
        }
    }

    #[test]
    fn clones_share_backend_state() {
        let calls = Arc::new(AtomicUsize::new(0));
        let graph = SessionGraph::from_backend(Box::new(CountingBackend {
            calls: Arc::clone(&calls),
        }));
        let clone = graph.clone();

        graph.enrich(Path::new("root.jsonl"), &mut []);
        clone.subagent_source(Path::new("root.jsonl"), "child");

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn claude_graph_resolves_existing_child_source() {
        let base = std::env::temp_dir().join(format!(
            "claude-replay-session-graph-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let root = base.join("session.jsonl");
        let child = base
            .join("session")
            .join("subagents")
            .join("agent-child.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&root, "").unwrap();
        std::fs::write(&child, "").unwrap();

        let graph = SessionGraph::open(Agent::Claude, &root);
        let clone = graph.clone();
        assert_eq!(
            clone.subagent_source(&root, "child").as_deref(),
            Some(child.as_path())
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn enrichment_synchronizes_resolved_terminal_status() {
        let graph = SessionGraph::from_backend(Box::new(CountingBackend {
            calls: Arc::new(AtomicUsize::new(0)),
        }));
        let mut blocks = vec![
            Block::SubAgent(SubAgent {
                agent_id: "child".into(),
                tool_use_id: "call".into(),
                agent_type: "agent".into(),
                description: "review".into(),
                prompt: String::new(),
                status: AgentStatus::Running,
                result: None,
                output_file: None,
                blocks: Vec::new(),
                subtree_cost: None,
            }),
            Block::AgentDone {
                agent_id: "child".into(),
                agent_type: String::new(),
                description: String::new(),
                status: AgentStatus::Completed,
                result: Some("PASS".into()),
            },
        ];

        graph.enrich(Path::new("root.jsonl"), &mut blocks);

        let Block::SubAgent(spawn) = &blocks[0] else {
            panic!("expected spawn");
        };
        assert_eq!(spawn.status, AgentStatus::Completed);
        assert_eq!(spawn.result.as_deref(), Some("PASS"));
    }
}

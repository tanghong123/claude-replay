use crate::engine::session::SessionMeta;
use crate::model::Block;
use crate::Agent;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Agent-neutral, operation-scoped view of transcript relationships.
///
/// Clones deliberately share one backend so a batch parse, a live follower, and
/// an HTML traversal observe the same operation boundary and resolver state.
#[derive(Clone)]
pub(crate) struct SessionGraph {
    backend: Arc<Mutex<Box<dyn SessionGraphBackend>>>,
}

impl std::fmt::Debug for SessionGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGraph").finish_non_exhaustive()
    }
}

pub(crate) trait SessionGraphBackend: Send {
    fn resolve(&mut self, source: &Path, blocks: &mut [Block]);
    fn resolve_meta(&mut self, _source: &Path, _meta: &mut SessionMeta) {}
    fn subagent_source(&mut self, root: &Path, child_id: &str) -> Option<PathBuf>;
}

impl SessionGraph {
    pub(crate) fn open(agent: Agent, anchor: &Path) -> Self {
        crate::adapter::adapter(agent).session_graph(anchor)
    }

    pub(crate) fn from_backend(backend: Box<dyn SessionGraphBackend>) -> Self {
        Self {
            backend: Arc::new(Mutex::new(backend)),
        }
    }

    /// Resolve agent-specific relationship identifiers and lifecycle state without
    /// loading child transcript content.
    pub(crate) fn resolve_relationships(&self, source: &Path, blocks: &mut [Block]) {
        self.with_backend(|backend| backend.resolve(source, blocks));
    }

    /// Normalize the lightweight live header through the same operation resolver as blocks.
    pub(crate) fn resolve_meta(&self, source: &Path, meta: &mut SessionMeta) {
        self.with_backend(|backend| backend.resolve_meta(source, meta));
    }

    pub(crate) fn subagent_source(&self, root: &Path, child_id: &str) -> Option<PathBuf> {
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
        fn resolve(&mut self, _source: &Path, _blocks: &mut [Block]) {
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

        graph.resolve_relationships(Path::new("root.jsonl"), &mut []);
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
    fn claude_relationship_resolution_keeps_child_content_lazy() {
        let base = std::env::temp_dir().join(format!(
            "claude-replay-session-graph-lazy-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::remove_dir_all(&base).ok();
        let root = base.join("session.jsonl");
        let child = base
            .join("session")
            .join("subagents")
            .join("agent-child.jsonl");
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&root, "").unwrap();
        std::fs::write(
            &child,
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"child turn"}]}}"#,
                "\n"
            ),
        )
        .unwrap();
        let mut blocks = vec![Block::SubAgent(SubAgent {
            agent_id: "child".into(),
            tool_use_id: "call".into(),
            agent_type: "Explore".into(),
            description: "inspect".into(),
            prompt: "inspect".into(),
            status: AgentStatus::Running,
            result: None,
            output_file: None,
            blocks: Vec::new(),
            subtree_cost: None,
        })];

        let graph = SessionGraph::open(Agent::Claude, &root);
        graph.resolve_relationships(&root, &mut blocks);

        let Block::SubAgent(agent) = &blocks[0] else {
            panic!("expected sub-agent");
        };
        assert!(
            agent.blocks.is_empty(),
            "relationship resolution must not eagerly parse child transcripts"
        );
        assert_eq!(graph.subagent_source(&root, "child"), Some(child));

        std::fs::remove_dir_all(base).unwrap();
    }
}

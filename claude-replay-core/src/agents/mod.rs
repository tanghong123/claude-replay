//! **The per-agent families** (#87) — one submodule per supported agent, each the
//! `model` (L1 tokenizer + `Shaping`) / `metrics` (usage folding) / `discover`
//! (transcript store) trio behind that agent's [`TranscriptAdapter`](crate::adapter)
//! row. A derived agent (QoderWork) carries only what it does differently.
//!
//! **The boundary rule (audited below):** code in this tree may reach the rest of the
//! crate ONLY through [`crate::engine::seam`] — the curated adapter contract. Anything
//! an adapter newly needs is added to the seam deliberately, never imported ad hoc; the
//! `agents_import_only_the_seam` test fails on any other `crate::` path. This is the
//! in-crate rehearsal of the future engine/agents crate split (design/core-layout.md).

pub mod claude {
    pub mod discover;
    pub(crate) mod metrics;
    pub(crate) mod model;
}
pub mod codex {
    pub mod discover;
    pub(crate) mod metrics;
    pub(crate) mod model;
}
pub mod qoderwork {
    pub mod discover;
}

#[cfg(test)]
mod tests {
    /// The boundary audit: every `crate::…` path in `agents/**` must be
    /// `crate::engine::seam` (comments exempt — doc links may name real paths).
    #[test]
    fn agents_import_only_the_seam() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/agents");
        let mut offenders = Vec::new();
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("read agents dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs")
                    || path.file_name().and_then(|n| n.to_str()) == Some("mod.rs")
                {
                    continue;
                }
                let src = std::fs::read_to_string(&path).expect("read source");
                for (n, line) in src.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    let mut rest = code;
                    while let Some(pos) = rest.find("crate::") {
                        let tail = &rest[pos..];
                        if !tail.starts_with("crate::engine::seam") {
                            offenders.push(format!(
                                "{}:{}: {}",
                                path.display(),
                                n + 1,
                                line.trim()
                            ));
                        }
                        rest = &rest[pos + "crate::".len()..];
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "agents/** must reach the crate only via crate::engine::seam \
             (add the item to the seam deliberately if an adapter needs it):\n{}",
            offenders.join("\n")
        );
    }
}

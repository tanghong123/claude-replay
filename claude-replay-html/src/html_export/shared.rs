//! Frontend modules shared by every consumer of the html crate's pages (seam 0 of
//! `design/monitor-shell-duplication.md`).
//!
//! `export.js` is embedded into self-contained pages — `--dump-html` writes a file that opens
//! from `file://` with no server behind it — so a module it shares with the monitor's app shell
//! cannot be `import`ed there. The one source lives here, and reaches its two consumers two
//! ways: the monitor serves each file unchanged as an ES module (`/monitor-ui/shared/<name>.js`,
//! see `claude_monitor::ui::asset`), and this crate INLINES each into its pages ahead of
//! `export.js` through [`inline_all`], where export.js reads it as `window.__shared.<name>`.
//!
//! The transform is deliberately textual and tiny, so it needs no JS tooling: a shared module
//! has **no imports** and **one trailing `export { a, b };` line**; the inliner drops that line,
//! publishes the same names on `window.__shared`, and wraps the body in an IIFE. The tests
//! below hold every shared module to the convention.

/// Every shared module: `(name, source)`, name without the `.js`.
pub const SHARED: &[(&str, &str)] = &[
    (
        "session-visibility",
        include_str!("../html/shared/session-visibility.js"),
    ),
    (
        "state-labels",
        include_str!("../html/shared/state-labels.js"),
    ),
    ("keymap", include_str!("../html/shared/keymap.js")),
    ("reading", include_str!("../html/shared/reading.js")),
    (
        "capabilities",
        include_str!("../html/shared/capabilities.js"),
    ),
    (
        "control-protocol",
        include_str!("../html/shared/control-protocol.js"),
    ),
    (
        "record-stream",
        include_str!("../html/shared/record-stream.js"),
    ),
    ("runtime", include_str!("../html/shared/runtime.js")),
    ("parts", include_str!("../html/shared/parts.js")),
    ("search", include_str!("../html/shared/search.js")),
    ("ids", include_str!("../html/shared/ids.js")),
];

/// The source of one shared module, for serving it as a module.
pub fn shared_source(name: &str) -> Option<&'static str> {
    SHARED.iter().find(|(n, _)| *n == name).map(|(_, s)| *s)
}

/// The names a module's trailing `export { … };` line publishes.
fn exported_names(source: &str) -> Option<Vec<&str>> {
    let line = source.trim_end().rsplit('\n').next()?.trim();
    let inner = line.strip_prefix("export {")?.strip_suffix("};")?;
    Some(
        inner
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect(),
    )
}

/// One module as a classic script: the body, then the names on `window.__shared`, in an IIFE.
fn inline_one(name: &str, source: &str) -> String {
    let names = exported_names(source).unwrap_or_default();
    let body_end = source.trim_end().rfind('\n').unwrap_or(0);
    let body = &source[..body_end];
    format!(
        "/* shared: {name} */\n(function () {{\n{body}\nwindow.__shared = window.__shared || {{}};\nObject.assign(window.__shared, {{ {} }});\n}})();\n",
        names.join(", ")
    )
}

/// Every shared module inlined, in [`SHARED`] order — the block the page carries before
/// `export.js`.
pub fn inline_all() -> String {
    SHARED
        .iter()
        .map(|(name, source)| inline_one(name, source))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The convention the inliner relies on, held for every shared module: no imports, one
    /// trailing export line that names something, nothing else exported.
    #[test]
    fn every_shared_module_keeps_the_convention() {
        for (name, source) in SHARED {
            let names = exported_names(source)
                .unwrap_or_else(|| panic!("{name}: no trailing `export {{ … }};` line"));
            assert!(!names.is_empty(), "{name}: the export line names nothing");
            for line in source.lines() {
                let t = line.trim_start();
                assert!(
                    !t.starts_with("import "),
                    "{name}: shared modules import nothing: {line}"
                );
            }
            let exports = source
                .lines()
                .filter(|l| l.trim_start().starts_with("export "))
                .count();
            assert_eq!(
                exports, 1,
                "{name}: exactly one export line, the trailing one"
            );
        }
    }

    /// The inlined block is a classic script — no module syntax survives — and publishes every
    /// name the module exported, so export.js can reach them on `window.__shared`.
    #[test]
    fn the_inlined_block_is_a_classic_script_that_publishes_every_name() {
        let block = inline_all();
        for line in block.lines() {
            let t = line.trim_start();
            assert!(
                !t.starts_with("export ") && !t.starts_with("import "),
                "module syntax leaked: {line}"
            );
        }
        for (name, source) in SHARED {
            assert!(block.contains(&format!("/* shared: {name} */")));
            for n in exported_names(source).unwrap() {
                assert!(
                    block.contains("Object.assign(window.__shared, { ") && block.contains(n),
                    "{name}: {n} is not published"
                );
            }
        }
        assert!(block.contains("window.__shared = window.__shared || {};"));
    }

    #[test]
    fn inline_one_is_exact() {
        let out = inline_one(
            "demo",
            "function a() { return 1; }\nconst b = 2;\n\nexport { a, b };\n",
        );
        assert_eq!(
            out,
            "/* shared: demo */\n(function () {\nfunction a() { return 1; }\nconst b = 2;\n\nwindow.__shared = window.__shared || {};\nObject.assign(window.__shared, { a, b });\n})();\n"
        );
    }
}

//! The **TUI** frontend: the ratatui viewer (`app` terminal + input, `view` state machine
//! and draw), its rendering pipeline (`render` blocks to lines, plus `markdown`, `wrap`,
//! `theme`), the fuzzy session `picker`, and OS `clipboard` access. Everything
//! terminal-specific lives here; the shared TEXT formatters live in `crate::present`, the
//! agent-neutral diff-row model in `crate::diff`, and the (TUI+HTML) syntax highlighter in
//! `crate::highlight`.

pub mod app;
pub mod view;

mod clipboard;
mod markdown;
mod picker;
mod render;
mod theme;
mod wrap;

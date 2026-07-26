//! The viewer's state machine + drawing, decoupled from the terminal so it can
//! be driven headless (ratatui `TestBackend`) without a real TTY.

use crate::discover::Candidate;
use crate::model::{Attachment, AttachmentContent, Block};
use crate::picker::Picker;
use crate::{render, theme, wrap, Args};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as WBlock, Borders, Clear, Paragraph};
use ratatui::Frame;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Columns of uncolored left margin before a diff/code row's gutter — matches
/// Claude Code's 6-space indent. `pub(crate)` so `render` can indent context rows
/// by the same amount that `fill_bg` insets the highlighted (+/−) rows, keeping
/// every gutter aligned.
pub(crate) const INSET: usize = 6;

/// Extend a line whose spans carry a background, so diff/user/shell/thinking rows
/// read as solid blocks (ratatui won't fill bg past the text otherwise). Lines
/// without a trailing background are left untouched.
///
/// Diff add/del rows are **inset** `INSET` columns on each side (uncolored
/// margin); every other background block (user / thinking / expanded shell) fills
/// the full row width.
/// The footer location segment's shed priority — never dropped, only truncated last.
const LOC_PRIO: u8 = 100;

/// Fit-and-shed the footer's LEFT run to `avail` columns: drop the highest-priority
/// (largest number) droppable segment (`1..LOC_PRIO`) first, repeat until it fits;
/// priority-0 segments (nav labels, live-state, position) never drop; the location
/// (`LOC_PRIO`) is truncated with `…` only as a last resort. Returns the survivors, in
/// order. Segment shed order matches the spec: cached(1) → %(2) → model(3) → in(4) →
/// out(5) → duration(6) → cost(7).
fn shed_footer(mut segs: Vec<(String, u8)>, avail: usize) -> Vec<(String, u8)> {
    let joined = |segs: &[(String, u8)]| -> usize {
        segs.iter().map(|(t, _)| t.chars().count()).sum::<usize>()
            + segs.len().saturating_sub(1) * 3
    };
    while joined(&segs) > avail {
        // Drop the LEAST-important droppable first — spec order cached(1) → %(2) →
        // model(3) → in(4) → out(5) → duration(6) → cost(7).
        let victim = segs
            .iter()
            .enumerate()
            .filter(|(_, (_, p))| (1..LOC_PRIO).contains(p))
            .min_by_key(|(_, (_, p))| *p)
            .map(|(i, _)| i);
        match victim {
            Some(i) => {
                segs.remove(i);
            }
            None => break, // only priority-0 + location remain
        }
    }
    if joined(&segs) > avail {
        let over = joined(&segs) - avail;
        if let Some((loc, _)) = segs.iter_mut().find(|(_, p)| *p == LOC_PRIO) {
            let keep = loc.chars().count().saturating_sub(over + 1);
            *loc = if keep == 0 {
                String::new()
            } else {
                loc.chars().take(keep).chain(['…']).collect()
            };
        }
    }
    segs
}

fn fill_bg(mut line: Line<'static>, width: usize, inset: bool) -> Line<'static> {
    let Some(bg) = line.spans.last().and_then(|s| s.style.bg) else {
        return line;
    };
    let (left, right) = if inset { (INSET, INSET) } else { (0, 0) };

    // Uncolored left margin shifts the colored band rightward.
    if left > 0 {
        line.spans.insert(0, Span::raw(" ".repeat(left)));
    }
    let used: usize = line
        .spans
        .iter()
        .map(|s| UnicodeWidthStr::width(s.content.as_ref()))
        .sum();
    let target = width.saturating_sub(right);
    if used < target {
        line.spans.push(Span::styled(
            " ".repeat(target - used),
            Style::default().bg(bg),
        ));
    }
    line
}

/// When a foldable block is focused (via `[`/`]`/hover), highlight it. Fold-header
/// summaries brighten (swap the resting fold-header color for the focused one); and
/// the block's *header* row gets a background bar — a universal cue that shows for
/// every block type, including ones whose header isn't fold-header-colored (Edit's
/// `⏺ Edit`/`└ Updated`, `⎿` results, command headers). `fill_bg` extends the bar
/// to the full row width.
fn focus_recolor(line: Line<'static>, focused: bool, header: bool) -> Line<'static> {
    if !focused {
        return line;
    }
    let bar = header.then(theme::focus_bg);
    let spans: Vec<Span<'static>> = line
        .spans
        .into_iter()
        .map(|mut s| {
            if s.style.fg == Some(theme::fold_header()) {
                s.style = s.style.fg(theme::fold_header_focused());
            }
            if let Some(bg) = bar {
                s.style = s.style.bg(bg);
            }
            s
        })
        .collect();
    Line::from(spans)
}

/// The canonical block-type keys (see `model::fold_key`).
const FOLD_KEYS: &[&str] = &[
    "user",
    "assistant",
    "thinking",
    "read",
    "bash",
    "edit",
    "write",
    "tool",
    "skill",
    "agent",
    "tool_result",
    "command",
];

/// Map a user-typed key to its canonical `&'static str` (accepts a few aliases).
fn canon_key(k: &str) -> Option<&'static str> {
    let k = k.trim().to_lowercase();
    let k = match k.as_str() {
        "result" | "results" | "toolresult" => "tool_result",
        "reads" => "read",
        "edits" => "edit",
        "writes" => "write",
        "think" => "thinking",
        other => other,
    };
    FOLD_KEYS.iter().copied().find(|c| *c == k)
}

fn parse_keys(csv: Option<&str>) -> Vec<&'static str> {
    csv.into_iter()
        .flat_map(|s| s.split(','))
        .filter_map(canon_key)
        .collect()
}

/// Which block types start collapsed. Defaults mirror Claude Code (thinking,
/// tool_result, and reads folded); `--fold`/`--unfold` adjust per type
/// (`--unfold` wins), and `--full` unfolds everything.
#[derive(Clone)]
pub struct FoldPolicy {
    folded: HashSet<&'static str>,
}

impl Default for FoldPolicy {
    fn default() -> Self {
        // Claude-Code-like: thinking, shell, reads, writes, and other tool calls
        // collapse; user/assistant/edit stay expanded.
        Self {
            folded: [
                "thinking",
                "tool_result",
                "read",
                "bash",
                "tool",
                "skill",
                "agent",
                "command",
                "write",
            ]
            .into_iter()
            .collect(),
        }
    }
}

impl FoldPolicy {
    /// A policy that folds nothing (everything expanded).
    pub fn none() -> Self {
        Self {
            folded: HashSet::new(),
        }
    }

    pub fn from_args(args: &Args) -> Self {
        let mut p = if args.full {
            Self::none()
        } else {
            Self::default()
        };
        for k in parse_keys(args.fold.as_deref()) {
            p.folded.insert(k);
        }
        for k in parse_keys(args.unfold.as_deref()) {
            p.folded.remove(k); // --unfold wins over --fold and the defaults
        }
        p
    }

    /// Does this policy start `b` collapsed? Also drives the HTML export's
    /// `data-open`, so a dump and an export fold identically.
    pub fn collapses(&self, b: &Block) -> bool {
        self.folded.contains(crate::model::fold_key(b))
    }

    /// Initial per-block fold state for a block list under this policy.
    fn collapsed_for(&self, blocks: &[Block]) -> Vec<bool> {
        blocks.iter().map(|b| self.collapses(b)).collect()
    }
}

/// Does this line carry a diff add/del background (i.e. needs the `INSET` margin)?
/// Computed from the *original* line, before search/focus recolor overwrites the
/// bg — otherwise a matched diff row would lose its inset and shift left.
fn is_diff_line(line: &Line<'static>) -> bool {
    matches!(
        line.spans.last().and_then(|s| s.style.bg),
        Some(c) if c == theme::diff_add_bg() || c == theme::diff_del_bg()
    )
}

/// Apply a search-highlight background to every span of a matching line.
fn highlight_bg(line: &Line<'static>, strong: bool) -> Line<'static> {
    let (bg, fg) = if strong {
        (Color::Yellow, Some(Color::Black))
    } else {
        (Color::Rgb(70, 70, 0), None)
    };
    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .map(|s| {
            let mut style = s.style.bg(bg);
            if let Some(f) = fg {
                style = style.fg(f);
            }
            Span::styled(s.content.clone(), style)
        })
        .collect();
    Line::from(spans)
}

/// Recolor the background of display columns `[c0, c1)` of `line` to the selection
/// colour (`c1 == usize::MAX` means "to end of line"). Splits spans at column
/// boundaries so a partial-line selection highlights exactly the dragged cells.
fn apply_selection(line: Line<'static>, c0: usize, c1: usize) -> Line<'static> {
    if c0 >= c1 {
        return line;
    }
    let sel = theme::selection_bg();
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut col = 0usize;
    for span in line.spans {
        let style = span.style;
        let (mut buf, mut buf_sel) = (String::new(), false);
        for ch in span.content.chars() {
            let in_sel = col >= c0 && col < c1;
            if !buf.is_empty() && in_sel != buf_sel {
                let st = if buf_sel { style.bg(sel) } else { style };
                out.push(Span::styled(std::mem::take(&mut buf), st));
            }
            buf_sel = in_sel;
            buf.push(ch);
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
        if !buf.is_empty() {
            let st = if buf_sel { style.bg(sel) } else { style };
            out.push(Span::styled(buf, st));
        }
    }
    Line::from(out)
}

/// The plain text of `line`'s display columns `[c0, c1)` (`c1 == usize::MAX` → EOL).
fn cols_of_line(line: &Line<'static>, c0: usize, c1: usize) -> String {
    let mut s = String::new();
    let mut col = 0usize;
    for span in &line.spans {
        for ch in span.content.chars() {
            if col >= c0 && col < c1 {
                s.push(ch);
            }
            col += UnicodeWidthChar::width(ch).unwrap_or(0);
        }
    }
    s
}

/// What activating the focused block (Enter, or a click) asks the caller to do — the
/// action can't complete inside `View` (revealing a path is an OS call; descending
/// into a sub-agent pushes a new `View` on the app's stack).
/// Which clickable footer label a footer-row click landed on.
#[derive(Debug, PartialEq, Eq)]
pub enum FooterHit {
    EscBack,
    ActiveAgents,
    None,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    /// Reveal this path in the OS file manager (a tool-header path / path-only attachment).
    Reveal(PathBuf),
    /// Descend into the `SubAgent` at this block index (open its child transcript).
    Descend(usize),
}

/// A resolved descend target — the agent to open, plus any pre-loaded child transcript
/// (a spawn carries it; a completion event loads it lazily). Built by [`View::descend_ref_at`].
pub struct DescendRef {
    pub agent_id: String,
    pub agent_type: String,
    pub blocks: Vec<Block>,
    pub subtree_cost: Option<f64>,
}

/// The outcome of a mouse click while the `a` active-sub-agents popup is open.
#[derive(Debug, PartialEq, Eq)]
pub enum PopupClick {
    /// Clicked an agent row — descend into the sub-agent at this block index.
    Descend(usize),
    /// Clicked elsewhere on the overlay — swallowed, popup stays open (`Esc`/`a` closes).
    Border,
    /// No popup was open.
    None,
}

pub struct View {
    blocks: Vec<Block>,
    collapsed: Vec<bool>,        // per-block fold state
    raw: Vec<Line<'static>>,     // unwrapped styled lines (width-aware: tables)
    raw_tag: Vec<usize>,         // raw[i] belongs to block raw_tag[i]
    raw_dirty: bool,             // raw needs rebuilding (fold/ingest/reset)
    wrapped: Vec<Line<'static>>, // wrapped to `width`
    wrapped_tag: Vec<usize>,     // wrapped[i] belongs to block wrapped_tag[i]
    width: u16,
    view_h: usize, // content rows (area height - 1 status row)
    scroll: usize, // top wrapped-line index
    follow: bool,  // pinned to bottom
    new_count: usize,
    title: String,
    live: bool,
    // search (P6)
    query: String,                  // current needle (empty = no search)
    searching: bool,                // in `/` input mode
    matches: Vec<usize>,            // wrapped-line indices containing the needle
    match_pos: usize,               // index into `matches`
    metrics: String,                // footer text (tokens/cost/duration/model) — legacy string
    footer_segs: Vec<(String, u8)>, // droppable footer metric parts (text, shed priority)
    descended: bool,                // this view is a descended sub-agent (footer shows `esc back`)
    fold: FoldPolicy,               // per-type default fold policy (applied to new content)
    focus: Option<usize>,           // focused foldable block index ([ / ] / hover)
    show_help: bool,                // `?` help overlay visible
    agents_popup: Option<usize>,    // `a` active-sub-agents popup: selected row, when open
    can_go_back: bool,              // launched via the picker → Esc returns to the session list
    can_open_picker: bool,          // `s` opens the session switcher overlay (--latest launch)
    switcher: Option<Picker>,       // session switcher overlay, when open
    // mouse text selection (wrapped-line coords, so it survives scrolling):
    sel_anchor: Option<(usize, usize)>, // (wrapped line, display col) where drag began
    sel_cursor: Option<(usize, usize)>, // current drag end; None until the mouse moves
    // Per-block rendered-body cache (keyed by collapsed state), so a fold toggle
    // re-renders only the toggled block instead of re-highlighting the whole doc.
    body_cache: Vec<Option<(bool, Vec<Line<'static>>)>>,
    cache_width: Option<u16>, // width the cache was built at; a change invalidates it
    cwd: Option<PathBuf>,     // session working dir — reverses a header's relativized path
    flash: Option<String>,    // transient status (e.g. "Saved to …"); cleared on next input
}

impl View {
    pub fn new(blocks: Vec<Block>, title: impl Into<String>, live: bool, fold: FoldPolicy) -> Self {
        let collapsed = fold.collapsed_for(&blocks);
        // Raw is built lazily on the first `layout`, once the real terminal width
        // is known — rendering here (at width 0) would be thrown away and re-done,
        // doubling the (expensive) syntax-highlight pass at startup.
        Self {
            blocks,
            collapsed,
            raw: Vec::new(),
            raw_tag: Vec::new(),
            raw_dirty: true,
            wrapped: Vec::new(),
            wrapped_tag: Vec::new(),
            width: 0,
            view_h: 0,
            scroll: 0,
            follow: true,
            new_count: 0,
            title: title.into(),
            live,
            query: String::new(),
            searching: false,
            matches: Vec::new(),
            match_pos: 0,
            metrics: String::new(),
            footer_segs: Vec::new(),
            descended: false,
            fold,
            focus: None,
            show_help: false,
            agents_popup: None,
            can_go_back: false,
            can_open_picker: false,
            switcher: None,
            sel_anchor: None,
            sel_cursor: None,
            body_cache: Vec::new(),
            cache_width: None,
            cwd: None,
            flash: None,
        }
    }

    /// Set the footer metrics text (tokens/cost/duration/model).
    pub fn set_metrics(&mut self, m: String) {
        self.metrics = m;
    }
    /// The droppable footer metric parts (from `Metrics::footer_segments`), for the
    /// fit-and-shed footer.
    pub fn set_footer_segments(&mut self, segs: Vec<(String, u8)>) {
        self.footer_segs = segs;
    }
    /// Mark this view as a descended sub-agent, so the footer offers `↑ esc back`.
    pub fn set_descended(&mut self, d: bool) {
        self.descended = d;
    }
    /// Direct sub-agents of THIS node that are still running (spawned, no terminal
    /// status) — gates the `a active N` footer label and the `a` popup, node-scoped.
    pub fn active_children(&self) -> usize {
        self.active_agent_indices().len()
    }
    /// Block indices of THIS node's direct sub-agents that are still running (spawned,
    /// no terminal status) — the `a` popup's rows, node-scoped.
    fn active_agent_indices(&self) -> Vec<usize> {
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| matches!(b, Block::SubAgent(sa) if !sa.status.is_terminal()))
            .map(|(i, _)| i)
            .collect()
    }
    /// `a` is offered only when this node has a running child (gated exactly like `s` on
    /// `can_open_picker`), so a finished replay never shows a dead affordance.
    pub fn can_open_agents(&self) -> bool {
        !self.active_agent_indices().is_empty()
    }
    pub fn open_agents_popup(&mut self) {
        if self.can_open_agents() {
            self.agents_popup = Some(0);
        }
    }
    pub fn agents_popup_open(&self) -> bool {
        self.agents_popup.is_some()
    }
    pub fn agents_popup_close(&mut self) {
        self.agents_popup = None;
    }
    pub fn agents_popup_move(&mut self, dir: isize) {
        let n = self.active_agent_indices().len();
        if let (Some(sel), true) = (self.agents_popup, n > 0) {
            self.agents_popup = Some((sel as isize + dir).rem_euclid(n as isize) as usize);
        }
    }
    /// Confirm the popup selection: close it and return the selected active agent's block
    /// index (for the caller to `Descend` into).
    pub fn agents_popup_confirm(&mut self) -> Option<usize> {
        let sel = self.agents_popup.take()?;
        self.active_agent_indices().get(sel).copied()
    }
    /// Hit-test a mouse click against the open `a` popup (a full-content-area overlay:
    /// header at row 0, one agent per row from row 1, footer at the bottom — mirrors
    /// [`render_agents_popup`]). A click on an agent row selects + descends into it; any
    /// other click is swallowed so it never leaks to the content underneath (`Esc`/`a`
    /// closes the popup).
    pub fn agents_popup_click(&mut self, row: u16) -> PopupClick {
        if self.agents_popup.is_none() {
            return PopupClick::None;
        }
        let idxs = self.active_agent_indices();
        let r = row as usize;
        if r >= 1 && r - 1 < idxs.len() {
            let i = r - 1;
            self.agents_popup = Some(i); // reflect the click in the highlight
            return PopupClick::Descend(idxs[i]);
        }
        PopupClick::Border
    }

    /// Record the session's working directory, so a click on a tool header's path
    /// can reverse its relativized display (`~/…`, `src/…`) to an absolute path.
    pub fn set_cwd(&mut self, cwd: Option<PathBuf>) {
        self.cwd = cwd;
    }
    /// This view's session cwd — a descended child inherits its parent's, so a tool
    /// path click still resolves against the real working dir.
    pub fn cwd_ref(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    /// Mark that this viewer was reached through the session picker, so `Esc`
    /// returns to the list (rather than quitting) and the help reflects that.
    pub fn set_can_go_back(&mut self, v: bool) {
        self.can_go_back = v;
    }

    /// Enable the `s` session-switcher overlay (used on a `--latest` launch, where
    /// `Esc` can't return to a list because none was shown).
    pub fn set_can_open_picker(&mut self, v: bool) {
        self.can_open_picker = v;
    }
    /// Whether `s` should open the switcher (also gates the help line).
    pub fn can_open_picker(&self) -> bool {
        self.can_open_picker
    }

    /// Open the session-switcher overlay over the current view (built from `cands`).
    pub fn open_switcher(&mut self, cands: Vec<Candidate>) {
        self.switcher = Some(Picker::new(cands));
    }
    /// Is the switcher overlay currently open?
    pub fn is_switcher_open(&self) -> bool {
        self.switcher.is_some()
    }
    /// Close the switcher without switching (keeps the current session/position).
    pub fn switcher_close(&mut self) {
        self.switcher = None;
    }
    pub fn switcher_up(&mut self) {
        if let Some(p) = self.switcher.as_mut() {
            p.up();
        }
    }
    pub fn switcher_down(&mut self) {
        if let Some(p) = self.switcher.as_mut() {
            p.down();
        }
    }
    pub fn switcher_input(&mut self, c: char) {
        if let Some(p) = self.switcher.as_mut() {
            p.push_char(c);
        }
    }
    pub fn switcher_backspace(&mut self) {
        if let Some(p) = self.switcher.as_mut() {
            p.backspace();
        }
    }
    /// Confirm the switcher's selection: close the overlay and return the chosen
    /// transcript path (None if there was no selection).
    pub fn switcher_confirm(&mut self) -> Option<PathBuf> {
        let path = self.switcher.as_ref().and_then(|p| p.selected_path());
        self.switcher = None;
        path
    }

    /// Re-render the raw (unwrapped) lines at the current width. Each block's body
    /// is cached (keyed by its collapsed state); a fold toggle only re-renders the
    /// block(s) whose state flipped, reusing every other cached body — so a toggle
    /// is O(one block) of syntax-highlighting instead of the whole document. A width
    /// change invalidates the cache (bodies are width-aware for tables). Appended
    /// blocks (live tail) render fresh; the rest of the cache is preserved.
    fn render_raw(&mut self) {
        if self.cache_width != Some(self.width) {
            self.body_cache.clear();
            self.cache_width = Some(self.width);
        }
        self.body_cache.resize(self.blocks.len(), None);
        let width = self.width as usize;
        let mut bodies: Vec<Vec<Line<'static>>> = Vec::with_capacity(self.blocks.len());
        for (i, b) in self.blocks.iter().enumerate() {
            let is_collapsed =
                self.collapsed.get(i).copied().unwrap_or(false) && render::foldable(b);
            let body = match &self.body_cache[i] {
                Some((cached, body)) if *cached == is_collapsed => body.clone(),
                _ => {
                    let body = render::block_body(b, is_collapsed, width);
                    self.body_cache[i] = Some((is_collapsed, body.clone()));
                    body
                }
            };
            bodies.push(body);
        }
        let r = render::assemble(bodies);
        self.raw = r.lines;
        self.raw_tag = r.block_of;
    }

    /// Mark raw stale so the next `layout` rebuilds and re-wraps it.
    fn rebuild_raw(&mut self) {
        self.raw_dirty = true;
        self.invalidate_wrap();
    }

    // --- inspection accessors (used by the headless TestBackend tests) ---
    #[cfg(test)]
    pub fn follow(&self) -> bool {
        self.follow
    }
    #[cfg(test)]
    pub fn new_count(&self) -> usize {
        self.new_count
    }
    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }
    #[cfg(test)]
    pub fn total_lines(&self) -> usize {
        self.wrapped.len()
    }
    #[cfg(test)]
    pub fn view_h(&self) -> usize {
        self.view_h
    }
    #[cfg(test)]
    pub fn is_collapsed(&self, i: usize) -> bool {
        self.collapsed[i]
    }
    /// The fold-key of every top-level block (for asserting live-tail grouping).
    #[cfg(test)]
    pub fn block_kinds(&self) -> Vec<&'static str> {
        self.blocks.iter().map(crate::model::fold_key).collect()
    }
    /// The source-block index that wrapped line `line` was rendered from.
    #[cfg(test)]
    pub fn block_of_line(&self, line: usize) -> Option<usize> {
        self.wrapped_tag.get(line).copied()
    }

    fn max_scroll(&self) -> usize {
        self.wrapped.len().saturating_sub(self.view_h)
    }

    /// Content rows (excludes the status row) — for mouse-click hit-testing.
    pub fn content_rows(&self) -> usize {
        self.view_h
    }

    /// Force a re-wrap on the next layout (after a resize or new content).
    pub fn invalidate_wrap(&mut self) {
        self.width = 0;
    }

    /// Compute geometry for a given area (call before scroll math in handlers).
    pub fn layout(&mut self, width: u16, height: u16) {
        self.view_h = height.saturating_sub(1) as usize;
        let width_changed = width != self.width;
        if width_changed {
            self.width = width;
        }
        // Rebuild raw on a width change (width-aware tables) or stale content,
        // then re-wrap. `width == 0` is the wrap-invalidation sentinel.
        if width_changed || self.raw_dirty {
            self.render_raw();
            self.raw_dirty = false;
            let (w, t) = wrap::wrap_all_tagged(&self.raw, &self.raw_tag, width as usize);
            self.wrapped = w;
            self.wrapped_tag = t;
            // Match indices are into `wrapped`, so only recompute when it was rebuilt.
            // A query change recomputes directly (search_input/backspace); a plain
            // scroll rebuilds nothing, so it no longer rescans every line.
            self.recompute_matches();
        }
        let max = self.max_scroll();
        if self.follow {
            self.scroll = max;
        }
        self.scroll = self.scroll.min(max);
    }

    pub fn scroll_by(&mut self, delta: isize) {
        let max = self.max_scroll();
        self.scroll = if delta >= 0 {
            (self.scroll + delta as usize).min(max)
        } else {
            self.scroll.saturating_sub((-delta) as usize)
        };
        self.follow = self.scroll >= max;
        if self.follow {
            self.new_count = 0;
        }
    }
    pub fn half_page(&mut self, down: bool) {
        let d = (self.view_h / 2).max(1) as isize;
        self.scroll_by(if down { d } else { -d });
    }
    pub fn full_page(&mut self, down: bool) {
        let d = self.view_h.max(1) as isize;
        self.scroll_by(if down { d } else { -d });
    }
    pub fn jump_top(&mut self) {
        self.scroll = 0;
        self.follow = false;
    }
    pub fn jump_bottom(&mut self) {
        self.scroll = self.max_scroll();
        self.follow = true;
        self.new_count = 0;
    }

    /// Toggle the `?` help overlay.
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }
    /// Is the `?` help overlay currently shown?
    pub fn is_help_open(&self) -> bool {
        self.show_help
    }

    // --- fold / expand (P4) ---
    /// Toggle the collapse state of a foldable block by index.
    pub fn toggle_block(&mut self, i: usize) {
        if self.blocks.get(i).map(render::foldable).unwrap_or(false) {
            if let Some(c) = self.collapsed.get_mut(i) {
                *c = !*c;
            }
            self.rebuild_raw();
        }
    }
    /// Toggle the block at the top of the viewport (the `t` key).
    pub fn toggle_at_cursor(&mut self) {
        // Prefer the focused foldable (set by `[`/`]`/hover); otherwise the first
        // foldable block visible in the viewport. (The block exactly at the top line
        // is usually non-foldable — e.g. assistant text — which made `t` look dead.)
        if let Some(f) = self
            .focus
            .filter(|&i| self.blocks.get(i).map(render::foldable).unwrap_or(false))
        {
            self.toggle_block(f);
            return;
        }
        let end = (self.scroll + self.view_h.max(1)).min(self.wrapped_tag.len());
        if let Some(&b) = self.wrapped_tag[self.scroll.min(self.wrapped_tag.len())..end]
            .iter()
            .find(|&&b| self.blocks.get(b).map(render::foldable).unwrap_or(false))
        {
            self.toggle_block(b);
        }
    }
    /// Collapse all foldable blocks, or expand all if any are already collapsed
    /// (the `T` key).
    pub fn toggle_all(&mut self) {
        let any_expanded = self
            .blocks
            .iter()
            .enumerate()
            .any(|(i, b)| render::foldable(b) && !self.collapsed[i]);
        for i in 0..self.blocks.len() {
            if render::foldable(&self.blocks[i]) {
                self.collapsed[i] = any_expanded;
            }
        }
        self.rebuild_raw();
    }
    /// Mouse click at a content cell (0-based row/col from the top-left of the
    /// content area). A click on the **path** in a tool header (`⏺ Write(<path>)`)
    /// returns that path (absolute) for the caller to reveal in the OS file
    /// manager; a click anywhere else in a foldable block toggles its fold and
    /// returns `None`.
    pub fn click_at(&mut self, row: u16, col: u16) -> Option<Action> {
        let idx = self.scroll + row as usize;
        let &b = self.wrapped_tag.get(idx)?;
        self.focus = Some(b);
        // A control inside a header owns its click and must NOT also fold the block (a
        // branch already run can't be undone by `stopPropagation`): a tool-header path
        // reveals its file; a spawn's agent-id descends into the child.
        if let Some(path) = self.header_path_hit(b, idx, col as usize) {
            return Some(Action::Reveal(path));
        }
        if self.agent_id_hit(b, idx, col as usize) {
            return Some(Action::Descend(b));
        }
        // A mouse click elsewhere on a spawn / completion header just FOLDS it (only the
        // agent id descends); other blocks do their normal activation.
        if matches!(
            self.blocks.get(b),
            Some(Block::SubAgent(_) | Block::AgentDone { .. })
        ) {
            self.toggle_block(b);
            return None;
        }
        self.activate_block(b)
    }

    /// Is the click at `(idx, col)` on a spawn / completion header's descend-target agent
    /// id? Works for both the `SubAgent` spawn and the `AgentDone` completion (any status).
    fn agent_id_hit(&self, b: usize, idx: usize, col: usize) -> bool {
        // Only the header's own first row carries the id.
        if idx != 0 && self.wrapped_tag.get(idx - 1) == Some(&b) {
            return false;
        }
        let span = match self.blocks.get(b) {
            Some(Block::SubAgent(sa)) => render::agent_id_span(sa),
            Some(Block::AgentDone {
                agent_id,
                agent_type,
                description,
                status,
                ..
            }) => render::agent_done_id_span(agent_type, description, *status, agent_id),
            _ => return false,
        };
        matches!(span, Some((s, e)) if col >= s && col < e)
    }

    /// The KEYBOARD (`Enter`) action for block `b`. A [`Block::SubAgent`] with a loaded
    /// child **descends** directly (Space still folds it); an [`Attachment`] downloads
    /// (flashed) or reveals its path; any other block toggles its fold.
    fn activate_block(&mut self, b: usize) -> Option<Action> {
        match self.blocks.get(b) {
            Some(Block::SubAgent(sa)) => {
                if sa.agent_id.is_empty() {
                    self.toggle_block(b); // no id ⇒ nothing to descend into → just fold
                    None
                } else {
                    // Descend even when `blocks` is empty — a running agent's child
                    // transcript loads lazily at descend time (from its own file).
                    Some(Action::Descend(b))
                }
            }
            // A completion event descends into the finished agent's transcript (any
            // terminal status), loaded lazily from its file at descend time.
            Some(Block::AgentDone { agent_id, .. }) => {
                if agent_id.is_empty() {
                    self.toggle_block(b);
                    None
                } else {
                    Some(Action::Descend(b))
                }
            }
            Some(Block::Attachment(a)) => {
                let a = a.clone();
                if a.content.is_some() {
                    self.flash = Some(match save_attachment(&a) {
                        Ok(p) => format!("Saved {}", pretty_home(&p)),
                        Err(e) => format!("Download failed: {e}"),
                    });
                    None
                } else {
                    a.path.map(|p| Action::Reveal(PathBuf::from(p)))
                }
            }
            _ => {
                self.toggle_block(b);
                None
            }
        }
    }

    /// The `SubAgent` at block index `b` — the caller descends using its `blocks`.
    pub fn subagent_at(&self, b: usize) -> Option<&crate::model::SubAgent> {
        match self.blocks.get(b) {
            Some(Block::SubAgent(sa)) => Some(sa),
            _ => None,
        }
    }

    /// The descend target at block index `b` — a `SubAgent` spawn (with any pre-loaded
    /// child `blocks`) OR an `AgentDone` completion (whose child always loads lazily from
    /// its file). `None` for any other block or a missing agent id.
    pub fn descend_ref_at(&self, b: usize) -> Option<DescendRef> {
        match self.blocks.get(b)? {
            Block::SubAgent(sa) if !sa.agent_id.is_empty() => Some(DescendRef {
                agent_id: sa.agent_id.clone(),
                agent_type: sa.agent_type.clone(),
                blocks: sa.blocks.clone(),
                subtree_cost: sa.subtree_cost,
            }),
            Block::AgentDone {
                agent_id,
                agent_type,
                ..
            } if !agent_id.is_empty() => Some(DescendRef {
                agent_id: agent_id.clone(),
                agent_type: agent_type.clone(),
                blocks: Vec::new(), // no inline child on a completion event → lazy-load
                subtree_cost: None,
            }),
            _ => None,
        }
    }

    /// Land the cursor on block `b` (the spawn we returned from) WITHOUT changing any
    /// fold state — the return-from-descend focus restore (§2.2). Scrolls it into view.
    pub fn focus_block(&mut self, b: usize) {
        if b < self.blocks.len() {
            self.focus = Some(b);
            self.scroll_block_into_view(b);
        }
    }

    /// The absolute file path a click at `(idx, col)` lands on, if `idx` is the
    /// first (header) row of a tool block and `col` falls within its `(target)`
    /// span — and the resolved path actually exists (so a `Bash(ls)` command or a
    /// `Grep(pattern)` header never masquerades as a file). Else `None`.
    fn header_path_hit(&self, b: usize, idx: usize, col: usize) -> Option<PathBuf> {
        // Only the header's own first row carries the path.
        if idx != 0 && self.wrapped_tag.get(idx - 1) == Some(&b) {
            return None;
        }
        let Block::ToolUse { name, target, .. } = self.blocks.get(b)? else {
            return None;
        };
        if target.is_empty() {
            return None;
        }
        let (start, end) = render::tool_header_target_span(name, target);
        if col < start || col >= end {
            return None;
        }
        let abs = self.resolve_target_path(target);
        abs.exists().then_some(abs)
    }

    /// Reverse a header's relativized `target` (`~/…` → `$HOME/…`, a bare relative
    /// path → under the session cwd, an absolute path unchanged) to a real path.
    fn resolve_target_path(&self, target: &str) -> PathBuf {
        if let Some(rest) = target.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                return PathBuf::from(home).join(rest);
            }
        }
        let p = PathBuf::from(target);
        if p.is_absolute() {
            return p;
        }
        match &self.cwd {
            Some(cwd) => cwd.join(target),
            None => p,
        }
    }

    // --- expandable-element focus ([ / ] / hover / Enter) ---
    /// Block indices the `[`/`]` keys can focus: foldable blocks plus attachments
    /// (which aren't foldable but are actionable via Enter — download/reveal).
    fn focusable_blocks(&self) -> Vec<usize> {
        (0..self.blocks.len())
            .filter(|&i| {
                render::foldable(&self.blocks[i]) || matches!(self.blocks[i], Block::Attachment(_))
            })
            .collect()
    }
    /// Move focus to the next (`]`) / previous (`[`) foldable block, wrapping,
    /// and scroll it into view.
    pub fn focus_next(&mut self) {
        self.move_focus(1);
    }
    pub fn focus_prev(&mut self) {
        self.move_focus(-1);
    }
    fn move_focus(&mut self, dir: isize) {
        let fold = self.focusable_blocks();
        if fold.is_empty() {
            return;
        }
        let cur = self.focus.and_then(|f| fold.iter().position(|&b| b == f));
        let pos = match cur {
            Some(p) => (p as isize + dir).rem_euclid(fold.len() as isize) as usize,
            None if dir > 0 => 0,
            None => fold.len() - 1,
        };
        let b = fold[pos];
        self.focus = Some(b);
        self.scroll_block_into_view(b);
    }
    fn scroll_block_into_view(&mut self, b: usize) {
        if let Some(idx) = self.wrapped_tag.iter().position(|&t| t == b) {
            if idx < self.scroll {
                self.scroll = idx;
                self.follow = false;
            } else if self.view_h > 0 && idx >= self.scroll + self.view_h {
                self.scroll = idx.saturating_sub(self.view_h - 1);
                self.follow = false;
            }
        }
    }
    /// Activate the focused block (the `Enter` key): descend a sub-agent, download/reveal
    /// an attachment, or toggle a foldable block. Returns the action for the caller.
    pub fn activate_focused(&mut self) -> Option<Action> {
        match self.focus {
            Some(b) => self.activate_block(b),
            None => None,
        }
    }

    /// Clear the transient status flash (called on the next input so a "Saved …"
    /// message doesn't linger).
    pub fn clear_flash(&mut self) {
        self.flash = None;
    }

    /// Which footer nav label a click at column `col` on the footer row lands on — so a
    /// click on `↑ esc back` ascends and one on `a active N` opens the popup, matching
    /// the label order `status_line` builds.
    pub fn footer_click(&self, col: usize) -> FooterHit {
        let mut c = 1usize; // leading space
        if self.descended {
            let w = "↑ esc back".chars().count();
            if col >= c && col < c + w {
                return FooterHit::EscBack;
            }
            c += w + 3; // " · "
        }
        let active = self.active_children();
        if active > 0 {
            let w = format!("a active {active}").chars().count();
            if col >= c && col < c + w {
                return FooterHit::ActiveAgents;
            }
        }
        FooterHit::None
    }
    /// Hover: focus the foldable block under a content row (mouse move).
    pub fn hover_row(&mut self, row: u16) {
        let idx = self.scroll + row as usize;
        if let Some(&b) = self.wrapped_tag.get(idx) {
            if render::foldable(&self.blocks[b]) {
                self.focus = Some(b);
            }
        }
    }
    #[cfg(test)]
    pub fn focused_block(&self) -> Option<usize> {
        self.focus
    }

    // --- search (P6) ---
    pub fn is_searching(&self) -> bool {
        self.searching
    }
    fn recompute_matches(&mut self) {
        self.matches.clear();
        if self.query.is_empty() {
            return;
        }
        let q = self.query.to_lowercase();
        for (i, l) in self.wrapped.iter().enumerate() {
            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
            if text.to_lowercase().contains(&q) {
                self.matches.push(i);
            }
        }
        if self.match_pos >= self.matches.len() {
            self.match_pos = 0;
        }
    }
    fn jump_to_current_match(&mut self) {
        if let Some(&line) = self.matches.get(self.match_pos) {
            self.scroll = line.min(self.max_scroll());
            self.follow = false;
        }
    }
    pub fn search_start(&mut self) {
        self.searching = true;
        self.query.clear();
        self.matches.clear();
        self.match_pos = 0;
    }
    pub fn search_input(&mut self, c: char) {
        self.query.push(c);
        self.recompute_matches();
        self.jump_to_current_match();
    }
    pub fn search_backspace(&mut self) {
        self.query.pop();
        self.recompute_matches();
    }
    pub fn search_confirm(&mut self) {
        self.searching = false; // keep query + highlights
    }
    pub fn search_cancel(&mut self) {
        self.searching = false;
        self.query.clear();
        self.matches.clear();
    }
    pub fn search_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.match_pos = (self.match_pos + 1) % self.matches.len();
        self.jump_to_current_match();
    }
    pub fn search_prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.match_pos = (self.match_pos + self.matches.len() - 1) % self.matches.len();
        self.jump_to_current_match();
    }
    #[cfg(test)]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Append newly-tailed blocks; bumps the new-message count while scrolled back.
    pub fn ingest(&mut self, mut new_blocks: Vec<Block>) {
        if new_blocks.is_empty() {
            return;
        }
        // Re-group across the poll boundary. `group_turns` runs per parse batch, so
        // a thinking block whose preceding activity tools were delivered in an
        // EARLIER poll couldn't absorb them — they'd linger as separate expanded
        // blocks until a restart re-parsed the whole file. If this batch opens with
        // a thinking turn, pull the trailing run of activity tool calls off the tail
        // of the existing blocks into it, matching a full re-parse.
        if let Some(Block::Thinking { tools, .. }) = new_blocks.first_mut() {
            let mut stolen: Vec<Block> = Vec::new();
            while matches!(
                self.blocks.last(),
                Some(Block::ToolUse { name, .. }) if crate::model::is_activity_tool(name)
            ) {
                stolen.push(self.blocks.pop().unwrap());
                self.collapsed.pop();
            }
            if !stolen.is_empty() {
                // `stolen` is tail-first; restore chronological order, then append
                // the tools this batch already grouped in.
                stolen.reverse();
                stolen.extend(std::mem::take(tools));
                *tools = stolen;
                // The popped blocks vacated their cache slots — drop the stale tail
                // so `render_raw`'s positional cache doesn't serve them for the new
                // blocks that take their place.
                self.body_cache.truncate(self.blocks.len());
            }
        }
        let n = new_blocks.len();
        // Live equivalent of the batch parser's immediate-pickup suppression: a queued
        // prompt whose `⧗ queued:` marker is the current tail (nothing emitted since)
        // was picked up immediately, so the arriving `❯` turn REPLACES the marker
        // rather than following it. A full re-parse (or the HTML live path) already does
        // this; the append-based tail must do it by hand. If agent work arrived between
        // the marker and the turn, the marker is no longer the tail and stays (delayed).
        let fresh = self.fold.collapsed_for(&new_blocks);
        for (b, c) in new_blocks.into_iter().zip(fresh) {
            if let Block::UserText(t) = &b {
                // Look only at the TRAILING run of `⧗ queued:` markers (those with no
                // agent block emitted after them). A match there is an immediate pickup
                // → drop that marker so this turn replaces it. Once agent work lands, the
                // run is empty and the marker survives (a delayed pickup keeps both).
                let run_start = self
                    .blocks
                    .iter()
                    .rposition(|x| !matches!(x, Block::QueueEvent { .. }))
                    .map_or(0, |i| i + 1);
                if let Some(off) = self.blocks[run_start..]
                    .iter()
                    .position(|x| matches!(x, Block::QueueEvent { text } if text == t))
                {
                    let idx = run_start + off;
                    self.blocks.remove(idx);
                    self.collapsed.remove(idx);
                    self.body_cache.truncate(idx); // positional cache invalid from here
                }
            }
            self.collapsed.push(c);
            self.blocks.push(b);
        }
        self.rebuild_raw();
        if !self.follow {
            self.new_count += n;
        }
    }

    /// Replace all content (after a transcript truncation/rewrite).
    pub fn reset(&mut self, blocks: Vec<Block>) {
        self.collapsed = self.fold.collapsed_for(&blocks);
        self.blocks = blocks;
        self.body_cache.clear(); // indices no longer map to the old blocks
        self.rebuild_raw();
    }

    /// Incremental live update (M16): replace the blocks with a fresh `FollowParser` snapshot
    /// while PRESERVING per-block fold toggles and the render cache for the unchanged prefix
    /// — so a live tail doesn't reset the user's expands/collapses or re-render settled
    /// blocks each poll. Only the changed/appended tail (a back-patched tool block, new
    /// turns) recomputes. Replaces the old ingest/full-reparse split with one correct path.
    pub fn update(&mut self, new_blocks: Vec<Block>) {
        // Longest unchanged prefix — keep its fold state and render cache.
        let d = self
            .blocks
            .iter()
            .zip(&new_blocks)
            .take_while(|(a, b)| a == b)
            .count();
        let tail = self.fold.collapsed_for(&new_blocks[d..]);
        self.collapsed.truncate(d);
        self.collapsed.extend(tail);
        self.body_cache.truncate(d);
        self.blocks = new_blocks;
        self.rebuild_raw();
    }

    fn status_line(&self) -> Line<'static> {
        if let Some(msg) = &self.flash {
            return Line::styled(format!(" {msg} "), theme::badge());
        }
        if self.searching {
            return Line::from(vec![
                Span::styled(" /", theme::user()),
                Span::raw(self.query.clone()),
                Span::styled("   (Enter keep · Esc cancel)", theme::dim()),
            ]);
        }
        if !self.query.is_empty() {
            let cur = if self.matches.is_empty() {
                0
            } else {
                self.match_pos + 1
            };
            return Line::styled(
                format!(
                    " search '{}'  {}/{}  (n/N next/prev · Esc-then-/ to clear) ",
                    self.query,
                    cur,
                    self.matches.len()
                ),
                theme::status(),
            );
        }
        if self.new_count > 0 && !self.follow {
            return Line::from(vec![Span::styled(
                format!(" ▼ {} new — G to jump ", self.new_count),
                theme::badge(),
            )]);
        }
        let max = self.max_scroll();
        let pct = (self.scroll * 100).checked_div(max).unwrap_or(100);
        let mark = if self.follow { "[bottom]" } else { "[scroll]" };
        let pos = (self.scroll + 1).min(self.wrapped.len().max(1));
        let total = self.wrapped.len().max(1);
        // Build the LEFT run in order, each segment with a shed priority (0 = never
        // drop: the nav labels, live-state, position; LOC_PRIO = the id, truncate last).
        let mut segs: Vec<(String, u8)> = Vec::new();
        if self.descended {
            segs.push(("↑ esc back".into(), 0)); // ascend hint + (future) click target
        }
        let active = self.active_children();
        if active > 0 {
            segs.push((format!("a active {active}"), 0));
        }
        segs.push((self.title.clone(), LOC_PRIO)); // session uuid / agent id
        segs.push((
            if self.live {
                format!("{mark} · live")
            } else {
                mark.to_string()
            },
            0,
        ));
        segs.push((format!("{pos}/{total}"), 0));
        segs.push((format!("{pct}%"), 2));
        segs.extend(self.footer_segs.iter().cloned());
        // The key-hint run is never what loses — it's fixed at the right, outside the
        // shed set; the left run sheds to fit the remaining columns.
        const HINT: &str = "?·[ ]·␣↵·/·n·g·q";
        let width = self.width.max(1) as usize;
        let avail = width.saturating_sub(HINT.chars().count() + 3);
        let kept = shed_footer(segs, avail);
        let left_w: usize = kept.iter().map(|(t, _)| t.chars().count()).sum::<usize>()
            + kept.len().saturating_sub(1) * 3;
        let mut spans = vec![Span::raw(" ")];
        for (i, (t, p)) in kept.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", theme::status()));
            }
            // The nav labels (priority 0, known text) read as targets: dim + underlined.
            let is_nav = *p == 0 && (t.starts_with("↑ esc back") || t.starts_with("a active"));
            let style = if is_nav {
                theme::status().add_modifier(Modifier::UNDERLINED)
            } else {
                theme::status()
            };
            spans.push(Span::styled(t.clone(), style));
        }
        // Pad so the hint run sits at the right edge.
        let pad = avail.saturating_sub(left_w).saturating_add(1);
        spans.push(Span::styled(
            format!("{}{HINT} ", " ".repeat(pad)),
            theme::status(),
        ));
        Line::from(spans)
    }

    /// Fully render every line as `draw` would — wrap to `width`, then apply the
    /// per-row background fill and diff inset — but return the lines instead of
    /// painting a frame. Used by `--dump` so its output matches the on-screen
    /// render exactly (diff `+`/`-` rows get their `INSET`, backgrounds fill).
    pub fn rendered_lines(&mut self, width: u16) -> Vec<Line<'static>> {
        self.layout(width, u16::MAX);
        self.wrapped
            .iter()
            .map(|line| fill_bg(line.clone(), width as usize, is_diff_line(line)))
            .collect()
    }

    // --- mouse text selection ---
    /// Begin a selection at viewport (row, col); clears any prior selection.
    pub fn sel_begin(&mut self, row: u16, col: u16) {
        self.sel_anchor = Some((self.scroll + row as usize, col as usize));
        self.sel_cursor = None;
    }
    /// Extend the in-progress selection to viewport (row, col) — a drag.
    pub fn sel_extend(&mut self, row: u16, col: u16) {
        if self.sel_anchor.is_some() {
            let line = (self.scroll + row as usize).min(self.wrapped.len().saturating_sub(1));
            self.sel_cursor = Some((line, col as usize));
        }
    }
    /// True if the current press has become a drag (moved after pressing).
    pub fn dragged(&self) -> bool {
        self.sel_cursor.is_some()
    }
    /// The selected text (keeps the highlight visible so the user sees what copied).
    pub fn take_selection_text(&mut self) -> Option<String> {
        self.selection_text()
    }
    pub fn clear_selection(&mut self) {
        self.sel_anchor = None;
        self.sel_cursor = None;
    }
    /// Ordered (start, end) endpoints of the active selection, or None.
    fn sel_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let (a, c) = (self.sel_anchor?, self.sel_cursor?);
        Some(if a <= c { (a, c) } else { (c, a) })
    }
    /// Selected display-column range `[c0, c1)` for wrapped line `ai` (`usize::MAX`
    /// = to end of line), or None if the line isn't in the selection.
    fn sel_cols(&self, ai: usize) -> Option<(usize, usize)> {
        let (s, e) = self.sel_bounds()?;
        if ai < s.0 || ai > e.0 {
            return None;
        }
        let c0 = if ai == s.0 { s.1 } else { 0 };
        let c1 = if ai == e.0 { e.1 } else { usize::MAX };
        (c0 < c1).then_some((c0, c1))
    }
    /// Extract the selected text across wrapped lines, joined by newlines.
    fn selection_text(&self) -> Option<String> {
        let (s, e) = self.sel_bounds()?;
        let last = self.wrapped.len().saturating_sub(1);
        let mut lines = Vec::new();
        for ai in s.0..=e.0.min(last) {
            let c0 = if ai == s.0 { s.1 } else { 0 };
            let c1 = if ai == e.0 { e.1 } else { usize::MAX };
            lines.push(cols_of_line(&self.wrapped[ai], c0, c1));
        }
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.layout(area.width, area.height);
        let end = (self.scroll + self.view_h).min(self.wrapped.len());
        let cur = self.matches.get(self.match_pos).copied();
        let mut view: Vec<Line> = Vec::new();
        for ai in self.scroll..end {
            let line = &self.wrapped[ai];
            // Detect the diff-inset need from the original line, before search
            // highlighting overwrites the bg (else the matched row shifts left).
            let inset = is_diff_line(line);
            let styled = if !self.query.is_empty() && self.matches.binary_search(&ai).is_ok() {
                highlight_bg(line, Some(ai) == cur)
            } else {
                line.clone()
            };
            let focused = self.focus.is_some() && self.wrapped_tag.get(ai).copied() == self.focus;
            // The header row is the first wrapped line of the focused block (its
            // predecessor belongs to a different block) — only it gets the focus bar.
            let is_header =
                focused && (ai == 0 || self.wrapped_tag.get(ai - 1).copied() != self.focus);
            let styled = focus_recolor(styled, focused, is_header);
            let filled = fill_bg(styled, area.width as usize, inset);
            // Mouse selection overlays everything else (drawn last).
            let filled = match self.sel_cols(ai) {
                Some((c0, c1)) => apply_selection(filled, c0, c1),
                None => filled,
            };
            view.push(filled);
        }
        f.render_widget(
            Paragraph::new(view),
            Rect::new(area.x, area.y, area.width, self.view_h as u16),
        );
        f.render_widget(
            Paragraph::new(self.status_line()),
            Rect::new(area.x, area.y + self.view_h as u16, area.width, 1),
        );
        if self.show_help {
            render_help(f, area, self.can_go_back, self.can_open_picker);
        }
        // The `a` active-sub-agents popup.
        if let Some(sel) = self.agents_popup {
            let rows: Vec<String> = self
                .active_agent_indices()
                .iter()
                .filter_map(|&i| self.subagent_at(i))
                .map(|sa| {
                    let id = if sa.agent_id.is_empty() {
                        sa.agent_type.clone()
                    } else {
                        sa.agent_id.clone()
                    };
                    format!("{id}   {}   {}", sa.agent_type, sa.description)
                })
                .collect();
            render_agents_popup(f, area, &rows, sel);
        }
        // The switcher overlay (Picker clears the frame itself) sits on top.
        if let Some(p) = self.switcher.as_mut() {
            p.draw(f);
        }
    }
}

/// The `a` popup: a full-content-area overlay (mirroring the session picker) — a header
/// line naming the running count, one selectable row per running sub-agent (caret + a
/// `⟳` spinner + id · type · description, the selected one highlighted full-width), and a
/// key-hint footer. `↵`/click opens the selected agent; `Esc`/`a` closes.
fn render_agents_popup(f: &mut Frame, area: Rect, rows: &[String], sel: usize) {
    use unicode_width::UnicodeWidthStr;
    let width = area.width as usize;
    let pad = |s: String| -> String {
        let w = UnicodeWidthStr::width(s.as_str());
        if w < width {
            format!("{s}{}", " ".repeat(width - w))
        } else {
            s
        }
    };
    let header = Style::default()
        .fg(theme::fold_header())
        .bg(theme::user_bg());
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
    lines.push(Line::from(Span::styled(
        pad(format!(
            " active sub-agents of this session — {} running",
            rows.len()
        )),
        header,
    )));
    for (i, r) in rows.iter().enumerate() {
        let mark = if i == sel { "❯ ⟳ " } else { "  ⟳ " };
        let style = if i == sel {
            theme::agent().bg(theme::focus_bg())
        } else {
            theme::agent()
        };
        lines.push(Line::from(Span::styled(pad(format!("{mark}{r}")), style)));
    }
    // Pad to the footer row, then the key-hint bar pinned at the bottom.
    let footer_row = area.height.saturating_sub(1) as usize;
    while lines.len() < footer_row {
        lines.push(Line::from(""));
    }
    lines.push(Line::from(Span::styled(
        pad(" j/k move · ↵ open · a/esc close".to_string()),
        header,
    )));
    f.render_widget(Clear, area);
    f.render_widget(Paragraph::new(lines), area);
}

/// The `?` help overlay: a centered bordered panel listing every hotkey.
fn render_help(f: &mut Frame, area: Rect, can_go_back: bool, can_open_picker: bool) {
    let mut rows: Vec<(&str, &str)> = vec![
        ("j / k   ↓ / ↑", "scroll one line"),
        ("Ctrl-d / Ctrl-u", "half page down / up"),
        ("PageDown / PageUp", "full page down / up"),
        ("g / G", "jump to top / bottom"),
        ("Space", "toggle fold (focused, else first visible)"),
        ("T", "toggle all folds"),
        ("[ / ]", "focus previous / next foldable"),
        ("Enter", "fold focused · or download/reveal an attachment"),
        ("/   n / N", "search, then next / prev match"),
        (
            "mouse",
            "wheel scrolls · click header=fold · attachment name=download/reveal",
        ),
        ("?", "toggle this help"),
    ];
    // `s` opens the session switcher (only offered on a --latest launch).
    if can_open_picker {
        rows.push(("s", "switch session (picker)"));
    }
    // `Esc` returns to the session list only when we came from the picker.
    if can_go_back {
        rows.push(("Esc", "back to session list"));
        rows.push(("q", "quit"));
    } else {
        rows.push(("q / Esc", "quit"));
    }
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows.len());
    for (k, d) in &rows {
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<17}"), theme::user()),
            Span::styled((*d).to_string(), theme::status()),
        ]));
    }
    let w = 56u16.min(area.width);
    let h = (rows.len() as u16 + 2).min(area.height); // +2 for the border
    let x = area.x + area.width.saturating_sub(w) / 2;
    let y = area.y + area.height.saturating_sub(h) / 2;
    let rect = Rect::new(x, y, w, h);
    let block = WBlock::default()
        .borders(Borders::ALL)
        .border_style(theme::table_border())
        .title(" Hotkeys — ? or Esc to close ");
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Save an embedded attachment to the user's Downloads folder, never overwriting (a
/// numeric suffix is added on collision). Text is written verbatim; base64 (images) is
/// decoded first. Returns the written path. Synchronous by design — payloads are small
/// (see `DESIGN.md`).
fn save_attachment(a: &Attachment) -> std::io::Result<PathBuf> {
    write_attachment_to(&downloads_dir(), a)
}

/// The testable core of [`save_attachment`]: write `a`'s embedded bytes into `dir`.
fn write_attachment_to(dir: &Path, a: &Attachment) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind, Write};
    let bytes: Vec<u8> = match a.content.as_ref() {
        Some(AttachmentContent::Text(t)) => t.clone().into_bytes(),
        Some(AttachmentContent::Base64 { b64, .. }) => crate::clipboard::base64_decode(b64)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "invalid base64"))?,
        None => return Err(Error::new(ErrorKind::InvalidInput, "nothing to download")),
    };
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, &download_filename(a));
    std::fs::File::create(&path)?.write_all(&bytes)?;
    Ok(path)
}

/// `~/Downloads` (via `$HOME`), falling back to the current directory.
fn downloads_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join("Downloads"),
        None => PathBuf::from("."),
    }
}

/// The download filename for an attachment: the basename of its path/name, with an
/// image extension appended from the MIME type when the name lacks one.
fn download_filename(a: &Attachment) -> String {
    let base = a.path.as_deref().unwrap_or(&a.name);
    let mut name = base.rsplit('/').next().unwrap_or(base).to_string();
    if name.is_empty() {
        name = "attachment".into();
    }
    if let Some(AttachmentContent::Base64 { mime, .. }) = &a.content {
        if !name.contains('.') {
            if let Some(ext) = mime.rsplit('/').next().filter(|e| !e.is_empty()) {
                name.push('.');
                name.push_str(ext);
            }
        }
    }
    name
}

/// A path in `dir` for `fname` that does not exist yet — appends ` (1)`, ` (2)`, … before
/// the extension on collision, so an existing download is never overwritten.
fn unique_path(dir: &Path, fname: &str) -> PathBuf {
    let cand = dir.join(fname);
    if !cand.exists() {
        return cand;
    }
    let (stem, ext) = match fname.rsplit_once('.') {
        Some((s, e)) => (s.to_string(), format!(".{e}")),
        None => (fname.to_string(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|c| !c.exists())
        .unwrap_or(cand)
}

/// Replace a leading `$HOME` with `~` for a compact status message.
fn pretty_home(p: &Path) -> String {
    let s = p.display().to_string();
    if let Some(h) = std::env::var_os("HOME").and_then(|h| h.into_string().ok()) {
        if let Some(rest) = s.strip_prefix(&h) {
            return format!("~{rest}");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn blocks(n: usize) -> Vec<Block> {
        (0..n)
            .map(|i| Block::AssistantText(format!("line {i}")))
            .collect()
    }

    /// The default fold policy (no `--unfold`) collapses `Agent`/`Task` spawn blocks —
    /// they classify under the "agent" fold key. (This assertion lived in `model`'s block
    /// tests until the parser core was split into `claude-replay-core`, which has no view
    /// layer; the policy is a view concern, so it belongs here.)
    #[test]
    fn default_fold_policy_collapses_agent_blocks() {
        let jsonl = concat!(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"tool_use\",",
            "\"id\":\"toolu_A\",\"name\":\"Agent\",\"input\":{\"subagent_type\":\"code-reviewer\",",
            "\"description\":\"d\",\"prompt\":\"p\"}}]}}\n"
        );
        let blocks = crate::model::parse(jsonl);
        assert!(!blocks.is_empty(), "parsed an agent spawn");
        let pol = FoldPolicy::from_args(&crate::Args::default());
        assert!(pol.collapses(&blocks[0]), "agent spawn default-folds");
    }

    fn draw(v: &mut View, w: u16, h: u16) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| v.draw(f)).unwrap();
        t.backend().buffer().clone()
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// A drag across two lines extracts the spanning text and paints the selection
    /// background on the selected cells (and not on cells before the start column).
    /// Uses one long token hard-wrapped at a narrow width, so the two lines are
    /// deterministic: `⏺ 01234567` then `  89ABCDEF` (2-col hanging indent).
    #[test]
    fn mouse_selection_spans_lines_and_highlights() {
        let mut v = View::new(
            vec![Block::AssistantText("0123456789ABCDEFGHIJ".into())],
            "t",
            false,
            FoldPolicy::none(),
        );
        let w = 10u16;
        let buf = draw(&mut v, w, 20);
        let l0 = (0..20)
            .find(|&y| row(&buf, y).contains("01234567"))
            .unwrap();
        let l1 = (0..20)
            .find(|&y| row(&buf, y).contains("89ABCDEF"))
            .unwrap();
        // Press at the '0' (col 2, past the `⏺ ` marker) — not yet a drag.
        v.sel_begin(l0, 2);
        assert!(!v.dragged(), "a press with no move must not be a drag");
        // Drag to col 5 of line 1 ("  89ABCDEF" → cols [0,5) = "  89A").
        v.sel_extend(l1, 5);
        assert!(v.dragged());
        assert_eq!(v.take_selection_text().as_deref(), Some("01234567\n  89A"));
        // The highlight shows on the selected cells but not before the start column.
        let buf = draw(&mut v, w, 20);
        let sel = Some(theme::selection_bg());
        assert_eq!(
            buf[(2u16, l0)].style().bg,
            sel,
            "selected cell not highlighted"
        );
        assert_ne!(
            buf[(0u16, l0)].style().bg,
            sel,
            "cell before selection highlighted"
        );
    }

    /// Single-line selection extracts exactly the dragged column range.
    #[test]
    fn mouse_selection_single_line_extract() {
        let mut v = View::new(
            vec![Block::AssistantText("alpha beta gamma".into())],
            "t",
            false,
            FoldPolicy::none(),
        );
        let _ = draw(&mut v, 40, 10);
        // line 0 is "⏺ alpha beta gamma"; cols [2, 7) = "alpha".
        v.sel_begin(0, 2);
        v.sel_extend(0, 7);
        assert_eq!(v.take_selection_text().as_deref(), Some("alpha"));
    }

    /// A press with no drag yields no selection (the caller treats it as a click).
    #[test]
    fn mouse_press_without_drag_is_not_a_selection() {
        let mut v = View::new(
            vec![Block::AssistantText("x".into())],
            "t",
            false,
            FoldPolicy::none(),
        );
        let _ = draw(&mut v, 40, 10);
        v.sel_begin(0, 2);
        assert!(!v.dragged());
        assert!(v.take_selection_text().is_none());
    }

    /// Clicking the **path** in *any* file-tool header (Write, Update/Edit, Read,
    /// …) reveals it (returns the absolute path); clicking elsewhere toggles the
    /// fold (`None`). A header whose target isn't a real path (a Bash command)
    /// never reveals — matching Claude Code, where every file tool's path is live.
    #[test]
    fn clicking_header_path_reveals_else_toggles() {
        let file = std::env::temp_dir().join(format!("cr-click-{}.txt", std::process::id()));
        std::fs::write(&file, "hi").unwrap();
        let path = file.to_string_lossy().to_string();
        let file_tool = |name: &str| Block::ToolUse {
            name: name.into(),
            target: path.clone(),
            diffs: vec![(String::new(), "a\nb\nc".into())],
            output: Some("x".into()),
            patch: None,
            read_lines: None,
        };
        // A Bash header whose "target" is a command, not a path.
        let bash = Block::ToolUse {
            name: "Bash".into(),
            target: "echo hi".into(),
            output: Some("hi".into()),
            diffs: vec![],
            patch: None,
            read_lines: None,
        };
        // (block, a column that lands inside its `(path)` span). Header layout is
        // `⏺ <DisplayName>(` — Write=7, Update=8, Read=6 cols before the path.
        let blocks = vec![
            file_tool("Write"),
            file_tool("Edit"),
            file_tool("Read"),
            bash,
        ];
        let mut v = View::new(blocks, "t", false, FoldPolicy::none());
        let _ = draw(&mut v, 200, 40); // wide → no header wraps

        for (idx, col, name) in [(0usize, 9u16, "Write"), (1, 10, "Update"), (2, 8, "Read")] {
            let row = v.wrapped_tag.iter().position(|&t| t == idx).unwrap() as u16;
            assert_eq!(
                v.click_at(row, col),
                Some(Action::Reveal(file.clone())),
                "clicking {name}'s path should reveal the file"
            );
        }
        // The `⏺` marker (col 0) of the first block is outside any path span.
        assert!(v.click_at(0, 0).is_none(), "marker click should not reveal");
        // A command header never masquerades as a file path.
        let bash_row = v.wrapped_tag.iter().position(|&t| t == 3).unwrap() as u16;
        assert!(
            v.click_at(bash_row, 8).is_none(),
            "a command header must not reveal"
        );

        std::fs::remove_file(&file).ok();
    }

    /// Backlog invariant: a shell command and its output are ONE foldable block.
    /// The `⏺ Bash` header and its `⎿` output share a source block (distinct from
    /// the neighbouring block), and a single `t` toggle folds/expands both —
    /// collapsing to the one-line `Ran 1 shell command` summary.
    #[test]
    fn shell_command_and_output_are_one_foldable_block() {
        let bash = Block::ToolUse {
            name: "Bash".into(),
            target: "echo hi".into(),
            output: Some("hi\nthere".into()),
            diffs: vec![],
            patch: None,
            read_lines: None,
        };
        // A trailing assistant block gives a distinct neighbour tag to compare with.
        let mut v = View::new(
            vec![bash, Block::AssistantText("after".into())],
            "t",
            false,
            FoldPolicy::none(),
        );
        let w = 60u16;
        let buf = draw(&mut v, w, 14);

        let find = |needle: &str| {
            (0..14)
                .find(|&y| row(&buf, y).contains(needle))
                .unwrap_or_else(|| panic!("no row containing {needle:?}"))
        };
        let header_y = find("Bash");
        let output_y = find("there");
        let after_y = find("after");

        // (a) header + output are the SAME block, distinct from the next block.
        let hb = v.block_of_line(header_y as usize);
        assert_eq!(
            hb,
            v.block_of_line(output_y as usize),
            "command header and its output are different blocks"
        );
        assert_ne!(
            hb,
            v.block_of_line(after_y as usize),
            "bash block bled into the next block"
        );

        // (b) one toggle folds both — output gone, single summary remains.
        v.toggle_at_cursor();
        let buf = draw(&mut v, w, 14);
        assert!(v.is_collapsed(0), "bash block did not collapse");
        let text: String = (0..14).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(
            !text.contains("there"),
            "output still visible after folding:\n{text}"
        );
        assert!(
            text.contains("Ran 1 shell command"),
            "no collapsed one-line summary:\n{text}"
        );
    }

    /// An added diff row's background is **inset** `INSET` columns on each side:
    /// the first/last `INSET` columns are uncolored, the band between is filled.
    #[test]
    fn diff_add_row_background_is_inset_both_sides() {
        use crate::model::Hunk;
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "x.rs".into(),
            diffs: vec![("a".into(), "b".into())],
            output: None,
            patch: Some(vec![Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec!["+let a = 2;".into()],
            }]),
            read_lines: None,
        };
        let w = 60u16;
        let mut v = View::new(vec![block], "t", false, FoldPolicy::none());
        let buf = draw(&mut v, w, 10);
        let y = (0..9)
            .find(|&y| row(&buf, y).contains("let a = 2"))
            .expect("added code row");
        let add = Some(theme::diff_add_bg());
        // Left margin: columns 0..INSET are uncolored.
        for x in 0..INSET as u16 {
            assert_ne!(buf[(x, y)].style().bg, add, "col {x} should be uncolored");
        }
        // Right margin: the last INSET columns are uncolored.
        for x in (w - INSET as u16)..w {
            assert_ne!(buf[(x, y)].style().bg, add, "col {x} should be uncolored");
        }
        // The band between carries the diff background (sample a middle column).
        assert_eq!(buf[(w / 2, y)].style().bg, add, "middle band not filled");
    }

    /// An expanded foldable block fills its whole body with a distinct block
    /// background (full row width); a non-foldable block (assistant text) does not.
    #[test]
    fn help_overlay_toggles_with_question_mark() {
        let mut v = View::new(blocks(5), "t", false, FoldPolicy::none());
        let txt = |b: &Buffer| {
            (0..b.area.height)
                .map(|y| row(b, y))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let b0 = draw(&mut v, 80, 20);
        assert!(!txt(&b0).contains("toggle fold"), "help hidden initially");
        v.toggle_help();
        let b1 = draw(&mut v, 80, 20);
        let t1 = txt(&b1);
        assert!(
            t1.contains("toggle fold") && t1.contains("search"),
            "help lists bindings:\n{t1}"
        );
        v.toggle_help();
        let b2 = draw(&mut v, 80, 20);
        assert!(
            !txt(&b2).contains("toggle fold"),
            "help hidden after toggle"
        );
    }

    /// The help footer reflects whether Esc goes back to the session list.
    #[test]
    fn help_esc_line_depends_on_can_go_back() {
        let txt = |b: &Buffer| {
            (0..b.area.height)
                .map(|y| row(b, y))
                .collect::<Vec<_>>()
                .join("\n")
        };

        // Default (direct launch): Esc quits, no "back to session list".
        let mut v = View::new(blocks(5), "t", false, FoldPolicy::none());
        v.toggle_help();
        let t = txt(&draw(&mut v, 80, 20));
        assert!(t.contains("quit"), "help mentions quit:\n{t}");
        assert!(
            !t.contains("back to session list"),
            "no back-nav when direct-launched:\n{t}"
        );

        // Launched via the picker: Esc backs to the list.
        let mut v = View::new(blocks(5), "t", false, FoldPolicy::none());
        v.set_can_go_back(true);
        v.toggle_help();
        let t = txt(&draw(&mut v, 80, 20));
        assert!(
            t.contains("back to session list"),
            "back-nav listed when launched via picker:\n{t}"
        );

        // `--latest` launch: help lists the `s` switcher.
        let mut v = View::new(blocks(5), "t", false, FoldPolicy::none());
        v.set_can_open_picker(true);
        v.toggle_help();
        let t = txt(&draw(&mut v, 80, 20));
        assert!(t.contains("switch session"), "help lists s:\n{t}");
    }

    /// `s` opens the switcher overlay over the current view; Enter confirms a
    /// selection and closes it; Esc (via switcher_close) leaves the view intact.
    #[test]
    fn switcher_overlay_opens_lists_and_confirms() {
        use crate::discover::Candidate;
        use std::time::SystemTime;
        let cand = |name: &str| Candidate {
            path: std::path::PathBuf::from(format!("/tmp/{name}.jsonl")),
            mtime: SystemTime::now(),
            project: "proj".into(),
            snippet: format!("{name} snippet"),
            cwd_affinity: false,
            agent: crate::Agent::Claude,
        };
        let txt = |b: &Buffer| {
            (0..b.area.height)
                .map(|y| row(b, y))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut v = View::new(blocks(3), "t", false, FoldPolicy::none());
        assert!(!v.is_switcher_open());
        v.open_switcher(vec![cand("alpha"), cand("beta")]);
        assert!(v.is_switcher_open());
        // The picker header is drawn on top of the transcript.
        let t = txt(&draw(&mut v, 80, 20));
        assert!(t.contains("pick a session"), "switcher drawn:\n{t}");

        // Confirm returns the selected transcript and closes the overlay.
        let p = v.switcher_confirm();
        assert!(
            p.map(|p| p.to_string_lossy().contains("alpha"))
                .unwrap_or(false),
            "confirm returns the selected path"
        );
        assert!(!v.is_switcher_open(), "closed after confirm");

        // Close (Esc) just dismisses without a switch.
        v.open_switcher(vec![cand("alpha")]);
        v.switcher_close();
        assert!(!v.is_switcher_open(), "closed after switcher_close");
    }

    #[test]
    fn t_toggles_first_visible_foldable_when_top_is_not_foldable() {
        // Top-of-viewport block is non-foldable (assistant text); a foldable
        // tool_result is visible just below. `t` should still toggle it.
        let blocks = vec![
            Block::AssistantText("hello".into()),
            Block::ToolResult("a\nb\nc\nd".into()),
        ];
        let mut v = View::new(blocks, "t", false, FoldPolicy::none());
        let _ = draw(&mut v, 60, 12); // sets view_h; both blocks visible
        assert!(!v.is_collapsed(1), "tool_result starts expanded");
        let before = v.total_lines();
        v.toggle_at_cursor();
        assert!(v.is_collapsed(1), "t should collapse the visible foldable");
        let _ = draw(&mut v, 60, 12); // re-layout so total_lines reflects the fold
        assert!(
            v.total_lines() < before,
            "collapsing shrinks the line count"
        );
        v.toggle_at_cursor();
        assert!(!v.is_collapsed(1), "t again re-expands it");
    }

    #[test]
    fn expanded_shell_output_row_fills_background() {
        // An expanded Bash block: BOTH the header and the output rows carry the
        // shell-expanded block bg across the full row width (regression: output rows
        // used to lose the bg after wrapping).
        let w = 60u16;
        let bash = Block::ToolUse {
            name: "Bash".into(),
            target: "ls".into(),
            diffs: vec![],
            output: Some("file-alpha\nfile-beta".into()),
            patch: None,
            read_lines: None,
        };
        let mut v = View::new(vec![bash], "t", false, FoldPolicy::none());
        let buf = draw(&mut v, w, 12);
        let bg = Some(theme::shell_expanded_bg());
        let yo = (0..11)
            .find(|&y| row(&buf, y).contains("file-alpha"))
            .expect("output row");
        assert_eq!(buf[(2, yo)].style().bg, bg, "output not filled at left");
        assert_eq!(
            buf[(w - 1, yo)].style().bg,
            bg,
            "output not filled to right edge"
        );
    }

    #[test]
    fn expanded_foldable_fills_row_background() {
        let w = 60u16;
        // Expanded tool result (FoldPolicy::none keeps it expanded).
        let result = Block::ToolResult("output line one\noutput line two".into());
        let mut v = View::new(vec![result], "t", false, FoldPolicy::none());
        let buf = draw(&mut v, w, 10);
        let y = (0..9)
            .find(|&y| row(&buf, y).contains("output line one"))
            .expect("result row");
        let bg = Some(theme::shell_expanded_bg());
        // The band fills the full row, including past the text and the last column.
        assert_eq!(buf[(2, y)].style().bg, bg, "left of result not filled");
        assert_eq!(buf[(w - 1, y)].style().bg, bg, "right edge not filled");

        // A non-foldable assistant block leaves its row background unset.
        let mut va = View::new(blocks(1), "t", false, FoldPolicy::none());
        let bufa = draw(&mut va, w, 10);
        let ya = (0..9)
            .find(|&y| row(&bufa, y).contains("line 0"))
            .expect("assistant row");
        assert_ne!(
            bufa[(w - 1, ya)].style().bg,
            bg,
            "non-foldable row should not carry the block bg"
        );
    }

    /// A context (unhighlighted) diff row's gutter number lines up in the same
    /// column as a highlighted (+) row's gutter — both inset by `INSET`.
    #[test]
    fn diff_context_gutter_aligns_with_highlighted_rows() {
        use crate::model::Hunk;
        let block = Block::ToolUse {
            name: "Edit".into(),
            target: "x.rs".into(),
            diffs: vec![("a".into(), "b".into())],
            output: None,
            patch: Some(vec![Hunk {
                old_start: 1,
                new_start: 1,
                lines: vec![" let x = 0;".into(), "+let a = 2;".into()],
            }]),
            read_lines: None,
        };
        let w = 60u16;
        let mut v = View::new(vec![block], "t", false, FoldPolicy::none());
        let buf = draw(&mut v, w, 10);
        // First ASCII-digit column in the row containing `needle` (the gutter no.).
        let digit_col = |needle: &str| -> usize {
            let y = (0..10)
                .find(|&y| row(&buf, y).contains(needle))
                .unwrap_or_else(|| panic!("row with {needle:?} not found"));
            let line = row(&buf, y);
            line.char_indices()
                .find(|(_, c)| c.is_ascii_digit())
                .map(|(i, _)| i)
                .expect("a gutter digit")
        };
        let ctx = digit_col("let x = 0");
        let add = digit_col("let a = 2");
        assert_eq!(
            ctx, add,
            "context gutter col {ctx} != added gutter col {add}"
        );
    }

    #[test]
    fn opens_pinned_to_bottom() {
        let mut v = View::new(blocks(100), "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        assert!(v.follow());
        assert_eq!(v.scroll(), v.total_lines().saturating_sub(v.view_h()));
    }

    #[test]
    fn scroll_up_unfollows_then_bottom_refollows() {
        let mut v = View::new(blocks(100), "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(-5);
        assert!(!v.follow());
        v.scroll_by(-100_000);
        assert_eq!(v.scroll(), 0);
        v.jump_bottom();
        assert!(v.follow());
    }

    #[test]
    fn new_messages_badge_appears_and_clears() {
        let mut v = View::new(blocks(100), "t", true, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(-20);
        assert!(!v.follow());
        v.ingest(blocks(3));
        assert_eq!(v.new_count(), 3);
        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("3 new"));
        v.jump_bottom();
        assert_eq!(v.new_count(), 0);
        let buf = draw(&mut v, 40, 10);
        assert!(!row(&buf, 9).contains("new"));
    }

    /// Live tail: activity tools that arrive in one poll and a thinking block that
    /// arrives in a LATER poll must still group — the thinking absorbs the earlier
    /// tools instead of leaving them as separate expanded blocks (which only a
    /// restart used to fix).
    #[test]
    fn live_thinking_absorbs_tools_from_an_earlier_poll() {
        let tool = |name: &str, target: &str| Block::ToolUse {
            name: name.into(),
            target: target.into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
        };
        let mut v = View::new(
            vec![Block::UserText("go".into())],
            "t",
            true,
            FoldPolicy::default(),
        );
        draw(&mut v, 60, 20);

        // Poll 1: the tools land as their own top-level blocks.
        v.ingest(vec![tool("Bash", "ls"), tool("Read", "x.rs")]);
        assert_eq!(v.block_kinds(), vec!["user", "bash", "read"]);

        // Poll 2: the thinking block arrives alone — it should swallow both tools.
        v.ingest(vec![Block::Thinking {
            text: "pondering the plan".into(),
            duration_secs: None,
            tools: vec![],
        }]);
        assert_eq!(
            v.block_kinds(),
            vec!["user", "thinking"],
            "thinking did not absorb the earlier-poll tools"
        );

        // The thinking folds by default; its collapsed summary names the absorbed
        // activities, and the tool bodies are hidden (grouped, not expanded).
        assert!(v.is_collapsed(1), "thinking should fold by default");
        let buf = draw(&mut v, 60, 20);
        let body: String = (0..19).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("(ls)") && body.contains("thought"),
            "collapsed summary missing absorbed activities:\n{body}"
        );
    }

    /// Live-tail two-tier queue collapse: a `⧗ queued:` marker shown in one poll is
    /// REPLACED by its `❯` turn when the pickup (a `queued_command` attachment → a
    /// matching `UserText`) arrives immediately after (nothing in between). If agent
    /// work arrives between them, the marker survives (a delayed pickup).
    #[test]
    fn live_immediate_pickup_replaces_queued_marker_with_the_turn() {
        let tool = |name: &str| Block::ToolUse {
            name: name.into(),
            target: "x".into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
        };
        let mut v = View::new(
            vec![Block::UserText("go".into())],
            "t",
            true,
            FoldPolicy::default(),
        );
        draw(&mut v, 60, 20);

        // Poll 1: two prompts queued while the agent is busy → two markers at the tail.
        v.ingest(vec![
            Block::QueueEvent {
                text: "immediate one".into(),
            },
            Block::QueueEvent {
                text: "delayed one".into(),
            },
        ]);
        assert_eq!(v.block_kinds(), vec!["user", "queue", "queue"]);

        // Poll 2: "immediate one" is picked up right away (its marker is the tail) →
        // the arriving turn replaces the marker, not appends after it.
        v.ingest(vec![Block::UserText("immediate one".into())]);
        assert_eq!(
            v.block_kinds(),
            vec!["user", "queue", "user"],
            "immediate marker should be replaced by its turn"
        );

        // Poll 3: agent work arrives, THEN "delayed one" is picked up → its marker
        // (no longer the tail) survives alongside the turn.
        v.ingest(vec![tool("Bash"), Block::UserText("delayed one".into())]);
        assert_eq!(
            v.block_kinds(),
            vec!["user", "queue", "user", "bash", "user"],
            "delayed marker should survive its turn"
        );
    }

    /// `[`/`]` can focus an attachment (it isn't foldable but is actionable), and
    /// activating a path-only attachment returns its path for the caller to reveal.
    #[test]
    fn attachment_is_focusable_and_reveal_returns_its_path() {
        let blocks = vec![
            Block::UserText("go".into()),
            Block::Attachment(Attachment {
                kind: "ref",
                name: "src/lib.rs".into(),
                path: Some("/w/src/lib.rs".into()),
                content: None,
            }),
        ];
        let mut v = View::new(blocks, "t", false, FoldPolicy::none());
        draw(&mut v, 80, 20);
        v.focus_next();
        assert_eq!(v.focused_block(), Some(1), "attachment should be focusable");
        // A reveal-only attachment yields its path (app reveals it); nothing written.
        assert_eq!(
            v.activate_focused(),
            Some(Action::Reveal(PathBuf::from("/w/src/lib.rs")))
        );
    }

    /// Keyboard `Enter` on a focused spawn DESCENDS directly (Space still folds); a
    /// mouse click on the header FOLDS, while a click on the agent-id descends. Returning
    /// (`focus_block`) lands on the spawn without mutating any fold state.
    #[test]
    fn subagent_enter_descends_click_header_folds_agentid_descends() {
        use crate::model::{AgentStatus, SubAgent};
        let sa = Block::SubAgent(SubAgent {
            agent_id: "aXYZ".into(),
            tool_use_id: "t".into(),
            agent_type: "gp".into(),
            description: "d".into(),
            prompt: "p".into(),
            status: AgentStatus::Completed,
            result: Some("r".into()),
            output_file: None,
            blocks: vec![
                Block::UserText("go".into()),
                Block::AssistantText("done".into()),
            ],
            subtree_cost: None,
        });
        let bash = Block::ToolUse {
            name: "Bash".into(),
            target: "ls".into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
        };
        let blocks = vec![Block::UserText("root".into()), sa, bash];
        let mut v = View::new(blocks, "t", false, FoldPolicy::default());
        draw(&mut v, 120, 20);
        v.focus_next();
        assert_eq!(v.focused_block(), Some(1));
        assert!(v.is_collapsed(1), "spawn starts collapsed");
        // Enter descends directly — no expand step — and doesn't mutate any fold state.
        assert_eq!(v.activate_focused(), Some(Action::Descend(1)));
        assert!(
            v.is_collapsed(1),
            "descend must not mutate the spawn's fold"
        );
        assert!(v.is_collapsed(2), "other folds untouched");

        // Mouse: a click on the header (not the id) FOLDS/expands; a click on the agent
        // id descends. Locate the spawn's header row + the id column span.
        let row = v.wrapped_tag.iter().position(|&t| t == 1).unwrap() as u16;
        let Block::SubAgent(spawn) = &v.blocks[1] else {
            unreachable!()
        };
        let (ids, ide) = crate::render::agent_id_span(spawn).expect("id span");
        // Click on the header caret (col 0) toggles the fold, does not descend.
        assert_eq!(v.click_at(row, 0), None, "header click folds, not descends");
        assert!(!v.is_collapsed(1), "header click expanded it");
        // Re-collapse so the id is on the header row again, then click the id → descend.
        v.click_at(row, 0);
        assert!(v.is_collapsed(1));
        let mid = ((ids + ide) / 2) as u16;
        assert_eq!(
            v.click_at(row, mid),
            Some(Action::Descend(1)),
            "clicking the agent id descends"
        );

        // Returning lands on the spawn without touching folds.
        v.focus_block(1);
        assert_eq!(v.focused_block(), Some(1));
        assert!(v.is_collapsed(1), "return must not mutate the spawn's fold");
        assert_eq!(v.subagent_at(1).map(|s| s.blocks.len()), Some(2));
    }

    /// A still-RUNNING agent (no completion yet, so its child transcript isn't loaded into
    /// `blocks`) still shows the `↵ id` descend affordance and descends on Enter/click —
    /// the child loads lazily at descend time. Regression: a live agent used to be
    /// unreachable because the affordance was gated on `!blocks.is_empty()`.
    #[test]
    fn running_agent_with_no_loaded_child_still_descends() {
        use crate::model::{AgentStatus, SubAgent};
        let sa = Block::SubAgent(SubAgent {
            agent_id: "aLIVE".into(),
            tool_use_id: "t".into(),
            agent_type: "gp".into(),
            description: "d".into(),
            prompt: "p".into(),
            status: AgentStatus::AsyncLaunched, // running, not terminal
            result: None,
            output_file: None,
            blocks: vec![], // child transcript not loaded yet
            subtree_cost: None,
        });
        let mut v = View::new(
            vec![Block::UserText("root".into()), sa],
            "t",
            true,
            FoldPolicy::default(),
        );
        draw(&mut v, 120, 20);
        // The affordance is present even with an empty child.
        let Block::SubAgent(spawn) = &v.blocks[1] else {
            unreachable!()
        };
        assert!(
            crate::render::agent_id_span(spawn).is_some(),
            "running agent shows the ↵ id descend target"
        );
        // It counts as active (footer `a active N` + `a` popup).
        assert_eq!(v.active_children(), 1);
        // Enter descends (the caller loads the child lazily); no fold mutation.
        v.focus_block(1);
        assert_eq!(v.activate_focused(), Some(Action::Descend(1)));
    }

    /// A completion (`AgentDone`) event is a descend target too: Enter descends, a click
    /// on its `↵ id` descends, a click elsewhere on its header folds, and the descend ref
    /// resolves (lazy child load) for any terminal status.
    #[test]
    fn completion_event_descends_via_id() {
        use crate::model::AgentStatus;
        let done = Block::AgentDone {
            agent_id: "aDONE".into(),
            agent_type: "gp".into(),
            description: "d".into(),
            status: AgentStatus::Failed, // any terminal status behaves the same
            result: Some("r".into()),
        };
        let mut v = View::new(
            vec![Block::UserText("root".into()), done],
            "t",
            false,
            FoldPolicy::default(),
        );
        draw(&mut v, 120, 20);
        // Enter on the focused completion descends.
        v.focus_block(1);
        assert_eq!(v.activate_focused(), Some(Action::Descend(1)));
        // The descend ref resolves the agent id with an empty (lazy) child.
        let dref = v.descend_ref_at(1).expect("completion is a descend target");
        assert_eq!(dref.agent_id, "aDONE");
        assert!(dref.blocks.is_empty(), "completion child loads lazily");
        // Mouse: clicking the header id descends; clicking the caret (col 0) folds.
        let row = v.wrapped_tag.iter().position(|&t| t == 1).unwrap() as u16;
        let (s, e) = crate::render::agent_done_id_span("gp", "d", AgentStatus::Failed, "aDONE")
            .expect("done id span");
        assert_eq!(
            v.click_at(row, ((s + e) / 2) as u16),
            Some(Action::Descend(1)),
            "clicking the completion id descends"
        );
        assert_eq!(v.click_at(row, 0), None, "header click folds, not descends");
    }

    /// The footer sheds least-important segments first (cached → % → model → in → out →
    /// duration → cost); the nav labels, live-state, and the id never drop (the id
    /// truncates last). The key-hint run lives outside the shed set entirely.
    #[test]
    fn footer_shed_order_keeps_nav_live_and_id() {
        let segs = || {
            vec![
                ("↑ esc back".to_string(), 0u8),
                ("uuid-abc".to_string(), LOC_PRIO),
                ("[bottom]".to_string(), 0),
                ("50%".to_string(), 2),
                ("14M cached".to_string(), 1),
                ("opus4.8".to_string(), 3),
                ("1.8M in".to_string(), 4),
                ("212K out".to_string(), 5),
                ("1h12m".to_string(), 6),
                ("~$11".to_string(), 7),
            ]
        };
        let texts =
            |v: Vec<(String, u8)>| -> Vec<String> { v.into_iter().map(|(t, _)| t).collect() };
        let full_w =
            segs().iter().map(|(t, _)| t.chars().count()).sum::<usize>() + (segs().len() - 1) * 3;
        // Nothing shed when it fits.
        assert!(texts(shed_footer(segs(), full_w)).contains(&"14M cached".to_string()));
        // First drop is cached; cost survives it (least → most important).
        let after1 = texts(shed_footer(segs(), full_w - 1));
        assert!(
            !after1.contains(&"14M cached".to_string()),
            "cached drops first"
        );
        assert!(after1.contains(&"~$11".to_string()), "cost outlives cached");
        // Very tight: all metric segments gone, but the nav label + live-state never do.
        let tight = texts(shed_footer(segs(), 24));
        assert!(tight.iter().any(|t| t == "↑ esc back"), "nav never drops");
        assert!(
            tight.iter().any(|t| t == "[bottom]"),
            "live-state never drops"
        );
        for m in [
            "14M cached",
            "50%",
            "opus4.8",
            "1.8M in",
            "212K out",
            "1h12m",
            "~$11",
        ] {
            assert!(
                !tight.contains(&m.to_string()),
                "{m} shed under tight width"
            );
        }
        // At a moderate width the id survives all metric drops, truncated if needed.
        let mid = texts(shed_footer(segs(), 40));
        assert!(
            mid.iter().any(|t| t.starts_with("uuid") || t.contains('…')),
            "id kept (possibly truncated): {mid:?}"
        );
    }

    /// The `a` popup is gated on this node having a running child (like `s` on the
    /// picker), lists only non-terminal children, and confirming descends into the
    /// selected one. A finished replay (all terminal) can't open it.
    #[test]
    fn active_agents_popup_gates_and_descends() {
        use crate::model::{AgentStatus, SubAgent};
        let mk = |id: &str, status| {
            Block::SubAgent(SubAgent {
                agent_id: id.into(),
                tool_use_id: "t".into(),
                agent_type: "gp".into(),
                description: "d".into(),
                prompt: "p".into(),
                status,
                result: None,
                output_file: None,
                blocks: vec![Block::UserText("x".into())],
                subtree_cost: None,
            })
        };
        let blocks = vec![
            Block::UserText("root".into()),
            mk("aRUN", AgentStatus::AsyncLaunched),
            mk("aDONE", AgentStatus::Completed),
        ];
        let mut v = View::new(blocks, "t", true, FoldPolicy::default());
        draw(&mut v, 80, 20);
        assert_eq!(v.active_children(), 1, "only the running child counts");
        assert!(v.can_open_agents());
        v.open_agents_popup();
        assert!(v.agents_popup_open());
        // The overlay renders full-frame: header at the top, the running agent as a row,
        // and the key-hint bar pinned to the bottom.
        let buf = draw(&mut v, 80, 20);
        assert!(
            row(&buf, 0).contains("active sub-agents") && row(&buf, 0).contains("1 running"),
            "header: {:?}",
            row(&buf, 0)
        );
        assert!(
            row(&buf, 1).contains("aRUN"),
            "agent row: {:?}",
            row(&buf, 1)
        );
        assert!(
            row(&buf, 19).contains("open") && row(&buf, 19).contains("close"),
            "footer: {:?}",
            row(&buf, 19)
        );
        // A mouse click on the agent's row (row 1 — header is row 0) descends into it.
        assert_eq!(
            v.agents_popup_click(1),
            PopupClick::Descend(1),
            "clicking the row descends into the running child"
        );
        // A click on the header row (0) is swallowed — never leaks to the content.
        v.open_agents_popup();
        assert_eq!(v.agents_popup_click(0), PopupClick::Border);
        assert!(v.agents_popup_open(), "stray click keeps the popup open");
        // Confirm descends into the running child (block index 1), and closes the popup.
        assert_eq!(v.agents_popup_confirm(), Some(1));
        assert!(!v.agents_popup_open());

        // A finished replay (no running children) can't open the popup.
        let mut done = View::new(
            vec![mk("aDONE", AgentStatus::Completed)],
            "t",
            false,
            FoldPolicy::none(),
        );
        assert!(!done.can_open_agents());
        done.open_agents_popup();
        assert!(!done.agents_popup_open(), "gated when no active children");
    }

    /// The rendered footer of a descended view carries `↑ esc back` AND the key-hint run
    /// (the hint is never shed), even at a narrow width.
    #[test]
    fn descended_footer_shows_esc_back_and_hint() {
        let mut v = View::new(
            vec![Block::UserText("x".into())],
            "377e-uuid-long-session-id",
            false,
            FoldPolicy::none(),
        );
        v.set_descended(true);
        v.set_footer_segments(vec![("opus4.8".into(), 3), ("~$11".into(), 7)]);
        let buf = draw(&mut v, 60, 10);
        let footer: String = row(&buf, 9);
        assert!(footer.contains("esc back"), "footer: {footer:?}");
        assert!(footer.contains('q'), "hint run survives: {footer:?}");
    }

    /// The download core writes embedded content into a target dir, decoding base64 and
    /// never overwriting an existing file.
    #[test]
    fn write_attachment_saves_text_image_and_avoids_overwrite() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "cr-attach-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);

        let text = Attachment {
            kind: "file",
            name: "notes.md".into(),
            path: Some("/w/notes.md".into()),
            content: Some(AttachmentContent::Text("hello".into())),
        };
        let p1 = write_attachment_to(&dir, &text).unwrap();
        assert_eq!(p1.file_name().unwrap(), "notes.md");
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "hello");
        // A second save of the same name must not overwrite.
        let p2 = write_attachment_to(&dir, &text).unwrap();
        assert_eq!(p2.file_name().unwrap(), "notes (1).md");

        // A base64 image decodes to bytes; the extension comes from the MIME type.
        let img = Attachment {
            kind: "image",
            name: "shot".into(),
            path: None,
            content: Some(AttachmentContent::Base64 {
                mime: "image/png".into(),
                b64: "Zm9v".into(), // "foo"
            }),
        };
        let pi = write_attachment_to(&dir, &img).unwrap();
        assert_eq!(pi.file_name().unwrap(), "shot.png");
        assert_eq!(std::fs::read(&pi).unwrap(), b"foo");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn following_view_keeps_new_content_visible() {
        let mut v = View::new(blocks(10), "t", true, FoldPolicy::none());
        draw(&mut v, 40, 8);
        v.ingest(vec![Block::AssistantText("SENTINEL_TAIL".into())]);
        let buf = draw(&mut v, 40, 8);
        let body: String = (0..7).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("SENTINEL_TAIL"), "tail not visible:\n{body}");
    }

    #[test]
    fn fold_collapses_and_expands_a_tool_result() {
        // A multi-line tool result is foldable.
        let big = (0..15)
            .map(|i| format!("output {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut v = View::new(vec![Block::ToolResult(big)], "t", false, FoldPolicy::none());
        draw(&mut v, 60, 30);
        let expanded = v.total_lines();
        assert!(expanded > 5);

        v.toggle_block(0); // collapse
        let buf = draw(&mut v, 60, 30);
        let collapsed = v.total_lines();
        assert!(
            collapsed < expanded,
            "collapse should shrink: {collapsed} !< {expanded}"
        );
        let body: String = (0..28).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("folded"), "placeholder missing:\n{body}");
        // The hint names the real fold key (space), not a stale one.
        assert!(
            body.contains("space / click to expand"),
            "placeholder should name the space key:\n{body}"
        );

        v.toggle_block(0); // expand
        draw(&mut v, 60, 30);
        assert_eq!(v.total_lines(), expanded);
    }

    #[test]
    fn toggle_all_collapses_then_expands() {
        let r1 = Block::ToolResult(
            (0..10)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        );
        // Two tool results (both expanded by default) — thinking starts
        // collapsed now, which has its own test below.
        let r2 = Block::ToolResult(
            (0..10)
                .map(|i| format!("res {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let mut v = View::new(vec![r1, r2], "t", false, FoldPolicy::none());
        draw(&mut v, 60, 40);
        let full = v.total_lines();
        v.toggle_all(); // collapse all
        draw(&mut v, 60, 40);
        assert!(v.total_lines() < full);
        v.toggle_all(); // expand all
        draw(&mut v, 60, 40);
        assert_eq!(v.total_lines(), full);
    }

    #[test]
    fn thinking_blocks_start_collapsed_and_expand() {
        let big = (0..8)
            .map(|i| format!("thought {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut v = View::new(
            vec![Block::Thinking {
                text: big,
                duration_secs: None,
                tools: vec![],
            }],
            "t",
            false,
            FoldPolicy::default(),
        );
        let buf = draw(&mut v, 60, 20);
        let body: String = (0..19).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("Thought (8 lines)"),
            "thinking should collapse to a summary:\n{body}"
        );
        assert!(
            !body.contains("thought 5"),
            "collapsed body should be hidden:\n{body}"
        );

        v.toggle_at_cursor(); // expand
        let buf = draw(&mut v, 60, 20);
        let body: String = (0..19).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("thought 5"),
            "expanding should reveal the body:\n{body}"
        );
    }

    fn args_with(fold: Option<&str>, unfold: Option<&str>, full: bool) -> Args {
        Args {
            target: None,
            agent: None,
            latest: false,
            follow: false,
            no_thinking: false,
            reads: false,
            results: false,
            no_user: false,
            full,
            fold: fold.map(String::from),
            unfold: unfold.map(String::from),
            read_match: None,
            dump: None,
            dump_html: None,
            dump_all_html: None,
            html: false,
            width: None,
        }
    }

    // read (block 0), tool_result (block 1), edit (block 2).
    fn policy_blocks() -> Vec<Block> {
        vec![
            Block::ToolUse {
                name: "Read".into(),
                target: "x".into(),
                diffs: vec![],
                output: None,
                patch: None,
                read_lines: None,
            },
            Block::ToolResult("a\nb\nc".into()),
            Block::ToolUse {
                name: "Edit".into(),
                target: "y".into(),
                diffs: vec![("a".into(), "b".into())],
                output: None,
                patch: None,
                read_lines: None,
            },
            Block::ToolUse {
                name: "Write".into(),
                target: "z".into(),
                diffs: vec![("".into(), "new file\nbody".into())],
                output: None,
                patch: None,
                read_lines: None,
            },
        ]
    }

    #[test]
    fn default_policy_folds_read_and_result_not_edit() {
        let v = View::new(
            policy_blocks(),
            "t",
            false,
            FoldPolicy::from_args(&args_with(None, None, false)),
        );
        assert!(v.is_collapsed(0), "read should be folded by default");
        assert!(v.is_collapsed(1), "tool_result should be folded by default");
        assert!(!v.is_collapsed(2), "edit should be expanded by default");
        assert!(v.is_collapsed(3), "write should be folded by default");
    }

    #[test]
    fn unfold_flag_expands_those_types() {
        let v = View::new(
            policy_blocks(),
            "t",
            false,
            FoldPolicy::from_args(&args_with(None, Some("read,tool_result"), false)),
        );
        assert!(!v.is_collapsed(0), "read unfolded");
        assert!(!v.is_collapsed(1), "tool_result unfolded");
        assert!(!v.is_collapsed(2), "edit still expanded");
    }

    #[test]
    fn fold_flag_collapses_edit_and_unfold_wins() {
        let v = View::new(
            policy_blocks(),
            "t",
            false,
            FoldPolicy::from_args(&args_with(Some("edit"), Some("read"), false)),
        );
        assert!(!v.is_collapsed(0), "read unfolded (--unfold)");
        assert!(v.is_collapsed(1), "tool_result still default-folded");
        assert!(v.is_collapsed(2), "edit folded via --fold");
    }

    #[test]
    fn bracket_focus_enter_and_hover() {
        let mk = |name: &str| Block::ToolUse {
            name: name.into(),
            target: "x".into(),
            diffs: vec![],
            output: Some("a\nb".into()),
            patch: None,
            read_lines: Some(3),
        };
        // 0: assistant (not foldable), 1: Bash, 2: Read — both fold by default.
        let blocks = vec![Block::AssistantText("hi".into()), mk("Bash"), mk("Read")];
        let mut v = View::new(blocks, "t", false, FoldPolicy::default());
        draw(&mut v, 60, 20);

        // ] / [ cycle the foldable blocks, skipping the assistant text.
        v.focus_next();
        assert_eq!(v.focused_block(), Some(1));
        v.focus_next();
        assert_eq!(v.focused_block(), Some(2));
        v.focus_prev();
        assert_eq!(v.focused_block(), Some(1));

        // Enter toggles the focused (Bash) collapsed → expanded (a non-attachment
        // block has no reveal path).
        assert!(v.is_collapsed(1));
        assert_eq!(v.activate_focused(), None);
        assert!(!v.is_collapsed(1));

        // Focus the Read summary and confirm it draws in the brighter color.
        v.focus_next();
        assert_eq!(v.focused_block(), Some(2));
        let buf = draw(&mut v, 60, 20);
        let y = (0..19)
            .find(|&y| row(&buf, y).contains("Read x"))
            .expect("read summary row");
        assert_eq!(
            buf[(2, y)].style().fg,
            Some(theme::fold_header_focused()),
            "focused header not brightened"
        );

        // Hovering a row focuses the foldable under it.
        v.hover_row(y);
        assert_eq!(v.focused_block(), Some(2));
    }

    /// Blocks whose header isn't fold-header-colored (Edit `⏺ Edit`, `⎿` results)
    /// still get a visible focus cue: a full-width background bar on the header row.
    #[test]
    fn focus_draws_a_bar_on_non_fold_header_blocks() {
        let edit = Block::ToolUse {
            name: "Edit".into(),
            target: "x".into(),
            diffs: vec![("old".into(), "new".into())],
            output: None,
            patch: None,
            read_lines: None,
        };
        let result = Block::ToolResult("some output line".into());
        let blocks = vec![Block::AssistantText("hi".into()), edit, result];
        let mut v = View::new(blocks, "t", false, FoldPolicy::none());

        // Focus the Edit block: its header ("⏺ Edit(x)") uses tool color, which
        // focus_recolor can't brighten — the bar is the only cue.
        v.focus_next();
        assert_eq!(v.focused_block(), Some(1));
        let buf = draw(&mut v, 60, 20);
        // Edit's header renders as "⏺ Update(x)" (display_name maps Edit → Update).
        let y = (0..19)
            .find(|&y| row(&buf, y).contains("Update"))
            .expect("edit header row");
        assert_eq!(
            buf[(0, y)].style().bg,
            Some(theme::focus_bg()),
            "focus bar missing on the Edit header"
        );
        // fill_bg extends the bar across the whole row.
        assert_eq!(
            buf[(58, y)].style().bg,
            Some(theme::focus_bg()),
            "focus bar should span the full row width"
        );

        // A row that belongs to no focused block carries no focus bar.
        let assistant_y = (0..19).find(|&y| row(&buf, y).contains("hi"));
        if let Some(ay) = assistant_y {
            assert_ne!(
                buf[(0, ay)].style().bg,
                Some(theme::focus_bg()),
                "unfocused row should not have the focus bar"
            );
        }
    }

    /// A slash-command block starts collapsed under the default policy, showing
    /// only its `❯` header + first `⎿` line (not the full output).
    #[test]
    fn command_block_starts_collapsed_by_default() {
        let cmd = Block::Command {
            name: "/compact".into(),
            args: String::new(),
            output: vec!["Compacted (ctrl+o to see full summary)".into()],
        };
        let v = View::new(vec![cmd], "t", false, FoldPolicy::default());
        assert!(v.is_collapsed(0), "command should fold by default");
    }

    #[test]
    fn full_flag_unfolds_thinking() {
        let v = View::new(
            vec![Block::Thinking {
                text: "a\nb\nc".into(),
                duration_secs: None,
                tools: vec![],
            }],
            "t",
            false,
            FoldPolicy::from_args(&args_with(None, None, true)),
        );
        assert!(!v.is_collapsed(0), "--full should expand thinking");
    }

    #[test]
    fn full_flag_unfolds_everything() {
        let v = View::new(
            policy_blocks(),
            "t",
            false,
            FoldPolicy::from_args(&args_with(None, None, true)),
        );
        assert!(!v.is_collapsed(0) && !v.is_collapsed(1) && !v.is_collapsed(2));
    }

    #[test]
    fn search_finds_navigates_and_shows_status() {
        let mut bs = blocks(30);
        bs[5] = Block::AssistantText("UNIQUEMATCH alpha".into());
        bs[20] = Block::AssistantText("UNIQUEMATCH beta".into());
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);

        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        assert_eq!(v.match_count(), 2);
        v.search_confirm(); // leave input mode; keep query + highlights

        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("search 'UNIQUEMATCH'"));
        let body: String = (0..9).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(
            body.contains("UNIQUEMATCH"),
            "first match not visible:\n{body}"
        );

        let first = v.scroll();
        v.search_next();
        draw(&mut v, 40, 10);
        assert_ne!(v.scroll(), first, "n should move to the next match");
    }
}

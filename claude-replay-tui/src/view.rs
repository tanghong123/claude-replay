//! The viewer's state machine + drawing, decoupled from the terminal so it can
//! be driven headless (ratatui `TestBackend`) without a real TTY.

use crate::discover::Candidate;
use crate::fold::FoldPolicy;
use crate::highlight::Hl;
use crate::metrics::Metrics;
use crate::model::{Attachment, AttachmentContent, Block, LoadedAttachment};
use crate::tui::picker::Picker;
use crate::tui::{render, theme, wrap};
use crate::Transcript;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as WBlock, Borders, Clear, Paragraph};
use ratatui::Frame;
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

/// Render + wrap one block, free of the `View` — so a measure pass can hand blocks to several
/// threads without the whole viewer having to be `Sync`. See [`View::wrapped_block_lines`].
fn wrapped_lines_of(
    block: &Block,
    is_collapsed: bool,
    width: usize,
    carry_in: bool,
) -> Vec<Line<'static>> {
    assembled_lines_of(block, is_collapsed, width, carry_in, Hl::Styled).1
}

/// One block's wrapped HEIGHT, skipping the syntax highlighter on every row where it provably
/// cannot change the answer — the measure pass's unit, shared by the serial prefix walk and the
/// parallel workers so there is exactly one implementation of the rule.
///
/// Measuring used to render the block for real and throw the styled lines away, and syntect
/// parsing (~150 µs/line) is what that costs: on this project's 107 MB session `edit` blocks are
/// 16% of the blocks and 97% of the measure time. But a row's height depends only on its WIDTH,
/// and [`Hl::Measure`] emits a row that cannot wrap as ONE uncoloured span carrying exactly the
/// text the styled path would have split — same characters, same width, no parse.
///
/// Span segmentation matters in exactly one place: `wrap_line` breaks words per span, so
/// `["abcdef"]` and `["abc","def"]` break differently. That can only bite a row wide enough to
/// wrap, and those rows ARE highlighted, identically to the render. So the result is exact on
/// both sides of the rule, and — unlike a per-BLOCK fallback, which re-rendered the whole block
/// and gave the win back at narrow widths (47% of blocks fell back at 80 columns) — nothing is
/// ever rendered twice.
///
/// `measure_matches_render_for_every_block_and_width` pins the equality this rests on; the byte
/// gate cannot, because heights drive scroll geometry rather than printed output.
fn measure_one(block: &Block, is_collapsed: bool, width: usize, carry_in: bool) -> usize {
    assembled_lines_of(block, is_collapsed, width, carry_in, Hl::Measure { width })
        .1
        .len()
}

/// `(unwrapped row count, wrapped lines)` for one block. Split out so the MEASURE pass can pass
/// [`Hl::Measure`] and so tests can compare the two counts; see [`measure_one`].
fn assembled_lines_of(
    block: &Block,
    is_collapsed: bool,
    width: usize,
    carry_in: bool,
    hl: Hl,
) -> (usize, Vec<Line<'static>>) {
    let body = render::block_body(block, is_collapsed, width, hl);
    let assembled = render::assemble_one(body, carry_in);
    let rows = assembled.len();
    let tags = vec![0usize; rows];
    (rows, wrap::wrap_all_tagged(&assembled, &tags, width).0)
}

/// Footer measurement is in terminal COLUMNS, not `char`s. Session titles are agent-supplied
/// (#106) and routinely CJK — "初筛候选人简历" is 7 chars but 14 columns, so counting chars
/// would let the footer overrun its line and wrap.
fn cols(s: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(s)
}

/// Truncate to at most `max` columns, spending one of them on the `…` when anything is dropped.
fn clip_cols(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

/// How many columns an agent-supplied session NAME may claim (a stem is exempt). A title is unbounded agent-supplied text (#106)
/// sharing one line with position, %, and the token/cost run, and it carries `LOC_PRIO` — the
/// truncate-LAST rank. Uncapped, that rank is backwards: a 90-column name would evict every
/// metric to keep itself whole, when the metrics are why the footer exists. So the title is
/// capped BEFORE it enters the shed set, and only then ranked last.
fn title_budget(avail: usize) -> usize {
    (avail / 3).clamp(12, 40)
}

/// Fit-and-shed the footer's LEFT run to `avail` columns: drop the highest-priority
/// (largest number) droppable segment (`1..LOC_PRIO`) first, repeat until it fits;
/// priority-0 segments (nav labels, live-state, position) never drop; the location
/// (`LOC_PRIO`) is truncated with `…` only as a last resort. Returns the survivors, in
/// order. Segment shed order matches the spec: cached(1) → %(2) → model(3) → in(4) →
/// out(5) → duration(6) → cost(7).
fn shed_footer(mut segs: Vec<(String, u8)>, avail: usize) -> Vec<(String, u8)> {
    let joined = |segs: &[(String, u8)]| -> usize {
        segs.iter().map(|(t, _)| cols(t)).sum::<usize>() + segs.len().saturating_sub(1) * 3
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
            *loc = clip_cols(loc, cols(loc).saturating_sub(over));
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

/// Does this line carry a diff add/del background (i.e. needs the `INSET` margin)?
/// Computed from the *original* line, before search/focus recolor overwrites the
/// bg — otherwise a matched diff row would lose its inset and shift left.
fn is_diff_line(line: &Line<'static>) -> bool {
    matches!(
        line.spans.last().and_then(|s| s.style.bg),
        Some(c) if c == theme::diff_add_bg() || c == theme::diff_del_bg()
    )
}

/// A display row's plain text, for the search-row check in `draw`.
fn row_text(line: &Line<'static>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
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
    Descend(crate::model::BlockIndex),
}

/// A resolved descend target — the agent to open, plus any pre-loaded child transcript
/// (a spawn carries it; a completion event loads it lazily). Built by [`View::descend_ref_at`].
pub struct DescendRef {
    pub agent_id: String,
    pub agent_type: String,
    pub blocks: Vec<Block>,
    pub subtree_cost: Option<crate::model::UsdCost>,
}

/// The outcome of a mouse click while the `a` active-sub-agents popup is open.
#[derive(Debug, PartialEq, Eq)]
pub enum PopupClick {
    /// Clicked an agent row — descend into the sub-agent at this block index.
    Descend(crate::model::BlockIndex),
    /// Clicked elsewhere on the overlay — swallowed, popup stays open (`Esc`/`a` closes).
    Border,
    /// No popup was open.
    None,
}

/// Derived per-session view state that outlives an evicted frame (#75): the measure pass's
/// outputs + the user's interaction state, keyed in the [`SessionCache`](claude_replay_present::SessionCache)
/// aux slot. Deliberately NOT in the session's `BlockStore::Bv`: geometry is a function of
/// (width, fold state), which change interactively — put-once storage can't hold it, an
/// invalidatable cache-level sidecar can.
pub struct ViewSidecar {
    width: u16,
    heights: Vec<usize>,
    prefix: Vec<usize>,
    collapsed: Vec<bool>,
    user_folds: std::collections::HashMap<crate::model::BlockIndex, bool>,
    scroll: usize,
    follow: bool,
}

pub struct View {
    blocks: Vec<std::sync::Arc<Block>>,
    collapsed: Vec<bool>, // per-block fold state
    /// Explicit user fold gestures by block index (#61) — re-applied over the
    /// policy-derived defaults whenever a live update re-folds the tail, so an
    /// expansion the user made survives incoming blocks. Position-keyed: across a
    /// tail reshape the override lands on whatever block now holds that index.
    user_folds: std::collections::HashMap<crate::model::BlockIndex, bool>,
    /// First block whose display geometry is stale (fold toggle / live update); `layout`
    /// re-measures from here. `None` = geometry current.
    dirty_from: Option<usize>,
    /// Wrapped display height of each block at (`width`, fold state) — the scroll-math index.
    /// Rendered lines are NOT retained per block; only the visible window is (see `hot`).
    heights: Vec<usize>,
    /// Prefix sums over `heights`: `prefix[b]` = first wrapped line of block `b`;
    /// `prefix[len]` = the total wrapped-line count. Line ↔ block mapping is a binary search.
    prefix: Vec<usize>,
    /// Bounded cache of rendered+wrapped lines for the blocks near the viewport — the ONLY
    /// styled-line residency (O(window), not O(session)). Evicted by distance when it grows.
    hot: std::collections::HashMap<crate::model::BlockIndex, Vec<Line<'static>>>,
    width: u16,
    view_h: usize, // content rows (area height - 1 status row)
    scroll: usize, // top wrapped-line index
    follow: bool,  // pinned to bottom
    new_count: usize,
    title: String,
    /// Is `title` an agent-supplied NAME (#106) rather than the session's stem? Only a name is
    /// length-capped: a stem is inherently bounded (a uuid is 36 columns) and truncating it would
    /// destroy the one thing that identifies the session, while a name is unbounded prose.
    title_named: bool,
    live: bool,
    // search (P6)
    query: String,   // current needle (empty = no search)
    searching: bool, // in `/` input mode
    /// Blocks containing ≥1 query occurrence (#84: block-level, fold-independent).
    matches: Vec<crate::model::BlockIndex>,
    /// Total occurrence count across all hit blocks (the status tally).
    occurrences: usize,
    /// The transiently peek-expanded hit block, if any (vim `foldopen=search`).
    peeked: Option<crate::model::BlockIndex>,
    match_pos: usize, // index into `matches`
    /// The viewport top when the CURRENT search began — the anchor the initial match is chosen
    /// against on every keystroke (#108 enhancement). Anchoring at the start (not at the live
    /// `scroll`) stops incremental typing from drifting: each jump moves `scroll`, and re-picking
    /// relative to the moved viewport would skip matches between the anchor and wherever the last
    /// partial query landed.
    search_origin: usize,
    metrics: String, // footer text (tokens/cost/duration/model) — legacy string
    footer_segs: Vec<(String, u8)>, // droppable footer metric parts (text, shed priority)
    descended: bool, // this view is a descended sub-agent (footer shows `esc back`)
    fold: FoldPolicy, // per-type default fold policy (applied to new content)
    focus: Option<crate::model::BlockIndex>, // focused foldable block index ([ / ] / hover)
    show_help: bool, // `?` help overlay visible
    agents_popup: Option<usize>, // `a` active-sub-agents popup: selected row, when open
    /// The `t` task/todo panel (#15): selected row, when open. The list itself is
    /// `tasks` — fed by the app (op-log state merged with the live task files).
    tasks_popup: Option<usize>,
    tasks: crate::engine::TaskList,
    can_go_back: bool, // launched via the picker → Esc returns to the session list
    can_open_picker: bool, // `s` opens the session switcher overlay (--latest launch)
    switcher: Option<Picker>, // session switcher overlay, when open
    // mouse text selection (wrapped-line coords, so it survives scrolling):
    sel_anchor: Option<(usize, usize)>, // (wrapped line, display col) where drag began
    sel_cursor: Option<(usize, usize)>, // current drag end; None until the mouse moves
    cwd: Option<PathBuf>, // session working dir — reverses a header's relativized path
    flash: Option<String>, // transient status (e.g. "Saved to …"); cleared on next input
    // The transcript these blocks were parsed from — the source for loading a `Deferred`
    // attachment's bytes on demand (this view holds only locators, never the content).
    source: Option<Transcript>,
}

impl View {
    pub fn new(blocks: Vec<Block>, title: impl Into<String>, live: bool, fold: FoldPolicy) -> Self {
        Self::new_shared(
            blocks.into_iter().map(std::sync::Arc::new).collect(),
            title,
            live,
            fold,
        )
    }

    /// [`new`](Self::new) over already-shared blocks (#84): the live path hands `Arc` clones
    /// of the cache's authoritative copy — the view holds pointers, content stays single-copy.
    pub fn new_shared(
        blocks: Vec<std::sync::Arc<Block>>,
        title: impl Into<String>,
        live: bool,
        fold: FoldPolicy,
    ) -> Self {
        let collapsed: Vec<bool> = blocks.iter().map(|b| fold.collapses(b)).collect();
        // Raw is built lazily on the first `layout`, once the real terminal width
        // is known — rendering here (at width 0) would be thrown away and re-done,
        // doubling the (expensive) syntax-highlight pass at startup.
        Self {
            blocks,
            collapsed,
            user_folds: std::collections::HashMap::new(),
            dirty_from: Some(0),
            heights: Vec::new(),
            prefix: vec![0],
            hot: std::collections::HashMap::new(),
            width: 0,
            view_h: 0,
            scroll: 0,
            follow: true,
            new_count: 0,
            title: title.into(),
            title_named: false,
            live,
            query: String::new(),
            searching: false,
            matches: Vec::new(),
            occurrences: 0,
            peeked: None,
            match_pos: 0,
            search_origin: 0,
            metrics: String::new(),
            footer_segs: Vec::new(),
            descended: false,
            fold,
            focus: None,
            show_help: false,
            agents_popup: None,
            tasks_popup: None,
            tasks: crate::engine::TaskList::default(),
            can_go_back: false,
            can_open_picker: false,
            switcher: None,
            sel_anchor: None,
            sel_cursor: None,
            cwd: None,
            flash: None,
            source: None,
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
    fn active_agent_indices(&self) -> Vec<crate::model::BlockIndex> {
        // Terminal-ness from the sub-agent index (derived from the spawn + finish events),
        // not the spawn block's status — the step off reading a back-patched block. Equivalent:
        // an empty-id / running spawn has no terminal map entry, so it stays "active".
        let agents = crate::engine::build_sub_agents(&self.blocks);
        self.blocks
            .iter()
            .enumerate()
            .filter(|(_, b)| {
                matches!(&***b, Block::SubAgent(sa)
                    if !agents.get(&sa.agent_id).map(|m| m.status.is_terminal()).unwrap_or(false))
            })
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
    // --- the `t` task/todo panel (#15) ---
    /// Replace the panel's task list (the app feeds the merged live+op-log state on
    /// load and on every live poll).
    pub fn set_tasks(&mut self, tasks: crate::engine::TaskList) {
        self.tasks = tasks;
        // Keep the selection in range across live shrinks.
        if let Some(sel) = self.tasks_popup {
            let n = self.tasks.items.len();
            self.tasks_popup = Some(if n == 0 { 0 } else { sel.min(n - 1) });
        }
    }
    /// The panel's current list (for the app's re-merge when no follower is resident).
    pub fn tasks_snapshot(&self) -> crate::engine::TaskList {
        self.tasks.clone()
    }
    pub fn toggle_tasks_popup(&mut self) {
        self.tasks_popup = match self.tasks_popup {
            Some(_) => None,
            None => Some(0),
        };
    }
    pub fn tasks_popup_open(&self) -> bool {
        self.tasks_popup.is_some()
    }
    pub fn tasks_popup_close(&mut self) {
        self.tasks_popup = None;
    }
    pub fn tasks_popup_move(&mut self, dir: isize) {
        let n = self.tasks.items.len();
        if let (Some(sel), true) = (self.tasks_popup, n > 0) {
            self.tasks_popup = Some((sel as isize + dir).rem_euclid(n as isize) as usize);
        }
    }

    pub fn agents_popup_confirm(&mut self) -> Option<crate::model::BlockIndex> {
        let sel = self.agents_popup.take()?;
        self.active_agent_indices().get(sel).copied()
    }
    /// Hit-test a mouse click against the open `a` popup (a full-content-area overlay:
    /// header at row 0, one agent per row from row 1, footer at the bottom — mirrors
    /// `render_agents_popup`). A click on an agent row selects + descends into it; any
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
    /// Record the transcript these blocks came from — the source for loading a `Deferred`
    /// attachment's bytes on demand when the reader downloads one.
    pub fn set_source(&mut self, source: Option<Transcript>) {
        self.source = source;
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
    /// Adopt an agent-supplied session NAME (#106) in place of the stem. Marked as a name so the
    /// footer caps its length (`title_budget`); a stem is left whole.
    pub fn set_session_name(&mut self, name: impl Into<String>) {
        self.title = name.into();
        self.title_named = true;
    }

    /// Confirm the switcher's selection: close the overlay and return the chosen
    /// transcript path (None if there was no selection).
    pub fn switcher_confirm(&mut self) -> Option<PathBuf> {
        let path = self.switcher.as_ref().and_then(|p| p.selected_path());
        self.switcher = None;
        path
    }

    /// Render + wrap ONE block's display lines in isolation — the windowed viewer's unit.
    /// `carry_in` is `assemble`'s one cross-block fact (has any earlier block emitted a line);
    /// see [`render::assemble_one`]. Transient: the caller decides whether to retain the result
    /// (the `hot` window cache) or just measure it (heights).
    fn wrapped_block_lines(&self, b: usize, carry_in: bool) -> Vec<Line<'static>> {
        let is_collapsed =
            self.collapsed.get(b).copied().unwrap_or(false) && render::foldable(&self.blocks[b]);
        wrapped_lines_of(&self.blocks[b], is_collapsed, self.width as usize, carry_in)
    }

    /// Block `b`'s wrapped HEIGHT, without syntax-highlighting it when that provably cannot
    /// change the answer.
    ///
    /// Measuring used to render the block for real and throw the styled lines away, and syntect
    /// parsing (~150 µs/line) is what that costs: on this project's 107 MB session, `edit`
    /// blocks are 16% of the blocks and 97% of the measure time (#107 follow-up). But a row's
    /// height depends only on its WIDTH, and `Hl::Plain` produces the very same rows with the
    /// very same text — it differs from `Hl::Styled` in colour alone.
    ///
    /// Span segmentation does matter in exactly one place: `wrap_line` breaks words per span, so
    /// a row wide enough to wrap can break differently depending on how the highlighter split
    /// it. So the plain count is trusted **only when nothing wrapped** — then every row fitted
    /// the width, and a row that fits occupies one line however it was segmented. Anything else
    /// falls back to the real, highlighted measure. Measured: 6.6% of edit blocks fall back.
    fn measure_block(&self, b: usize, carry_in: bool) -> usize {
        let is_collapsed =
            self.collapsed.get(b).copied().unwrap_or(false) && render::foldable(&self.blocks[b]);
        measure_one(&self.blocks[b], is_collapsed, self.width as usize, carry_in)
    }

    /// The blocks, for the `switch_cost` probe to attribute measure time by kind. Measurement
    /// scaffolding, not view API.
    #[cfg(test)]
    pub(crate) fn blocks_for_measure(&self) -> &[std::sync::Arc<Block>] {
        &self.blocks
    }

    /// Measure ONE block exactly as the layout pass does, returning its height — the
    /// `switch_cost` probe's unit of attribution.
    #[cfg(test)]
    pub(crate) fn measure_one_for_probe(&self, b: usize) -> usize {
        self.wrapped_block_lines(b, true).len()
    }

    /// `(fast measure, real render height, took the fallback)` for block `b` — lets the
    /// `switch_cost` probe assert the measure/render equality over a whole real transcript.
    #[cfg(test)]
    pub(crate) fn measure_check_for_probe(&self, b: usize) -> (usize, usize, bool) {
        let is_collapsed =
            self.collapsed.get(b).copied().unwrap_or(false) && render::foldable(&self.blocks[b]);
        let w = self.width as usize;
        let (rows, wrapped) = assembled_lines_of(
            &self.blocks[b],
            is_collapsed,
            w,
            true,
            Hl::Measure { width: w },
        );
        (
            measure_one(&self.blocks[b], is_collapsed, w, true),
            self.wrapped_block_lines(b, true).len(),
            wrapped.len() != rows,
        )
    }

    /// `(rows before wrapping, rows after)` for block `b` — the probe that decides whether
    /// skipping the highlighter is worth building: a row only needs its spans measured when it
    /// is wide enough to wrap, and this says how often that happens.
    #[cfg(test)]
    pub(crate) fn wrap_ratio_for_probe(&self, b: usize) -> (usize, usize) {
        let is_collapsed =
            self.collapsed.get(b).copied().unwrap_or(false) && render::foldable(&self.blocks[b]);
        let body = render::block_body(
            &self.blocks[b],
            is_collapsed,
            self.width as usize,
            Hl::Styled,
        );
        let assembled = render::assemble_one(body, true);
        let pre = assembled.len();
        let tags = vec![0usize; pre];
        let post = wrap::wrap_all_tagged(&assembled, &tags, self.width as usize)
            .0
            .len();
        (pre, post)
    }

    /// Wrapped HEIGHTS for `[from, to)`, every block of which is known to have `carry_in = true`
    /// — spread across the machine's cores. Rendering a block is pure (it reads the block, the
    /// fold bit and the width, and shares only syntect's immutable `SyntaxSet`), so the split is
    /// invisible: each block produces the same height it would have produced serially.
    ///
    /// Worth it only in bulk — the first layout of a large session, which is the 4.9 s this
    /// exists to cut (#107). A live tail re-measures a handful of blocks and stays serial, where
    /// spawning would cost more than the work.
    fn measure_parallel(&self, from: usize, to: usize) -> Vec<usize> {
        const MIN_PER_THREAD: usize = 64;
        let n = to - from;
        let workers = std::thread::available_parallelism()
            .map_or(1, |p| p.get())
            .min(n.div_ceil(MIN_PER_THREAD));
        if workers <= 1 {
            return (from..to).map(|b| self.measure_block(b, true)).collect();
        }
        let (blocks, folds, width) = (&self.blocks, &self.collapsed, self.width as usize);
        let mut out = vec![0usize; n];
        std::thread::scope(|sc| {
            for (k, slice) in out.chunks_mut(n.div_ceil(workers)).enumerate() {
                let base = from + k * n.div_ceil(workers);
                sc.spawn(move || {
                    for (i, h) in slice.iter_mut().enumerate() {
                        let b = base + i;
                        let is_collapsed =
                            folds.get(b).copied().unwrap_or(false) && render::foldable(&blocks[b]);
                        *h = measure_one(&blocks[b], is_collapsed, width, true);
                    }
                });
            }
        });
        out
    }

    /// Re-measure display geometry from block `d` on: render each block once, record its wrapped
    /// height + plain text (the search index), and DROP the styled lines — the resident geometry
    /// is O(#blocks) integers + content-sized text, never O(session) styled lines. `[0..d]` keeps
    /// its heights/prefix/text (the live-tail unchanged prefix).
    fn measure_from(&mut self, d: usize) {
        let d = d.min(self.blocks.len()).min(self.heights.len());
        self.heights.truncate(d);
        self.prefix.truncate(d + 1);
        self.hot.retain(|&b, _| b < d);
        let mut total = self.prefix[d];
        let mut carry = self.prefix[d] > 0;
        // `carry_in` is a prefix-OR — "has anything before me emitted a line" — so it can only go
        // false → true, once. Walk serially until it flips (usually the very first block), and
        // from there every remaining block has the SAME carry_in and the order stops mattering.
        //
        // Today this prefix changes no height: `assemble_one` consults `carry_in` only to decide
        // whether a body's OPENING blank line is a duplicate, and no current block kind opens
        // blank (`assemble_one_drops_a_leading_blank_only_when_something_preceded_it` pins the
        // mechanism). It is kept because the fold contract says carry_in is an input, and this is
        // what keeps the parallel split honest the day a block does open blank.
        let n = self.blocks.len();
        let mut b = d;
        let mut measured: Vec<usize> = Vec::with_capacity(n - d);
        while b < n && !carry {
            let h = self.measure_block(b, carry);
            carry |= h > 0;
            measured.push(h);
            b += 1;
        }
        if b < n {
            measured.extend(self.measure_parallel(b, n));
        }
        for h in measured {
            total += h;
            self.heights.push(h);
            self.prefix.push(total);
        }
    }

    /// Total wrapped display lines (the scroll range) — O(1) off the prefix sums.
    fn total_wrapped(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0)
    }

    /// The block owning wrapped line `line` — a binary search over the prefix sums (replaces the
    /// retired O(N) `wrapped_tag` vector).
    fn tag_of(&self, line: usize) -> Option<crate::model::BlockIndex> {
        (line < self.total_wrapped()).then(|| self.prefix.partition_point(|&p| p <= line) - 1)
    }

    /// The first wrapped line of block `b`, if it renders any lines at the current fold state.
    fn block_start(&self, b: crate::model::BlockIndex) -> Option<usize> {
        (self.heights.get(b).copied().unwrap_or(0) > 0).then(|| self.prefix[b])
    }

    /// Wrapped line `i`, rendered on demand through the bounded `hot` window cache.
    fn line_at(&mut self, i: usize) -> Option<Line<'static>> {
        let b = self.tag_of(i)?;
        let off = i - self.prefix[b];
        if !self.hot.contains_key(&b) {
            let lines = self.wrapped_block_lines(b, self.prefix[b] > 0);
            self.evict_hot();
            self.hot.insert(b, lines);
        }
        self.hot.get(&b).and_then(|ls| ls.get(off)).cloned()
    }

    /// Bound the window cache: past the cap, keep only blocks near the viewport (visible ±
    /// margin). Called before an insert, so the cache stays O(window).
    fn evict_hot(&mut self) {
        const HOT_CAP: usize = 96;
        const MARGIN: usize = 8;
        if self.hot.len() < HOT_CAP {
            return;
        }
        let total = self.total_wrapped();
        let lo = self
            .tag_of(self.scroll.min(total.saturating_sub(1)))
            .unwrap_or(0)
            .saturating_sub(MARGIN);
        let hi = self
            .tag_of((self.scroll + self.view_h).min(total.saturating_sub(1)))
            .unwrap_or(usize::MAX)
            .saturating_add(MARGIN);
        self.hot.retain(|&b, _| b >= lo && b <= hi);
    }

    /// Re-measure ONE toggled block in place: its height changes, every later block's prefix
    /// shifts by the delta (integer adds), and only its own lines re-render — O(one block) of
    /// syntax-highlighting per fold toggle. Falls back to a full re-measure when the block's
    /// emptiness flips (its blank-carry could affect a later block's leading blank).
    fn remeasure_block(&mut self, b: usize) {
        if b >= self.heights.len() {
            self.dirty_from = Some(self.dirty_from.map_or(b, |d| d.min(b)));
            return;
        }
        let old_h = self.heights[b];
        let wrapped = self.wrapped_block_lines(b, self.prefix[b] > 0);
        let new_h = wrapped.len();
        if (old_h == 0) != (new_h == 0) {
            self.measure_from(b); // emptiness flip can ripple the blank-carry
            return;
        }
        self.hot.insert(b, wrapped);
        self.heights[b] = new_h;
        if new_h != old_h {
            let delta = new_h as isize - old_h as isize;
            for p in &mut self.prefix[b + 1..] {
                *p = (*p as isize + delta) as usize;
            }
        }
    }

    /// Mark display geometry stale from block 0 (fold-all / policy changes); the next `layout`
    /// re-measures.
    fn rebuild_raw(&mut self) {
        self.dirty_from = Some(0);
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
        self.total_wrapped()
    }
    #[cfg(test)]
    pub fn view_h(&self) -> usize {
        self.view_h
    }
    #[cfg(test)]
    pub fn is_collapsed(&self, i: crate::model::BlockIndex) -> bool {
        self.collapsed[i]
    }
    /// The fold-key of every top-level block (for asserting live-tail grouping).
    #[cfg(test)]
    pub fn block_kinds(&self) -> Vec<&'static str> {
        self.blocks
            .iter()
            .map(|b| crate::model::fold_key(b))
            .collect()
    }
    /// The source-block index that wrapped line `line` was rendered from.
    #[cfg(test)]
    pub fn block_of_line(&self, line: usize) -> Option<crate::model::BlockIndex> {
        self.tag_of(line)
    }

    fn max_scroll(&self) -> usize {
        self.total_wrapped().saturating_sub(self.view_h)
    }

    /// Content rows (excludes the status row) — for mouse-click hit-testing.
    pub fn content_rows(&self) -> usize {
        self.view_h
    }

    /// Force a re-wrap on the next layout (after a resize or new content).
    pub fn invalidate_wrap(&mut self) {
        self.width = 0;
    }

    /// Extract this view's **sidecar** (#75): the expensive measure pass's outputs (heights,
    /// prefix sums, the search-text index) plus the user's interaction state (fold toggles,
    /// scroll, follow) — everything worth keeping when the frame LRU evicts the view. Stored
    /// in the `SessionCache`'s aux slot; re-installed by [`adopt_sidecar`](Self::adopt_sidecar).
    pub fn into_sidecar(self) -> ViewSidecar {
        ViewSidecar {
            width: self.width,
            heights: self.heights,
            prefix: self.prefix,
            collapsed: self.collapsed,
            user_folds: self.user_folds,
            scroll: self.scroll,
            follow: self.follow,
        }
    }

    /// Re-install an evicted frame's sidecar onto a freshly-rebuilt view of the same session.
    /// Valid only while the geometry's inputs still match: the block count must equal the
    /// sidecar's (a session that grew while evicted re-measures instead). Width validity is
    /// free: the sidecar's width is installed, so the first `layout()` at any OTHER width
    /// takes the width-changed path and re-measures — the existing sentinel mechanics.
    pub fn adopt_sidecar(&mut self, sc: ViewSidecar) -> bool {
        if sc.heights.len() != self.blocks.len() || sc.collapsed.len() != self.blocks.len() {
            return false; // the session changed shape while evicted — fresh measure
        }
        self.width = sc.width;
        self.heights = sc.heights;
        self.prefix = sc.prefix;
        self.collapsed = sc.collapsed;
        self.user_folds = sc.user_folds;
        self.scroll = sc.scroll;
        self.follow = sc.follow;
        self.dirty_from = None; // geometry is valid as adopted
        true
    }

    /// Compute geometry for a given area (call before scroll math in handlers).
    pub fn layout(&mut self, width: u16, height: u16) {
        self.view_h = height.saturating_sub(1) as usize;
        let width_changed = width != self.width;
        if width_changed {
            self.width = width;
        }
        // Re-measure geometry on a width change (width-aware wraps) or stale content.
        // `width == 0` is the invalidation sentinel. A plain scroll re-measures nothing.
        let dirty = if width_changed {
            Some(0)
        } else {
            self.dirty_from.take()
        };
        if let Some(d) = dirty {
            self.dirty_from = None;
            if width_changed {
                self.hot.clear(); // widths baked into every cached line
            }
            self.measure_from(if width_changed { 0 } else { d });
            // Hit blocks may change on a geometry rebuild (live tail growth) — rescan.
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
    pub fn toggle_block(&mut self, i: crate::model::BlockIndex) {
        if self
            .blocks
            .get(i)
            .map(|b| render::foldable(b))
            .unwrap_or(false)
        {
            if let Some(c) = self.collapsed.get_mut(i) {
                *c = !*c;
                // An explicit user gesture (#61): pin it so a live tail re-fold
                // (apply_from re-deriving policy defaults) can't undo it.
                self.user_folds.insert(i, *c);
            }
            // O(one block): re-render/measure only the toggled block; later blocks shift by
            // an integer delta in the prefix sums.
            self.remeasure_block(i);
        }
    }
    /// Toggle the block at the top of the viewport (the `t` key).
    pub fn toggle_at_cursor(&mut self) {
        // Prefer the focused foldable (set by `[`/`]`/hover); otherwise the first
        // foldable block visible in the viewport. (The block exactly at the top line
        // is usually non-foldable — e.g. assistant text — which made `t` look dead.)
        if let Some(f) = self.focus.filter(|&i| {
            self.blocks
                .get(i)
                .map(|b| render::foldable(b))
                .unwrap_or(false)
        }) {
            self.toggle_block(f);
            return;
        }
        let total = self.total_wrapped();
        let end = (self.scroll + self.view_h.max(1)).min(total);
        let Some(mut b) = self.tag_of(self.scroll.min(total.saturating_sub(1))) else {
            return;
        };
        while b < self.blocks.len() && self.prefix[b] < end {
            if self
                .blocks
                .get(b)
                .map(|b| render::foldable(b))
                .unwrap_or(false)
            {
                self.toggle_block(b);
                return;
            }
            b += 1;
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
                // Also a user intent — pinned like a single toggle (#61).
                self.user_folds.insert(i, any_expanded);
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
        let b = self.tag_of(idx)?;
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
            self.blocks.get(b).map(|b| &**b),
            Some(Block::SubAgent(_) | Block::AgentDone { .. })
        ) {
            self.toggle_block(b);
            return None;
        }
        self.activate_block(b)
    }

    /// Is the click at `(idx, col)` on a spawn / completion header's descend-target agent
    /// id? Works for both the `SubAgent` spawn and the `AgentDone` completion (any status).
    fn agent_id_hit(&self, b: crate::model::BlockIndex, idx: usize, col: usize) -> bool {
        // Only the header's own first row carries the id.
        if idx != 0 && self.tag_of(idx - 1) == Some(b) {
            return false;
        }
        let span = match self.blocks.get(b).map(|b| &**b) {
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
    fn activate_block(&mut self, b: crate::model::BlockIndex) -> Option<Action> {
        match self.blocks.get(b).map(|b| &**b) {
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
                if matches!(a.content, AttachmentContent::Deferred { .. }) {
                    self.flash = Some(match save_attachment(&a, self.source.as_ref()) {
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
    pub fn subagent_at(&self, b: crate::model::BlockIndex) -> Option<&crate::model::SubAgent> {
        match self.blocks.get(b).map(|b| &**b) {
            Some(Block::SubAgent(sa)) => Some(sa),
            _ => None,
        }
    }

    /// The descend target at block index `b` — a `SubAgent` spawn (with any pre-loaded
    /// child `blocks`) OR an `AgentDone` completion (whose child always loads lazily from
    /// its file). `None` for any other block or a missing agent id.
    pub fn descend_ref_at(&self, b: crate::model::BlockIndex) -> Option<DescendRef> {
        match &**self.blocks.get(b)? {
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
    pub fn focus_block(&mut self, b: crate::model::BlockIndex) {
        if b < self.blocks.len() {
            self.focus = Some(b);
            self.scroll_block_into_view(b);
        }
    }

    /// The absolute file path a click at `(idx, col)` lands on, if `idx` is the
    /// first (header) row of a tool block and `col` falls within its `(target)`
    /// span — and the resolved path actually exists (so a `Bash(ls)` command or a
    /// `Grep(pattern)` header never masquerades as a file). Else `None`.
    fn header_path_hit(
        &self,
        b: crate::model::BlockIndex,
        idx: usize,
        col: usize,
    ) -> Option<PathBuf> {
        // Only the header's own first row carries the path.
        if idx != 0 && self.tag_of(idx - 1) == Some(b) {
            return None;
        }
        let Block::ToolUse { name, target, .. } = &**self.blocks.get(b)? else {
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
    fn focusable_blocks(&self) -> Vec<crate::model::BlockIndex> {
        (0..self.blocks.len())
            .filter(|&i| {
                render::foldable(&self.blocks[i]) || matches!(*self.blocks[i], Block::Attachment(_))
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
    fn scroll_block_into_view(&mut self, b: crate::model::BlockIndex) {
        if let Some(idx) = self.block_start(b) {
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

    /// Show a transient one-line notice in the status row — the same surface the `y`
    /// attachment-save path uses, cleared on the next input. This is the TUI's only
    /// in-viewer message channel (#110): anything that must be SAID without leaving the
    /// session (a refused switch, a failed reveal) goes through here.
    pub fn set_flash(&mut self, msg: impl Into<String>) {
        self.flash = Some(msg.into());
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
        if let Some(b) = self.tag_of(idx) {
            if render::foldable(&self.blocks[b]) {
                self.focus = Some(b);
            }
        }
    }
    #[cfg(test)]
    pub fn focused_block(&self) -> Option<crate::model::BlockIndex> {
        self.focus
    }

    // --- search (P6) ---
    pub fn is_searching(&self) -> bool {
        self.searching
    }
    /// Source-of-truth search (#84): DISCOVERY scans every block's text content — the one
    /// shared in-memory copy, which exists precisely to make this fast — so matches inside
    /// FOLDED blocks are found (the old display-text index was fold-blind). Extraction is
    /// string-only (no render, no wrap, no styling); position mapping stays lazy (the jump
    /// target's block start comes from the prefix sums; visible highlighting happens during
    /// the normal draw of the hot window).
    fn recompute_matches(&mut self) {
        self.matches.clear();
        self.occurrences = 0;
        if self.query.is_empty() {
            return;
        }
        let q = self.query.to_lowercase();
        for (i, b) in self.blocks.iter().enumerate() {
            let n = block_occurrences(b, &q);
            if n > 0 {
                self.matches.push(i);
                self.occurrences += n;
            }
        }
        if self.match_pos >= self.matches.len() {
            self.match_pos = 0;
        }
    }

    /// Navigate to the current hit block. A hit inside a FOLDED block **peek-expands** it
    /// (vim's `foldopen=search` behaviour): the expansion is transient — stepping away
    /// re-collapses it unless the user touched it (a toggle records a `user_folds` entry,
    /// which converts the peek to sticky). `Enter` (keep) also stickies the current peek;
    /// `Esc` (cancel) restores it.
    fn jump_to_current_match(&mut self) {
        self.unpeek();
        let Some(&b) = self.matches.get(self.match_pos) else {
            return;
        };
        if self.collapsed.get(b).copied().unwrap_or(false) && render::foldable(&self.blocks[b]) {
            self.collapsed[b] = false; // peek — deliberately NOT a user_folds gesture
            self.peeked = Some(b);
            self.dirty_from = Some(self.dirty_from.map_or(b, |d| d.min(b)));
        }
        // The block's first display line is exact without any wrapping (prefix sums); the
        // peek expansion only changes heights AFTER this block, so the target is stable.
        // A hit whose block already STARTS on screen is left where it is — the reader is
        // looking at it; yanking it to the top would turn "the match is right there" into a
        // viewport jump. Off-screen hits scroll so the block starts at the top, as before.
        if let Some(&line) = self.prefix.get(b) {
            let in_view = line >= self.scroll && line < self.scroll + self.view_h;
            if !in_view {
                self.scroll = line.min(self.max_scroll());
            }
            self.follow = false;
        }
    }

    /// Restore a transient peek expansion, unless the user made it sticky by toggling it
    /// (which records a `user_folds` entry).
    fn unpeek(&mut self) {
        if let Some(p) = self.peeked.take() {
            if !self.user_folds.contains_key(&p)
                && p < self.collapsed.len()
                && self
                    .blocks
                    .get(p)
                    .map(|b| render::foldable(b))
                    .unwrap_or(false)
            {
                self.collapsed[p] = true;
                self.dirty_from = Some(self.dirty_from.map_or(p, |d| d.min(p)));
            }
        }
    }
    pub fn search_start(&mut self) {
        self.searching = true;
        self.query.clear();
        self.matches.clear();
        self.match_pos = 0;
        self.search_origin = self.scroll;
    }
    /// The match the search should START on: [`first_match_from`](Self::first_match_from) the
    /// viewport top where the search was opened. (Matches vim's `/`: search forward from here,
    /// wrap at the end.)
    fn initial_match(&self) -> usize {
        self.first_match_from(self.search_origin)
    }
    /// The first hit block that BEGINS at or below display row `origin` — the forward entry
    /// point into the match cycle for a viewport anchored there: the nearest hit that is on
    /// screen or reached by scrolling down. Every hit block strictly above wraps around to the
    /// end of the cycle, so when the whole document's hits are behind the reader the pick loops
    /// to the first.
    fn first_match_from(&self, origin: usize) -> usize {
        let below = self
            .matches
            .partition_point(|&b| self.prefix.get(b).copied().unwrap_or(0) < origin);
        if below == self.matches.len() {
            0 // every hit is above the viewpoint — loop around
        } else {
            below
        }
    }
    /// The backward mirror of [`first_match_from`](Self::first_match_from): the LAST hit block
    /// that begins above display row `limit` (exclusive) — the bottom-most on-screen hit for a
    /// `limit` at the viewport bottom, else the nearest hit above. Wraps to the final hit when
    /// every one is below.
    fn last_match_before(&self, limit: usize) -> usize {
        let above = self
            .matches
            .partition_point(|&b| self.prefix.get(b).copied().unwrap_or(0) < limit);
        if above == 0 {
            self.matches.len() - 1 // every hit is below the viewport — loop around
        } else {
            above - 1
        }
    }
    /// Whether the CURRENT hit's block is anywhere on screen — the test that decides if
    /// `n`/`N` continue the walk or re-anchor it at the viewport. Any overlap counts: a hit
    /// the reader can still see is a position they are still AT; one scrolled fully away is
    /// not, and stepping relative to it would yank the view somewhere unrelated.
    fn current_match_on_screen(&self) -> bool {
        let Some(&b) = self.matches.get(self.match_pos) else {
            return false;
        };
        let top = self.prefix.get(b).copied().unwrap_or(0);
        let h = self.heights.get(b).copied().unwrap_or(0);
        top < self.scroll + self.view_h && top + h > self.scroll
    }
    pub fn search_input(&mut self, c: char) {
        self.query.push(c);
        self.recompute_matches();
        self.match_pos = self.initial_match();
        self.jump_to_current_match();
    }
    pub fn search_backspace(&mut self) {
        self.query.pop();
        self.recompute_matches();
        self.match_pos = self.initial_match();
    }
    pub fn search_confirm(&mut self) {
        self.searching = false; // keep query + highlights
        if let Some(p) = self.peeked.take() {
            // Enter = "I want to stay here": the current hit's expansion becomes sticky.
            self.user_folds.insert(p, false);
        }
    }
    pub fn search_cancel(&mut self) {
        self.unpeek();
        self.searching = false;
        self.query.clear();
        self.matches.clear();
        self.occurrences = 0;
    }
    /// Step to the next hit — FROM THE VIEWPORT when the reader has scrolled away from the
    /// current one (`less`'s `n`, and the same rule that anchors a fresh search at the
    /// viewpoint): the first hit at or below the top, which is the first on-screen hit when
    /// one is visible and the next below otherwise. With the current hit still on screen it
    /// is the plain sequential step. Match order is document order, so re-anchoring only
    /// changes where the cycle is entered — never the walk itself.
    pub fn search_next(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.match_pos = if self.current_match_on_screen() {
            (self.match_pos + 1) % self.matches.len()
        } else {
            self.first_match_from(self.scroll)
        };
        self.jump_to_current_match();
    }
    /// [`search_next`](Self::search_next)'s backward mirror: scrolled away, `N` re-anchors to
    /// the bottom-most hit beginning above the viewport bottom — the last on-screen hit when
    /// one is visible, the nearest above otherwise.
    pub fn search_prev(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.match_pos = if self.current_match_on_screen() {
            (self.match_pos + self.matches.len() - 1) % self.matches.len()
        } else {
            self.last_match_before(self.scroll + self.view_h)
        };
        self.jump_to_current_match();
    }
    #[cfg(test)]
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// Incremental live update (M16): replace the blocks with a fresh `FollowParser` snapshot
    /// while PRESERVING per-block fold toggles and the render cache for the unchanged prefix
    /// — so a live tail doesn't reset the user's expands/collapses or re-render settled
    /// blocks each poll. Only the changed/appended tail (a back-patched tool block, new
    /// turns) recomputes. This is the sole live path: the `FollowParser` snapshot is already
    /// fully regrouped (thinking absorbs its tools, immediate-pickup markers suppressed) by the
    /// shared `Replayer`, so the view just diffs and swaps — no view-level re-grouping.
    pub fn update(&mut self, new_blocks: Vec<Block>) {
        // Longest unchanged prefix — keep its fold state and render cache. (Batch/test entry: it
        // re-derives the boundary by scan. The live loop uses `apply_poll`, where the engine hands
        // the boundary in O(turn) — no scan.)
        let d = self
            .blocks
            .iter()
            .zip(&new_blocks)
            .take_while(|(a, b)| a.as_ref() == *b)
            .count();
        self.apply_from(new_blocks, d);
    }

    /// The **single** live-update entry the event loop calls (finding #7): the engine
    /// (`FollowParser::poll_delta`) hands the new blocks, the metrics, and the
    /// exact `changed_from` boundary — so this needs **no** O(N) prefix scan (finding #4) — and it
    /// refreshes the footer in the same call, so a block update can never drift out of sync with the
    /// cost/token footer.
    pub fn apply_poll(&mut self, new_blocks: Vec<Block>, metrics: &Metrics, changed_from: usize) {
        self.apply_from(new_blocks, changed_from);
        self.metrics = metrics.footer();
        self.footer_segs = metrics.footer_segments();
    }

    /// Swap in `new_blocks`, preserving fold toggles + the render cache for `[0..d]` (`d` = the first
    /// changed block) and recomputing only the tail. Shared by the batch `update` and live
    /// `apply_poll`. The snapshot is already fully regrouped by the shared `Replayer`, so this just
    /// diffs and swaps — no view-level re-grouping.
    fn apply_from(&mut self, new_blocks: Vec<Block>, d: usize) {
        let d = d.min(self.blocks.len()).min(new_blocks.len());
        // Live "▼ N new — G to jump" badge: while scrolled back (not following), accumulate
        // the net growth so the reader sees how much arrived below. Follow mode stays pinned
        // to the bottom, so it shows no badge. (A back-patch that doesn't grow bumps nothing.)
        if !self.follow {
            self.new_count += new_blocks.len().saturating_sub(self.blocks.len());
        }
        self.blocks = new_blocks.into_iter().map(std::sync::Arc::new).collect();
        self.post_splice(d);
    }

    /// The **shared-copy** apply (#84/#85): splice the delta in place — keep
    /// `[0..frontier)`, append `Arc` clones of the newly-committed blocks and the fresh
    /// open turn. Content lives once, in the cache's authoritative copy; a live tick costs
    /// O(delta) refcount bumps, not an O(session) content move.
    pub fn apply_view(&mut self, d: claude_replay_present::cache::ViewDelta) {
        let prev_committed = (d.committed_len - d.committed_delta.len()).min(self.blocks.len());
        let joined = d.committed_len + d.provisional.len();
        if !self.follow {
            self.new_count += joined.saturating_sub(self.blocks.len());
        }
        self.blocks.truncate(prev_committed);
        self.blocks.extend(d.committed_delta);
        self.blocks.extend(d.provisional);
        self.post_splice(d.changed_from);
        self.metrics = d.metrics.footer();
        self.footer_segs = d.metrics.footer_segments();
    }

    /// Shared post-splice bookkeeping over `self.blocks`: re-derive fold defaults for the
    /// changed tail, overlay the user's explicit fold gestures (#61 — position-keyed, the
    /// same heuristic the HTML client uses), and mark geometry dirty FROM `d` only — the
    /// next layout re-measures the changed tail (heights + text index), keeping a live poll
    /// O(tail), not O(session).
    fn post_splice(&mut self, d: usize) {
        let d = d.min(self.blocks.len());
        let tail: Vec<bool> = self.blocks[d..]
            .iter()
            .map(|b| self.fold.collapses(b))
            .collect();
        self.collapsed.truncate(d);
        self.collapsed.extend(tail);
        for (&i, &c) in &self.user_folds {
            if i >= d
                && self
                    .blocks
                    .get(i)
                    .map(|b| render::foldable(b))
                    .unwrap_or(false)
                && i < self.collapsed.len()
            {
                self.collapsed[i] = c;
            }
        }
        self.dirty_from = Some(self.dirty_from.map_or(d, |cur| cur.min(d)));
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
                    " search '{}'  block {}/{} · {} hit{}  (n/N next/prev · Esc-then-/ to clear) ",
                    self.query,
                    cur,
                    self.matches.len(),
                    self.occurrences,
                    if self.occurrences == 1 { "" } else { "s" }
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
        let pos = (self.scroll + 1).min(self.total_wrapped().max(1));
        let total = self.total_wrapped().max(1);
        // Build the LEFT run in order, each segment with a shed priority (0 = never
        // drop: the nav labels, live-state, position; LOC_PRIO = the id, truncate last).
        // The key-hint run is never what loses — it's fixed at the right, outside the
        // shed set; the left run sheds to fit the remaining columns.
        const HINT: &str = "?·[ ]·␣↵·/·n·g·q";
        let width = self.width.max(1) as usize;
        let avail = width.saturating_sub(cols(HINT) + 3);
        let mut segs: Vec<(String, u8)> = Vec::new();
        if self.descended {
            segs.push(("↑ esc back".into(), 0)); // ascend hint + (future) click target
        }
        let active = self.active_children();
        if active > 0 {
            segs.push((format!("a active {active}"), 0));
        }
        // The session name (#106) — an agent-supplied title when there is one, else the uuid.
        let name = if self.title_named {
            clip_cols(&self.title, title_budget(avail))
        } else {
            self.title.clone() // a stem: bounded already, and shed's last resort still trims it
        };
        segs.push((name, LOC_PRIO));
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
        let kept = shed_footer(segs, avail);
        let left_w: usize =
            kept.iter().map(|(t, _)| cols(t)).sum::<usize>() + kept.len().saturating_sub(1) * 3;
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
        // One streaming pass: render each block, emit, drop — `--dump` never retains the
        // document's styled lines (same windowed principle, degenerate window). Threads the
        // blank-carry exactly as `layout`'s measure does, so output == the interactive render.
        self.width = width;
        self.dirty_from = Some(0); // a later interactive layout re-measures from scratch
        let mut out = Vec::new();
        let mut carry = false;
        for b in 0..self.blocks.len() {
            let wrapped = self.wrapped_block_lines(b, carry);
            carry |= !wrapped.is_empty();
            for line in wrapped {
                let inset = is_diff_line(&line);
                out.push(fill_bg(line, width as usize, inset));
            }
        }
        out
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
            let line = (self.scroll + row as usize).min(self.total_wrapped().saturating_sub(1));
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
    /// Extract the selected text across wrapped lines, joined by newlines. Renders the selected
    /// range on demand through the window cache (a selection is viewport-sized in practice).
    fn selection_text(&mut self) -> Option<String> {
        let (s, e) = self.sel_bounds()?;
        let last = self.total_wrapped().saturating_sub(1);
        let mut lines = Vec::new();
        for ai in s.0..=e.0.min(last) {
            let c0 = if ai == s.0 { s.1 } else { 0 };
            let c1 = if ai == e.0 { e.1 } else { usize::MAX };
            let line = self.line_at(ai)?;
            lines.push(cols_of_line(&line, c0, c1));
        }
        let text = lines.join("\n");
        (!text.trim().is_empty()).then_some(text)
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        self.layout(area.width, area.height);
        let end = (self.scroll + self.view_h).min(self.total_wrapped());
        let cur = self.matches.get(self.match_pos).copied();
        // Since #84 `matches` holds BLOCK indices (discovery scans block text, not display
        // text), so the row highlight can't come from an index lookup: a row lights up when
        // its own text contains the needle, brighter when its block is the current hit.
        let needle = (!self.query.is_empty()).then(|| self.query.to_lowercase());
        let mut view: Vec<Line> = Vec::new();
        for ai in self.scroll..end {
            let Some(line) = self.line_at(ai) else { break };
            // Detect the diff-inset need from the original line, before search
            // highlighting overwrites the bg (else the matched row shifts left).
            let inset = is_diff_line(&line);
            let styled = match &needle {
                Some(q) if row_text(&line).to_lowercase().contains(q.as_str()) => {
                    highlight_bg(&line, self.tag_of(ai) == cur)
                }
                _ => line,
            };
            let focused = self.focus.is_some() && self.tag_of(ai) == self.focus;
            // The header row is the first wrapped line of the focused block (its
            // predecessor belongs to a different block) — only it gets the focus bar.
            let is_header = focused && (ai == 0 || self.tag_of(ai - 1) != self.focus);
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
        // The `t` task/todo panel (#15).
        if let Some(sel) = self.tasks_popup {
            render_tasks_popup(f, area, &self.tasks, sel);
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

/// The `t` task/todo panel (#15): a full-content-area overlay — a header with the
/// open/total counts, one selectable row per task (status glyph + #id + subject, the
/// in-progress one adding its activeForm), and a details region beneath the list
/// showing the SELECTED task's description + dependency edges (the "see details"
/// requirement — description text is only present when the task file / create op
/// carried it). `j/k` move; `t`/`Esc` close.
fn render_tasks_popup(f: &mut Frame, area: Rect, tasks: &crate::engine::TaskList, sel: usize) {
    use crate::engine::TaskStatus;
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
    let open = tasks
        .items
        .iter()
        .filter(|t| t.status != TaskStatus::Completed)
        .count();
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(area.height as usize);
    lines.push(Line::from(Span::styled(
        pad(format!(
            " tasks — {open} open / {} total",
            tasks.items.len()
        )),
        header,
    )));
    // The details region: ~1/3 of the panel, min 4 rows (only when a task is selected).
    let details_rows = if tasks.items.is_empty() {
        0
    } else {
        ((area.height as usize) / 3).clamp(4, 10)
    };
    let list_rows = (area.height as usize)
        .saturating_sub(2) // header + footer
        .saturating_sub(details_rows);
    // Scroll the list window so the selection stays visible.
    let first = sel.saturating_sub(list_rows.saturating_sub(1));
    for (i, t) in tasks.items.iter().enumerate().skip(first).take(list_rows) {
        let glyph = match t.status {
            TaskStatus::Pending => "○",
            TaskStatus::InProgress => "◐",
            TaskStatus::Completed => "●",
        };
        let active = if t.status == TaskStatus::InProgress && !t.active_form.is_empty() {
            format!(" · {}", t.active_form)
        } else {
            String::new()
        };
        let row = format!(
            "{} {glyph} #{}  {}{active}",
            if i == sel { "❯" } else { " " },
            t.id,
            t.subject
        );
        let style = if i == sel {
            match t.status {
                TaskStatus::Completed => theme::dim().bg(theme::focus_bg()),
                _ => theme::user().bg(theme::focus_bg()),
            }
        } else {
            match t.status {
                TaskStatus::Completed => theme::dim(),
                TaskStatus::InProgress => theme::user(),
                TaskStatus::Pending => theme::status(),
            }
        };
        lines.push(Line::from(Span::styled(pad(row), style)));
    }
    if tasks.items.is_empty() {
        lines.push(Line::from(Span::styled(
            pad("   (no tasks recorded for this session)".to_string()),
            theme::dim(),
        )));
    }
    // Details for the selected task.
    if let Some(t) = tasks.items.get(sel) {
        while lines.len() < (area.height as usize).saturating_sub(1 + details_rows) {
            lines.push(Line::from(""));
        }
        let mut deps = String::new();
        if !t.blocked_by.is_empty() {
            deps.push_str(&format!("  blocked by: {}", t.blocked_by.join(", ")));
        }
        if !t.blocks.is_empty() {
            deps.push_str(&format!("  blocks: {}", t.blocks.join(", ")));
        }
        lines.push(Line::from(Span::styled(
            pad(format!(" #{} · {}{deps}", t.id, t.status.label())),
            header,
        )));
        let body = if t.description.is_empty() {
            "(no recorded description)".to_string()
        } else {
            t.description.clone()
        };
        // Simple greedy char-wrap — good enough for a details pane.
        let w = width.saturating_sub(2).max(8);
        let mut wrapped: Vec<String> = Vec::new();
        for para in body.lines() {
            let mut cur = String::new();
            for ch in para.chars() {
                cur.push(ch);
                if cur.chars().count() >= w {
                    wrapped.push(std::mem::take(&mut cur));
                }
            }
            wrapped.push(cur);
        }
        for l in wrapped.into_iter().take(details_rows.saturating_sub(1)) {
            lines.push(Line::from(Span::styled(format!("  {l}"), theme::status())));
        }
    }
    let footer_row = area.height.saturating_sub(1) as usize;
    while lines.len() < footer_row {
        lines.push(Line::from(""));
    }
    lines.truncate(footer_row);
    lines.push(Line::from(Span::styled(
        pad(" j/k move · t/esc close".to_string()),
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
        ("t", "task/todo panel (session task queue)"),
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
    // The version identifies WHICH build is running (#55 — brew-installed vs
    // target/release is otherwise invisible in the UI).
    let block = WBlock::default()
        .borders(Borders::ALL)
        .border_style(theme::table_border())
        .title(concat!(
            " claude-replay v",
            env!("CARGO_PKG_VERSION"),
            " — ? or Esc to close "
        ));
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new(lines).block(block), rect);
}

/// Save an embedded attachment to the user's Downloads folder, never overwriting (a
/// numeric suffix is added on collision). The bytes are loaded on demand from `source` (the
/// transcript) — one attachment resident at a time — then written and dropped. Text is written
/// verbatim; base64 (images) is decoded first. Returns the written path. Synchronous by design
/// — payloads are small (see `DESIGN.md`).
fn save_attachment(a: &Attachment, source: Option<&Transcript>) -> std::io::Result<PathBuf> {
    write_attachment_to(&downloads_dir(), a, source)
}

/// The testable core of [`save_attachment`]: load `a`'s embedded bytes from `source` and write
/// them into `dir`.
fn write_attachment_to(
    dir: &Path,
    a: &Attachment,
    source: Option<&Transcript>,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind, Write};
    let loaded = load_attachment_content(a, source)?;
    let (bytes, mime): (Vec<u8>, Option<String>) = match loaded {
        LoadedAttachment::Text(t) => (t.into_bytes(), None),
        LoadedAttachment::Base64 { b64, mime } => (
            crate::diff::base64_decode(&b64)
                .ok_or_else(|| Error::new(ErrorKind::InvalidData, "invalid base64"))?,
            Some(mime),
        ),
    };
    std::fs::create_dir_all(dir)?;
    let path = unique_path(dir, &download_filename(a, mime.as_deref()));
    std::fs::File::create(&path)?.write_all(&bytes)?;
    Ok(path)
}

/// Load a `Deferred` attachment's bytes from its transcript `source`, on demand. Errors if the
/// attachment is path-only (nothing to download), the source is missing, or the locator is
/// stale (the line no longer holds that content).
fn load_attachment_content(
    a: &Attachment,
    source: Option<&Transcript>,
) -> std::io::Result<LoadedAttachment> {
    use std::io::{Error, ErrorKind};
    let AttachmentContent::Deferred { at, index } = &a.content else {
        return Err(Error::new(ErrorKind::InvalidInput, "nothing to download"));
    };
    let source =
        source.ok_or_else(|| Error::new(ErrorKind::NotFound, "no transcript to load from"))?;
    source
        .load_attachment(*at, *index)?
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "attachment content not found"))
}

/// `~/Downloads` (via `$HOME`), falling back to the current directory.
fn downloads_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(h) => PathBuf::from(h).join("Downloads"),
        None => PathBuf::from("."),
    }
}

/// The download filename for an attachment: the basename of its path/name, with an
/// image extension appended from the loaded content's MIME type when the name lacks one.
fn download_filename(a: &Attachment, mime: Option<&str>) -> String {
    let base = a.path.as_deref().unwrap_or(&a.name);
    let mut name = base.rsplit('/').next().unwrap_or(base).to_string();
    if name.is_empty() {
        name = "attachment".into();
    }
    if let Some(mime) = mime {
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

/// Occurrences of the (lowercased) needle in one block's TEXT CONTENT — the search
/// discovery primitive (#84). String-only: no render, no wrap, no styling; nested blocks
/// (a thinking span's absorbed tools, a spawn's inline child) recurse so folded and
/// nested content is searchable. The contract is raw-content search (like vim searching
/// the source, not the wrapped display).
fn block_occurrences(b: &Block, needle: &str) -> usize {
    fn count(hay: &str, needle: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }
        hay.to_lowercase().matches(needle).count()
    }
    match b {
        Block::UserText(t) | Block::AssistantText(t) | Block::ToolResult(t) => count(t, needle),
        Block::QueueEvent { text } => count(text, needle),
        Block::Thinking { text, tools, .. } => {
            count(text, needle)
                + tools
                    .iter()
                    .map(|t| block_occurrences(t, needle))
                    .sum::<usize>()
        }
        Block::ToolUse {
            name,
            target,
            diffs,
            output,
            ..
        } => {
            count(name, needle)
                + count(target, needle)
                + output.as_deref().map(|o| count(o, needle)).unwrap_or(0)
                + diffs
                    .iter()
                    .map(|(a, b)| count(a, needle) + count(b, needle))
                    .sum::<usize>()
        }
        Block::Command { name, args, output } => {
            count(name, needle)
                + count(args, needle)
                + output.iter().map(|o| count(o, needle)).sum::<usize>()
        }
        Block::SubAgent(sa) => {
            count(&sa.agent_id, needle)
                + count(&sa.description, needle)
                + count(&sa.prompt, needle)
                + sa.result.as_deref().map(|r| count(r, needle)).unwrap_or(0)
                + sa.blocks
                    .iter()
                    .map(|c| block_occurrences(c, needle))
                    .sum::<usize>()
        }
        Block::AgentDone {
            agent_id,
            description,
            ..
        } => count(agent_id, needle) + count(description, needle),
        Block::Attachment(a) => count(&a.name, needle),
        // The summary prose is real content a reader searches for; the divider's own
        // metadata is chrome the search shouldn't match.
        Block::Compaction { summary, .. } => count(summary, needle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Args; // fold-policy test helpers build an `Args`
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;

    fn blocks(n: usize) -> Vec<Block> {
        (0..n)
            .map(|i| Block::AssistantText(format!("line {i}")))
            .collect()
    }

    /// An `Edit` whose diff turns `old` into `new` — the block kind the plain-first measure
    /// exists for (16% of a big session's blocks, 97% of its old measure cost).
    fn edit(old: &str, new: &str) -> Block {
        Block::ToolUse {
            name: "Edit".into(),
            target: "src/lib.rs".into(),
            diffs: vec![(old.into(), new.into())],
            output: None,
            patch: None,
            read_lines: None,
        }
    }

    /// The measure/render equality the plain-first path rests on: `measure_one` must return
    /// exactly what rendering-and-wrapping returns, for every block, at every width — including
    /// the two branches (nothing wrapped → trust the plain count; something wrapped → fall back)
    /// and the `==` boundary between them.
    ///
    /// This is THE verification for the optimisation. The byte-identical gate cannot catch a
    /// height error: `--dump` prints every block top to bottom, while heights only drive
    /// `prefix` → scroll geometry, so a wrong height passes the gate and shows up later as the
    /// TUI drawing the wrong block for a line.
    #[test]
    fn measure_matches_render_for_every_block_and_width() {
        // A diff row is `INSET? + gutter + ' ' + marker + code`, so sweeping widths across the
        // code lengths below walks the fits/overflows boundary one column at a time.
        let long = "x".repeat(200);
        let cases = vec![
            edit("a\nb\nc", "a\nB\nc"),                       // short rows, never wrap
            edit(&long, &format!("{long}!")),                 // rows far wider than any width
            edit("fn main() {}", "fn main() { let x = 1; }"), // real code, real tokens
            edit("", "added\nlines\nhere"),                   // pure insert
            edit("removed\nlines", ""),                       // pure delete
            edit("tab\there", "tab\tthere"),                  // tabs: sanitize expands them
            edit("héllo wörld", "héllo wörld!"),              // multi-byte
            Block::AssistantText("some **markdown** with `code`".into()),
            Block::UserText("a user turn".into()),
            Block::ToolResult("result text".into()),
        ];
        for (i, b) in cases.iter().enumerate() {
            for width in [1usize, 2, 10, 20, 21, 22, 23, 24, 40, 80, 120, 400] {
                for collapsed in [false, true] {
                    for carry in [false, true] {
                        let is_collapsed = collapsed && render::foldable(b);
                        let fast = measure_one(b, is_collapsed, width, carry);
                        let real = assembled_lines_of(b, is_collapsed, width, carry, Hl::Styled)
                            .1
                            .len();
                        assert_eq!(
                            fast, real,
                            "case {i} width {width} collapsed {collapsed} carry {carry}"
                        );
                    }
                }
            }
        }
    }

    /// The plain-first path must actually TAKE both branches in the test above — an
    /// optimisation that silently always fell back would pass the equality test while doing
    /// nothing, and one that never fell back would not be exercising the fallback.
    #[test]
    fn both_measure_branches_are_exercised() {
        let short = edit("a\nb", "a\nB");
        let long = "x".repeat(200);
        let wide = edit(&long, &format!("{long}!"));
        let fits = |b: &Block, w: usize| {
            let (rows, wrapped) = assembled_lines_of(b, false, w, true, Hl::Measure { width: w });
            wrapped.len() == rows
        };
        assert!(fits(&short, 120), "short rows must take the fast path");
        assert!(!fits(&wide, 120), "over-wide rows must take the fallback");
    }

    /// The live-delta `apply_poll` (engine-provided `changed_from`) yields the exact same view
    /// state as the scan-based `update` for the same new blocks — same rendered lines, same
    /// per-block fold state, and a preserved fold toggle on the unchanged prefix. Proves the
    /// O(turn)-boundary path is behaviorally identical to the O(N)-scan path.
    #[test]
    fn apply_poll_delta_equals_update_scan() {
        let a = blocks(10);
        let mut v1 = View::new(a.clone(), "m", true, FoldPolicy::default());
        let mut v2 = View::new(a.clone(), "m", true, FoldPolicy::default());
        v1.layout(80, 24);
        v2.layout(80, 24);
        // Toggle a fold on the unchanged prefix; both paths must preserve it.
        v1.collapsed[2] = true;
        v2.collapsed[2] = true;
        let b = blocks(13); // grew by 3 (a pure append ⇒ changed_from == 10)
        let d = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        v1.update(b.clone());
        v2.apply_poll(b.clone(), &Metrics::default(), d);
        v1.layout(80, 24);
        v2.layout(80, 24);
        assert_eq!(v1.total_lines(), v2.total_lines(), "same rendered lines");
        assert_eq!(v1.block_kinds(), v2.block_kinds(), "same blocks");
        assert!(
            v1.is_collapsed(2) && v2.is_collapsed(2),
            "fold toggle on the unchanged prefix survived both paths"
        );
    }

    /// The shared-copy `apply_view` (#84/#85) yields the exact same view state as the
    /// scan-based `update` for the same session evolution — same rendered lines, same
    /// per-block fold state, and a preserved fold toggle on the unchanged prefix — while
    /// the View splices deltas instead of swapping whole vectors. Also covers the
    /// commit-shaped delta (prefix moves from open to committed).
    #[test]
    fn apply_view_equals_update_scan() {
        let a = blocks(10);
        let mut v1 = View::new(a.clone(), "m", true, FoldPolicy::default());
        let mut v2 = View::new(a.clone(), "m", true, FoldPolicy::default());
        v1.layout(80, 24);
        v2.layout(80, 24);
        v1.collapsed[2] = true;
        v2.collapsed[2] = true;
        let b = blocks(13);
        let d = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        v1.update(b.clone());
        // Simulate the handoff shape: the first 11 blocks committed (blocks 10.. newly so —
        // the delta hands over 11-10=… everything past the View's prior committed frontier of
        // 8), the last 2 provisional. The splice must land identically to the full scan.
        let committed_len = 11usize;
        let prev_committed = 8usize; // pretend 8 were already handed over
        v2.apply_view(claude_replay_present::cache::ViewDelta {
            reset: false,
            committed_delta: b[prev_committed..committed_len]
                .iter()
                .cloned()
                .map(std::sync::Arc::new)
                .collect(),
            committed_len,
            provisional: b[committed_len..]
                .iter()
                .cloned()
                .map(std::sync::Arc::new)
                .collect(),
            changed_from: d,
            user_times: Vec::new(),
            metrics: Metrics::default(),
            tasks: Default::default(),
        });
        v1.layout(80, 24);
        v2.layout(80, 24);
        assert_eq!(v1.total_lines(), v2.total_lines(), "same rendered lines");
        assert_eq!(v1.block_kinds(), v2.block_kinds(), "same blocks");
        assert!(
            v1.is_collapsed(2) && v2.is_collapsed(2),
            "fold toggle on the unchanged prefix survived both paths"
        );
    }

    /// The `t` task/todo panel (#15): renders the fed TaskList — status glyphs, the
    /// selected task highlighted with its description in the details region — and
    /// j/k moves the selection (TestBackend, no TTY).
    #[test]
    fn tasks_panel_renders_and_navigates() {
        use crate::engine::{TaskItem, TaskList, TaskStatus};
        let mut v = View::new(blocks(3), "m", false, FoldPolicy::default());
        v.set_tasks(TaskList {
            items: vec![
                TaskItem {
                    id: "9".into(),
                    subject: "ship the panel".into(),
                    description: "the long details of nine".into(),
                    status: TaskStatus::Completed,
                    ..TaskItem::default()
                },
                TaskItem {
                    id: "12".into(),
                    subject: "fix the parser".into(),
                    description: "the long details of twelve".into(),
                    active_form: "Fixing the parser".into(),
                    status: TaskStatus::InProgress,
                    blocked_by: vec!["9".into()],
                    ..TaskItem::default()
                },
            ],
        });
        let txt = |b: &Buffer| {
            (0..b.area.height)
                .map(|y| row(b, y))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let b0 = draw(&mut v, 80, 24);
        assert!(!txt(&b0).contains("ship the panel"), "panel starts closed");
        v.toggle_tasks_popup();
        let b1 = draw(&mut v, 80, 24);
        let t1 = txt(&b1);
        assert!(t1.contains("1 open / 2 total"), "header counts:\n{t1}");
        assert!(t1.contains("#9  ship the panel"), "row 9:\n{t1}");
        assert!(
            t1.contains("#12  fix the parser · Fixing the parser"),
            "in-progress row shows activeForm:\n{t1}"
        );
        assert!(
            t1.contains("the long details of nine"),
            "selected (#9) details show:\n{t1}"
        );
        v.tasks_popup_move(1);
        let b2 = draw(&mut v, 80, 24);
        let t2 = txt(&b2);
        assert!(
            t2.contains("the long details of twelve") && t2.contains("blocked by: 9"),
            "selection moved to #12 with dependency edges:\n{t2}"
        );
        v.tasks_popup_close();
        let b3 = draw(&mut v, 80, 24);
        assert!(!txt(&b3).contains("ship the panel"), "panel closes");
    }

    /// #61: a block the USER expanded stays expanded when a live update re-folds the
    /// tail (apply_poll re-derives the policy defaults for `[d..]` — without the
    /// user-fold overlay, the expansion snapped shut exactly when following the tail).
    #[test]
    fn user_fold_survives_live_tail_refold() {
        // A foldable tool block in the tail (Bash folds by default policy).
        let tool = |cmd: &str| Block::ToolUse {
            name: "Bash".into(),
            target: cmd.into(),
            diffs: vec![],
            output: Some("out".into()),
            patch: None,
            read_lines: None,
        };
        let a = vec![
            Block::UserText("go".into()),
            Block::AssistantText("working".into()),
            tool("ls -la"),
        ];
        let mut v = View::new(a.clone(), "m", true, FoldPolicy::default());
        v.layout(80, 24);
        assert!(v.is_collapsed(2), "bash starts collapsed by policy");
        // USER expands it (a real gesture, not a direct collapsed[] poke).
        v.toggle_block(2);
        assert!(!v.is_collapsed(2), "expanded by the user");
        // A live update re-emits the tail from index 2 (a re-fold of the open turn:
        // the same block plus a new one after it).
        let b = vec![
            a[0].clone(),
            a[1].clone(),
            tool("ls -la"),
            Block::AssistantText("new message".into()),
        ];
        v.apply_poll(b, &Metrics::default(), 2);
        v.layout(80, 24);
        assert!(
            !v.is_collapsed(2),
            "the user's expansion survived the live tail re-fold"
        );
        assert_eq!(v.block_kinds().len(), 4, "new block arrived too");
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
        // Parse through the public path-based entry (as a consumer would), not the core's
        // per-agent internals — so the viewer stays off `claude_model`.
        let path = std::env::temp_dir().join(format!("cr-fold-{}.jsonl", std::process::id()));
        std::fs::write(&path, jsonl).unwrap();
        let blocks = claude_replay_core::parse_session_as(crate::Agent::CLAUDE, &path)
            .unwrap()
            .blocks();
        let _ = std::fs::remove_file(&path);
        assert!(!blocks.is_empty(), "parsed an agent spawn");
        let pol = crate::Args::default().fold_policy();
        assert!(pol.collapses(&blocks[0]), "agent spawn default-folds");
    }

    /// The transient flash (#110): `set_flash` takes over the status row on the next draw,
    /// `clear_flash` (what any keystroke calls) hands the row back. This is the surface a
    /// refused mid-session switch reports through, so it must actually reach the screen.
    #[test]
    fn a_flash_takes_the_status_row_and_clears() {
        let mut v = View::new(
            vec![Block::UserText("hello".into())],
            "s",
            false,
            FoldPolicy::default(),
        );
        v.set_flash("in use by another claude-replay (pid 42)");
        let buf = draw(&mut v, 60, 6);
        let status = row(&buf, buf.area.height - 1);
        assert!(
            status.contains("in use by another claude-replay (pid 42)"),
            "flash owns the status row: {status:?}"
        );

        v.clear_flash();
        let buf = draw(&mut v, 60, 6);
        let status = row(&buf, buf.area.height - 1);
        assert!(
            !status.contains("in use"),
            "cleared on input, the ordinary footer returns: {status:?}"
        );
    }

    fn draw(v: &mut View, w: u16, h: u16) -> Buffer {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| v.draw(f)).unwrap();
        t.backend().buffer().clone()
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
    }

    /// #75 sidecar roundtrip: an evicted view's derived state (geometry + search index +
    /// folds + scroll) re-adopts onto a fresh view of the same blocks — the first layout at
    /// the same width skips the measure pass and the interaction state is intact. A view of a
    /// DIFFERENT shape refuses the sidecar (fresh measure instead of corrupt geometry).
    #[test]
    fn sidecar_roundtrips_geometry_and_interaction_state() {
        let blocks = vec![
            Block::UserText("go".into()),
            Block::ToolUse {
                name: "Bash".into(),
                target: "ls -la".into(),
                diffs: Vec::new(),
                output: Some("a\nb\nc".into()),
                patch: None,
                read_lines: None,
            },
            Block::AssistantText("done with a fairly long line that wraps at ten".into()),
        ];
        let mut v = View::new(blocks.clone(), "t", false, FoldPolicy::default());
        let _ = draw(&mut v, 10, 8); // layout at width 10 → measure
        v.toggle_block(1);
        v.scroll_by(2);
        let (heights, scroll, folded) = (v.heights.clone(), v.scroll, v.is_collapsed(1));
        let sc = v.into_sidecar();

        let mut v2 = View::new(blocks.clone(), "t", false, FoldPolicy::default());
        assert!(v2.adopt_sidecar(sc), "same shape ⇒ adopts");
        assert_eq!(v2.heights, heights, "measure pass reused");
        assert_eq!(v2.scroll, scroll, "scroll restored");
        assert_eq!(v2.is_collapsed(1), folded, "fold gesture restored");
        assert!(
            v2.dirty_from.is_none(),
            "geometry valid — no pending re-measure"
        );
        let _ = draw(&mut v2, 10, 8); // same width: layout must keep the adopted geometry
        assert_eq!(
            v2.heights, heights,
            "same-width layout did not re-measure differently"
        );

        // Shape mismatch: a grown session refuses the sidecar.
        let mut v3 = View::new(blocks[..2].to_vec(), "t", false, FoldPolicy::default());
        let _ = draw(&mut v3, 10, 8);
        let sc2 = v2.into_sidecar();
        assert!(
            !v3.adopt_sidecar(sc2),
            "different block count ⇒ fresh measure"
        );
    }

    /// #84 source-of-truth search: a match INSIDE a collapsed tool output is found
    /// (the retired display-text index was fold-blind), navigation peek-expands the
    /// folded hit, stepping away re-collapses it, Esc restores, and Enter makes the
    /// current peek sticky — vim's foldopen=search semantics.
    #[test]
    fn search_finds_folded_content_and_peeks() {
        let blocks = vec![
            Block::UserText("go".into()),
            Block::ToolUse {
                name: "Bash".into(),
                target: "run".into(),
                diffs: Vec::new(),
                output: Some("the ZEBRA lives here".into()),
                patch: None,
                read_lines: None,
            },
            Block::AssistantText("done".into()),
            Block::ToolUse {
                name: "Bash".into(),
                target: "again".into(),
                diffs: Vec::new(),
                output: Some("another ZEBRA sighting".into()),
                patch: None,
                read_lines: None,
            },
        ];
        let mut v = View::new(blocks, "t", false, FoldPolicy::default());
        v.layout(80, 24);
        assert!(
            v.is_collapsed(1) && v.is_collapsed(3),
            "bash folds by default"
        );

        v.search_start();
        for c in "zebra".chars() {
            v.search_input(c);
        }
        assert_eq!(v.match_count(), 2, "hits inside FOLDED outputs are found");
        assert!(!v.is_collapsed(1), "navigating peek-expanded the first hit");
        assert!(v.is_collapsed(3), "the other hit stays folded");

        v.search_next();
        assert!(v.is_collapsed(1), "stepping away re-collapsed the peek");
        assert!(!v.is_collapsed(3), "the new current hit peek-expanded");

        // Esc restores the current peek too.
        v.search_cancel();
        assert!(v.is_collapsed(3), "cancel restored the peek");

        // Enter (keep) makes the current peek sticky instead.
        v.search_start();
        for c in "zebra".chars() {
            v.search_input(c);
        }
        assert!(!v.is_collapsed(1), "peeked again");
        v.search_confirm();
        assert!(!v.is_collapsed(1), "Enter kept the current hit expanded");
        // …and a live update must not snap it back (it is a user_folds entry now).
        v.layout(80, 24);
        assert!(!v.is_collapsed(1), "sticky across layout");
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
            let row = v.block_start(idx).unwrap() as u16;
            assert_eq!(
                v.click_at(row, col),
                Some(Action::Reveal(file.clone())),
                "clicking {name}'s path should reveal the file"
            );
        }
        // The `⏺` marker (col 0) of the first block is outside any path span.
        assert!(v.click_at(0, 0).is_none(), "marker click should not reveal");
        // A command header never masquerades as a file path.
        let bash_row = v.block_start(3).unwrap() as u16;
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
        // The overlay title identifies WHICH build is running (#55).
        assert!(
            t1.contains(concat!("claude-replay v", env!("CARGO_PKG_VERSION"))),
            "help title names the version:\n{t1}"
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
            agent: crate::Agent::CLAUDE,
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
        // Live path: a poll delivers the cumulative snapshot (100 old + 3 new).
        v.update(blocks(103));
        assert_eq!(v.new_count(), 3);
        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("3 new"));
        v.jump_bottom();
        assert_eq!(v.new_count(), 0);
        let buf = draw(&mut v, 40, 10);
        assert!(!row(&buf, 9).contains("new"));
    }

    // NOTE: cross-poll re-grouping (a thinking block absorbing activity tools delivered in
    // an earlier poll) and live immediate-pickup marker suppression used to be done here in
    // the view's `ingest`. Since M16 the `FollowParser`/`Replayer` produce a fully-regrouped
    // cumulative snapshot each poll, so `update` just diffs and swaps — the behavior is owned
    // (and byte-identically tested) by core: `incremental_line_by_line_matches_full_replay`
    // and `queue_markers_suppress_on_immediate_pickup_but_survive_a_gap` in `claude_model`.

    /// `[`/`]` can focus an attachment (it isn't foldable but is actionable), and
    /// activating a path-only attachment returns its path for the caller to reveal.
    #[test]
    fn attachment_is_focusable_and_reveal_returns_its_path() {
        let blocks = vec![
            Block::UserText("go".into()),
            Block::Attachment(Attachment {
                kind: crate::model::AttachmentKind::Ref,
                name: "src/lib.rs".into(),
                path: Some("/w/src/lib.rs".into()),
                content: AttachmentContent::None,
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
        let row = v.block_start(1).unwrap() as u16;
        let Block::SubAgent(spawn) = &*v.blocks[1] else {
            unreachable!()
        };
        let (ids, ide) = crate::tui::render::agent_id_span(spawn).expect("id span");
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
        let Block::SubAgent(spawn) = &*v.blocks[1] else {
            unreachable!()
        };
        assert!(
            crate::tui::render::agent_id_span(spawn).is_some(),
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
        let row = v.block_start(1).unwrap() as u16;
        let (s, e) =
            crate::tui::render::agent_done_id_span("gp", "d", AgentStatus::Failed, "aDONE")
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

    /// The download core loads embedded content on demand from the transcript source and writes
    /// it into a target dir, decoding base64 and never overwriting an existing file. The blocks
    /// carry only `Deferred` locators — the bytes come from `Transcript::load_attachment`.
    #[test]
    fn write_attachment_saves_text_image_and_avoids_overwrite() {
        use std::io::Write;
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let uniq = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("cr-attach-{}-{}", std::process::id(), uniq));
        let _ = std::fs::remove_dir_all(&dir);

        // A real transcript: a `file` attachment on line 0, a base64 image on line 1.
        let l0 = r#"{"type":"attachment","attachment":{"type":"file","filename":"/w/notes.md","displayPath":"notes.md","content":{"type":"text","file":{"filePath":"/w/notes.md","content":"hello"}}}}"#;
        let l1 = r#"{"type":"user","message":{"content":[{"type":"image","source":{"type":"base64","media_type":"image/png","data":"Zm9v"}}]}}"#;
        let tpath = std::env::temp_dir().join(format!(
            "cr-attach-src-{}-{}.jsonl",
            std::process::id(),
            uniq
        ));
        std::fs::File::create(&tpath)
            .unwrap()
            .write_all(format!("{l0}\n{l1}\n").as_bytes())
            .unwrap();
        let src = Transcript::open(crate::Agent::CLAUDE, &tpath);

        let off_img = (l0.len() + 1) as u64;
        let text = Attachment {
            kind: crate::model::AttachmentKind::File,
            name: "notes.md".into(),
            path: Some("/w/notes.md".into()),
            content: AttachmentContent::Deferred { at: 0, index: 0 },
        };
        let p1 = write_attachment_to(&dir, &text, Some(&src)).unwrap();
        assert_eq!(p1.file_name().unwrap(), "notes.md");
        assert_eq!(std::fs::read_to_string(&p1).unwrap(), "hello");
        // A second save of the same name must not overwrite.
        let p2 = write_attachment_to(&dir, &text, Some(&src)).unwrap();
        assert_eq!(p2.file_name().unwrap(), "notes (1).md");

        // A base64 image decodes to bytes; the extension comes from the loaded MIME type.
        let img = Attachment {
            kind: crate::model::AttachmentKind::Image,
            name: "shot".into(),
            path: None,
            content: AttachmentContent::Deferred {
                at: off_img,
                index: 0,
            },
        };
        let pi = write_attachment_to(&dir, &img, Some(&src)).unwrap();
        assert_eq!(pi.file_name().unwrap(), "shot.png");
        assert_eq!(std::fs::read(&pi).unwrap(), b"foo");

        // No source → a graceful error, never a panic.
        assert!(write_attachment_to(&dir, &text, None).is_err());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&tpath);
    }

    #[test]
    fn following_view_keeps_new_content_visible() {
        let mut v = View::new(blocks(10), "t", true, FoldPolicy::none());
        draw(&mut v, 40, 8);
        let mut snapshot = blocks(10);
        snapshot.push(Block::AssistantText("SENTINEL_TAIL".into()));
        v.update(snapshot);
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
            no_cache: true,
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
            args_with(None, None, false).fold_policy(),
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
            args_with(None, Some("read,tool_result"), false).fold_policy(),
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
            args_with(Some("edit"), Some("read"), false).fold_policy(),
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
            args_with(None, None, true).fold_policy(),
        );
        assert!(!v.is_collapsed(0), "--full should expand thinking");
    }

    #[test]
    fn full_flag_unfolds_everything() {
        let v = View::new(
            policy_blocks(),
            "t",
            false,
            args_with(None, None, true).fold_policy(),
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

    /// A long agent-supplied title (#106) must not be able to evict the metrics. The title's
    /// `LOC_PRIO` means "truncate last", which read alone would let a novel-length name push
    /// position, % and the token run off the line — so the cap is applied BEFORE the shed set.
    #[test]
    fn a_long_title_is_capped_before_it_can_evict_the_metrics() {
        let long = "refactor the transcript folding pipeline and also the cache".repeat(4);
        let mut v = View::new(
            vec![Block::UserText("x".into())],
            "stem",
            false,
            FoldPolicy::default(),
        );
        v.set_session_name(long.clone());
        let buf = draw(&mut v, 120, 10);
        let f = row(&buf, 9);
        assert!(
            f.contains('…'),
            "the title is elided, not printed whole:\n{f}"
        );
        assert!(
            f.contains("1/2") && f.contains('%'),
            "position and % survive a long title:\n{f}"
        );
        assert!(
            !f.contains(&long[..60]),
            "the title never claims the whole line:\n{f}"
        );
        // And the cap is a fraction of the room, so a wider terminal shows more of it.
        assert!(title_budget(30) < title_budget(200));
    }

    /// QoderWork titles are Chinese ("初筛候选人简历"): 7 chars, 14 COLUMNS. Measuring the
    /// footer in `char`s would under-count by half and overrun the line into a wrap.
    #[test]
    fn a_cjk_title_is_measured_in_columns_not_chars() {
        use unicode_width::UnicodeWidthStr;
        assert_eq!(cols("初筛候选人简历"), 14);
        assert_eq!(
            UnicodeWidthStr::width(clip_cols("初筛候选人简历", 7).as_str()),
            7
        );

        // The line is always exactly `w` cells, so an overrun shows up as the RIGHT-aligned
        // key-hint run being shoved off the edge and clipped — that is what to assert.
        for w in [40u16, 60, 120] {
            let mut v = View::new(
                vec![Block::UserText("x".into())],
                "初筛候选人简历初筛候选人简历初筛候选人简历",
                false,
                FoldPolicy::default(),
            );
            v.set_session_name("初筛候选人简历初筛候选人简历初筛候选人简历");
            let buf = draw(&mut v, w, 10);
            let f = row(&buf, 9);
            assert!(
                f.contains("?·[ ]·␣↵·/·n·g·q"),
                "a CJK title pushed the key hints off the {w}-column line:\n{f}"
            );
        }
    }

    /// A session with NO agent name shows its stem, and a stem is never capped: a Claude uuid is
    /// 36 columns and `title_budget` at a normal width is 33, so capping it would elide the last
    /// three characters of the only thing identifying the session.
    #[test]
    fn a_bare_uuid_stem_is_never_elided() {
        const STEM: &str = "4752d00e-3b98-4c8d-bc68-c7ca742b11cc";
        assert_eq!(cols(STEM), 36);
        for w in [100u16, 120, 200] {
            let mut v = View::new(
                vec![Block::UserText("x".into())],
                STEM,
                false,
                FoldPolicy::default(),
            );
            let buf = draw(&mut v, w, 10);
            let f = row(&buf, 9);
            assert!(f.contains(STEM), "the stem is truncated at width {w}:\n{f}");
        }
        // But an agent NAME of the same length IS capped — that is the whole distinction.
        let mut v = View::new(
            vec![Block::UserText("x".into())],
            STEM,
            false,
            FoldPolicy::default(),
        );
        v.set_session_name(STEM);
        let buf = draw(&mut v, 120, 10);
        assert!(
            !row(&buf, 9).contains(STEM),
            "a name of any length is capped"
        );
    }

    /// The cap is a ceiling, not a floor: a short title — the common case, and every uuid —
    /// is printed whole and unmarked.
    #[test]
    fn a_short_title_is_left_alone() {
        let mut v = View::new(
            vec![Block::UserText("x".into())],
            "fix the parser",
            false,
            FoldPolicy::default(),
        );
        let buf = draw(&mut v, 120, 10);
        let f = row(&buf, 9);
        assert!(f.contains("fix the parser"), "{f}");
        assert!(!f.contains('…'), "nothing to elide:\n{f}");
    }

    /// The measure pass is split across threads (#107). It must produce EXACTLY the heights a
    /// serial walk would — the heights are the scroll geometry, so a single divergence puts the
    /// scrollbar and every click target out of step with what is drawn.
    ///
    /// Sized past `MIN_PER_THREAD` so the split actually happens, and led by blocks that render
    /// to nothing, which is the one case where `carry_in` is still false and the serial prefix
    /// has to do the work itself.
    #[test]
    fn the_parallel_measure_agrees_with_a_serial_walk() {
        let mut blocks: Vec<Block> = vec![
            // Zero-height leaders: `carry_in` stays false across these.
            Block::UserText(String::new()),
            Block::UserText(String::new()),
        ];
        for i in 0..300 {
            blocks.push(Block::UserText(format!(
                "line {i} with a fairly long body that will wrap at least once at this width"
            )));
            blocks.push(Block::AssistantText(format!(
                "```rust\nfn f{i}() {{ let s = \"a string\"; /* note */ s.len() }}\n```"
            )));
            // Foldable, and 6 lines expanded against 2 collapsed — so a worker that got the fold
            // bit wrong could not possibly agree with the serial walk.
            blocks.push(Block::ToolUse {
                name: "Edit".into(),
                target: format!("src/f{i}.rs"),
                diffs: vec![("a\nb\nc\n".into(), "a\nB\nc\n".into())],
                output: None,
                patch: None,
                read_lines: None,
            });
        }
        let n = blocks.len();
        let mut v = View::new(blocks, "t", false, FoldPolicy::default());
        draw(&mut v, 100, 20);

        // Recompute serially, exactly as the pre-#107 loop did.
        let mut carry = false;
        let mut total = 0usize;
        for b in 0..n {
            let h = v.wrapped_block_lines(b, carry).len();
            carry |= h > 0;
            assert_eq!(
                v.heights[b], h,
                "block {b} measured differently in parallel"
            );
            total += h;
            assert_eq!(v.prefix[b + 1], total, "prefix diverged at block {b}");
        }
        assert_eq!(v.total_wrapped(), total);
        assert!(
            v.heights[0] == 0 && v.heights[1] == 0,
            "the zero-height leaders are the point of this fixture"
        );
        assert!(
            v.heights.contains(&2) && v.heights.iter().any(|&h| h > 2),
            "the fixture must mix collapsed and expanded heights for the fold bit to matter"
        );
        assert!(
            n > 128,
            "must exceed MIN_PER_THREAD so the work is actually split"
        );
    }

    /// Opening a search picks the hit nearest the CURRENT viewpoint — on screen or below —
    /// not the document's first hit. Two hit blocks, viewport scrolled between them: the
    /// search must start on the second.
    #[test]
    fn search_starts_at_the_hit_nearest_the_viewpoint() {
        let mut bs = blocks(30);
        bs[5] = Block::AssistantText("UNIQUEMATCH alpha".into());
        bs[20] = Block::AssistantText("UNIQUEMATCH beta".into());
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10); // first draw follows to the bottom
        v.scroll_by(-9999); // take the viewport to the top (clears follow)…
        v.scroll_by(30); // …then between the two hits: block 5 behind, block 20 ahead
        let origin = v.scroll();
        assert_eq!(origin, 30);
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        v.search_confirm();
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 2/2"),
            "the search starts on the hit AHEAD of the viewpoint:\n{}",
            row(&buf, 9)
        );
        assert!(v.scroll() > origin, "reaching it means scrolling DOWN");
        let body: String = (0..9).map(|y| row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(body.contains("UNIQUEMATCH beta"), "{body}");
    }

    /// Scrolled away from the current hit, `n` re-anchors AT THE VIEWPORT (`less`'s `n`,
    /// extending the v1.43.0 rule that a fresh search starts at the viewpoint): the first hit
    /// at or below the top — 3/4 here, not the sequential 2/4 the old walk would give. And an
    /// already-visible hit is selected WITHOUT yanking the view.
    #[test]
    fn n_after_scrolling_reanchors_at_the_viewport() {
        let mut bs = blocks(60);
        for i in [5usize, 20, 35, 50] {
            bs[i] = Block::AssistantText(format!("UNIQUEMATCH {i}"));
        }
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(-9999); // to the top (clears follow)
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        v.search_confirm();
        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("block 1/4"), "{}", row(&buf, 9));

        // Scroll so the viewport top sits exactly on hit 3 (block 35): the current hit
        // (block 5) is far off screen, hit 3 is visible from its first row.
        let target = v.prefix[35];
        v.scroll_by(target as isize - v.scroll() as isize);
        v.search_next();
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 3/4"),
            "n re-anchors to the first hit at the viewport, not the sequential next:\n{}",
            row(&buf, 9)
        );
        assert_eq!(
            v.scroll(),
            target,
            "an on-screen hit is selected without moving the view"
        );

        // From here the walk is sequential again — the hit is on screen.
        v.search_next();
        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("block 4/4"), "{}", row(&buf, 9));
    }

    /// The backward mirror: scrolled away, `N` re-anchors to the bottom-most hit above the
    /// viewport bottom — 2/3 here, where the sequential prev from 1/3 would wrap to 3/3.
    #[test]
    fn shift_n_after_scrolling_reanchors_backward() {
        let mut bs = blocks(60);
        for i in [10usize, 25, 55] {
            bs[i] = Block::AssistantText(format!("UNIQUEMATCH {i}"));
        }
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(-9999);
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        v.search_confirm();
        let buf = draw(&mut v, 40, 10);
        assert!(row(&buf, 9).contains("block 1/3"), "{}", row(&buf, 9));

        // Park the viewport between hits 2 and 3 (top on block 40): nothing matching is on
        // screen, hit 2 is the nearest ABOVE.
        let target = v.prefix[40];
        v.scroll_by(target as isize - v.scroll() as isize);
        v.search_prev();
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 2/3"),
            "N re-anchors to the nearest hit above the viewport, not the sequential wrap:\n{}",
            row(&buf, 9)
        );
        assert!(v.scroll() < target, "reaching it means scrolling UP");
    }

    /// With the current hit still ON SCREEN the walk is untouched: `n` steps sequentially
    /// even when an earlier hit sits nearer the viewport top — re-anchoring is only for a
    /// reader who scrolled AWAY.
    #[test]
    fn n_walks_sequentially_while_the_current_hit_is_on_screen() {
        let mut bs = blocks(30);
        for i in [5usize, 6, 7] {
            bs[i] = Block::AssistantText(format!("UNIQUEMATCH {i}"));
        }
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(-9999);
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        v.search_confirm();
        v.search_next(); // 2/3, on screen beside 1 and 3
        v.search_next(); // must be the SEQUENTIAL 3/3, not a re-anchor back to 1/3
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 3/3"),
            "on-screen hit => plain sequential walk:\n{}",
            row(&buf, 9)
        );
    }

    /// Every hit behind the viewpoint: the pick loops around to the document's first hit —
    /// and `n` keeps cycling through the wrap, so no hit is ever unreachable.
    #[test]
    fn search_loops_to_the_top_when_every_hit_is_behind() {
        let mut bs = blocks(30);
        bs[5] = Block::AssistantText("UNIQUEMATCH alpha".into());
        bs[20] = Block::AssistantText("UNIQUEMATCH beta".into());
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10);
        v.scroll_by(9999); // bottom: both hits are above
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        v.search_confirm();
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 1/2"),
            "wrapped to the FIRST hit:\n{}",
            row(&buf, 9)
        );
        // And n cycles: 1 → 2 → wraps back to 1.
        v.search_next();
        draw(&mut v, 40, 10);
        v.search_next();
        let buf = draw(&mut v, 40, 10);
        assert!(
            row(&buf, 9).contains("block 1/2"),
            "n wraps:\n{}",
            row(&buf, 9)
        );
    }

    /// A hit whose block already starts on screen does not move the viewport — the reader is
    /// looking at it; the highlight is enough.
    #[test]
    fn search_does_not_scroll_when_the_hit_is_already_in_view() {
        let mut bs = blocks(30);
        bs[2] = Block::AssistantText("UNIQUEMATCH here".into());
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 10); // first draw follows to the bottom
        v.scroll_by(-9999); // top; block 2 is on screen
        assert_eq!(v.scroll(), 0);
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        draw(&mut v, 40, 10);
        assert_eq!(v.scroll(), 0, "the hit was on screen — no jump");
    }

    /// The hit rows are actually PAINTED: the row containing the needle carries the search
    /// background, strong (current hit) vs dim (other hits). Regression test for the #84
    /// switch to block-index matches, after which the draw kept comparing them against
    /// wrapped-LINE indices — highlighting nothing, or an arbitrary early row.
    #[test]
    fn search_highlight_paints_the_needle_rows() {
        let mut bs = blocks(30);
        bs[2] = Block::AssistantText("UNIQUEMATCH one".into());
        bs[4] = Block::AssistantText("UNIQUEMATCH two".into());
        let mut v = View::new(bs, "t", false, FoldPolicy::none());
        draw(&mut v, 40, 12); // first draw follows to the bottom
        v.scroll_by(-9999); // top: both hit blocks on screen
        v.search_start();
        for c in "UNIQUEMATCH".chars() {
            v.search_input(c);
        }
        let buf = draw(&mut v, 40, 12);
        let bg_of = |needle_row: &str| {
            let y = (0..11)
                .find(|&y| row(&buf, y).contains(needle_row))
                .unwrap_or_else(|| panic!("{needle_row} not on screen"));
            let x = row(&buf, y).find('U').unwrap() as u16;
            buf[(x, y)].style().bg
        };
        assert_eq!(
            bg_of("UNIQUEMATCH one"),
            Some(ratatui::style::Color::Yellow),
            "the CURRENT hit row is strong-highlighted"
        );
        assert_eq!(
            bg_of("UNIQUEMATCH two"),
            Some(ratatui::style::Color::Rgb(70, 70, 0)),
            "another hit row is dim-highlighted"
        );
        // A row with no occurrence is untouched.
        let y0 = (0..11).find(|&y| row(&buf, y).contains("line 0")).unwrap();
        let x0 = row(&buf, y0).find('l').unwrap() as u16;
        let plain = buf[(x0, y0)].style().bg;
        assert!(
            plain != Some(ratatui::style::Color::Yellow)
                && plain != Some(ratatui::style::Color::Rgb(70, 70, 0)),
            "non-hit rows keep their bg, got {plain:?}"
        );
    }
}

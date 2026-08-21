//! Terminal wiring + input loop. All view state/drawing lives in `view::View`
//! (testable headless via ratatui's TestBackend).

use crate::sys::{deduce_stem, reveal_in_file_manager};

/// The TUI's cache instantiation — both seams doing chosen work (#85: no phantom
/// parameters): the live store is the cache-owned shared copy, the aux slot parks
/// evicted frames' derived view state.
pub(crate) type TuiCache = claude_replay_present::SessionCache<
    crate::store::ArcLog, // live store: cache-owned shared copy (#84), logged for #96
    crate::view::ViewSidecar, // aux slot: evicted frames' derived state (#75)
>;

/// This run's cache. Always a real cache; what `--no-cache` chooses is a different ROOT (#165) —
/// this run's own [`throwaway_root`](crate::sys::throwaway_root) instead of the shared one, so a
/// second view of a session someone else holds folds, locks and resumes exactly like the first,
/// just without coordinating with it. Same root when the cache home cannot be resolved at all:
/// there is then nowhere to coordinate, which is a fact about the machine, not a degraded mode.
fn make_cache(args: &Args) -> TuiCache {
    // Take back what dead runs left behind, once per run.
    crate::sys::reclaim();
    let root = match args.no_cache {
        true => crate::sys::throwaway_root(),
        false => cache::admit::default_root().unwrap_or_else(crate::sys::throwaway_root),
    };
    TuiCache::durable(
        Presentation::Tui,
        root,
        // The TUI has no render parameters baked into a stored block: `Block`s are stored,
        // and fold/scroll are applied at draw time. So no flavor.
        Versions::current(None),
    )
}

/// Bring `id`'s session into the cache, or explain why we will not.
///
/// A session another instance already holds is **refused**, and now unconditionally: every
/// viewer tails, so there is no read-only case to carve out. Two instances would each fold and
/// hold the same growing session in RAM, invisibly, and `tmux attach` is the real sharing
/// primitive — which is why the refusal names the holder's pane when it can. `--no-cache` is
/// the deliberate escape hatch for a genuine second view.
fn admit_root(
    cache: &TuiCache,
    id: &str,
) -> Result<std::sync::Arc<claude_replay_present::cache::SharedSession<crate::store::ArcLog>>> {
    match cache.admit(
        id,
        |dir| crate::store::ArcLog::open_append(&dir.join("blocks.jsonl")),
        |h: &claude_replay_present::cache::Holder<crate::store::TuiNote>| {
            claude_replay_present::cache::lock::pid_alive(h.pid)
        },
    ) {
        Admission::Owned { session, .. } => {
            // Publishes here because the entry is ours from this line on, and the TUI's note
            // (its tmux pane) is known at startup — unlike a server's port.
            let _ = cache.publish(id, crate::store::TuiNote::here());
            Ok(session)
        }
        Admission::Denied(Denial::Held(h)) => Err(anyhow::Error::new(HeldElsewhere {
            pid: h.pid,
            pane: h.note.and_then(|n| n.pane),
        })),
        // Not a competitor — a machine that cannot host a cache entry at all (#163). This used to
        // run cache-less, which is how a viewer could end up folding a session it did not own;
        // since `--no-cache` became a real cache at its own root (#165) there is no benign reason
        // left to reach here, so say which one it was rather than carry on regardless.
        Admission::Denied(Denial::Unavailable(why)) => Err(anyhow::anyhow!(
            "session {id} cannot be opened: {}",
            match why {
                Unavailable::NoCacheFlag => "this viewer has no cache root".to_string(),
                Unavailable::UnwritableRoot => format!(
                    "the cache directory is not writable{}",
                    cache::admit::cache_home()
                        .map(|h| format!(" ({})", h.display()))
                        .unwrap_or_default()
                ),
                Unavailable::NoLivenessCheck =>
                    "this platform cannot tell whether a lock's holder is still running".to_string(),
                Unavailable::UnknownSession =>
                    "no transcript is registered under that id".to_string(),
            }
        )),
    }
}

/// A session another live claude-replay holds — **typed**, not a bare message, because the two
/// places it surfaces need opposite outcomes (#110). At LAUNCH it propagates out of `run` and is
/// printed with the full guidance below — exiting to the shell is where you want to be. On a
/// mid-session `s`-switch it must NOT kill the viewer you were reading: the switch arm downcasts
/// to this, stays on the current session, and shows [`flash`](Self::flash) instead.
#[derive(Debug)]
struct HeldElsewhere {
    pid: u32,
    /// The holder's `$TMUX_PANE`, when it published one — `tmux attach` being the real
    /// sharing primitive the refusal defers to (#96 §8.4).
    pane: Option<String>,
}

impl HeldElsewhere {
    /// The one-line form for the viewer's status flash. No `--no-cache` guidance here — that is
    /// launch advice; mid-session, the viewer you are in IS the session you keep.
    fn flash(&self) -> String {
        format!(
            "in use by another claude-replay (pid {}){}",
            self.pid,
            self.pane
                .as_deref()
                .map(|p| format!(" — tmux attach -t {p}"))
                .unwrap_or_default()
        )
    }
}

impl std::fmt::Display for HeldElsewhere {
    /// The launch-path message, byte-for-byte what the pre-#110 `bail!` printed.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session already open in another claude-replay (pid {}){}\n\
             or pass --no-cache for a second read-only view",
            self.pid,
            self.pane
                .as_deref()
                .map(|p| format!("; attach with `tmux attach -t {p}`"))
                .unwrap_or_default()
        )
    }
}

impl std::error::Error for HeldElsewhere {}
use crate::tui::picker::Picker;
use crate::tui::view::View;
use crate::{discover, discover::Candidate, Agent, Args};
use anyhow::Result;
use claude_replay_core::engine::meta_stream::Versions;
use claude_replay_present::cache::{self, Admission, Denial, Presentation, Unavailable};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::stdout;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

/// View a single session (explicit target / `--latest` / the only session). There
/// is no list to return to, so both `q` and `Esc` quit. A `--latest` launch can
/// still hop to another session via the `s` switcher (`run_view_loop` handles it).
pub fn run(args: &Args, path: &Path) -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    // `run_view_loop` releases its cache on every exit path, including the error one below.
    let res = run_view_loop(&mut term, args, path.to_path_buf(), false).map(|_| ());

    disable_raw_mode().ok();
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    term.show_cursor().ok();

    // Fast exit: a large transcript's `View` (tens of thousands of styled lines)
    // is slow to drop. The terminal is already restored, so skip running those
    // destructors — the OS reclaims the memory far faster than Rust's drop glue,
    // which made quitting feel laggy. Propagate a real error first (rare).
    res?;
    std::io::Write::flush(&mut stdout()).ok();
    std::process::exit(0);
}

/// Interactive entry when no target/`--latest`/`--dump` was given: discover the
/// sessions and, when there's more than one, loop between the picker and the
/// viewer so `Esc` in the viewer returns to the list instead of quitting.
pub fn run_interactive(args: &Args) -> Result<()> {
    // Merge sessions from every agent (filtered by --agent) for this directory.
    let mut cands = discover::candidates_all(args.agent);
    if cands.is_empty() {
        anyhow::bail!("no transcripts found for any agent in this directory");
    }
    // Only one session — open it directly; there's no list to go back to.
    if cands.len() == 1 {
        return run(args, &cands.remove(0).path);
    }

    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;

    let mut picker = Picker::new(cands);
    let res = picker_viewer_loop(&mut term, args, &mut picker);

    disable_raw_mode().ok();
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    term.show_cursor().ok();

    res?;
    std::io::Write::flush(&mut stdout()).ok();
    std::process::exit(0);
}

/// Show the session picker and return the chosen transcript path — the same
/// cwd-scoped selection the no-arg viewer flow uses, but for `--html` (which then
/// opens the browser instead of the TUI). One candidate auto-selects; none errors;
/// `Esc` returns `Ok(None)`. Needs a TTY (like `-f`).
/// Stay on the session picker and hand each pick to `on_pick`, instead of returning the
/// first one. Backs `-f --html` with several matches: every discovered session is already
/// being served, so picking one (`Enter`, or a mouse click on its row) opens that session's
/// browser tab and the picker **stays up** for the next one — the way back the one-shot
/// [`pick_session`] never had. `Esc`/`Ctrl-C`/`q` quits.
///
/// `status` is shown in the header (the server URL); picks are marked `●` in the list.
/// Returns once the user quits; the caller owns process teardown.
pub fn pick_session_loop(
    cands: Vec<Candidate>,
    status: &str,
    on_pick: &mut dyn FnMut(&Path),
) -> Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    let mut picker = Picker::new(cands);
    picker.set_status(status.to_string());
    let res = pick_multi_loop(&mut term, &mut picker, on_pick);

    disable_raw_mode().ok();
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    term.show_cursor().ok();
    res
}

/// The `pick_session_loop` event loop: like [`pick_loop`], but a confirm fires `on_pick`
/// and continues rather than returning. Split out so it is drivable under `TestBackend`.
fn pick_multi_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    picker: &mut Picker,
    on_pick: &mut dyn FnMut(&Path),
) -> Result<()> {
    loop {
        term.draw(|f| picker.draw(f))?;
        match pick_action(&event::read()?, picker) {
            PickAction::Quit => return Ok(()),
            PickAction::Confirm => {
                if let Some(path) = picker.selected_path() {
                    on_pick(&path);
                    picker.mark_selected_opened();
                }
            }
            PickAction::None => {}
        }
    }
}

/// What one event means to the multi-open picker. Pure (all terminal state lives in
/// `picker`), so the loop's behavior is testable without a TTY.
#[derive(Debug, PartialEq, Eq)]
enum PickAction {
    None,
    /// Open the current selection — and STAY on the picker.
    Confirm,
    Quit,
}

/// Apply one event to `picker` and say what the loop should do. `Enter` and a left-click
/// on a row are the same thing: select + confirm.
fn pick_action(ev: &Event, picker: &mut Picker) -> PickAction {
    match ev {
        Event::Key(k) => {
            if k.kind == KeyEventKind::Release {
                return PickAction::None;
            }
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                KeyCode::Esc => PickAction::Quit,
                KeyCode::Char('c') if ctrl => PickAction::Quit,
                KeyCode::Enter => PickAction::Confirm,
                KeyCode::Up => {
                    picker.up();
                    PickAction::None
                }
                KeyCode::Down => {
                    picker.down();
                    PickAction::None
                }
                KeyCode::Backspace => {
                    picker.backspace();
                    PickAction::None
                }
                KeyCode::Char(c) => {
                    picker.push_char(c);
                    PickAction::None
                }
                _ => PickAction::None,
            }
        }
        Event::Mouse(m) => match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if picker.click(m.row) {
                    PickAction::Confirm
                } else {
                    PickAction::None
                }
            }
            MouseEventKind::ScrollUp => {
                picker.up();
                PickAction::None
            }
            MouseEventKind::ScrollDown => {
                picker.down();
                PickAction::None
            }
            _ => PickAction::None,
        },
        _ => PickAction::None,
    }
}

pub fn pick_session(args: &Args) -> Result<Option<PathBuf>> {
    let mut cands = discover::candidates_all(args.agent);
    if cands.is_empty() {
        anyhow::bail!("no transcripts found for any agent in this directory");
    }
    if cands.len() == 1 {
        return Ok(Some(cands.remove(0).path));
    }
    enable_raw_mode()?;
    let mut out = stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture)?;
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    let res = pick_loop(&mut term, &mut Picker::new(cands));
    disable_raw_mode().ok();
    execute!(
        term.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    term.show_cursor().ok();
    res
}

/// Alternate picker ↔ viewer in one terminal session. The picker's `Esc` quits;
/// the viewer's `Esc` (Outcome::Back) returns here to re-pick, `q` quits. Reusing
/// the same `Picker` preserves the query and selection across a round trip.
fn picker_viewer_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    args: &Args,
    picker: &mut Picker,
) -> Result<()> {
    loop {
        let path = match pick_loop(term, picker)? {
            Some(p) => p,
            None => return Ok(()),
        };
        match run_view_loop(term, args, path, true)? {
            Outcome::Back => continue,
            _ => return Ok(()), // Quit (run_view_loop never returns Switch)
        }
    }
}

/// Cap on **resident sub-agent Views**. The main session (stack root) is always resident and is
/// not counted; beyond this many loaded sub-agent frames, the least-recently-viewed one is evicted
/// (`view: None`) and re-parsed from its own transcript on re-visit — a custom LRU cache policy
/// that bounds TUI memory by navigation recency, not by the agent tree's size.
const MAX_RESIDENT_SUBAGENTS: usize = 4;

/// How many ROOTS an `s`-switch leaves resident behind it (#109). Each one holds that session's
/// whole committed block vector, so this is a memory cap, not a policy preference: two covers the
/// A→B→A toggle the retention exists for, and the third-oldest is dropped rather than accumulated.
const MAX_RETAINED_ROOTS: usize = 2;

/// One level of the sub-agent navigation stack: a `View` (or `None` when evicted) plus its tail
/// reader, agent, path, and the parent block index it was descended *from* (so ascending lands the
/// cursor back on that spawn). The root's `from` is unused.
struct Frame {
    /// The loaded viewer, or `None` when this sub-agent frame has been LRU-evicted. Reloaded on
    /// demand by [`ensure_loaded`] (re-parsing from the child's own transcript). The root frame
    /// (index 0) is **pinned** — always `Some`.
    view: Option<View>,
    /// This frame's key in the session cache (the root's session id, or a child's agent id).
    /// Empty when the frame has no followable source (e.g. a spawn with no recorded agent id).
    /// The live follower itself lives in the [`SessionCache`](claude_replay_present::SessionCache) — the frame
    /// keeps only presentation state.
    id: String,
    /// The sidecar key (#75): the child's agent id when it has one, else a stable
    /// path+spawn-index composite — non-empty for every evictable frame, unlike `id`
    /// (which is empty when the frame has no followable source). Empty only for the root
    /// (pinned, never evicted).
    sc_key: String,
    agent: Agent,
    path: PathBuf,
    from: crate::model::BlockIndex,
    /// Monotonic focus tick, bumped when this frame becomes the current view — the LRU key.
    last_used: u64,
}

/// View a session, staying in the viewer across `s`-switches AND sub-agent descents. A
/// **stack** of `Frame`s keeps each ancestor `View` alive (not re-parsed), so its scroll
/// offset and fold state are preserved on return; memory is bounded by depth, not the
/// session's total agent count. Returns only `Quit` or `Back`.
fn run_view_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    args: &Args,
    path: PathBuf,
    can_go_back: bool,
) -> Result<Outcome> {
    let mut tick: u64 = 0;
    // The session domain (live followers + their residency) lives in the cache; frames keep only
    // presentation state. ONE cache for the whole loop, including across `s`-switches (#109) —
    // see the `Outcome::Switch` arm for what a switch does instead of replacing it.
    let cache = make_cache(args);
    // Roots this loop has left behind, least-recently-visited first: released (unlocked,
    // quiesced) but still RESIDENT, so switching back to one costs a lock and a `stat` instead of
    // a resume. Capped, because each entry holds that session's whole committed block vector.
    let mut retained: Vec<String> = Vec::new();
    let mut stack: Vec<Frame> = vec![build_frame(args, &cache, &path, can_go_back, 0)?];
    loop {
        // The current top must be loaded to view it (an ascent may have landed on an evicted
        // frame); reload it (and any evicted ancestors it needs) on demand.
        let top = stack.len() - 1;
        ensure_loaded(args, &cache, &mut stack, top)?;
        tick += 1;
        stack[top].last_used = tick;

        let descended = stack.len() > 1;
        let frame = stack.last_mut().expect("stack never empty");
        let outcome = event_loop(
            term,
            frame.agent,
            args,
            &frame.path,
            frame.view.as_mut().expect("top is loaded"),
            &cache,
            &frame.id,
            descended,
        )?;
        match outcome {
            // Descend into a sub-agent: build a child `View` from its (already-parsed)
            // transcript and push it, keeping the parent alive underneath — then evict the
            // least-recently-viewed sub-agent if we're over the resident cap.
            Outcome::Descend(idx) => {
                let top = stack.last().expect("stack never empty");
                let (agent, path) = (top.agent, top.path.clone());
                let built = build_child_frame(
                    args,
                    &cache,
                    top.view.as_ref().expect("top is loaded"),
                    agent,
                    &path,
                    idx,
                );
                match built {
                    Ok(Some(mut child)) => {
                        tick += 1;
                        child.last_used = tick;
                        stack.push(child);
                        enforce_cap(&cache, &mut stack);
                    }
                    Ok(None) => {} // not a descendable block
                    Err(ChildUnavailable(msg)) => {
                        if let Some(v) = stack.last_mut().and_then(|f| f.view.as_mut()) {
                            v.set_flash(msg);
                        }
                    }
                }
            }
            // Ascend: drop the child `View`, reload the parent if it was evicted, and land its
            // cursor on the spawn block we came from — without touching its fold state (§2.2).
            Outcome::Ascend if descended => {
                let popped = stack.pop().expect("descended ⇒ non-root");
                // Park the child's derived state (#75): a later re-descend into the same
                // child re-adopts its folds/scroll/measure instead of starting cold.
                if let Some(view) = popped.view {
                    if !popped.sc_key.is_empty() {
                        cache.aux_put(&popped.sc_key, view.into_sidecar());
                    }
                }
                let ni = stack.len() - 1;
                ensure_loaded(args, &cache, &mut stack, ni)?;
                if let Some(parent) = stack.last_mut() {
                    parent
                        .view
                        .as_mut()
                        .expect("reloaded")
                        .focus_block(popped.from);
                }
            }
            Outcome::Ascend => {} // at root: nothing above to ascend to
            // `s`-switch resets the whole stack to the newly chosen session.
            Outcome::Switch(p) => {
                // Re-picking the session you are on is a NO-OP, and the guard is load-bearing:
                // the general path below builds the target BEFORE releasing the current root,
                // which for the same id would install a fresh session and then quiesce it —
                // leaving the viewer on a frozen session that silently stops following.
                let new_id = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("session")
                    .to_string();
                if stack.first().is_some_and(|r| r.id == new_id) {
                    continue; // the switcher already closed itself on confirm
                }
                // Build the TARGET first (#110): admission is where a `Held` refusal surfaces,
                // and until the target is actually ours nothing of the current stack may be
                // torn down — a refused pick must leave the viewer exactly where it was, not
                // dead at the shell. Building against the shared cache needs no teardown to
                // have happened; the ids differ (guarded above), so the entries are disjoint.
                let mut frame = match build_frame(args, &cache, &p, can_go_back, 0) {
                    Ok(frame) => frame,
                    Err(e) => {
                        let held = e.downcast_ref::<HeldElsewhere>().map(HeldElsewhere::flash);
                        if held.is_none() {
                            // Not a refusal: the build may have got PAST admission and failed
                            // reading (an unreadable transcript). Undo the partial open, or we
                            // stay on the old session while silently holding the target's lock.
                            // Both calls are no-ops when admission never happened; on a Held
                            // refusal nothing was opened AND a resident for that id is a legit
                            // retained one (#109) worth keeping — hence the `if`.
                            cache.release(&new_id);
                            cache.remove_pull(&new_id);
                        }
                        let msg = held.unwrap_or_else(|| {
                            format!("cannot open that session: {e:#}").replace('\n', " · ")
                        });
                        if let Some(v) = stack.last_mut().and_then(|f| f.view.as_mut()) {
                            v.set_flash(msg);
                        }
                        continue; // stay put — the stack was never touched
                    }
                };
                // The target is ours: RELEASE the session we are leaving — but keep the cache
                // and everything in it (#109). Releasing is what #107 actually needed: a session
                // merely browsed past must not stay locked against a second terminal. `release`
                // quiesces: the writer detaches, the lock goes, the blocks stay resident.
                //
                // Sub-agent residents go with the stack that owned them. None of them is durable
                // (they are opened uncached), and the new stack rebuilds whichever it needs.
                // Except an id that IS the target (a session open here as a descended child):
                // dropping that would evict the resident the build just installed.
                for f in stack.iter().skip(1).filter(|f| f.id != frame.id) {
                    cache.remove_pull(&f.id);
                }
                if let Some(root) = stack.first_mut() {
                    // The root's derived geometry parks in the cache's own sidecar slot, beside
                    // the blocks it describes — the same home an evicted sub-agent's uses.
                    // `adopt_sidecar` refuses a stale entry (different block count; a width
                    // change re-measures from 0), so a session that grew while you were away is
                    // re-measured rather than mismeasured.
                    if let Some(view) = root.view.take() {
                        cache.aux_put(&root.id, view.into_sidecar());
                    }
                    cache.release(&root.id);
                    retained.retain(|id| id != &root.id);
                    retained.push(root.id.clone());
                }
                if let (Some(view), Some(sc)) = (frame.view.as_mut(), cache.aux_take(&frame.id)) {
                    view.adopt_sidecar(sc);
                }
                stack = vec![frame];
                // The session we just entered is no longer "left behind"; then trim the rest.
                retained.retain(|id| id != &stack[0].id);
                while retained.len() > MAX_RETAINED_ROOTS {
                    cache.remove_pull(&retained.remove(0));
                }
            }
            other => {
                // Quit / Back — the loop's ONLY other exit, and both lead to a
                // `process::exit(0)` that skips destructors. Flush and unlock here or a lock
                // outlives the process and denies the session to the next run.
                cache.release_all();
                return Ok(other);
            }
        }
    }
}

/// Ensure `stack[i]` is loaded, re-parsing it (and any evicted ancestors it depends on) on demand.
/// A hollow sub-agent frame is rebuilt from its parent's loaded view via [`build_child_frame`]; the
/// recursion bottoms out at the pinned root (always loaded). No-op if already loaded.
fn ensure_loaded(args: &Args, cache: &TuiCache, stack: &mut [Frame], i: usize) -> Result<()> {
    if stack[i].view.is_some() {
        return Ok(());
    }
    debug_assert!(i > 0, "root frame is pinned — never evicted");
    ensure_loaded(args, cache, stack, i - 1)?;
    let from = stack[i].from;
    let agent = stack[i - 1].agent;
    let path = stack[i - 1].path.clone();
    // Rebuild from the (now-loaded) parent; the immutable borrow of `stack[i-1]` ends when
    // `build_child_frame` returns (it owns its `View`), before we write `stack[i]`.
    let rebuilt = build_child_frame(
        args,
        cache,
        stack[i - 1].view.as_ref().expect("parent loaded"),
        agent,
        &path,
        from,
    );
    match rebuilt {
        Ok(Some(f)) => stack[i].view = f.view,
        Ok(None) => {}
        // The frame stays hollow and the next draw shows the flash; a rebuild that cannot admit
        // must not leave a view that silently never ticks.
        Err(ChildUnavailable(msg)) => {
            if let Some(v) = stack[i - 1].view.as_mut() {
                v.set_flash(msg);
            }
        }
    }
    Ok(())
}

/// Evict least-recently-viewed loaded sub-agent frames until at most [`MAX_RESIDENT_SUBAGENTS`]
/// remain loaded. The root (index 0) is pinned and never counted/evicted; the current top is never
/// evicted (it's being viewed).
fn enforce_cap(cache: &TuiCache, stack: &mut [Frame]) {
    let top = stack.len() - 1;
    loop {
        let loaded: Vec<usize> = (1..stack.len())
            .filter(|&i| stack[i].view.is_some())
            .collect();
        if loaded.len() <= MAX_RESIDENT_SUBAGENTS {
            break;
        }
        // The least-recently-used loaded sub-agent, excluding the top.
        let victim = loaded
            .into_iter()
            .filter(|&i| i != top)
            .min_by_key(|&i| stack[i].last_used);
        match victim {
            Some(v) => {
                // Park the derived view state in the cache's aux slot (#75): heights/search
                // index + fold/scroll survive the eviction; the blocks (the heavy part) drop.
                if let Some(view) = stack[v].view.take() {
                    if !stack[v].sc_key.is_empty() {
                        cache.aux_put(&stack[v].sc_key, view.into_sidecar());
                    }
                }
            }
            None => break, // only the top is loaded — can't evict it
        }
    }
    // The followers obey the same budget in the cache (the root is pinned there too); an evicted
    // follower re-materializes from the registry on its next poll.
    // The retained roots (#109) share this registry, so the budget has to cover both — otherwise
    // opening a few sub-agents would silently evict the sessions the switch just parked.
    cache.reap_over_budget(MAX_RESIDENT_SUBAGENTS + MAX_RETAINED_ROOTS, &stack[0].id);
}

/// Build the root frame for `path`: detect the agent, stream-parse, build the `View`
/// with cwd/metrics/picker wiring, and open a tail reader when following.
fn build_frame(
    args: &Args,
    cache: &TuiCache,
    path: &Path,
    can_go_back: bool,
    from: crate::model::BlockIndex,
) -> Result<Frame> {
    // The agent is a property of the file — detect it from the contents so the right
    // parser/metrics run, whether we got here from the picker or a path.
    let agent = discover::detect_agent(path);
    // The session's own name when it has one, else its id. A transcript's stem is a UUID, which
    // tells you nothing about which session you are looking at — the agent usually knows better.
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();
    // The agent's own name for this session (#106), when it has one. Kept apart from the stem:
    // the view caps a NAME's footer width (unbounded prose) but never a stem's (already bounded,
    // and the only thing that identifies the session).
    let name = crate::Transcript::open(agent, path)
        .card()
        .and_then(|c| c.title);
    let fold = args.fold_policy();
    // Live (`-f`): register the source and let the CACHE's follower own both the initial fold
    // (its first poll folds the whole current file, matching a one-shot `parse_session_as`) and
    // the tail (the event loop's `poll_delta`). Non-live: one plain streaming parse — the cache
    // is never touched, so no follower exists.
    let transcript = crate::Transcript::open(agent, path);
    // The session ID is the transcript's stem, and deliberately NOT the display title: it keys
    // the cache entry, the lock and the registry. A title is user-settable and not unique — two
    // sessions called "fix the parser" would collide on one cache entry and one lock.
    let id = stem.clone();
    // Both paths go through the cache now (#96). Following needs it for the tail; a one-shot
    // read wants it for the RESUME — skipping the bytes a previous run already folded is exactly
    // where a large transcript's open time goes. The follower's first poll folds whatever is
    // above the resume point, matching a one-shot `parse_session_as` from there.
    cache.register(&id, transcript.clone());
    let session = admit_root(cache, &id)?;
    let polled = cache
        .poll_view(&id, crate::store::ArcLog::memory)
        .and_then(|r| r.ok());
    let (blocks, cwd, metrics, oplog_tasks) = match polled {
        Some(d) => {
            // The WHOLE committed prefix, not the tick's delta: this process has no blocks yet,
            // and after a resume the delta covers only what was folded above the resume point.
            let mut blocks = session.committed_arcs();
            blocks.extend(d.provisional);
            (blocks, discover::first_cwd(path), d.metrics, d.tasks)
        }
        // An idle first poll no longer means "empty or unreadable transcript". A RETAINED session
        // (#109) is already folded to EOF, so its first tick after a re-admit has nothing to
        // report — and re-parsing here would throw away the entire state retention exists to
        // keep, on every switch back. So read the session first; a parse is the fallback for a
        // session that genuinely holds nothing, which is where an unreadable file still reports
        // its error instead of opening blank.
        None => {
            let mut blocks = session.committed_arcs();
            let d = session.pull_delta(session.epoch(), blocks.len());
            if blocks.is_empty() && d.provisional.is_empty() {
                let s = transcript.parse()?;
                (
                    s.blocks().into_iter().map(std::sync::Arc::new).collect(),
                    s.cwd,
                    s.metrics,
                    s.tasks,
                )
            } else {
                blocks.extend(d.provisional.into_iter().map(std::sync::Arc::new));
                (blocks, discover::first_cwd(path), d.metrics, d.tasks)
            }
        }
    };
    // Always live: the viewer tails, full stop (`-f` is deprecated and ignored).
    let mut view = View::new_shared(blocks, stem.clone(), true, fold);
    if let Some(name) = name {
        view.set_session_name(name);
    }
    view.set_can_go_back(can_go_back);
    view.set_cwd(cwd);
    // Disk-grounded live repo location for the reveal action: if this session's repo
    // was moved, a click on a now-dead recorded path re-roots to where the repo lives now.
    view.set_reveal_root(discover::project_path(path));
    // The task panel's initial state (#15): the transcript's op-log merged with the
    // live task files (disk wins per id; the op-log backfills pruned files).
    view.set_tasks(crate::engine::tasks::merged(
        &oplog_tasks,
        discover::session_tasks(agent, path),
    ));
    // The blocks hold only attachment locators; give the view the transcript to load them from.
    view.set_source(Some(transcript));
    view.set_can_open_picker(args.latest);
    view.set_metrics(metrics.footer());
    view.set_footer_segments(metrics.footer_segments());
    view.set_descended(false);
    Ok(Frame {
        view: Some(view),
        id,
        sc_key: String::new(), // root: pinned, never evicted
        agent,
        path: path.to_path_buf(),
        from,
        last_used: 0,
    })
}

/// Build a child frame from the descend target (a `SubAgent` spawn OR an `AgentDone`
/// completion) at block index `idx` of the parent's `parent_view`. A spawn reuses its
/// already-parsed child `blocks`; a completion (and any agent whose child wasn't pre-loaded) loads
/// the child transcript from disk. `None` if `idx` isn't a descendable agent block. Takes the
/// parent's view + `agent`/`root_path` (not the whole `Frame`) so [`ensure_loaded`] can rebuild an
/// evicted frame from just its parent's loaded view.
fn build_child_frame(
    args: &Args,
    cache: &TuiCache,
    parent_view: &View,
    agent: Agent,
    root_path: &Path,
    idx: crate::model::BlockIndex,
) -> Result<Option<Frame>, ChildUnavailable> {
    let Some(dref) = parent_view.descend_ref_at(idx) else {
        return Ok(None);
    };
    // Own the fields we need so the borrow of `parent_view` ends before we build the view.
    let mut blocks = dref.blocks;
    let agent_type = dref.agent_type;
    let subtree_cost = dref.subtree_cost;
    let agent_id = dref.agent_id;
    let title = if agent_id.is_empty() {
        agent_type.clone()
    } else {
        agent_id.clone()
    };
    // Live-tail an open child from its own file (Stage 6): when following, tail
    // `subagents/agent-<id>.jsonl`; the child grows independently of the parent.
    let child_transcript = discover::subagent_source(agent, root_path, &agent_id)
        .map(|f| crate::Transcript::open(agent, f));
    // Parse the child once via the library entry point (enriched: its own sub-agent tree),
    // giving BOTH its blocks and its own metrics in one read — the footer below reuses the
    // metrics instead of a second parse. A running agent's child file often appears (or fills
    // in) AFTER the parent was parsed, so we load it fresh at descend time.
    let child_session = child_transcript
        .as_ref()
        .and_then(|t| t.parse_enriched().ok());
    if blocks.is_empty() {
        if let Some(s) = &child_session {
            blocks = s.blocks();
        }
    }
    if blocks.is_empty() {
        return Ok(None);
    }
    // Live-tail an open child through the CACHE's follower for its id (registered here, polled
    // by the event loop): its first poll re-folds the child transcript (== the blocks just
    // loaded), then only deltas. The residency budget in `enforce_cap` bounds how many child
    // followers stay materialized.
    //
    // A child is ADMITTED, exactly like the session the user opened (#163). Registration alone is
    // not enough: `poll_view` will not materialize a session `admit` never granted, because
    // `admit` is the only path that takes the lock. There used to be a cache-less shortcut here,
    // on the reasoning that a sub-agent is small and short-lived and not worth an entry — but a
    // session handed out without owning its entry is exactly the thing that produced two writers
    // on one log, and a child is not special enough to keep a second way of doing it.
    let live = child_transcript.is_some() && !agent_id.is_empty();
    if live {
        cache.register_new(
            &agent_id,
            child_transcript.clone().expect("live ⇒ transcript"),
        );
        if let Err(e) = admit_root(cache, &agent_id) {
            // Nobody else can hold a child of the session we are reading, so this is a machine
            // fault (an unwritable cache root, a lock naming a live stranger), not a normal
            // outcome. Say so and stop, rather than open a view that silently never ticks.
            return Err(ChildUnavailable(format!("sub-agent {agent_id}: {e}")));
        }
    }
    // The sidecar key (#75): stable across evict/reload cycles of this same child.
    let sc_key = if agent_id.is_empty() {
        format!("{}#{idx}", root_path.display())
    } else {
        agent_id.clone()
    };
    let fold = args.fold_policy();
    let mut view = View::new(blocks, title, live, fold);
    // Re-adopt a parked sidecar from a previous eviction of this child: the measure pass and
    // the user's fold/scroll state come back for free (discarded if the session changed shape;
    // a width change re-measures via the layout sentinel).
    if let Some(sc) = cache.aux_take(&sc_key) {
        let _ = view.adopt_sidecar(sc);
    }
    // A child descends further; `Esc` there ascends (never Back), so it isn't "go back".
    view.set_can_go_back(false);
    view.set_descended(true); // footer offers `↑ esc back`
    view.set_cwd(parent_view.cwd_ref().cloned());
    // Inherit the parent's live repo location for reveals: a child runs in the parent's
    // repo, and its own flat transcript dir wouldn't decode to that repo.
    view.set_reveal_root(parent_view.reveal_root_ref().cloned());
    // A descended child's attachment locators point into the child's own transcript file.
    view.set_source(child_transcript);
    // The child's footer shows ITS OWN token metrics (model/in/out/cached from the child
    // transcript) plus the rolled-up subtree cost — so the hint row is node-scoped. Reuse the
    // metrics from the parse above (no second read).
    let mut segs = vec![(agent_type, 3u8)];
    if let Some(s) = &child_session {
        segs = s.metrics.footer_segments();
    }
    // Prefer the subtree cost (child + descendants) over the child's own cost segment.
    if let Some(cost) = subtree_cost {
        segs.retain(|(t, _)| !t.starts_with("~$"));
        segs.push((format!("~${cost:.2}"), 7));
    }
    view.set_footer_segments(segs);
    Ok(Some(Frame {
        view: Some(view),
        id: if live { agent_id } else { String::new() },
        sc_key,
        agent,
        path: root_path.to_path_buf(),
        from: idx,
        last_used: 0,
    }))
}

/// A sub-agent that cannot be opened, and why — the TUI's one-line status flash (#163).
///
/// Distinct from "nothing to descend into" (`Ok(None)`), which is an ordinary answer for a block
/// that is not an agent. This one means the machine said no, and it must be visible: the whole
/// point of removing the cache-less shortcut is that a session which cannot own its entry is not
/// quietly opened anyway.
#[derive(Debug)]
struct ChildUnavailable(String);

/// How the viewer's input loop ended.
enum Outcome {
    /// Leave the program.
    Quit,
    /// Return to the session picker (honored only when launched via it).
    Back,
    /// Switch to another session (chosen via the `s` switcher overlay).
    Switch(PathBuf),
    /// Descend into the sub-agent at this block index of the current view.
    Descend(crate::model::BlockIndex),
    /// Ascend to the parent view (the sub-agent `Esc`/`⌫`), landing on the spawn block.
    Ascend,
}

#[allow(clippy::too_many_arguments)]
fn event_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    _agent: Agent,
    args: &Args,
    _path: &Path,
    view: &mut View,
    cache: &TuiCache,
    id: &str,
    descended: bool,
) -> Result<Outcome> {
    loop {
        term.draw(|f| view.draw(f))?;

        // No input this tick → pump the live tail incrementally (M16). `poll_delta` folds only the
        // newly-appended lines through the persistent `Replayer` (back-patching a cross-poll tool
        // result without a full re-parse) AND hands back the exact `changed_from` boundary, so the
        // view preserves fold toggles + render cache for the unchanged prefix without re-scanning
        // the whole block list. `apply_poll` swaps blocks + refreshes the footer in one call.
        if !event::poll(Duration::from_millis(250))? {
            // Every viewer tails. The only thing that can have no follower is a frame with no
            // session id (a sub-agent whose child transcript was never found).
            if !id.is_empty() {
                if let Some(Ok(d)) = cache.poll_view(id, crate::store::ArcLog::memory) {
                    // The tick carries the task op-log state (#15) — one call, no second
                    // cache lock; the on-disk side refreshes when the panel opens.
                    view.set_tasks(crate::engine::tasks::merged(
                        &d.tasks,
                        discover::session_tasks(_agent, _path),
                    ));
                    view.apply_view(d);
                }
            }
            continue;
        }
        match event::read()? {
            // While typing a `/` search, route keys to the search input.
            Event::Key(k) if k.kind != KeyEventKind::Release && view.is_searching() => {
                match k.code {
                    KeyCode::Esc => view.search_cancel(),
                    KeyCode::Enter => view.search_confirm(),
                    KeyCode::Backspace => view.search_backspace(),
                    KeyCode::Char(c) => view.search_input(c),
                    _ => {}
                }
            }
            // While the help overlay is open, `?`/Esc/`q` dismiss it; other keys are
            // swallowed (so `q` doesn't quit out from under the overlay).
            Event::Key(k) if k.kind != KeyEventKind::Release && view.is_help_open() => {
                if matches!(
                    k.code,
                    KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q')
                ) {
                    view.toggle_help();
                }
            }
            // While the active-sub-agents popup is open, route keys to it: ↑/↓ select,
            // Enter descends into the chosen agent, Esc / `a` close.
            // While the `t` task panel is open, route keys to it (#15).
            Event::Key(k) if k.kind != KeyEventKind::Release && view.tasks_popup_open() => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('t') | KeyCode::Char('q') => {
                        view.tasks_popup_close()
                    }
                    KeyCode::Up | KeyCode::Char('k') => view.tasks_popup_move(-1),
                    KeyCode::Down | KeyCode::Char('j') => view.tasks_popup_move(1),
                    _ => {}
                }
            }
            Event::Key(k) if k.kind != KeyEventKind::Release && view.agents_popup_open() => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('a') => view.agents_popup_close(),
                    KeyCode::Up | KeyCode::Char('k') => view.agents_popup_move(-1),
                    KeyCode::Down | KeyCode::Char('j') => view.agents_popup_move(1),
                    KeyCode::Enter => {
                        if let Some(idx) = view.agents_popup_confirm() {
                            return Ok(Outcome::Descend(idx));
                        }
                    }
                    _ => {}
                }
            }
            // While the session switcher is open, route keys to it. Enter switches
            // (reloads the chosen session); Esc closes it, keeping the current view.
            Event::Key(k) if k.kind != KeyEventKind::Release && view.is_switcher_open() => {
                match k.code {
                    KeyCode::Esc => view.switcher_close(),
                    KeyCode::Enter => {
                        if let Some(p) = view.switcher_confirm() {
                            return Ok(Outcome::Switch(p));
                        }
                    }
                    KeyCode::Up => view.switcher_up(),
                    KeyCode::Down => view.switcher_down(),
                    KeyCode::Backspace => view.switcher_backspace(),
                    KeyCode::Char(c) => view.switcher_input(c),
                    _ => {}
                }
            }
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
                // Any keystroke dismisses a lingering "Saved …" flash; the action below
                // may set a fresh one (e.g. Enter downloading an attachment).
                view.clear_flash();
                match k.code {
                    KeyCode::Char('?') => view.toggle_help(),
                    // `q` always leaves; `Esc` steps back to the session list when
                    // we came from the picker (the driver maps Back→quit otherwise).
                    KeyCode::Char('q') => return Ok(Outcome::Quit),
                    // Inside a sub-agent, `Esc`/`⌫` ascend to the parent; only at the
                    // root do they fall through to the picker (Back).
                    KeyCode::Esc | KeyCode::Backspace if descended => return Ok(Outcome::Ascend),
                    KeyCode::Esc => return Ok(Outcome::Back),
                    KeyCode::Char('j') | KeyCode::Down => view.scroll_by(1),
                    KeyCode::Char('k') | KeyCode::Up => view.scroll_by(-1),
                    KeyCode::Char('d') if ctrl => view.half_page(true),
                    KeyCode::Char('u') if ctrl => view.half_page(false),
                    KeyCode::PageDown => view.full_page(true),
                    KeyCode::PageUp => view.full_page(false),
                    KeyCode::Char('g') => view.jump_top(),
                    KeyCode::Char('G') => view.jump_bottom(),
                    KeyCode::Char(' ') => view.toggle_at_cursor(),
                    KeyCode::Char('T') => view.toggle_all(),
                    KeyCode::Char('t') => {
                        // Open the task panel with a FRESH disk read (#15) merged over
                        // the current op-log state.
                        if !view.tasks_popup_open() {
                            let oplog = cache
                                .resident_tasks(id)
                                .unwrap_or_else(|| view.tasks_snapshot());
                            view.set_tasks(crate::engine::tasks::merged(
                                &oplog,
                                discover::session_tasks(_agent, _path),
                            ));
                        }
                        view.toggle_tasks_popup();
                    }
                    KeyCode::Char(']') => view.focus_next(),
                    KeyCode::Char('[') => view.focus_prev(),
                    // Enter activates the focused block: fold toggle, descend a sub-agent,
                    // or download (embedded) / reveal (path-only) an attachment.
                    KeyCode::Enter => match view.activate_focused() {
                        Some(crate::tui::view::Action::Reveal(p)) => reveal_in_file_manager(&p),
                        Some(crate::tui::view::Action::Descend(idx)) => {
                            return Ok(Outcome::Descend(idx))
                        }
                        None => {}
                    },
                    KeyCode::Char('/') => view.search_start(),
                    KeyCode::Char('n') => view.search_next(),
                    KeyCode::Char('N') => view.search_prev(),
                    // Open the session switcher (only when enabled, i.e. --latest).
                    KeyCode::Char('s') if view.can_open_picker() => {
                        view.open_switcher(discover::candidates_all(args.agent))
                    }
                    // Open the active-sub-agents popup (only when this node has one).
                    KeyCode::Char('a') if view.can_open_agents() => view.open_agents_popup(),
                    _ => {}
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollDown => {
                    view.clear_flash();
                    view.scroll_by(3);
                }
                MouseEventKind::ScrollUp => {
                    view.clear_flash();
                    view.scroll_by(-3);
                }
                // Press begins a potential text selection (also the anchor for a
                // click-to-fold if the mouse doesn't move before release).
                // While the active-sub-agents popup is open it owns every click: a row
                // descends, anything else is swallowed (never leaks to the content
                // underneath). Selection drags are suppressed too.
                MouseEventKind::Down(MouseButton::Left) if view.agents_popup_open() => {}
                MouseEventKind::Drag(MouseButton::Left) if view.agents_popup_open() => {}
                MouseEventKind::Up(MouseButton::Left) if view.agents_popup_open() => {
                    if let crate::tui::view::PopupClick::Descend(idx) =
                        view.agents_popup_click(m.row)
                    {
                        return Ok(Outcome::Descend(idx));
                    }
                }
                MouseEventKind::Down(MouseButton::Left)
                    if (m.row as usize) < view.content_rows() =>
                {
                    view.sel_begin(m.row, m.column)
                }
                // Drag extends the selection.
                MouseEventKind::Drag(MouseButton::Left)
                    if (m.row as usize) < view.content_rows() =>
                {
                    view.sel_extend(m.row, m.column)
                }
                // Release: a drag copies the selected text; a plain click folds.
                MouseEventKind::Up(MouseButton::Left) => {
                    if view.dragged() {
                        if let Some(text) = view.take_selection_text() {
                            crate::tui::clipboard::copy(&text);
                        }
                    } else {
                        view.clear_selection();
                        view.clear_flash();
                        let row = m.row as usize;
                        if row < view.content_rows() {
                            // A click activates whatever it lands on: descend a sub-agent,
                            // download/reveal an attachment (or a tool-header path), else fold.
                            match view.click_at(m.row, m.column) {
                                Some(crate::tui::view::Action::Reveal(p)) => {
                                    reveal_in_file_manager(&p)
                                }
                                Some(crate::tui::view::Action::Descend(idx)) => {
                                    return Ok(Outcome::Descend(idx))
                                }
                                None => {}
                            }
                        } else if row == view.content_rows() {
                            // Footer row: the nav labels are click targets.
                            match view.footer_click(m.column as usize) {
                                crate::tui::view::FooterHit::EscBack if descended => {
                                    return Ok(Outcome::Ascend)
                                }
                                crate::tui::view::FooterHit::ActiveAgents => {
                                    view.open_agents_popup()
                                }
                                _ => {}
                            }
                        }
                    }
                }
                // Hover a foldable header to focus it (brighten).
                MouseEventKind::Moved if (m.row as usize) < view.content_rows() => {
                    view.hover_row(m.row)
                }
                _ => {}
            },
            Event::Resize(_, _) => view.invalidate_wrap(),
            _ => {}
        }
    }
}

/// Run the picker's input loop against an already-set-up terminal. Returns the
/// chosen transcript path, or None if the user pressed Esc/Ctrl-c (quit).
fn pick_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    picker: &mut Picker,
) -> Result<Option<PathBuf>> {
    loop {
        term.draw(|f| picker.draw(f))?;
        if let Event::Key(k) = event::read()? {
            if k.kind == KeyEventKind::Release {
                continue;
            }
            let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
            match k.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char('c') if ctrl => return Ok(None),
                KeyCode::Enter => return Ok(picker.selected_path()),
                KeyCode::Up => picker.up(),
                KeyCode::Down => picker.down(),
                KeyCode::Backspace => picker.backspace(),
                KeyCode::Char(c) => picker.push_char(c),
                _ => {}
            }
        }
    }
}

/// Columns to lay `--dump` out to: `--width` if given, else the real terminal
/// width, else `render::DUMP_WIDTH`.
fn dump_width(args: &Args) -> usize {
    if let Some(w) = args.width {
        return w.max(1);
    }
    crossterm::terminal::size()
        .ok()
        .map(|(c, _)| c as usize)
        .filter(|c| *c > 0)
        .unwrap_or(crate::tui::render::DUMP_WIDTH)
}

/// `--dump`: render the whole transcript at a chosen width and either print plain
/// text to stdout (`--dump -`) or write `<stem>.txt` + `<stem>.ansi` (the `.ansi`
/// carries SGR colour). With no `<stem>`, the stem is deduced from the session.
pub fn dump(args: &Args, path: &Path) -> Result<()> {
    let agent = discover::detect_agent(path);
    // Dogfood the library entry point: one `parse_session_enriched_as` yields the full block
    // tree (incl. sub-agents). `--dump` only needs the blocks here.
    let blocks = crate::parse_session_enriched_as(agent, path)?.blocks();
    let width = dump_width(args);
    // Render through the same pipeline as the live TUI (wrap + per-row background
    // fill + diff inset) so the dump matches the on-screen render byte-for-byte.
    // Fold with the same policy as the TUI (default-folded thinking/reads/tools…),
    // so the dump reflects what the viewer actually shows; `--full` expands it all.
    let fold = args.fold_policy();
    let mut view = View::new(blocks, "dump", false, fold);
    let lines = view.rendered_lines(width as u16);

    // `dump` is only called when `args.dump` is Some(..).
    let stem = match args.dump.as_ref().and_then(|o| o.as_deref()) {
        Some("-") => {
            for line in &lines {
                println!("{}", plain_line(line));
            }
            return Ok(());
        }
        Some(s) => s.to_string(),
        None => deduce_stem(path, Some(width)),
    };

    let txt: String = lines.iter().map(plain_line).collect::<Vec<_>>().join("\n");
    let ansi: String = lines.iter().map(ansi_line).collect::<Vec<_>>().join("\n");
    std::fs::write(format!("{stem}.txt"), format!("{txt}\n"))?;
    std::fs::write(format!("{stem}.ansi"), format!("{ansi}\n"))?;
    eprintln!(
        "wrote {stem}.txt + {stem}.ansi ({width} cols, {} lines)",
        lines.len()
    );
    println!("{stem}"); // last stdout line = the stem, for scripting
    Ok(())
}

/// A line's text with all styling flattened away (the `.txt` form).
fn plain_line(line: &ratatui::text::Line) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// A line re-emitted with SGR escapes (the `.ansi` form): each run of same-styled
/// text is wrapped in `ESC[..m … ESC[0m`; unstyled runs pass through verbatim.
/// Adjacent spans that share a style are coalesced into one run so the output
/// matches a real terminal's compact encoding (word-wrapping splits a styled
/// paragraph into per-word spans, but they carry identical styles).
fn ansi_line(line: &ratatui::text::Line) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < line.spans.len() {
        let style = line.spans[i].style;
        // Absorb the run of following spans with the same style.
        let mut j = i + 1;
        while j < line.spans.len() && line.spans[j].style == style {
            j += 1;
        }
        let content: String = line.spans[i..j]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        i = j;
        let sgr = sgr_params(style);
        if sgr.is_empty() {
            out.push_str(&content);
        } else {
            out.push_str(&format!("\x1b[{}m{}\x1b[0m", sgr.join(";"), content));
        }
    }
    out
}

/// SGR numeric params for a ratatui `Style` (modifiers + fg/bg), empty if default.
fn sgr_params(style: ratatui::style::Style) -> Vec<String> {
    use ratatui::style::Modifier;
    let mut p = Vec::new();
    let m = style.add_modifier;
    if m.contains(Modifier::BOLD) {
        p.push("1".into());
    }
    if m.contains(Modifier::DIM) {
        p.push("2".into());
    }
    if m.contains(Modifier::ITALIC) {
        p.push("3".into());
    }
    if m.contains(Modifier::UNDERLINED) {
        p.push("4".into());
    }
    if let Some(c) = style.fg {
        p.extend(color_sgr(c, true));
    }
    if let Some(c) = style.bg {
        p.extend(color_sgr(c, false));
    }
    p
}

/// SGR params for one colour, as a foreground (`fg=true`) or background.
fn color_sgr(c: ratatui::style::Color, fg: bool) -> Vec<String> {
    use ratatui::style::Color;
    let named = |n: u32| vec![(if fg { 30 + n } else { 40 + n }).to_string()];
    let bright = |n: u32| vec![(if fg { 90 + n } else { 100 + n }).to_string()];
    let base = if fg { "38" } else { "48" };
    match c {
        Color::Reset => vec![],
        Color::Black => named(0),
        Color::Red => named(1),
        Color::Green => named(2),
        Color::Yellow => named(3),
        Color::Blue => named(4),
        Color::Magenta => named(5),
        Color::Cyan => named(6),
        Color::Gray => named(7),
        Color::DarkGray => bright(0),
        Color::LightRed => bright(1),
        Color::LightGreen => bright(2),
        Color::LightYellow => bright(3),
        Color::LightBlue => bright(4),
        Color::LightMagenta => bright(5),
        Color::LightCyan => bright(6),
        Color::White => bright(7),
        Color::Indexed(n) => vec![base.into(), "5".into(), n.to_string()],
        Color::Rgb(r, g, b) => vec![
            base.into(),
            "2".into(),
            r.to_string(),
            g.to_string(),
            b.to_string(),
        ],
    }
}

/// Where a session SWITCH's time actually goes — the follow-up measurement to #107, which took
/// the 102 MB case from 5.9 s to 816 ms and left it still perceptible.
///
/// It times the REAL [`build_frame`] a switch calls (`Outcome::Switch` → `make_cache` →
/// `build_frame`), then the first layout and draw, against a real transcript — a synthetic one
/// would not reproduce the block mix that drives the cost. `#[ignore]`d: a measurement, not an
/// assertion. Run with:
///   SWITCH_COST_PATH=~/.claude/projects/<proj>/<id>.jsonl \
///   cargo test -p claude-replay-tui --release switch_phase_breakdown -- --ignored --nocapture
#[cfg(test)]
mod switch_cost {
    use super::*;
    use std::time::{Duration, Instant};

    /// Pin the width: a wider terminal wraps less and measures faster, so inheriting the
    /// harness's happens-to-be width would make runs incomparable.
    fn width() -> u16 {
        std::env::var("SWITCH_COST_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(120)
    }
    const HEIGHT: u16 = 40;

    #[test]
    #[ignore]
    fn switch_phase_breakdown() {
        let Some(path) = std::env::var_os("SWITCH_COST_PATH").map(std::path::PathBuf::from) else {
            eprintln!("set SWITCH_COST_PATH=<transcript.jsonl>");
            return;
        };
        let mb = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) as f64 / 1e6;
        let args = Args {
            latest: true,
            ..Default::default()
        };

        // A switch drops the old cache and builds a fresh one (#107), so the resume below is a
        // genuine cold-process resume off the durable stream, not a warm in-memory hit.
        let t = Instant::now();
        let cache = make_cache(&args);
        let t_cache = t.elapsed();

        let t = Instant::now();
        let mut frame = build_frame(&args, &cache, &path, true, 0).expect("frame");
        let t_frame = t.elapsed();

        let view = frame.view.as_mut().expect("loaded");

        let t = Instant::now();
        view.layout(width(), HEIGHT);
        let t_layout = t.elapsed();

        let mut term =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width(), HEIGHT)).unwrap();
        let t = Instant::now();
        term.draw(|f| view.draw(f)).unwrap();
        let t_draw = t.elapsed();

        let total = t_cache + t_frame + t_layout + t_draw;
        eprintln!("\n{} MB, {}x{HEIGHT}", mb.round(), width());
        let row = |name: &str, d: Duration| {
            eprintln!(
                "  {name:<26} {:>8.1} ms  {:>5.1}%",
                d.as_secs_f64() * 1e3,
                d.as_secs_f64() / total.as_secs_f64() * 100.0
            )
        };
        row("make_cache", t_cache);
        row("build_frame (resume+fold)", t_frame);
        row("FIRST LAYOUT (measure)", t_layout);
        row("first draw", t_draw);
        row("TOTAL", total);

        // WHERE inside the measure: charge each block's serial measure cost to its kind, so the
        // answer to "measure fewer blocks vs measure each block more cheaply" is read off the
        // data rather than guessed. Serial on purpose — parallel timings would attribute wall
        // clock, not work.
        let mut by_kind: std::collections::BTreeMap<&str, (usize, f64, usize)> = Default::default();
        for (b, blk) in view.blocks_for_measure().iter().enumerate() {
            let t = Instant::now();
            let h = view.measure_one_for_probe(b);
            let e = by_kind
                .entry(claude_replay_core::model::fold_key(blk))
                .or_default();
            e.0 += 1;
            e.1 += t.elapsed().as_secs_f64() * 1e3;
            e.2 += h;
        }
        let mut rows: Vec<_> = by_kind.into_iter().collect();
        rows.sort_by(|a, b| b.1 .1.partial_cmp(&a.1 .1).unwrap());
        eprintln!(
            "\n  {:<12} {:>7} {:>10} {:>10} {:>9}",
            "kind", "count", "ms", "ms/block", "lines"
        );
        for (k, (n, ms, lines)) in rows {
            eprintln!(
                "  {k:<12} {n:>7} {ms:>10.1} {:>10.3} {lines:>9}",
                ms / n as f64
            );
        }

        // How often does a row actually WRAP? A row narrower than the terminal occupies exactly
        // one line however the highlighter split it into spans, so its height needs no syntect at
        // all. This says what fraction of the work that observation can remove.
        // Per KIND, because the fallback is per block: a kind whose blocks nearly always contain
        // one over-wide row keeps paying full price, and `edit` is the only kind whose cost
        // matters. `ms_clean` is the measure time that a plain-first pass could actually avoid.
        let mut wrap_by_kind: std::collections::BTreeMap<&str, (usize, usize, f64)> =
            Default::default();
        let (mut pre_t, mut post_t) = (0usize, 0usize);
        for (b, blk) in view.blocks_for_measure().iter().enumerate() {
            let t = Instant::now();
            let (pre, post) = view.wrap_ratio_for_probe(b);
            let ms = t.elapsed().as_secs_f64() * 1e3;
            pre_t += pre;
            post_t += post;
            let e = wrap_by_kind
                .entry(claude_replay_core::model::fold_key(blk))
                .or_default();
            e.0 += 1;
            if post > pre {
                e.1 += 1;
            } else {
                e.2 += ms;
            }
        }
        eprintln!(
            "\n  rows {pre_t} -> {post_t} after wrapping ({:.1}% growth)",
            (post_t - pre_t) as f64 / pre_t as f64 * 100.0
        );
        eprintln!(
            "  {:<12} {:>7} {:>10} {:>9} {:>12}",
            "kind", "blocks", "wrapping", "%", "ms_clean"
        );
        let mut wr: Vec<_> = wrap_by_kind.into_iter().collect();
        wr.sort_by(|a, b| b.1 .2.partial_cmp(&a.1 .2).unwrap());
        for (k, (n, w, ms)) in wr {
            eprintln!(
                "  {k:<12} {n:>7} {w:>10} {:>8.1}% {ms:>11.1}",
                w as f64 / n as f64 * 100.0
            );
        }
        // The equality the optimisation rests on, over EVERY block of a real transcript — the
        // synthetic cases in `measure_matches_render_for_every_block_and_width` cannot cover
        // the block mix a 107 MB session actually contains.
        let mut fallbacks = 0usize;
        for b in 0..view.blocks_for_measure().len() {
            let (fast, real, fell_back) = view.measure_check_for_probe(b);
            assert_eq!(fast, real, "block {b} measured {fast}, renders {real}");
            fallbacks += usize::from(fell_back);
        }
        eprintln!(
            "\n  measure == render for all {} blocks; {fallbacks} took the styled fallback ({:.1}%)",
            view.blocks_for_measure().len(),
            fallbacks as f64 / view.blocks_for_measure().len() as f64 * 100.0
        );
        // A RE-VISIT — the whole of what `Outcome::Switch` does on the way back (#109): park the
        // geometry in the cache's sidecar slot, RELEASE (quiesce + unlock, blocks stay resident),
        // then re-admit and re-adopt. Nothing here is a stand-in: this is the same sequence the
        // switch arm runs, so the number is the switch-back a user feels.
        let id = again_id(&path);
        let parked =
            std::mem::replace(view, View::new_shared(vec![], "", true, Default::default()))
                .into_sidecar();
        cache.aux_put(&id, parked);
        cache.release(&id);

        let t = Instant::now();
        let mut again = build_frame(&args, &cache, &path, true, 0).expect("frame");
        let t_re_frame = t.elapsed();
        let v2 = again.view.as_mut().expect("loaded");
        let t = Instant::now();
        let adopted = cache.aux_take(&id).map(|sc| v2.adopt_sidecar(sc));
        v2.layout(width(), HEIGHT);
        let t_re_layout = t.elapsed();
        eprintln!(
            "\n  RE-VISIT (release → re-admit): frame {:.1} ms + layout {:.1} ms (sidecar adopted = {adopted:?})",
            t_re_frame.as_secs_f64() * 1e3,
            t_re_layout.as_secs_f64() * 1e3
        );

        cache.release_all();
    }

    /// The cache id `build_frame` derives for a transcript — its stem.
    fn again_id(path: &std::path::Path) -> String {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

    /// **A descended sub-agent really ticks** — the whole path, not just `admit`.
    ///
    /// `build_child_frame` → `admit_root` → the event loop's poll for that child's id. It used to
    /// open children cache-less, precisely because a durable cache will not materialize a session
    /// `admit` never granted; #163 removed that shortcut, so a child now takes an entry and a lock
    /// like anything else. If that wiring is wrong the failure is silent and specific — the child
    /// opens, shows its already-parsed blocks, and never advances again — which is exactly what
    /// the shortcut existed to prevent.
    #[test]
    fn a_descended_child_owns_its_entry_and_keeps_ticking() {
        use std::io::Write;
        let base = std::env::temp_dir().join(format!("cr-descend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let sadir = base.join("proj").join("sid").join("subagents");
        std::fs::create_dir_all(&sadir).unwrap();
        let sess = base.join("proj").join("sid.jsonl");
        std::fs::File::create(&sess)
            .unwrap()
            .write_all(concat!(
                r#"{"type":"user","cwd":"/w","message":{"role":"user","content":[{"type":"text","text":"go"}]},"timestamp":"2026-08-01T10:00:00Z"}"#, "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_A","name":"Agent","input":{"subagent_type":"general-purpose","description":"child","prompt":"go"}}]},"timestamp":"2026-08-01T10:00:01Z"}"#, "\n",
                r#"{"type":"user","toolUseResult":{"agentId":"achild01","status":"completed"},"message":{"content":[{"type":"tool_result","tool_use_id":"toolu_A","content":"done"}]},"timestamp":"2026-08-01T10:00:02Z"}"#, "\n",
            ).as_bytes())
            .unwrap();
        let child_path = sadir.join("agent-achild01.jsonl");
        std::fs::File::create(&child_path)
            .unwrap()
            .write_all(concat!(
                r#"{"type":"user","message":{"content":"go"},"timestamp":"2026-08-01T10:00:01Z"}"#, "\n",
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"c1","name":"Read","input":{"file_path":"/a"}}]},"timestamp":"2026-08-01T10:00:02Z"}"#, "\n",
            ).as_bytes())
            .unwrap();

        // A durable cache at the TEST's own root — never the developer's.
        let root = base.join("cache");
        let cache = TuiCache::durable(Presentation::Tui, root.clone(), Versions::current(None));
        let args = Args {
            target: Some(sess.display().to_string()),
            ..Default::default()
        };
        let parent = build_frame(&args, &cache, &sess, false, 0).expect("parent frame");
        let view = parent.view.as_ref().expect("loaded");
        let spawn = (0..view.block_kinds().len())
            .find(|i| view.descend_ref_at(*i).is_some())
            .expect("the fixture must have a descendable spawn");

        let child = build_child_frame(&args, &cache, view, Agent::CLAUDE, &sess, spawn)
            .expect("a free child entry must be admitted, not refused")
            .expect("the spawn is descendable");
        assert!(!child.id.is_empty(), "a live child is followed by id");
        assert!(
            cache::admit::entry_dir(&root, Presentation::Tui, &child.id).exists(),
            "and it owns a durable entry, like every other session"
        );

        // The point of admitting it: the follower materializes and the child advances.
        let delta = cache
            .poll_view(&child.id, crate::store::ArcLog::memory)
            .expect("registered")
            .expect("readable");
        assert!(
            delta.committed_len + delta.provisional.len() > 0,
            "an admitted child folds and ticks"
        );

        drop(child);
        drop(parent);
        cache.release_all();
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The `-f --html` multi-open contract: a pick OPENS and the picker STAYS — only an
    /// explicit quit ends the loop. Drives the loop's real decision function with synthetic
    /// events (no TTY), so "Enter and a click do the same thing, and neither exits" is
    /// pinned rather than assumed.
    #[test]
    fn multi_open_picker_confirms_without_quitting() {
        use crossterm::event::{KeyEvent, KeyModifiers, MouseEvent};
        let cands: Vec<Candidate> = ["alpha", "bravo"]
            .iter()
            .map(|n| Candidate {
                path: std::path::PathBuf::from(format!("/tmp/{n}.jsonl")),
                mtime: std::time::SystemTime::now(),
                project: n.to_string(),
                snippet: "session".into(),
                cwd_affinity: false,
                agent: Agent::CLAUDE,
            })
            .collect();
        let mut picker = Picker::new(cands);
        // Lay the list out once so click geometry is real.
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 10)).unwrap();
        term.draw(|f| picker.draw(f)).unwrap();

        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let click = |row| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };

        // Enter confirms the first entry — and is NOT a quit.
        assert_eq!(
            pick_action(&key(KeyCode::Enter), &mut picker),
            PickAction::Confirm
        );
        assert_eq!(
            picker.selected_path().unwrap().file_name().unwrap(),
            "alpha.jsonl"
        );

        // A click on the second row confirms it too — same effect as Enter.
        assert_eq!(pick_action(&click(2), &mut picker), PickAction::Confirm);
        assert_eq!(
            picker.selected_path().unwrap().file_name().unwrap(),
            "bravo.jsonl"
        );

        // A click on the header is not a pick; navigation keys are not picks.
        assert_eq!(pick_action(&click(0), &mut picker), PickAction::None);
        assert_eq!(
            pick_action(&key(KeyCode::Up), &mut picker),
            PickAction::None
        );

        // Only Esc / Ctrl-C end the loop.
        assert_eq!(
            pick_action(&key(KeyCode::Esc), &mut picker),
            PickAction::Quit
        );
        assert_eq!(
            pick_action(
                &Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
                &mut picker
            ),
            PickAction::Quit
        );
    }

    /// The sub-agent LRU policy (#36): [`enforce_cap`] keeps at most `MAX_RESIDENT_SUBAGENTS`
    /// loaded sub-agent frames, evicting the least-recently-viewed first, while pinning the root
    /// (index 0) and never evicting the current top (it's on screen).
    #[test]
    fn lru_caps_subagents_pinning_root_and_top() {
        fn frame(last_used: u64) -> Frame {
            Frame {
                sc_key: String::new(),
                view: Some(View::new(
                    Vec::new(),
                    "t",
                    false,
                    crate::fold::FoldPolicy::default(),
                )),
                id: String::new(),
                agent: Agent::CLAUDE,
                path: std::path::PathBuf::from("/x"),
                from: 0,
                last_used,
            }
        }
        // root + (MAX+2) sub-agents, all loaded; last_used == index, so the shallow ones are LRU.
        let n = MAX_RESIDENT_SUBAGENTS + 2;
        let mut stack: Vec<Frame> = (0..=n as u64).map(frame).collect();
        let top = stack.len() - 1;
        enforce_cap(&TuiCache::new(), &mut stack);

        let loaded: Vec<usize> = (1..stack.len())
            .filter(|&i| stack[i].view.is_some())
            .collect();
        assert_eq!(loaded.len(), MAX_RESIDENT_SUBAGENTS, "capped to the max");
        assert!(stack[0].view.is_some(), "root is pinned");
        assert!(
            stack[top].view.is_some(),
            "the current top is never evicted"
        );
        // The two least-recently-used sub-agents (indices 1,2) are the ones dropped.
        assert!(
            stack[1].view.is_none() && stack[2].view.is_none(),
            "LRU evicted first"
        );
        assert!(stack[3].view.is_some(), "more-recent sub-agents kept");
    }

    /// Strip `ESC[..m` SGR sequences (char-wise so multibyte content survives).
    fn strip_sgr(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for d in chars.by_ref() {
                    if d == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn dump_txt_is_plain_and_ansi_round_trips() {
        let line = Line::from(vec![
            Span::raw("plain ──┼ "),
            Span::styled("bold", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" blue", Style::default().fg(Color::Indexed(153))),
        ]);
        let txt = plain_line(&line);
        let ansi = ansi_line(&line);
        assert!(
            !txt.contains('\x1b'),
            "txt must have no escape codes: {txt:?}"
        );
        assert!(ansi.contains('\x1b'), "ansi must carry SGR: {ansi:?}");
        assert_eq!(
            strip_sgr(&ansi),
            txt,
            "ansi must strip back to the plain text"
        );
        assert!(ansi.contains("\x1b[1m"), "bold SGR present");
        assert!(ansi.contains("\x1b[38;5;153m"), "256-colour fg SGR present");
    }

    #[test]
    fn deduced_stem_shape() {
        // deduce_stem now reads cwd/sessionId from a bounded prefix of the file,
        // so write the first line to a temp transcript and point it there.
        let content =
            r#"{"sessionId":"094539f2-40d7-4abc","cwd":"/Users/dev/projects/claude-replay"}"#;
        let dir = std::env::temp_dir();
        let file = dir.join("claude-replay-deduce-stem-test-094539f2-40d7-4abc.jsonl");
        std::fs::write(&file, format!("{content}\n")).unwrap();
        let stem = deduce_stem(&file, Some(140));
        std::fs::remove_file(&file).ok();
        assert!(stem.starts_with("claude-replay-"), "basename: {stem}");
        assert!(stem.ends_with("-094539-140"), "sessionid6 + width: {stem}");
        let hex = stem
            .strip_prefix("claude-replay-")
            .and_then(|s| s.strip_suffix("-094539-140"))
            .expect("hash segment");
        assert_eq!(hex.len(), 6, "pathhash is 6 hex chars: {hex:?}");
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()), "hex: {hex:?}");
    }
    /// **The switch-back path, end to end** (#109): park the geometry, `release`, then rebuild
    /// the frame. What is pinned is that the second frame comes back with the SAME blocks — a
    /// retained session is already folded to EOF, so its first poll is idle, and the branch that
    /// used to read "idle ⇒ empty or unreadable transcript" would re-parse the whole file here.
    /// On a 107 MB session that is a multi-second stall on every switch back, and no cheaper
    /// test reaches it: the cache-level suite never goes through `build_frame`.
    #[test]
    fn a_released_frame_rebuilds_from_the_retained_session() {
        let dir = std::env::temp_dir().join(format!("cr-retain-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("s.jsonl");
        std::fs::write(
            &src,
            "{\"type\":\"user\",\"cwd\":\"/r\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"first\"}]},\"timestamp\":\"2026-08-07T10:00:00Z\"}\n{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"reply\"}]},\"timestamp\":\"2026-08-07T10:00:01Z\"}\n{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"second\"}]},\"timestamp\":\"2026-08-07T10:00:02Z\"}\n",
        )
        .unwrap();

        // A durable cache rooted in this test's own directory (never the user's real one).
        let cache: TuiCache = TuiCache::durable(
            Presentation::Tui,
            dir.join("cache"),
            Versions::current(None),
        );
        let args = Args::default();

        let mut first = build_frame(&args, &cache, &src, false, 0).expect("frame");
        let id = first.id.clone();
        let view = first.view.as_mut().expect("loaded");
        view.layout(80, 24);
        let before = view.blocks_for_measure().to_vec();
        assert!(!before.is_empty(), "the fixture has blocks");
        // Only the COMMITTED prefix is retained by identity; the open turn is re-wrapped in
        // fresh `Arc`s on every tick by design (`ViewDelta::provisional`).
        let committed = cache.shared_peek(&id).expect("resident").counters().2;
        assert!(committed > 0, "the fixture commits a turn");

        // Exactly what `Outcome::Switch` does to the session it leaves.
        cache.aux_put(&id, first.view.take().expect("loaded").into_sidecar());
        cache.release(&id);

        let mut again = build_frame(&args, &cache, &src, false, 0).expect("frame");
        let v2 = again.view.as_mut().expect("loaded");
        assert!(
            cache.aux_take(&id).is_some_and(|sc| v2.adopt_sidecar(sc)),
            "the parked geometry still fits the retained blocks"
        );
        // POINTER equality, not block equality: a cold re-parse of the same file yields blocks
        // that COMPARE equal, so only the `Arc` identity separates "retained the resident copy"
        // from "silently re-read 107 MB and got the same answer".
        let after = v2.blocks_for_measure();
        assert_eq!(after, &before[..], "the same blocks, in the same order");
        assert!(
            before[..committed]
                .iter()
                .zip(after)
                .all(|(a, b)| std::sync::Arc::ptr_eq(a, b)),
            "the rebuilt frame must hold the SAME committed copies — a re-parse allocates new ones"
        );
        cache.release_all();
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// The Held refusal is TYPED (#110): the switch arm downcasts to `HeldElsewhere` to stay
    /// put with a one-line flash, while the launch path keeps the full printed guidance. The
    /// holder is a real live process (a spawned `sleep`), because `admit` believes a lock only
    /// when its pid is alive — a made-up pid is reclaimed, not refused.
    #[test]
    #[cfg(unix)]
    fn a_held_refusal_is_typed_with_both_message_forms() {
        use claude_replay_present::cache::lock;
        let dir = std::env::temp_dir().join(format!("cr-held-typed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("s.jsonl");
        std::fs::write(
            &src,
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n",
        )
        .unwrap();

        let mut holder = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let entry = claude_replay_present::cache::admit::entry_dir(
            &dir.join("cache"),
            Presentation::Tui,
            "s",
        );
        std::fs::create_dir_all(&entry).unwrap();
        std::fs::write(
            lock::lock_path(&entry),
            serde_json::to_string(&claude_replay_present::cache::Holder {
                pid: holder.id(),
                dir: entry.clone(),
                note: Some(crate::store::TuiNote {
                    pane: Some("%7".into()),
                }),
            })
            .unwrap(),
        )
        .unwrap();

        let cache: TuiCache = TuiCache::durable(
            Presentation::Tui,
            dir.join("cache"),
            Versions::current(None),
        );
        cache.register("s", crate::Transcript::open(Agent::CLAUDE, src));
        let e = match admit_root(&cache, "s") {
            Err(e) => e,
            Ok(_) => panic!("a live holder must refuse"),
        };
        let h = e
            .downcast_ref::<HeldElsewhere>()
            .expect("the refusal must be typed, or the switch path cannot catch it");
        assert_eq!(h.pid, holder.id());
        let flash = h.flash();
        assert!(
            flash.contains("in use") && flash.contains("tmux attach -t %7"),
            "one-line form names the pane: {flash:?}"
        );
        assert!(!flash.contains('\n'), "a flash must fit the one status row");
        let launch = e.to_string();
        assert!(
            launch.contains("already open") && launch.contains("--no-cache"),
            "the launch form keeps the full guidance: {launch:?}"
        );

        let _ = holder.kill();
        let _ = holder.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Terminal wiring + input loop. All view state/drawing lives in `view::View`
//! (testable headless via ratatui's TestBackend).

use crate::picker::Picker;
use crate::tail::TailReader;
use crate::view::View;
use crate::{discover, model, Args};
use anyhow::Result;
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
    let mut cands = discover::candidates();
    if cands.is_empty() {
        anyhow::bail!(
            "no transcripts found under {}",
            discover::projects_dir().display()
        );
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

/// View a session, staying in the viewer across `s`-switches: an `Outcome::Switch`
/// swaps the path and re-views. Returns only `Quit` or `Back`.
fn run_view_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    args: &Args,
    mut path: PathBuf,
    can_go_back: bool,
) -> Result<Outcome> {
    loop {
        match view_session(term, args, &path, can_go_back)? {
            Outcome::Switch(p) => path = p,
            other => return Ok(other),
        }
    }
}

/// Build a `View` for `path` and run its input loop. `can_go_back` marks that we
/// arrived via the picker, so `Esc` returns to the list (Outcome::Back).
fn view_session<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    args: &Args,
    path: &Path,
    can_go_back: bool,
) -> Result<Outcome> {
    // Stream the parse from the file (one line resident at a time) instead of
    // reading the whole transcript into a `String` + `Vec<Value>` — a 298 MB
    // session otherwise peaks at ~2 GB. See `STREAMING-PARSE-DESIGN.md`.
    let blocks = model::parse_path(path, args)?;
    let title = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session")
        .to_string();
    let mut reader = args.follow.then(|| TailReader::open_at_end(path));
    let fold = crate::view::FoldPolicy::from_args(args);
    let mut view = View::new(blocks, title, reader.is_some(), fold);
    view.set_can_go_back(can_go_back);
    // `--latest` didn't show a list, so offer `s` to reach the session picker.
    view.set_can_open_picker(args.latest);
    // Re-open for the metrics pass (also streaming) rather than hold the content.
    let metrics = crate::metrics::parse_reader(std::io::BufReader::new(std::fs::File::open(path)?));
    view.set_metrics(metrics.footer());

    event_loop(term, args, path, &mut view, &mut reader)
}

/// How the viewer's input loop ended.
enum Outcome {
    /// Leave the program.
    Quit,
    /// Return to the session picker (honored only when launched via it).
    Back,
    /// Switch to another session (chosen via the `s` switcher overlay).
    Switch(PathBuf),
}

fn event_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    args: &Args,
    path: &Path,
    view: &mut View,
    reader: &mut Option<TailReader>,
) -> Result<Outcome> {
    loop {
        term.draw(|f| view.draw(f))?;

        // No input this tick → pump the live tail.
        if !event::poll(Duration::from_millis(250))? {
            if let Some(r) = reader.as_mut() {
                let p = r.poll().unwrap_or_default();
                if p.reset {
                    if let Ok(blocks) = model::parse_path(path, args) {
                        view.reset(blocks);
                    }
                }
                if !p.lines.is_empty() {
                    view.ingest(model::parse(&p.lines.join("\n"), args));
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
                match k.code {
                    KeyCode::Char('?') => view.toggle_help(),
                    // `q` always leaves; `Esc` steps back to the session list when
                    // we came from the picker (the driver maps Back→quit otherwise).
                    KeyCode::Char('q') => return Ok(Outcome::Quit),
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
                    KeyCode::Char(']') => view.focus_next(),
                    KeyCode::Char('[') => view.focus_prev(),
                    KeyCode::Enter => view.toggle_focused(),
                    KeyCode::Char('/') => view.search_start(),
                    KeyCode::Char('n') => view.search_next(),
                    KeyCode::Char('N') => view.search_prev(),
                    // Open the session switcher (only when enabled, i.e. --latest).
                    KeyCode::Char('s') if view.can_open_picker() => {
                        view.open_switcher(discover::candidates())
                    }
                    _ => {}
                }
            }
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollDown => view.scroll_by(3),
                MouseEventKind::ScrollUp => view.scroll_by(-3),
                // Press begins a potential text selection (also the anchor for a
                // click-to-fold if the mouse doesn't move before release).
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
                            crate::clipboard::copy(&text);
                        }
                    } else {
                        view.clear_selection();
                        if (m.row as usize) < view.content_rows() {
                            view.click_row(m.row);
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
        .unwrap_or(crate::render::DUMP_WIDTH)
}

/// `--dump`: render the whole transcript at a chosen width and either print plain
/// text to stdout (`--dump -`) or write `<stem>.txt` + `<stem>.ansi` (the `.ansi`
/// carries SGR colour). With no `<stem>`, the stem is deduced from the session.
pub fn dump(args: &Args, path: &Path) -> Result<()> {
    let blocks = model::parse_path(path, args)?;
    let width = dump_width(args);
    // Render through the same pipeline as the live TUI (wrap + per-row background
    // fill + diff inset) so the dump matches the on-screen render byte-for-byte.
    // Fold with the same policy as the TUI (default-folded thinking/reads/tools…),
    // so the dump reflects what the viewer actually shows; `--full` expands it all.
    let fold = crate::view::FoldPolicy::from_args(args);
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
        None => deduce_stem(path, width),
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

/// The first `"key":"…"` string value in the transcript JSON, if present.
fn json_field<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":\"");
    let start = content.find(&pat)? + pat.len();
    let rest = &content[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Deduce the default dump stem: `<basename>-<pathhash>-<sessionid>-<width>` where
/// basename/pathhash come from the session's project cwd, sessionid is its first 6
/// chars, and width is the render width. cwd/sessionId are read from the transcript.
fn deduce_stem(path: &Path, width: usize) -> String {
    use std::hash::{Hash, Hasher};
    use std::io::BufRead;
    // cwd/sessionId live in the transcript's first event; read a bounded prefix
    // rather than pull the whole (possibly huge) file into memory.
    let mut content = String::new();
    if let Ok(f) = std::fs::File::open(path) {
        for line in std::io::BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .take(50)
        {
            content.push_str(&line);
            content.push('\n');
            if json_field(&content, "cwd").is_some() && json_field(&content, "sessionId").is_some()
            {
                break;
            }
        }
    }
    let content = content.as_str();
    let cwd = json_field(content, "cwd").unwrap_or("");
    let basename = Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("session");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    let pathhash: String = format!("{:016x}", h.finish())[..6].to_string();
    let sid = json_field(content, "sessionId")
        .map(str::to_string)
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string()
        });
    let sid6: String = sid.chars().take(6).collect();
    format!("{basename}-{pathhash}-{sid6}-{width}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};

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
        let stem = deduce_stem(&file, 140);
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
}

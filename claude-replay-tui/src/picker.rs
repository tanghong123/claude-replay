//! Session picker: a fuzzy-filterable list of transcripts (shown when no id/path
//! is given). Decoupled from the terminal so it's testable headless.

use crate::discover::Candidate;
use crate::tui::theme;
use nucleo::pattern::{CaseMatching, Normalization, Pattern};
use nucleo::{Config, Matcher, Utf32Str};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;
use std::path::PathBuf;
use std::time::SystemTime;

pub struct Picker {
    cands: Vec<Candidate>,
    labels: Vec<String>,
    query: String,
    order: Vec<usize>, // ranked indices into `cands`
    sel: usize,        // index into `order`
    matcher: Matcher,
    now: SystemTime,
    view_h: usize,
    /// Terminal row the list starts at, and the first visible entry — recorded each
    /// `draw` so a mouse click can be mapped back to an entry (`click`).
    list_y: u16,
    win_start: usize,
    /// Extra header text (the live server URL, in the `-f --html` multi-open flow).
    status: Option<String>,
    /// Candidate indices already opened in a browser tab. Empty in the TUI flow, which
    /// is what keeps that flow's rows byte-identical.
    opened: std::collections::HashSet<usize>,
}

fn human_age(now: SystemTime, t: SystemTime) -> String {
    let secs = now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    if secs < 90 {
        format!("{secs}s")
    } else if secs < 5400 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

impl Picker {
    pub fn new(cands: Vec<Candidate>) -> Self {
        let now = SystemTime::now();
        let labels = cands
            .iter()
            .map(|c| format!("{} {}", c.project, c.snippet))
            .collect();
        let mut p = Self {
            order: (0..cands.len()).collect(),
            labels,
            cands,
            query: String::new(),
            sel: 0,
            matcher: Matcher::new(Config::DEFAULT),
            now,
            view_h: 0,
            list_y: 0,
            win_start: 0,
            status: None,
            opened: std::collections::HashSet::new(),
        };
        p.refilter();
        p
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.order
            .get(self.sel)
            .map(|&i| self.cands[i].path.clone())
    }

    /// Map a click at terminal row `row` onto a list entry and select it. `true` when the
    /// click landed on a real entry — the caller then treats it exactly like `Enter`
    /// (select **and** confirm), which is what makes clicking a row open it.
    pub fn click(&mut self, row: u16) -> bool {
        if row < self.list_y {
            return false; // the header
        }
        let off = self.win_start + (row - self.list_y) as usize;
        if off >= self.order.len() || off >= self.win_start + self.view_h.max(1) {
            return false; // past the last match, or below the list (the query row)
        }
        self.sel = off;
        true
    }

    /// Header text for the multi-open flow (the live server's URL). `None` keeps the
    /// terminal viewer's original header.
    pub fn set_status(&mut self, status: String) {
        self.status = Some(status);
    }

    /// Mark the current selection as opened — it gets a `●` in the list, so a user who
    /// stays on the picker can see which sessions they already have a tab for.
    pub fn mark_selected_opened(&mut self) {
        if let Some(&ci) = self.order.get(self.sel) {
            self.opened.insert(ci);
        }
    }
    #[cfg(test)]
    pub fn matches(&self) -> usize {
        self.order.len()
    }

    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.order = (0..self.cands.len()).collect(); // already cwd/recency sorted
        } else {
            let pat = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut buf = Vec::new();
            let mut scored: Vec<(u32, usize)> = self
                .labels
                .iter()
                .enumerate()
                .filter_map(|(i, label)| {
                    let hay = Utf32Str::new(label, &mut buf);
                    pat.score(hay, &mut self.matcher).map(|s| (s, i))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.order = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.sel = 0;
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.refilter();
    }
    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }
    pub fn up(&mut self) {
        self.sel = self.sel.saturating_sub(1);
    }
    pub fn down(&mut self) {
        if self.sel + 1 < self.order.len() {
            self.sel += 1;
        }
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let area = f.area();
        // Clear first: when reopened from the viewer (session switch) the frame
        // still holds transcript content underneath.
        f.render_widget(Clear, area);
        self.view_h = area.height.saturating_sub(2) as usize; // header + query rows
        let rows = self.view_h.max(1);

        // window the list so the selection stays visible
        let start = if self.sel >= rows {
            self.sel - rows + 1
        } else {
            0
        };
        self.list_y = area.y + 1;
        self.win_start = start;
        let mut lines: Vec<Line> = Vec::new();
        for (off, &ci) in self.order.iter().enumerate().skip(start).take(rows) {
            let c = &self.cands[ci];
            let marker = if off == self.sel { "❯ " } else { "  " };
            let age = human_age(self.now, c.mtime);
            let aff = if c.cwd_affinity { "*" } else { " " };
            let agent = c.agent.label();
            // Opened-marker column exists only once something HAS been opened, so the
            // terminal-viewer picker keeps its exact historical row format.
            let dot = match (self.opened.is_empty(), self.opened.contains(&ci)) {
                (true, _) => "",
                (false, true) => "● ",
                (false, false) => "  ",
            };
            let text = format!(
                "{marker}{dot}{aff}{age:>4}  {agent:<6}  {:<16}  {}",
                c.project, c.snippet
            );
            let style = if off == self.sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::styled(text, style));
        }
        f.render_widget(
            Paragraph::new(Line::styled(
                match &self.status {
                    Some(extra) => format!(" {extra} — {} match(es) ", self.order.len()),
                    None => format!(
                        " pick a session — {} match(es), * = this dir ",
                        self.order.len()
                    ),
                },
                theme::status(),
            )),
            Rect::new(area.x, area.y, area.width, 1),
        );
        f.render_widget(
            Paragraph::new(lines),
            Rect::new(area.x, area.y + 1, area.width, rows as u16),
        );
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" / ", theme::dim()),
                Span::raw(self.query.clone()),
                Span::styled(
                    "   (type to filter · ↑/↓ · Enter open · Esc quit)",
                    theme::dim(),
                ),
            ])),
            Rect::new(
                area.x,
                area.y + area.height.saturating_sub(1),
                area.width,
                1,
            ),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn cand(project: &str, snippet: &str, affinity: bool) -> Candidate {
        Candidate {
            path: PathBuf::from(format!("/tmp/{project}.jsonl")),
            mtime: SystemTime::now(),
            project: project.to_string(),
            snippet: snippet.to_string(),
            cwd_affinity: affinity,
            agent: crate::Agent::CLAUDE,
        }
    }

    /// The multi-open flow (`-f --html` with several matches): a click maps to the row
    /// under the cursor, opened rows are marked, and the header carries the server URL —
    /// all rendered through a real `TestBackend` frame, so the geometry `click` relies on
    /// is the geometry `draw` actually produced.
    #[test]
    fn click_maps_to_the_row_under_the_cursor() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut p = Picker::new(vec![
            cand("alpha", "first session", true),
            cand("bravo", "second session", false),
            cand("charlie", "third session", false),
        ]);
        p.set_status("serving 3 sessions at 127.0.0.1:4242".into());
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| p.draw(f)).unwrap();

        // Header row: not a list entry — a click there must NOT confirm.
        assert!(!p.click(0), "header click is not a selection");
        // The list starts at row 1: row 1 = entry 0, row 2 = entry 1, …
        assert!(p.click(2), "click lands on the second entry");
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/tmp/bravo.jsonl")),
            "click selects the row under the cursor"
        );
        // Well past the last match: no entry there.
        assert!(!p.click(9), "click below the matches is not a selection");
        assert_eq!(
            p.selected_path(),
            Some(PathBuf::from("/tmp/bravo.jsonl")),
            "a miss leaves the selection alone"
        );

        // Opening marks the row, and the status replaces the default header.
        p.mark_selected_opened();
        term.draw(|f| p.draw(f)).unwrap();
        let dump = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            dump.contains("serving 3 sessions at 127.0.0.1:4242"),
            "{dump}"
        );
        assert!(dump.contains('●'), "the opened row is marked: {dump}");
    }

    #[test]
    fn fuzzy_filter_narrows_and_selects() {
        // Picker preserves input order for an empty query (affinity/recency
        // ranking is discover::candidates()' job); pass them pre-ranked.
        let p_cands = vec![
            cand("toolbox", "fix the keep script", true),
            cand("kwire", "build the tui", false),
            cand("coach", "training plan", false),
        ];
        let mut p = Picker::new(p_cands);
        assert_eq!(p.matches(), 3);
        assert!(p
            .selected_path()
            .unwrap()
            .to_string_lossy()
            .contains("toolbox"));

        p.push_char('k');
        p.push_char('w');
        assert!(p.matches() >= 1);
        assert!(
            p.selected_path()
                .unwrap()
                .to_string_lossy()
                .contains("kwire"),
            "expected kwire to match 'kw'"
        );

        p.backspace();
        p.backspace();
        assert_eq!(p.matches(), 3);
    }

    #[test]
    fn down_up_move_selection() {
        let mut p = Picker::new(vec![
            cand("a", "x", false),
            cand("b", "y", false),
            cand("c", "z", false),
        ]);
        let first = p.selected_path();
        p.down();
        assert_ne!(p.selected_path(), first);
        p.up();
        assert_eq!(p.selected_path(), first);
    }
}

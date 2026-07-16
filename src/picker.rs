//! Session picker: a fuzzy-filterable list of transcripts (shown when no id/path
//! is given). Decoupled from the terminal so it's testable headless.

use crate::discover::Candidate;
use crate::theme;
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
        };
        p.refilter();
        p
    }

    pub fn selected_path(&self) -> Option<PathBuf> {
        self.order
            .get(self.sel)
            .map(|&i| self.cands[i].path.clone())
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
        let mut lines: Vec<Line> = Vec::new();
        for (off, &ci) in self.order.iter().enumerate().skip(start).take(rows) {
            let c = &self.cands[ci];
            let marker = if off == self.sel { "❯ " } else { "  " };
            let age = human_age(self.now, c.mtime);
            let aff = if c.cwd_affinity { "*" } else { " " };
            let text = format!("{marker}{aff}{age:>4}  {:<16}  {}", c.project, c.snippet);
            let style = if off == self.sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::styled(text, style));
        }
        f.render_widget(
            Paragraph::new(Line::styled(
                format!(
                    " pick a session — {} match(es), * = this dir ",
                    self.order.len()
                ),
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
        }
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

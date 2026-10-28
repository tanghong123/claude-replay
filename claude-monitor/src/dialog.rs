//! A Claude Code dialog read off its pane (#s298, Hong's Way 1 on #s299): a question
//! (`AskUserQuestion`), a permission prompt or a plan's approval, as the client draws it in the
//! terminal, so the page can show it and a tap can answer it.
//!
//! Measured on Claude Code 2.1.312 in a herdr pane (herdr 0.9.5; the screens, rewritten with the
//! fictional world, are `tests/fixtures/dialogs/`): every one is a run of numbered choices at the
//! foot of the screen, under a rule, where the input box would be; a digit picks a choice at once
//! (no Enter), a question with several choices toggles `[ ]` with its digits, Tab goes to the next
//! question, and a written answer is its choice's digit, the text, then Enter. The transcript does
//! not carry a question or a plan while it waits, so the screen is the only place to read them.
//!
//! A layout this does not know is no dialog (`parse` answers `None`): the page then offers
//! nothing to tap. The fingerprint names the dialog's stable parts, so an answer typed against a
//! dialog that changed or went is refused rather than read by whatever is there now.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A held tool call: "Do you want to proceed?".
    Permission,
    /// An `AskUserQuestion`, one question of it at a time.
    Question,
    /// The last tab of a question with several parts: the answers, then Submit or Cancel.
    Review,
    /// A plan waiting for approval (`ExitPlanMode`).
    Plan,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Permission => "permission",
            Kind::Question => "question",
            Kind::Review => "review",
            Kind::Plan => "plan",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub n: u32,
    pub label: String,
    pub description: String,
    /// A choice of a question that takes several: whether it is ticked now.
    pub checked: Option<bool>,
    /// The choice the reader writes in ("Type something", a plan's "Tell Claude what to change").
    pub free_text: bool,
    /// The client's cursor is on it.
    pub cursor: bool,
}

/// One tab of a question with several parts, and whether it has an answer yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub label: String,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub kind: Kind,
    /// A permission's tool ("Bash command"), a question's header, or empty.
    pub title: String,
    /// What is asked: "Do you want to proceed?", the question, the plan's line.
    pub text: String,
    /// A permission's command, or a review's answers.
    pub detail: String,
    /// A permission's description of its call.
    pub note: String,
    pub tabs: Vec<Tab>,
    pub multi: bool,
    pub choices: Vec<Choice>,
    /// A plan's file, as the screen names it (`~/…/plans/<name>.md`).
    pub plan_file: Option<String>,
}

/// The digits a choice may have before its label (`1.`), after the cursor `❯` if it has it.
fn choice_line(line: &str) -> Option<(usize, bool, u32, &str)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let (cursor, rest) = match trimmed.strip_prefix('\u{276f}') {
        Some(rest) => (true, rest.trim_start()),
        None => (false, trimmed),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || digits > 2 {
        return None;
    }
    let n: u32 = rest[..digits].parse().ok()?;
    let label = rest[digits..].strip_prefix(". ")?;
    Some((indent, cursor, n, label.trim_end()))
}

/// A rule across the screen: `─` (the dialog's top, and the line above a question's "Chat about
/// this"), or `╌` (around a permission's command, and a plan's text).
fn is_rule(line: &str, c: char) -> bool {
    let t = line.trim();
    t.chars().count() >= 20 && t.chars().all(|x| x == c)
}

/// The input box's prompt (`❯` and a NO-BREAK SPACE, as `control::herdr_input_line` reads it): where
/// it shows, the input box is drawn and no dialog stands in its place.
const PROMPT: &str = "\u{276f}\u{a0}";

/// The tabs of a question with several parts: `←  ☐ Plots  ☒ Sheet  ✔ Submit  →`.
fn tabs(line: &str) -> Option<Vec<Tab>> {
    let inner = line
        .trim()
        .strip_prefix('\u{2190}')?
        .strip_suffix('\u{2192}')?;
    let tabs: Vec<Tab> = inner
        .split("  ")
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .filter_map(|t| {
            let (mark, label) = t.split_at(t.char_indices().nth(1)?.0);
            let done = match mark {
                "\u{2610}" => false,
                "\u{2612}" => true,
                _ => return None,
            };
            Some(Tab {
                label: label.trim().to_string(),
                done,
            })
        })
        .collect();
    (!tabs.is_empty()).then_some(tabs)
}

/// A question's own header when it is the only one: ` ☐ Ridge`.
fn header(line: &str) -> Option<&str> {
    line.trim()
        .strip_prefix('\u{2610}')
        .or_else(|| line.trim().strip_prefix('\u{2612}'))
        .map(str::trim)
}

fn joined(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What may follow a dialog's choices on the screen: its own foot (how to answer, "ctrl+g to edit
/// in Vim", a plan's file), a rule, or nothing. Anything else after them is not a dialog's foot, so
/// the choices are the agent's text, not a dialog (#s298's review: a reply ending "1. Yes / 2. No"
/// above a dialog with no numbered choices read as a permission).
fn is_foot(line: &str) -> bool {
    let t = line.trim();
    t.is_empty()
        || is_rule(line, '\u{2500}')
        || [
            "Esc to cancel",
            "Enter to select",
            "Enter to confirm",
            "ctrl+g to edit",
        ]
        .iter()
        .any(|f| t.starts_with(f))
        || (t.ends_with(".md") && t.contains("/plans/") && !t.contains(' '))
}

/// The dialog on a pane's screen (plain text, `herdr pane read --source visible`), if it is one of
/// the four this knows.
pub fn parse(screen: &str) -> Option<Dialog> {
    let lines: Vec<&str> = screen.lines().collect();
    // The choices: the LAST run numbered 1, 2, … on the screen, and nothing after it but the
    // dialog's own foot. A numbered list in the agent's reply above is an earlier run, and the
    // input box under one means no dialog stands there.
    let first = lines.iter().enumerate().rev().find_map(|(i, l)| {
        let (_, _, n, _) = choice_line(l)?;
        (n == 1).then_some(i)
    })?;
    if lines[first..].iter().any(|l| l.contains(PROMPT)) {
        return None;
    }
    let (base, ..) = choice_line(lines[first])?;
    let mut choices: Vec<Choice> = Vec::new();
    let mut end = first;
    for (i, line) in lines.iter().enumerate().skip(first) {
        if let Some((indent, cursor, n, label)) = choice_line(line) {
            if n as usize == choices.len() + 1 && indent <= base + 2 {
                let (checked, label) = match label.strip_prefix("[ ] ") {
                    Some(rest) => (Some(false), rest),
                    None => match label.strip_prefix("[\u{2714}] ") {
                        Some(rest) => (Some(true), rest),
                        None => (None, label),
                    },
                };
                choices.push(Choice {
                    n,
                    label: label.trim().to_string(),
                    description: String::new(),
                    checked,
                    free_text: false,
                    cursor,
                });
                end = i;
                continue;
            }
        }
        let t = line.trim();
        if t.is_empty() || is_rule(line, '\u{2500}') {
            continue;
        }
        // A line indented past its choice's number continues it: its description, or its label
        // wrapped. "Next" closes a list of ticks.
        let indent = line.len() - line.trim_start().len();
        if t == "Next" && indent > base {
            end = i;
            continue;
        }
        match choices.last_mut() {
            Some(c) if indent > base => {
                if !c.description.is_empty() {
                    c.description.push(' ');
                }
                c.description.push_str(t);
                end = i;
            }
            _ if indent > base => {}
            _ => break,
        }
    }
    if choices.len() < 2 || !lines[end + 1..].iter().all(|l| is_foot(l)) {
        return None;
    }
    // The dialog's top: the nearest rule above its first choice that runs the screen's width (the
    // dialog's own; a plan's is indented, so nine tenths of the widest) and is not inside a `╌` box,
    // where a preview of a file the call writes is drawn and may hold a rule of its own.
    let rule_len = |l: &str| {
        if is_rule(l, '\u{2500}') {
            l.trim().chars().count()
        } else {
            0
        }
    };
    let widest = lines.iter().map(|l| rule_len(l)).max()?;
    let top = (0..first).rev().find(|&i| {
        rule_len(lines[i]) * 10 >= widest * 9
            && lines[i + 1..first]
                .iter()
                .filter(|l| is_rule(l, '\u{254c}'))
                .count()
                % 2
                == 0
    })?;
    let head: Vec<&str> = lines[top + 1..first].to_vec();
    // A numbered line between the top and the choices: the run is not the dialog's own (a choice's
    // description numbered "1." took it over, or the dialog has no numbered choices).
    if head.iter().any(|l| choice_line(l).is_some()) {
        return None;
    }
    let foot: Vec<&str> = lines[end + 1..].to_vec();
    let asked_at = head.iter().rposition(|l| !l.trim().is_empty())?;
    let asked = head[asked_at].trim();
    let first_at = head.iter().position(|l| !l.trim().is_empty())?;
    let first_line = head[first_at].trim();
    let multi = choices.iter().any(|c| c.checked.is_some());

    let mut d = Dialog {
        kind: Kind::Question,
        title: String::new(),
        text: asked.to_string(),
        detail: String::new(),
        note: String::new(),
        tabs: Vec::new(),
        multi,
        choices,
        plan_file: None,
    };
    // A question's own markers decide first, so one worded "Do you want …?" stays a question.
    if let Some(t) = tabs(first_line) {
        d.tabs = t;
        if let Some(from) = head.iter().position(|l| l.trim() == "Review your answers") {
            d.kind = Kind::Review;
            d.detail = joined(&head[from + 1..asked_at]);
        } else {
            d.text = joined(&head[first_at + 1..]);
        }
    } else if let Some(h) = header(first_line) {
        d.title = h.to_string();
        d.text = joined(&head[first_at + 1..]);
    } else if asked.ends_with("Would you like to proceed?") && asked.contains("plan") {
        d.kind = Kind::Plan;
        d.title = "Claude's plan".into();
        d.plan_file = foot
            .iter()
            .map(|l| l.trim())
            .find(|l| l.ends_with(".md") && l.contains("/plans/"))
            .map(str::to_string);
        d.plan_file.as_ref()?;
        // The plan's own text, where it fits on the screen: between the `╌` rules above the dialog.
        let dashes: Vec<usize> = lines[..top]
            .iter()
            .enumerate()
            .filter(|(_, l)| is_rule(l, '\u{254c}'))
            .map(|(i, _)| i)
            .collect();
        if let [.., a, b] = dashes[..] {
            d.detail = joined(&lines[a + 1..b]);
        }
    } else if asked.starts_with("Do you want") && asked.ends_with('?') {
        // A permission: its tool, a note on what the call does, and its command between the `╌`
        // rules nearest the question (a preview drawn above it has rules of its own).
        d.kind = Kind::Permission;
        d.title = first_line.to_string();
        let dashes: Vec<usize> = head
            .iter()
            .enumerate()
            .filter(|(_, l)| is_rule(l, '\u{254c}'))
            .map(|(i, _)| i)
            .collect();
        let body_end = dashes.first().copied().unwrap_or(asked_at);
        let notes: Vec<&str> = head[..body_end]
            .iter()
            .filter(|l| !l.trim().is_empty())
            .skip(1)
            .filter(|l| !l.trim().starts_with("Tip:"))
            .copied()
            .collect();
        d.note = joined(&notes);
        if let [.., a, b] = dashes[..] {
            d.detail = joined(&head[a + 1..b]);
        }
    } else {
        return None;
    }
    if d.kind == Kind::Question && d.text.is_empty() {
        return None;
    }
    // The written answer's choice: by its label, or by its PLACE once typing has replaced the label
    // with the text — a question's is the one just above "Chat about this", a plan's the one that
    // says it approves with this feedback.
    let chat = d.choices.iter().position(|c| c.label == "Chat about this");
    let kind = d.kind;
    for (i, c) in d.choices.iter_mut().enumerate() {
        c.free_text = match kind {
            Kind::Question => {
                c.label.trim_end_matches('.') == "Type something" || (!multi && chat == Some(i + 1))
            }
            Kind::Plan => {
                c.description.contains("approve with this feedback")
                    || c.label == "Tell Claude what to change"
            }
            _ => false,
        };
    }
    Some(d)
}

impl Choice {
    /// Whether the page may offer it, and an answer name it. A permission offers Yes and No only
    /// (Hong, #s299): "don't ask again" and "switch to auto mode" change settings. A plan offers
    /// its choices but one that bypasses permissions. "Chat about this" leaves the question for the
    /// terminal, and a question with several choices takes no written answer here (not measured).
    fn offered(&self, d: &Dialog) -> bool {
        match d.kind {
            Kind::Permission => {
                self.label == "Yes" || self.label == "No" || self.label.starts_with("No,")
            }
            Kind::Question => self.label != "Chat about this" && !(self.free_text && d.multi),
            Kind::Plan => !self.label.to_ascii_lowercase().contains("bypass"),
            Kind::Review => true,
        }
    }
}

impl Dialog {
    pub fn offered(&self) -> impl Iterator<Item = &Choice> {
        self.choices.iter().filter(|c| c.offered(self))
    }

    /// The dialog's identity: what it asks and offers, never where the cursor is or what is
    /// ticked (an answer in progress changes those), nor a written answer's text.
    pub fn fingerprint(&self) -> String {
        self.fingerprint_with(None)
    }

    /// The identity with a plan's text read from its file, which the reader was shown and the
    /// screen may not hold whole.
    pub fn fingerprint_with(&self, plan: Option<&str>) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        let file = self.plan_file.as_deref().unwrap_or("");
        for part in [
            self.kind.as_str(),
            &self.title,
            &self.text,
            &self.detail,
            &self.note,
            file,
            plan.unwrap_or(""),
        ] {
            h.update(part.as_bytes());
            h.update([0]);
        }
        for t in &self.tabs {
            h.update(t.label.as_bytes());
            h.update([1]);
        }
        for c in self.choices.iter().filter(|c| !c.free_text) {
            h.update(c.n.to_string().as_bytes());
            h.update(c.label.as_bytes());
            h.update([2]);
            h.update(c.description.as_bytes());
            h.update([3]);
        }
        h.finalize()[..8]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// The `/api/dialog` body's `dialog`: the choices the page may offer, and the plan's text when
    /// the caller read it (`plan`).
    pub fn to_json(&self, plan: Option<&str>) -> Value {
        let choices: Vec<Value> = self
            .offered()
            .map(|c| {
                let mut v = json!({"n": c.n, "label": c.label, "description": c.description});
                if let Some(checked) = c.checked {
                    v["checked"] = json!(checked);
                }
                if c.free_text {
                    v["freeText"] = json!(true);
                }
                v
            })
            .collect();
        let mut v = json!({
            "kind": self.kind.as_str(),
            "fingerprint": self.fingerprint_with(plan),
            "title": self.title,
            "text": self.text,
            "detail": self.detail,
            "note": self.note,
            "multi": self.multi,
            "choices": choices,
        });
        if !self.tabs.is_empty() {
            v["tabs"] = json!(self
                .tabs
                .iter()
                .map(|t| json!({"label": t.label, "done": t.done}))
                .collect::<Vec<_>>());
        }
        if let Some(plan) = plan {
            v["plan"] = json!(plan);
        }
        v
    }
}

/// What the reader chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    /// One choice, by its number.
    Choice(u32),
    /// A question with several choices: the ones to tick (the rest are left unticked).
    Choices(Vec<u32>),
    /// A written answer.
    Text(String),
}

/// The longest written answer typed for the reader.
const TEXT_MAX: usize = 2000;

/// The `/api/answer` body: `{"fingerprint", "choice" | "choices" | "text"}`.
pub fn parse_answer(body: &[u8]) -> Result<(String, Answer), String> {
    let v: Value =
        serde_json::from_slice(body).map_err(|_| "the answer is not JSON".to_string())?;
    let fingerprint = v
        .get("fingerprint")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
        .ok_or("the answer names no dialog (fingerprint)")?
        .to_string();
    let answer = if let Some(n) = v.get("choice").and_then(Value::as_u64) {
        Answer::Choice(u32::try_from(n).map_err(|_| "no such choice")?)
    } else if let Some(ns) = v.get("choices").and_then(Value::as_array) {
        let mut out = Vec::new();
        for n in ns {
            let n = n
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("no such choice")?;
            if !out.contains(&n) {
                out.push(n);
            }
        }
        Answer::Choices(out)
    } else if let Some(text) = v.get("text").and_then(Value::as_str) {
        // One line, as the field takes it: a newline would be Enter, a control character a key.
        let text: String = text
            .chars()
            .map(|c| {
                if c == '\n' || c == '\r' || c == '\t' {
                    ' '
                } else {
                    c
                }
            })
            .filter(|c| !c.is_control())
            .collect();
        let text = text.trim().to_string();
        if text.is_empty() {
            return Err("the written answer is empty".into());
        }
        if text.chars().count() > TEXT_MAX {
            return Err(format!(
                "a written answer may be at most {TEXT_MAX} characters"
            ));
        }
        Answer::Text(text)
    } else {
        return Err("the answer names no choice and no text".into());
    };
    Ok((fingerprint, answer))
}

/// One step of typing an answer into the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Type these characters (a choice's digits, or the written answer), with no Enter.
    Type(String),
    /// Press a key by herdr's name for it.
    Key(&'static str),
    /// Read the pane until the cursor is on this choice: the written answer's field is open.
    Cursor(u32),
    /// Read the pane until this choice reads as the written answer (its start: it may wrap) and
    /// no longer as what it read before the text went in.
    Label(u32, String, String),
    /// Read the pane until exactly these choices are ticked.
    Ticks(Vec<u32>),
}

/// How to type an answer to this dialog, or why it cannot be (the choice is not offered). Never
/// an Enter after a digit: a digit picks at once, and an Enter that outlived its dialog would
/// submit whatever is in the input box. The only Enter submits a written answer once the pane
/// shows it in its choice.
pub fn steps(d: &Dialog, answer: &Answer) -> Result<Vec<Step>, String> {
    let offered = |n: u32| d.offered().find(|c| c.n == n);
    match answer {
        Answer::Choice(n) => {
            let c = offered(*n).ok_or_else(|| format!("choice {n} is not offered here"))?;
            if d.multi || c.free_text {
                return Err(format!("choice {n} takes ticks or text, not a tap"));
            }
            Ok(vec![Step::Type(n.to_string())])
        }
        Answer::Choices(ns) => {
            if !d.multi {
                return Err("this question takes one choice".into());
            }
            let mut out = Vec::new();
            for n in ns {
                if offered(*n).is_none() {
                    return Err(format!("choice {n} is not offered here"));
                }
            }
            // A tick on a choice the page does not offer could never be cleared from here, and
            // the ticks would never read as asked.
            if d.choices
                .iter()
                .any(|c| c.checked == Some(true) && !c.offered(d))
            {
                return Err(
                    "a choice the monitor does not offer is ticked: answer it in the session"
                        .into(),
                );
            }
            for c in d.offered() {
                if c.checked.unwrap_or(false) != ns.contains(&c.n) {
                    out.push(Step::Type(c.n.to_string()));
                }
            }
            let mut ticks = ns.clone();
            ticks.sort_unstable();
            out.push(Step::Ticks(ticks));
            out.push(Step::Key("tab"));
            Ok(out)
        }
        Answer::Text(text) => {
            let c = d
                .offered()
                .find(|c| c.free_text)
                .ok_or("this dialog takes no written answer")?;
            // Digits alone could pick a choice rather than be written (not measured): refused.
            if text.chars().all(|ch| ch.is_ascii_digit()) {
                return Err("a written answer of digits alone: answer it in the session".into());
            }
            let start: String = text.chars().take(24).collect();
            Ok(vec![
                Step::Type(c.n.to_string()),
                Step::Cursor(c.n),
                Step::Type(text.clone()),
                Step::Label(c.n, start.trim_end().to_string(), c.label.clone()),
                Step::Key("enter"),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/dialogs")
            .join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn labels(d: &Dialog) -> Vec<(u32, &str)> {
        d.offered().map(|c| (c.n, c.label.as_str())).collect()
    }

    /// A permission: its tool, its note and command, and Yes and No alone of its four choices.
    #[test]
    fn a_permission_prompt_offers_yes_and_no() {
        let d = parse(&fixture("permission-bash.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Permission);
        assert_eq!(d.title, "Bash command");
        assert_eq!(d.text, "Do you want to proceed?");
        assert_eq!(d.note, "Count the survey's lines into counts/today.txt");
        assert_eq!(
            d.detail,
            "mkdir -p counts && wc -l < survey.csv > counts/today.txt"
        );
        assert_eq!(d.choices.len(), 4);
        assert!(
            d.choices[1].description.ends_with("h-ridge"),
            "a wrapped label continues: {:?}",
            d.choices[1]
        );
        assert_eq!(labels(&d), [(1, "Yes"), (4, "No")]);
        assert!(d.choices[0].cursor);
        assert_eq!(
            steps(&d, &Answer::Choice(1)).unwrap(),
            [Step::Type("1".into())]
        );
        assert!(
            steps(&d, &Answer::Choice(2)).is_err(),
            "don't ask again changes settings"
        );
        assert!(steps(&d, &Answer::Choice(3)).is_err(), "nor auto mode");
        assert!(steps(&d, &Answer::Text("no".into())).is_err());
    }

    /// One question: its header, the question, three choices with their descriptions, and a
    /// written answer; "Chat about this" below the rule is not offered.
    #[test]
    fn a_question_offers_its_choices_and_a_written_answer() {
        let d = parse(&fixture("question-single.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Question);
        assert_eq!(d.title, "Ridge");
        assert_eq!(d.text, "Which ridge should the survey start on?");
        assert!(!d.multi);
        assert_eq!(
            labels(&d),
            [
                (1, "North ridge"),
                (2, "South ridge"),
                (3, "East ridge"),
                (4, "Type something.")
            ]
        );
        assert_eq!(
            d.choices[1].description,
            "The petrel burrows, quieter in the morning."
        );
        assert!(d.choices[3].free_text);
        assert_eq!(
            steps(&d, &Answer::Choice(2)).unwrap(),
            [Step::Type("2".into())]
        );
        assert!(steps(&d, &Answer::Choice(5)).is_err(), "chat about this");
        assert!(
            steps(&d, &Answer::Choice(4)).is_err(),
            "the written one takes text"
        );
        assert_eq!(
            steps(&d, &Answer::Text("West ridge, by the cairn".into())).unwrap(),
            [
                Step::Type("4".into()),
                Step::Cursor(4),
                Step::Type("West ridge, by the cairn".into()),
                Step::Label(
                    4,
                    "West ridge, by the cairn".into(),
                    "Type something.".into()
                ),
                Step::Key("enter"),
            ]
        );
    }

    /// Several choices: ticks read as they stand, an answer ticks and unticks only what differs,
    /// then Tab; the tabs say which parts have answers.
    #[test]
    fn a_question_with_several_choices_ticks_only_what_differs() {
        let d = parse(&fixture("question-multi.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Question);
        assert!(d.multi);
        assert_eq!(d.text, "Which plots should be counted today?");
        assert_eq!(
            d.tabs,
            [
                Tab {
                    label: "Plots".into(),
                    done: false
                },
                Tab {
                    label: "Sheet".into(),
                    done: false
                },
            ]
        );
        assert_eq!(labels(&d), [(1, "Plot A"), (2, "Plot B"), (3, "Plot C")]);
        assert_eq!(
            d.choices[0].description, "By the hut.",
            "Next is no description"
        );
        assert_eq!(
            steps(&d, &Answer::Choices(vec![3, 1])).unwrap(),
            [
                Step::Type("1".into()),
                Step::Type("3".into()),
                Step::Ticks(vec![1, 3]),
                Step::Key("tab")
            ]
        );
        assert!(steps(&d, &Answer::Choice(1)).is_err(), "ticks, not a tap");
        assert!(
            steps(&d, &Answer::Choices(vec![4])).is_err(),
            "no written tick"
        );
        let toggled = parse(&fixture("question-multi-toggled.txt")).expect("a dialog");
        assert_eq!(
            toggled.fingerprint(),
            d.fingerprint(),
            "ticking does not change which dialog it is"
        );
        assert_eq!(
            toggled
                .choices
                .iter()
                .map(|c| c.checked)
                .collect::<Vec<_>>(),
            [Some(true), Some(false), Some(true), Some(false), None],
            "and Chat about this below the rule"
        );
        assert_eq!(
            steps(&toggled, &Answer::Choices(vec![2, 3])).unwrap(),
            [
                Step::Type("1".into()),
                Step::Type("2".into()),
                Step::Ticks(vec![2, 3]),
                Step::Key("tab")
            ]
        );
    }

    /// The next part of the question, and the review at the end.
    #[test]
    fn a_question_in_parts_goes_tab_by_tab_to_its_review() {
        let d = parse(&fixture("question-second-tab.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Question);
        assert_eq!(d.text, "Which tally sheet should the counts go on?");
        assert_eq!(
            d.tabs.iter().map(|t| t.done).collect::<Vec<_>>(),
            [true, false]
        );
        assert_eq!(
            labels(&d),
            [(1, "Paper"), (2, "Tablet"), (3, "Type something.")]
        );
        let r = parse(&fixture("question-review.txt")).expect("a dialog");
        assert_eq!(r.kind, Kind::Review);
        assert_eq!(r.text, "Ready to submit your answers?");
        assert!(r.detail.contains("\u{2192} Plot A, Plot C"), "{}", r.detail);
        assert_eq!(labels(&r), [(1, "Submit answers"), (2, "Cancel")]);
        assert_eq!(
            steps(&r, &Answer::Choice(1)).unwrap(),
            [Step::Type("1".into())]
        );
    }

    /// A written answer: once its choice is picked the cursor is on it, and once typed the
    /// choice reads as the text, while the dialog stays the same one.
    #[test]
    fn a_written_answer_shows_in_its_choice() {
        let chosen = parse(&fixture("question-free-chosen.txt")).expect("a dialog");
        assert!(chosen.choices[2].cursor && chosen.choices[2].free_text);
        let typed = parse(&fixture("question-free-typed.txt")).expect("a dialog");
        assert_eq!(typed.choices[2].label, "Petrel watch");
        assert!(typed.choices[2].cursor && typed.choices[2].free_text);
        assert_eq!(typed.fingerprint(), chosen.fingerprint());
    }

    /// A plan: its line, its file, its text where the screen shows it, and its three choices.
    #[test]
    fn a_plan_offers_its_three_choices() {
        let d = parse(&fixture("plan.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Plan);
        assert_eq!(
            d.plan_file.as_deref(),
            Some("~/.claude/plans/tally-the-petrel-nests-quiet-ridge.md")
        );
        assert!(
            d.detail.starts_with("Plan: tally the petrel nests"),
            "{}",
            d.detail
        );
        assert!(d.detail.contains("2. Write the totals"), "{}", d.detail);
        assert_eq!(
            labels(&d),
            [
                (1, "Yes, and use auto mode"),
                (2, "Yes, manually approve edits"),
                (3, "Tell Claude what to change")
            ]
        );
        assert_eq!(
            d.choices[2].description,
            "shift+tab to approve with this feedback"
        );
        assert!(d.choices[2].free_text);
        assert_eq!(
            steps(&d, &Answer::Choice(2)).unwrap(),
            [Step::Type("2".into())]
        );
        assert_eq!(
            steps(&d, &Answer::Text("Count plot C first.".into())).unwrap()[0],
            Step::Type("3".into())
        );
        let typed = parse(&fixture("plan-typed.txt")).expect("a dialog");
        assert_eq!(typed.choices[2].label, "Count plot C first.");
        assert_eq!(typed.fingerprint(), d.fingerprint());
    }

    /// A numbered list and a question in the agent's reply, with the input box below them, is no
    /// dialog; nor is an empty screen or one with a single numbered line.
    #[test]
    fn a_reply_with_a_numbered_list_is_no_dialog() {
        assert_eq!(parse(&fixture("no-dialog.txt")), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse(&format!("{}\n 1. Only one\n", "─".repeat(40))), None);
    }

    /// Different dialogs have different fingerprints.
    #[test]
    fn each_dialog_has_its_own_fingerprint() {
        let prints: Vec<String> = [
            "permission-bash.txt",
            "question-single.txt",
            "question-multi.txt",
            "question-second-tab.txt",
            "question-review.txt",
            "question-free-chosen.txt",
            "plan.txt",
        ]
        .iter()
        .map(|f| parse(&fixture(f)).expect(f).fingerprint())
        .collect();
        for (i, a) in prints.iter().enumerate() {
            assert_eq!(a.len(), 16);
            assert!(!prints[i + 1..].contains(a), "{prints:?}");
        }
    }

    /// The body: a choice, ticks (repeats dropped), or one line of text, always naming the dialog.
    #[test]
    fn an_answer_body_names_its_dialog_and_one_kind_of_answer() {
        let a = |s: &str| parse_answer(s.as_bytes());
        assert_eq!(
            a(r#"{"fingerprint":"ab","choice":2}"#).unwrap(),
            ("ab".into(), Answer::Choice(2))
        );
        assert_eq!(
            a(r#"{"fingerprint":"ab","choices":[3,1,3]}"#).unwrap().1,
            Answer::Choices(vec![3, 1])
        );
        assert_eq!(
            a("{\"fingerprint\":\"ab\",\"text\":\" two\\nlines\\u001b[A \"}")
                .unwrap()
                .1,
            Answer::Text("two lines[A".into()),
            "one line, no control characters"
        );
        assert!(a(r#"{"choice":2}"#).is_err());
        assert!(a(r#"{"fingerprint":"ab"}"#).is_err());
        assert!(a(r#"{"fingerprint":"ab","text":"  "}"#).is_err());
        assert!(a("nope").is_err());
        let long = format!(
            r#"{{"fingerprint":"ab","text":"{}"}}"#,
            "x".repeat(TEXT_MAX + 1)
        );
        assert!(a(&long).is_err());
    }

    /// #s298's review: a numbered list in the agent's reply over a dialog with no numbered choices,
    /// and a choice whose description reads as a numbered line, are no dialog — nothing to tap
    /// that would type a digit into something else.
    #[test]
    fn numbers_that_are_not_the_dialogs_own_choices_are_no_dialog() {
        assert_eq!(parse(&fixture("no-dialog-reply-above-trust.txt")), None);
        assert_eq!(parse(&fixture("question-numbered-description.txt")), None);
        // Anything after the choices but the dialog's own foot.
        let trailing = format!(
            "{}\nmore of the agent's reply\n",
            fixture("question-single.txt")
        );
        assert_eq!(parse(&trailing), None);
    }

    /// #s298's review: a question's header decides before its wording, so one asking "Do you want
    /// …?" is a question offering every choice, and No is No exactly.
    #[test]
    fn a_question_worded_like_a_permission_stays_a_question() {
        let d = parse(&fixture("question-worded-do-you-want.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Question);
        assert_eq!(d.title, "Start");
        assert_eq!(
            labels(&d),
            [(1, "North ridge"), (2, "Not yet"), (3, "Type something.")]
        );
    }

    /// #s298's review: a preview with a rule of its own inside its `╌` box does not move the
    /// dialog's top; the title is the dialog's, and Yes and No are offered.
    #[test]
    fn a_preview_inside_a_permission_keeps_the_dialogs_own_top() {
        let d = parse(&fixture("permission-write-preview.txt")).expect("a dialog");
        assert_eq!(d.kind, Kind::Permission);
        assert_eq!(d.title, "Create file");
        assert_eq!(d.note, "notes.md");
        assert_eq!(d.text, "Do you want to create notes.md?");
        assert!(d.detail.starts_with("# Tern count"), "{}", d.detail);
        assert_eq!(labels(&d), [(1, "Yes"), (3, "No")]);
    }

    /// #s298's review: what a fingerprint covers — a permission's note and command, a choice's
    /// description, a plan's file and its text as read.
    #[test]
    fn the_fingerprint_covers_what_the_reader_was_shown() {
        let d = parse(&fixture("permission-bash.txt")).unwrap();
        let mut other = d.clone();
        other.note = "Remove the counts".into();
        assert_ne!(other.fingerprint(), d.fingerprint());
        let mut desc = parse(&fixture("question-single.txt")).unwrap();
        let before = desc.fingerprint();
        desc.choices[0].description = "Another reason.".into();
        assert_ne!(desc.fingerprint(), before);
        let plan = parse(&fixture("plan.txt")).unwrap();
        assert_ne!(
            plan.fingerprint_with(Some("# one plan")),
            plan.fingerprint_with(Some("# another"))
        );
    }

    /// #s298's review: ticks refuse when a choice the page cannot offer is ticked, a written answer
    /// of digits alone is refused, and a plan never offers to bypass permissions.
    #[test]
    fn answers_the_page_cannot_finish_are_refused_before_a_key() {
        let multi = fixture("question-multi.txt").replacen(
            "[ ] Type something",
            "[\u{2714}] Type something",
            1,
        );
        let d = parse(&multi).expect("a dialog");
        assert!(steps(&d, &Answer::Choices(vec![1])).is_err());
        let one = parse(&fixture("question-single.txt")).unwrap();
        assert!(steps(&one, &Answer::Text("2".into())).is_err());
        assert!(steps(&one, &Answer::Text("2 ridges".into())).is_ok());
        let bypass =
            fixture("plan.txt").replace("Yes, and use auto mode", "Yes, and bypass permissions");
        let p = parse(&bypass).expect("a dialog");
        assert_eq!(
            labels(&p),
            [
                (2, "Yes, manually approve edits"),
                (3, "Tell Claude what to change")
            ]
        );
    }
}

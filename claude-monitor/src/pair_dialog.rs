//! The dialog that shows a page code on this Mac (#s618, `design/pair-from-the-page.md` §4.2).
//!
//! A browser on this machine asks the monitor to pair it; the monitor shows a code here and only
//! here, and the person types it into the page. On macOS that is `osascript`'s `display dialog`,
//! measured on macOS 27.0.1 from a monitor in a terminal and from one launchd runs: it shows with
//! no permission prompt, and a SIGTERM closes it within 15 ms. `withdraw` sends SIGKILL (std's
//! `Child::kill`), which ends the same process and so its window; that signal was not measured
//! apart. The dialog names the page's port, so the person can tell which page asked. The script,
//! code and all, goes in on osascript's STDIN, never in its arguments, which every user of the
//! machine can list; and the code goes nowhere else (no log, no reply, no notification). Anywhere the dialog cannot show (another
//! platform, no one logged in at the console, `AGENT_MONITOR_PAIR_DIALOG=0`), the code waits for
//! `agent-monitor --pair-code` in a terminal on the machine instead.
//!
//! The dialog closes when its code is used, burned or replaced (`withdraw`), when it gives up at the
//! code's expiry, and with the monitor: launchd ends the job's whole process group, and Ctrl-C in a
//! terminal reaches the foreground group, which the dialog is part of.

use claude_replay_html::CodeDisplay;
use std::sync::{Arc, Mutex};

/// The file page codes live in, under the monitor's state dir: apart from the phone codes.
pub const PAGE_CODES_FILE: &str = "page-codes";

/// The command that prints a page code in a terminal, named in the page and the gate's replies.
pub const PAIR_CODE_COMMAND: &str = "agent-monitor --pair-code";

/// The environment switch that keeps the monitor from raising a dialog: `0` turns it off, so every
/// page code waits for `--pair-code`. The browser suite sets it on every monitor it starts.
pub const DIALOG_SWITCH: &str = "AGENT_MONITOR_PAIR_DIALOG";

/// How long a dialog that cannot show takes to fail: osascript with no window server exits at once.
/// One still running after this is taken as showing.
const EARLY_EXIT: std::time::Duration = std::time::Duration::from_millis(600);

/// The words of the dialog's script for `code` (`ABCD-EFGH`), giving up after `secs`, for the page
/// on `port`.
pub fn dialog_script(code: &str, secs: u64, port: u16) -> String {
    // Only the code's own characters reach the script, so nothing in it needs quoting.
    let code: String = code
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    format!(
        "display dialog \"A browser on this Mac asks to pair with Agent Monitor at localhost:{port}, \
         so it can read the monitor and write to your sessions. Type this code in that page, and \
         only there:\" & \
         return & return & \"{code}\" & return & return & \"It works once, for five minutes. If \
         you did not ask, someone else did: click Ignore.\" with title \"Agent Monitor\" buttons \
         {{\"Ignore\", \"Close\"}} default button \"Close\" with icon caution giving up after {secs}\n"
    )
}

/// Whether osascript's answer says the person clicked Ignore.
fn said_ignore(output: &str) -> bool {
    output.trim_start().starts_with("button returned:Ignore,")
}

/// The dialog now showing, if any, and a count that tells a replaced one's watcher to stop.
#[derive(Default)]
struct Slot {
    generation: u64,
    child: Option<std::process::Child>,
    /// Dialogs taken down as they finished starting, their turn gone meanwhile: what the race
    /// case reads, so it fails rather than passes when its withdraw came too late to race.
    #[cfg(test)]
    taken_down_starting: u32,
}

/// Shows page codes in a dialog on this Mac; see the module's doc.
pub struct PairDialog {
    slot: Arc<Mutex<Slot>>,
    /// No dialog: every code waits for `--pair-code`.
    off: bool,
    /// What runs the script: osascript, or a stand-in in a test.
    program: std::path::PathBuf,
    /// How long a dialog that exits at once may take to: [`EARLY_EXIT`], longer in a test.
    early: std::time::Duration,
}

impl PairDialog {
    /// The monitor's display: a dialog on macOS unless [`DIALOG_SWITCH`] is `0`; none elsewhere.
    pub fn from_env() -> Self {
        let off =
            !cfg!(target_os = "macos") || std::env::var_os(DIALOG_SWITCH).is_some_and(|v| v == "0");
        Self {
            slot: Arc::default(),
            off,
            program: "/usr/bin/osascript".into(),
            early: EARLY_EXIT,
        }
    }

    fn spawn(&self, code: &str, secs: u64, port: u16) -> Option<std::process::Child> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new(&self.program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let fed = child.stdin.take().is_some_and(|mut stdin| {
            stdin
                .write_all(dialog_script(code, secs, port).as_bytes())
                .is_ok()
        });
        let started = std::time::Instant::now();
        while fed && started.elapsed() < self.early {
            match child.try_wait() {
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                // Gone already: no window server, or the script refused.
                _ => return None,
            }
        }
        if !fed {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(child)
    }
}

impl CodeDisplay for PairDialog {
    fn show(&self, code: &str, secs: u64, port: u16, ignored: Box<dyn FnOnce() + Send>) -> bool {
        // What showed before goes, and this show takes its turn. A withdraw while the dialog is
        // still starting (its code used or burned in that time, #s618 review) moves the turn on, and
        // the dialog is then closed as soon as it has started rather than left up for a dead code.
        let generation = {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            slot.generation += 1;
            if let Some(mut child) = slot.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            slot.generation
        };
        if self.off {
            return false;
        }
        let Some(mut child) = self.spawn(code, secs, port) else {
            return false;
        };
        {
            let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
            if slot.generation != generation {
                let _ = child.kill();
                let _ = child.wait();
                #[cfg(test)]
                {
                    slot.taken_down_starting += 1;
                }
                return true;
            }
            slot.child = Some(child);
        }
        // The watcher reads the answer once the dialog ends on its own. A withdrawn or replaced
        // dialog was killed under the lock and taken out of the slot, so it never calls `ignored`.
        let slot = self.slot.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_millis(250));
            let mut held = slot.lock().unwrap_or_else(|e| e.into_inner());
            if held.generation != generation {
                return;
            }
            let Some(child) = held.child.as_mut() else {
                return;
            };
            if let Ok(None) = child.try_wait() {
                continue;
            }
            let mut child = held.child.take().expect("the child was just read");
            drop(held);
            let mut output = String::new();
            if let Some(out) = child.stdout.take() {
                use std::io::Read;
                let _ = out.take(4096).read_to_string(&mut output);
            }
            // It has ended (try_wait said so); this only reaps it.
            let _ = child.wait();
            if said_ignore(&output) {
                ignored();
            }
            return;
        });
        true
    }

    fn withdraw(&self) {
        let mut slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.generation += 1;
        if let Some(mut child) = slot.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The page codes' file for a monitor that is starting, with any code a run before left
/// cleared, since nothing shows it any more. None when they could not be cleared: the monitor
/// then offers no page pairing for this run. The state dir is made first: a monitor that has never
/// been paired may start before anything else has written there (#s619).
pub fn page_codes_at_start(state_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::create_dir_all(state_dir).ok()?;
    let path = state_dir.join(PAGE_CODES_FILE);
    claude_replay_html::pairing::clear_page_codes(&path).then_some(path)
}

/// `agent-monitor --pair-code`: print the page code waiting, or a new one, to a terminal only. On a
/// monitor not yet paired, the code typed into its page pairs the monitor too (#s619).
pub fn print_code(state_dir: &std::path::Path, stdout_terminal: bool) -> anyhow::Result<()> {
    anyhow::ensure!(
        stdout_terminal,
        "--pair-code prints its code only to a terminal, never into a pipe or a log."
    );
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join(PAGE_CODES_FILE);
    use claude_replay_html::pairing;
    let code = pairing::terminal_page_code(&path, pairing::now_secs())
        .ok_or_else(|| anyhow::anyhow!("could not write {}", path.display()))?;
    println!(
        "Type this code into the monitor's page on this {}, and only there:\n\n    {}\n\n\
         It works once, for five minutes.",
        pairing::HOST_WORD,
        pairing::display_code(&code)
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script names the code once, gives up at its expiry, and offers Ignore; nothing but the
    /// code's own characters reaches it.
    #[test]
    fn the_dialog_script_carries_the_code_and_gives_up_at_expiry() {
        let script = dialog_script("ABCD-EFGH", 300, 2727);
        assert_eq!(script.matches("ABCD-EFGH").count(), 1, "{script}");
        assert!(script.contains("giving up after 300"), "{script}");
        assert!(script.contains("{\"Ignore\", \"Close\"}"), "{script}");
        let hostile = dialog_script("AB\"CD & (do shell script \"x\")", 5, 2727);
        assert!(!hostile.contains("do shell script"), "{hostile}");
        assert!(hostile.contains("\"ABCDdoshellscriptx\""), "{hostile}");
    }

    #[test]
    fn only_the_ignore_button_burns_the_code() {
        assert!(said_ignore("button returned:Ignore, gave up:false\n"));
        assert!(!said_ignore("button returned:Close, gave up:false\n"));
        assert!(!said_ignore("button returned:, gave up:true\n"));
        assert!(!said_ignore(""));
    }

    /// Switched off, nothing is spawned and the code is left to the terminal.
    #[test]
    fn a_dialog_switched_off_shows_nothing() {
        let display = PairDialog {
            slot: Arc::default(),
            off: true,
            program: "/nonexistent/never-run".into(),
            early: EARLY_EXIT,
        };
        assert!(!display.show("ABCD-EFGH", 300, 2727, Box::new(|| panic!("never ignored"))));
        display.withdraw();
    }

    /// A stand-in for osascript (no test raises a real dialog): it records its arguments, the
    /// arguments `ps` shows every user, and what came in on stdin; then it does what `behaviour`
    /// says. Answers the display and the directory the records land in.
    #[cfg(unix)]
    fn stand_in(tag: &str, behaviour: &str) -> (PairDialog, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cm-dialog-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join("osascript");
        let d = dir.display();
        // Written by a `cp` child (`write_script`): a stand-in this process writes and then runs is
        // refused at random on Linux (ETXTBSY), so CI once saw it never run (#s641).
        crate::write_script(
            &program,
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$#\" \"$@\" > '{d}/args'\n\
                 echo $$ > '{d}/pid'\nps -o args= -p $$ > '{d}/ps'\ncat > '{d}/stdin'\n{behaviour}\n"
            ),
        );
        let display = PairDialog {
            slot: Arc::default(),
            off: false,
            program,
            early: EARLY_EXIT,
        };
        (display, dir)
    }

    #[cfg(unix)]
    fn wait_for(path: &std::path::Path) -> String {
        for _ in 0..100 {
            if let Ok(text) = std::fs::read_to_string(path) {
                if !text.is_empty() {
                    return text;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("{} never written", path.display());
    }

    /// The code reaches the dialog on its stdin alone: not in its arguments, not in what `ps`
    /// shows. Withdrawn, the dialog is killed, and that is never read as Ignore.
    #[cfg(unix)]
    #[test]
    fn the_code_goes_in_on_stdin_and_a_withdrawn_dialog_is_no_ignore() {
        let (display, dir) = stand_in("stdin", "sleep 30");
        let ignored = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ignored.clone();
        let shown = display.show(
            "WXYZ-2345",
            300,
            2727,
            Box::new(move || flag.store(true, std::sync::atomic::Ordering::SeqCst)),
        );
        assert!(shown, "a dialog still up after the early window is showing");
        let stdin = wait_for(&dir.join("stdin"));
        assert!(
            stdin.contains("WXYZ-2345")
                && stdin.contains("giving up after 300")
                && stdin.contains("localhost:2727"),
            "{stdin}"
        );
        let args = wait_for(&dir.join("args"));
        assert_eq!(args.trim(), "0", "no arguments at all: {args}");
        let ps = wait_for(&dir.join("ps"));
        assert!(!ps.contains("WXYZ"), "ps shows no code: {ps}");
        let pid = wait_for(&dir.join("pid"));
        assert!(alive(&pid), "the dialog is up");
        display.withdraw();
        assert!(!alive(&pid), "withdrawn, the dialog is gone");
        std::thread::sleep(std::time::Duration::from_millis(600));
        assert!(!ignored.load(std::sync::atomic::Ordering::SeqCst));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// #s618 review: a code used or burned while its dialog is still starting (the early window)
    /// takes that dialog down as soon as it has started; it is never left up for a dead code.
    #[cfg(unix)]
    #[test]
    fn a_dialog_withdrawn_while_it_starts_does_not_stay_up() {
        let (mut display, dir) = stand_in("starting", "sleep 30");
        // A long early window, so the withdraw lands inside it even on a loaded box.
        display.early = std::time::Duration::from_secs(3);
        let display = Arc::new(display);
        let shower = display.clone();
        let show = std::thread::spawn(move || {
            shower.show("WXYZ-2345", 300, 2727, Box::new(|| panic!("never ignored")))
        });
        let pid = wait_for(&dir.join("pid"));
        display.withdraw();
        assert!(show.join().unwrap(), "it did start");
        assert!(!alive(&pid), "and was taken down at once");
        let slot = display.slot.lock().unwrap();
        assert!(slot.child.is_none());
        assert_eq!(
            slot.taken_down_starting, 1,
            "by the show itself, as it finished starting (a withdraw after that proves nothing)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A paired monitor that starts clears the codes a run before left; where it cannot, it offers
    /// no page pairing for the run.
    #[cfg(unix)]
    #[test]
    fn a_starting_monitor_clears_page_codes_or_offers_no_page_pairing() {
        use claude_replay_html::pairing;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("cm-page-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(PAGE_CODES_FILE);
        assert!(matches!(
            pairing::ask_page_code(&path, pairing::now_secs()),
            pairing::PageAsk::Minted(_)
        ));
        assert_eq!(page_codes_at_start(&dir).as_deref(), Some(path.as_path()));
        assert!(
            pairing::terminal_page_code(&path, pairing::now_secs()).is_some()
                && pairing::redeem_page_code(&path, "ZZZZZZZZ", pairing::now_secs())
                    == pairing::PageRedeem::Missed { burned: false },
            "the old code is gone; a new one stands in its place"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
        let root = std::fs::write(dir.join("probe"), "x").is_ok();
        if !root {
            assert_eq!(
                page_codes_at_start(&dir),
                None,
                "not cleared, no page pairing"
            );
        }
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A dialog that cannot show (it exits at once) is no dialog: the code waits for the terminal.
    /// One the person dismisses with Ignore burns its code; one closed or given up does not.
    #[cfg(unix)]
    #[test]
    fn a_dialog_that_exits_at_once_is_none_and_only_ignore_burns() {
        let (mut display, dir) = stand_in("early", "");
        // Exits before anything else, as osascript does with no window server to draw on.
        let early = dir.join("early");
        crate::write_script(&early, "#!/bin/sh\nexit 1\n");
        display.program = early;
        assert!(!display.show("ABCD-EFGH", 300, 2727, Box::new(|| panic!("never"))));
        let _ = std::fs::remove_dir_all(&dir);
        for (answer, burns) in [
            ("button returned:Ignore, gave up:false", true),
            ("button returned:Close, gave up:false", false),
            ("button returned:, gave up:true", false),
        ] {
            let (display, dir) = stand_in("answer", &format!("sleep 1\necho '{answer}'"));
            let (tx, rx) = std::sync::mpsc::channel();
            assert!(display.show(
                "ABCD-EFGH",
                300,
                2727,
                Box::new(move || tx.send(()).unwrap())
            ));
            let heard = rx.recv_timeout(std::time::Duration::from_secs(4)).is_ok();
            assert_eq!(heard, burns, "{answer}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn pair_code_prints_only_to_a_terminal() {
        let dir = std::env::temp_dir().join(format!("cm-pair-code-{}", std::process::id()));
        let piped = print_code(&dir, false).unwrap_err().to_string();
        assert!(piped.contains("only to a terminal"), "{piped}");
        assert!(
            !dir.join(PAGE_CODES_FILE).exists(),
            "a refusal mints nothing"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

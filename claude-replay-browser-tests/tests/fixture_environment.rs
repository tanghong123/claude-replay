//! The monitor subprocess must not inherit real configuration or store overrides.
mod harness;
use std::collections::BTreeMap;
use std::process::Command;

#[test]
fn monitor_fixture_overrides_inherited_configuration_and_store_paths() {
    let _serial = harness::serial();
    let root = harness::base("fixture-environment");
    let stores = harness::Stores::new(&root);
    for fixture in [Some(&stores), None] {
        let mut cmd = Command::new("synthetic-monitor");
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "AGENT_MONITOR_STATE",
            "CLAUDE_MONITOR_STATE",
            "AGENT_MONITOR_CACHE",
            "CLAUDE_MONITOR_CACHE",
            "CODEX_HOME",
            "CODEX_SESSIONS_DIR",
            "ANTIGRAVITY_CLI_HOME",
            "QODERWORK_DB",
            "QODER_TASKS_ROOT",
        ] {
            cmd.env(key, "/synthetic/inherited");
        }
        harness::configure_monitor_environment(&mut cmd, &root, 3195, fixture);
        let values: BTreeMap<_, _> = cmd
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|v| {
                    (
                        key.to_string_lossy().into_owned(),
                        std::path::PathBuf::from(v),
                    )
                })
            })
            .collect();
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
            "XDG_CACHE_HOME",
            "AGENT_MONITOR_STATE",
            "CLAUDE_MONITOR_STATE",
            "AGENT_MONITOR_CACHE",
            "CLAUDE_MONITOR_CACHE",
            "CODEX_HOME",
            "CODEX_SESSIONS_DIR",
            "ANTIGRAVITY_CLI_HOME",
            "QODERWORK_DB",
            "QODER_TASKS_ROOT",
        ] {
            assert!(
                values.get(key).is_some_and(|p| p.starts_with(&root)),
                "{key} escaped its fixture: {values:?}"
            );
        }
        assert_eq!(values["AGENT_MONITOR_STATE"], root.join("state-3195"));
        assert_eq!(
            values["AGENT_MONITOR_STATE"],
            values["CLAUDE_MONITOR_STATE"]
        );
        assert_eq!(
            values["AGENT_MONITOR_CACHE"],
            values["CLAUDE_MONITOR_CACHE"]
        );
        assert_eq!(
            values["CODEX_SESSIONS_DIR"],
            stores.root.join("codex/sessions")
        );
    }
}

/// #s142: a case that passes removes the scratch roots it made, and a failed one keeps them for
/// diagnosis. The roots used to stay until the release's sweep: a day's suites left 741 of them.
#[test]
fn a_passing_case_removes_its_scratch_and_a_failing_case_keeps_it() {
    if std::env::var_os("CR_KEEP_SCRATCH").is_some() {
        return;
    }
    let passed = std::thread::spawn(|| {
        let _serial = harness::serial();
        harness::base("scratch-passes")
    })
    .join()
    .unwrap();
    assert!(
        !passed.exists(),
        "a passing case leaves no root: {}",
        passed.display()
    );
    let made = std::sync::Arc::new(std::sync::Mutex::new(None));
    let seen = made.clone();
    let outcome = std::thread::spawn(move || {
        let _serial = harness::serial();
        *seen.lock().unwrap() = Some(harness::base("scratch-fails"));
        panic!("the case fails");
    })
    .join();
    assert!(outcome.is_err());
    let failed = made.lock().unwrap().take().unwrap();
    assert!(
        failed.exists(),
        "a failed case keeps its root: {}",
        failed.display()
    );
    let _ = std::fs::remove_dir_all(&failed);
}

/// #s143: a Chrome that starts and never prints its DevTools address no longer stops the run.
/// headless_chrome waits for that line with a blocking read, and a suite stood still on one for
/// an hour; the launch is now given up on time, and that launch's own processes are killed.
#[test]
fn a_mute_chrome_launch_is_given_up_and_killed() {
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};
    let _serial = harness::serial();
    let root = harness::base("mute-chrome");
    let draft = root.join("mute-chrome.draft");
    std::fs::write(&draft, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    std::fs::set_permissions(&draft, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Copied into place by a child, so the script is never open for writing in this process:
    // on Linux a fork while it is would make its exec fail with ETXTBSY.
    let fake = root.join("mute-chrome");
    assert!(Command::new("cp")
        .arg(&draft)
        .arg(&fake)
        .status()
        .unwrap()
        .success());
    let t0 = Instant::now();
    let launched = harness::launch_within(Some(fake.clone()), &[], Duration::from_secs(2));
    let waited = t0.elapsed();
    let err = launched
        .err()
        .expect("a Chrome that says nothing never launches");
    assert!(
        err.contains("no DevTools address") && waited < Duration::from_secs(10),
        "the launch is given up on time: {err} after {waited:?}"
    );
    assert!(
        !err.contains("killed its 0 "),
        "…and its process is killed: {err}"
    );
    let ps = Command::new("ps")
        .args(["-Awwo", "command="])
        .output()
        .unwrap();
    let fake = fake.display().to_string();
    let left: Vec<String> = String::from_utf8_lossy(&ps.stdout)
        .lines()
        .filter(|line| line.contains(&fake))
        .map(str::to_string)
        .collect();
    assert!(left.is_empty(), "nothing of that launch is left: {left:?}");
}

/// #s614: a case's browser ends on its own and takes its singleton folder with it. Chrome links
/// its profile's `SingletonSocket` into a folder of the system temp folder and removes that folder
/// only when it exits on its own; headless_chrome's own drop SIGKILLs it right after
/// `Browser.close`, and every launch left one (75,886 on hong-devserver). The case ends its browser
/// as a case's drop does: the process exits within [`harness::GENTLE_END`], unkilled, and on a Mac
/// the folder is gone. On Linux Chrome keeps the folder even when it exits on its own (measured on
/// CI's Ubuntu runner), in `TMPDIR`, the workspace's scratch whose age rule clears it, which the case
/// checks. Chrome for Testing 159 keeps it on a Mac as well, and the guard removes it once the
/// process has gone (#s710); either way no folder is left there.
#[test]
#[ignore]
fn a_dropped_browser_takes_its_singleton_folder_with_it() {
    let _serial = harness::serial();
    let (browser, tab) = harness::chrome_tab();
    let folder = browser
        .singleton()
        .map(std::path::Path::to_path_buf)
        .expect("Chrome links its profile's SingletonSocket into a folder of a temp folder");
    assert!(
        folder.exists(),
        "the folder is there while the browser runs: {}",
        folder.display()
    );
    drop(tab);
    let ended = browser.end();
    assert!(
        ended.exited && ended.took < harness::GENTLE_END,
        "the browser exits on its own after the SIGTERM, unkilled: {ended:?}"
    );
    match &ended.folder_left {
        None => {}
        Some(left) if cfg!(target_os = "macos") => panic!(
            "the browser took its singleton folder with it: {} is still there",
            left.display()
        ),
        Some(left) => assert!(
            left.starts_with(std::env::temp_dir()),
            "a folder Chrome keeps here is in the workspace's scratch: {} is outside {}",
            left.display(),
            std::env::temp_dir().display()
        ),
    }
}

/// #s293: a case that ends takes every process left of the browsers its runner launched with it —
/// a helper whose browser was killed but which never exited — and leaves a sibling run's alone. On
/// a drained, throttled Mac about 200 such helpers piled up under one scenarios binary.
#[test]
fn a_case_that_ends_takes_its_runners_leftover_browser_helpers_with_it() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::thread::spawn(|| {
        let _serial = harness::serial();
        harness::base("helper-leftovers")
    })
    .join()
    .unwrap();
    let _ = std::fs::create_dir_all(&root);
    let draft = root.join("helper.draft");
    std::fs::write(&draft, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
    std::fs::set_permissions(&draft, std::fs::Permissions::from_mode(0o755)).unwrap();
    // Copied into place by a child, so the script is never open for writing in this process.
    let helper = root.join("helper");
    assert!(Command::new("cp")
        .arg(&draft)
        .arg(&helper)
        .status()
        .unwrap()
        .success());
    let profile = |owner: u32| {
        format!(
            "--user-data-dir={}",
            harness::browser_profiles_root()
                .join(format!("cr-browser-chrome-{owner}-999"))
                .display()
        )
    };
    let mut ours = Command::new(&helper)
        .arg(profile(std::process::id()))
        .spawn()
        .unwrap();
    // Another run's: the parent of this process (cargo, or the shell) is alive.
    let mut theirs = Command::new(&helper)
        .arg(profile(std::os::unix::process::parent_id()))
        .spawn()
        .unwrap();
    std::thread::spawn(|| drop(harness::serial()))
        .join()
        .unwrap();
    let gone = (0..50).any(|_| {
        if ours.try_wait().unwrap().is_some() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        false
    });
    let survived = theirs.try_wait().unwrap().is_none();
    let _ = ours.kill();
    let _ = theirs.kill();
    let _ = ours.wait();
    let _ = theirs.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert!(gone, "this runner's leftover helper goes when a case ends");
    assert!(survived, "…and another run's is left alone");
}

/// #s495: a case's monitor serves on the case's port only while nothing else holds it, so an
/// address a case writes itself must come from the handle (`Monitor::url`, `remote_url`, `port`),
/// never from the number it asked for: one literal left in an address hands that case to whatever
/// holds the port, and it fails as a page that never came. The scan reads every test file for a
/// loopback or `phone.test` address with a port number in it, or a page asked for its literal
/// port. Scratch names (`state-2933`) are the case's own and stay.
#[test]
fn no_case_writes_a_monitor_port_into_an_address() {
    let tests = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut files: Vec<_> = std::fs::read_dir(&tests)
        .unwrap()
        .chain(std::fs::read_dir(tests.join("harness")).unwrap())
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "rs"))
        .collect();
    files.sort();
    let mut found = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for needle in ["127.0.0.1:", "phone.test:", "location.port === '"] {
                for (at, _) in line.match_indices(needle) {
                    let digits: String = line[at + needle.len()..]
                        .chars()
                        .take_while(char::is_ascii_digit)
                        .collect();
                    // `:0` asks the kernel; `:9` is the discard port a case names as no service.
                    if digits.len() >= 2 {
                        found.push(format!(
                            "{}:{}: {needle}{digits}",
                            file.file_name().unwrap().to_string_lossy(),
                            n + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "an address names a port the case asked for; read it from the monitor's handle:\n{}",
        found.join("\n")
    );
}

/// #s495: where there is no `lsof` (a Linux runner may lack it), a held port's holder is read off
/// `ss -Hltnp`, in the same `command (pid N)` form the spawn's panic names.
#[test]
fn a_held_ports_holder_reads_the_same_from_ss() {
    let line = "LISTEN 0      128        *:2728      *:*    users:((\"ssh\",pid=8913,fd=4),(\"sshd\",pid=77,fd=5))";
    assert_eq!(
        harness::ss_holders(line),
        vec!["ssh (pid 8913)".to_string(), "sshd (pid 77)".to_string()]
    );
    assert!(harness::ss_holders("LISTEN 0 128 *:2728 *:*").is_empty());
}

/// #s495: the reaper of a monitor a killed run stranded reads a WHOLE `--port` argument, so a
/// case's port (3276) never reaches a monitor another case was moved to (32761).
#[test]
fn the_stranded_monitor_reaper_matches_the_whole_port() {
    let bin = "/w/target/release/agent-monitor";
    let on = |args: &str| harness::names_port(&format!("{bin} {args}"), 3276);
    assert!(on("--port 3276 --no-open"));
    assert!(on("--no-open --port 3276"));
    assert!(!on("--port 32761 --no-open"));
    assert!(!on("--port 13276"));
    assert!(!on("--no-open"));
}

/// #s690: the reaper takes only a monitor whose test process is gone. A monitor another run
/// started (the local suite's other leg, whose case asked for the same port) has a live parent and
/// is left alone; killing it lost that leg's page its monitor as it opened.
#[test]
fn the_stranded_monitor_reaper_leaves_a_live_runs_monitor_alone() {
    let bin = "/w/target/release/agent-monitor";
    let ps = [
        "  501     1 {bin} --port 2820 --no-open",
        "  600     1",
        "  640   600 /w/target/debug/deps/browser_follow-0a1b --ignored --skip a_phone",
        "  777   640 {bin} --port 2820 --pair --no-open",
        "  888   999 {bin} --port 2820",
        "  901     1 {bin} --port 2821",
        "  902     1 {bin} --port 28201",
        "  903     1 /opt/homebrew/bin/agent-monitor --port 2820",
        "  904     1 {bin}-fleet --port 2820",
        "  905   600 {bin} --port 2820",
    ]
    .join("\n")
    .replace("{bin}", bin);
    assert_eq!(
        harness::stranded_monitors(&ps, bin, 2820),
        vec![501, 888],
        "re-parented to launchd (501) or a parent gone from the listing (888); never the live \
         run's (777), nor one whose parent's row shows no command (905), another port's (901, \
         902), one built elsewhere (903) or another program beside it (904)"
    );
    assert!(harness::stranded_monitors("", bin, 2820).is_empty());
}

/// #s525: a read of `~/Downloads` that never answers (a macOS privacy prompt nobody was there to
/// answer held one in the kernel for two hours) is given up after its wait, and the guard checks
/// nothing rather than holding the case.
#[test]
fn a_downloads_read_that_never_answers_is_given_up_after_its_wait() {
    let (hold, held) = std::sync::mpsc::channel::<()>();
    let wait = std::time::Duration::from_millis(300);
    let start = std::time::Instant::now();
    let names = harness::list_names_with(
        std::path::Path::new("/nowhere/Downloads"),
        wait,
        move |_| {
            let _ = held.recv();
            Ok(Default::default())
        },
    );
    let took = start.elapsed();
    drop(hold);
    assert!(
        names.is_none(),
        "a read that never answered checks nothing: {names:?}"
    );
    assert!(
        took >= wait && took < wait + std::time::Duration::from_secs(10),
        "given up after its wait, not before it or long after: {took:?}"
    );
}

/// #s525: where the folder answers, the guard is what it was (#s254): a file the developer
/// downloads meanwhile passes, and a file named like the case's fails the case.
#[test]
fn the_downloads_guard_still_fails_a_save_where_the_folder_answers() {
    let dir = std::env::temp_dir().join(format!("cr-home-downloads-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("old.txt"), b"x").unwrap();
    let guard = harness::HomeDownloads::of(Some(dir.clone()), harness::DOWNLOADS_READ_WAIT);
    std::fs::write(dir.join("unrelated.pdf"), b"x").unwrap();
    guard.assert_gained_nothing_like("tour");
    std::fs::write(dir.join("tour.mp4"), b"x").unwrap();
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        guard.assert_gained_nothing_like("tour")
    }));
    std::fs::remove_dir_all(&dir).unwrap();
    let panic = caught.expect_err("a save into the folder fails the case");
    let text = panic.downcast_ref::<String>().cloned().unwrap_or_default();
    assert!(
        text.contains("tour.mp4"),
        "the failure names the file: {text}"
    );
}

/// #s658: the session filter the harness seeds (#202) still holds for the page a case opens after
/// the app's page saved its choices. Pairing used to write the seed under the app's page it had
/// just loaded, which held the default filter and writes all its choices back whenever it saves
/// one. So a save that landed after the seed (the one as the index arrives and the list picks a
/// session, on a loaded machine) put the default back, and the next page hid the case's idle
/// fixture: "No sessions in this filter". Here the save comes at once, on purpose: Expand all.
#[test]
#[ignore = "needs a local Chrome and a built agent-monitor"]
fn the_filter_seed_holds_after_the_page_pairing_leaves_saves() {
    use std::time::Duration;
    let _serial = harness::serial();
    let base = harness::base("filter-seed-save");
    let stores = harness::Stores::new(&base);
    let sid = "cccccccc-0000-4000-8000-000000000658";
    stores.claude_session(sid, &harness::long_session(2, harness::Shape::default()));
    let monitor = harness::Monitor::spawn(harness::Kind::V1, 3277, &base, Some(&stores), true);
    let (_browser, tab) = harness::chrome_tab();
    monitor.pair(&tab);
    harness::until(
        &tab,
        "!!document.getElementById('expandBtn')",
        "the app's list, with its Expand all",
        Duration::from_secs(30),
        "document.body.innerText.slice(0, 200)",
    );
    harness::eval(&tab, "document.getElementById('expandBtn').click(); 'ok'");
    let saved = harness::eval(
        &tab,
        "String(localStorage.getItem('am-prod-session-filter'))",
    );
    assert!(
        saved.as_str().is_some_and(|saved| saved.contains("idle")),
        "the save writes the seeded filter back: {saved}"
    );
    monitor.open(&tab, &format!("?session={sid}"));
    harness::until(
        &tab,
        "!!document.querySelector('#sessionList .tree-row.session')",
        "the idle fixture listed in the session list",
        Duration::from_secs(30),
        "JSON.stringify({ filter: localStorage.getItem('am-prod-session-filter'), list: (document.getElementById('sessionList') || {}).innerText })",
    );
}

/// #s658: the origin the filter seed is written in is a URL's scheme, host and port alone.
#[test]
fn a_urls_origin_drops_its_path_query_and_fragment() {
    for (url, origin) in [
        (
            "http://host.example:8080/?ui=app",
            "http://host.example:8080",
        ),
        (
            "http://host.example:8080?session=x",
            "http://host.example:8080",
        ),
        (
            "http://other.example:9090#turn-4",
            "http://other.example:9090",
        ),
        ("http://host.example:8080", "http://host.example:8080"),
    ] {
        assert_eq!(harness::origin_of(url), origin, "the origin of {url}");
    }
}

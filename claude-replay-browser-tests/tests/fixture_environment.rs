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
            std::env::temp_dir()
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

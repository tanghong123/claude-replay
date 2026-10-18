//! Invalid receipt paths must be rejected before changing the caller's filesystem.

#![cfg(unix)]

use claude_monitor_codex::{runtime::NativeRuntime, runtime::RuntimeConfig, Config, Gateway};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn relative_state_paths_leave_cwd_and_parent_untouched() {
    if let Ok(entry) = std::env::var("NATIVE_PATH_TEST_ENTRY") {
        let cwd = std::env::current_dir().unwrap();
        let state_path = PathBuf::from(std::env::var_os("NATIVE_PATH_TEST_INPUT").unwrap());
        let config = Config {
            endpoint: "ws://127.0.0.1:1".into(),
            allowed_roots: vec![cwd.clone()],
            state_path,
        };
        let error = if entry == "runtime" {
            match NativeRuntime::new(RuntimeConfig {
                gateway: config,
                binary: cwd.join("missing-codex"),
                home: cwd.join("home"),
                log_path: cwd.join("server.log"),
            }) {
                Ok(runtime) => {
                    // A regression may reach credential setup, but can never launch Codex.
                    assert!(!cwd.join("missing-codex").exists());
                    let _ = runtime.activate(true);
                    None
                }
                Err(error) => Some(error),
            }
        } else {
            Gateway::start(config).err()
        };
        let mode = std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o7777;
        println!(
            "entry={entry}, mode={mode:o}, token_in_cwd={}, token_in_parent={}",
            cwd.join("codex-server.token").exists(),
            cwd.parent().unwrap().join("codex-server.token").exists()
        );
        let error = error.expect("relative state paths must be rejected");
        assert!(error.to_string().contains("must be absolute"), "{error:#}");
        return;
    }

    let root = std::env::temp_dir().join(format!("native-relative-paths-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut failures = Vec::new();
    for entry in ["runtime", "gateway"] {
        for (index, input) in [
            "receipts.sqlite3",
            "../x.sqlite3",
            "nested/receipts.sqlite3",
        ]
        .iter()
        .enumerate()
        {
            let parent = root.join(format!("{entry}-{index}"));
            let cwd = parent.join("checkout");
            std::fs::create_dir_all(&cwd).unwrap();
            for path in [&parent, &cwd] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o2775)).unwrap();
            }
            // The kernel may clear setgid when the directory's group is not ours.
            let parent_mode = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o7777;
            let cwd_mode = std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o7777;
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "relative_state_paths_leave_cwd_and_parent_untouched",
                    "--nocapture",
                ])
                .current_dir(&cwd)
                .env("NATIVE_PATH_TEST_ENTRY", entry)
                .env("NATIVE_PATH_TEST_INPUT", input)
                .output()
                .unwrap();
            let untouched = std::fs::metadata(&parent).unwrap().permissions().mode() & 0o7777
                == parent_mode
                && std::fs::metadata(&cwd).unwrap().permissions().mode() & 0o7777 == cwd_mode
                && std::fs::read_dir(&parent).unwrap().count() == 1
                && std::fs::read_dir(&cwd).unwrap().count() == 0;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let executed =
                stdout.contains(&format!("entry={entry},")) && stdout.contains("1 passed;");
            if !output.status.success() || !executed || !untouched {
                failures.push(format!(
                    "{entry} {input}: executed={executed}, untouched={untouched}\n{}\n{}",
                    stdout,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
    }
    std::fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

//! Gate the native frontend unit tests through ordinary Cargo and CI checks.

#[test]
fn native_frontend_unit_tests() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    let mut scripts: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("codex_") && name.ends_with(".test.mjs"))
        })
        .collect();
    scripts.extend(
        std::fs::read_dir(root.parent().unwrap().parent().unwrap().join("scripts"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("codex-") && name.ends_with(".test.mjs"))
            }),
    );
    scripts.sort();
    assert!(
        !scripts.is_empty(),
        "native frontend test discovery must not be empty"
    );
    let output = match std::process::Command::new("node")
        .arg("--test")
        .args(&scripts)
        .output()
    {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skip: node is unavailable; CI requires and runs the same frontend suite");
            return;
        }
        Err(error) => panic!("start native frontend Node tests: {error}"),
    };
    assert!(
        output.status.success(),
        "native frontend tests failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

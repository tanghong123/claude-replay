//! `--dump-html --mask-secrets` (#365): a dump is what gets sent on, so the secrets a transcript
//! carries are `*` in the file — and only when asked; without the flag the page is as it was.
use std::process::Command;

#[test]
fn a_dump_masks_the_secrets_it_carries_only_when_asked() {
    let dir = std::env::temp_dir().join(format!("cr-dump-mask-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Assembled here, so no line of this source is a whole key (CI runs gitleaks over it).
    let key = format!("ghp_{}", "Q".repeat(36));
    let path = dir.join("0f0e0d0c-0000-4000-8000-000000000365.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n",
            serde_json::json!({
                "type": "user", "cwd": "/w", "sessionId": "0f0e0d0c-0000-4000-8000-000000000365",
                "version": "2.1.300", "timestamp": "2026-10-02T00:00:00Z",
                "message": {"role": "user", "content": [{"type": "text", "text": format!("push with {key} please")}]}
            })
        ),
    )
    .unwrap();
    let dump = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_agent-replay"))
            .arg(&path)
            .args(["--dump-html", "-"])
            .args(extra)
            .env("CLAUDE_REPLAY_CACHE", dir.join("cache"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        (
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (plain, _) = dump(&[]);
    assert!(
        plain.contains(&key),
        "without the flag the page is as it was"
    );
    let (masked, err) = dump(&["--mask-secrets"]);
    assert!(!masked.contains(&key), "with it, the key is gone");
    assert!(
        masked.contains(&"*".repeat(40)) && masked.contains("push with "),
        "…replaced by *, the words kept"
    );
    // The prompt's words appear more than once in the stream (its head and its body): each is masked.
    assert!(
        err.contains("masked ") && err.contains("secret(s)"),
        "and it says so: {err}"
    );
    assert_eq!(
        masked.len(),
        plain.len(),
        "the same length: only bytes inside strings change"
    );
}

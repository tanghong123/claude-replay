//! #200: `scripts/release-check.sh`, the release's version guard, against a hermetic git
//! repository under the workspace scratch — a Cargo.toml whose first `version` line is
//! `version.workspace = true` (the naive match that once sent a release backwards), tags, and
//! commits shaped like the histories the guard has to tell apart.

use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/release-check.sh")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cargo_toml(version: &str) -> String {
    format!(
        "[package]\nname = \"x\"\nversion.workspace = true\n\n[workspace]\nmembers = [\".\"]\n\n[workspace.package]\nversion = \"{version}\"\nedition = \"2021\"\n"
    )
}

/// A repository with `version` in Cargo.toml, committed, and the given tags on that commit.
fn repo(name: &str, version: &str, tags: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("release-check-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("Cargo.toml"), cargo_toml(version)).unwrap();
    git(&dir, &["add", "Cargo.toml"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    for t in tags {
        git(&dir, &["tag", "-a", t, "-m", t]);
    }
    dir
}

/// Run the guard in `dir`: (exit code, stdout).
fn check(dir: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new("sh")
        .arg(script())
        .args(args)
        .current_dir(dir)
        .output()
        .expect("sh scripts/release-check.sh");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

fn commit(dir: &Path, version: &str, subject: &str) {
    std::fs::write(dir.join("Cargo.toml"), cargo_toml(version)).unwrap();
    git(dir, &["commit", "-q", "-am", subject]);
}

#[test]
fn the_guard_reads_the_workspace_package_version_not_the_first_version_line() {
    let dir = repo("parse", "1.2.0", &["v1.2.0"]);
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains("workspace version 1.2.0"),
        "the [workspace.package] line, not `version.workspace = true`: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_next_version_must_be_above_every_tag_and_untagged() {
    let dir = repo("next", "1.2.0", &["v1.1.0", "v1.2.0", "v1.9.0"]);
    let (code, out) = check(&dir, &["--next", "1.10.0"]);
    assert_eq!(
        code, 0,
        "1.10.0 is above 1.9.0 numerically, not lexically: {out}"
    );
    let (code, out) = check(&dir, &["--next", "1.9.0"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("already a tag"),
        "{out}"
    );
    let (code, out) = check(&dir, &["--next", "1.5.0"]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("not above the highest tag v1.9.0"),
        "a release only moves forward: {out}"
    );
    let (code, out) = check(&dir, &["--next", "1.10"]);
    assert_eq!(code, 2, "MAJOR.MINOR.PATCH only: {out}");
    let (code, out) = check(&dir, &["--next", "1.5.0", "--allow-backwards"]);
    assert_eq!(code, 0, "the explicit override: {out}");
    assert!(out.contains("OVERRIDE"), "…and it says so: {out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_commit_below_the_highest_tag_is_refused() {
    // The 2026-09-12 shape: main at 1.259.0 (tagged), a release commit that moved it to 1.258.0.
    let dir = repo("backwards", "1.259.0", &["v1.258.0", "v1.259.0"]);
    commit(
        &dir,
        "1.258.0",
        "release: v1.258.0 — a release from a stale tree",
    );
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("REFUSED") && out.contains("below the highest tag v1.259.0"),
        "{out}"
    );
    let (code, out) = check(&dir, &["--allow-backwards"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("OVERRIDE"), "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_release_commit_must_name_the_workspace_version_and_own_its_tag() {
    let dir = repo("release-commit", "1.2.0", &["v1.2.0"]);
    // A good release commit: bumps to 1.3.0 and says so; no tag yet (the tag push follows).
    commit(&dir, "1.3.0", "release: v1.3.0 — the good shape");
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 0, "{out}");
    // …and once its tag exists at HEAD it is still good.
    git(&dir, &["tag", "-a", "v1.3.0", "-m", "v1.3.0"]);
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 0, "{out}");
    // A release commit that names a version other than the workspace's.
    commit(&dir, "1.4.0", "release: v1.5.0 — the subject lies");
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("names v1.5.0 but the workspace version is 1.4.0"),
        "{out}"
    );
    // A release commit claiming a version whose tag sits on another commit.
    commit(
        &dir,
        "1.3.0",
        "release: v1.3.0 — released elsewhere already",
    );
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 2, "{out}");
    assert!(
        out.contains("v1.3.0 is already a tag at"),
        "the reuse is refused before `git tag` has to: {out}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_plain_commit_after_a_release_is_fine() {
    let dir = repo("plain", "1.2.0", &["v1.2.0"]);
    std::fs::write(dir.join("README.md"), "a change that is not a release\n").unwrap();
    git(&dir, &["add", "README.md"]);
    git(
        &dir,
        &["commit", "-q", "-m", "docs: nothing to do with a release"],
    );
    let (code, out) = check(&dir, &[]);
    assert_eq!(code, 0, "{out}");
    let (code, out) = check(&dir, &["--next", "1.2.1"]);
    assert_eq!(code, 0, "{out}");
    let _ = std::fs::remove_dir_all(&dir);
}

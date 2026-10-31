//! #s15: the Release workflow's Homebrew step retires the v1.101.0 rename's two compat layers
//! (Hong, 2026-10-26: "remove them on 11/01"). The formulas it writes install no `claude-*`
//! command, and the tap keeps no `formula_renames.json`: the step removes it, never writes it,
//! and stages nothing by its name once it is gone (`git add` fails on a pathspec that matches
//! nothing, which would stop the tap step of every release after this one).

use std::path::Path;

fn workflow() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(".github/workflows/release.yml"),
    )
    .expect("release.yml")
}

#[test]
fn the_tap_installs_no_claude_command_and_keeps_no_rename_map() {
    let wf = workflow();
    let symlinks: Vec<&str> = wf
        .lines()
        .filter(|l| l.contains("install_symlink"))
        .collect();
    assert!(
        symlinks.is_empty(),
        "a formula still installs a symlink: {symlinks:?}"
    );
    for old in ["claude-replay", "claude-monitor", "claude-monitor-fleet"] {
        assert!(
            !wf.contains(&format!("=> \"{old}\"")),
            "a formula still installs the old command {old}"
        );
    }
    assert!(
        !wf.contains("> tap/formula_renames.json"),
        "the step still writes the rename map"
    );
    assert!(
        wf.contains("git rm -q --ignore-unmatch formula_renames.json"),
        "the step must remove a rename map the tap still holds"
    );
    let adds: Vec<&str> = wf
        .lines()
        .filter(|l| l.trim_start().starts_with("git add") && l.contains("formula_renames.json"))
        .collect();
    assert!(
        adds.is_empty(),
        "a `git add` still names the removed rename map: {adds:?}"
    );
}

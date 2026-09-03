//! Run the frontend's own contract checks (`tests/ui_contract.mjs`) from `cargo test`.
//!
//! The .mjs file existed but no workflow, Cargo target or script referenced it, so ~2,000 lines
//! of shipped frontend had nothing executing against them. It asserts the things only the JS can
//! answer: that `reference.css` / `reference-shell.html` are still exact extractions of the
//! design demo, and that the production CSS and the viewport module still carry the behaviours
//! the shell depends on. Running it here puts it inside the repo's ordinary gate.
//!
//! Node is not a build dependency of this workspace, so a machine without it SKIPS rather than
//! fails — CI runs the same script as its own step, where node is always present.

#[test]
fn the_frontend_contract_holds() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/ui_contract.mjs");
    let out = match std::process::Command::new("node").arg(script).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("skip: no `node` on PATH — CI runs this script as its own step");
            return;
        }
        Err(e) => panic!("could not run node: {e}"),
    };
    assert!(
        out.status.success(),
        "ui_contract.mjs failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
}

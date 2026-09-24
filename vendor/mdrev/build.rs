//! The vendored release's `bundle/` as a table of `include_bytes!` (#274): one row per file, keyed
//! by its path inside `bundle/` and sorted by it, so the monitor finds a file by binary search.
//! Generated rather than written by hand because a release's lazy chunks are content-hashed and
//! their names change with every version.

use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets it"));
    let bundle = manifest.join("release").join("bundle");
    // A directory: cargo rebuilds when anything beneath it changes, which a re-vendor always does.
    println!("cargo:rerun-if-changed=release");

    let mut files = Vec::new();
    walk(&bundle, &bundle, &mut files);
    files.sort();
    let mut out = String::from(
        "/// Every file of the release's `bundle/` — the guest the page mounts — by its path inside\n\
         /// `bundle/`, sorted by that path.\n\
         pub static BUNDLE: &[(&str, &[u8])] = &[\n",
    );
    for rel in &files {
        let abs = bundle.join(rel);
        let abs = abs.to_str().expect("the checkout's path is UTF-8");
        out.push_str(&format!("    ({rel:?}, include_bytes!({abs:?})),\n"));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets it")).join("bundle.rs");
    std::fs::write(&dest, out).unwrap_or_else(|e| panic!("{}: {e}", dest.display()));
}

/// Every file under `dir`, as a `/`-separated path relative to `base`. Anything that is neither a
/// file nor a directory stops the build: a symlink in a vendored tree could embed a file from
/// outside it.
fn walk(base: &Path, dir: &Path, files: &mut Vec<String>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| {
        panic!(
            "{}: {e} — the pin is vendored by scripts/vendor-mdrev.sh",
            dir.display()
        )
    });
    for entry in entries {
        let entry = entry.expect("a readable directory entry");
        let path = entry.path();
        let kind = entry.file_type().expect("a file type");
        if kind.is_dir() {
            walk(base, &path, files);
        } else if kind.is_file() {
            let rel = path.strip_prefix(base).expect("beneath the bundle");
            let parts: Vec<&str> = rel
                .components()
                .map(|c| c.as_os_str().to_str().expect("a UTF-8 file name"))
                .collect();
            files.push(parts.join("/"));
        } else {
            panic!(
                "{}: the vendored release holds only files and directories",
                path.display()
            );
        }
    }
}

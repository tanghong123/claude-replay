//! Writes the kit (#s432, `claude_monitor::kit`) into a directory: `cargo run -p claude-monitor
//! --example kit -- <dir>` makes `<dir>/agent-monitor-kit-<version>/` and prints its path.
//! `scripts/kit.sh` packs it for a release. It refuses a kit directory that is there already.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(out) = std::env::args_os().nth(1) else {
        eprintln!("usage: cargo run -p claude-monitor --example kit -- <dir>");
        return ExitCode::from(2);
    };
    let files = match claude_monitor::kit::files() {
        Ok(files) => files,
        Err(why) => {
            eprintln!("the kit could not be laid out: {why}");
            return ExitCode::FAILURE;
        }
    };
    let root = Path::new(&out).join(claude_monitor::kit::name());
    if root.exists() {
        eprintln!("{} is there already", root.display());
        return ExitCode::FAILURE;
    }
    for (path, bytes) in files {
        let file = root.join(&path);
        let written = file
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&file, &bytes));
        if let Err(why) = written {
            eprintln!("could not write {}: {why}", file.display());
            return ExitCode::FAILURE;
        }
    }
    println!("{}", root.display());
    ExitCode::SUCCESS
}

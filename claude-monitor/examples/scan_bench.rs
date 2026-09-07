//! Throwaway measurement: how long one `Index` scan cycle costs on this machine.
//! Roots come from env so nothing touches the owner's live monitor.

use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let root = PathBuf::from(std::env::var("BENCH_ROOT").expect("BENCH_ROOT"));
    std::fs::create_dir_all(&root).unwrap();
    let idx = claude_monitor::index::Index::new(root, Vec::new());
    for i in 0..3 {
        if i > 0 {
            std::thread::sleep(std::time::Duration::from_millis(2200));
        }
        let t = Instant::now();
        let json = idx.sessions_json(|_p| {});
        let el = t.elapsed();
        let rows = json.matches("\"activityTs\"").count();
        let groups = json.matches("\"metaLine\"").count();
        let visited = json.matches("\"visited\":true").count();
        println!(
            "scan {i}: {:?}  json={} bytes  rows={rows} groups={groups} visited_true={visited}",
            el,
            json.len()
        );
    }
}

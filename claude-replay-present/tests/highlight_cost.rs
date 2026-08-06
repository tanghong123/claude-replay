//! Where the syntax highlighter's time actually goes (#107).
//!
//! The first layout of a 107 MB session spends 5681 ms of its 5.9 s inside `render::block_body`,
//! and effectively all of that is syntect. Before changing anything, split that cost into the two
//! things a call does: PER-CALL SETUP (find the syntax by token, build a `HighlightLines`) and
//! PER-LINE PARSING. The diff renderer calls `highlight_one` once per row, so it pays setup on
//! every line; `highlight_spans` pays it once for the whole block.
//!
//! If setup dominates, the fix is a memo table and carries no risk to the rendered output at all.
//! If parsing dominates, no amount of caching helps and the fix has to avoid the work entirely.
//!
//! `#[ignore]`d — a measurement, not an assertion. Run with:
//!   cargo test -p claude-replay-present --release --test highlight_cost -- --ignored --nocapture
use claude_replay_present::highlight;
use std::time::Instant;

const LINE: &str = "    let mut spans = vec![Span::styled(format!(\"{gutter} \"), patch(style))];";

#[test]
#[ignore]
fn per_call_setup_versus_per_line_parsing() {
    const N: usize = 2000;
    let code: String = std::iter::repeat_n(LINE, N).collect::<Vec<_>>().join("\n");

    // Warm the lazily-loaded SyntaxSet so its one-time load isn't charged to either number.
    let _ = highlight::highlight_one(LINE, "rs");

    // One call per line — what the diff renderer does today.
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(highlight::highlight_one(LINE, "rs"));
    }
    let per_call = t.elapsed();

    // One call for all N lines — setup paid once.
    let t = Instant::now();
    std::hint::black_box(highlight::highlight_spans(&code, "rs"));
    let batched = t.elapsed();

    // A token that matches nothing still walks the whole syntax set before falling back.
    let t = Instant::now();
    for _ in 0..N {
        std::hint::black_box(highlight::highlight_one(LINE, ""));
    }
    let plain_token = t.elapsed();

    println!("\n{N} lines of Rust");
    println!("  highlight_one  (setup per line) : {per_call:?}");
    println!("  highlight_spans(setup once)     : {batched:?}");
    println!("  highlight_one  (empty token)    : {plain_token:?}");
    println!(
        "  => setup is {:.1}x the parsing; batching would save {:.0}%",
        per_call.as_secs_f64() / batched.as_secs_f64().max(1e-9),
        100.0 * (1.0 - batched.as_secs_f64() / per_call.as_secs_f64().max(1e-9))
    );
}

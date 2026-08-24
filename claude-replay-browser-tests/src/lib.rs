//! No library code: this crate exists to hold `tests/browser_follow.rs` outside the workspace's
//! `default-members`, so its headless-Chrome dependency is built only when those tests are run
//! explicitly. See the notes in `Cargo.toml`.

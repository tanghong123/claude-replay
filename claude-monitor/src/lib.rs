//! `claude-monitor` as a LIBRARY — the machine-wide session index (#98) plus the control
//! plane (#133), so a second front-end can reuse them instead of forking them.
//!
//! The binary (`agent-monitor`, `src/main.rs`) is the v1 rail over these modules;
//! `agent-monitor-v2` composes the same session view without an iframe and drives the same
//! `Index`, `ConsentStore`, `Passcode` and send transports through here. Splitting the crate
//! this way is what keeps ONE implementation of "may this prompt be injected into that pane".

pub mod consent;
pub mod control;
pub mod cost;
pub mod index;
pub mod state;
pub mod ui;

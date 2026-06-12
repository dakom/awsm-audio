//! Authored WebAudio "sample" schema — the pure-data shape the awsm-audio
//! editor saves/loads and the `awsm-audio-player` runtime instantiates onto a
//! real `web_sys::AudioContext`. No `web-sys` dependency lives here: these are
//! plain serde types so they round-trip through TOML (the hand-editable
//! authored format), travel over an MCP/IPC transport, and run in headless
//! tests unchanged.
//!
//! The types are laid out to serialize cleanly to TOML: scalar fields are
//! declared before nested tables, and data-carrying enums are adjacently
//! tagged.
//!
//! # The model
//!
//! A [`Sample`] is the unit of authoring. It owns a [`Graph`] — a set of
//! [`Node`]s wired by [`Connection`]s — and publishes three things to whoever
//! hosts it:
//!
//! - **inlets / outlets** ([`PortDecl`]): named audio boundary ports, so the
//!   sample can be dropped into a larger graph and wired up like any other node.
//! - **parameters** ([`SampleParam`]): exposed "macro" knobs that fan out to one
//!   or more inner [`AudioParam`]s (the parameterization layer).
//! - **trigger** ([`TriggerSpec`]): which source nodes start/stop on note-on /
//!   note-off, so the sample plays like an instrument.
//!
//! # Composition (nesting)
//!
//! A node's [`NodeKind`] is either a primitive WebAudio node (every node type
//! the platform offers — see [`nodes`]) **or** [`NodeKind::Sample`], a reference
//! to another sample by id. A referenced sample behaves uniformly like a
//! primitive: its inlets/outlets become the ref node's numbered inputs/outputs
//! and its exposed params become settable/modulatable. This is the standard
//! sub-patch/abstraction model (Max/MSP, Pd, Reaktor) and is what makes
//! "compose samples out of smaller samples" work.
//!
//! # No hardware destination inside a sample
//!
//! A sample never embeds the context `AudioDestinationNode` — that would make it
//! un-composable. Instead its **outlets** are its outputs; the top-level host
//! ([`SampleLibrary::root`]) is what the player ultimately wires to
//! `ctx.destination`.
//!
//! # Time & values
//!
//! AudioParam intrinsic values are `f32` (as in WebAudio); automation/scheduling
//! times are `f64` seconds, interpreted relative to the sample's trigger
//! (note-on = t0) rather than the absolute context clock.

pub mod arrangement;
pub mod asset;
pub mod connection;
pub mod enums;
pub mod error;
pub mod flatten;
pub mod graph;
pub mod ids;
pub mod library;
pub mod nodes;
pub mod param;
pub mod sample;
pub mod song;

pub use arrangement::*;
pub use asset::*;
pub use connection::*;
pub use enums::*;
pub use error::*;
pub use flatten::*;
pub use graph::*;
pub use ids::*;
pub use library::*;
pub use nodes::*;
pub use param::*;
pub use sample::*;
pub use song::*;

#[cfg(test)]
mod tests;

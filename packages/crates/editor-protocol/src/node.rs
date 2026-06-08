//! Pure-data node vocabulary shared with the editor: the graph-boundary kind
//! ([`BoundaryPort`]), the editor-only wire identity ([`ConnId`]), and where a
//! wire lands ([`ConnSink`]).
//!
//! The *live* node structures (the `Mutable`/`Rc`-wrapped `EditorNode` /
//! `EditorConnection`) stay in the editor crate and re-export these names.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use awsm_audio_schema::ParamId;

/// Which kind of graph boundary a boundary node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryPort {
    /// A sample input — feeds signal in (renders with one output port).
    Inlet,
    /// A sample output — emits signal out (renders with one input port).
    Outlet,
}

/// Editor-only identity for a wire (the schema has no per-connection id).
pub type ConnId = Uuid;

/// Where a wire lands: a node's audio input, one of its automatable params
/// (modulation), or an instrument-ref's trigger inlet (a sequencer binding —
/// not an audio edge; the scheduler consumes it).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnSink {
    Input(u32),
    Param(ParamId),
    Trigger,
}

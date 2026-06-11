//! Shared, serializable command/query/transport vocabulary for driving the
//! awsm-audio editor remotely (MCP / WebSocket) and from headless tests.
//!
//! Pure data — no DOM, audio, reactive, or async deps — so it compiles for both
//! the editor's wasm target and the native MCP server. Heavy payloads (the audio
//! graph, samples, arrangement) live in `awsm_audio_schema`, which this crate
//! re-exports so callers have one import path. This crate is the *vocabulary*;
//! the editor crate is the *interpreter*.

mod clipboard;
mod command;
mod field;
mod node;
mod query;
mod snapshot;
mod transport;

pub use awsm_audio_schema as schema;

pub use clipboard::Clipboard;
pub use command::{ArrangeOp, ControlOp, EditorCommand, PlacedClip, SongOp};
pub use field::FieldValue;
pub use node::{BoundaryPort, ConnId, ConnSink};
pub use query::{
    AssetInfo, EditorQuery, FieldInfo, NodeKindInfo, QueryResult, RenderPlanInfo, SampleInfo,
    TrackStats, TransportInfo, WavStats, WaveformEnvelope,
};
pub use snapshot::{EditorProject, EditorSnapshot, NodeLayout};
pub use transport::{
    AudioInfo, BatchItemResult, EditorEvent, RenderHandle, Request, Response, WsClientMsg,
    WsServerMsg,
};

#[cfg(test)]
mod tests;

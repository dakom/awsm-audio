//! Re-exports of the editor's command/query vocabulary, now owned by the shared
//! [`awsm_audio_editor_protocol`] crate.
//!
//! The pure-data types (`EditorCommand`, `EditorQuery`, `QueryResult`, the
//! sequencer/arrangement ops, the read-back info structs) moved into the
//! protocol crate so the native MCP server and the editor speak one vocabulary.
//! This module stays as a thin re-export hub so existing `controller::command::*`
//! / `controller::*` paths still resolve; the *interpreter* — applying these to
//! the live [`EditorController`](super::EditorController) — lives in
//! [`super`] (`dispatch`/`query`) and [`super::snapshot`].

pub use awsm_audio_editor_protocol::{
    ArrangeOp, AssetInfo, ControlOp, EditorCommand, EditorQuery, FieldInfo, NodeKindInfo,
    PlacedClip, QueryResult, SampleInfo, SongOp, TransportInfo,
};

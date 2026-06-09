//! The request/response envelope exchanged over the WebTransport link between
//! the native MCP server and the in-browser editor. One request per
//! server-initiated bidi stream; the editor replies on the same stream. No
//! request-id correlation — stream identity is the correlation, framing is by
//! stream-finish. JSON-encoded.

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{NodeId, SampleId};

use crate::{EditorCommand, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Apply a mutation through `EditorController::dispatch`.
    Dispatch(EditorCommand),
    /// Apply a list of mutations in order (one round-trip). awsm-audio has no
    /// batch-undo API yet, so the editor applies these sequentially.
    DispatchBatch(Vec<EditorCommand>),
    /// Run a read-only `EditorQuery`.
    Query(EditorQuery),
    /// Transport control (the `editor_play`/`editor_stop` seams).
    Play,
    Stop,
    /// Navigation: make `sample` the active editing canvas, so subsequent
    /// `Dispatch`/`Query` operate on its graph. Session state — like Play/Stop,
    /// not a document command. Needed to author multi-sample projects
    /// (instruments, sub-Sounds) over MCP: switch to a sub-sample, edit it, switch
    /// back.
    SetActiveSample {
        sample: SampleId,
    },
    /// Render a Sound offline to a `.wav` (raw bytes). `sample = None` renders
    /// the project root. Optional `sample_rate` overrides the bounce rate.
    RenderWav {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample: Option<SampleId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<f32>,
    },
    /// Attach a compiled WASM DSP module (base64-encoded `.wasm`) to an
    /// AudioWorklet node. Carries bytes (not an `EditorCommand`) for the same
    /// reason WAV renders are a `Response`, not a command: large binary stays out
    /// of the command/undo stream. The editor compiles + discovers params + binds
    /// it.
    AttachWasm {
        node: NodeId,
        wasm_base64: String,
        #[serde(default)]
        label: String,
    },
}

/// Editor → server **push** event (unsolicited channel). One per uni stream.
/// Relayed to the agent as an MCP logging notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorEvent {
    /// `"toast"` | `"selection"` | `"transport"`.
    pub kind: String,
    /// Toast severity (`"info"` | `"warning"` | `"error"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Selected node ids for `kind == "selection"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
}

/// Editor → server. The reply to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// A mutation / control op succeeded with no payload.
    Ok,
    /// A query result (boxed — `QueryResult::Snapshot` is large).
    Query(Box<QueryResult>),
    /// Raw `.wav` file bytes (RIFF/WAVE container).
    Wav(Vec<u8>),
    /// The request failed; the string is a human-readable reason.
    Err(String),
}

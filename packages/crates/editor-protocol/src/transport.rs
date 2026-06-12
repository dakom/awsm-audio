//! The request/response vocabulary exchanged over the editor link between the
//! native MCP server and the in-browser editor, plus the [`WsServerMsg`] /
//! [`WsClientMsg`] frames that carry them over the WebSocket. The link is one
//! ordered channel, so `id` correlates each [`Response`] to its [`Request`].
//! JSON text frames.

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{NodeId, SampleId};

use crate::{EditorCommand, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// the project root. Optional `sample_rate` overrides the bounce rate;
    /// optional `duration_secs` overrides the render length (capture a fixed span
    /// of a procedural / worklet source that would otherwise render a tiny default).
    RenderWav {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample: Option<SampleId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
        /// Strip leading/trailing silence (below -60 dBFS) from the rendered PCM
        /// before encoding — tight starts and controlled tails for one-shots.
        #[serde(default, skip_serializing_if = "is_false")]
        trim_silence: bool,
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
    /// Load an external audio file into an `AudioBufferSource` (or `Convolver`)
    /// node's buffer. The editor fetches `url` and `decodeAudioData`s it — bytes
    /// never ride this link. For an agent-local file the server hosts it under
    /// `/assets/<id>` and passes that URL here.
    LoadAudio {
        node: NodeId,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<String>,
    },
    /// Merge a serialized [`SampleLibrary`](awsm_audio_schema::SampleLibrary)
    /// (TOML — what `export_sample` writes) into the open project: its samples
    /// and the assets they reference. Samples whose ids already exist are
    /// rejected (re-import the same patch via `duplicate_sample` instead).
    /// Carries the payload on the link like `AttachWasm` does — imports are
    /// rare and the libraries small.
    ImportSamples {
        library_toml: String,
    },
}

/// serde helper: skip serializing a `false` flag (keeps default wire shapes
/// byte-identical to before the field existed).
fn is_false(b: &bool) -> bool {
    !*b
}

/// Server → browser WebSocket frame.
// `Request` is the dominant variant (one per editor request), so boxing it to
// shrink the rarely-used unit variants would just add an allocation to the hot
// path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsServerMsg {
    /// Serve this request and reply with [`WsClientMsg::Response`] carrying the
    /// same `id`.
    Request { id: u64, req: Request },
    /// The agent that wants this editor is ambiguous and supplied no pairing
    /// code — the editor should prompt for one and send [`WsClientMsg::Pair`].
    PairingRequired,
    /// This socket's binding was taken over (another tab/agent paired) — the
    /// editor should show itself disconnected.
    Detached,
}

/// Browser → server WebSocket frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WsClientMsg {
    /// Claim a binding to the agent holding this pairing code. Optional first
    /// frame; unnecessary in the unambiguous 1:1 auto-bind case.
    Pair { code: String },
    /// Reply to a [`WsServerMsg::Request`] with the matching `id`.
    Response { id: u64, resp: Response },
    /// An unsolicited editor push event.
    Event(EditorEvent),
}

/// Editor → server **push** event (unsolicited channel). Relayed to the bound
/// agent as an MCP logging notification.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// A reference to a render the editor has uploaded out-of-band. The WAV bytes do
/// **not** ride the control link — the editor POSTs them to the server's
/// `/renders/<render_id>` HTTP route and returns this small handle here instead.
/// Keeps the link byte-light (a large render never blocks small frames).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderHandle {
    /// Opaque id (uuid v4) the editor minted and POSTed the bytes under.
    pub render_id: String,
    /// Size of the uploaded `.wav` in bytes.
    pub byte_len: usize,
    /// Rendered duration in seconds.
    pub duration_secs: f64,
    /// Peak absolute sample (0.0..=1.0+; >1.0 means clipping).
    pub peak: f32,
    /// RMS level across the render.
    pub rms: f32,
}

/// Result of a [`Request::LoadAudio`] — the decoded buffer's shape, so the agent
/// can confirm the load without the samples crossing the link.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioInfo {
    /// The buffer asset the editor created and assigned to the node.
    pub asset_id: String,
    pub duration_secs: f64,
    pub sample_rate: f32,
    pub channels: usize,
}

/// Per-command outcome inside a [`Response::Batch`] — mirrors the order of the
/// dispatched [`Request::DispatchBatch`] list, so an agent can see which command
/// created what (and which one failed) without a follow-up snapshot.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchItemResult {
    /// Did this command apply? (The editor's `dispatch` is infallible, so this is
    /// `false` only for a command rejected before dispatch.)
    pub ok: bool,
    /// The uuid of the node / sample / boundary / sample-ref this command created,
    /// if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// A human-readable reason this command failed, if `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Editor → server. The reply to a [`Request`].
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// A mutation / control op succeeded with no payload.
    Ok,
    /// A mutation succeeded and created a document object; carries its minted
    /// uuid (a node / sample / boundary / sample-ref id) so the caller needn't
    /// re-snapshot to learn it.
    Created { id: String },
    /// Per-command results for a [`Request::DispatchBatch`], in order.
    Batch(Vec<BatchItemResult>),
    /// A query result (boxed — `QueryResult::Snapshot` is large).
    Query(Box<QueryResult>),
    /// A render reference. The `.wav` bytes were uploaded out-of-band (see
    /// [`RenderHandle`]); only this handle crosses the control link.
    Render(RenderHandle),
    /// An audio buffer was loaded into a node (see [`Request::LoadAudio`]).
    AudioLoaded(AudioInfo),
    /// Samples were merged into the project (see [`Request::ImportSamples`]) —
    /// one entry per imported sample.
    Imported(Vec<crate::SampleInfo>),
    /// The request failed; the string is a human-readable reason.
    Err(String),
}

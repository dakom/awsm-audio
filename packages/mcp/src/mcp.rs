//! The rmcp tool layer. Each tool is a thin typed wrapper that builds a protocol
//! [`Request`] and relays it to this session's bound editor tab over the
//! WebSocket link, then shapes the [`Response`] into an MCP result. All editor
//! mutation flows through `EditorController` on the far side; this layer only
//! translates.
//!
//! Coverage: discovery/read queries, the WAV readback surface (render_wav /
//! wav_stats / waveform), transport, a few ergonomic typed mutators, and the
//! generic escape hatches (`dispatch_command` / `dispatch_batch` / `run_query`)
//! so every `EditorCommand` / `EditorQuery` variant is reachable even when its
//! payload references schema types without a JSON schema.

use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, CallToolResult, Content, GetPromptRequestParams, GetPromptResult,
    ListPromptsResult, ListResourcesResult, LoggingLevel, LoggingMessageNotificationParam,
    PaginatedRequestParams, Prompt, PromptMessage, PromptMessageRole, RawResource,
    ReadResourceRequestParams, ReadResourceResult, ResourceContents, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::{NotificationContext, RequestContext};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, schemars, tool, tool_handler, tool_router,
};
use serde_json::Value;

use awsm_audio_editor_protocol::schema::{
    ArrSection, AutomationEvent, NodeId, NodeKind, SampleId, SampleKind,
};
use awsm_audio_editor_protocol::{
    ArrangeOp, AssetInfo, BoundaryPort, EditorCommand, EditorQuery, FieldValue, PlacedClip,
    QueryResult, Request, Response,
};

use std::sync::Arc;

use crate::link::{AgentSession, EditorLink, LinkError};

/// The MCP tool provider — one per MCP session. Cheap to clone (handles are
/// `Arc`s); clones share the same [`AgentSession`], so a session's editor binding
/// is stable across clones.
#[derive(Clone)]
pub struct EditorMcp {
    link: EditorLink,
    /// This session's identity + editor binding. Every request routes only to the
    /// bound editor tab.
    agent: Arc<AgentSession>,
    // Populated by `Self::tool_router()` and consumed by the `#[tool_handler]`
    // generated routing; the dead-code lint can't see that use.
    #[allow(dead_code)]
    tool_router: ToolRouter<EditorMcp>,
}

// ───────────────────────────── parameter types ──────────────────────────────

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SnapshotParams {
    /// `"full"` (default) returns everything; `"ids"` omits the (large) embedded
    /// note_sequencer song events, returning node ids/kinds/wires + a per-track
    /// `events_count`. Use `"ids"` for later round-trips once patterns are authored.
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SampleArg {
    /// Target Sound/sample id (from `list_samples`). Omit to use the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SampleReq {
    /// Target sample id (from `list_samples`).
    pub sample: SampleId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NodeArg {
    /// Target node id (from `get_snapshot`).
    pub node: NodeId,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenderWavParams {
    /// Sound to render. Omit to render the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
    /// Override the bounce sample rate (Hz).
    #[serde(default)]
    pub sample_rate: Option<f32>,
    /// Fixed render length in seconds — capture a span of a procedural / worklet
    /// source that otherwise renders only a tiny default. Omit for the default.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// Strip leading/trailing silence (below -60 dBFS) before encoding — tight
    /// starts and controlled tails for one-shot exports.
    #[serde(default)]
    pub trim_silence: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WavStatsParams {
    /// Sound to inspect. Omit to use the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
    /// false (default): measure the LIVE graph (what it sounds like right now).
    /// true: measure the stored BOUNCED asset (what plays in arrangements) —
    /// returns "not yet bounced" if it hasn't been bounced.
    #[serde(default)]
    pub bounced: bool,
    /// Live-render window in seconds (only when bounced=false) — e.g. to capture a
    /// long release tail. Omit for the auto length.
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaveformParams {
    /// Sound to render. Omit to render the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
    /// Number of min/max buckets (envelope columns) to return.
    pub buckets: u32,
    /// false (default): the LIVE graph; true: the stored BOUNCED asset ("not yet
    /// bounced" if none).
    #[serde(default)]
    pub bounced: bool,
    /// Live-render window in seconds (only when bounced=false). Omit for the auto
    /// length.
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddNodeParams {
    /// The node kind to create — either a bare kind-name string with default
    /// props (`"oscillator"`, `"gain"`, … — any tag from `list_node_kinds`), or a
    /// full `NodeKind` value, adjacently tagged (`{"kind":"<tag>","props":{…}}`).
    /// The param schema lists every kind and its props; `list_node_kinds` gives a
    /// copy-paste `example` + docs per kind.
    pub kind: Flexible<NodeKind>,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddChainParams {
    /// The node kinds to create, source → … → sink, each auto-connected to the
    /// next (output 0 → input 0). A NodeKind value or a bare kind-name string —
    /// e.g. `["oscillator", "biquad_filter", "gain"]`, or full
    /// `{"kind":"…","props":{…}}` values from `list_node_kinds`.
    pub kinds: Vec<Flexible<NodeKind>>,
    /// World x of the first node (each subsequent node is offset by `spacing`).
    #[serde(default)]
    pub x: f64,
    /// World y (shared by the row).
    #[serde(default)]
    pub y: f64,
    /// Horizontal gap between nodes. Defaults to 180.
    #[serde(default)]
    pub spacing: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RefBatchParams {
    /// EditorCommands applied in order in one round-trip, with **symbolic refs**.
    /// Each element is a command **object** — `{"cmd":…,"args":…}` (NOT a
    /// JSON-encoded string; stringified objects are tolerated but discouraged) —
    /// plus an optional `"ref":"<name>"` that labels the id it mints. Any later
    /// command can use `"$<name>"` anywhere an id is expected and it's
    /// substituted with the real id before dispatch — so a create-then-connect
    /// flow is one tool call:
    /// `[{"cmd":"add_node","ref":"osc","args":{"kind":"oscillator","x":0,"y":0}},
    ///   {"cmd":"add_node","ref":"amp","args":{"kind":"gain","x":200,"y":0}},
    ///   {"cmd":"connect","args":{"from":"$osc","to":"$amp"}}]`.
    /// (`add_node` args accept a bare kind tag, expanded to default props.)
    pub commands: Vec<serde_json::Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddBoundaryParams {
    /// `"inlet"` (a sample input — feeds signal in) or `"outlet"` (a sample
    /// output — emits signal out; an instrument Sound needs one).
    pub port: BoundaryPort,
    /// World x position.
    #[serde(default)]
    pub x: f64,
    /// World y position.
    #[serde(default)]
    pub y: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetAutomationParams {
    /// Target node id (from get_snapshot / add_node).
    pub node: NodeId,
    /// The AudioParam to automate, by its field key — e.g. `"gain"`,
    /// `"frequency"` (see get_node_fields for a node's automatable keys).
    pub param: String,
    /// The complete scheduled timeline for that param (REPLACES any existing
    /// events). Times are seconds — from note-on for a triggered instrument
    /// voice, from render start otherwise. Each event is adjacently tagged
    /// `{"event":"<name>","args":{…}}`; the schema lists every accepted shape.
    pub events: Vec<AutomationEvent>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ValidateParams {
    /// Commands to validate without dispatching — the same shapes
    /// dispatch_batch / dispatch_refs accept, including `"ref"` labels,
    /// `"$ref"` placeholders, and bare node-kind tags in add_node.
    pub commands: Vec<Value>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CreateArrangementParams {
    /// Tempo in BPM (the bar/beat grid). Omit for the default.
    #[serde(default)]
    pub bpm: Option<f64>,
    /// Timeline length in seconds. Omit for the default.
    #[serde(default)]
    pub length_secs: Option<f64>,
    /// Initial track names, top to bottom (e.g. `["Drums","Bass","Lead"]`) —
    /// saves one add_arrangement_track + set_track_name round-trip per track.
    #[serde(default)]
    pub tracks: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddTracksParams {
    /// Names for the new tracks, appended in order (one add+rename per name,
    /// applied in a single round-trip).
    pub names: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackGainItem {
    /// 0-based track index.
    pub track: usize,
    /// Linear gain (1.0 = unity).
    pub gain: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackGainsParams {
    /// Per-track gains to set, applied in one round-trip.
    pub gains: Vec<TrackGainItem>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DuplicateSampleParams {
    /// The Sound or Arrangement to duplicate (graph, trigger, arrangement,
    /// bounce — everything).
    pub sample: SampleId,
    /// Name for the copy. Omit for `"<original> (clone)"`.
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CompareSamplesParams {
    /// First sample to render + measure.
    pub a: SampleId,
    /// Second sample to render + measure.
    pub b: SampleId,
    /// Shared render window in seconds (live renders only) — pass it so both
    /// sides measure the same span. Omit for each side's auto length.
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// false (default): measure each LIVE graph; true: measure the stored
    /// BOUNCED assets (errors if either side has no bounce).
    #[serde(default)]
    pub bounced: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportWavParams {
    /// Destination `.wav` path. A directory (or trailing `/`) gets
    /// `<sample-name>.wav` appended. Relative paths resolve against the MCP
    /// server's working directory — prefer an absolute path.
    pub path: String,
    /// Sound/Arrangement to render. Omit to render the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
    /// Override the render sample rate (Hz).
    #[serde(default)]
    pub sample_rate: Option<f32>,
    /// Fixed render length in seconds (see render_wav).
    #[serde(default)]
    pub duration_secs: Option<f64>,
    /// Strip leading/trailing silence (below -60 dBFS) before encoding.
    #[serde(default)]
    pub trim_silence: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct NotesParams {
    /// The sample to annotate.
    pub sample: SampleId,
    /// Free-form working notes ("impact variant", "keeper", "needs shorter
    /// tail"). Empty clears them. Shown in list_samples.
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SectionsParams {
    /// Named timeline regions, e.g. `[{"name":"intro","start":0,"end":8}, …]`
    /// (seconds). REPLACES the arrangement's whole section list; empty clears.
    pub sections: Vec<ArrSection>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SweepParams {
    /// The node whose field to sweep.
    pub node: NodeId,
    /// The set_field key to vary (see get_node_fields).
    pub key: String,
    /// The values to try, in order (each is set, rendered, measured).
    pub values: Vec<f64>,
    /// Render window per measurement in seconds. Omit for the auto length.
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExportSampleParams {
    /// The sample to export (with every sub-sample it references and the assets
    /// they use).
    pub sample: SampleId,
    /// Destination `.toml` path. A directory (or trailing `/`) gets
    /// `<sample-name>.toml` appended. Relative paths resolve against the MCP
    /// server's working directory — prefer an absolute path.
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImportSampleParams {
    /// Path to a `.toml` written by export_sample (or any SampleLibrary TOML).
    pub path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ConnectParams {
    /// Source node id.
    pub from: NodeId,
    /// Source output port index.
    #[serde(default)]
    pub from_output: u32,
    /// Destination node id.
    pub to: NodeId,
    /// Destination input port index.
    #[serde(default)]
    pub to_input: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SetFieldParams {
    /// Target node id.
    pub node: NodeId,
    /// Field key (from `get_node_fields` / `list_node_kinds`).
    pub key: String,
    /// The value: a number (most fields), a string (a choice/text field like an
    /// oscillator `type`), or a bool. The right `FieldValue` is chosen from the
    /// JSON type.
    pub value: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BindParams {
    /// Sequencer node id (the SeqOut source).
    pub from: NodeId,
    /// Instrument Sample-ref node id (the trigger inlet to drive).
    pub to: NodeId,
    /// Output index in the sequencer's `outputs`. Give this OR `from_output_key`.
    #[serde(default)]
    pub from_output: Option<u32>,
    /// Output KEY (e.g. `"t0"` or `"t2:n36"`) — resolved to its index for you, so
    /// you needn't snapshot to learn output order. Give this OR `from_output`.
    #[serde(default)]
    pub from_output_key: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct CommandParams {
    /// An `EditorCommand` (adjacently tagged by `"cmd"`/`"args"`). The param schema
    /// documents every command variant and its args.
    pub command: Flexible<EditorCommand>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BatchParams {
    /// `EditorCommand`s applied in order in one round-trip.
    pub commands: Vec<Flexible<EditorCommand>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct QueryParams {
    /// An `EditorQuery` (adjacently tagged by `"query"`/`"args"`).
    pub query: Flexible<EditorQuery>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct MarkersParams {
    /// Loop/export start marker (seconds). Omit ALL marker fields to clear the
    /// markers (loop + export span the whole timeline).
    #[serde(default)]
    pub start: Option<f64>,
    /// Loop/export end marker (seconds). Must be > start to take effect.
    #[serde(default)]
    pub end: Option<f64>,
    /// Start marker in bars (converted with the arrangement BPM); alternative to
    /// `start`.
    #[serde(default)]
    pub start_bars: Option<f64>,
    /// End marker in bars; alternative to `end`.
    #[serde(default)]
    pub end_bars: Option<f64>,
    /// Beats per bar for the bar forms. Defaults to 4.
    #[serde(default)]
    pub beats_per_bar: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LoadAudioParams {
    /// AudioBufferSource (or Convolver) node to load the audio into. Create one
    /// first with add_node (kind audio_buffer_source).
    pub node: NodeId,
    /// An agent-local audio file path (WAV/mp3/flac/ogg/…). The server reads it,
    /// hosts it off the link, and the editor fetches + decodes it. Provide this
    /// OR `url`, not both.
    #[serde(default)]
    pub path: Option<String>,
    /// A browser-reachable audio URL the editor fetches directly. Provide this OR
    /// `path`, not both.
    #[serde(default)]
    pub url: Option<String>,
    /// Optional label for the created buffer asset.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AttachWasmParams {
    /// AudioWorklet node id. Create one first with add_node (kind audio_worklet).
    pub node: NodeId,
    /// Path to the compiled .wasm (e.g.
    /// target/wasm32-unknown-unknown/release/foo.wasm). The server reads + encodes
    /// it.
    #[serde(default)]
    pub wasm_path: Option<String>,
    /// Or inline base64 (standard, padded) if you already encoded it.
    #[serde(default)]
    pub wasm_base64: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkletParamSpec {
    /// Param name (becomes a labelled, automatable knob on the node).
    pub name: String,
    /// Minimum value. Defaults to 0.0.
    #[serde(default)]
    pub min: f32,
    /// Maximum value.
    pub max: f32,
    /// Initial value.
    pub default: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScaffoldWorkletParams {
    /// Crate/package name (kebab-case). Defaults to "my-worklet".
    #[serde(default)]
    pub name: Option<String>,
    /// The automatable params the processor declares (each becomes a knob).
    /// Defaults to a single `gain` (0.0–2.0, default 1.0).
    #[serde(default)]
    pub params: Option<Vec<WorkletParamSpec>>,
    /// Emit the `#![no_std]` variant (smaller wasm; the macro also emits the
    /// required panic handler). Defaults to false (a std crate, which "just works").
    #[serde(default)]
    pub no_std: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BounceParams {
    /// Target sample id (from `list_samples`).
    pub sample: SampleId,
    /// Optional fixed render length in seconds. Overrides the auto-computed
    /// duration — use it to capture a fixed span of a procedural / worklet source
    /// that otherwise renders only a tiny default. Omit to keep the default.
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RenameSampleParams {
    /// Sample to rename (Sound or Arrangement), from `list_samples`.
    pub sample: SampleId,
    /// The new name.
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddClipParams {
    /// Track index (0-based) in the active arrangement.
    pub track: usize,
    /// Clip start on the timeline, in seconds. Give exactly one of `start`,
    /// `start_beats`, or `start_bars` (the beat/bar forms use the arrangement's
    /// BPM, so no hand-conversion / float drift).
    #[serde(default)]
    pub start: Option<f64>,
    /// Clip start in beats (converted with the active arrangement's BPM).
    #[serde(default)]
    pub start_beats: Option<f64>,
    /// Clip start in bars (converted with the BPM × `beats_per_bar`).
    #[serde(default)]
    pub start_bars: Option<f64>,
    /// Beats per bar for `start_bars`. Defaults to 4.
    #[serde(default)]
    pub beats_per_bar: Option<f64>,
    /// Bounced Sound to place (from `list_assets`/`list_samples`). Must be bounced.
    pub source: SampleId,
    /// Timeline length in seconds; omit to use the full bounce duration.
    #[serde(default)]
    pub length: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddClipsParams {
    /// Track index (0-based) in the active arrangement.
    pub track: usize,
    /// Bounced Sound to place at every position (must be bounced).
    pub source: SampleId,
    /// Explicit start positions in seconds.
    #[serde(default)]
    pub starts: Option<Vec<f64>>,
    /// Or start positions in beats (converted with the arrangement BPM).
    #[serde(default)]
    pub starts_beats: Option<Vec<f64>>,
    /// Or start positions in bars (converted with BPM × `beats_per_bar`).
    #[serde(default)]
    pub starts_bars: Option<Vec<f64>>,
    /// Or a bar-range section string like `"3-12, 15-20"` — each range expands to
    /// one clip per bar over `[start, end)` (end-exclusive), i.e. a 1-bar loop
    /// tiled across the section. Bar units, using the arrangement BPM.
    #[serde(default)]
    pub sections: Option<String>,
    /// Beats per bar for the bar forms. Defaults to 4.
    #[serde(default)]
    pub beats_per_bar: Option<f64>,
    /// Per-clip timeline length in seconds; omit to use the full bounce duration.
    #[serde(default)]
    pub length: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipRef {
    /// Track index (0-based).
    pub track: usize,
    /// Clip index (0-based) within the track.
    pub clip: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ClipGainParams {
    pub track: usize,
    pub clip: usize,
    /// Linear gain (1.0 = unity).
    pub gain: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackArg {
    /// Track index (0-based).
    pub track: usize,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackNameParams {
    pub track: usize,
    pub name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TrackGainParams {
    pub track: usize,
    /// Linear gain (1.0 = unity).
    pub gain: f32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BpmParams {
    /// Tempo in BPM (clamped 20–400 by the editor).
    pub bpm: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LengthParams {
    /// Timeline length in seconds.
    pub secs: f64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct BeatsParams {
    /// Tempo in BPM.
    pub bpm: f64,
    /// Number of beats to convert (added to `bars`).
    #[serde(default)]
    pub beats: Option<f64>,
    /// Number of bars to convert (added to `beats`).
    #[serde(default)]
    pub bars: Option<f64>,
    /// Beats per bar (time-signature numerator). Defaults to 4.
    #[serde(default)]
    pub beats_per_bar: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DuplicateClipsParams {
    /// Track to duplicate within. Omit to duplicate every track's clips.
    #[serde(default)]
    pub track: Option<usize>,
    /// How many copies to append after the originals (each offset one interval more).
    pub count: usize,
    /// Repeat interval in seconds (takes priority if set).
    #[serde(default)]
    pub interval_secs: Option<f64>,
    /// Or the interval in beats (converted with `bpm`).
    #[serde(default)]
    pub interval_beats: Option<f64>,
    /// Or the interval in bars (converted with `bpm` × `beats_per_bar`).
    #[serde(default)]
    pub interval_bars: Option<f64>,
    /// BPM for beat/bar conversion. Defaults to the active arrangement's BPM.
    #[serde(default)]
    pub bpm: Option<f64>,
    /// Beats per bar for `interval_bars`. Defaults to 4.
    #[serde(default)]
    pub beats_per_bar: Option<f64>,
}

// ──────────────────────────────── tools ─────────────────────────────────────

#[tool_router]
impl EditorMcp {
    pub fn new(link: EditorLink) -> Self {
        let agent = link.register_agent();
        Self {
            link,
            agent,
            tool_router: Self::tool_router(),
        }
    }

    // ── discovery / read ────────────────────────────────────────────────────

    #[tool(
        description = "Full editor snapshot: graph (nodes + connections), node \
        layout, camera, selection, and the active arrangement. The starting point \
        for discovering node ids. Note it is scoped to the ACTIVE sample's canvas \
        only — call list_samples to see every sample and switch with \
        set_active_sample. Each connection carries a stable `id` you can pass to \
        the `disconnect` command to remove that one wire without touching its \
        endpoint nodes. Pass `detail:\"ids\"` to omit the large embedded sequencer \
        song events (keeps node ids/kinds/wires + per-track note counts) on later \
        round-trips."
    )]
    async fn get_snapshot(
        &self,
        Parameters(p): Parameters<SnapshotParams>,
    ) -> Result<CallToolResult, McpError> {
        let qr = self.query_result(EditorQuery::Snapshot).await?;
        let mut v = serde_json::to_value(&qr)
            .map_err(|e| McpError::internal_error(format!("encode snapshot: {e}"), None))?;
        if p.detail.as_deref() == Some("ids") {
            slim_snapshot(&mut v);
        }
        Ok(text(
            serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
        ))
    }

    #[tool(
        description = "List every creatable node kind with a ready-to-use default \
        value (`example`) and its editable field keys (`fields`) — so you can \
        add_node + set_field with no schema guessing. Call this before building a \
        graph."
    )]
    async fn list_node_kinds(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Catalog).await
    }

    #[tool(description = "The editable fields of one live node: each `key` (for \
        set_field), control type, current value, choice options, and whether it's \
        modulation-targetable. Use this to discover a node's set_field keys \
        (including a worklet's discovered params).")]
    async fn get_node_fields(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::NodeFields { node: p.node }).await
    }

    #[tool(description = "Every modulatable/automatable param across the ACTIVE \
        canvas, grouped by node: `[{node, label, kind, params}]`. The graph-wide \
        \"what can I modulate or automate?\" map — these params are exactly what \
        `set_automation` and `modulate` (the param targets of `connect`-style \
        wiring via dispatch_command) accept. Per-node detail (ranges, current \
        values): get_node_fields.")]
    async fn list_modulation_targets(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ModulationTargets).await
    }

    #[tool(description = "Report this session's editor pairing state WITHOUT \
        performing an editor operation: whether an editor tab is bound (or would \
        auto-bind), this session's pairing code, and how many tabs/agents are \
        connected. Call this first — or after a 'no editor is paired' error — to \
        know whether to wait or surface the pairing code, instead of issuing \
        doomed audio calls.")]
    async fn pairing_status(&self) -> Result<CallToolResult, McpError> {
        let editors = self.link.connection_count();
        let agents = self.link.agent_count();
        let bound = self.agent.bound_conn_id().is_some();
        // Mirrors `resolve`'s auto-bind rule: 1 unbound tab + 1 agent binds on
        // the next request, so report that as ready-to-pair.
        let will_auto_bind = !bound && editors == 1 && agents == 1;
        let authority = self
            .link
            .self_origin()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .to_string();
        let status = if bound {
            "paired"
        } else if will_auto_bind {
            "ready (auto-binds on the next editor operation)"
        } else if editors == 0 {
            "waiting for an editor tab to connect"
        } else {
            "ambiguous — pairing code required"
        };
        Ok(text(
            serde_json::json!({
                "status": status,
                "paired": bound,
                "editors_connected": editors,
                "agents_connected": agents,
                "pair_code": self.agent.pair_code,
                "how_to_pair": format!(
                    "open the editor with ?mcp={authority}&pair={} appended to its URL, \
                     or enter the code in its MCP connect modal",
                    self.agent.pair_code
                ),
            })
            .to_string(),
        ))
    }

    #[tool(description = "List every sample (id, name, kind, root/active flags).")]
    async fn list_samples(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Samples).await
    }

    #[tool(description = "List every bounceable Sound with its bounce status + \
        bounced duration.")]
    async fn list_assets(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Assets).await
    }

    #[tool(
        description = "The active sample's arrangement (tracks + clips), if it \
        is an Arrangement."
    )]
    async fn get_arrangement(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Arrangement).await
    }

    #[tool(description = "Live transport state: playing / peak / playhead / \
        audio-context state.")]
    async fn get_transport(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Transport).await
    }

    #[tool(description = "One Sound's bounce status: none (never bounced) / \
        clean / stale (graph changed since the bounce — re-bounce) / rendering \
        (in flight) / 'failed: <msg>' (the last render crashed — msg names the \
        offending node).")]
    async fn get_bounce_status(
        &self,
        Parameters(p): Parameters<SampleReq>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::BounceStatus { sample: p.sample })
            .await
    }

    #[tool(
        description = "How long an un-overridden bounce / render_wav will render a \
        Sound, and WHY — the queryable form of the (surprising) auto-duration \
        rules: a sequencer-driven Sound renders its song-loop length + release \
        tail; a continuous/one-shot graph renders a fixed default window. Returns \
        {duration_secs, is_sequence, loop_secs?, reason}. Call this before bouncing \
        a procedural source to decide whether you need a `duration_secs` override. \
        Omit `sample` for the project root."
    )]
    async fn get_render_plan(
        &self,
        Parameters(p): Parameters<SampleArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::RenderPlan { sample: p.sample })
            .await
    }

    // ── audio readback (the WAV surface) ─────────────────────────────────────

    #[tool(
        description = "Render a Sound offline to a .wav, saved to a temp file; \
        returns the path + byte count. An agent can't hear bytes — open the file \
        or use wav_stats/waveform to reason about it. Omit `sample` for the root. \
        For VERIFYING audio content, prefer `bounce` → wav_stats / waveform: those \
        keep the audio inside the document and avoid the upload round-trip. Use \
        render_wav for final exports of short clips."
    )]
    async fn render_wav(
        &self,
        Parameters(p): Parameters<RenderWavParams>,
    ) -> Result<CallToolResult, McpError> {
        let req = Request::RenderWav {
            sample: p.sample,
            sample_rate: p.sample_rate,
            duration_secs: p.duration_secs,
            trim_silence: p.trim_silence,
        };
        self.wav(req).await
    }

    #[tool(description = "Numeric stats of a Sound: duration_secs, peak, rms, \
        channels, sample_rate. By default measures the LIVE graph (what it sounds \
        like right now). Set bounced=true to measure the stored BOUNCED asset (what \
        plays in arrangements) — returns 'not yet bounced' if it hasn't been \
        bounced. Omit `sample` for the root.")]
    async fn wav_stats(
        &self,
        Parameters(p): Parameters<WavStatsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::WavStats {
            sample: p.sample,
            bounced: p.bounced,
            duration_secs: p.duration_secs,
        })
        .await
    }

    #[tool(
        description = "A downsampled min/max envelope (`buckets` columns) of a \
        Sound's render, so you can reason about the waveform shape in text. By \
        default the LIVE graph (what it sounds like now); set bounced=true for the \
        stored BOUNCED asset ('not yet bounced' if none). Omit `sample` for the root."
    )]
    async fn waveform(
        &self,
        Parameters(p): Parameters<WaveformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Waveform {
            sample: p.sample,
            buckets: p.buckets,
            bounced: p.bounced,
            duration_secs: p.duration_secs,
        })
        .await
    }

    // ── transport ────────────────────────────────────────────────────────────

    #[tool(
        description = "Start playback of the root Sound / arrangement. With the \
        transport loop off, a song/arrangement auto-stops and returns to idle when \
        its content ends (so a later play starts fresh, not mid-timeline); a \
        free-running audition plays until you call stop. Offline `bounce` / \
        render_wav are unaffected by transport state."
    )]
    async fn play(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Play).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(description = "Stop playback.")]
    async fn stop(&self) -> Result<CallToolResult, McpError> {
        match self.req(Request::Stop).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    // ── ergonomic typed mutators (common cases) ──────────────────────────────

    #[tool(
        description = "Add a node of `kind` (a typed NodeKind — see the param \
        schema, or copy a kind's `example` from list_node_kinds) at world (x, y). \
        Returns the minted node `id` (as `{ok, id}`) — no follow-up get_snapshot \
        needed to learn it. (add_sample/create_arrangement/add_boundary/ \
        add_sample_ref via dispatch_command, and every create in dispatch_batch, \
        return their id the same way.)"
    )]
    async fn add_node(
        &self,
        Parameters(p): Parameters<AddNodeParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddNode {
            kind: p.kind.0,
            x: p.x,
            y: p.y,
        })
        .await
    }

    #[tool(
        description = "Create a linear chain of nodes (source → … → sink) in one \
        call, auto-connecting each output 0 → the next input 0, and return all \
        minted ids in order. Covers the common synth-patch shape \
        (oscillator/noise → filter → gain → shaper → outlet). `kinds` are NodeKind \
        values or bare kind-name strings. Returns `{ids:[…]}`; set_field on any id \
        afterward to shape it. For non-linear graphs use dispatch_refs."
    )]
    async fn add_chain(
        &self,
        Parameters(p): Parameters<AddChainParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.kinds.is_empty() {
            return Err(McpError::invalid_params("kinds must be non-empty", None));
        }
        let spacing = p.spacing.unwrap_or(180.0);
        // Create each node, capturing its minted id (needed to wire the chain).
        let mut ids: Vec<String> = Vec::with_capacity(p.kinds.len());
        for (i, kind) in p.kinds.into_iter().enumerate() {
            let id = self
                .dispatch_created(EditorCommand::AddNode {
                    kind: kind.0,
                    x: p.x + i as f64 * spacing,
                    y: p.y,
                })
                .await?
                .ok_or_else(|| {
                    McpError::internal_error("add_node did not return a node id", None)
                })?;
            ids.push(id);
        }
        // Wire consecutive nodes (output 0 → input 0) in one batch.
        let mut connects: Vec<EditorCommand> = Vec::new();
        for w in ids.windows(2) {
            let (from, to) = (parse_node_id(&w[0])?, parse_node_id(&w[1])?);
            connects.push(EditorCommand::Connect {
                from,
                from_output: 0,
                to,
                to_input: 0,
            });
        }
        if !connects.is_empty() {
            match self.req(Request::DispatchBatch(connects)).await? {
                Response::Batch(_) | Response::Ok => {}
                Response::Err(e) => return Err(McpError::internal_error(e, None)),
                other => return Err(unexpected(other)),
            }
        }
        Ok(text(serde_json::json!({ "ids": ids }).to_string()))
    }

    #[tool(description = "Remove a node and every wire touching it.")]
    async fn remove_node(
        &self,
        Parameters(p): Parameters<NodeArg>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RemoveNode { id: p.node })
            .await
    }

    #[tool(description = "Wire an output port to an input port.")]
    async fn connect(
        &self,
        Parameters(p): Parameters<ConnectParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Connect {
            from: p.from,
            from_output: p.from_output,
            to: p.to,
            to_input: p.to_input,
        })
        .await
    }

    #[tool(
        description = "Set a node setting (the SetField command). `value` may be \
        a number (most fields), a string (a choice/text field like an oscillator \
        `type`), or a bool — the field type is inferred from the JSON value."
    )]
    async fn set_field(
        &self,
        Parameters(p): Parameters<SetFieldParams>,
    ) -> Result<CallToolResult, McpError> {
        let value = match p.value {
            Value::Number(n) => FieldValue::Num(n.as_f64().unwrap_or(0.0)),
            Value::String(s) => FieldValue::Text(s),
            Value::Bool(b) => FieldValue::Bool(b),
            other => {
                return Err(McpError::invalid_params(
                    format!("value must be a number, string, or bool (got {other})"),
                    None,
                ));
            }
        };
        self.dispatch(EditorCommand::SetField {
            id: p.node,
            key: p.key,
            value,
        })
        .await
    }

    #[tool(
        description = "Bind a Note Sequencer output to an instrument Sample-ref's \
        trigger inlet (a SeqOut → Trigger wire). Identify the output by `from_output` \
        (index) or `from_output_key` (its stable key like \"t0\" or \"t2:n36\", \
        resolved to the index for you — so order changes don't break your call and \
        you needn't snapshot first)."
    )]
    async fn bind(
        &self,
        Parameters(p): Parameters<BindParams>,
    ) -> Result<CallToolResult, McpError> {
        let from_output = match (p.from_output, p.from_output_key) {
            (Some(i), None) => i,
            (None, Some(key)) => self.resolve_output_index(p.from, &key).await?,
            (Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    "give from_output OR from_output_key, not both",
                    None,
                ));
            }
            (None, None) => 0,
        };
        self.dispatch(EditorCommand::Bind {
            from: p.from,
            from_output,
            to: p.to,
        })
        .await
    }

    #[tool(
        description = "Add an inlet/outlet boundary node at world (x, y) — the \
        typed form of dispatch_command add_boundary. An instrument Sound needs an \
        `outlet` so referencing graphs (and the renderer) can read its signal. \
        Returns the minted node id."
    )]
    async fn add_boundary(
        &self,
        Parameters(p): Parameters<AddBoundaryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddBoundary {
            port: p.port,
            x: p.x,
            y: p.y,
        })
        .await
    }

    #[tool(
        description = "Replace the automation timeline of a node's AudioParam — \
        the typed form of dispatch_command set_automation, for envelopes and \
        parameter motion (attack/decay on a voice's gain, a filter sweep, …). \
        Times are seconds from note-on (triggered instrument voices) or from \
        render start. The `events` schema lists every accepted event shape; e.g. \
        a simple AD envelope on `gain` is \
        [{\"event\":\"set_value\",\"args\":{\"value\":0.0,\"time\":0.0}}, \
        {\"event\":\"linear_ramp\",\"args\":{\"value\":1.0,\"time\":0.01}}, \
        {\"event\":\"exponential_ramp\",\"args\":{\"value\":0.001,\"time\":0.4}}]."
    )]
    async fn set_automation(
        &self,
        Parameters(p): Parameters<SetAutomationParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetAutomation {
            id: p.node,
            param: p.param,
            events: p.events,
        })
        .await
    }

    #[tool(
        description = "Render a Sound (`sample`) offline and store it as that \
        sample's bounce (so it can be dropped into an arrangement). Sequenced \
        Sounds render their loop + a release tail but STORE exactly the loop \
        (tails fold onto the loop start so clips tile seamlessly) — the result \
        reports both rendered_duration_secs and stored_duration_secs. Pass \
        `duration_secs` to instead render and store an exact unfolded span (e.g. \
        a fixed window of a procedural / worklet / buffer source). Blocks until \
        the render lands or a ~30 s safety timeout (no manual polling), then \
        returns {stored_duration_secs, rendered_duration_secs, peak, rms, \
        clipping} — so you see immediately whether it clipped (peak > 1.0) \
        without a separate wav_stats call. On timeout it returns the last \
        status; re-check get_bounce_status / list_assets."
    )]
    async fn bounce(
        &self,
        Parameters(p): Parameters<BounceParams>,
    ) -> Result<CallToolResult, McpError> {
        use std::time::{Duration, Instant};
        // The plan, read up-front: what the un-overridden render will run
        // (loop + tail for sequences), so the result can report rendered vs
        // stored without the agent reconciling them (they legitimately differ).
        let rendered_duration = match p.duration_secs {
            Some(d) => d,
            None => match self
                .query_result(EditorQuery::RenderPlan {
                    sample: Some(p.sample),
                })
                .await?
            {
                QueryResult::RenderPlan(plan) => plan.duration_secs,
                other => return Err(unexpected_query(other)),
            },
        };
        // Kick off the (async) offline render.
        self.dispatch(EditorCommand::Bounce {
            sample: p.sample,
            duration_secs: p.duration_secs,
        })
        .await?;
        // Block until it lands instead of making the agent blind-poll: a short
        // render usually completes within a couple of ticks; the timeout bounds a
        // long or failed render so the tool can't hang.
        let timeout = Duration::from_secs(30);
        let start = Instant::now();
        loop {
            let status = match self
                .query_result(EditorQuery::BounceStatus { sample: p.sample })
                .await?
            {
                QueryResult::BounceStatus(s) => s,
                other => return Err(unexpected_query(other)),
            };
            if status == "clean" {
                // Fold the verification into the bounce result: report the
                // bounced asset's stats (and whether it clips) so the caller
                // needn't a separate wav_stats round-trip.
                let stats = match self
                    .query_result(EditorQuery::WavStats {
                        sample: Some(p.sample),
                        bounced: true,
                        duration_secs: None,
                    })
                    .await?
                {
                    QueryResult::WavStats(s) => s,
                    other => return Err(unexpected_query(other)),
                };
                return Ok(text(
                    serde_json::json!({
                        "status": "clean",
                        "sample": p.sample,
                        // What plays from arrangements (the stored asset)…
                        "stored_duration_secs": stats.duration_secs,
                        // …vs what the offline render ran (loop + tail for a
                        // sequence; identical when duration_secs was passed).
                        "rendered_duration_secs": rendered_duration,
                        "duration_secs": stats.duration_secs,
                        "peak": stats.peak,
                        "rms": stats.rms,
                        "clipping": stats.clipping,
                    })
                    .to_string(),
                ));
            }
            // Fail fast instead of waiting out the timeout when the render crashed
            // (e.g. an offline-unsupported node). The status names the offender.
            if status.starts_with("failed") {
                return Err(McpError::internal_error(
                    format!("bounce {status} for {}", p.sample),
                    None,
                ));
            }
            if start.elapsed() >= timeout {
                return Ok(text(format!(
                    "bounce still '{status}' after {}s — the render may still be \
                     running or have failed; re-check get_bounce_status / list_assets.",
                    timeout.as_secs()
                )));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    #[tool(
        description = "Mark a sample as the project root (the one that plays / \
        exports)."
    )]
    async fn set_root(
        &self,
        Parameters(p): Parameters<SampleReq>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetRoot { id: p.sample }).await
    }

    #[tool(
        description = "Switch the active editing canvas to `sample`, so subsequent \
        edits/queries (add_node, connect, get_snapshot, …) operate on its graph. \
        Use this to author a sub-sample (e.g. an instrument Sound): switch to it, \
        build its graph, then switch back to wire it up."
    )]
    async fn set_active_sample(
        &self,
        Parameters(p): Parameters<SampleReq>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .req(Request::SetActiveSample { sample: p.sample })
            .await?
        {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Set or clear the active Arrangement's loop/export markers \
        (seconds). When both are set (end > start), playback loops that region and \
        export (render_wav on the arrangement) renders exactly it; omit both to \
        clear. Switch to the arrangement with set_active_sample first."
    )]
    async fn set_arrangement_markers(
        &self,
        Parameters(p): Parameters<MarkersParams>,
    ) -> Result<CallToolResult, McpError> {
        // Resolve bar forms to seconds (querying BPM only if a bar form is used).
        let needs_bpm = p.start_bars.is_some() || p.end_bars.is_some();
        let secs_per_bar = if needs_bpm {
            p.beats_per_bar.unwrap_or(4.0) * 60.0 / self.arrangement_bpm().await?
        } else {
            0.0
        };
        let start = p.start.or(p.start_bars.map(|b| b * secs_per_bar));
        let end = p.end.or(p.end_bars.map(|b| b * secs_per_bar));
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::SetMarkers { start, end },
        })
        .await
    }

    #[tool(
        description = "Replace the active arrangement's named timeline sections \
        (\"intro\", \"main\", \"outro\" — start/end in seconds). Annotation \
        metadata for navigation while arranging; playback never interprets it. \
        Read back via get_arrangement. Empty clears."
    )]
    async fn set_arrangement_sections(
        &self,
        Parameters(p): Parameters<SectionsParams>,
    ) -> Result<CallToolResult, McpError> {
        for s in &p.sections {
            if s.end <= s.start {
                return Err(McpError::invalid_params(
                    format!(
                        "section \"{}\": end ({}) must be > start ({})",
                        s.name, s.end, s.start
                    ),
                    None,
                ));
            }
        }
        self.arrange(ArrangeOp::SetSections {
            sections: p.sections,
        })
        .await
    }

    // ── arrangement editing (dedicated wrappers over EditArrange) ────────────
    // These edit the *active* sample, which must be an Arrangement — switch to
    // one with set_active_sample (or create_arrangement) first.

    #[tool(description = "Create a new Arrangement sample and make it active — \
        optionally with its BPM, timeline length, and named tracks in the same \
        call (one round-trip instead of one per track). Returns the minted id. \
        Build it further with add_clip / add_clips.")]
    async fn create_arrangement(
        &self,
        Parameters(p): Parameters<CreateArrangementParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = self
            .dispatch_created(EditorCommand::AddSample {
                kind: SampleKind::Arrangement,
            })
            .await?
            .ok_or_else(|| McpError::internal_error("create returned no id", None))?;
        // The new arrangement is active, so the EditArrange setup ops target it.
        let mut cmds: Vec<EditorCommand> = Vec::new();
        if let Some(bpm) = p.bpm {
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::SetBpm(bpm),
            });
        }
        if let Some(len) = p.length_secs {
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::SetLengthSecs(len),
            });
        }
        let tracks = p.tracks.unwrap_or_default();
        for (i, name) in tracks.iter().enumerate() {
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::AddTrack,
            });
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::SetTrackName {
                    track: i,
                    name: name.clone(),
                },
            });
        }
        if !cmds.is_empty() {
            match self.req(Request::DispatchBatch(cmds)).await? {
                Response::Batch(_) | Response::Ok => {}
                Response::Err(e) => return Err(McpError::internal_error(e, None)),
                other => return Err(unexpected(other)),
            }
        }
        Ok(text(
            serde_json::json!({ "ok": true, "id": id, "tracks": tracks }).to_string(),
        ))
    }

    #[tool(description = "Append an empty track to the active arrangement.")]
    async fn add_arrangement_track(&self) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::AddTrack).await
    }

    #[tool(
        description = "Append several named tracks to the active arrangement in \
        ONE round-trip (each is an add + rename) — use this instead of repeated \
        add_arrangement_track + set_track_name calls. Returns the new tracks' \
        indices."
    )]
    async fn add_arrangement_tracks(
        &self,
        Parameters(p): Parameters<AddTracksParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.names.is_empty() {
            return Err(McpError::invalid_params("names must be non-empty", None));
        }
        // New tracks land after the existing ones — read the current count so
        // the rename ops target the right indices.
        let base = match self.query_result(EditorQuery::Arrangement).await? {
            QueryResult::Arrangement(Some(a)) => a.tracks.len(),
            QueryResult::Arrangement(None) => {
                return Err(McpError::invalid_params(
                    "active sample is not an arrangement — set_active_sample to one first",
                    None,
                ));
            }
            other => return Err(unexpected_query(other)),
        };
        let mut cmds: Vec<EditorCommand> = Vec::new();
        for (i, name) in p.names.iter().enumerate() {
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::AddTrack,
            });
            cmds.push(EditorCommand::EditArrange {
                op: ArrangeOp::SetTrackName {
                    track: base + i,
                    name: name.clone(),
                },
            });
        }
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Batch(_) | Response::Ok => {}
            Response::Err(e) => return Err(McpError::internal_error(e, None)),
            other => return Err(unexpected(other)),
        }
        let added: Vec<Value> = p
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| serde_json::json!({ "track": base + i, "name": n }))
            .collect();
        Ok(text(serde_json::json!({ "added": added }).to_string()))
    }

    #[tool(
        description = "Set several tracks' gains on the active arrangement in ONE \
        round-trip — the batch form of set_track_gain, for mix passes."
    )]
    async fn set_track_gains(
        &self,
        Parameters(p): Parameters<TrackGainsParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.gains.is_empty() {
            return Err(McpError::invalid_params("gains must be non-empty", None));
        }
        let cmds: Vec<EditorCommand> = p
            .gains
            .iter()
            .map(|g| EditorCommand::EditArrange {
                op: ArrangeOp::SetTrackGain {
                    track: g.track,
                    gain: g.gain,
                },
            })
            .collect();
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Batch(_) | Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(description = "Remove a track (by 0-based index) from the active arrangement.")]
    async fn remove_arrangement_track(
        &self,
        Parameters(p): Parameters<TrackArg>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::RemoveTrack { track: p.track })
            .await
    }

    #[tool(description = "Rename a track in the active arrangement.")]
    async fn set_track_name(
        &self,
        Parameters(p): Parameters<TrackNameParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::SetTrackName {
            track: p.track,
            name: p.name,
        })
        .await
    }

    #[tool(description = "Set a track's linear gain in the active arrangement. \
        Linear amplitude: 1.0 = unity, 0.5 ≈ −6 dB, 2.0 = +6 dB. The UI knob/slider \
        spans 0.0–2.0, so unity (1.0) sits at mid-travel by design — that's not an \
        under-turned knob.")]
    async fn set_track_gain(
        &self,
        Parameters(p): Parameters<TrackGainParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::SetTrackGain {
            track: p.track,
            gain: p.gain,
        })
        .await
    }

    #[tool(
        description = "Place a bounced Sound as a clip on a track. Give the start \
        as `start` (seconds), `start_beats`, or `start_bars` — the beat/bar forms \
        use the arrangement's BPM directly, so no hand-conversion. `source` must be \
        bounced. `length` defaults to the full bounce duration. For placing the same \
        loop at many positions, use add_clips."
    )]
    async fn add_clip(
        &self,
        Parameters(p): Parameters<AddClipParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_source_bounced(p.source).await?;
        let start = self
            .resolve_start_secs(p.start, p.start_beats, p.start_bars, p.beats_per_bar)
            .await?;
        self.arrange(ArrangeOp::AddClip {
            track: p.track,
            start,
            source: p.source,
            length: p.length,
        })
        .await
    }

    #[tool(
        description = "Place a bounced Sound on a track at MANY positions in one \
        call — the section-builder for arrangement-first workflows (drums in the \
        drop, keys out of the breakdown, …). Give positions as `starts` (seconds), \
        `starts_beats`, `starts_bars`, or a `sections` bar-range string like \
        \"3-12, 15-20\" (each range tiles a 1-bar loop over [start,end)). Bar/beat \
        forms use the arrangement BPM. One undo. `length` defaults to the full \
        bounce duration per clip."
    )]
    async fn add_clips(
        &self,
        Parameters(p): Parameters<AddClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        self.ensure_source_bounced(p.source).await?;
        let bpb = p.beats_per_bar.unwrap_or(4.0);
        // Collect start positions (seconds) from whichever form was given.
        let mut starts: Vec<f64> = Vec::new();
        if let Some(s) = &p.starts {
            starts.extend(s.iter().copied());
        }
        if p.starts_beats.is_some() || p.starts_bars.is_some() || p.sections.is_some() {
            let bpm = self.arrangement_bpm().await?;
            let per_beat = 60.0 / bpm;
            if let Some(b) = &p.starts_beats {
                starts.extend(b.iter().map(|x| x * per_beat));
            }
            if let Some(b) = &p.starts_bars {
                starts.extend(b.iter().map(|x| x * bpb * per_beat));
            }
            if let Some(spec) = &p.sections {
                for bar in parse_sections(spec)? {
                    starts.push(bar * bpb * per_beat);
                }
            }
        }
        if starts.is_empty() {
            return Err(McpError::invalid_params(
                "no positions — give starts / starts_beats / starts_bars / sections",
                None,
            ));
        }
        let n = starts.len();
        let cmds: Vec<EditorCommand> = starts
            .into_iter()
            .map(|start| EditorCommand::EditArrange {
                op: ArrangeOp::AddClip {
                    track: p.track,
                    start,
                    source: p.source,
                    length: p.length,
                },
            })
            .collect();
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Batch(_) | Response::Ok => {
                Ok(text(format!("placed {n} clip(s) on track {}", p.track)))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Remove a clip (0-based `clip` index) from a track in the \
        active arrangement."
    )]
    async fn remove_clip(
        &self,
        Parameters(p): Parameters<ClipRef>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::RemoveClip {
            track: p.track,
            clip: p.clip,
        })
        .await
    }

    #[tool(description = "Set a clip's linear gain in the active arrangement. \
        Linear amplitude: 1.0 = unity, 0.5 ≈ −6 dB, 2.0 = +6 dB. The UI knob/slider \
        spans 0.0–2.0, so unity (1.0) sits at mid-travel by design.")]
    async fn set_clip_gain(
        &self,
        Parameters(p): Parameters<ClipGainParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::SetClipGain {
            track: p.track,
            clip: p.clip,
            gain: p.gain,
        })
        .await
    }

    #[tool(description = "Set the active arrangement's tempo (BPM). A new \
        arrangement defaults to 120 BPM — set this to your parts' tempo BEFORE \
        using the stored BPM (e.g. get_arrangement) to compute clip placement, or \
        clips land at the wrong times.")]
    async fn set_arrangement_bpm(
        &self,
        Parameters(p): Parameters<BpmParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::SetBpm(p.bpm)).await
    }

    #[tool(description = "Set the active arrangement's timeline length (seconds).")]
    async fn set_arrangement_length(
        &self,
        Parameters(p): Parameters<LengthParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::SetLengthSecs(p.secs)).await
    }

    #[tool(
        description = "Remove every clip from every track of the active arrangement \
        (tracks stay). A one-shot reset before rebuilding."
    )]
    async fn clear_arrangement(&self) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::Clear).await
    }

    #[tool(
        description = "Duplicate clips along the timeline at a fixed interval — the \
        loop/section-tiling helper. Copies each clip on `track` (or every track if \
        omitted) `count` times, each offset one more interval. Give the interval as \
        `interval_secs`, `interval_beats`, or `interval_bars` (beats/bars use `bpm`, \
        defaulting to the arrangement's BPM)."
    )]
    async fn duplicate_clips(
        &self,
        Parameters(p): Parameters<DuplicateClipsParams>,
    ) -> Result<CallToolResult, McpError> {
        let arr = match self.query_result(EditorQuery::Arrangement).await? {
            QueryResult::Arrangement(Some(a)) => a,
            QueryResult::Arrangement(None) => {
                return Err(McpError::invalid_params(
                    "active sample is not an arrangement — set_active_sample to one first",
                    None,
                ));
            }
            other => return Err(unexpected_query(other)),
        };
        let bpm = p.bpm.unwrap_or(arr.bpm);
        let interval = match (p.interval_secs, p.interval_beats, p.interval_bars) {
            (Some(s), _, _) => s,
            (None, Some(b), _) if bpm > 0.0 => b * 60.0 / bpm,
            (None, None, Some(bars)) if bpm > 0.0 => {
                bars * p.beats_per_bar.unwrap_or(4.0) * 60.0 / bpm
            }
            _ => {
                return Err(McpError::invalid_params(
                    "need interval_secs, or interval_beats/interval_bars with a positive bpm",
                    None,
                ));
            }
        };
        if interval <= 0.0 || p.count == 0 {
            return Err(McpError::invalid_params(
                "interval must be > 0 and count >= 1",
                None,
            ));
        }
        let mut placed: Vec<PlacedClip> = Vec::new();
        for (ti, track) in arr.tracks.iter().enumerate() {
            if let Some(only) = p.track {
                if only != ti {
                    continue;
                }
            }
            for clip in &track.clips {
                for k in 1..=p.count {
                    let mut c = clip.clone();
                    c.start += interval * k as f64;
                    placed.push(PlacedClip { track: ti, clip: c });
                }
            }
        }
        if placed.is_empty() {
            return Ok(text("no clips to duplicate"));
        }
        let n = placed.len();
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::PasteClips { clips: placed },
        })
        .await?;
        Ok(text(format!(
            "duplicated {n} clip placement(s): interval {interval:.4}s × {} cop{}",
            p.count,
            if p.count == 1 { "y" } else { "ies" }
        )))
    }

    // ── beat/bar time math ───────────────────────────────────────────────────

    #[tool(
        description = "Convert beats and/or bars to seconds at a given BPM, so you \
        never hand-compute clip start/length. secs = (beats + bars*beats_per_bar) * \
        60 / bpm. beats_per_bar defaults to 4. Pass the BPM your parts were authored \
        at — `bpm` is explicit here precisely so placement never silently uses the \
        wrong tempo. The active arrangement's stored BPM (in get_arrangement) \
        defaults to 120 until you set_arrangement_bpm."
    )]
    async fn beats_to_secs(
        &self,
        Parameters(p): Parameters<BeatsParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.bpm <= 0.0 {
            return Err(McpError::invalid_params("bpm must be > 0", None));
        }
        let beats_per_bar = p.beats_per_bar.unwrap_or(4.0);
        let total_beats = p.beats.unwrap_or(0.0) + p.bars.unwrap_or(0.0) * beats_per_bar;
        let secs = total_beats * 60.0 / p.bpm;
        Ok(text(
            serde_json::json!({
                "bpm": p.bpm,
                "beats_per_bar": beats_per_bar,
                "total_beats": total_beats,
                "secs": secs,
            })
            .to_string(),
        ))
    }

    // ── samples & assets ─────────────────────────────────────────────────────

    #[tool(
        description = "Rename a sample (Sound or Arrangement) — fixes auto names \
        like \"sound 11\"."
    )]
    async fn rename_sample(
        &self,
        Parameters(p): Parameters<RenameSampleParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::RenameSample {
            id: p.sample,
            name: p.name,
        })
        .await
    }

    #[tool(description = "Set a sample's free-form working notes — annotation \
        metadata for experiments (\"impact variant\", \"keeper\", \
        \"needs shorter tail\") without encoding state in the name. Shown in \
        list_samples; saved with the project; never interpreted by playback. \
        Empty clears.")]
    async fn set_sample_notes(
        &self,
        Parameters(p): Parameters<NotesParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetSampleNotes {
            id: p.sample,
            notes: p.notes,
        })
        .await
    }

    #[tool(
        description = "Duplicate a sample — graph, trigger, arrangement, bounce, \
        everything — under a new id, and make the copy active. Fork a patch to \
        try a variant, compare with compare_samples, keep the better one. \
        Returns the new id."
    )]
    async fn duplicate_sample(
        &self,
        Parameters(p): Parameters<DuplicateSampleParams>,
    ) -> Result<CallToolResult, McpError> {
        let id = self
            .dispatch_created(EditorCommand::CloneSample { id: p.sample })
            .await?
            .ok_or_else(|| {
                McpError::invalid_params(format!("no sample {} to duplicate", p.sample), None)
            })?;
        if let Some(name) = &p.name {
            let new_id: SampleId = id
                .parse()
                .map_err(|e| McpError::internal_error(format!("bad minted id: {e}"), None))?;
            self.dispatch(EditorCommand::RenameSample {
                id: new_id,
                name: name.clone(),
            })
            .await?;
        }
        Ok(text(
            serde_json::json!({ "ok": true, "id": id, "name": p.name }).to_string(),
        ))
    }

    #[tool(
        description = "Render two samples and return their stats side by side \
        plus deltas (peak/RMS in dB, durations) — A/B a fork made with \
        duplicate_sample, or any two Sounds. Pass duration_secs so both sides \
        measure the same window; bounced=true compares the stored bounces \
        instead of the live graphs. Purely mechanical numbers — which one is \
        'better' is your call."
    )]
    async fn compare_samples(
        &self,
        Parameters(p): Parameters<CompareSamplesParams>,
    ) -> Result<CallToolResult, McpError> {
        let stats_of = |sample: SampleId| {
            self.query_result(EditorQuery::WavStats {
                sample: Some(sample),
                bounced: p.bounced,
                duration_secs: p.duration_secs,
            })
        };
        let a = match stats_of(p.a).await? {
            QueryResult::WavStats(s) => s,
            other => return Err(unexpected_query(other)),
        };
        let b = match stats_of(p.b).await? {
            QueryResult::WavStats(s) => s,
            other => return Err(unexpected_query(other)),
        };
        let db = |x: f32, y: f32| -> Option<f32> {
            (x > 0.0 && y > 0.0).then(|| 20.0 * (x / y).log10())
        };
        Ok(text(
            serde_json::json!({
                "a": { "sample": p.a, "stats": a },
                "b": { "sample": p.b, "stats": b },
                "delta": {
                    "peak_db_a_minus_b": db(a.peak, b.peak),
                    "rms_db_a_minus_b": db(a.rms, b.rms),
                    "duration_secs_a_minus_b": a.duration_secs - b.duration_secs,
                },
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "Sweep one numeric field across several values: for each, \
        set it, render the ACTIVE sample, and return that value's wav stats — \
        then restore the original value. Mechanical exploration support (which \
        cutoff/feedback/rate does what); judging the results is your call. \
        Returns `[{value, stats}]`. Long sweeps render once per value — keep \
        `values` short and `duration_secs` small."
    )]
    async fn parameter_sweep(
        &self,
        Parameters(p): Parameters<SweepParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.values.is_empty() {
            return Err(McpError::invalid_params("values must be non-empty", None));
        }
        if p.values.len() > 16 {
            return Err(McpError::invalid_params(
                "at most 16 values per sweep (each is a full offline render)",
                None,
            ));
        }
        // The active sample is what renders; the node must live on its canvas.
        let active = match self.query_result(EditorQuery::Samples).await? {
            QueryResult::Samples(samples) => samples
                .iter()
                .find(|s| s.is_active)
                .map(|s| s.id)
                .ok_or_else(|| McpError::internal_error("no active sample reported", None))?,
            other => return Err(unexpected_query(other)),
        };
        // Remember the original value so the sweep is non-destructive.
        let original = match self
            .query_result(EditorQuery::NodeFields { node: p.node })
            .await?
        {
            QueryResult::NodeFields(fields) => fields
                .iter()
                .find(|f| f.key == p.key)
                .and_then(|f| f.value_num),
            other => return Err(unexpected_query(other)),
        };
        let set = |value: f64| {
            self.req(Request::Dispatch(EditorCommand::SetField {
                id: p.node,
                key: p.key.clone(),
                value: FieldValue::Num(value),
            }))
        };
        let mut points: Vec<Value> = Vec::with_capacity(p.values.len());
        for &value in &p.values {
            set(value).await?;
            let stats = match self
                .query_result(EditorQuery::WavStats {
                    sample: Some(active),
                    bounced: false,
                    duration_secs: p.duration_secs,
                })
                .await?
            {
                QueryResult::WavStats(s) => s,
                other => return Err(unexpected_query(other)),
            };
            points.push(serde_json::json!({ "value": value, "stats": stats }));
        }
        if let Some(orig) = original {
            set(orig).await?;
        }
        Ok(text(
            serde_json::json!({
                "node": p.node,
                "key": p.key,
                "restored_to": original,
                "points": points,
            })
            .to_string(),
        ))
    }

    #[tool(description = "Export a sample as a portable patch: a self-contained \
        SampleLibrary TOML holding the sample, every sub-sample it references, \
        and the assets they use (WASM modules, audio buffers, bounces) — \
        experiments are valuable as patches, not just rendered audio. Re-open \
        with import_sample (any project). A directory path gets \
        `<sample-name>.toml` appended.")]
    async fn export_sample(
        &self,
        Parameters(p): Parameters<ExportSampleParams>,
    ) -> Result<CallToolResult, McpError> {
        use awsm_audio_editor_protocol::schema::SampleLibrary;
        let project = match self.query_result(EditorQuery::Project).await? {
            QueryResult::Project(proj) => proj,
            other => return Err(unexpected_query(other)),
        };
        let lib = project.library;
        // Transitive closure over Sample-ref nodes, starting at the target.
        let mut keep: Vec<SampleId> = vec![p.sample];
        let mut i = 0;
        while i < keep.len() {
            let id = keep[i];
            i += 1;
            let Some(sample) = lib.samples.iter().find(|s| s.id == id) else {
                return Err(McpError::invalid_params(format!("no sample {id}"), None));
            };
            for node in &sample.graph.nodes {
                if let NodeKind::Sample(sr) = &node.kind {
                    if !keep.contains(&sr.sample) {
                        keep.push(sr.sample);
                    }
                }
            }
        }
        let samples: Vec<_> = lib
            .samples
            .iter()
            .filter(|s| keep.contains(&s.id))
            .cloned()
            .collect();
        // Prune the asset table to what the kept samples actually reference.
        let mut out = SampleLibrary {
            root: Some(p.sample),
            samples,
            ..Default::default()
        };
        for sample in &out.samples.clone() {
            if let Some(b) = &sample.bounce {
                if let Some(a) = lib.assets.buffers.iter().find(|a| a.id == b.asset) {
                    if !out.assets.buffers.iter().any(|x| x.id == a.id) {
                        out.assets.buffers.push(a.clone());
                    }
                }
            }
            for node in &sample.graph.nodes {
                let buffer = match &node.kind {
                    NodeKind::AudioBufferSource(b) => b.buffer,
                    NodeKind::Convolver(c) => c.buffer,
                    NodeKind::AudioWorklet(w) => {
                        if let Some(a) = w
                            .module
                            .and_then(|m| lib.assets.wasm_modules.iter().find(|x| x.id == m))
                        {
                            if !out.assets.wasm_modules.iter().any(|x| x.id == a.id) {
                                out.assets.wasm_modules.push(a.clone());
                            }
                        }
                        None
                    }
                    _ => None,
                };
                if let Some(a) = buffer.and_then(|b| lib.assets.buffers.iter().find(|x| x.id == b))
                {
                    if !out.assets.buffers.iter().any(|x| x.id == a.id) {
                        out.assets.buffers.push(a.clone());
                    }
                }
            }
        }
        let name = out
            .samples
            .iter()
            .find(|s| s.id == p.sample)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "sample".to_string());
        let toml_text = toml::to_string_pretty(&out)
            .map_err(|e| McpError::internal_error(format!("encode TOML: {e}"), None))?;
        let mut dest = std::path::PathBuf::from(&p.path);
        if p.path.ends_with('/') || dest.is_dir() {
            let safe: String = name
                .chars()
                .map(|c| if c == '/' || c == '\\' { '_' } else { c })
                .collect();
            dest.push(format!("{safe}.toml"));
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    McpError::internal_error(format!("create {}: {e}", parent.display()), None)
                })?;
            }
        }
        std::fs::write(&dest, &toml_text).map_err(|e| {
            McpError::internal_error(format!("write {}: {e}", dest.display()), None)
        })?;
        Ok(text(
            serde_json::json!({
                "ok": true,
                "path": dest.display().to_string(),
                "samples": out.samples.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
                "buffers": out.assets.buffers.len(),
                "wasm_modules": out.assets.wasm_modules.len(),
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "Import a patch written by export_sample (a SampleLibrary \
        TOML): merges its samples and embedded assets into the open project. \
        Samples whose ids already exist are rejected (import once, then fork \
        with duplicate_sample). Returns the imported samples."
    )]
    async fn import_sample(
        &self,
        Parameters(p): Parameters<ImportSampleParams>,
    ) -> Result<CallToolResult, McpError> {
        let library_toml = std::fs::read_to_string(&p.path)
            .map_err(|e| McpError::invalid_params(format!("read {}: {e}", p.path), None))?;
        match self.req(Request::ImportSamples { library_toml }).await? {
            Response::Imported(samples) => Ok(text(
                serde_json::json!({
                    "ok": true,
                    "imported": samples
                        .iter()
                        .map(|s| serde_json::json!({ "id": s.id, "name": s.name, "kind": s.kind }))
                        .collect::<Vec<_>>(),
                })
                .to_string(),
            )),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Render a Sound/Arrangement offline and write the `.wav` to \
        a path you choose — the durable form of render_wav (which writes a temp \
        file). Omit `sample` to export the project root. A directory path gets \
        `<sample-name>.wav` appended. Returns the written path + stats."
    )]
    async fn export_wav(
        &self,
        Parameters(p): Parameters<ExportWavParams>,
    ) -> Result<CallToolResult, McpError> {
        // Render first (same path as render_wav), then copy the temp file out.
        let handle = match self
            .req(Request::RenderWav {
                sample: p.sample,
                sample_rate: p.sample_rate,
                duration_secs: p.duration_secs,
                trim_silence: p.trim_silence,
            })
            .await?
        {
            Response::Render(h) => h,
            Response::Err(e) => return Err(McpError::internal_error(e, None)),
            other => return Err(unexpected(other)),
        };
        let src = crate::http::render_path(&handle.render_id);
        let mut dest = std::path::PathBuf::from(&p.path);
        if p.path.ends_with('/') || dest.is_dir() {
            // Default the filename to the sample's name (or "root").
            let name = match self.query_result(EditorQuery::Samples).await? {
                QueryResult::Samples(samples) => samples
                    .iter()
                    .find(|s| match p.sample {
                        Some(id) => s.id == id,
                        None => s.is_root,
                    })
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "root".to_string()),
                other => return Err(unexpected_query(other)),
            };
            // A touch of filename hygiene; keep it permissive.
            let safe: String = name
                .chars()
                .map(|c| if c == '/' || c == '\\' { '_' } else { c })
                .collect();
            dest.push(format!("{safe}.wav"));
        }
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    McpError::internal_error(format!("create {}: {e}", parent.display()), None)
                })?;
            }
        }
        std::fs::copy(&src, &dest).map_err(|e| {
            McpError::internal_error(
                format!("write {} → {}: {e}", src.display(), dest.display()),
                None,
            )
        })?;
        Ok(text(
            serde_json::json!({
                "ok": true,
                "path": dest.display().to_string(),
                "byte_len": handle.byte_len,
                "duration_secs": handle.duration_secs,
                "peak": handle.peak,
                "rms": handle.rms,
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "Re-bounce every Sound whose bounce is missing or stale (not \
        clean), in one batch. Renders are async — re-query list_samples / list_assets \
        to confirm they all went clean."
    )]
    async fn bounce_all_dirty(&self) -> Result<CallToolResult, McpError> {
        let assets = match self.query_result(EditorQuery::Assets).await? {
            QueryResult::Assets(a) => a,
            other => return Err(unexpected_query(other)),
        };
        let stale: Vec<&AssetInfo> = assets.iter().filter(|a| a.bounce != "clean").collect();
        if stale.is_empty() {
            return Ok(text("all bounces are clean — nothing to do"));
        }
        let names: Vec<String> = stale
            .iter()
            .map(|a| format!("{} ({})", a.name, a.bounce))
            .collect();
        let cmds: Vec<EditorCommand> = stale
            .iter()
            .map(|a| EditorCommand::Bounce {
                sample: a.id,
                duration_secs: None,
            })
            .collect();
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Batch(_) | Response::Ok => Ok(text(format!(
                "started bouncing {} sound(s): {}",
                names.len(),
                names.join(", ")
            ))),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Per-track peak/rms of the active arrangement, each track \
        rendered SOLO (in isolation) over the effective window — so you can see \
        which stem is hot and fix that one track's gain instead of rescaling \
        everything. Returns `[{track, name, peak, rms, clips}]` (peak > 1.0 means \
        that stem clips on its own). Pair with the master render's wav_stats to \
        balance a mix."
    )]
    async fn arrangement_track_stats(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::ArrangementTrackStats).await
    }

    #[tool(description = "One-call pre-export check of the active arrangement: \
        combines the bounce report (stale/missing clip sources), per-track solo \
        peaks/RMS, the master render's stats, the marker window, and clip \
        overruns (clips extending past the timeline) — plus mechanical \
        `recommendations` (e.g. 'master peak 1.02 clips: reduce track gains', \
        'bounce 2 stale sources'). Run it before export_wav / render_wav instead \
        of stitching arrangement_bounce_report + arrangement_track_stats + \
        wav_stats yourself.")]
    async fn verify_arrangement(&self) -> Result<CallToolResult, McpError> {
        let arr = match self.query_result(EditorQuery::Arrangement).await? {
            QueryResult::Arrangement(Some(a)) => a,
            QueryResult::Arrangement(None) => {
                return Err(McpError::invalid_params(
                    "active sample is not an arrangement — set_active_sample to one first",
                    None,
                ));
            }
            other => return Err(unexpected_query(other)),
        };
        // The active arrangement's id (the master render targets it).
        let (arr_id, arr_name) = match self.query_result(EditorQuery::Samples).await? {
            QueryResult::Samples(samples) => {
                let active = samples
                    .iter()
                    .find(|s| s.is_active)
                    .ok_or_else(|| McpError::internal_error("no active sample reported", None))?;
                (active.id, active.name.clone())
            }
            other => return Err(unexpected_query(other)),
        };
        // Clip source bounce statuses (the bounce-report logic).
        let assets = match self.query_result(EditorQuery::Assets).await? {
            QueryResult::Assets(a) => a,
            other => return Err(unexpected_query(other)),
        };
        let (mut stale, mut missing) = (0u32, 0u32);
        let mut clip_problems: Vec<Value> = Vec::new();
        for (ti, track) in arr.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                let bounce = assets
                    .iter()
                    .find(|a| a.id == clip.source)
                    .map(|a| a.bounce.as_str())
                    .unwrap_or("missing");
                match bounce {
                    "clean" => {}
                    "stale" => {
                        stale += 1;
                        clip_problems.push(serde_json::json!({
                            "track": ti, "clip": ci, "source": clip.source,
                            "problem": "stale bounce",
                        }));
                    }
                    _ => {
                        missing += 1;
                        clip_problems.push(serde_json::json!({
                            "track": ti, "clip": ci, "source": clip.source,
                            "problem": "missing bounce",
                        }));
                    }
                }
                let end = clip.start + clip.length;
                if end > arr.length_secs + 1e-6 {
                    clip_problems.push(serde_json::json!({
                        "track": ti, "clip": ci, "source": clip.source,
                        "problem": format!(
                            "overruns the timeline: ends at {end:.3}s, length is {:.3}s",
                            arr.length_secs
                        ),
                    }));
                }
            }
        }
        // Per-track solo stats + the master render.
        let track_stats = match self
            .query_result(EditorQuery::ArrangementTrackStats)
            .await?
        {
            QueryResult::ArrangementTrackStats(t) => t,
            other => return Err(unexpected_query(other)),
        };
        let master = match self
            .query_result(EditorQuery::WavStats {
                sample: Some(arr_id),
                bounced: false,
                duration_secs: None,
            })
            .await?
        {
            QueryResult::WavStats(s) => s,
            other => return Err(unexpected_query(other)),
        };
        // Mechanical recommendations — level / bookkeeping facts only.
        let mut recommendations: Vec<String> = Vec::new();
        if master.clipping {
            recommendations.push(format!(
                "master peak {:.3} clips (>1.0): reduce track gains (set_track_gains)",
                master.peak
            ));
        } else if master.peak > 0.98 {
            recommendations.push(format!(
                "master peak {:.3} is within 0.02 of clipping: consider a little headroom",
                master.peak
            ));
        }
        for t in &track_stats {
            if t.peak > 1.0 {
                recommendations.push(format!(
                    "track {} ({}) clips solo (peak {:.3}): lower its gain or its clips'",
                    t.track, t.name, t.peak
                ));
            }
        }
        if stale > 0 {
            recommendations.push(format!(
                "{stale} clip(s) play a stale bounce: bounce_all_dirty re-renders them"
            ));
        }
        if missing > 0 {
            recommendations.push(format!(
                "{missing} clip(s) have no bounce at all (silent): bounce their sources"
            ));
        }
        if master.dc_offset.abs() > 0.01 {
            recommendations.push(format!(
                "master has a DC offset of {:.4}: check for un-zeroed constant sources",
                master.dc_offset
            ));
        }
        let ok = recommendations.is_empty() && clip_problems.is_empty();
        Ok(text(
            serde_json::json!({
                "ok": ok,
                "arrangement": {
                    "id": arr_id,
                    "name": arr_name,
                    "bpm": arr.bpm,
                    "length_secs": arr.length_secs,
                    "markers": { "start": arr.loop_start, "end": arr.loop_end },
                    "tracks": arr.tracks.len(),
                },
                "master": master,
                "tracks": track_stats,
                "clip_problems": clip_problems,
                "bounce_summary": { "stale": stale, "missing": missing },
                "recommendations": recommendations,
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "Report, per clip in the active arrangement, whether its source \
        Sound's bounce is clean / stale / missing — so you can spot clips playing an \
        out-of-date bounce before exporting. Pair with bounce_all_dirty to fix them."
    )]
    async fn arrangement_bounce_report(&self) -> Result<CallToolResult, McpError> {
        let arr = match self.query_result(EditorQuery::Arrangement).await? {
            QueryResult::Arrangement(Some(a)) => a,
            QueryResult::Arrangement(None) => {
                return Err(McpError::invalid_params(
                    "active sample is not an arrangement — set_active_sample to one first",
                    None,
                ));
            }
            other => return Err(unexpected_query(other)),
        };
        let assets = match self.query_result(EditorQuery::Assets).await? {
            QueryResult::Assets(a) => a,
            other => return Err(unexpected_query(other)),
        };
        let lookup = |id: SampleId| assets.iter().find(|a| a.id == id);
        let mut clips = Vec::new();
        let (mut clean, mut stale, mut missing) = (0u32, 0u32, 0u32);
        for (ti, track) in arr.tracks.iter().enumerate() {
            for (ci, clip) in track.clips.iter().enumerate() {
                let info = lookup(clip.source);
                let status = match info.map(|a| a.bounce.as_str()) {
                    Some("clean") => {
                        clean += 1;
                        "clean"
                    }
                    Some("stale") => {
                        stale += 1;
                        "stale"
                    }
                    // "none" (never bounced) or source no longer a bounceable Sound.
                    _ => {
                        missing += 1;
                        "missing"
                    }
                };
                clips.push(serde_json::json!({
                    "track": ti,
                    "clip": ci,
                    "source": clip.source,
                    "name": info.map(|a| a.name.clone()).unwrap_or_default(),
                    "bounce": status,
                }));
            }
        }
        Ok(text(
            serde_json::json!({
                "clips": clips,
                "summary": { "clean": clean, "stale": stale, "missing": missing },
            })
            .to_string(),
        ))
    }

    #[tool(
        description = "Return the recommended Cargo.toml for authoring an awsm-audio \
        WASM DSP worklet (the crates.io release; dependency version derived so it \
        never goes stale). Prefer `scaffold_worklet`, which emits this PLUS a \
        ready-to-build src/lib.rs. Reach for a worklet only when no built-in node \
        or combination expresses the DSP — FFT/spectral, granular, physical \
        modeling, per-sample stateful nonlinearities, or custom synthesis (see the \
        awsm-audio://docs/worklet-abi guide; API at docs.rs/awsm-audio-worklet/latest)."
    )]
    async fn worklet_cargo_toml(&self) -> Result<CallToolResult, McpError> {
        Ok(text(worklet_cargo_toml_text("my-worklet")))
    }

    #[tool(
        description = "Scaffold a ready-to-build awsm-audio WASM DSP worklet crate: \
        returns {files:{\"Cargo.toml\",\"src/lib.rs\"}, build, wasm_path, next}. The \
        lib.rs has the ABI fully wired (Processor impl, your `params` as declared \
        ParamDesc knobs, the awsm_worklet! macro) with a passthrough `process()` — \
        you only write the DSP. The Cargo.toml's dependency version is derived (never \
        stale). Write the files, run `build`, then attach_wasm the `wasm_path` onto an \
        audio_worklet node. Reach for this only for DSP no built-in expresses \
        (FFT/spectral, granular, physical modeling, per-sample state, custom synthesis)."
    )]
    async fn scaffold_worklet(
        &self,
        Parameters(p): Parameters<ScaffoldWorkletParams>,
    ) -> Result<CallToolResult, McpError> {
        // Sanitize the crate name to a valid kebab-case package name.
        let raw = p.name.unwrap_or_default();
        let name = sanitize_crate_name(&raw);
        // Params: default to a single gain knob.
        let params = p.params.unwrap_or_else(|| {
            vec![WorkletParamSpec {
                name: "gain".into(),
                min: 0.0,
                max: 2.0,
                default: 1.0,
            }]
        });
        if params.len() > 32 {
            return Err(McpError::invalid_params(
                "a worklet declares at most 32 params (MAX_PARAMS)",
                None,
            ));
        }
        let lib_name = name.replace('-', "_");
        let struct_name = kebab_to_camel(&name);
        let files = serde_json::json!({
            "Cargo.toml": worklet_cargo_toml_text(&name),
            "src/lib.rs": worklet_lib_rs(&struct_name, &params, p.no_std),
        });
        Ok(text(
            serde_json::json!({
                "files": files,
                "build": format!(
                    "cargo build -p {name} --target wasm32-unknown-unknown --release"
                ),
                "wasm_path": format!(
                    "target/wasm32-unknown-unknown/release/{lib_name}.wasm"
                ),
                "next": "write the files, run `build`, then attach_wasm { node, wasm_path } \
                         onto an audio_worklet node (add_node kind audio_worklet first)",
            })
            .to_string(),
        ))
    }

    // ── worklet authoring ────────────────────────────────────────────────────

    #[tool(
        description = "Attach a compiled WASM DSP module to an AudioWorklet node. \
        Author a crate against awsm-audio-worklet (see the awsm-audio://docs/worklet-abi \
        resource; crate API at https://docs.rs/awsm-audio-worklet/latest, Cargo.toml \
        from the worklet_cargo_toml tool), `cargo build --target \
        wasm32-unknown-unknown --release`, then pass the .wasm path here. On success \
        the node's discovered params show up in get_snapshot. A bad module returns \
        the compile/ABI error."
    )]
    async fn attach_wasm(
        &self,
        Parameters(p): Parameters<AttachWasmParams>,
    ) -> Result<CallToolResult, McpError> {
        let node = p.node;
        let wasm_base64 = match (p.wasm_base64, p.wasm_path) {
            (Some(b64), _) => b64,
            (None, Some(path)) => {
                let bytes = std::fs::read(&path)
                    .map_err(|e| McpError::internal_error(format!("read {path}: {e}"), None))?;
                STANDARD.encode(bytes)
            }
            (None, None) => {
                return Err(McpError::invalid_params(
                    "need wasm_path or wasm_base64",
                    None,
                ));
            }
        };
        let label = p.label.unwrap_or_else(|| "module".to_string());
        match self
            .req(Request::AttachWasm {
                node,
                wasm_base64,
                label: label.clone(),
            })
            .await?
        {
            Response::Ok => Ok(text(
                "ok — params discovered; call get_snapshot to see them",
            )),
            // Carry the node/label context so a failure in a multi-worklet
            // session names which attach broke (the raw editor error doesn't).
            Response::Err(e) => Err(McpError::internal_error(
                format!("attach_wasm '{label}' onto node {node}: {e}"),
                None,
            )),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Load an external audio file (WAV/mp3/flac/ogg/…) into an \
        existing AudioBufferSource (or Convolver) node's buffer. add_node an \
        audio_buffer_source first, then load_audio onto it, then connect it. \
        Provide exactly one of `path` (an agent-local file — the server hosts it \
        and the editor fetches it off the link) or `url` (a browser-reachable \
        URL). Bytes never cross the editor link. Returns the decoded duration / \
        sample-rate / channel count; render_wav / waveform to inspect it."
    )]
    async fn load_audio(
        &self,
        Parameters(p): Parameters<LoadAudioParams>,
    ) -> Result<CallToolResult, McpError> {
        let url = match (p.path, p.url) {
            (Some(_), Some(_)) => {
                return Err(McpError::invalid_params(
                    "provide exactly one of `path` or `url`, not both",
                    None,
                ));
            }
            (None, None) => {
                return Err(McpError::invalid_params("need `path` or `url`", None));
            }
            (Some(path), None) => {
                let bytes = std::fs::read(&path)
                    .map_err(|e| McpError::invalid_params(format!("read {path}: {e}"), None))?;
                let id = uuid::Uuid::new_v4().to_string();
                self.link
                    .store_asset(id.clone(), bytes, content_type_for(&path));
                format!("{}/assets/{}", self.link.self_origin(), id)
            }
            (None, Some(url)) => url,
        };
        match self
            .req(Request::LoadAudio {
                node: p.node,
                url,
                label: p.label,
            })
            .await?
        {
            Response::AudioLoaded(info) => Ok(text(format!(
                "loaded {} channel(s), {:.3}s @ {} Hz (asset {}); the node's buffer is set — \
                 connect it and render_wav / waveform to inspect.",
                info.channels, info.duration_secs, info.sample_rate, info.asset_id
            ))),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    // ── generic escape hatches ───────────────────────────────────────────────

    #[tool(description = "Dispatch any EditorCommand (escape hatch for commands \
        without a dedicated tool — sequencer/arrangement edits, etc.). The param \
        schema documents every command + its args.")]
    async fn dispatch_command(
        &self,
        Parameters(p): Parameters<CommandParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(p.command.0).await
    }

    #[tool(description = "Dispatch a list of EditorCommands in order in one \
        round-trip (applied sequentially). Cuts latency for multi-step edits.")]
    async fn dispatch_batch(
        &self,
        Parameters(p): Parameters<BatchParams>,
    ) -> Result<CallToolResult, McpError> {
        let cmds: Vec<EditorCommand> = p.commands.into_iter().map(|c| c.0).collect();
        if cmds.is_empty() {
            return Err(McpError::invalid_params(
                "dispatch_batch needs a non-empty `commands` array (e.g. commands: [{cmd, args}])",
                None,
            ));
        }
        match self.req(Request::DispatchBatch(cmds)).await? {
            // Per-command results, in order: each carries the minted id (for
            // add_node / add_sample / add_boundary / add_sample_ref) so a
            // create-then-connect flow needs no follow-up snapshot.
            Response::Batch(items) => Ok(text(
                serde_json::to_string(&items).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            )),
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    #[tool(
        description = "Dispatch a batch of EditorCommands in order with symbolic \
        id refs, so a create-then-connect flow is one tool call. Each command may \
        carry a `\"ref\":\"<name>\"` labeling the id it creates; later commands use \
        `\"$<name>\"` wherever an id is expected and it's replaced with the real \
        minted id before dispatch. Returns the `{refs:{name:id}, results:[…]}` map. \
        Use this instead of dispatch_batch when later commands reference earlier \
        nodes; use add_chain for the simple linear case."
    )]
    async fn dispatch_refs(
        &self,
        Parameters(p): Parameters<RefBatchParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.commands.is_empty() {
            return Err(McpError::invalid_params("commands must be non-empty", None));
        }
        let mut refs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut results: Vec<Value> = Vec::with_capacity(p.commands.len());
        for (i, raw) in p.commands.into_iter().enumerate() {
            // Tolerate a JSON-*string* element (some clients stringify nested
            // objects) — parse it to the command object it encodes.
            let mut raw = match raw {
                Value::String(s) => serde_json::from_str(&s).map_err(|e| {
                    schema_error(
                        format!(
                            "command {i} was a string that isn't valid JSON ({e}) — \
                             commands are objects, not strings"
                        ),
                        command_example(None),
                        Value::String(s),
                    )
                })?,
                other => other,
            };
            // Pull the optional `ref` label off the command object.
            let ref_name = raw
                .as_object_mut()
                .and_then(|o| o.remove("ref"))
                .and_then(|v| v.as_str().map(str::to_string));
            // Substitute `$name` → captured id anywhere in the command, and
            // resolve a bare kind tag in an add_node's args.
            substitute_refs(&mut raw, &refs);
            let raw = normalize_command_value(raw);
            let cmd_name = raw.get("cmd").and_then(Value::as_str).map(str::to_string);
            let cmd: EditorCommand = serde_json::from_value(raw.clone()).map_err(|e| {
                schema_error(
                    format!("command {i}: {e}"),
                    command_example(cmd_name.as_deref()),
                    raw,
                )
            })?;
            let id = self.dispatch_created(cmd).await?;
            if let (Some(name), Some(id)) = (&ref_name, &id) {
                refs.insert(name.clone(), id.clone());
            }
            results.push(serde_json::json!({ "ok": true, "ref": ref_name, "id": id }));
        }
        Ok(text(
            serde_json::json!({ "refs": refs, "results": results }).to_string(),
        ))
    }

    #[tool(
        description = "Validate command payloads WITHOUT dispatching them — a \
        dry-run for dispatch_command / dispatch_batch / dispatch_refs. Returns, \
        per command, either its normalized canonical JSON (what the editor would \
        receive, with bare kind tags expanded) or a structured error with a \
        copy-pasteable `expected` example. `\"$ref\"` placeholders are accepted \
        (substituted with a nil id for validation only). Use this to check a \
        batch's shapes in one call instead of discovering errors one field at a \
        time against the live document."
    )]
    async fn validate_command(
        &self,
        Parameters(p): Parameters<ValidateParams>,
    ) -> Result<CallToolResult, McpError> {
        if p.commands.is_empty() {
            return Err(McpError::invalid_params("commands must be non-empty", None));
        }
        // `$ref` placeholders resolve to a nil id, purely so the typed decode can
        // proceed — validation checks shapes, not document state.
        let nil_refs: std::collections::HashMap<String, String> = p
            .commands
            .iter()
            .filter_map(|c| c.get("ref").and_then(Value::as_str))
            .map(|r| {
                (
                    r.to_string(),
                    "00000000-0000-0000-0000-000000000000".to_string(),
                )
            })
            .collect();
        let mut results: Vec<Value> = Vec::with_capacity(p.commands.len());
        for (i, raw) in p.commands.into_iter().enumerate() {
            let mut raw = match raw {
                Value::String(s) => match serde_json::from_str(&s) {
                    Ok(v) => v,
                    Err(e) => {
                        results.push(serde_json::json!({
                            "index": i, "ok": false,
                            "error": format!("a string element must be JSON-encoded ({e}) — commands are objects"),
                            "expected": command_example(None),
                        }));
                        continue;
                    }
                },
                other => other,
            };
            if let Some(o) = raw.as_object_mut() {
                o.remove("ref");
            }
            substitute_refs(&mut raw, &nil_refs);
            let raw = normalize_command_value(raw);
            let cmd_name = raw.get("cmd").and_then(Value::as_str).map(str::to_string);
            match serde_json::from_value::<EditorCommand>(raw) {
                Ok(cmd) => results.push(serde_json::json!({
                    "index": i, "ok": true,
                    "normalized": serde_json::to_value(&cmd).unwrap_or(Value::Null),
                })),
                Err(e) => results.push(serde_json::json!({
                    "index": i, "ok": false,
                    "error": e.to_string(),
                    "expected": command_example(cmd_name.as_deref()),
                    "docs_uri": "awsm-audio://docs/vocabulary",
                })),
            }
        }
        Ok(text(serde_json::json!({ "results": results }).to_string()))
    }

    #[tool(
        description = "Run any EditorQuery (escape hatch for queries without a \
        dedicated tool). The param schema documents every query + its args."
    )]
    async fn run_query(
        &self,
        Parameters(p): Parameters<QueryParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(p.query.0).await
    }
}

// ──────────────────────────────── helpers ───────────────────────────────────

impl EditorMcp {
    async fn req(&self, r: Request) -> Result<Response, McpError> {
        self.link
            .request(&self.agent, &r)
            .await
            .map_err(|e| match e {
                LinkError::PairingRequired(code) => McpError::invalid_request(
                    format!(
                        "No editor is paired with this MCP session. Ask the user to open the \
                         awsm-audio editor with `?pair={code}` appended to its URL, or to enter \
                         pairing code `{code}` in the editor's MCP connect modal. (Auto-pairs \
                         when exactly one editor tab and one agent are connected.)"
                    ),
                    None,
                ),
                LinkError::Transport(msg) => McpError::internal_error(msg, None),
            })
    }

    async fn dispatch(&self, cmd: EditorCommand) -> Result<CallToolResult, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Ok => Ok(text("ok")),
            // A create command echoes its minted id, so the caller needn't
            // re-snapshot to learn it.
            Response::Created { id } => Ok(text(
                serde_json::json!({ "ok": true, "id": id }).to_string(),
            )),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Dispatch one command and return the id it minted (if it created a
    /// node/sample/boundary/sample-ref), for the chain/ref builders.
    async fn dispatch_created(&self, cmd: EditorCommand) -> Result<Option<String>, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Created { id } => Ok(Some(id)),
            Response::Ok => Ok(None),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Dispatch an [`ArrangeOp`] against the active Arrangement sample.
    async fn arrange(&self, op: ArrangeOp) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::EditArrange { op }).await
    }

    /// Resolve a sequencer output key (e.g. `"t2:n36"`) to its index in node
    /// `from`'s `outputs`, by reading the current snapshot. Errors if the node or
    /// key isn't found (listing the available keys).
    async fn resolve_output_index(&self, from: NodeId, key: &str) -> Result<u32, McpError> {
        let qr = self.query_result(EditorQuery::Snapshot).await?;
        let v = serde_json::to_value(&qr)
            .map_err(|e| McpError::internal_error(format!("encode snapshot: {e}"), None))?;
        let from = from.to_string();
        let nodes = v
            .pointer("/data/graph/nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| McpError::internal_error("snapshot had no nodes", None))?;
        let node = nodes
            .iter()
            .find(|n| n.get("id").and_then(Value::as_str) == Some(from.as_str()))
            .ok_or_else(|| McpError::invalid_params(format!("no node {from}"), None))?;
        let outputs = node
            .pointer("/kind/props/outputs")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                McpError::invalid_params(
                    format!("node {from} is not a sequencer with outputs"),
                    None,
                )
            })?;
        let mut keys = Vec::new();
        for (i, out) in outputs.iter().enumerate() {
            if let Some(k) = out.get("key").and_then(Value::as_str) {
                if k == key {
                    return Ok(i as u32);
                }
                keys.push(k.to_string());
            }
        }
        Err(McpError::invalid_params(
            format!(
                "no output key '{key}' on node {from}; available: {}",
                keys.join(", ")
            ),
            None,
        ))
    }

    /// Error if `source` has never been bounced — a clip of it would be silent.
    /// (A `stale` bounce still plays its last audio, so only `none` is rejected;
    /// `arrangement_bounce_report` flags stale clips.)
    async fn ensure_source_bounced(&self, source: SampleId) -> Result<(), McpError> {
        match self
            .query_result(EditorQuery::BounceStatus { sample: source })
            .await?
        {
            QueryResult::BounceStatus(s) if s == "none" => Err(McpError::invalid_params(
                format!(
                    "source {source} is not bounced — call bounce on it first so the clip has audio"
                ),
                None,
            )),
            QueryResult::BounceStatus(_) => Ok(()),
            other => Err(unexpected_query(other)),
        }
    }

    /// The active arrangement's BPM (for beat/bar → seconds conversion). Errors if
    /// the active sample isn't an arrangement.
    async fn arrangement_bpm(&self) -> Result<f64, McpError> {
        match self.query_result(EditorQuery::Arrangement).await? {
            QueryResult::Arrangement(Some(a)) if a.bpm > 0.0 => Ok(a.bpm),
            QueryResult::Arrangement(Some(_)) => Err(McpError::internal_error(
                "arrangement BPM is not positive",
                None,
            )),
            QueryResult::Arrangement(None) => Err(McpError::invalid_params(
                "active sample is not an arrangement — set_active_sample to one first",
                None,
            )),
            other => Err(unexpected_query(other)),
        }
    }

    /// Resolve a clip start given in seconds / beats / bars to seconds, querying
    /// the arrangement BPM only when a beat/bar form is used. Exactly one form may
    /// be set; none defaults to 0 (timeline start).
    async fn resolve_start_secs(
        &self,
        start: Option<f64>,
        beats: Option<f64>,
        bars: Option<f64>,
        beats_per_bar: Option<f64>,
    ) -> Result<f64, McpError> {
        match (start, beats, bars) {
            (Some(s), None, None) => Ok(s),
            (None, None, None) => Ok(0.0),
            (None, Some(b), None) => Ok(b * 60.0 / self.arrangement_bpm().await?),
            (None, None, Some(bar)) => {
                Ok(bar * beats_per_bar.unwrap_or(4.0) * 60.0 / self.arrangement_bpm().await?)
            }
            _ => Err(McpError::invalid_params(
                "give exactly one of start / start_beats / start_bars",
                None,
            )),
        }
    }

    /// Run a query and hand back the typed [`QueryResult`] (for tools that compose
    /// over the result rather than just relaying its JSON).
    async fn query_result(&self, q: EditorQuery) -> Result<QueryResult, McpError> {
        match self.req(Request::Query(q)).await? {
            Response::Query(qr) => Ok(*qr),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    async fn query(&self, q: EditorQuery) -> Result<CallToolResult, McpError> {
        match self.req(Request::Query(q)).await? {
            Response::Query(qr) => Ok(text(
                serde_json::to_string_pretty(&*qr)
                    .unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            )),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// RenderWav → the editor uploaded the `.wav` out-of-band (off the link); we
    /// return its on-disk path + a one-line summary. (An agent can't hear bytes;
    /// the human/tooling opens the file or fetches `/renders/<id>.wav`.)
    async fn wav(&self, r: Request) -> Result<CallToolResult, McpError> {
        match self.req(r).await? {
            Response::Render(h) => {
                let path = crate::http::render_path(&h.render_id);
                Ok(text(format!(
                    "wrote {} bytes to {} (also at /renders/{}.wav) — \
                     duration {:.3}s, peak {:.3}, rms {:.3}",
                    h.byte_len,
                    path.display(),
                    h.render_id,
                    h.duration_secs,
                    h.peak,
                    h.rms,
                )))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }
}

#[tool_handler]
impl ServerHandler for EditorMcp {
    fn get_info(&self) -> ServerInfo {
        // `ServerInfo` is `#[non_exhaustive]` in rmcp 1.x — build from Default and
        // set the public fields rather than a struct literal.
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .enable_prompts()
            .build();
        info.instructions = Some(
            "Drive the awsm-audio node-graph WebAudio editor. Start with list_samples \
             (every sample + which is root/active) and get_snapshot (the ACTIVE \
             sample's canvas — nodes + connections, each wire with a stable id for \
             disconnect); get_snapshot only shows the active canvas, so use \
             list_samples + set_active_sample to navigate a multi-sample project. \
             Mutate with the graph/sequencer/arrangement tools (or dispatch_command / \
             dispatch_batch for anything without a dedicated tool), bounce a Sound \
             and call wav_stats / waveform to inspect the result (prefer these over \
             render_wav for verification). To use an external audio sample (a \
             drum hit, vocal, field recording, impulse response), add_node an \
             audio_buffer_source (or convolver) and load_audio a local file path \
             or a URL onto it. Compose built-in nodes first; reach for an \
             audio_worklet only for DSP no built-in or combination expresses — \
             FFT/spectral, granular, physical modeling, per-sample stateful \
             nonlinearities, or custom synthesis (not chorus/phaser/distortion, \
             which are delay/all-pass/waveshaper). Use scaffold_worklet to emit a \
             ready-to-build crate, then attach_wasm the compiled .wasm (read \
             awsm-audio://docs/worklet-abi for the ABI).\n\n\
             For a song / full-track request, work arrangement-first instead \
             of one monolithic root sequencer: build and bounce each part as its own \
             short loop Sound, then create_arrangement and place clips into sections — \
             add_arrangement_track + add_clip/add_clips (start_bars/start_beats), with \
             duplicate_clips for tiling. Check wav_stats / waveform after every major \
             bounce (they catch a too-short render, a hot/clipping bounce, overlapping \
             clips). add_chain builds a linear node patch in one call; dispatch_refs \
             wires a non-linear graph in one call with $ref ids; get_render_plan \
             explains how long a bounce will run; arrangement_track_stats shows which \
             stem is hot. The musical decisions (genre, instrumentation, arrangement, \
             feel) are yours — bring your own knowledge; the docs only cover the \
             tool's mechanics: awsm-audio://docs/track-workflow (how to assemble a \
             track in this editor) and awsm-audio://docs/instruments (voice anatomy, \
             a worked kick, velocity, render duration)."
                .to_string(),
        );
        info
    }

    // ── push channel: forward this session's editor events as MCP logging ────
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let mut rx = self.link.subscribe_events();
        let peer = context.peer;
        let agent = self.agent.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok((conn_id, ev)) => {
                        // Only forward events from the tab this agent is bound to.
                        if agent.bound_conn_id() != Some(conn_id) {
                            continue;
                        }
                        let level = match ev.level.as_deref() {
                            Some("error") => LoggingLevel::Error,
                            Some("warning") => LoggingLevel::Warning,
                            _ => LoggingLevel::Info,
                        };
                        let param = LoggingMessageNotificationParam {
                            level,
                            logger: Some("awsm-audio-editor".to_string()),
                            data: serde_json::to_value(&ev).unwrap_or(Value::Null),
                        };
                        // Stops the forwarder once this MCP session drops.
                        if peer.notify_logging_message(param).await.is_err() {
                            break;
                        }
                    }
                    // Slow consumer dropped some events — keep going.
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // ── resources: the worklet-authoring guide (read-only) ───────────────────
    async fn list_resources(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let res = |uri: &str, name: &str, desc: &str| {
            let mut r = RawResource::new(uri, name);
            r.description = Some(desc.to_string());
            r.mime_type = Some("text/markdown".to_string());
            r.no_annotation()
        };
        Ok(ListResourcesResult::with_all_items(vec![
            res(
                "awsm-audio://docs/vocabulary",
                "Command/query vocabulary",
                "The JSON shapes for dispatch_command / run_query (node kinds, \
                 set_field, the sequencer + arrangement ops) — read this before \
                 using the escape hatches.",
            ),
            res(
                "awsm-audio://docs/worklet-abi",
                "Worklet ABI",
                "How to author a WASM DSP worklet against awsm-audio-worklet, build \
                 it, and attach it with attach_wasm — with a minimal Gain example.",
            ),
            res(
                "awsm-audio://docs/track-workflow",
                "Track-building workflow",
                "The awsm-audio-specific workflow for assembling a full track \
                 (build parts → bounce → arrange → mix → verify). Genre-agnostic by \
                 design — it leaves all musical/taste decisions to your own knowledge.",
            ),
            res(
                "awsm-audio://docs/instruments",
                "Instrument anatomy & rendering model",
                "The core mental model: instrument = graph with an outlet boundary; \
                 a sequencer trigger spawns a voice; AudioParam automation runs in \
                 seconds from note-on. Worked kick example, velocity scaling, the \
                 bounce auto-duration algorithm, and feedback-loop renderability.",
            ),
        ]))
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let body = match req.uri.as_str() {
            // The vocabulary carries a generated index derived from the same
            // serde/schemars types the server validates against (no drift).
            "awsm-audio://docs/vocabulary" => vocabulary_doc(),
            "awsm-audio://docs/worklet-abi" => WORKLET_ABI_DOC.to_string(),
            "awsm-audio://docs/track-workflow" => TRACK_WORKFLOW_DOC.to_string(),
            "awsm-audio://docs/instruments" => INSTRUMENTS_DOC.to_string(),
            other => {
                return Err(McpError::resource_not_found(
                    format!("unknown resource {other}"),
                    None,
                ));
            }
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(
            body, req.uri,
        )]))
    }

    // ── prompts: the worklet-authoring workflow ──────────────────────────────
    async fn list_prompts(
        &self,
        _req: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, McpError> {
        Ok(ListPromptsResult::with_all_items(vec![Prompt::new(
            "author_worklet",
            Some("Author a custom WASM DSP worklet end-to-end and attach it to a node."),
            None,
        )]))
    }

    async fn get_prompt(
        &self,
        req: GetPromptRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<GetPromptResult, McpError> {
        match req.name.as_str() {
            "author_worklet" => Ok(GetPromptResult::new(vec![PromptMessage::new_text(
                PromptMessageRole::User,
                WORKLET_ABI_DOC,
            )])
            .with_description("Author + attach a custom WASM DSP worklet.")),
            other => Err(McpError::invalid_params(
                format!("unknown prompt {other}"),
                None,
            )),
        }
    }
}

/// The command/query JSON-shape reference served as `awsm-audio://docs/vocabulary`. It
/// pins the serde tagging the escape hatches (`dispatch_command` / `run_query`)
/// expect, so an agent isn't guessing at `cmd`/`args`/`op` nesting.
/// The vocabulary resource body: the curated guide plus an index **generated
/// from the same serde/schemars types the server validates against** — the tag
/// lists below can never drift from the code (the prose examples are pinned by
/// the decode tests in [`tests`]).
fn vocabulary_doc() -> String {
    let section =
        |title: &str, tags: Vec<String>| format!("\n### {title}\n\n    {}\n", tags.join(", "));
    let mut out = String::from(VOCABULARY_DOC);
    out.push_str("\n## Generated index (derived from the live schema)\n");
    out.push_str(&section(
        "Node kind tags (`add_node` / `add_chain` kind)",
        NodeKind::all_tags().iter().map(|s| s.to_string()).collect(),
    ));
    out.push_str(&section(
        "EditorCommand `cmd` tags (dispatch_command / dispatch_batch / dispatch_refs)",
        enum_tags::<EditorCommand>("cmd"),
    ));
    out.push_str(&section(
        "EditorQuery `query` tags (run_query)",
        enum_tags::<EditorQuery>("query"),
    ));
    out.push_str(&section(
        "ArrangeOp `op` tags (edit_arrange)",
        enum_tags::<ArrangeOp>("op"),
    ));
    out.push_str(&section(
        "SongOp `op` tags (edit_song)",
        enum_tags::<awsm_audio_editor_protocol::SongOp>("op"),
    ));
    out.push_str(&section(
        "ControlOp `op` tags (edit_control)",
        enum_tags::<awsm_audio_editor_protocol::ControlOp>("op"),
    ));
    out.push_str(&section(
        "AutomationEvent `event` tags (set_automation)",
        enum_tags::<AutomationEvent>("event"),
    ));
    out
}

/// Every tag value an adjacently-tagged enum accepts in its `tag_field`,
/// extracted from the schemars JSON Schema (each variant subschema carries the
/// tag as a `const` or single-value `enum` on that property).
fn enum_tags<T: schemars::JsonSchema>(tag_field: &str) -> Vec<String> {
    let schema = schemars::schema_for!(T);
    let v = serde_json::to_value(&schema).unwrap_or_default();
    let mut tags = std::collections::BTreeSet::new();
    collect_tag_values(&v, tag_field, &mut tags);
    tags.into_iter().collect()
}

fn collect_tag_values(v: &Value, field: &str, out: &mut std::collections::BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            if let Some(tag) = o.get("properties").and_then(|p| p.get(field)) {
                if let Some(c) = tag.get("const").and_then(Value::as_str) {
                    out.insert(c.to_string());
                }
                if let Some(e) = tag.get("enum").and_then(Value::as_array) {
                    for s in e.iter().filter_map(Value::as_str) {
                        out.insert(s.to_string());
                    }
                }
            }
            for val in o.values() {
                collect_tag_values(val, field, out);
            }
        }
        Value::Array(a) => {
            for val in a {
                collect_tag_values(val, field, out);
            }
        }
        _ => {}
    }
}

const VOCABULARY_DOC: &str = r#"# awsm-audio command/query vocabulary

Most graph building is covered by the typed tools (add_node, connect, set_field,
bounce, render_wav, …). For anything without a dedicated tool, use the escape
hatches `dispatch_command` (an EditorCommand) and `run_query` (an EditorQuery).
Their JSON is serde-tagged — the shapes below are exact.

## Discover first

- `list_node_kinds` → every creatable node kind with a default `example` value
  (the exact `add_node` `kind`) and its editable field `key`s.
- `get_node_fields { node }` → one live node's set_field keys + current values
  (covers worklet params discovered at runtime).
- `get_snapshot` → ids of existing nodes/connections + the active arrangement.

## Node kinds (for add_node)

`add_node` accepts either a bare kind-name string (defaults filled in):

    { "kind": "oscillator", "x": 0, "y": 0 }

or a full value (copy a kind's `example` from list_node_kinds):

    { "kind": {"kind":"biquad_filter","props":{"type":"lowpass",
      "frequency":{"value":1000.0},"q":{"value":1.0},"gain":{"value":0.0},
      "detune":{"value":0.0}}}, "x": 0, "y": 0 }

A NodeKind value is adjacently tagged: `{"kind":"<tag>","props":{…}}`. Bare
kind tags work everywhere a NodeKind is accepted — add_node, add_chain, and an
`add_node` command's `args.kind` inside dispatch_command / dispatch_batch /
dispatch_refs. (`"sample"` is the one exception: a sample-ref needs a target id,
so use `add_sample_ref`.) Unsure a batch is shaped right? `validate_command`
dry-runs the exact payload without mutating anything.

## EditorCommand (dispatch_command)

Adjacently tagged by `cmd`/`args`. Examples:

    {"cmd":"set_field","args":{"id":"<node>","key":"frequency","value":{"t":"num","v":880.0}}}
    {"cmd":"connect","args":{"from":"<a>","from_output":0,"to":"<b>","to_input":0}}
    {"cmd":"add_sample","args":{"kind":"arrangement"}}   // kind: "sound" | "arrangement"

`set_field`'s `value` is a FieldValue: `{"t":"num","v":1.0}`, `{"t":"text","v":"x"}`,
or `{"t":"bool","v":true}`. (The set_field *tool* takes a plain number; use this
form only via dispatch_command for text/bool fields.)

### Boundaries (add_boundary) and automation (set_automation)

Both have dedicated typed tools — prefer them. The raw command shapes, exact:

    {"cmd":"add_boundary","args":{"port":"outlet","x":0.0,"y":0.0}}   // port: "inlet" | "outlet"

    {"cmd":"set_automation","args":{"id":"<node>","param":"gain","events":[
       {"event":"set_value","args":{"value":0.0,"time":0.0}},
       {"event":"linear_ramp","args":{"value":1.0,"time":0.01}},
       {"event":"exponential_ramp","args":{"value":0.001,"time":0.4}}
    ]}}

An AutomationEvent is adjacently tagged `{"event":"<name>","args":{…}}`. The
names + args mirror the AudioParam scheduling methods one-to-one:

    set_value         {"value":1.0,"time":0.0}
    linear_ramp       {"value":1.0,"time":0.5}
    exponential_ramp  {"value":0.001,"time":0.5}      // value must be > 0
    set_target        {"target":0.5,"start_time":0.2,"time_constant":0.05}
    set_value_curve   {"values":[0.0,1.0,0.0],"start_time":0.0,"duration":1.0}
    cancel_scheduled  {"time":1.0}
    cancel_and_hold   {"time":1.0}

`events` REPLACES the param's whole timeline. Times are seconds — from note-on
for a triggered instrument voice, from render start otherwise.

### Sequencer + arrangement sub-ops

These wrap a nested op, itself adjacently tagged by `op`/`args`:

    // Note Sequencer (node-addressed):
    {"cmd":"edit_song","args":{"node":"<seq>","op":{"op":"add_track"}}}
    {"cmd":"edit_song","args":{"node":"<seq>","op":{"op":"add_note","args":{
       "track":0,"event":{"start":0.0,"length":1.0,"note":60,"velocity":100}}}}}
    {"cmd":"edit_song","args":{"node":"<seq>","op":{"op":"set_bpm","args":120.0}}}

    // Control Sequencer (node-addressed): edit_control, ops add_lane / add_point / …
    {"cmd":"edit_control","args":{"node":"<seq>","op":{"op":"add_lane"}}}

    // Arrangement (no node — edits the active Arrangement sample):
    {"cmd":"edit_arrange","args":{"op":{"op":"add_track"}}}
    {"cmd":"edit_arrange","args":{"op":{"op":"add_clip","args":{
       "track":0,"start":0.0,"source":"<bounced-sample-id>"}}}}

Tuple-variant ops take a bare value as `args` (e.g. `set_bpm` → `"args":120.0`).

### Every ArrangeOp (the `op` inside edit_arrange)

Most of these now have a dedicated tool (shown in parens) — prefer it; use
edit_arrange only for the few without one. All edit the *active* Arrangement.

    set_bpm           "args": 120.0                              (set_arrangement_bpm)
    set_length_secs   "args": 32.0                               (set_arrangement_length)
    set_markers       {"start":0.0,"end":8.0}  // both null clears (set_arrangement_markers)
    add_track         (no args)                                  (add_arrangement_track)
    remove_track      {"track":0}                                (remove_arrangement_track)
    set_track_name    {"track":0,"name":"Drums"}                 (set_track_name)
    set_track_gain    {"track":0,"gain":0.8}                     (set_track_gain)
    set_track_mute    {"track":0,"mute":true}
    set_track_solo    {"track":0,"solo":true}
    add_clip          {"track":0,"start":0.0,"source":"<id>","length":4.0}  (add_clip)
    remove_clip       {"track":0,"clip":2}                       (remove_clip)
    paste_clip        {"track":0,"clip":{<full Clip>}}
    paste_clips       {"clips":[{"track":0,"clip":{<Clip>}}, …]}  (duplicate_clips builds these)
    move_clip         {"track":0,"clip":2,"new_track":1,"start":8.0}
    resize_clip       {"track":0,"clip":2,"length":4.0}
    stretch_clip      {"track":0,"clip":2,"length":4.0,"speed":1.0}
    set_clip_offset   {"track":0,"clip":2,"offset":0.5}
    trim_start        {"track":0,"clip":2,"start":1.0,"offset":0.5}
    split_clip        {"track":0,"clip":2,"at":4.0}
    set_clip_gain     {"track":0,"clip":2,"gain":0.7}            (set_clip_gain)
    set_clip_loop     {"track":0,"clip":2,"looping":true}
    clear             (no args) — drop every clip, keep tracks   (clear_arrangement)

Times are in seconds. Use `beats_to_secs {bpm,beats,bars}` to convert beats/bars
(get_arrangement reports the active arrangement's bpm). `duplicate_clips` tiles a
track's clips at a bar/beat interval. `arrangement_bounce_report` flags clips whose
source bounce is stale/missing; `bounce_all_dirty` re-bounces them.

## EditorQuery (run_query)

Adjacently tagged by `query`/`args`. Unit variants need no args:

    {"query":"snapshot"}
    {"query":"samples"}
    {"query":"bounce_status","args":{"sample":"<id>"}}
    {"query":"waveform","args":{"sample":null,"buckets":64}}

## Typical flow

1. list_node_kinds → pick kinds. (pairing_status first if you're unsure an
   editor is attached.)
2. add_node (source) + add_node (effects); connect them; the last unconnected
   output auditions to master.
3. set_field / set_automation to shape it; render_wav / wav_stats / waveform to
   inspect (wav_stats also reports crest factor, DC offset, lead/trail silence,
   approximate attack/decay — useful for one-shots and sound effects).
4. bounce a Sound, then build an Arrangement (create_arrangement takes bpm /
   length_secs / named tracks in one call; add_arrangement_tracks +
   set_track_gains batch later edits). render_wav / wav_stats / waveform work on
   an arrangement sample too — they render its clip timeline. Optionally set
   loop/export markers (set_arrangement_markers, or edit_arrange set_markers) to
   render just a region; clear them to render the whole timeline.
5. verify_arrangement before exporting — one call combining the bounce report,
   per-track solo stats, master stats, and clip overruns, with mechanical
   recommendations. Then export_wav writes the `.wav` to a path you choose
   (`trim_silence: true` strips lead/tail silence below -60 dBFS).
6. Forking experiments: duplicate_sample clones a patch; compare_samples renders
   two and returns stats side by side; parameter_sweep measures one field across
   several values (non-destructive); set_sample_notes tags variants ("keeper",
   "too bright"). export_sample / import_sample move a patch (sample +
   sub-samples + assets) between projects as a self-contained TOML.
7. Discovery extras: list_modulation_targets maps every automatable param on the
   active canvas; set_arrangement_sections names timeline regions ("intro",
   "main") as metadata; the generated index below lists every tag the escape
   hatches accept.

## Multi-sample: an instrument played by a sequencer

A Sound is a node graph; another Sound can reference it (a Sample-ref) and a Note
Sequencer can trigger it per note. `set_active_sample` switches which sample's
graph you're editing.

1. Make an instrument Sound: `add_sample {kind:"sound"}` (it becomes active and
   gets a fresh id — find it with list_samples), then add its voice
   (e.g. add_node "oscillator"). When triggered, its sources play at the note's
   pitch for the note's length.
2. `set_active_sample` back to the song Sound (e.g. the root "main").
3. There, `add_node "note_sequencer"`; author notes with
   `edit_song add_track` / `add_note` (see above). Add a Sample-ref to the
   instrument: `{"cmd":"add_sample_ref","args":{"sample":"<instrument-id>","x":0,"y":0}}`.
4. Bind the sequencer's output to the ref's trigger:
   `{"cmd":"bind","args":{"from":"<sequencer>","from_output":0,"to":"<sample-ref>"}}`
   (`from_output` indexes the sequencer's `outputs`, one per melodic track / drum
   note). The root Sound is now a "song" — render_wav plays the sequence.

## Building a drum kit

A drum kit is the multi-sample pattern above with a **drum-mode** sequencer: its
`outputs` are one per distinct note number (GM: 36 kick, 38 snare, 42 closed hat),
each bound to its own one-shot voice.

1. One instrument Sound per voice (kick / snare / hat / …) — a short percussive
   graph (e.g. a pitched sine blip, or a noise burst through a filter + envelope).
2. A song Sound with a drum `note_sequencer`. Author the whole pattern at once —
   either create the node with an inline `song` (its `outputs` are derived
   immediately, so binds work right away) or load a track in one shot with
   `{"cmd":"edit_song","args":{"node":"<seq>","op":{"op":"set_track_events","args":{
      "track":0,"events":[{"start":0.0,"length":0.5,"note":36,"velocity":110}, …]}}}}`.
   Each distinct `note` becomes one output (sorted by note number).
3. `add_sample_ref` per voice; `bind` each sequencer output to the matching voice's
   trigger (`from_output` = that drum note's index in `outputs` — read them back
   from get_snapshot / the node's `outputs`).
4. Wire each Sample-ref's audio output into a bus / Output node, set gains, bounce.

(Binding an output that doesn't exist yet is rejected with a message — author the
notes/tracks first so the outputs are derived.)
"#;

/// The worklet-authoring guide served both as the `awsm-audio://docs/worklet-abi`
/// resource and the `author_worklet` prompt, so an agent can write a correct
/// crate without reading the repo.
const WORKLET_ABI_DOC: &str = r#"# Authoring an awsm-audio WASM DSP worklet

An AudioWorklet node runs a **native Rust → wasm** DSP processor you author,
compile, and attach. The MCP server only relays the bytes — you compile locally
(so you get cargo's errors directly) and pass the `.wasm` to `attach_wasm`.

**When to use one — and when NOT to.** Reach for a worklet *only* when no built-in
node (or combination of them) can express the DSP. That means:

- **FFT / spectral** — vocoder, spectral freeze / gate, pitch-shift, spectral blur.
- **Granular synthesis** — grain clouds, time-stretch, texture smearing.
- **Physical modeling** — Karplus-Strong pluck, waveguides, modal resonators.
- **Per-sample stateful nonlinearities** — wavefolders, custom saturation chains,
  envelope followers, dynamics beyond the compressor.
- **Custom synthesis algorithms** — FM operator stacks, additive with per-partial
  control, a supersaw (detuned voices) in one node.

**Don't** write a worklet for things the built-ins already do: modulated-delay
effects (chorus / flanger) are a `delay` with an LFO on `delayTime`; a phaser is an
all-pass `biquad_filter` chain; distortion / bitcrush is a `waveshaper` (with a
custom curve if needed). Compose nodes first; drop to a worklet for genuine gaps.

The fastest path is the `scaffold_worklet` tool — it emits a ready-to-build crate
(Cargo.toml + a wired `src/lib.rs` with your params, `process()` stubbed) so you
only write DSP. The full Rust API is at <https://docs.rs/awsm-audio-worklet/latest>.

## 1. Author a crate

`Cargo.toml`:

```toml
[package]
name = "my-worklet"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
awsm-audio-worklet = "1"
```

(The `worklet_cargo_toml` tool returns this snippet ready to paste.)

`src/lib.rs` — implement `Processor` and call `awsm_worklet!` exactly once:

```rust
use awsm_audio_worklet::{awsm_worklet, ParamDesc, Params, Processor};

struct Gain;

impl Processor for Gain {
    // name, min, max, default — each becomes a labelled, automatable knob.
    const PARAMS: &'static [ParamDesc] = &[ParamDesc::new("gain", 0.0, 2.0, 1.0)];

    fn new(_sample_rate: f32) -> Self { Gain }

    // Per-channel (planar) slices, equal length (<= 128 frames). NO allocation.
    fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], params: &Params) {
        let g = params.get(0);
        for ch in 0..output.len() {
            for (o, &i) in output[ch].iter_mut().zip(input[ch]) {
                *o = i * g;
            }
        }
    }
}

awsm_worklet!(Gain);
```

ABI rules: stereo (`CHANNELS = 2`), <= `MAX_FRAMES` (128) frames/quantum,
<= `MAX_PARAMS` (32) params, no allocation in `process`. Use the crate's
`math::{sin, tanh}` instead of `f32::sin`/`tanh` (those pull extra wasm symbols).
See `packages/worklets/{gain,ringmod,drive,bitcrusher}` for worked examples.

**`#![no_std]` crates:** a `#![no_std]` cdylib must define a `#[panic_handler]`,
or the build fails at link with `` error: `#[panic_handler]` function required, but
not found ``. Don't hand-write it — invoke the macro as `awsm_worklet!(Gain,
no_std);` and it emits a minimal one for you. (A normal std crate needs nothing
extra; std supplies the handler.)

## 2. Compile

```sh
cargo build -p my-worklet --target wasm32-unknown-unknown --release
# → target/wasm32-unknown-unknown/release/my_worklet.wasm
```

A compile error here is yours to fix — it shows up in your own build output.

## 3. Attach

1. `add_node` an `audio_worklet` node (e.g. `dispatch_command` with
   `{"cmd":"add_node","args":{"kind":{"audio_worklet":{}},"x":0,"y":0}}`), then
   `get_snapshot` to read its id.
2. `attach_wasm { node, wasm_path }` (or `wasm_base64`). On success the editor
   compiles the module, discovers its params, and binds it.
3. `get_snapshot` again — the node now lists the discovered params, editable /
   automatable / modulation-targetable like any field. Wire it up and
   `render_wav` / `wav_stats` to hear/inspect the result.

A module that compiles but violates the ABI returns the error from `attach_wasm`.

## 4. Driving a worklet source for a real duration

A worklet used directly as a *source* Sound often renders only a tiny default
window when you bounce it — there's no note/gate telling it how long to sound. Two
ways to capture a real-length render:

- Quick: `bounce` (or `render_wav`) with `duration_secs` set — forces a fixed-length
  offline render of the procedural source.
- Musical (the robust pattern): wrap it in a sequencer-triggered voice, then bounce
  the wrapper —
  1. make the worklet its own source Sound;
  2. make a second Sound with a Note Sequencer;
  3. add a Sample-ref to the worklet Sound and bind a track's output to it;
  4. trigger it with a long note (the note length is the sounding length);
  5. `bounce` the wrapper Sound — that's your arrangement-ready clip.

Either way, follow with `wav_stats` / `waveform` to confirm the length and level.
"#;

/// The awsm-audio-specific track-building *workflow* (not music advice), served as
/// `awsm-audio://docs/track-workflow`. Nudges song requests toward bounced loops +
/// a sectioned arrangement instead of one monolithic root sequencer, and leaves
/// all genre/taste decisions to the agent's own knowledge.
const TRACK_WORKFLOW_DOC: &str = r#"# Building a full track: the awsm-audio workflow

This is the **tool-specific workflow** — how to assemble a track out of awsm-audio's
primitives. It is deliberately genre-agnostic: *you* know the genre, its feel, its
instrumentation and arrangement conventions far better than any checklist here, so
bring that knowledge and apply it to these primitives. The only thing this resource
adds is how the pieces fit together in this editor.

## Build parts → bounce → arrange (don't build one giant root sequencer)

1. For each part you decide the track needs — author a short **loop Sound** (a graph,
   or a sequencer-driven instrument; see the `awsm-audio://docs/instruments`
   resource for voice anatomy), then `bounce` it. Check `wav_stats` / `waveform`
   after each bounce to catch a too-short render or a hot/clipping bounce.
2. `create_arrangement`; `add_arrangement_track` one per part; name them
   (`set_track_name`).
3. Place clips into sections with `add_clip` / `add_clips` — give positions in
   `start_bars` / `start_beats` directly (they use the arrangement BPM), or an
   `add_clips` `sections` string like `"3-12, 15-20"`. `duplicate_clips` tiles a
   track's clips at a bar/beat interval.
4. Balance with `set_track_gain` / `set_clip_gain`. Use `arrangement_track_stats`
   to see which stem is hot (per-track solo render) instead of rescaling blindly.
   Set loop/export markers (`set_arrangement_markers`, bar forms accepted) to
   render just a region.
5. Before exporting: `arrangement_bounce_report` to spot stale/missing clip
   bounces, `bounce_all_dirty` to fix them, then `render_wav` / `wav_stats` on the
   arrangement (watch for clipping from overlapping clips).

## What's yours vs. what's the tool's

- **Yours (your knowledge):** genre, tempo, swing/feel, which instruments, how the
  parts interact, harmony, the arrangement's emotional shape, sound-design choices.
  The primitives are expressive — don't settle for a generic version of a style.
- **The tool's (this doc + `instruments`):** the build order above, voice anatomy
  and envelope/velocity mechanics, how long a bounce runs (`get_render_plan`), and
  the verification loop. Lean on these so the mechanics never trip you up; lean on
  your own taste for everything that makes the track good.
"#;

/// The instrument-anatomy + rendering-model guide served as
/// `awsm-audio://docs/instruments`. The mental model an agent otherwise has to
/// infer from scattered field docs: what an instrument *is*, how a trigger spawns
/// a voice, how envelopes are timed, how long a bounce runs, and which graphs are
/// renderable offline.
const INSTRUMENTS_DOC: &str = r#"# Instruments & the rendering model

## What an instrument is

An **instrument** is just a Sound (a node graph) with an **outlet boundary** node
(`add_boundary` → outlet). The outlet is the voice's audio out. When a Note
Sequencer triggers the instrument (via a Sample-ref + `bind`), the editor **spawns
one voice per note**: it instantiates the graph, plays its sources at the note's
pitch for the note's length, and routes the outlet to the mix.

- A Sound with **no** outlet boundary just auditions its loose ends to master — fine
  for sound-design, but to be *played by a sequencer* it needs the outlet.
- The trigger sets the voice's pitch (from the note number) and gate length (from
  the note's length). Sources that respond to pitch (oscillator frequency) track
  the note; others (noise, samples) just start/stop.

## Envelope timing (AudioParam automation)

A node's AudioParam automation (set via `set_automation` / the inspector) runs in
**seconds relative to note-on** (voice start), not absolute timeline seconds. So an
amp envelope of `[{t:0, v:0}, {t:0.005, v:1}, {t:0.2, v:0}]` is a 5 ms attack →
200 ms decay *from each note's start*. This is what makes one authored envelope work
for every note a sequencer fires.

## Velocity scaling

A note's `velocity` (0–127) scales the voice amplitude roughly **linearly as
v/127** — so `velocity: 1` is ~ −42 dB, essentially silent. Velocity is *not* a
boolean "on" flag: for an audible hit use 90–120; reserve low velocities for genuine
dynamics. (If you wired a crackle/one-shot and "nothing plays", check velocity.)

## Worked example: a kick drum

A short pitched sine blip with a fast pitch drop and amp decay:

1. `add_chain ["oscillator", "gain"]` → ids `[osc, amp]` (osc → amp).
2. `add_boundary` outlet; `connect amp → outlet`.
3. Pitch drop: `set_automation` on `osc.frequency` =
   `[{t:0, v:150}, {t:0.06, v:45}]` (150 Hz → 45 Hz over 60 ms).
4. Amp decay: `set_automation` on `amp.gain` =
   `[{t:0, v:1}, {t:0.18, v:0}]` (instant attack → 180 ms decay).
5. Drive it from a drum `note_sequencer` (note 36) bound to a Sample-ref of this
   Sound; or `bounce` with `duration_secs: 0.3` to audition the one-shot.

A snare = noise burst → bandpass → fast amp decay; a hat = noise → highpass → very
fast decay. Same anatomy, different source/filter.

## How long a bounce renders (auto-duration)

`bounce` / `render_wav` pick a length automatically — call **`get_render_plan`** to
see it (and why) before rendering:

- **Sequencer-driven Sound** (a sequencer is wired into the audible path): renders
  the **song-loop length** + a 3 s release tail (so note tails fold cleanly onto the
  loop). A silent/unbound sequencer doesn't count — it's treated as a plain graph.
- **Continuous / one-shot graph** (no sequencer audible): renders a fixed **6 s**
  default window.
- **Override anytime** with `duration_secs` on `bounce` / `render_wav` — the right
  tool for a procedural source (noise, a worklet) with no note to bound it.

## Offline renderability

- **DelayNode feedback loops** (`delay → gain → delay`) render fine offline — the
  delay breaks the cycle, so a feedback echo/reverb is fully bounceable.
- **Custom-wave oscillators** (`type: custom` + `harmonics`) render via a
  PeriodicWave — supported.
- **Live sources can't render offline**: a mic (`media_stream_source`) or media
  element (`media_element_source`) makes a Sound unbounceable; `get_render_plan`
  reports this. Replace them with an `audio_buffer_source` + `load_audio` to bounce.
"#;

fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s.into())])
}

/// Best-effort content type from a file extension (for hosting a loaded audio
/// file). `decodeAudioData` sniffs the bytes regardless, so this is advisory.
fn content_type_for(path: &str) -> String {
    let p = path.to_lowercase();
    if p.ends_with(".wav") {
        "audio/wav"
    } else if p.ends_with(".mp3") {
        "audio/mpeg"
    } else if p.ends_with(".flac") {
        "audio/flac"
    } else if p.ends_with(".ogg") || p.ends_with(".oga") {
        "audio/ogg"
    } else if p.ends_with(".m4a") || p.ends_with(".aac") {
        "audio/aac"
    } else {
        "application/octet-stream"
    }
    .to_string()
}

fn unexpected(resp: Response) -> McpError {
    McpError::internal_error(format!("unexpected response: {resp:?}"), None)
}

/// An invalid-params error whose `data` is agent-actionable in one retry: the
/// `problem`, a copy-pasteable `expected` payload, the `received` value, and the
/// `docs_uri` to read — instead of a bare serde message that only reveals the
/// next missing field.
fn schema_error(problem: impl Into<String>, expected: Value, received: Value) -> McpError {
    let problem = problem.into();
    McpError::invalid_params(
        problem.clone(),
        Some(serde_json::json!({
            "problem": problem,
            "expected": expected,
            "received": received,
            "docs_uri": "awsm-audio://docs/vocabulary",
        })),
    )
}

/// A minimal **valid** payload for a command (by its `cmd` tag), for the
/// `expected` slot of [`schema_error`] — mechanical shapes only, no musical
/// content. Unknown/omitted tags get a generic add_node + connect pair.
fn command_example(cmd: Option<&str>) -> Value {
    match cmd {
        Some("add_node") => serde_json::json!(
            {"cmd":"add_node","args":{"kind":"oscillator","x":0.0,"y":0.0}}
        ),
        Some("add_boundary") => serde_json::json!(
            {"cmd":"add_boundary","args":{"port":"outlet","x":0.0,"y":0.0}}
        ),
        Some("set_automation") => serde_json::json!(
            {"cmd":"set_automation","args":{"id":"<node-id>","param":"gain","events":[
                {"event":"set_value","args":{"value":0.0,"time":0.0}},
                {"event":"linear_ramp","args":{"value":1.0,"time":0.1}}
            ]}}
        ),
        Some("set_field") => serde_json::json!(
            {"cmd":"set_field","args":{"id":"<node-id>","key":"frequency","value":{"t":"num","v":440.0}}}
        ),
        Some("connect") => serde_json::json!(
            {"cmd":"connect","args":{"from":"<node-id>","to":"<node-id>"}}
        ),
        Some("edit_song") => serde_json::json!(
            {"cmd":"edit_song","args":{"node":"<seq-id>","op":{"op":"add_note","args":{
                "track":0,"event":{"start":0.0,"length":1.0,"note":60,"velocity":100}}}}}
        ),
        Some("edit_arrange") => serde_json::json!(
            {"cmd":"edit_arrange","args":{"op":{"op":"add_track"}}}
        ),
        _ => serde_json::json!([
            {"cmd":"add_node","ref":"osc","args":{"kind":"oscillator","x":0.0,"y":0.0}},
            {"cmd":"add_node","ref":"out","args":{"kind":"output","x":200.0,"y":0.0}},
            {"cmd":"connect","args":{"from":"$osc","to":"$out"}}
        ]),
    }
}

/// Drop the bulky embedded note_sequencer song events from a snapshot value
/// (the `detail:"ids"` mode): replaces each sequencer track's `events` array with
/// an empty one plus an `events_count`, leaving node ids/kinds/wires intact.
/// Automation `events` on params are left untouched (small + useful).
fn slim_snapshot(v: &mut Value) {
    let Some(nodes) = v
        .pointer_mut("/data/graph/nodes")
        .and_then(Value::as_array_mut)
    else {
        return;
    };
    for node in nodes {
        let Some(kind) = node.get_mut("kind") else {
            continue;
        };
        if kind.get("kind").and_then(Value::as_str) != Some("note_sequencer") {
            continue;
        }
        let Some(tracks) = kind
            .pointer_mut("/props/song/tracks")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        for track in tracks {
            let Some(obj) = track.as_object_mut() else {
                continue;
            };
            if let Some(count) = obj.get("events").and_then(Value::as_array).map(Vec::len) {
                obj.insert("events".into(), Value::Array(Vec::new()));
                obj.insert("events_count".into(), Value::from(count));
            }
        }
    }
}

/// Parse a bar-range section spec like `"3-12, 15-20"` into per-bar start
/// offsets (bar units, end-exclusive): `[3,4,…,11, 15,16,…,19]`. A bare `"5"`
/// yields `[5]`. Powers `add_clips`'s `sections`.
fn parse_sections(spec: &str) -> Result<Vec<f64>, McpError> {
    let bad = |m: String| McpError::invalid_params(m, None);
    let mut out = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let a: i64 = a
                .trim()
                .parse()
                .map_err(|_| bad(format!("bad section start in '{part}'")))?;
            let b: i64 = b
                .trim()
                .parse()
                .map_err(|_| bad(format!("bad section end in '{part}'")))?;
            if b <= a {
                return Err(bad(format!("section '{part}': end must be > start")));
            }
            out.extend((a..b).map(|bar| bar as f64));
        } else {
            let a: i64 = part
                .parse()
                .map_err(|_| bad(format!("bad section bar '{part}'")))?;
            out.push(a as f64);
        }
    }
    if out.is_empty() {
        return Err(bad("sections string has no bars".into()));
    }
    Ok(out)
}

/// Parse a uuid string into a [`NodeId`] (for wiring returned chain ids).
fn parse_node_id(s: &str) -> Result<NodeId, McpError> {
    s.parse::<NodeId>()
        .map_err(|e| McpError::internal_error(format!("bad node id {s}: {e}"), None))
}

/// Recursively replace any `"$name"` string in `v` with `map["name"]` — the
/// symbolic-id substitution behind `dispatch_refs`. A `$name` with no matching
/// ref is left as-is (it will surface as a deserialize error downstream).
fn substitute_refs(v: &mut Value, map: &std::collections::HashMap<String, String>) {
    match v {
        Value::String(s) => {
            if let Some(name) = s.strip_prefix('$') {
                if let Some(id) = map.get(name) {
                    *s = id.clone();
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| substitute_refs(x, map)),
        Value::Object(o) => o.values_mut().for_each(|x| substitute_refs(x, map)),
        _ => {}
    }
}

fn unexpected_query(qr: QueryResult) -> McpError {
    McpError::internal_error(format!("unexpected query result: {qr:?}"), None)
}

/// The `awsm-audio-worklet` semver requirement to emit in a generated worklet
/// crate — the **major** of *this binary's* version (the crate publishes in
/// lockstep via `version.workspace = true`), as a caret req so cargo always picks
/// the latest compatible publish. Derived (never a hardcoded literal) so it can't
/// go stale across a major bump. `env!("CARGO_PKG_VERSION")` is `&'static`, and
/// `split_once` hands back `&'static` subslices — so this stays `&'static str`.
fn worklet_dep_req() -> &'static str {
    let v = env!("CARGO_PKG_VERSION");
    v.split_once('.').map(|(major, _)| major).unwrap_or(v)
}

/// The Cargo.toml for a worklet crate named `name`, with the dependency version
/// derived by [`worklet_dep_req`].
fn worklet_cargo_toml_text(name: &str) -> String {
    format!(
        r#"# Cargo.toml for an awsm-audio WASM DSP worklet
[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
awsm-audio-worklet = "{req}"

# Build: cargo build -p {name} --target wasm32-unknown-unknown --release
# Attach the resulting .wasm with the attach_wasm tool.
# Guide: awsm-audio://docs/worklet-abi   API: https://docs.rs/awsm-audio-worklet/latest
#
# A std crate (the above) "just works". For a smaller `#![no_std]` crate, call the
# macro as `awsm_worklet!(MyProc, no_std);` — that form also emits the
# `#[panic_handler]` a no_std cdylib requires (otherwise the build fails at link
# with "`#[panic_handler]` function required, but not found").
"#,
        name = name,
        req = worklet_dep_req(),
    )
}

/// A valid kebab-case crate name from arbitrary input (invalid chars → `-`),
/// falling back to `my-worklet`.
fn sanitize_crate_name(s: &str) -> String {
    let cleaned: String = s
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "my-worklet".to_string()
    } else {
        trimmed.to_string()
    }
}

/// `"my-worklet"` → `"MyWorklet"` (the Processor struct name).
fn kebab_to_camel(s: &str) -> String {
    let camel: String = s
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect();
    if camel.is_empty() {
        "Worklet".to_string()
    } else {
        camel
    }
}

/// The `src/lib.rs` for a scaffolded worklet: the ABI fully wired (Processor impl,
/// the declared params as ParamDesc knobs, the `awsm_worklet!` macro) with a
/// passthrough `process()` the author replaces with DSP.
fn worklet_lib_rs(struct_name: &str, params: &[WorkletParamSpec], no_std: bool) -> String {
    let param_lines: String = params
        .iter()
        .map(|p| {
            format!(
                "        ParamDesc::new({:?}, {:?}, {:?}, {:?}),",
                p.name, p.min, p.max, p.default
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let read_hints: String = params
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "        //   params.get({i})  → {:?}  ({}..{}, default {})",
                p.name, p.min, p.max, p.default
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let header = if no_std { "#![no_std]\n\n" } else { "" };
    let macro_call = if no_std {
        format!("awsm_worklet!({struct_name}, no_std);")
    } else {
        format!("awsm_worklet!({struct_name});")
    };
    format!(
        r#"{header}use awsm_audio_worklet::{{awsm_worklet, ParamDesc, Params, Processor}};

struct {struct_name};

impl Processor for {struct_name} {{
    // Each ParamDesc(name, min, max, default) is a labelled, automatable knob.
    const PARAMS: &'static [ParamDesc] = &[
{param_lines}
    ];

    fn new(_sample_rate: f32) -> Self {{
        {struct_name}
    }}

    // Per-channel (planar) slices, equal length (<= 128 frames). NO allocation in
    // here. Use the crate's `math::{{sin, tanh, ...}}` instead of `f32::sin` etc.
    fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], params: &Params) {{
        // Your declared params (read with params.get(i)):
{read_hints}
        let _ = params;
        // TODO: replace this passthrough with your DSP.
        for ch in 0..output.len() {{
            for (o, &i) in output[ch].iter_mut().zip(input[ch]) {{
                *o = i;
            }}
        }}
    }}
}}

{macro_call}
"#
    )
}

/// A tool argument that is **strongly typed** — its JSON Schema is exactly `T`'s,
/// so a fresh agent sees the precise shape — yet tolerant of clients that deliver
/// a nested object as a JSON *string* (it deserializes from either form), and of
/// bare-name strings where `T` supports them (a catalog kind tag for `NodeKind`).
/// Typed, self-documenting, and robust.
#[derive(Debug, Clone)]
pub struct Flexible<T>(pub T);

/// Construct a `T` from a bare (non-JSON) name token, where that's meaningful.
/// The default is "not supported"; `NodeKind` resolves catalog tags like
/// `"oscillator"` to a default-props kind. This is what makes
/// `add_node {kind: "gain"}` / `add_chain {kinds: ["oscillator", …]}` work —
/// previously a bare tag fell into the JSON-string parse and died with the
/// opaque `expected value at line 1 column 1`.
pub trait FromBareName: Sized {
    fn from_bare_name(_name: &str) -> Option<Self> {
        None
    }
    /// A hint appended to decode errors (e.g. the accepted tag list).
    fn bare_name_hint() -> Option<String> {
        None
    }
    /// Pre-normalize a raw JSON value before the typed decode — e.g. resolve a
    /// bare kind tag nested inside a command (`add_node`'s `args.kind`).
    fn normalize_value(v: Value) -> Value {
        v
    }
}

/// Resolve a bare kind tag nested in a command's `args.kind` (the `add_node`
/// case) to its full default `NodeKind` value, so bare tags work through the
/// dispatch_command / dispatch_batch / dispatch_refs escape hatches too — not
/// just the typed add_node / add_chain tools.
fn normalize_command_value(mut v: Value) -> Value {
    let is_add_node = v.get("cmd").and_then(Value::as_str) == Some("add_node");
    if is_add_node {
        if let Some(kind_slot) = v.pointer_mut("/args/kind") {
            if let Some(tag) = kind_slot.as_str() {
                if let Some(kind) = NodeKind::from_tag(tag.trim()) {
                    if let Ok(full) = serde_json::to_value(&kind) {
                        *kind_slot = full;
                    }
                }
            }
        }
    }
    v
}

impl FromBareName for NodeKind {
    fn from_bare_name(name: &str) -> Option<Self> {
        NodeKind::from_tag(name)
    }
    fn bare_name_hint() -> Option<String> {
        Some(format!(
            "accepted bare kind tags: {}; or pass a full {{\"kind\":\"<tag>\",\"props\":{{…}}}} \
             value (copy an `example` from list_node_kinds)",
            NodeKind::all_tags().join(", ")
        ))
    }
}
impl FromBareName for EditorCommand {
    fn bare_name_hint() -> Option<String> {
        Some(
            "a command is an object {\"cmd\":\"<name>\",\"args\":{…}} — \
             read awsm-audio://docs/vocabulary for every shape"
                .to_string(),
        )
    }
    fn normalize_value(v: Value) -> Value {
        normalize_command_value(v)
    }
}
impl FromBareName for EditorQuery {
    fn bare_name_hint() -> Option<String> {
        Some(
            "a query is an object {\"query\":\"<name>\"} or {\"query\":\"<name>\",\"args\":{…}}"
                .to_string(),
        )
    }
}

impl<'de, T: serde::de::DeserializeOwned + FromBareName> serde::Deserialize<'de> for Flexible<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let inner = match Value::deserialize(d)? {
            Value::String(s) => {
                // A bare name first (e.g. a kind tag), then a JSON-encoded payload.
                if let Some(v) = T::from_bare_name(s.trim()) {
                    v
                } else {
                    let payload: Value = serde_json::from_str(&s).map_err(|e| {
                        Error::custom(match T::bare_name_hint() {
                            Some(hint) => format!("string {s:?} is not valid here ({e}) — {hint}"),
                            None => e.to_string(),
                        })
                    })?;
                    serde_json::from_value(T::normalize_value(payload)).map_err(|e| {
                        Error::custom(match T::bare_name_hint() {
                            Some(hint) => format!("{e} — {hint}"),
                            None => e.to_string(),
                        })
                    })?
                }
            }
            other => serde_json::from_value(T::normalize_value(other)).map_err(|e| {
                Error::custom(match T::bare_name_hint() {
                    Some(hint) => format!("{e} — {hint}"),
                    None => e.to_string(),
                })
            })?,
        };
        Ok(Flexible(inner))
    }
}

// Schema is exactly `T`'s — clients that respect schemas send a structured object.
impl<T: schemars::JsonSchema> schemars::JsonSchema for Flexible<T> {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }
    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }
    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(generator)
    }
}

#[cfg(test)]
mod tests {
    //! Regressions for the agent-experience failures captured in the field notes
    //! (schema-exploration loops): bare kind tags, stringified dispatch_refs
    //! commands, add_boundary / set_automation shapes, and the guarantee that
    //! every `expected` example we hand back in an error actually decodes.

    use super::*;

    fn flexible_kind(v: Value) -> Result<NodeKind, String> {
        serde_json::from_value::<Flexible<NodeKind>>(v)
            .map(|f| f.0)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn bare_kind_tags_decode_for_every_catalog_kind() {
        // The exact failures from the logs: "audio_buffer_source" and "output"
        // as bare strings died with `expected value at line 1 column 1`.
        for tag in NodeKind::all_tags() {
            let kind = flexible_kind(Value::String((*tag).to_string()))
                .unwrap_or_else(|e| panic!("bare tag {tag:?} failed: {e}"));
            let v = serde_json::to_value(&kind).unwrap();
            assert_eq!(v.get("kind").and_then(Value::as_str), Some(*tag));
        }
    }

    #[test]
    fn unknown_bare_kind_names_the_accepted_tags() {
        let err = flexible_kind(Value::String("wibble".into())).unwrap_err();
        assert!(
            err.contains("oscillator") && err.contains("list_node_kinds"),
            "error should teach recovery, got: {err}"
        );
    }

    #[test]
    fn full_kind_objects_still_decode() {
        let full = serde_json::json!({"kind":"gain","props":{"gain":{"value":0.5}}});
        let kind = flexible_kind(full).unwrap();
        assert!(matches!(kind, NodeKind::Gain(_)));
    }

    #[test]
    fn add_node_command_accepts_bare_kind_tag() {
        // Through the dispatch_command escape hatch (Flexible<EditorCommand>).
        let cmd: Flexible<EditorCommand> = serde_json::from_value(serde_json::json!(
            {"cmd":"add_node","args":{"kind":"oscillator","x":1.0,"y":2.0}}
        ))
        .expect("bare kind tag inside add_node args");
        assert!(matches!(cmd.0, EditorCommand::AddNode { .. }));
    }

    #[test]
    fn every_command_example_is_itself_valid() {
        // The `expected` payloads we return in schema errors must decode — an
        // example an agent can't paste back is worse than none.
        // Examples carry human-readable `<node-id>`-style placeholders where a
        // real id goes; swap them for a nil uuid before the decode check.
        fn fill_placeholders(v: &mut Value) {
            match v {
                Value::String(s) if s.starts_with('<') && s.ends_with('>') => {
                    *s = "00000000-0000-0000-0000-000000000000".to_string();
                }
                Value::Array(a) => a.iter_mut().for_each(fill_placeholders),
                Value::Object(o) => o.values_mut().for_each(fill_placeholders),
                _ => {}
            }
        }
        for cmd in [
            "add_node",
            "add_boundary",
            "set_automation",
            "set_field",
            "connect",
            "edit_song",
            "edit_arrange",
        ] {
            let mut ex = normalize_command_value(command_example(Some(cmd)));
            fill_placeholders(&mut ex);
            serde_json::from_value::<EditorCommand>(ex.clone())
                .unwrap_or_else(|e| panic!("example for {cmd} does not decode: {e}\n{ex}"));
        }
        // The generic example is a dispatch_refs batch — each element must decode
        // once its `ref` label is dropped and `$refs` are substituted.
        let Value::Array(batch) = command_example(None) else {
            panic!("generic example should be a batch");
        };
        let refs: std::collections::HashMap<String, String> = batch
            .iter()
            .filter_map(|c| c.get("ref").and_then(Value::as_str))
            .map(|r| {
                (
                    r.to_string(),
                    "00000000-0000-0000-0000-000000000000".to_string(),
                )
            })
            .collect();
        for mut item in batch {
            if let Some(o) = item.as_object_mut() {
                o.remove("ref");
            }
            substitute_refs(&mut item, &refs);
            let item = normalize_command_value(item);
            serde_json::from_value::<EditorCommand>(item.clone())
                .unwrap_or_else(|e| panic!("generic example element does not decode: {e}\n{item}"));
        }
    }

    #[test]
    fn set_automation_event_shape_decodes() {
        // The worst agentic loop in the field notes: four retries, each error
        // revealing one more field, never the full shape. Lock the full shape.
        let events: Vec<AutomationEvent> = serde_json::from_value(serde_json::json!([
            {"event":"set_value","args":{"value":0.0,"time":0.0}},
            {"event":"linear_ramp","args":{"value":1.0,"time":0.01}},
            {"event":"exponential_ramp","args":{"value":0.001,"time":0.4}},
            {"event":"set_target","args":{"target":0.5,"start_time":0.2,"time_constant":0.05}},
            {"event":"cancel_scheduled","args":{"time":1.0}}
        ]))
        .expect("documented automation event shapes decode");
        assert_eq!(events.len(), 5);
    }

    #[test]
    fn generated_vocabulary_index_tracks_the_schema() {
        // The generated index must list every escape-hatch tag — including the
        // ones this change-set added — straight from the schemars schemas.
        let doc = vocabulary_doc();
        for needle in [
            "add_node",
            "set_automation",
            "add_boundary",
            "set_sample_notes",
            "set_sections",
            "modulation_targets",
            "set_value_curve",
            "oscillator",
        ] {
            assert!(doc.contains(needle), "vocabulary index missing {needle}");
        }
    }

    #[test]
    fn new_command_shapes_decode() {
        // set_sample_notes + edit_arrange set_sections (this change-set's new
        // escape-hatch shapes).
        let cmd: EditorCommand = serde_json::from_value(serde_json::json!(
            {"cmd":"set_sample_notes","args":{
                "id":"00000000-0000-0000-0000-000000000000","notes":"keeper"}}
        ))
        .expect("set_sample_notes decodes");
        assert!(matches!(cmd, EditorCommand::SetSampleNotes { .. }));
        let cmd: EditorCommand = serde_json::from_value(serde_json::json!(
            {"cmd":"edit_arrange","args":{"op":{"op":"set_sections","args":{
                "sections":[{"name":"intro","start":0.0,"end":8.0}]}}}}
        ))
        .expect("set_sections decodes");
        assert!(matches!(cmd, EditorCommand::EditArrange { .. }));
    }

    #[test]
    fn add_boundary_command_shape_decodes() {
        // `command 2: missing field port` — lock the corrected shape.
        let cmd: EditorCommand = serde_json::from_value(serde_json::json!(
            {"cmd":"add_boundary","args":{"port":"outlet","x":0.0,"y":0.0}}
        ))
        .expect("add_boundary with `port` decodes");
        assert!(matches!(cmd, EditorCommand::AddBoundary { .. }));
    }
}

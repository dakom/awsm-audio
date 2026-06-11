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

use awsm_audio_editor_protocol::schema::{NodeId, NodeKind, SampleId, SampleKind};
use awsm_audio_editor_protocol::{
    ArrangeOp, AssetInfo, EditorCommand, EditorQuery, FieldValue, PlacedClip, QueryResult, Request,
    Response,
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
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WaveformParams {
    /// Sound to render. Omit to render the project root.
    #[serde(default)]
    pub sample: Option<SampleId>,
    /// Number of min/max buckets (envelope columns) to return.
    pub buckets: u32,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct AddNodeParams {
    /// The node kind to create — a `NodeKind` value, adjacently tagged
    /// (`{"kind":"<tag>","props":{…}}`). The param schema lists every kind and its
    /// props; `list_node_kinds` gives a copy-paste `example` + docs per kind. The
    /// editor mints the node id — read it back from a follow-up `get_snapshot`.
    pub kind: Flexible<NodeKind>,
    pub x: f64,
    pub y: f64,
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
    /// Numeric value (most fields). For a text/bool field, use `dispatch_command`
    /// with a `FieldValue`.
    pub value: f64,
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
    /// Loop/export start marker (seconds). Omit BOTH start and end to clear the
    /// markers (loop + export span the whole timeline).
    #[serde(default)]
    pub start: Option<f64>,
    /// Loop/export end marker (seconds). Must be > start to take effect.
    #[serde(default)]
    pub end: Option<f64>,
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
    /// Clip start on the timeline, in seconds (use `beats_to_secs` for beat math).
    pub start: f64,
    /// Bounced Sound to place (from `list_assets`/`list_samples`). Must be bounced.
    pub source: SampleId,
    /// Timeline length in seconds; omit to use the full bounce duration.
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
        endpoint nodes."
    )]
    async fn get_snapshot(&self) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Snapshot).await
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

    #[tool(description = "One Sound's bounce status (none / clean / dirty).")]
    async fn get_bounce_status(
        &self,
        Parameters(p): Parameters<SampleReq>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::BounceStatus { sample: p.sample })
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
        };
        self.wav(req).await
    }

    #[tool(description = "Cheap numeric stats of a Sound's offline render: \
        duration_secs, peak, rms, channels, sample_rate. Omit `sample` for the \
        root.")]
    async fn wav_stats(
        &self,
        Parameters(p): Parameters<SampleArg>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::WavStats { sample: p.sample }).await
    }

    #[tool(
        description = "A downsampled min/max envelope (`buckets` columns) of a \
        Sound's render, so you can reason about the waveform shape in text. Omit \
        `sample` for the root."
    )]
    async fn waveform(
        &self,
        Parameters(p): Parameters<WaveformParams>,
    ) -> Result<CallToolResult, McpError> {
        self.query(EditorQuery::Waveform {
            sample: p.sample,
            buckets: p.buckets,
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
        The editor mints the id; read it back with get_snapshot."
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

    #[tool(description = "Set a numeric setting on a node (the SetField command).")]
    async fn set_field(
        &self,
        Parameters(p): Parameters<SetFieldParams>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::SetField {
            id: p.node,
            key: p.key,
            value: FieldValue::Num(p.value),
        })
        .await
    }

    #[tool(
        description = "Render a Sound (`sample`) offline and store it as that \
        sample's bounce (so it can be dropped into an arrangement). Pass \
        `duration_secs` to capture a fixed span of a procedural / worklet source \
        that otherwise renders only a tiny default. Blocks until the render lands \
        (bounce status → clean) or a ~30 s safety timeout, so you can inspect \
        wav_stats / waveform immediately after — no manual polling. On timeout it \
        returns the last status; re-check get_bounce_status / list_assets."
    )]
    async fn bounce(
        &self,
        Parameters(p): Parameters<BounceParams>,
    ) -> Result<CallToolResult, McpError> {
        use std::time::{Duration, Instant};
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
                return Ok(text(format!(
                    "bounce complete (status: clean) for {}",
                    p.sample
                )));
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
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::SetMarkers {
                start: p.start,
                end: p.end,
            },
        })
        .await
    }

    // ── arrangement editing (dedicated wrappers over EditArrange) ────────────
    // These edit the *active* sample, which must be an Arrangement — switch to
    // one with set_active_sample (or create_arrangement) first.

    #[tool(
        description = "Create a new (empty) Arrangement sample and make it active. \
        Build it with add_arrangement_track + add_clip; find its id via list_samples."
    )]
    async fn create_arrangement(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::AddSample {
            kind: SampleKind::Arrangement,
        })
        .await
    }

    #[tool(description = "Append an empty track to the active arrangement.")]
    async fn add_arrangement_track(&self) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::AddTrack).await
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
        description = "Place a bounced Sound as a clip on a track at `start` seconds. \
        `source` must be bounced. `length` defaults to the full bounce duration. Use \
        beats_to_secs to turn beats/bars into the `start` seconds."
    )]
    async fn add_clip(
        &self,
        Parameters(p): Parameters<AddClipParams>,
    ) -> Result<CallToolResult, McpError> {
        self.arrange(ArrangeOp::AddClip {
            track: p.track,
            start: p.start,
            source: p.source,
            length: p.length,
        })
        .await
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
            Response::Ok => Ok(text(format!(
                "started bouncing {} sound(s): {}",
                names.len(),
                names.join(", ")
            ))),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
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
                    Some("dirty") => {
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
        description = "Return the recommended Cargo.toml dependency snippet for \
        authoring an awsm-audio WASM DSP worklet (the crates.io release). Paste it \
        into your worklet crate, then follow the awsm-audio://docs/worklet-abi guide \
        (full Rust API: https://docs.rs/awsm-audio-worklet/latest). Use a worklet \
        when no built-in node does the job — chorus/flanger, phaser, bitcrusher, \
        ring modulator, custom grain/spectral effects."
    )]
    async fn worklet_cargo_toml(&self) -> Result<CallToolResult, McpError> {
        Ok(text(WORKLET_CARGO_TOML))
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
                label,
            })
            .await?
        {
            Response::Ok => Ok(text(
                "ok — params discovered; call get_snapshot to see them",
            )),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
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
        match self.req(Request::DispatchBatch(cmds)).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
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
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }

    /// Dispatch an [`ArrangeOp`] against the active Arrangement sample.
    async fn arrange(&self, op: ArrangeOp) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::EditArrange { op }).await
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
             or a URL onto it. For DSP no built-in node provides — chorus / flanger, \
             phaser, bitcrusher, ring modulator, custom grain/spectral effects — use \
             an audio_worklet: read the awsm-audio://docs/worklet-abi resource, author \
             + build a worklet crate, and attach it with the attach_wasm tool.\n\n\
             For a song / full-track / genre request, work arrangement-first instead \
             of one monolithic root sequencer: build and bounce each part (drums, \
             bass, chords/skank, FX) as its own short loop Sound, then create_arrangement \
             and place clips into sections (intro / drop / switch / outro) — \
             add_arrangement_track + add_clip, with beats_to_secs / duplicate_clips for \
             the timing. Check wav_stats / waveform after every major bounce (they catch \
             a too-short render, a hot/clipping bounce, overlapping clips). See the \
             awsm-audio://docs/genres resource for per-genre checklists."
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
                "awsm-audio://docs/genres",
                "Genre style checklists",
                "Arrangement-first workflow + short per-genre style checklists \
                 (drums, bass, harmony, FX, sectioning) for common electronic genres.",
            ),
        ]))
    }

    async fn read_resource(
        &self,
        req: ReadResourceRequestParams,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let body = match req.uri.as_str() {
            "awsm-audio://docs/vocabulary" => VOCABULARY_DOC,
            "awsm-audio://docs/worklet-abi" => WORKLET_ABI_DOC,
            "awsm-audio://docs/genres" => GENRES_DOC,
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

`add_node` accepts either a kind-name string (defaults filled in):

    { "kind": "oscillator", "x": 0, "y": 0 }

or a full value (copy a kind's `example` from list_node_kinds):

    { "kind": {"kind":"biquad_filter","props":{"type":"lowpass",
      "frequency":{"value":1000.0},"q":{"value":1.0},"gain":{"value":0.0},
      "detune":{"value":0.0}}}, "x": 0, "y": 0 }

A NodeKind value is adjacently tagged: `{"kind":"<tag>","props":{…}}`.

## EditorCommand (dispatch_command)

Adjacently tagged by `cmd`/`args`. Examples:

    {"cmd":"set_field","args":{"id":"<node>","key":"frequency","value":{"t":"num","v":880.0}}}
    {"cmd":"connect","args":{"from":"<a>","from_output":0,"to":"<b>","to_input":0}}
    {"cmd":"add_sample","args":{"kind":"arrangement"}}   // kind: "sound" | "arrangement"

`set_field`'s `value` is a FieldValue: `{"t":"num","v":1.0}`, `{"t":"text","v":"x"}`,
or `{"t":"bool","v":true}`. (The set_field *tool* takes a plain number; use this
form only via dispatch_command for text/bool fields.)

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

1. list_node_kinds → pick kinds.
2. add_node (source) + add_node (effects); connect them; the last unconnected
   output auditions to master.
3. set_field to shape it; render_wav / wav_stats / waveform to inspect.
4. bounce a Sound, then build an Arrangement (add_sample arrangement →
   edit_arrange add_track / add_clip). render_wav / wav_stats / waveform work on
   an arrangement sample too — they render its clip timeline. Optionally set
   loop/export markers (set_arrangement_markers, or edit_arrange set_markers) to
   render just a region; clear them to render the whole timeline.

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

**When to use one:** reach for a worklet whenever no built-in node does the job —
chorus / flanger (modulated delay), phaser (all-pass chain), bitcrusher, ring
modulator, custom grain / spectral effects. The full Rust API of the
`awsm-audio-worklet` crate is at <https://docs.rs/awsm-audio-worklet/latest>.

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
awsm-audio-worklet = "0.1"
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

/// Arrangement-first workflow + per-genre style checklists, served as the
/// `awsm-audio://docs/genres` resource. Nudges song requests toward bounced
/// loops + a sectioned arrangement instead of one monolithic root sequencer.
const GENRES_DOC: &str = r#"# Building a track: arrangement-first + genre checklists

## The workflow (any genre)

Don't build one giant root sequencer. Build *parts*, bounce them, arrange them:

1. For each part — drums, bass, chords/skank, FX/calls — author a short loop Sound
   (graph or sequencer-driven instrument), then `bounce` it. Check `wav_stats` /
   `waveform` after each bounce (catch a too-short render, a hot/clipping bounce).
2. `create_arrangement`; `add_arrangement_track` one per part; name them
   (`set_track_name`).
3. Place clips into sections — intro / drop / switch / outro — with `add_clip`
   (use `beats_to_secs` for the `start`; `get_arrangement` has the BPM).
4. Tile loops across a section with `duplicate_clips` (interval in bars/beats).
5. Balance with `set_track_gain` / `set_clip_gain`. Set loop/export markers
   (`set_arrangement_markers`) to render a region.
6. Before exporting: `arrangement_bounce_report` to spot stale/missing clip
   bounces, `bounce_all_dirty` to fix them, then `render_wav` / `wav_stats` on the
   arrangement (watch for clipping from overlapping clips).

## Per-genre checklists

### Ragga jungle (~160–175 BPM)
- Chopped break (e.g. Amen): slice + re-sequence, don't loop it flat.
- Deep sub bass (sine/triangle), often a reggae-style bassline.
- Offbeat reggae "skank" stabs (organ/stab on the off-beats).
- Dub FX: delay + reverb throws on stabs and one-shots.
- Siren / call-and-response vocal-style accents.
- Sectioned arrangement: intro (filtered/sparse) → drop (full break+sub) →
  switch (re-chop / bass change) → outro.

(Other genres follow the same shape — swap the parts: e.g. house = four-on-the-floor
kick, offbeat hats, bass, chord stabs, vocal/FX; techno = driving kick, rumble bass,
percussion layers, atmospheric FX. Build each as a loop, bounce, arrange.)
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

fn unexpected_query(qr: QueryResult) -> McpError {
    McpError::internal_error(format!("unexpected query result: {qr:?}"), None)
}

/// The crates.io dependency snippet returned by `worklet_cargo_toml`. Kept in
/// lockstep with the published `awsm-audio-worklet` version.
const WORKLET_CARGO_TOML: &str = r#"# Cargo.toml for an awsm-audio WASM DSP worklet
[package]
name = "my-worklet"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
awsm-audio-worklet = "0.1"

# Build: cargo build -p my-worklet --target wasm32-unknown-unknown --release
# Attach the resulting .wasm with the attach_wasm tool.
# Guide: awsm-audio://docs/worklet-abi   API: https://docs.rs/awsm-audio-worklet/latest
#
# A std crate (the above) "just works". For a smaller `#![no_std]` crate, call the
# macro as `awsm_worklet!(MyProc, no_std);` — that form also emits the
# `#[panic_handler]` a no_std cdylib requires (otherwise the build fails at link
# with "`#[panic_handler]` function required, but not found").
"#;

/// A tool argument that is **strongly typed** — its JSON Schema is exactly `T`'s,
/// so a fresh agent sees the precise shape — yet tolerant of clients that deliver
/// a nested object as a JSON *string* (it deserializes from either form). Typed,
/// self-documenting, and robust.
#[derive(Debug, Clone)]
pub struct Flexible<T>(pub T);

impl<'de, T: serde::de::DeserializeOwned> serde::Deserialize<'de> for Flexible<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let inner = match Value::deserialize(d)? {
            Value::String(s) => serde_json::from_str(&s).map_err(Error::custom)?,
            other => serde_json::from_value(other).map_err(Error::custom)?,
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

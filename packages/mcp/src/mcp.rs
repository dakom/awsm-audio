//! The rmcp tool layer. Each tool is a thin typed wrapper that builds a protocol
//! [`Request`] and relays it to the attached editor over the WebTransport link,
//! then shapes the [`Response`] into an MCP result. All editor mutation flows
//! through `EditorController` on the far side; this layer only translates.
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

use awsm_audio_editor_protocol::schema::{NodeId, NodeKind, SampleId};
use awsm_audio_editor_protocol::{
    ArrangeOp, EditorCommand, EditorQuery, FieldValue, Request, Response,
};

use crate::link::EditorLink;

/// The MCP tool provider. Cheap to clone (the link is an `Arc` handle).
#[derive(Clone)]
pub struct EditorMcp {
    link: EditorLink,
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

// ──────────────────────────────── tools ─────────────────────────────────────

#[tool_router]
impl EditorMcp {
    pub fn new(link: EditorLink) -> Self {
        Self {
            link,
            tool_router: Self::tool_router(),
        }
    }

    // ── discovery / read ────────────────────────────────────────────────────

    #[tool(
        description = "Full editor snapshot: graph (nodes + connections), node \
        layout, camera, selection, and the active arrangement. The starting point \
        for discovering node/sample ids."
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
        or use wav_stats/waveform to reason about it. Omit `sample` for the root."
    )]
    async fn render_wav(
        &self,
        Parameters(p): Parameters<RenderWavParams>,
    ) -> Result<CallToolResult, McpError> {
        let req = Request::RenderWav {
            sample: p.sample,
            sample_rate: p.sample_rate,
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

    #[tool(description = "Start playback of the root Sound / arrangement.")]
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
        sample's bounce (so it can be dropped into an arrangement)."
    )]
    async fn bounce(
        &self,
        Parameters(p): Parameters<SampleReq>,
    ) -> Result<CallToolResult, McpError> {
        self.dispatch(EditorCommand::Bounce { sample: p.sample })
            .await
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

    // ── worklet authoring ────────────────────────────────────────────────────

    #[tool(
        description = "Attach a compiled WASM DSP module to an AudioWorklet node. \
        Author a crate against awsm-audio-worklet (see the awsm-audio://docs/worklet-abi \
        resource), `cargo build --target wasm32-unknown-unknown --release`, then \
        pass the .wasm path here. On success the node's discovered params show up \
        in get_snapshot. A bad module returns the compile/ABI error."
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
            .request(&r)
            .await
            .map_err(|e| McpError::internal_error(e.to_string(), None))
    }

    async fn dispatch(&self, cmd: EditorCommand) -> Result<CallToolResult, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Ok => Ok(text("ok")),
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

    /// RenderWav → save the `.wav` to a temp file, return the path + a one-line
    /// summary. (An agent can't hear bytes; the human/tooling opens the file.)
    async fn wav(&self, r: Request) -> Result<CallToolResult, McpError> {
        match self.req(r).await? {
            Response::Wav(bytes) => {
                let path = std::env::temp_dir().join("awsm-audio-mcp-last.wav");
                std::fs::write(&path, &bytes)
                    .map_err(|e| McpError::internal_error(format!("write wav: {e}"), None))?;
                Ok(text(format!(
                    "wrote {} bytes to {}",
                    bytes.len(),
                    path.display()
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
            "Drive the awsm-audio node-graph WebAudio editor. Call get_snapshot to \
             discover node/sample ids, mutate with the graph/sequencer/arrangement \
             tools (or dispatch_command / dispatch_batch for anything without a \
             dedicated tool), bounce a Sound and call render_wav / wav_stats / \
             waveform to inspect the result. To add a custom DSP node, read the \
             awsm-audio://docs/worklet-abi resource, author + build a worklet crate, and \
             attach it with the attach_wasm tool."
                .to_string(),
        );
        info
    }

    // ── push channel: forward editor events as MCP logging notifications ─────
    async fn on_initialized(&self, context: NotificationContext<RoleServer>) {
        let mut rx = self.link.subscribe_events();
        let peer = context.peer;
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(ev) => {
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
"#;

/// The worklet-authoring guide served both as the `awsm-audio://docs/worklet-abi`
/// resource and the `author_worklet` prompt, so an agent can write a correct
/// crate without reading the repo.
const WORKLET_ABI_DOC: &str = r#"# Authoring an awsm-audio WASM DSP worklet

An AudioWorklet node runs a **native Rust → wasm** DSP processor you author,
compile, and attach. The MCP server only relays the bytes — you compile locally
(so you get cargo's errors directly) and pass the `.wasm` to `attach_wasm`.

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
awsm-audio-worklet = { path = "PATH/TO/packages/crates/worklet" }
```

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
"#;

fn text(s: impl Into<String>) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s.into())])
}

fn unexpected(resp: Response) -> McpError {
    McpError::internal_error(format!("unexpected response: {resp:?}"), None)
}

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

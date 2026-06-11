//! [`EditorCommand`] — the serializable description of every editor *document*
//! mutation.
//!
//! Every change to the saved document goes through one command, dispatched to
//! `EditorController::dispatch` in the editor crate. Because the enum is
//! serde-derived, this same command stream is exactly what the MCP/WebSocket
//! transport feeds in — the transport is a thin adapter over `dispatch`.
//! Authoring a whole song over MCP is "send these commands."

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{
    AutomationEvent, Clip, ControlPoint, Curve, NodeId, NodeKind, NoteEvent, ParamId, SampleId,
    SampleKind,
};

use crate::clipboard::Clipboard;
use crate::field::FieldValue;
use crate::node::{BoundaryPort, ConnId};

/// Adjacently tagged (`cmd` + `args`) so it round-trips through TOML/JSON.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "cmd", content = "args")]
pub enum EditorCommand {
    /// Create a node of `kind` at world position `(x, y)` and select it.
    AddNode { kind: NodeKind, x: f64, y: f64 },
    /// Move a node to a new world position (transient — fires continuously
    /// during a drag).
    MoveNode { id: NodeId, x: f64, y: f64 },
    /// Remove a node and every wire touching it.
    RemoveNode { id: NodeId },
    /// Duplicate a node (same kind + settings) offset from the original.
    CloneNode { id: NodeId },
    /// Set one editable setting on a node (see the editor's `fields`).
    SetField {
        id: NodeId,
        key: String,
        value: FieldValue,
    },
    /// Replace the automation timeline of a node's named AudioParam (envelope).
    SetAutomation {
        id: NodeId,
        param: String,
        events: Vec<AutomationEvent>,
    },
    /// Wire an output port to an input port. `from_output` / `to_input` default
    /// to 0 (the common single-port case) when omitted — so a bare
    /// `{from, to}` connects port 0 → port 0 via dispatch_command / dispatch_batch
    /// too, matching the dedicated `connect` tool.
    Connect {
        from: NodeId,
        #[serde(default)]
        from_output: u32,
        to: NodeId,
        #[serde(default)]
        to_input: u32,
    },
    /// Wire an output port to a node's automatable parameter (modulation).
    /// `from_output` defaults to 0 when omitted.
    Modulate {
        from: NodeId,
        #[serde(default)]
        from_output: u32,
        to: NodeId,
        param: ParamId,
    },
    /// Bind a sequencer's keyed output (`from_output` = its sound/lane/zone port)
    /// to an instrument-ref's trigger inlet — a `SeqOut → Trigger` wire.
    /// `from_output` defaults to 0 when omitted.
    Bind {
        from: NodeId,
        #[serde(default)]
        from_output: u32,
        to: NodeId,
    },
    /// Remove a single wire by its editor id.
    Disconnect {
        // `ConnId` is a bare `Uuid`; describe it as a uuid string for JSON Schema.
        #[cfg_attr(feature = "schemars", schemars(with = "String"))]
        id: ConnId,
    },
    /// Edit a Note Sequencer node's song / sound outputs (see [`SongOp`]).
    EditSong { node: NodeId, op: SongOp },
    /// Edit a Control Sequencer node's lanes / breakpoints (see [`ControlOp`]).
    EditControl { node: NodeId, op: ControlOp },
    /// Edit the active Arrangement sample's tracks / clips (see [`ArrangeOp`]).
    /// Unlike Song/Control, an arrangement isn't a canvas node — it lives on the
    /// active sample — so this op carries no node id.
    EditArrange { op: ArrangeOp },
    /// Render a Sound (`sample`) offline to a PCM buffer and store it as that
    /// sample's [`Bounce`](awsm_audio_schema::Bounce). Mutates the document (the
    /// bounce + embedded buffer), so it's a command — and MCP-drivable. The render
    /// itself is async; this kicks it off. `duration_secs` overrides the
    /// auto-computed render length (song loop length, or a fixed one-shot window) —
    /// pass it to capture a fixed span of a procedural / worklet source that would
    /// otherwise render only a tiny default. `None` keeps the default.
    Bounce {
        sample: SampleId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_secs: Option<f64>,
    },
    /// Replace (or, with `additive`, extend) the selection.
    SelectNodes { ids: Vec<NodeId>, additive: bool },
    /// Clear the selection.
    ClearSelection,
    /// Set the canvas camera (pan in screen px, zoom factor).
    SetCamera { pan_x: f64, pan_y: f64, zoom: f64 },

    // ---- Document structure: samples (the project's instruments/arrangements) ----
    /// Create a new empty sample of `kind` and make it active.
    AddSample { kind: SampleKind },
    /// Delete a sample (never the last one); repoints root/active if needed.
    RemoveSample { id: SampleId },
    /// Duplicate a sample (graph, trigger, arrangement, bounce) under a new id
    /// with " (clone)" appended to the name, and make the copy active.
    CloneSample { id: SampleId },
    /// Rename a sample.
    RenameSample { id: SampleId, name: String },
    /// Mark a sample as the project root (the one that plays / exports).
    SetRoot { id: SampleId },

    // ---- Canvas extras (nodes that aren't plain `AddNode` kinds) ----
    /// Add an inlet/outlet boundary node at world `(x, y)`.
    AddBoundary { port: BoundaryPort, x: f64, y: f64 },
    /// Add a Sample-reference node targeting `sample` at world `(x, y)`.
    AddSampleRef { sample: SampleId, x: f64, y: f64 },
    /// Point an existing Sample-reference node at a different sample.
    SetSampleRef { node: NodeId, sample: SampleId },
    /// Rename a node (empty label clears it back to the type name).
    RenameNode { id: NodeId, label: String },
    /// Set an inlet boundary node's default value.
    SetInputDefault { node: NodeId, value: f32 },
    /// Set (or add) a per-instance inlet override on a Sample-reference node.
    SetInputValue {
        node: NodeId,
        port: String,
        value: f32,
    },

    // ---- Scene ----
    /// Set the spatial listener position.
    SetListener { x: f32, y: f32, z: f32 },

    // ---- Composite canvas edits ----
    /// Encapsulate the given nodes into a new sub-sample, wiring a Sample-ref in
    /// their place (auto-creating inlets/outlets at the cut wires).
    Encapsulate { ids: Vec<NodeId> },
    /// Paste a clipboard payload onto the canvas (new ids, offset, selected).
    Paste { clip: Clipboard },
}

/// A clip plus the track it belongs on — the serde-friendly element of a
/// multi-clip paste (a named struct, not a tuple, so it round-trips through TOML).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacedClip {
    pub track: usize,
    pub clip: Clip,
}

/// A single edit to a Note Sequencer node. Sound outputs are auto-derived from
/// the song (one per melodic track / per drum note) and addressed by index.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum SongOp {
    SetBpm(f64),
    SetStart(f64),
    /// Playback-window stop in beats; `None` plays to the song's end.
    SetEnd(Option<f64>),
    /// Authored grid length in beats (`0` = auto-fit content).
    SetLength(f64),
    SetLooping(bool),
    /// Append an empty track (and regenerate its sound output).
    AddTrack,
    AddNote {
        track: usize,
        event: NoteEvent,
    },
    UpdateNote {
        track: usize,
        index: usize,
        event: NoteEvent,
    },
    RemoveNote {
        track: usize,
        index: usize,
    },
    /// Replace **all** events of one track in a single op (and regenerate its
    /// sound outputs). Lets a full pattern land in one round-trip instead of one
    /// AddNote per note. Out-of-range `track` is ignored.
    SetTrackEvents {
        track: usize,
        events: Vec<NoteEvent>,
    },
    SetOutputTranspose {
        index: usize,
        semitones: i32,
    },
    SetOutputGain {
        index: usize,
        gain: f32,
    },
    SetOutputLabel {
        index: usize,
        label: String,
    },
}

/// A single edit to a Control Sequencer node. Lanes (and their breakpoints) are
/// addressed by index; each lane is an output wired to a parameter.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum ControlOp {
    SetBpm(f64),
    SetStart(f64),
    SetLooping(bool),
    AddLane,
    RemoveLane {
        index: usize,
    },
    SetLaneLabel {
        index: usize,
        label: String,
    },
    AddPoint {
        lane: usize,
        beat: f64,
        value: f32,
    },
    RemovePoint {
        lane: usize,
        index: usize,
    },
    SetPoints {
        lane: usize,
        points: Vec<ControlPoint>,
    },
    /// Set the curve shape of the segment *reaching* point `index` from the
    /// previous point. Cycled in the lane editor; drivable for MCP.
    SetPointCurve {
        lane: usize,
        index: usize,
        curve: Curve,
    },
}

/// A single edit to the active Arrangement. Tracks and clips are addressed by
/// index. Structural edits (add/remove/split/move) push undo; continuous drags
/// (move/resize) are transient — the UI pushes one undo on drag start.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "op", content = "args")]
pub enum ArrangeOp {
    SetBpm(f64),
    /// Timeline length in seconds.
    SetLengthSecs(f64),
    /// Set or clear the loop/export markers (seconds). Both `None` clears them
    /// (loop + export span the whole timeline). With both set and `end > start`,
    /// playback loops the region and export renders exactly it.
    SetMarkers {
        #[serde(default)]
        start: Option<f64>,
        #[serde(default)]
        end: Option<f64>,
    },
    AddTrack,
    RemoveTrack {
        track: usize,
    },
    SetTrackName {
        track: usize,
        name: String,
    },
    SetTrackGain {
        track: usize,
        gain: f32,
    },
    SetTrackMute {
        track: usize,
        mute: bool,
    },
    /// Solo a track. If any track is soloed, only soloed (non-muted) tracks play.
    SetTrackSolo {
        track: usize,
        solo: bool,
    },
    /// Drop a bounced Sound (`source`) as a clip on `track` at `start` seconds.
    /// `length` (timeline seconds) defaults to the full bounce duration; the Draw
    /// tool passes a shorter length so a long Sound needn't fill the whole bar.
    AddClip {
        track: usize,
        start: f64,
        source: SampleId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        length: Option<f64>,
    },
    RemoveClip {
        track: usize,
        clip: usize,
    },
    /// Insert a fully-specified clip (paste) on `track`; `clip.start` is where it
    /// lands. Preserves source/offset/length/gain/loop/name — the copy/paste path.
    PasteClip {
        track: usize,
        clip: Clip,
    },
    /// Insert several clips at once (multi-clip paste) — one undo.
    PasteClips {
        clips: Vec<PlacedClip>,
    },
    /// Move a clip — possibly to another track (`new_track`) and a new `start` (s).
    MoveClip {
        track: usize,
        clip: usize,
        new_track: usize,
        start: f64,
    },
    /// Change a clip's timeline length in seconds (right-edge trim).
    ResizeClip {
        track: usize,
        clip: usize,
        length: f64,
    },
    /// Time-stretch: set a clip's timeline `length` and playback `speed` together
    /// (the same buffer content scaled to a new length; pitch shifts with speed).
    StretchClip {
        track: usize,
        clip: usize,
        length: f64,
        speed: f32,
    },
    /// Set a clip's start offset into its buffer in seconds (left-edge trim).
    SetClipOffset {
        track: usize,
        clip: usize,
        offset: f64,
    },
    /// Atomic left-edge trim: drag the clip's start later while keeping its right
    /// edge fixed. Sets `start` (timeline secs) and `offset` (into buffer) together.
    TrimStart {
        track: usize,
        clip: usize,
        start: f64,
        offset: f64,
    },
    /// Split a clip at `at` (timeline seconds) into two.
    SplitClip {
        track: usize,
        clip: usize,
        at: f64,
    },
    /// Set a clip's gain (linear).
    SetClipGain {
        track: usize,
        clip: usize,
        gain: f32,
    },
    /// Loop the clip's buffer to fill its length.
    SetClipLoop {
        track: usize,
        clip: usize,
        looping: bool,
    },
    /// Remove every clip from every track, leaving the (empty) tracks in place —
    /// a one-shot reset so an agent can rebuild an arrangement without removing
    /// and re-adding tracks.
    Clear,
}

impl EditorCommand {
    /// Whether `dispatch` should skip its automatic undo snapshot for this
    /// command. True for continuous/view-only gestures (move, select, camera),
    /// and for the structured `Edit{Song,Control,Live}` commands — those manage
    /// their own snapshots internally (only structural edits push undo, so a
    /// value tweak doesn't spam the stack).
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            EditorCommand::MoveNode { .. }
                | EditorCommand::SelectNodes { .. }
                | EditorCommand::ClearSelection
                | EditorCommand::SetCamera { .. }
                | EditorCommand::EditSong { .. }
                | EditorCommand::EditControl { .. }
                | EditorCommand::EditArrange { .. }
                | EditorCommand::Bounce { .. }
                // Sample-list + scene state aren't in the undo snapshot (which
                // captures the active canvas), so they don't push one — but they
                // still flow through `dispatch` for MCP.
                | EditorCommand::AddSample { .. }
                | EditorCommand::RemoveSample { .. }
                | EditorCommand::CloneSample { .. }
                | EditorCommand::RenameSample { .. }
                | EditorCommand::SetRoot { .. }
                | EditorCommand::SetListener { .. }
        )
    }
}

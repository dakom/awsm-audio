//! Editor-side view structures wrapping the audio schema.
//!
//! The audio truth (a node's [`NodeKind`]) comes straight from
//! `awsm-audio-schema`; everything else here — world position, selection, the
//! resolved endpoints of a wire — is editor view-state that never enters the
//! schema. Each is a shared `Mutable`/`Rc` handle so the reactive UI updates
//! granularly as the controller mutates it.

use std::cell::RefCell;
use std::rc::Rc;

use awsm_audio_schema::{NodeId, NodeKind};
use futures_signals::signal::Mutable;

// The pure-data wire vocabulary (`BoundaryPort` / `ConnId` / `ConnSink`) now
// lives in the shared protocol crate; the live `Mutable`/`Rc` node + connection
// structs below stay here and use the re-exported names.
pub use awsm_audio_editor_protocol::{BoundaryPort, ConnId, ConnSink};

/// A node placed on the canvas.
pub struct EditorNode {
    pub id: NodeId,
    /// The audio node definition. Mutable so inline param edits (the `SetField`
    /// command) write straight through; the schema projection reads it back. For
    /// boundary nodes this is an unused placeholder.
    pub kind: RefCell<NodeKind>,
    /// Human label shown in the header (empty = fall back to the type name). For
    /// boundary nodes this is the port name (an inlet/outlet id).
    pub label: Mutable<String>,
    /// If set, this is a graph boundary port rather than an audio node (it maps
    /// to a `PortDecl` + an `Inlet`/`Outlet` connection endpoint in the schema).
    pub boundary: Option<BoundaryPort>,
    /// World-space top-left position.
    pub pos: Mutable<(f64, f64)>,
    /// Whether this node is in the current selection.
    pub selected: Mutable<bool>,
    /// Inlet boundary nodes only: the input.s default value (round-trips to the
    /// [`PortDecl`](awsm_audio_schema::PortDecl)). Unused on other nodes.
    pub default: Mutable<f32>,
}

impl EditorNode {
    pub fn new(id: NodeId, kind: NodeKind, pos: (f64, f64)) -> Rc<Self> {
        Rc::new(Self {
            id,
            kind: RefCell::new(kind),
            label: Mutable::new(String::new()),
            boundary: None,
            pos: Mutable::new(pos),
            selected: Mutable::new(false),
            default: Mutable::new(0.0),
        })
    }

    /// Create a boundary (inlet/outlet) node with a port `name`.
    pub fn boundary(
        id: NodeId,
        port: BoundaryPort,
        name: impl Into<String>,
        pos: (f64, f64),
    ) -> Rc<Self> {
        Rc::new(Self {
            id,
            // A placeholder kind that's never materialized (boundary nodes don't
            // become schema nodes — they become PortDecls).
            kind: RefCell::new(NodeKind::Gain(Default::default())),
            label: Mutable::new(name.into()),
            boundary: Some(port),
            pos: Mutable::new(pos),
            selected: Mutable::new(false),
            default: Mutable::new(0.0),
        })
    }
}

/// A committed wire, with both endpoints resolved to live node handles so the
/// renderer can derive its path from their position signals directly.
pub struct EditorConnection {
    pub id: ConnId,
    pub from: Rc<EditorNode>,
    pub from_output: u32,
    pub to: Rc<EditorNode>,
    pub sink: ConnSink,
}

/// Live state while dragging an envelope breakpoint in the inspector plot. The
/// edited `events` are previewed (the plot reads them) and only committed —
/// dispatched as `SetAutomation` — on release.
#[derive(Clone)]
pub struct EnvDrag {
    pub node: NodeId,
    pub key: String,
    pub index: usize,
    pub events: Vec<awsm_audio_schema::AutomationEvent>,
}

/// A wire being dragged out from an output port, before it lands on an input.
pub struct PendingWire {
    pub from: Rc<EditorNode>,
    pub from_output: u32,
    /// Current cursor position in world space (follows the pointer).
    pub cursor: Mutable<(f64, f64)>,
}

// (A resolved port reference type will land here when param-modulation and
// sample-outlet wiring are added; audio node→node wiring needs only the node
// handle + index carried by `PendingWire` / `EditorConnection`.)

/// Transient pointer-drag gesture state. Lives on the controller so the node
/// elements (which start drags) and the canvas (which tracks pointer moves)
/// share one source of truth. Never serialized.
#[derive(Clone)]
pub enum DragState {
    /// Panning the canvas: cursor + pan at gesture start (screen px).
    Pan {
        start_cx: f64,
        start_cy: f64,
        start_px: f64,
        start_py: f64,
    },
    /// Moving the selected node(s) as a rigid group: each node's grab offset
    /// from its origin (world units), so a multi-selection drags together.
    Node { items: Vec<(NodeId, f64, f64)> },
    /// Rubber-band box select: the anchor corner in world units.
    Box { start_x: f64, start_y: f64 },
}

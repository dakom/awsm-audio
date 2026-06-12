//! `EditorController` — the single command/query authority.
//!
//! All editor state is governed here. The UI is just one driver: event handlers
//! translate gestures into [`EditorCommand`]s and call [`dispatch`]; they never
//! mutate state directly. A serializable [`EditorSnapshot`] read API
//! ([`snapshot`](EditorController::snapshot)) exists for external inspection.
//!
//! A future MCP/websocket transport is a thin adapter over `dispatch` /
//! `snapshot` — designed for now (serde-derived commands, the read seam), built
//! later. The JS-callable bridges live in `main.rs`.

mod command;
mod layout;
mod node;
pub mod snapshot;

pub use command::{ArrangeOp, ControlOp, EditorCommand, EditorQuery, QueryResult, SongOp};
// `Clipboard` (the serializable `Paste` payload) now lives in the protocol crate.
pub use awsm_audio_editor_protocol::Clipboard;
pub use node::{
    BoundaryPort, ConnId, ConnSink, DragState, EditorConnection, EditorNode, EnvDrag, PendingWire,
};
// Part of the read API (returned by `snapshot`); kept exported even though the
// binary doesn't reference the names directly.
#[allow(unused_imports)]
pub use snapshot::{EditorSnapshot, NodeLayout};

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use awsm_audio_schema::NodeId;
use futures_signals::signal::Mutable;
use futures_signals::signal_vec::MutableVec;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

thread_local! {
    static CONTROLLER: OnceCell<EditorController> = const { OnceCell::new() };
}

/// Install the controller singleton. Call once at boot, before mounting the UI.
pub fn init() {
    CONTROLLER.with(|c| {
        let _ = c.set(EditorController::new());
    });
}

/// A cheap clone of the controller singleton (every field is a shared handle).
pub fn controller() -> EditorController {
    CONTROLLER.with(|c| c.get().expect("controller not initialized").clone())
}

/// A live `setTimeout` handle (id) plus the closure it fires, kept alive
/// together — used to re-arm the song loop.
type SongTimer = Rc<RefCell<Option<(i32, Closure<dyn FnMut()>)>>>;

fn remap_connection_node_ids(
    c: &mut awsm_audio_schema::Connection,
    ids: &std::collections::HashMap<NodeId, NodeId>,
) {
    match &mut c.from {
        awsm_audio_schema::ConnectionSource::NodeOutput { node, .. }
        | awsm_audio_schema::ConnectionSource::SeqOut { node, .. } => {
            if let Some(new_id) = ids.get(node) {
                *node = *new_id;
            }
        }
        awsm_audio_schema::ConnectionSource::Inlet { .. } => {}
    }
    match &mut c.to {
        awsm_audio_schema::ConnectionSink::NodeInput { node, .. }
        | awsm_audio_schema::ConnectionSink::NodeParam { node, .. }
        | awsm_audio_schema::ConnectionSink::Trigger { node } => {
            if let Some(new_id) = ids.get(node) {
                *node = *new_id;
            }
        }
        awsm_audio_schema::ConnectionSink::Outlet { .. } => {}
    }
}

/// The command/query authority. Clone is cheap — all fields are shared handles.
#[derive(Clone)]
pub struct EditorController {
    /// Nodes on the canvas (reactive; drives node rendering).
    pub nodes: MutableVec<Rc<EditorNode>>,
    /// Wires between nodes (reactive; drives wire rendering).
    pub connections: MutableVec<Rc<EditorConnection>>,
    /// Camera pan, screen px.
    pub pan: Mutable<(f64, f64)>,
    /// Camera zoom factor.
    pub zoom: Mutable<f64>,
    /// The wire currently being dragged out of an output port, if any.
    pub pending: Mutable<Option<Rc<PendingWire>>>,
    /// Transient pointer-drag gesture (pan, node move, or box-select).
    pub drag: Rc<RefCell<Option<DragState>>>,
    /// Active box-select rectangle in world coords `(x0, y0, x1, y1)`, for the
    /// rubber-band overlay.
    pub box_select: Mutable<Option<(f64, f64, f64, f64)>>,
    /// Whether the audio graph is currently playing.
    pub playing: Mutable<bool>,
    /// Paused (stopped but holding position so Play resumes from there). Distinct
    /// from a full Stop, which returns to the play origin.
    pub paused: Mutable<bool>,
    /// Where the current play session started (seconds) — Stop returns here.
    play_origin: Rc<Cell<f64>>,
    /// Whether buffer sources loop.
    pub looping: Mutable<bool>,
    /// The node-type help doc currently shown, if any.
    pub help: Mutable<Option<crate::catalog::NodeDoc>>,
    /// Whether the "Load example" browser modal is open.
    pub examples_open: Mutable<bool>,
    /// Whether the sample picker modal (the scalable replacement for the inline
    /// tab strip) is open.
    pub sample_picker_open: Mutable<bool>,
    /// Whether the page Help / onboarding modal is open.
    pub help_open: Mutable<bool>,
    /// Which Help tab to show when it opens (set by `open_help_at`).
    pub help_tab: Mutable<usize>,
    /// Open node context menu: `(node, screen_x, screen_y)`.
    pub context_menu: Mutable<Option<(ContextTarget, f64, f64)>>,
    /// The single selected node, if exactly one — drives the inspector.
    pub inspected: Mutable<Option<NodeId>>,
    /// Bumped on any param/automation edit so the inspector re-reads the node.
    pub inspector_rev: Mutable<u32>,
    /// Live envelope-breakpoint drag in the inspector plot (preview before
    /// commit). Drives the plot reactively without re-rendering the whole panel.
    pub env_drag: Mutable<Option<EnvDrag>>,
    /// The open piano roll, as `(sequencer node, track index)`, or `None`.
    pub piano_roll: Mutable<Option<(NodeId, usize)>>,
    /// Live song playback position in beats for the piano-roll playhead, updated
    /// each animation frame; `-1` when not playing (hidden).
    pub playhead: Mutable<f64>,
    /// Song-loop bookkeeping: a generation that invalidates stale timers, the
    /// loop length (s), the next pass's absolute start (s), and the live
    /// `setTimeout` (id + closure kept alive). `song_start` is the context time
    /// the current song pass began, for the piano-roll playhead.
    song_gen: Rc<Cell<u64>>,
    song_loop_secs: Rc<Cell<f64>>,
    song_next_start: Rc<Cell<f64>>,
    song_start: Rc<Cell<f64>>,
    song_timer: SongTimer,
    /// True while the loop timer should recompute parts from the active
    /// Arrangement sample (Arrange view) rather than the Sequences canvas.
    loop_arrangement: Rc<Cell<bool>>,
    /// Where arrangement playback begins (beats) — the scrub position. Playback
    /// (and its loop region) runs from here to the timeline end.
    arrange_start: Rc<Cell<f64>>,
    /// Live arrangement playhead in beats (-1 when idle), for the timeline ruler.
    pub arrange_playhead: Mutable<f64>,
    /// The track targeted by double-click / "place at playhead" + header highlight.
    pub selected_track: Mutable<usize>,
    /// The selected arrangement clips `(track, clip)` — highlight + copy/delete
    /// targets. Shared so the context menu can act on the whole selection.
    pub selected_clips: Mutable<Vec<(usize, usize)>>,
    /// Clipboard holding copied arrangement clips as `(track, clip)` (copy/paste,
    /// supports multi-selection).
    clip_clipboard: Rc<RefCell<Vec<(usize, awsm_audio_schema::Clip)>>>,
    /// Undo / redo snapshot stacks + availability flags.
    undo_stack: Rc<RefCell<Vec<EditorSnapshot>>>,
    redo_stack: Rc<RefCell<Vec<EditorSnapshot>>>,
    pub can_undo: Mutable<bool>,
    pub can_redo: Mutable<bool>,
    /// The audio engine, created lazily (needs a user gesture).
    player: Rc<RefCell<Option<awsm_audio_player::Player>>>,
    /// WASM DSP assets loaded this session (id → serializable source), so the
    /// project saves self-contained and reloads them. The *compiled* modules live
    /// in the [`player`](Self::player).
    wasm_assets: Rc<
        RefCell<
            std::collections::HashMap<awsm_audio_schema::AssetId, awsm_audio_schema::WasmAsset>,
        >,
    >,
    /// Loaded audio buffers this session (id → serializable source), so projects
    /// save self-contained. The *decoded* buffers live in the [`player`](Self::player).
    buffer_assets: Rc<
        RefCell<
            std::collections::HashMap<awsm_audio_schema::AssetId, awsm_audio_schema::BufferAsset>,
        >,
    >,
    /// Optional base URL/path for resolving project-relative `AudioSource::Path`
    /// assets in bundled examples.
    asset_base_path: Rc<RefCell<Option<String>>>,
    /// The canvas viewport element, for client→world coordinate conversion.
    viewport: Rc<RefCell<Option<web_sys::Element>>>,
    /// Copy/paste clipboard: nodes (kind + relative pos) and their internal wires.
    clipboard: Rc<RefCell<Option<Clipboard>>>,
    /// The palette item currently being dragged toward the canvas, if any. Set
    /// on `dragstart`, consumed on `drop` to place the node at the cursor.
    palette_drag: Rc<RefCell<Option<PaletteDrag>>>,
    /// The project's spatial listener (position/orientation), applied each play.
    listener: Rc<RefCell<awsm_audio_schema::Listener>>,
    /// Every sample in the project (including the one on the canvas). The canvas
    /// (`nodes`/`connections`) is the working copy of [`active`](Self::active);
    /// switching commits the canvas back here.
    samples: Rc<RefCell<Vec<StoredSample>>>,
    /// Which sample id the canvas currently represents.
    active: Rc<RefCell<awsm_audio_schema::SampleId>>,
    /// The entry sample the player flattens + plays.
    root: Rc<RefCell<awsm_audio_schema::SampleId>>,
    /// Bumped when the sample set changes (drives the tab strip).
    pub samples_rev: Mutable<u32>,
    /// The active top-level view: Instruments (sound design) or Sequences
    /// (arrangement). Filters the tab strip + palette and selects play behavior.
    pub view: Mutable<awsm_audio_schema::SampleKind>,
    /// A transient status/error message shown in the transport (e.g. "wire an
    /// Output to play the sequence"). Cleared on the next successful play / stop.
    pub status: Mutable<Option<String>>,
    /// The id (uuid string) of the document object the most recent [`dispatch`]
    /// created — a node, sample, boundary, or sample-ref. Cleared at the start of
    /// every `dispatch` and set in the create arms. The MCP/remote layer reads it
    /// back via [`take_created_id`](Self::take_created_id) so a create command can
    /// return the minted id without a follow-up snapshot.
    created_id: Rc<RefCell<Option<String>>>,
    /// Transient per-Sound render state for the bounce observability surface:
    /// `Rendering` while an offline render is in flight, `Failed(msg)` if it
    /// errored. Absent once a render lands cleanly (the stored bounce + hash then
    /// say clean/dirty). Read by the `bounce_status` query so an agent can tell
    /// *never bounced* from *currently rendering* from *render crashed*.
    render_state: Rc<RefCell<std::collections::HashMap<awsm_audio_schema::SampleId, RenderState>>>,
}

/// In-flight / failed state of a Sound's offline render (see `render_state`).
#[derive(Clone)]
enum RenderState {
    Rendering,
    Failed(String),
}

/// A stored sample: its schema data plus per-node canvas layout (node ids are
/// globally unique, so layout round-trips through one flat list across samples).
#[derive(Clone)]
struct StoredSample {
    sample: awsm_audio_schema::Sample,
    layout: Vec<(NodeId, (f64, f64))>,
}

/// A tab descriptor for the sample strip.
pub struct SampleTab {
    pub id: awsm_audio_schema::SampleId,
    pub name: String,
    pub is_root: bool,
    pub is_active: bool,
}

/// A palette item being dragged onto the canvas. Carries enough to construct
/// What a right-click context menu is targeting.
#[derive(Clone, Copy)]
pub enum ContextTarget {
    /// A canvas node (Clone / Delete).
    Node(NodeId),
    /// A wire between two nodes (Delete).
    Wire(ConnId),
    /// An arrangement clip `(track, clip)` (Delete).
    Clip { track: usize, clip: usize },
    /// A bounced Sound in the Assets panel (Go to / Place at playhead).
    Sound(awsm_audio_schema::SampleId),
    /// Empty lane space at `(track, secs)` (Paste here / Paste at playhead).
    Lane { track: usize, secs: f64 },
    /// An arrangement track gain automation point.
    TrackGainPoint { track: usize, index: usize },
}

/// A Sound's bounce state, for the assets view + sample badges.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BounceStatus {
    /// Never bounced — can't be placed on the arrangement timeline yet.
    None,
    /// Bounced and up to date with the source graph.
    Clean,
    /// Bounced, but the source graph has changed since — re-bounce to refresh.
    Dirty,
}

impl BounceStatus {
    /// Stable string form for the read/query surface.
    pub fn as_str(self) -> &'static str {
        match self {
            BounceStatus::None => "none",
            BounceStatus::Clean => "clean",
            BounceStatus::Dirty => "stale",
        }
    }
}

/// How the active sample plays, derived from its content (not a stored kind).
#[derive(Clone, Copy, PartialEq, Eq)]
enum PlayRole {
    /// Audition the patch (sources auto-start, unconnected nodes reach master).
    Audition,
    /// Schedule the canvas's trigger/control wires through a wired Output.
    Song,
    /// Perform the sample's timeline (`sample.arrangement`).
    Arrangement,
}

/// the node at the drop position (plain kinds, or the composition specials).
#[derive(Clone)]
pub enum PaletteDrag {
    Node(Box<awsm_audio_schema::NodeKind>),
    Inlet,
    Outlet,
    SampleRef,
}

impl EditorController {
    fn new() -> Self {
        let ctrl = Self {
            nodes: MutableVec::new(),
            connections: MutableVec::new(),
            pan: Mutable::new((0.0, 0.0)),
            zoom: Mutable::new(1.0),
            pending: Mutable::new(None),
            drag: Rc::new(RefCell::new(None)),
            box_select: Mutable::new(None),
            playing: Mutable::new(false),
            paused: Mutable::new(false),
            play_origin: Rc::new(Cell::new(0.0)),
            looping: Mutable::new(false),
            help: Mutable::new(None),
            examples_open: Mutable::new(false),
            sample_picker_open: Mutable::new(false),
            help_open: Mutable::new(false),
            help_tab: Mutable::new(0),
            context_menu: Mutable::new(None),
            inspected: Mutable::new(None),
            inspector_rev: Mutable::new(0),
            env_drag: Mutable::new(None),
            piano_roll: Mutable::new(None),
            playhead: Mutable::new(-1.0),
            song_gen: Rc::new(Cell::new(0)),
            song_loop_secs: Rc::new(Cell::new(0.0)),
            song_next_start: Rc::new(Cell::new(0.0)),
            song_start: Rc::new(Cell::new(0.0)),
            song_timer: Rc::new(RefCell::new(None)),
            loop_arrangement: Rc::new(Cell::new(false)),
            arrange_start: Rc::new(Cell::new(0.0)),
            arrange_playhead: Mutable::new(-1.0),
            selected_track: Mutable::new(0),
            selected_clips: Mutable::new(Vec::new()),
            clip_clipboard: Rc::new(RefCell::new(Vec::new())),
            undo_stack: Rc::new(RefCell::new(Vec::new())),
            redo_stack: Rc::new(RefCell::new(Vec::new())),
            can_undo: Mutable::new(false),
            can_redo: Mutable::new(false),
            player: Rc::new(RefCell::new(None)),
            wasm_assets: Rc::new(RefCell::new(std::collections::HashMap::new())),
            buffer_assets: Rc::new(RefCell::new(std::collections::HashMap::new())),
            asset_base_path: Rc::new(RefCell::new(None)),
            viewport: Rc::new(RefCell::new(None)),
            clipboard: Rc::new(RefCell::new(None)),
            palette_drag: Rc::new(RefCell::new(None)),
            listener: Rc::new(RefCell::new(awsm_audio_schema::Listener::default())),
            samples: Rc::new(RefCell::new(vec![StoredSample {
                sample: awsm_audio_schema::Sample::new("main"),
                layout: Vec::new(),
            }])),
            active: Rc::new(RefCell::new(awsm_audio_schema::SampleId::new())),
            root: Rc::new(RefCell::new(awsm_audio_schema::SampleId::new())),
            samples_rev: Mutable::new(0),
            view: Mutable::new(awsm_audio_schema::SampleKind::Sound),
            status: Mutable::new(None),
            created_id: Rc::new(RefCell::new(None)),
            render_state: Rc::new(RefCell::new(std::collections::HashMap::new())),
        };
        // Point active + root at the initial "main" sample.
        let id = ctrl.samples.borrow()[0].sample.id;
        *ctrl.active.borrow_mut() = id;
        *ctrl.root.borrow_mut() = id;
        ctrl
    }

    /// The graph to play: the canvas projected to a library and flattened (so any
    /// `Sample` reference nodes are inlined). Flattens the *active* sample — not
    /// the root — so switching to a sub-sample tab auditions that sample in
    /// isolation. Falls back to the raw canvas graph.
    fn playable_graph(&self) -> awsm_audio_schema::Graph {
        let lib = self.to_library();
        let active = *self.active.borrow();
        let g = awsm_audio_schema::flatten(&lib, active);
        if g.nodes.is_empty() {
            self.to_graph()
        } else {
            g
        }
    }

    // ==================================================================
    // Multi-sample model: the canvas is the active sample; switching commits it.
    // ==================================================================

    /// Write the current canvas (graph + layout) back into the active sample.
    pub fn commit_active(&self) {
        let id = *self.active.borrow();
        let graph = self.to_graph();
        let layout: Vec<(NodeId, (f64, f64))> = self
            .nodes
            .lock_ref()
            .iter()
            .map(|n| (n.id, n.pos.get()))
            .collect();
        if let Some(st) = self
            .samples
            .borrow_mut()
            .iter_mut()
            .find(|s| s.sample.id == id)
        {
            st.sample.graph = graph;
            st.layout = layout;
        }
    }

    /// Load a stored sample's graph + layout onto the canvas (no commit).
    fn load_sample_onto_canvas(&self, id: awsm_audio_schema::SampleId) {
        let (graph, layout) = {
            let samples = self.samples.borrow();
            match samples.iter().find(|s| s.sample.id == id) {
                Some(st) => (st.sample.graph.clone(), st.layout.clone()),
                None => return,
            }
        };
        let pos: std::collections::HashMap<NodeId, (f64, f64)> = layout.into_iter().collect();
        let fallback = layout::auto_layout(&graph);
        self.rebuild(&graph, |nid| {
            pos.get(&nid)
                .copied()
                .or_else(|| fallback.get(&nid).copied())
                .unwrap_or((80.0, 80.0))
        });
        self.undo_stack.borrow_mut().clear();
        self.redo_stack.borrow_mut().clear();
        self.refresh_undo_flags();
    }

    /// Switch the canvas to another sample (commits the current one first).
    pub fn switch_sample(&self, id: awsm_audio_schema::SampleId) {
        if *self.active.borrow() == id {
            return;
        }
        self.commit_active();
        *self.active.borrow_mut() = id;
        self.load_sample_onto_canvas(id);
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Add a new empty sample of the active view's kind and switch to it.
    pub fn add_sample(&self) {
        self.dispatch(EditorCommand::AddSample {
            kind: self.view.get(),
        });
    }
    fn add_sample_impl(&self, kind: awsm_audio_schema::SampleKind) {
        self.commit_active();
        let n = self.samples.borrow().len() + 1;
        let sample = match kind {
            awsm_audio_schema::SampleKind::Arrangement => {
                awsm_audio_schema::Sample::new_arrangement(format!("arrangement {n}"))
            }
            awsm_audio_schema::SampleKind::Sound => {
                awsm_audio_schema::Sample::new(format!("sound {n}"))
            }
        };
        let id = sample.id;
        self.set_created_id(id);
        self.samples.borrow_mut().push(StoredSample {
            sample,
            layout: Vec::new(),
        });
        self.switch_sample(id);
    }

    /// Rename a sample.
    pub fn rename_sample(&self, id: awsm_audio_schema::SampleId, name: String) {
        self.dispatch(EditorCommand::RenameSample { id, name });
    }
    fn rename_sample_impl(&self, id: awsm_audio_schema::SampleId, name: String) {
        if let Some(st) = self
            .samples
            .borrow_mut()
            .iter_mut()
            .find(|s| s.sample.id == id)
        {
            st.sample.name = name;
        }
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Set a sample's free-form working notes (annotation metadata).
    fn set_sample_notes_impl(&self, id: awsm_audio_schema::SampleId, notes: String) {
        if let Some(st) = self
            .samples
            .borrow_mut()
            .iter_mut()
            .find(|s| s.sample.id == id)
        {
            st.sample.notes = notes;
        }
    }

    /// Delete a sample (never the last one). If it was active/root, repoints.
    pub fn delete_sample(&self, id: awsm_audio_schema::SampleId) {
        self.dispatch(EditorCommand::RemoveSample { id });
    }
    fn remove_sample_impl(&self, id: awsm_audio_schema::SampleId) {
        {
            let mut samples = self.samples.borrow_mut();
            if samples.len() <= 1 {
                return;
            }
            samples.retain(|s| s.sample.id != id);
        }
        let first = self.samples.borrow()[0].sample.id;
        if *self.root.borrow() == id {
            *self.root.borrow_mut() = first;
        }
        if *self.active.borrow() == id {
            *self.active.borrow_mut() = first;
            self.load_sample_onto_canvas(first);
        }
        // Keep the current view populated (re-create an empty one if we just
        // deleted its last sample, or repoint into it if active drifted).
        self.ensure_view_sample();
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Duplicate a sample under a new id (name + " (clone)") and make it active.
    pub fn clone_sample(&self, id: awsm_audio_schema::SampleId) {
        self.dispatch(EditorCommand::CloneSample { id });
    }
    fn clone_sample_impl(&self, id: awsm_audio_schema::SampleId) {
        // Flush any pending canvas edits so a clone of the *active* sample
        // captures what's on screen, not the last-committed state.
        self.commit_active();
        let cloned = {
            let samples = self.samples.borrow();
            let Some(src) = samples.iter().find(|s| s.sample.id == id) else {
                return;
            };
            let mut sample = src.sample.clone();
            let mut id_map = std::collections::HashMap::new();
            for node in &mut sample.graph.nodes {
                let old = node.id;
                node.id = awsm_audio_schema::NodeId::new();
                id_map.insert(old, node.id);
            }
            for c in &mut sample.graph.connections {
                remap_connection_node_ids(c, &id_map);
            }
            for source in &mut sample.trigger.sources {
                if let Some(new_id) = id_map.get(source) {
                    *source = *new_id;
                }
            }
            sample.id = awsm_audio_schema::SampleId::new();
            sample.name = format!("{} (clone)", src.sample.name);
            let layout = src
                .layout
                .iter()
                .filter_map(|(node, pos)| id_map.get(node).map(|new_id| (*new_id, *pos)))
                .collect();
            StoredSample { sample, layout }
        };
        let new_id = cloned.sample.id;
        self.set_created_id(new_id);
        self.samples.borrow_mut().push(cloned);
        self.switch_sample(new_id);
    }

    /// Mark a sample as the project root (the one that plays).
    pub fn set_root(&self, id: awsm_audio_schema::SampleId) {
        self.dispatch(EditorCommand::SetRoot { id });
    }
    fn set_root_impl(&self, id: awsm_audio_schema::SampleId) {
        *self.root.borrow_mut() = id;
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// The current sample tabs (for the strip UI) — only samples in the active
    /// view (Instruments vs Sequences).
    pub fn sample_tabs(&self) -> Vec<SampleTab> {
        let active = *self.active.borrow();
        let root = *self.root.borrow();
        let view = self.view.get();
        self.samples
            .borrow()
            .iter()
            .filter(|s| s.sample.kind == view)
            .map(|s| SampleTab {
                id: s.sample.id,
                name: s.sample.name.clone(),
                is_root: s.sample.id == root,
                is_active: s.sample.id == active,
            })
            .collect()
    }

    /// Switch the top-level view. Commits the canvas, then ensures the active
    /// sample belongs to the new view (picking or creating one as needed).
    pub fn switch_view(&self, view: awsm_audio_schema::SampleKind) {
        if self.view.get() == view {
            return;
        }
        self.commit_active();
        self.view.set(view);
        self.status.set(None);
        self.ensure_view_sample();
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Guarantee the active sample is in the current view: if it isn't, switch to
    /// the first sample of that kind, creating an empty one if none exists.
    /// Assumes the canvas has already been committed.
    fn ensure_view_sample(&self) {
        let kind = self.view.get();
        let active = *self.active.borrow();
        let active_ok = self
            .samples
            .borrow()
            .iter()
            .any(|s| s.sample.id == active && s.sample.kind == kind);
        if active_ok {
            return;
        }
        let first = self
            .samples
            .borrow()
            .iter()
            .find(|s| s.sample.kind == kind)
            .map(|s| s.sample.id);
        let id = match first {
            Some(id) => id,
            None => {
                let n = self.samples.borrow().len() + 1;
                let sample = match kind {
                    awsm_audio_schema::SampleKind::Arrangement => {
                        awsm_audio_schema::Sample::new_arrangement(format!("arrangement {n}"))
                    }
                    awsm_audio_schema::SampleKind::Sound => {
                        awsm_audio_schema::Sample::new(format!("sound {n}"))
                    }
                };
                let id = sample.id;
                self.samples.borrow_mut().push(StoredSample {
                    sample,
                    layout: Vec::new(),
                });
                id
            }
        };
        *self.active.borrow_mut() = id;
        self.load_sample_onto_canvas(id);
    }

    /// Sound sample ids + names available to reference / embed (excludes `except`
    /// and Arrangements, which aren't node graphs).
    pub fn other_samples(
        &self,
        except: awsm_audio_schema::SampleId,
    ) -> Vec<(awsm_audio_schema::SampleId, String)> {
        self.samples
            .borrow()
            .iter()
            .filter(|s| {
                s.sample.id != except && s.sample.kind == awsm_audio_schema::SampleKind::Sound
            })
            .map(|s| (s.sample.id, s.sample.name.clone()))
            .collect()
    }

    /// The active sample's id.
    pub fn active_sample(&self) -> awsm_audio_schema::SampleId {
        *self.active.borrow()
    }

    /// Add a boundary (inlet/outlet) node near the viewport center.
    pub fn add_boundary(&self, port: BoundaryPort) {
        let (cx, cy) = self.world_center();
        self.dispatch(EditorCommand::AddBoundary {
            port,
            x: cx - 50.0,
            y: cy - 28.0,
        });
    }

    /// Add a boundary node at a specific world position (e.g. a palette drop).
    /// The name is made unique so multiple inlets/outlets stay distinct (each is
    /// a separately addressable port on a parent's Sample-reference node).
    fn add_boundary_impl(&self, port: BoundaryPort, x: f64, y: f64) {
        let base = match port {
            BoundaryPort::Inlet => "in",
            BoundaryPort::Outlet => "out",
        };
        let name = self.unique_boundary_name(base);
        let node = EditorNode::boundary(NodeId::new(), port, &name, (x, y));
        let id = node.id;
        self.set_created_id(id);
        self.nodes.lock_mut().push_cloned(node);
        self.set_selection(&[id], false);
    }

    /// A boundary port name not already used by another boundary node on the
    /// canvas: `base`, then `base 2`, `base 3`, …
    fn unique_boundary_name(&self, base: &str) -> String {
        let used: std::collections::HashSet<String> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.boundary.is_some())
            .map(|n| n.label.get_cloned())
            .collect();
        if !used.contains(base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base} {i}"))
            .find(|c| !used.contains(c))
            .unwrap_or_else(|| base.to_string())
    }

    /// Add a Sample-reference node targeting the first other sample (if any).
    pub fn add_sample_ref(&self) {
        let (cx, cy) = self.world_center();
        self.add_sample_ref_at(cx - 60.0, cy - 36.0);
    }

    /// Add a Sample-reference node at a specific world position.
    pub fn add_sample_ref_at(&self, x: f64, y: f64) {
        let Some((sample, _)) = self.other_samples(*self.active.borrow()).into_iter().next() else {
            self.status.set(Some(
                "No instrument to reference yet — create one in the Instruments view first.".into(),
            ));
            return;
        };
        self.dispatch(EditorCommand::AddSampleRef { sample, x, y });
    }

    /// Add a Sample-reference node targeting `sample` at world `(x, y)`.
    fn add_sample_ref_impl(&self, sample: awsm_audio_schema::SampleId, x: f64, y: f64) {
        let node = EditorNode::new(
            NodeId::new(),
            awsm_audio_schema::NodeKind::Sample(awsm_audio_schema::SampleRef {
                sample,
                inputs: Vec::new(),
            }),
            (x, y),
        );
        let id = node.id;
        self.set_created_id(id);
        self.nodes.lock_mut().push_cloned(node);
        self.set_selection(&[id], false);
    }

    /// Begin dragging a palette item toward the canvas (stash until drop).
    pub fn begin_palette_drag(&self, item: PaletteDrag) {
        *self.palette_drag.borrow_mut() = Some(item);
    }

    /// Drop the in-flight palette item at a client (screen) position, placing the
    /// new node centered on the cursor. No-op if nothing is being dragged.
    pub fn drop_palette_item(&self, client_x: f64, client_y: f64) {
        let Some(item) = self.palette_drag.borrow_mut().take() else {
            return;
        };
        let (wx, wy) = self.client_to_world(client_x, client_y);
        // Offset so the node body roughly centers on the cursor.
        let (x, y) = (wx - 86.0, wy - 28.0);
        match item {
            PaletteDrag::Node(kind) => self.dispatch(EditorCommand::AddNode { kind: *kind, x, y }),
            PaletteDrag::Inlet => self.dispatch(EditorCommand::AddBoundary {
                port: BoundaryPort::Inlet,
                x,
                y,
            }),
            PaletteDrag::Outlet => self.dispatch(EditorCommand::AddBoundary {
                port: BoundaryPort::Outlet,
                x,
                y,
            }),
            PaletteDrag::SampleRef => self.add_sample_ref_at(x, y),
        }
    }

    /// Names of a sample's inlets / outlets (for labeling a Sample-ref node's
    /// ports). Returns `(inlet_names, outlet_names)` in port order.
    pub fn sample_port_names(
        &self,
        sample: awsm_audio_schema::SampleId,
    ) -> (Vec<String>, Vec<String>) {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == sample)
            .map(|s| {
                (
                    s.sample
                        .graph
                        .inlets
                        .iter()
                        .map(|p| p.id.0.clone())
                        .collect(),
                    s.sample
                        .graph
                        .outlets
                        .iter()
                        .map(|p| p.id.0.clone())
                        .collect(),
                )
            })
            .unwrap_or_default()
    }

    /// Point a Sample-reference node at a different sample.
    pub fn set_sample_ref(&self, node: NodeId, sample: awsm_audio_schema::SampleId) {
        self.dispatch(EditorCommand::SetSampleRef { node, sample });
    }
    fn set_sample_ref_impl(&self, node: NodeId, sample: awsm_audio_schema::SampleId) {
        if let Some(n) = self.node_by_id(node) {
            if let awsm_audio_schema::NodeKind::Sample(sr) = &mut *n.kind.borrow_mut() {
                sr.sample = sample;
            }
        }
        self.touch_node(node);
        if self.playing.get() {
            self.play();
        }
    }

    /// Encapsulate the selected nodes into a new sub-sample, auto-creating
    /// inlets/outlets at the cut wires and replacing the selection on the canvas
    /// with a wired Sample-reference node.
    pub fn encapsulate_selection(&self) {
        let ids: Vec<NodeId> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.selected.get() && n.boundary.is_none())
            .map(|n| n.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        self.dispatch(EditorCommand::Encapsulate { ids });
    }
    fn encapsulate_impl(&self, ids: &[NodeId]) {
        use awsm_audio_schema::{
            Connection, ConnectionSink, ConnectionSource, Node, NodeKind, PortDecl, PortId, Sample,
            SampleRef,
        };
        use std::collections::HashSet;

        let sel_ids: HashSet<NodeId> = ids.iter().copied().collect();
        let sel: Vec<Rc<EditorNode>> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| sel_ids.contains(&n.id) && n.boundary.is_none())
            .cloned()
            .collect();
        if sel.is_empty() {
            return;
        }

        let conns: Vec<Rc<EditorConnection>> =
            self.connections.lock_ref().iter().cloned().collect();

        let mut sub = Sample::new(format!("sample {}", self.samples.borrow().len() + 1));
        let sub_id = sub.id;
        for n in &sel {
            let label = n.label.get_cloned();
            sub.graph.nodes.push(Node {
                id: n.id,
                label: (!label.is_empty()).then_some(label),
                kind: n.kind.borrow().clone(),
            });
        }

        let mut inlet_order: Vec<(NodeId, u32)> = Vec::new();
        let mut outlet_order: Vec<(NodeId, u32)> = Vec::new();
        let mut parent_inputs: Vec<(NodeId, u32, usize)> = Vec::new();
        let mut parent_outputs: Vec<(usize, NodeId, ConnSink)> = Vec::new();

        for c in &conns {
            if c.from.boundary.is_some() || c.to.boundary.is_some() {
                continue;
            }
            let from_in = sel_ids.contains(&c.from.id);
            let to_in = sel_ids.contains(&c.to.id);
            match (from_in, to_in) {
                (true, true) => {
                    // Internal wire — copy verbatim into the sub-sample.
                    let to = match &c.sink {
                        ConnSink::Input(i) => ConnectionSink::NodeInput {
                            node: c.to.id,
                            input: *i,
                        },
                        ConnSink::Param(p) => ConnectionSink::NodeParam {
                            node: c.to.id,
                            param: p.clone(),
                        },
                        // Trigger wires are a Sequences-view concept; never present
                        // in an instrument being encapsulated.
                        ConnSink::Trigger => continue,
                    };
                    sub.graph.connections.push(Connection {
                        id: None,
                        from: ConnectionSource::NodeOutput {
                            node: c.from.id,
                            output: c.from_output,
                        },
                        to,
                    });
                }
                (false, true) => {
                    // Incoming — becomes an inlet (audio inputs only).
                    let ConnSink::Input(input) = c.sink else {
                        tracing::warn!("dropping cross-boundary modulation into the selection");
                        continue;
                    };
                    let ext = (c.from.id, c.from_output);
                    let idx = inlet_order
                        .iter()
                        .position(|e| *e == ext)
                        .unwrap_or_else(|| {
                            inlet_order.push(ext);
                            inlet_order.len() - 1
                        });
                    sub.graph.connections.push(Connection {
                        id: None,
                        from: ConnectionSource::Inlet {
                            port: PortId::from(format!("in{idx}")),
                        },
                        to: ConnectionSink::NodeInput {
                            node: c.to.id,
                            input,
                        },
                    });
                    if !parent_inputs.contains(&(c.from.id, c.from_output, idx)) {
                        parent_inputs.push((c.from.id, c.from_output, idx));
                    }
                }
                (true, false) => {
                    // Outgoing — becomes an outlet.
                    let src = (c.from.id, c.from_output);
                    let idx = outlet_order
                        .iter()
                        .position(|e| *e == src)
                        .unwrap_or_else(|| {
                            outlet_order.push(src);
                            outlet_order.len() - 1
                        });
                    sub.graph.connections.push(Connection {
                        id: None,
                        from: ConnectionSource::NodeOutput {
                            node: c.from.id,
                            output: c.from_output,
                        },
                        to: ConnectionSink::Outlet {
                            port: PortId::from(format!("out{idx}")),
                        },
                    });
                    parent_outputs.push((idx, c.to.id, c.sink.clone()));
                }
                (false, false) => {}
            }
        }
        for i in 0..inlet_order.len() {
            sub.graph
                .inlets
                .push(PortDecl::new(PortId::from(format!("in{i}"))));
        }
        for i in 0..outlet_order.len() {
            sub.graph
                .outlets
                .push(PortDecl::new(PortId::from(format!("out{i}"))));
        }

        // Centroid of the selection → where the ref node lands.
        let (mut sx, mut sy) = (0.0, 0.0);
        for n in &sel {
            let (x, y) = n.pos.get();
            sx += x;
            sy += y;
        }
        let pos = (sx / sel.len() as f64, sy / sel.len() as f64);

        // Store the new sub-sample (with the selected nodes' layout).
        self.samples.borrow_mut().push(StoredSample {
            layout: sel.iter().map(|n| (n.id, n.pos.get())).collect(),
            sample: sub,
        });

        // Drop the selected nodes + their wires from the canvas.
        self.connections
            .lock_mut()
            .retain(|c| !sel_ids.contains(&c.from.id) && !sel_ids.contains(&c.to.id));
        self.nodes.lock_mut().retain(|n| !sel_ids.contains(&n.id));

        // Add the reference node and re-wire the cut connections to it.
        let ref_node = EditorNode::new(
            NodeId::new(),
            NodeKind::Sample(SampleRef {
                sample: sub_id,
                inputs: Vec::new(),
            }),
            pos,
        );
        let ref_id = ref_node.id;
        self.nodes.lock_mut().push_cloned(ref_node);
        for (en, eo, idx) in parent_inputs {
            self.add_connection(en, eo, ref_id, ConnSink::Input(idx as u32));
        }
        for (idx, to_id, sink) in parent_outputs {
            self.add_connection(ref_id, idx as u32, to_id, sink);
        }
        self.set_selection(&[ref_id], false);
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
        if self.playing.get() {
            self.play();
        }
    }

    /// Inlet/outlet counts of a referenced sample (for Sample-ref node ports).
    pub fn sample_io(&self, sample: awsm_audio_schema::SampleId) -> (u32, u32) {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == sample)
            .map(|s| {
                // Always expose at least one input port: besides any declared
                // inlets, it's the drop target for a sequencer's trigger wire
                // (which binds the part to this instrument; the index is unused).
                (
                    (s.sample.graph.inlets.len() as u32).max(1),
                    s.sample.graph.outlets.len() as u32,
                )
            })
            .unwrap_or((1, 1))
    }

    // ==================================================================
    // Inputs: a sample's inlets are its named, settable inputs. Edit an inlet's
    // default value + set/add mode, set per-instance values on a Sample ref, and
    // MIDI-map an input (see the MIDI section).
    // ==================================================================

    /// The active sample's input (inlet) boundary nodes on the canvas.
    pub fn active_inputs(&self) -> Vec<Rc<EditorNode>> {
        self.nodes
            .lock_ref()
            .iter()
            .filter(|n| n.boundary == Some(BoundaryPort::Inlet))
            .cloned()
            .collect()
    }

    /// Readable targets an inlet drives ("Oscillator \u{00b7} frequency"), from
    /// the canvas wires leaving it.
    pub fn input_targets(&self, inlet: NodeId) -> Vec<String> {
        self.connections
            .lock_ref()
            .iter()
            .filter(|c| c.from.id == inlet)
            .map(|c| {
                let l = c.to.label.get_cloned();
                let name = if l.trim().is_empty() {
                    crate::ports::kind_label(&c.to.kind.borrow()).to_string()
                } else {
                    l
                };
                match &c.sink {
                    ConnSink::Param(p) => format!("{name} \u{00b7} {}", p.0),
                    ConnSink::Input(_) => format!("{name} (audio in)"),
                    ConnSink::Trigger => format!("{name} (trigger)"),
                }
            })
            .collect()
    }

    /// Set an inlet's default value (used standalone and as the fallback when a
    /// parent doesn't override it).
    pub fn set_input_default(&self, inlet: NodeId, value: f32) {
        self.dispatch(EditorCommand::SetInputDefault { node: inlet, value });
    }
    fn set_input_default_impl(&self, inlet: NodeId, value: f32) {
        if let Some(n) = self.node_by_id(inlet) {
            n.default.set(value);
        }
        self.inspector_rev.replace_with(|r| r.wrapping_add(1));
        if self.playing.get() {
            // Same live path as MIDI CC: glide param targets without a rebuild so
            // dragging the value while playing is smooth.
            match self.live_param_targets(inlet) {
                Some(targets) => {
                    for (node, param) in targets {
                        self.set_param_live(node, &param, value);
                    }
                }
                None => self.play(),
            }
        }
    }

    /// The inlets (inputs) of a referenced sample, as `(name, default)` in order
    /// \u2014 for a Sample-ref's per-instance value fields.
    pub fn referenced_inputs(&self, sample: awsm_audio_schema::SampleId) -> Vec<(String, f32)> {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == sample)
            .map(|s| {
                s.sample
                    .graph
                    .inlets
                    .iter()
                    .map(|p| (p.id.0.clone(), p.default))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The per-instance value set for `port` on a Sample-ref node, if any.
    pub fn input_value(&self, node: NodeId, port: &str) -> Option<f32> {
        let n = self.node_by_id(node)?;
        let kind = n.kind.borrow();
        if let awsm_audio_schema::NodeKind::Sample(sr) = &*kind {
            sr.inputs
                .iter()
                .find(|iv| iv.port.0 == port)
                .map(|iv| iv.value)
        } else {
            None
        }
    }

    /// Set (or replace) a per-instance input value on a Sample-ref node.
    pub fn set_input_value(&self, node: NodeId, port: &str, value: f32) {
        self.dispatch(EditorCommand::SetInputValue {
            node,
            port: port.to_string(),
            value,
        });
    }
    fn set_input_value_impl(&self, node: NodeId, port: &str, value: f32) {
        use awsm_audio_schema::{InputValue, NodeKind, PortId};
        if let Some(n) = self.node_by_id(node) {
            if let NodeKind::Sample(sr) = &mut *n.kind.borrow_mut() {
                if let Some(iv) = sr.inputs.iter_mut().find(|iv| iv.port.0 == port) {
                    iv.value = value;
                } else {
                    sr.inputs.push(InputValue {
                        port: PortId::from(port),
                        value,
                    });
                }
            }
        }
        if self.playing.get() {
            self.play();
        }
    }

    // ==================================================================
    // Write surface — the single mutation entry point.
    // ==================================================================

    /// Apply a command. The only way editor state changes. Non-transient
    /// commands snapshot the prior state onto the undo stack first.
    /// Record the id a create command just minted (see `created_id`).
    fn set_created_id(&self, id: impl std::fmt::Display) {
        *self.created_id.borrow_mut() = Some(id.to_string());
    }

    /// Take (and clear) the id minted by the most recent [`dispatch`], if it
    /// created a node / sample / boundary / sample-ref. The remote/MCP layer calls
    /// this right after a `dispatch` to echo the new id back to the agent.
    pub fn take_created_id(&self) -> Option<String> {
        self.created_id.borrow_mut().take()
    }

    pub fn dispatch(&self, cmd: EditorCommand) {
        // Each dispatch reports at most one created id; clear the prior one so a
        // non-creating command leaves `None` (see `created_id`).
        self.created_id.borrow_mut().take();
        if !cmd.is_transient() {
            self.push_undo();
        }
        match cmd {
            EditorCommand::AddNode { kind, x, y } => {
                let node = EditorNode::new(NodeId::new(), kind, (x, y));
                let id = node.id;
                self.set_created_id(id);
                // A NoteSequencer created with an inline, already-populated `song`
                // (tracks + note events, e.g. via add_node / dispatch_command) must
                // have its sound `outputs` derived up front — otherwise they stay
                // empty and any later `bind` targets a non-existent port (silent
                // silence). Mirror what add_note/set_song do, but at creation time.
                if let awsm_audio_schema::NodeKind::NoteSequencer(ms) = &mut *node.kind.borrow_mut()
                {
                    ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
                }
                // Push first so the node exists in the list before selection
                // (set_selection scans `nodes` to flag + compute the inspected one).
                self.nodes.lock_mut().push_cloned(node);
                self.set_selection(&[id], false);
            }

            EditorCommand::MoveNode { id, x, y } => {
                if let Some(n) = self.node_by_id(id) {
                    n.pos.set((x, y));
                }
            }

            EditorCommand::RemoveNode { id } => {
                // Drop every wire touching the node first.
                self.connections
                    .lock_mut()
                    .retain(|c| c.from.id != id && c.to.id != id);
                self.nodes.lock_mut().retain(|n| n.id != id);
            }

            EditorCommand::CloneNode { id } => {
                if let Some(src) = self.node_by_id(id) {
                    let kind = src.kind.borrow().clone();
                    let (x, y) = src.pos.get();
                    let dup = EditorNode::new(NodeId::new(), kind, (x + 26.0, y + 26.0));
                    let dup_id = dup.id;
                    self.set_created_id(dup_id);
                    self.nodes.lock_mut().push_cloned(dup);
                    self.set_selection(&[dup_id], false);
                }
            }

            EditorCommand::SetField { id, key, value } => {
                if let Some(node) = self.node_by_id(id) {
                    crate::fields::apply(&mut node.kind.borrow_mut(), &key, &value);
                    self.inspector_rev.replace_with(|r| r.wrapping_add(1));
                    // Channel-count changes alter the node's port count — re-render
                    // its card so the ports (and any wires) update.
                    if key == "number_of_outputs" || key == "number_of_inputs" {
                        self.touch_node(id);
                    }
                    // If the edited graph is playing, re-instantiate so the
                    // change is audible immediately.
                    if self.playing.get() {
                        self.play();
                    }
                }
            }

            EditorCommand::SetAutomation { id, param, events } => {
                if let Some(node) = self.node_by_id(id) {
                    crate::fields::set_automation(&mut node.kind.borrow_mut(), &param, events);
                    self.inspector_rev.replace_with(|r| r.wrapping_add(1));
                    if self.playing.get() {
                        self.play();
                    }
                }
            }

            EditorCommand::Connect {
                from,
                from_output,
                to,
                to_input,
            } => {
                // No self-loops, no exact duplicates.
                if from == to {
                    return;
                }
                let (Some(from_node), Some(to_node)) = (self.node_by_id(from), self.node_by_id(to))
                else {
                    return;
                };
                let dup = self.connections.lock_ref().iter().any(|c| {
                    c.from.id == from
                        && c.from_output == from_output
                        && c.to.id == to
                        && matches!(c.sink, ConnSink::Input(i) if i == to_input)
                });
                if dup {
                    return;
                }
                self.connections
                    .lock_mut()
                    .push_cloned(Rc::new(EditorConnection {
                        id: ConnId::new_v4(),
                        from: from_node,
                        from_output,
                        to: to_node,
                        sink: ConnSink::Input(to_input),
                    }));
            }

            EditorCommand::Modulate {
                from,
                from_output,
                to,
                param,
            } => {
                if from == to {
                    return;
                }
                let (Some(from_node), Some(to_node)) = (self.node_by_id(from), self.node_by_id(to))
                else {
                    return;
                };
                let dup = self.connections.lock_ref().iter().any(|c| {
                    c.from.id == from
                        && c.to.id == to
                        && matches!(&c.sink, ConnSink::Param(p) if *p == param)
                });
                if dup {
                    return;
                }
                self.connections
                    .lock_mut()
                    .push_cloned(Rc::new(EditorConnection {
                        id: ConnId::new_v4(),
                        from: from_node,
                        from_output,
                        to: to_node,
                        sink: ConnSink::Param(param),
                    }));
            }

            EditorCommand::Bind {
                from,
                from_output,
                to,
            } => {
                if from == to {
                    return;
                }
                let (Some(from_node), Some(to_node)) = (self.node_by_id(from), self.node_by_id(to))
                else {
                    return;
                };
                // Fail loudly instead of silently storing a wire to a port that
                // doesn't exist. A sequencer's outputs are derived from its song
                // (one per melodic track / drum note); binding `from_output` before
                // that output exists (e.g. an empty sequencer) would otherwise be
                // accepted and then resolve to an empty key — a wire that carries
                // nothing. Reject it and tell the user what's missing.
                {
                    let kind = from_node.kind.borrow();
                    if crate::ports::is_sequencer(&kind)
                        && crate::ports::seq_key_at(&kind, from_output as usize).is_none()
                    {
                        drop(kind);
                        let msg = format!(
                            "Bind ignored: sequencer has no output #{from_output} yet \
                             (add tracks/notes first — outputs are derived from the song)."
                        );
                        tracing::warn!("{msg}");
                        self.status.set(Some(msg));
                        return;
                    }
                }
                // One trigger binding per (sequencer-output → instrument); a fresh
                // bind to the same instrument from a different output layers them.
                let dup = self.connections.lock_ref().iter().any(|c| {
                    c.from.id == from
                        && c.from_output == from_output
                        && c.to.id == to
                        && matches!(c.sink, ConnSink::Trigger)
                });
                if dup {
                    return;
                }
                self.connections
                    .lock_mut()
                    .push_cloned(Rc::new(EditorConnection {
                        id: ConnId::new_v4(),
                        from: from_node,
                        from_output,
                        to: to_node,
                        sink: ConnSink::Trigger,
                    }));
            }

            EditorCommand::Disconnect { id } => {
                self.connections.lock_mut().retain(|c| c.id != id);
            }

            // Structured sequencing edits route to the node-specific editors
            // below. These own their undo snapshots, so dispatch treats them as
            // transient (no auto-snapshot) — see EditorCommand::is_transient.
            EditorCommand::EditSong { node, op } => self.edit_song_op(node, op),
            EditorCommand::EditControl { node, op } => self.edit_control_op(node, op),
            EditorCommand::EditArrange { op } => self.edit_arrange_op(op),
            EditorCommand::Bounce {
                sample,
                duration_secs,
            } => self.bounce_sample(sample, duration_secs),

            EditorCommand::SelectNodes { ids, additive } => {
                self.set_selection(&ids, additive);
            }

            EditorCommand::ClearSelection => {
                self.set_selection(&[], false);
            }

            EditorCommand::SetCamera { pan_x, pan_y, zoom } => {
                self.pan.set((pan_x, pan_y));
                self.zoom.set(zoom);
            }

            EditorCommand::AddSample { kind } => self.add_sample_impl(kind),
            EditorCommand::RemoveSample { id } => self.remove_sample_impl(id),
            EditorCommand::CloneSample { id } => self.clone_sample_impl(id),
            EditorCommand::RenameSample { id, name } => self.rename_sample_impl(id, name),
            EditorCommand::SetSampleNotes { id, notes } => self.set_sample_notes_impl(id, notes),
            EditorCommand::SetRoot { id } => self.set_root_impl(id),
            EditorCommand::AddBoundary { port, x, y } => self.add_boundary_impl(port, x, y),
            EditorCommand::AddSampleRef { sample, x, y } => self.add_sample_ref_impl(sample, x, y),
            EditorCommand::SetSampleRef { node, sample } => self.set_sample_ref_impl(node, sample),
            EditorCommand::RenameNode { id, label } => self.rename_node_impl(id, label),
            EditorCommand::SetInputDefault { node, value } => {
                self.set_input_default_impl(node, value)
            }
            EditorCommand::SetInputValue { node, port, value } => {
                self.set_input_value_impl(node, &port, value)
            }
            EditorCommand::SetListener { x, y, z } => self.set_listener_position_impl(x, y, z),
            EditorCommand::Encapsulate { ids } => self.encapsulate_impl(&ids),
            EditorCommand::Paste { clip } => self.paste_impl(clip),
        }
    }

    /// The read counterpart to [`dispatch`](Self::dispatch): answer an
    /// [`EditorQuery`] with a [`QueryResult`]. This is the single read authority a
    /// future MCP transport calls (via [`editor_query_toml`](crate::editor_query_toml)).
    pub fn query(&self, q: EditorQuery) -> QueryResult {
        use command::{AssetInfo, SampleInfo, TransportInfo};
        // Flush the live canvas into its sample first, so reads of the active
        // sample's graph (bounce status / dirty detection, assets, samples) reflect
        // edits made since the last commit — otherwise an uncommitted set_field
        // leaves the bounce looking falsely "clean".
        self.commit_active();
        match q {
            EditorQuery::Snapshot => QueryResult::Snapshot(Box::new(self.snapshot())),
            EditorQuery::Project => QueryResult::Project(Box::new(self.to_project())),
            EditorQuery::Samples => {
                let root = *self.root.borrow();
                let active = *self.active.borrow();
                // Snapshot the ids/kinds first so we don't hold the `samples`
                // borrow while `bounce_status` / `bounce_duration` re-borrow it.
                let rows: Vec<_> = self
                    .samples
                    .borrow()
                    .iter()
                    .map(|s| {
                        (
                            s.sample.id,
                            s.sample.name.clone(),
                            s.sample.kind,
                            s.sample.notes.clone(),
                        )
                    })
                    .collect();
                QueryResult::Samples(
                    rows.into_iter()
                        .map(|(id, name, kind, notes)| {
                            // Bounce state only applies to Sounds.
                            let (bounce, duration_secs) =
                                if kind == awsm_audio_schema::SampleKind::Sound {
                                    (
                                        Some(self.bounce_status(id).as_str().to_string()),
                                        self.bounce_duration(id),
                                    )
                                } else {
                                    (None, None)
                                };
                            SampleInfo {
                                id,
                                name,
                                kind,
                                is_root: id == root,
                                is_active: id == active,
                                bounce,
                                duration_secs,
                                notes,
                            }
                        })
                        .collect(),
                )
            }
            EditorQuery::Assets => QueryResult::Assets(
                self.assets_list()
                    .into_iter()
                    .map(|(id, name, status, dur)| AssetInfo {
                        id,
                        name,
                        bounce: status.as_str().to_string(),
                        duration_secs: dur,
                    })
                    .collect(),
            ),
            EditorQuery::BounceStatus { sample } => {
                QueryResult::BounceStatus(self.bounce_status_str(sample))
            }
            EditorQuery::RenderPlan { sample } => QueryResult::RenderPlan(self.render_plan(sample)),
            EditorQuery::Arrangement => QueryResult::Arrangement(self.active_arrangement()),
            // Served on the async render branch in `remote.rs` (it renders each
            // track); this sync path is never reached for it.
            EditorQuery::ArrangementTrackStats => QueryResult::ArrangementTrackStats(Vec::new()),
            EditorQuery::ArrangementSectionStats { .. } => {
                QueryResult::ArrangementSectionStats(Vec::new())
            }
            EditorQuery::Transport => QueryResult::Transport(TransportInfo {
                playing: self.playing.get(),
                peak: self.audio_peak(),
                playhead: self.playhead.get(),
                audio_state: self.audio_state(),
            }),
            // Discovery: the creatable-node catalog (default value + field keys
            // per kind) so an MCP agent can build a graph with zero schema
            // knowledge.
            EditorQuery::Catalog => {
                use command::NodeKindInfo;
                let mut out = Vec::new();
                for section in crate::catalog::sections() {
                    let section_name = section.name.to_string();
                    for kind in section.kinds {
                        let tag = serde_json::to_value(&kind)
                            .ok()
                            .and_then(|v| {
                                v.get("kind").and_then(|k| k.as_str()).map(str::to_string)
                            })
                            .unwrap_or_default();
                        let label = crate::ports::kind_label(&kind).to_string();
                        let help = crate::catalog::doc(&kind);
                        let fields = crate::fields::fields(&kind)
                            .iter()
                            .map(field_info)
                            .collect();
                        out.push(NodeKindInfo {
                            kind: tag,
                            label,
                            section: section_name.clone(),
                            description: help.body.to_string(),
                            mdn: help.mdn.to_string(),
                            example: kind,
                            fields,
                        });
                    }
                }
                QueryResult::Catalog(out)
            }
            // Discovery: the editable fields of one live node (covers worklet
            // nodes whose params are discovered at runtime).
            EditorQuery::NodeFields { node } => {
                let lock = self.nodes.lock_ref();
                let fields = lock
                    .iter()
                    .find(|n| n.id == node)
                    .map(|n| {
                        let k = n.kind.borrow();
                        crate::fields::fields(&k).iter().map(field_info).collect()
                    })
                    .unwrap_or_default();
                QueryResult::NodeFields(fields)
            }
            // Discovery: every modulatable param across the active canvas — the
            // graph-wide "what can I automate?" map (per-node form: NodeFields).
            EditorQuery::ModulationTargets => {
                use awsm_audio_editor_protocol::ModTargetInfo;
                let lock = self.nodes.lock_ref();
                let out = lock
                    .iter()
                    .filter(|n| n.boundary.is_none())
                    .filter_map(|n| {
                        let k = n.kind.borrow();
                        let params: Vec<String> = crate::fields::fields(&k)
                            .iter()
                            .filter_map(|f| f.modulation.map(|m| m.to_string()))
                            .collect();
                        if params.is_empty() {
                            return None;
                        }
                        let label = {
                            let l = n.label.get_cloned();
                            if l.is_empty() {
                                crate::ports::kind_label(&k).to_string()
                            } else {
                                l
                            }
                        };
                        let tag = serde_json::to_value(&*k)
                            .ok()
                            .and_then(|v| {
                                v.get("kind").and_then(|t| t.as_str()).map(str::to_string)
                            })
                            .unwrap_or_default();
                        Some(ModTargetInfo {
                            node: n.id,
                            label,
                            kind: tag,
                            params,
                        })
                    })
                    .collect();
                QueryResult::ModulationTargets(out)
            }
            // The WAV-readback queries need an async offline render, so the remote
            // transport routes them to a dedicated async path (see `remote.rs`);
            // they never reach this synchronous interpreter.
            EditorQuery::WavStats { .. } | EditorQuery::Waveform { .. } => {
                unreachable!("WavStats/Waveform are answered on the async render path")
            }
        }
    }

    // ==================================================================
    // Gesture helpers — controller-mediated transient state (the UI never
    // pokes these `Mutable`s directly). Each ends in a `dispatch` or a
    // pending-wire mutation; none change the document on their own.
    // ==================================================================

    /// Record the canvas viewport element (called once on mount).
    pub fn set_viewport(&self, el: web_sys::Element) {
        *self.viewport.borrow_mut() = Some(el);
    }

    /// Top-left of the viewport in client coordinates.
    fn viewport_origin(&self) -> (f64, f64) {
        self.viewport
            .borrow()
            .as_ref()
            .map(|el| {
                let r = el.get_bounding_client_rect();
                (r.left(), r.top())
            })
            .unwrap_or((0.0, 0.0))
    }

    /// World coordinates at the center of the viewport (for placing new nodes).
    pub fn world_center(&self) -> (f64, f64) {
        let (w, h) = self
            .viewport
            .borrow()
            .as_ref()
            .map(|el| {
                let r = el.get_bounding_client_rect();
                (r.width(), r.height())
            })
            // Fall back when the viewport isn't measurable yet (e.g. a node added
            // before first layout settles), so we never center on (0, 0).
            .filter(|(w, h)| *w > 1.0 && *h > 1.0)
            .unwrap_or((800.0, 600.0));
        let (px, py) = self.pan.get();
        let z = self.zoom.get();
        ((w / 2.0 - px) / z, (h / 2.0 - py) / z)
    }

    /// Convert a browser client point to canvas world coordinates.
    pub fn client_to_world(&self, client_x: f64, client_y: f64) -> (f64, f64) {
        let (rx, ry) = self.viewport_origin();
        let (px, py) = self.pan.get();
        let z = self.zoom.get();
        (((client_x - rx) - px) / z, ((client_y - ry) - py) / z)
    }

    /// Zoom by `factor` while keeping the world point under the cursor fixed.
    pub fn zoom_at(&self, client_x: f64, client_y: f64, factor: f64) {
        let (rx, ry) = self.viewport_origin();
        let (cx, cy) = (client_x - rx, client_y - ry);
        let old_z = self.zoom.get();
        let new_z = (old_z * factor).clamp(0.25, 3.0);
        let (px, py) = self.pan.get();
        // World point under the cursor must stay put: c = w*z + p.
        let wx = (cx - px) / old_z;
        let wy = (cy - py) / old_z;
        self.dispatch(EditorCommand::SetCamera {
            pan_x: cx - wx * new_z,
            pan_y: cy - wy * new_z,
            zoom: new_z,
        });
    }

    /// Begin dragging a wire out of an output port.
    pub fn begin_wire(&self, from: Rc<EditorNode>, from_output: u32, world: (f64, f64)) {
        self.pending.set(Some(Rc::new(PendingWire {
            from,
            from_output,
            cursor: Mutable::new(world),
        })));
    }

    /// Update the dragged wire's free end (follows the cursor).
    pub fn update_wire(&self, world: (f64, f64)) {
        if let Some(pw) = self.pending.lock_ref().as_ref() {
            pw.cursor.set(world);
        }
    }

    /// Drop a pending wire without connecting.
    pub fn cancel_wire(&self) {
        self.pending.set(None);
    }

    /// Land a pending wire on an input port. The typed port matrix decides: a
    /// **note** sequencer output binds as a trigger (`Bind`); an **audio** output
    /// connects (`Connect`); a **control** sequencer output is rejected here (it
    /// must land on a parameter, not an audio input).
    pub fn commit_wire(&self, to: Rc<EditorNode>, to_input: u32) {
        let Some(pw) = self.pending.lock_ref().clone() else {
            return;
        };
        self.pending.set(None);
        let emit = Self::source_emit(&pw.from.kind.borrow());
        match emit {
            awsm_audio_schema::Emit::Trigger => self.dispatch(EditorCommand::Bind {
                from: pw.from.id,
                from_output: pw.from_output,
                to: to.id,
            }),
            awsm_audio_schema::Emit::Audio => self.dispatch(EditorCommand::Connect {
                from: pw.from.id,
                from_output: pw.from_output,
                to: to.id,
                to_input,
            }),
            awsm_audio_schema::Emit::Control => self.status.set(Some(
                "A control output drives a parameter — wire it to a node's param dot, not an audio input.".into(),
            )),
        }
    }

    /// Land a pending wire on a node param → `Modulate`. A param accepts audio
    /// (modulation) or a control-sequencer stream; a note trigger is rejected.
    pub fn commit_modulation(&self, to: Rc<EditorNode>, param: awsm_audio_schema::ParamId) {
        let Some(pw) = self.pending.lock_ref().clone() else {
            return;
        };
        self.pending.set(None);
        let emit = Self::source_emit(&pw.from.kind.borrow());
        if emit == awsm_audio_schema::Emit::Trigger {
            self.status.set(Some(
                "A note trigger plays an instrument — wire it to a trigger inlet, not a parameter."
                    .into(),
            ));
            return;
        }
        self.dispatch(EditorCommand::Modulate {
            from: pw.from.id,
            from_output: pw.from_output,
            to: to.id,
            param,
        });
    }

    /// The signal a node's *output* emits, for the typed port matrix: a Note
    /// Sequencer emits triggers, a Control Sequencer emits control, everything
    /// else emits audio. (Mirrors `Graph::source_emit` for the live canvas.)
    fn source_emit(kind: &awsm_audio_schema::NodeKind) -> awsm_audio_schema::Emit {
        use awsm_audio_schema::{Emit, NodeKind};
        match kind {
            NodeKind::ControlSequencer(_) => Emit::Control,
            NodeKind::NoteSequencer(_) => Emit::Trigger,
            _ => Emit::Audio,
        }
    }

    // ==================================================================
    // Transport — drive the audio engine. Imperative (side-effecting),
    // controller-mediated; the UI never touches the engine directly.
    // ==================================================================

    /// How the active sample should play — derived from its *content*, not a
    /// stored kind. Timeline data → an arrangement; a graph that drives
    /// instruments (trigger wires) or has a speaker `Output` → a song; otherwise
    /// audition the patch.
    fn play_role(&self) -> PlayRole {
        if self.active_arrangement().is_some() {
            return PlayRole::Arrangement;
        }
        let has_trigger = self
            .connections
            .lock_ref()
            .iter()
            .any(|c| matches!(c.sink, ConnSink::Trigger));
        if has_trigger || self.has_wired_output() {
            return PlayRole::Song;
        }
        PlayRole::Audition
    }

    /// Build the current graph onto the audio engine and start it. The path is
    /// chosen by [`play_role`](Self::play_role): an arrangement performs its
    /// timeline; a song schedules its trigger/control wires (needs a wired
    /// `Output`); otherwise the patch is auditioned to the default destination.
    pub fn play(&self) {
        // Starting fresh (not resuming a pause) records the play origin, so Stop
        // can return there. A reschedule while already playing keeps it.
        if !self.playing.get() && !self.paused.get() {
            self.play_origin.set(self.arrange_start.get());
        }
        self.paused.set_neq(false);
        match self.play_role() {
            PlayRole::Arrangement => {
                self.play_arrangement_sample();
                return;
            }
            PlayRole::Song => {
                self.play_sequence();
                return;
            }
            PlayRole::Audition => {}
        }
        self.status.set(None);
        // Clear any pending auto-stop/loop timer left by a prior song/arrangement
        // pass so it can't fire mid-audition. (An audition has no defined content
        // length, so it isn't itself auto-stopped — it plays until told to stop.)
        self.cancel_song_loop();
        let graph = self.playable_graph();
        let mut slot = self.player.borrow_mut();
        if slot.is_none() {
            match awsm_audio_player::Player::new() {
                Ok(p) => *slot = Some(p),
                Err(e) => {
                    tracing::error!("audio init failed: {e}");
                    return;
                }
            }
        }
        if let Some(p) = slot.as_mut() {
            // Full volume for transport playback (MIDI velocity may have left the
            // master bus attenuated).
            p.set_master_gain(1.0);
            if let Err(e) = p.play(&graph, self.looping.get()) {
                tracing::error!("play failed: {e}");
                return;
            }
        }
        self.playing.set_neq(true);
    }

    /// Whether the canvas has an Output (or Spatial Output) node with at least
    /// one wire landing on it — the requirement to perform a Sequence.
    fn has_wired_output(&self) -> bool {
        let conns = self.connections.lock_ref();
        self.nodes.lock_ref().iter().any(|n| {
            n.boundary.is_none()
                && matches!(
                    &*n.kind.borrow(),
                    awsm_audio_schema::NodeKind::Output(_)
                        | awsm_audio_schema::NodeKind::SpatialOutput(_)
                )
                && conns.iter().any(|c| c.to.id == n.id)
        })
    }

    /// Perform the arrangement (Sequences view). Requires a wired Output, else
    /// surfaces a clear message instead of silently doing nothing.
    fn play_sequence(&self) {
        if !self.has_wired_output() {
            self.status.set(Some(
                "Add an Output node and wire your mix into it to play the sequence.".into(),
            ));
            return;
        }
        self.status.set(None);
        self.play_song();
    }

    /// auto-stop at the content's end.
    fn play_song(&self) {
        self.loop_arrangement.set(false);
        // Shared assembly: the same `sequence_parts` the bounce path + a standalone
        // player use, over the committed document.
        let sp =
            awsm_audio_player::document::sequence_parts(&self.to_library(), *self.active.borrow());
        let content_secs = (sp.loop_secs > 0.0).then(|| sp.loop_secs.max(0.05));
        if sp.triggers.is_empty() && sp.control.is_empty() {
            self.status.set(Some(
                "Wire a sequencer output to an instrument's trigger inlet (or a parameter) to play."
                    .into(),
            ));
        }
        if !self.ensure_player() {
            return;
        }
        self.cancel_song_loop(); // invalidate any prior loop timer
        let gen = self.song_gen.get();
        let mut at = 0.0;
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.set_master_gain(1.0);
            if let Err(e) = p.play_arrangement(&sp.graph, self.looping.get()) {
                tracing::error!("play arrangement failed: {e}");
                return;
            }
            at = p.current_time() + 0.1;
            if let Err(e) = p.schedule_triggers(&sp.triggers, at) {
                tracing::error!("schedule_triggers failed: {e}");
            }
            p.schedule_control(&sp.control, at);
        }
        self.song_start.set(at);
        self.playing.set_neq(true);
        if let Some(ls) = content_secs {
            if self.looping.get() {
                self.song_loop_secs.set(ls);
                self.song_next_start.set(at + ls);
                self.arm_song_loop(gen);
            } else {
                // Not looping: return to idle when the content finishes (so the
                // transport doesn't sit "playing" forever and a later play starts
                // fresh rather than from a mid-timeline position).
                self.arm_auto_stop(gen, ls);
            }
        }
    }

    /// A read-only clone of the active sample if it's an Arrangement.
    /// If an arrangement is playing, re-schedule from the current playhead so a
    /// track mute/solo change takes effect immediately (clips are scheduled in
    /// advance, so a toggle otherwise wouldn't be heard until the next loop).
    fn reschedule_arrangement(&self) {
        if self.playing.get() && self.loop_arrangement.get() {
            let head = self.arrange_playhead.get().max(0.0);
            self.set_arrange_start(head);
        }
    }

    fn active_arrangement(&self) -> Option<awsm_audio_schema::Arrangement> {
        let active = *self.active.borrow();
        self.samples
            .borrow()
            .iter()
            .find(|s| {
                s.sample.id == active && s.sample.kind == awsm_audio_schema::SampleKind::Arrangement
            })
            .map(|s| s.sample.arrangement.clone())
    }

    /// The bounced-buffer asset id of a Sound, if it has a (current or stale)
    /// bounce.
    fn bounce_asset(
        &self,
        source: awsm_audio_schema::SampleId,
    ) -> Option<awsm_audio_schema::AssetId> {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == source)
            .and_then(|s| s.sample.bounce.as_ref().map(|b| b.asset))
    }

    /// Duration (seconds) of a Sound's bounced buffer, from the stored PCM asset.
    pub fn bounce_duration(&self, source: awsm_audio_schema::SampleId) -> Option<f64> {
        use awsm_audio_schema::AudioSource;
        let asset = self.bounce_asset(source)?;
        let bufs = self.buffer_assets.borrow();
        let ba = bufs.get(&asset)?;
        match &ba.source {
            AudioSource::Pcm {
                sample_rate,
                channels,
            } => channels
                .first()
                .map(|c| c.len() as f64 / (*sample_rate as f64).max(1.0)),
            _ => None,
        }
    }

    /// Compile the active Arrangement's clips into scheduled audio-buffer parts,
    /// resolving each clip's source bounce, folding in track + clip gain, and
    /// applying the scrub seek (drop clips fully before it; trim the rest).
    fn arrangement_audio_clips(&self) -> Vec<awsm_audio_player::AudioClipPart> {
        // Delegate to the shared player assembly (the active sample is the
        // arrangement; non-arrangements resolve to no clips).
        awsm_audio_player::document::audio_clip_parts(
            &self.to_library(),
            *self.active.borrow(),
            self.arrange_start.get().max(0.0),
        )
    }

    /// The stored [`Arrangement`](awsm_audio_schema::Arrangement) of sample `id`
    /// (any sample, not just the active one — arrangement edits write straight to
    /// `sample.arrangement`). `None` if `id` isn't an Arrangement sample.
    fn arrangement_for(
        &self,
        id: awsm_audio_schema::SampleId,
    ) -> Option<awsm_audio_schema::Arrangement> {
        self.samples
            .borrow()
            .iter()
            .find(|s| {
                s.sample.id == id && s.sample.kind == awsm_audio_schema::SampleKind::Arrangement
            })
            .map(|s| s.sample.arrangement.clone())
    }

    /// Set the arrangement scrub/playback-start position (seconds), move the
    /// playhead there, and restart playback from it if currently playing.
    /// The current arrangement scrub/playhead-start position (seconds).
    pub fn arrange_start_secs(&self) -> f64 {
        self.arrange_start.get().max(0.0)
    }

    pub fn set_arrange_start(&self, secs: f64) {
        let secs = secs.max(0.0);
        self.arrange_start.set(secs);
        self.arrange_playhead.set(secs);
        if self.playing.get() && self.loop_arrangement.get() {
            self.play_arrangement_sample();
        }
    }

    /// Set the loop/export **in** marker to the current playhead (keeps the out).
    pub fn arrange_set_loop_in(&self) {
        let start = self.arrange_start_secs();
        let end = self.active_arrangement().and_then(|a| a.loop_end);
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::SetMarkers {
                start: Some(start),
                end,
            },
        });
    }

    /// Set the loop/export **out** marker to the current playhead (keeps the in).
    pub fn arrange_set_loop_out(&self) {
        let end = self.arrange_start_secs();
        let start = self.active_arrangement().and_then(|a| a.loop_start);
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::SetMarkers {
                start,
                end: Some(end),
            },
        });
    }

    /// Clear the loop/export markers (loop + export span the whole timeline).
    pub fn arrange_clear_loop(&self) {
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::SetMarkers {
                start: None,
                end: None,
            },
        });
    }

    /// The live arrangement playhead in seconds (scrub start + elapsed), or `None`
    /// when not performing an arrangement. Driven each frame by the waveform loop.
    pub fn arrangement_playhead_secs(&self) -> Option<f64> {
        if !self.playing.get() || !self.loop_arrangement.get() {
            return None;
        }
        let now = self.player.borrow().as_ref()?.current_time();
        let elapsed = (now - self.song_start.get()).max(0.0);
        Some(self.arrange_start.get() + elapsed)
    }

    /// Perform the active Arrangement (Arrange view): schedule its bounced audio
    /// clips, arming a loop region (seek → end) if Loop is on.
    fn play_arrangement_sample(&self) {
        self.loop_arrangement.set(true);
        self.status.set(None);
        // Loop/export markers, when set, drive the playback window: start at the
        // marker start and loop the marked region.
        if let Some(arr) = self.active_arrangement() {
            if arr.has_markers() {
                let (s, _) = arr.range();
                self.arrange_start.set(s);
                self.arrange_playhead.set(s);
            }
        }
        let clips = self.arrangement_audio_clips();
        if clips.is_empty() {
            self.status.set(Some(
                "Bounce a Sound (Assets panel) and drop it on a track to play.".into(),
            ));
        }
        if !self.ensure_player() {
            return;
        }
        self.cancel_song_loop();
        let gen = self.song_gen.get();
        let mut at = 0.0;
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.set_master_gain(1.0);
            p.arrange_audio_begin();
            at = p.current_time() + 0.1;
            let _ = p.schedule_audio_clips(&clips, at);
        }
        self.song_start.set(at);
        self.playing.set_neq(true);
        // The play region: the marked range, else from the start to the timeline
        // end. Drives the loop length (loop on) or the auto-stop point (loop off).
        let region = self
            .active_arrangement()
            .map(|a| {
                if a.has_markers() {
                    let (s, e) = a.range();
                    (e - s).max(0.1)
                } else {
                    (a.length_secs - self.arrange_start.get().max(0.0)).max(0.1)
                }
            })
            .unwrap_or(0.0);
        if region > 0.1 {
            if self.looping.get() {
                self.song_loop_secs.set(region);
                self.song_next_start.set(at + region);
                self.arm_song_loop(gen);
            } else {
                // Not looping: stop + return to idle when the timeline ends.
                self.arm_auto_stop(gen, region);
            }
        }
    }

    /// Schedule the next loop pass when the timer fires (if still current).
    fn song_loop_tick(&self, gen: u64) {
        if gen != self.song_gen.get() || !self.playing.get() {
            return;
        }
        let start = self.song_next_start.get();
        if self.loop_arrangement.get() {
            // Arrangement: re-schedule the audio clips (picks up edits).
            let clips = self.arrangement_audio_clips();
            if clips.is_empty() {
                return;
            }
            if let Some(p) = self.player.borrow_mut().as_mut() {
                let _ = p.schedule_audio_clips(&clips, start);
            }
        } else {
            // Sequences song: re-schedule note triggers + control (shared assembly).
            let sp = awsm_audio_player::document::sequence_parts(
                &self.to_library(),
                *self.active.borrow(),
            );
            if (sp.triggers.is_empty() && sp.control.is_empty()) || sp.loop_secs <= 0.0 {
                return;
            }
            if let Some(p) = self.player.borrow_mut().as_mut() {
                let _ = p.schedule_triggers(&sp.triggers, start);
                p.schedule_control(&sp.control, start);
            }
        }
        self.song_start.set(start);
        self.song_next_start.set(start + self.song_loop_secs.get());
        self.arm_song_loop(gen);
    }

    /// Arm a `setTimeout` to schedule the next loop pass shortly before the
    /// current one ends. The closure is kept alive in `song_timer`; the prior
    /// timer is cleared first.
    fn arm_song_loop(&self, gen: u64) {
        let Some(win) = web_sys::window() else { return };
        // Fire ~0.25 s before the next pass is due so it's scheduled ahead.
        let now = self
            .player
            .borrow()
            .as_ref()
            .map(|p| p.current_time())
            .unwrap_or(0.0);
        let delay_ms = ((self.song_next_start.get() - now - 0.25).max(0.05) * 1000.0) as i32;
        let ctrl = self.clone();
        let cb = Closure::<dyn FnMut()>::new(move || ctrl.song_loop_tick(gen));
        // Clear any previous timer before replacing it.
        if let Some((id, _)) = self.song_timer.borrow_mut().take() {
            win.clear_timeout_with_handle(id);
        }
        match win.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            delay_ms,
        ) {
            Ok(id) => *self.song_timer.borrow_mut() = Some((id, cb)),
            Err(e) => tracing::error!("loop timer failed: {e:?}"),
        }
    }

    /// Cancel any pending loop timer and invalidate in-flight ticks.
    fn cancel_song_loop(&self) {
        self.song_gen.set(self.song_gen.get().wrapping_add(1));
        if let Some((id, _)) = self.song_timer.borrow_mut().take() {
            if let Some(win) = web_sys::window() {
                win.clear_timeout_with_handle(id);
            }
        }
    }

    /// Arm a one-shot timer to stop the transport `secs` into a non-looping pass —
    /// when the content finishes — returning to idle. Reuses the `song_timer` slot
    /// (a non-looping pass arms no loop timer). A small tail past the content end
    /// lets the final release / reverb ring out before the teardown.
    fn arm_auto_stop(&self, gen: u64, secs: f64) {
        let Some(win) = web_sys::window() else { return };
        let delay_ms = ((secs + 0.25).max(0.05) * 1000.0) as i32;
        let ctrl = self.clone();
        let cb = Closure::<dyn FnMut()>::new(move || ctrl.auto_stop_fired(gen));
        // Clear any previous timer before replacing it.
        if let Some((id, _)) = self.song_timer.borrow_mut().take() {
            win.clear_timeout_with_handle(id);
        }
        match win.set_timeout_with_callback_and_timeout_and_arguments_0(
            cb.as_ref().unchecked_ref(),
            delay_ms,
        ) {
            Ok(id) => *self.song_timer.borrow_mut() = Some((id, cb)),
            Err(e) => tracing::error!("auto-stop timer failed: {e:?}"),
        }
    }

    /// The auto-stop timer fired: tear playback down and return to idle (mirrors
    /// [`stop`](Self::stop)). Deliberately does **not** touch `song_timer` — we're
    /// running inside its own closure, so taking + dropping it here would free the
    /// closure on the stack. The next `play`/`stop`/`cancel_song_loop` clears the
    /// (now-finished) slot. Stale fires (a newer pass superseded us, or already
    /// stopped) are ignored via the generation check.
    fn auto_stop_fired(&self, gen: u64) {
        if gen != self.song_gen.get() || !self.playing.get() {
            return;
        }
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.stop();
        }
        self.playing.set_neq(false);
        self.paused.set_neq(false);
        self.status.set(None);
        // Return to where this play session began (the play origin).
        let origin = self.play_origin.get();
        self.arrange_start.set(origin);
        self.arrange_playhead.set(origin);
        // Invalidate any other in-flight tick; leaves `song_timer` intact (us).
        self.song_gen.set(self.song_gen.get().wrapping_add(1));
    }

    /// Playback position within the current song pass, in beats — for the
    /// piano-roll playhead. `None` when not playing a song.
    pub fn song_playhead_beats(&self, node: NodeId) -> Option<f64> {
        if !self.playing.get() {
            return None;
        }
        let now = self.player.borrow().as_ref()?.current_time();
        let elapsed = now - self.song_start.get();
        if elapsed < 0.0 {
            return None;
        }
        let ms = self.song_node(node)?;
        Some(ms.song.secs_to_beats(elapsed) + ms.start)
    }

    // ==================================================================
    // Note Sequencer node editing.
    //
    // A Note Sequencer owns a `Song` plus one `SoundOut` per distinct sound: a
    // melodic track is one sound (the whole track), a drum track is one sound
    // per distinct percussion note used. Each `SoundOut` is an output port,
    // identified by a stable `key` ("t0", "t2:n36"), wired to an instrument by
    // identity — so adding/removing sounds never silently re-routes a wire.
    // ==================================================================

    /// Stable wire key for a track's melodic sound.
    fn track_key(track: usize) -> awsm_audio_schema::SeqKey {
        format!("t{track}")
    }

    /// Stable wire key for one drum note on a track.
    fn drum_key(track: usize, note: u8) -> awsm_audio_schema::SeqKey {
        format!("t{track}:n{note}")
    }

    /// Derive the sound-output set for a song, carrying over per-output edits
    /// (label / transpose / gain) from `prev` whose key still matches.
    fn outputs_for_song(
        mode: awsm_audio_schema::SequencerMode,
        song: &awsm_audio_schema::Song,
        prev: &[awsm_audio_schema::SoundOut],
    ) -> Vec<awsm_audio_schema::SoundOut> {
        use awsm_audio_schema::SoundOut;
        let carry = |key: &str, mk: &dyn Fn() -> SoundOut| -> SoundOut {
            prev.iter()
                .find(|o| o.key == key)
                .cloned()
                .unwrap_or_else(mk)
        };
        let mut outs = Vec::new();
        for (t, track) in song.tracks.iter().enumerate() {
            if mode.is_drum() {
                let mut notes: Vec<u8> = track.events.iter().map(|e| e.note).collect();
                notes.sort_unstable();
                notes.dedup();
                for n in notes {
                    let key = Self::drum_key(t, n);
                    outs.push(carry(&key, &|| SoundOut {
                        key: key.clone(),
                        track: t,
                        note: Some(n),
                        label: format!(
                            "{} \u{b7} {}",
                            track.name,
                            awsm_audio_schema::gm_drum_name(n).unwrap_or("perc")
                        ),
                        transpose: 0,
                        gain: 1.0,
                    }));
                }
            } else {
                let key = Self::track_key(t);
                outs.push(carry(&key, &|| SoundOut {
                    key: key.clone(),
                    track: t,
                    note: None,
                    label: if track.name.is_empty() {
                        format!("track {}", t + 1)
                    } else {
                        track.name.clone()
                    },
                    transpose: 0,
                    gain: 1.0,
                }));
            }
        }
        outs
    }

    /// Snapshot of a sequencer node's data, for the inspector / piano roll.
    pub fn song_node(&self, node: NodeId) -> Option<awsm_audio_schema::NoteSequencerNode> {
        let n = self.node_by_id(node)?;
        let out = match &*n.kind.borrow() {
            awsm_audio_schema::NodeKind::NoteSequencer(ms) => Some(ms.clone()),
            _ => None,
        };
        out
    }

    /// Mutate sequencer node `id` in place, refresh its card (port count may
    /// change) + inspector, and snapshot for undo when `undo` is set.
    fn edit_song(
        &self,
        id: NodeId,
        undo: bool,
        f: impl FnOnce(&mut awsm_audio_schema::NoteSequencerNode),
    ) {
        let Some(n) = self.node_by_id(id) else { return };
        if undo {
            self.push_undo();
        }
        {
            let mut k = n.kind.borrow_mut();
            let awsm_audio_schema::NodeKind::NoteSequencer(ms) = &mut *k else {
                return;
            };
            f(ms);
        }
        self.touch_node(id);
        self.inspector_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Load a `.mid` file onto a sequencer node (async read + parse).
    pub fn load_midi_file(&self, node: NodeId, file: web_sys::File) {
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let buf = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("read .mid: {e:?}");
                    return;
                }
            };
            let bytes =
                js_sys::Uint8Array::new(&buf.unchecked_into::<js_sys::ArrayBuffer>()).to_vec();
            match awsm_audio_schema::parse_smf(&bytes) {
                Ok(song) => ctrl.set_song(node, song),
                Err(e) => tracing::error!("MIDI parse failed: {e}"),
            }
        });
    }

    /// Replace a sequencer node's song, regenerating its sound outputs.
    fn set_song(&self, node: NodeId, song: awsm_audio_schema::Song) {
        self.edit_song(node, true, move |ms| {
            ms.outputs = Self::outputs_for_song(ms.mode, &song, &[]);
            ms.start = 0.0;
            ms.song = song;
        });
    }

    /// Set the playback-window start (beats); kept below the stop if one is set.
    fn set_song_start(&self, node: NodeId, beats: f64) {
        self.edit_song(node, false, |ms| {
            let start = beats.max(0.0);
            ms.start = match ms.end {
                Some(end) => start.min((end - 0.25).max(0.0)),
                None => start,
            };
        });
    }

    /// Set the playback-window stop (beats); `None` plays to the song end. Kept
    /// above the start.
    fn set_song_end(&self, node: NodeId, beats: Option<f64>) {
        self.edit_song(node, false, |ms| {
            ms.end = beats.map(|b| b.max(ms.start + 0.25));
        });
    }

    /// Set the authored grid length (beats); `0` = auto-fit content.
    fn set_song_length(&self, node: NodeId, beats: f64) {
        self.edit_song(node, false, |ms| ms.length = beats.max(0.0));
    }

    /// Set the song tempo (BPM).
    fn set_song_bpm(&self, node: NodeId, bpm: f64) {
        self.edit_song(node, false, |ms| ms.song.bpm = bpm.max(1.0));
    }

    /// Toggle whole-song looping.
    fn set_song_loop(&self, node: NodeId, on: bool) {
        self.edit_song(node, false, |ms| ms.looping = on);
    }

    /// Set sound-output `idx`'s semitone transpose.
    fn set_output_transpose(&self, node: NodeId, idx: usize, semitones: i32) {
        self.edit_song(node, false, |ms| {
            if let Some(o) = ms.outputs.get_mut(idx) {
                o.transpose = semitones;
            }
        });
    }

    /// Set sound-output `idx`'s linear gain.
    fn set_output_gain(&self, node: NodeId, idx: usize, gain: f32) {
        self.edit_song(node, false, |ms| {
            if let Some(o) = ms.outputs.get_mut(idx) {
                o.gain = gain.max(0.0);
            }
        });
    }

    /// Rename sound-output `idx` (display label only; its wire key is stable).
    fn set_output_label(&self, node: NodeId, idx: usize, label: String) {
        self.edit_song(node, false, |ms| {
            if let Some(o) = ms.outputs.get_mut(idx) {
                o.label = label;
            }
        });
    }

    /// Add an empty track and regenerate outputs, so it can be authored (piano
    /// roll) and wired immediately.
    fn add_song_track(&self, node: NodeId) {
        use awsm_audio_schema::Track;
        self.edit_song(node, true, |ms| {
            let n = ms.song.tracks.len();
            ms.song.tracks.push(Track {
                name: format!("track {}", n + 1),
                ..Default::default()
            });
            ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
        });
    }

    /// Add a note to a track (piano-roll authoring); re-derive outputs so a new
    /// drum note gets its own sound.
    fn add_song_note(&self, node: NodeId, track: usize, ev: awsm_audio_schema::NoteEvent) {
        self.edit_song(node, false, |ms| {
            if let Some(t) = ms.song.tracks.get_mut(track) {
                t.events.push(ev);
                t.events.sort_by(|a, b| {
                    a.start
                        .partial_cmp(&b.start)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
        });
    }

    /// Replace the note at `idx` (move / resize / re-velocity), keeping the
    /// track time-ordered and outputs in sync.
    fn update_song_note(
        &self,
        node: NodeId,
        track: usize,
        idx: usize,
        ev: awsm_audio_schema::NoteEvent,
    ) {
        self.edit_song(node, false, |ms| {
            if let Some(t) = ms.song.tracks.get_mut(track) {
                if idx < t.events.len() {
                    t.events[idx] = ev;
                    t.events.sort_by(|a, b| {
                        a.start
                            .partial_cmp(&b.start)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
            }
            ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
        });
    }

    /// Remove the note at `idx` from a track (outputs re-derived).
    fn remove_song_note(&self, node: NodeId, track: usize, idx: usize) {
        self.edit_song(node, false, |ms| {
            if let Some(t) = ms.song.tracks.get_mut(track) {
                if idx < t.events.len() {
                    t.events.remove(idx);
                }
            }
            ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
        });
    }

    /// Replace every event of `track` in one shot (bulk pattern authoring), kept
    /// time-ordered, then re-derive outputs so new drum notes get their own
    /// sounds. Snapshots for undo since it can change the whole track at once.
    fn set_song_track_events(
        &self,
        node: NodeId,
        track: usize,
        events: Vec<awsm_audio_schema::NoteEvent>,
    ) {
        self.edit_song(node, true, move |ms| {
            if let Some(t) = ms.song.tracks.get_mut(track) {
                t.events = events;
                t.events.sort_by(|a, b| {
                    a.start
                        .partial_cmp(&b.start)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
            ms.outputs = Self::outputs_for_song(ms.mode, &ms.song, &ms.outputs);
        });
    }

    // ==================================================================
    // Control Sequencer node editing. A Control Sequencer owns lanes; each lane
    // is a keyed output port wired to a node parameter (`SeqOut → NodeParam`),
    // whose value-over-time (breakpoints in beats) automates that param.
    // ==================================================================

    /// Snapshot of a control-sequencer node's data, for the inspector.
    pub fn control_node(&self, node: NodeId) -> Option<awsm_audio_schema::ControlSequencerNode> {
        let n = self.node_by_id(node)?;
        let out = match &*n.kind.borrow() {
            awsm_audio_schema::NodeKind::ControlSequencer(cs) => Some(cs.clone()),
            _ => None,
        };
        out
    }

    /// Mutate a control-sequencer node, refresh its card + inspector; `undo`
    /// snapshots (use for lane add/remove which change the port count).
    fn edit_control(
        &self,
        id: NodeId,
        undo: bool,
        f: impl FnOnce(&mut awsm_audio_schema::ControlSequencerNode),
    ) {
        let Some(n) = self.node_by_id(id) else { return };
        if undo {
            self.push_undo();
        }
        {
            let mut k = n.kind.borrow_mut();
            let awsm_audio_schema::NodeKind::ControlSequencer(cs) = &mut *k else {
                return;
            };
            f(cs);
        }
        self.touch_node(id);
        self.inspector_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// A lane key not already used on this node: `lane0`, `lane1`, …
    fn unique_lane_key(lanes: &[awsm_audio_schema::ControlLane]) -> String {
        (0..)
            .map(|i| format!("lane{i}"))
            .find(|k| !lanes.iter().any(|l| &l.key == k))
            .unwrap()
    }

    /// Set the control sequencer's tempo (BPM for its lanes' beat positions).
    fn set_control_bpm(&self, node: NodeId, bpm: f64) {
        self.edit_control(node, false, |cs| cs.bpm = bpm.max(1.0));
    }

    /// Set the control sequencer's start offset (beats).
    fn set_control_start(&self, node: NodeId, beats: f64) {
        self.edit_control(node, false, |cs| cs.start = beats.max(0.0));
    }

    /// Toggle control-sequencer looping.
    fn set_control_loop(&self, node: NodeId, on: bool) {
        self.edit_control(node, false, |cs| cs.looping = on);
    }

    /// Add a new (empty) automation lane — a fresh output port to wire to a param.
    fn add_control_lane(&self, node: NodeId) {
        self.edit_control(node, true, |cs| {
            let key = Self::unique_lane_key(&cs.lanes);
            let n = cs.lanes.len() + 1;
            cs.lanes.push(awsm_audio_schema::ControlLane {
                key,
                label: format!("lane {n}"),
                points: Vec::new(),
            });
        });
    }

    /// Remove lane `idx` (and its output port).
    fn remove_control_lane(&self, node: NodeId, idx: usize) {
        self.edit_control(node, true, |cs| {
            if idx < cs.lanes.len() {
                cs.lanes.remove(idx);
            }
        });
    }

    /// Rename lane `idx` (display only; its wire key is stable).
    fn set_control_lane_label(&self, node: NodeId, idx: usize, label: String) {
        self.edit_control(node, false, |cs| {
            if let Some(l) = cs.lanes.get_mut(idx) {
                l.label = label;
            }
        });
    }

    /// Add a breakpoint `(beat, value)` to lane `idx`, keeping it beat-ordered.
    fn add_control_point(&self, node: NodeId, idx: usize, beat: f64, value: f32) {
        self.edit_control(node, false, |cs| {
            if let Some(l) = cs.lanes.get_mut(idx) {
                l.points.push(awsm_audio_schema::ControlPoint {
                    beat: beat.max(0.0),
                    value,
                    curve: awsm_audio_schema::Curve::Linear,
                });
                l.points.sort_by(|a, b| {
                    a.beat
                        .partial_cmp(&b.beat)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            }
        });
    }

    /// Remove breakpoint `pt` from lane `idx`.
    fn remove_control_point(&self, node: NodeId, idx: usize, pt: usize) {
        self.edit_control(node, false, |cs| {
            if let Some(l) = cs.lanes.get_mut(idx) {
                if pt < l.points.len() {
                    l.points.remove(pt);
                }
            }
        });
    }

    /// Set the curve shape of breakpoint `pt` in lane `idx` (the segment that
    /// reaches it from the previous point).
    fn set_control_point_curve(
        &self,
        node: NodeId,
        idx: usize,
        pt: usize,
        curve: awsm_audio_schema::Curve,
    ) {
        self.edit_control(node, false, |cs| {
            if let Some(p) = cs.lanes.get_mut(idx).and_then(|l| l.points.get_mut(pt)) {
                p.curve = curve;
            }
        });
    }

    /// Replace all breakpoints of lane `idx` (used by drag / clear).
    fn set_control_points(
        &self,
        node: NodeId,
        idx: usize,
        points: Vec<awsm_audio_schema::ControlPoint>,
    ) {
        self.edit_control(node, false, |cs| {
            if let Some(l) = cs.lanes.get_mut(idx) {
                let mut points = points;
                points.sort_by(|a, b| {
                    a.beat
                        .partial_cmp(&b.beat)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                l.points = points;
            }
        });
    }

    /// Load `.mid` controller-change data onto a control sequencer: one lane per
    /// (channel, CC) with events, values normalized 0..1 (async read + parse).
    pub fn load_midi_cc(&self, node: NodeId, file: web_sys::File) {
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let buf = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("read .mid: {e:?}");
                    return;
                }
            };
            let bytes =
                js_sys::Uint8Array::new(&buf.unchecked_into::<js_sys::ArrayBuffer>()).to_vec();
            match awsm_audio_schema::parse_smf_control(&bytes) {
                Ok(lanes) => {
                    if lanes.is_empty() {
                        ctrl.status
                            .set(Some("That .mid has no CC automation.".into()));
                        return;
                    }
                    ctrl.edit_control(node, true, |cs| {
                        for (label, points) in lanes {
                            let key = Self::unique_lane_key(&cs.lanes);
                            cs.lanes.push(awsm_audio_schema::ControlLane {
                                key,
                                label,
                                points: points
                                    .into_iter()
                                    .map(|(beat, value)| awsm_audio_schema::ControlPoint {
                                        beat,
                                        value,
                                        curve: awsm_audio_schema::Curve::Linear,
                                    })
                                    .collect(),
                            });
                        }
                    });
                }
                Err(e) => tracing::error!("MIDI CC parse failed: {e}"),
            }
        });
    }

    // ==================================================================
    // Command routers: map a serializable op (the MCP/serde write surface) to
    // the node-specific editor methods above. Each `EditSong/Control/Live`
    // command dispatches here; the methods own their undo snapshots.
    // ==================================================================

    /// Route a [`SongOp`] to the Note Sequencer editors.
    fn edit_song_op(&self, node: NodeId, op: SongOp) {
        match op {
            SongOp::SetBpm(v) => self.set_song_bpm(node, v),
            SongOp::SetStart(v) => self.set_song_start(node, v),
            SongOp::SetEnd(v) => self.set_song_end(node, v),
            SongOp::SetLength(v) => self.set_song_length(node, v),
            SongOp::SetLooping(on) => self.set_song_loop(node, on),
            SongOp::AddTrack => self.add_song_track(node),
            SongOp::AddNote { track, event } => self.add_song_note(node, track, event),
            SongOp::UpdateNote {
                track,
                index,
                event,
            } => self.update_song_note(node, track, index, event),
            SongOp::RemoveNote { track, index } => self.remove_song_note(node, track, index),
            SongOp::SetTrackEvents { track, events } => {
                self.set_song_track_events(node, track, events)
            }
            SongOp::SetOutputTranspose { index, semitones } => {
                self.set_output_transpose(node, index, semitones)
            }
            SongOp::SetOutputGain { index, gain } => self.set_output_gain(node, index, gain),
            SongOp::SetOutputLabel { index, label } => self.set_output_label(node, index, label),
        }
    }

    /// Route a [`ControlOp`] to the Control Sequencer editors.
    fn edit_control_op(&self, node: NodeId, op: ControlOp) {
        match op {
            ControlOp::SetBpm(v) => self.set_control_bpm(node, v),
            ControlOp::SetStart(v) => self.set_control_start(node, v),
            ControlOp::SetLooping(on) => self.set_control_loop(node, on),
            ControlOp::AddLane => self.add_control_lane(node),
            ControlOp::RemoveLane { index } => self.remove_control_lane(node, index),
            ControlOp::SetLaneLabel { index, label } => {
                self.set_control_lane_label(node, index, label)
            }
            ControlOp::AddPoint { lane, beat, value } => {
                self.add_control_point(node, lane, beat, value)
            }
            ControlOp::RemovePoint { lane, index } => self.remove_control_point(node, lane, index),
            ControlOp::SetPoints { lane, points } => self.set_control_points(node, lane, points),
            ControlOp::SetPointCurve { lane, index, curve } => {
                self.set_control_point_curve(node, lane, index, curve)
            }
        }
    }

    /// Mutate the active Arrangement sample's data in place, then bump the
    /// samples revision so the Arrange view re-renders. `undo` pushes a snapshot
    /// first (arrangement state is captured by [`EditorSnapshot`]); pass `false`
    /// for continuous value tweaks (e.g. the BPM field) so they don't spam undo.
    fn edit_arrange(&self, undo: bool, f: impl FnOnce(&mut awsm_audio_schema::Arrangement)) {
        if undo {
            self.push_undo();
        }
        let active = *self.active.borrow();
        {
            let mut samples = self.samples.borrow_mut();
            let Some(st) = samples.iter_mut().find(|s| {
                s.sample.id == active && s.sample.kind == awsm_audio_schema::SampleKind::Arrangement
            }) else {
                return;
            };
            f(&mut st.sample.arrangement);
        }
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
        self.inspector_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Route an [`ArrangeOp`] to the active Arrangement sample.
    fn edit_arrange_op(&self, op: ArrangeOp) {
        use awsm_audio_schema::{ArrTrack, Clip};
        match op {
            ArrangeOp::SetBpm(v) => self.edit_arrange(false, |a| a.bpm = v.clamp(20.0, 400.0)),
            ArrangeOp::SetLengthSecs(v) => {
                self.edit_arrange(false, |a| a.length_secs = v.clamp(1.0, 3600.0))
            }
            ArrangeOp::SetMarkers { start, end } => self.edit_arrange(true, |a| {
                // Normalize: clamp into the timeline, keep start < end, or clear.
                a.loop_start = start.map(|s| s.clamp(0.0, a.length_secs));
                a.loop_end = end.map(|e| e.clamp(0.0, a.length_secs));
                if let (Some(s), Some(e)) = (a.loop_start, a.loop_end) {
                    if e <= s {
                        a.loop_start = None;
                        a.loop_end = None;
                    }
                }
            }),
            ArrangeOp::AddTrack => self.edit_arrange(true, |a| {
                let n = a.tracks.len() + 1;
                a.tracks.push(ArrTrack {
                    name: format!("Track {n}"),
                    ..Default::default()
                });
            }),
            ArrangeOp::RemoveTrack { track } => self.edit_arrange(true, |a| {
                if track < a.tracks.len() {
                    a.tracks.remove(track);
                }
            }),
            ArrangeOp::SetTrackName { track, name } => self.edit_arrange(true, |a| {
                if let Some(t) = a.tracks.get_mut(track) {
                    t.name = name;
                }
            }),
            ArrangeOp::SetTrackGain { track, gain } => self.edit_arrange(false, |a| {
                if let Some(t) = a.tracks.get_mut(track) {
                    t.gain = gain.clamp(0.0, 4.0);
                }
            }),
            ArrangeOp::SetTrackGainPoints { track, mut points } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        awsm_audio_schema::normalize_gain_points(&mut points);
                        t.gain_automation = points;
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::AddTrackGainPoint { track, point } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        t.gain_automation.push(point);
                        awsm_audio_schema::normalize_gain_points(&mut t.gain_automation);
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::RemoveTrackGainPoint { track, index } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        if index < t.gain_automation.len() {
                            t.gain_automation.remove(index);
                        }
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::MoveTrackGainPoint {
                track,
                index,
                time,
                gain,
            } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        if let Some(point) = t.gain_automation.get_mut(index) {
                            point.time = time.max(0.0);
                            point.gain = gain.clamp(0.0, 4.0);
                            awsm_audio_schema::normalize_gain_points(&mut t.gain_automation);
                        }
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::ClearTrackGainAutomation { track } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        t.gain_automation.clear();
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::SetTrackMute { track, mute } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        t.mute = mute;
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::SetTrackSolo { track, solo } => {
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        t.solo = solo;
                    }
                });
                self.reschedule_arrangement();
            }
            ArrangeOp::AddClip {
                track,
                start,
                source,
                length,
            } => {
                // Clip length: explicit (Draw tool) or the full bounce duration.
                let full = self.bounce_duration(source).unwrap_or(1.0).max(0.05);
                let length = length.map(|l| l.clamp(0.05, full)).unwrap_or(full);
                let name = self.sample_name(source).unwrap_or_default();
                self.edit_arrange(true, |a| {
                    if let Some(t) = a.tracks.get_mut(track) {
                        t.clips.push(Clip {
                            start: start.max(0.0),
                            length,
                            source,
                            offset: 0.0,
                            gain: 1.0,
                            looping: false,
                            speed: 1.0,
                            name,
                        });
                    }
                });
            }
            ArrangeOp::RemoveClip { track, clip } => self.edit_arrange(true, |a| {
                if let Some(t) = a.tracks.get_mut(track) {
                    if clip < t.clips.len() {
                        t.clips.remove(clip);
                    }
                }
            }),
            ArrangeOp::PasteClip { track, mut clip } => self.edit_arrange(true, |a| {
                if let Some(t) = a.tracks.get_mut(track) {
                    clip.start = clip.start.max(0.0);
                    t.clips.push(clip);
                }
            }),
            ArrangeOp::PasteClips { clips } => self.edit_arrange(true, |a| {
                for pc in clips {
                    let mut clip = pc.clip;
                    if let Some(t) = a.tracks.get_mut(pc.track) {
                        clip.start = clip.start.max(0.0);
                        t.clips.push(clip);
                    }
                }
            }),
            ArrangeOp::MoveClip {
                track,
                clip,
                new_track,
                start,
            } => self.edit_arrange(true, |a| {
                if track >= a.tracks.len() || new_track >= a.tracks.len() {
                    return;
                }
                if clip >= a.tracks[track].clips.len() {
                    return;
                }
                if new_track == track {
                    // Same track: edit in place so the clip's index (and thus the
                    // UI selection) is preserved.
                    a.tracks[track].clips[clip].start = start.max(0.0);
                } else {
                    let mut c = a.tracks[track].clips.remove(clip);
                    c.start = start.max(0.0);
                    a.tracks[new_track].clips.push(c);
                }
            }),
            ArrangeOp::ResizeClip {
                track,
                clip,
                length,
            } => self.edit_arrange(true, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    c.length = length.max(0.05);
                }
            }),
            ArrangeOp::StretchClip {
                track,
                clip,
                length,
                speed,
            } => self.edit_arrange(true, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    c.length = length.max(0.05);
                    c.speed = speed.clamp(0.1, 10.0);
                }
            }),
            ArrangeOp::SetClipOffset {
                track,
                clip,
                offset,
            } => self.edit_arrange(true, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    c.offset = offset.max(0.0);
                }
            }),
            ArrangeOp::TrimStart {
                track,
                clip,
                start,
                offset,
            } => self.edit_arrange(true, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    // Keep the right edge fixed: shrink length as the start moves in.
                    let right = c.start + c.length;
                    let new_start = start.clamp(0.0, right - 0.05);
                    c.length = right - new_start;
                    c.offset = offset.max(0.0);
                    c.start = new_start;
                }
            }),
            ArrangeOp::SplitClip { track, clip, at } => self.edit_arrange(true, |a| {
                let Some(t) = a.tracks.get_mut(track) else {
                    return;
                };
                let Some(c) = t.clips.get(clip).cloned() else {
                    return;
                };
                let local = at - c.start;
                if local <= 0.0 || local >= c.length {
                    return; // split point outside the clip
                }
                // Left keeps [0,local); the right half starts deeper into the buffer
                // by the amount of audio that played during `local` — which is
                // `local * speed` (a stretched/sped clip consumes the buffer faster
                // or slower than the timeline).
                let mut left = c.clone();
                left.length = local;
                let mut right = c.clone();
                right.start = c.start + local;
                right.length = c.length - local;
                right.offset = c.offset + local * c.speed as f64;
                t.clips[clip] = left;
                t.clips.insert(clip + 1, right);
            }),
            ArrangeOp::SetClipGain { track, clip, gain } => self.edit_arrange(false, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    c.gain = gain.clamp(0.0, 4.0);
                }
            }),
            ArrangeOp::SetClipLoop {
                track,
                clip,
                looping,
            } => self.edit_arrange(true, |a| {
                if let Some(c) = a.tracks.get_mut(track).and_then(|t| t.clips.get_mut(clip)) {
                    c.looping = looping;
                }
            }),
            ArrangeOp::Clear => self.edit_arrange(true, |a| {
                for t in &mut a.tracks {
                    t.clips.clear();
                }
            }),
            // Annotation metadata only (no audio effect) — no undo snapshot.
            ArrangeOp::SetSections { sections } => self.edit_arrange(false, |a| {
                a.sections = sections;
            }),
        }
    }

    /// The kind of a sample by id.
    pub fn sample_kind(
        &self,
        id: awsm_audio_schema::SampleId,
    ) -> Option<awsm_audio_schema::SampleKind> {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == id)
            .map(|s| s.sample.kind)
    }

    /// The display name of a sample by id.
    pub fn sample_name(&self, id: awsm_audio_schema::SampleId) -> Option<String> {
        self.samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == id)
            .map(|s| s.sample.name.clone())
    }

    /// Open the piano roll for `(node, track)`.
    pub fn open_piano_roll(&self, node: NodeId, track: usize) {
        self.piano_roll.set(Some((node, track)));
    }

    /// Whether an arrangement clip is set to loop (for the clip context menu).
    pub fn clip_looping(&self, track: usize, clip: usize) -> Option<bool> {
        self.active_arrangement().and_then(|a| {
            a.tracks
                .get(track)
                .and_then(|t| t.clips.get(clip).map(|c| c.looping))
        })
    }

    /// Copy one clip into the clip clipboard (used by the clip context menu).
    pub fn copy_clip(&self, track: usize, clip: usize) {
        self.copy_clips(&[(track, clip)]);
    }

    /// Copy a set of clips (`(track, clip)`) into the clipboard, preserving their
    /// tracks and relative timing.
    pub fn copy_clips(&self, sel: &[(usize, usize)]) {
        let Some(arr) = self.active_arrangement() else {
            return;
        };
        let mut out = Vec::new();
        for &(t, c) in sel {
            if let Some(clip) = arr.tracks.get(t).and_then(|tr| tr.clips.get(c)).cloned() {
                out.push((t, clip));
            }
        }
        if !out.is_empty() {
            *self.clip_clipboard.borrow_mut() = out;
        }
    }

    /// Whether the clip clipboard has anything to paste.
    pub fn has_clip_clipboard(&self) -> bool {
        !self.clip_clipboard.borrow().is_empty()
    }

    /// Paste at the playhead, keeping each clip's own track + relative timing
    /// (earliest lands at the playhead).
    pub fn paste_clip(&self) {
        let head = self.arrange_start_secs();
        self.paste_clips_anchored(None, head);
    }

    /// Paste at `(track, secs)` — the earliest clip lands there, others keep their
    /// relative track + time (the "Paste here" action).
    pub fn paste_clip_at(&self, track: usize, secs: f64) {
        self.paste_clips_anchored(Some(track), secs.max(0.0));
    }

    /// Shared paste: place clipboard clips with earliest at `start`. If
    /// `base_track` is set, anchor the topmost clip there (keeping relative track
    /// offsets); otherwise keep each clip's original track. One `PasteClips`
    /// command (MCP-drivable, single undo).
    fn paste_clips_anchored(&self, base_track: Option<usize>, start: f64) {
        let clips = self.clip_clipboard.borrow().clone();
        if clips.is_empty() {
            return;
        }
        let ntracks = self.active_arrangement().map_or(0, |a| a.tracks.len());
        if ntracks == 0 {
            return;
        }
        let earliest = clips
            .iter()
            .map(|(_, c)| c.start)
            .fold(f64::INFINITY, f64::min);
        let min_track = clips.iter().map(|(t, _)| *t).min().unwrap_or(0);
        let placed: Vec<command::PlacedClip> = clips
            .into_iter()
            .map(|(track, mut clip)| {
                clip.start = start + (clip.start - earliest);
                let track = match base_track {
                    Some(base) => base + (track - min_track),
                    None => track,
                };
                command::PlacedClip {
                    track: track.min(ntracks - 1),
                    clip,
                }
            })
            .collect();
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::PasteClips { clips: placed },
        });
    }

    /// The currently selected clips `(track, clip)`.
    pub fn selected_clips(&self) -> Vec<(usize, usize)> {
        self.selected_clips.get_cloned()
    }

    /// Copy the current selection to the clip clipboard.
    pub fn copy_selected_clips(&self) {
        let sel = self.selected_clips.get_cloned();
        if !sel.is_empty() {
            self.copy_clips(&sel);
        }
    }

    /// Delete the current selection (one undo) and clear it.
    pub fn delete_selected_clips(&self) {
        let sel = self.selected_clips.get_cloned();
        if sel.is_empty() {
            return;
        }
        self.selected_clips.set(Vec::new());
        self.delete_clips(&sel);
    }

    /// Delete a set of clips atomically (one undo). Removes per track in
    /// descending index order so earlier removals don't shift later ones.
    pub fn delete_clips(&self, sel: &[(usize, usize)]) {
        let mut sel: Vec<(usize, usize)> = sel.to_vec();
        sel.sort_by_key(|x| std::cmp::Reverse(x.1)); // clip index descending
        self.edit_arrange(true, |a| {
            for (t, c) in sel {
                if let Some(tr) = a.tracks.get_mut(t) {
                    if c < tr.clips.len() {
                        tr.clips.remove(c);
                    }
                }
            }
        });
    }

    /// Follow an arrangement clip to its source Sound (double-click a clip).
    pub fn open_clip_source(&self, track: usize, clip: usize) {
        if let Some(src) = self.active_arrangement().and_then(|a| {
            a.tracks
                .get(track)
                .and_then(|t| t.clips.get(clip).map(|c| c.source))
        }) {
            self.open_sample(src);
        }
    }

    /// Jump to a sample: show its view and make it active.
    pub fn open_sample(&self, id: awsm_audio_schema::SampleId) {
        let Some(kind) = self.sample_kind(id) else {
            return;
        };
        self.piano_roll.set(None);
        self.commit_active();
        self.view.set(kind);
        *self.active.borrow_mut() = id;
        self.load_sample_onto_canvas(id);
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Close the piano roll.
    pub fn close_piano_roll(&self) {
        self.piano_roll.set(None);
    }

    /// A read-only clone of the active arrangement (for the Arrange view UI).
    pub fn arrangement_view(&self) -> Option<awsm_audio_schema::Arrangement> {
        self.active_arrangement()
    }

    // ==================================================================
    // Bounce: render a Sound to an audio clip the arrangement can play.
    // ==================================================================

    /// Collect every sample transitively referenced by `id` (via `Sample(SampleRef)`
    /// nodes), in stable encounter order, deduped. Used so a bounce goes "dirty"
    /// when an *instrument it embeds* (not just its own graph) changes.
    fn collect_sample_refs(
        &self,
        id: awsm_audio_schema::SampleId,
        out: &mut Vec<awsm_audio_schema::SampleId>,
        seen: &mut std::collections::HashSet<awsm_audio_schema::SampleId>,
    ) {
        use awsm_audio_schema::NodeKind;
        let graph = {
            let samples = self.samples.borrow();
            match samples.iter().find(|s| s.sample.id == id) {
                Some(s) => s.sample.graph.clone(),
                None => return,
            }
        };
        for node in &graph.nodes {
            if let NodeKind::Sample(r) = &node.kind {
                if seen.insert(r.sample) {
                    out.push(r.sample);
                    self.collect_sample_refs(r.sample, out, seen);
                }
            }
        }
    }

    /// A deep content hash of a Sound: its own graph plus every sample it embeds
    /// (recursively). This is what's stored at bounce time and compared for the
    /// dirty check, so editing a referenced instrument flags the parent bounce.
    fn deep_source_hash(&self, id: awsm_audio_schema::SampleId) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut refs = Vec::new();
        let mut seen = std::collections::HashSet::new();
        seen.insert(id);
        self.collect_sample_refs(id, &mut refs, &mut seen);
        let samples = self.samples.borrow();
        let graph_toml = |sid: awsm_audio_schema::SampleId| -> String {
            samples
                .iter()
                .find(|s| s.sample.id == sid)
                .map(|s| toml::to_string(&s.sample.graph).unwrap_or_default())
                .unwrap_or_default()
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        graph_toml(id).hash(&mut h);
        for r in &refs {
            graph_toml(*r).hash(&mut h);
        }
        h.finish() & awsm_audio_schema::MAX_TOML_U64
    }

    /// A Sound's bounce state (never / clean / stale).
    /// The bounce status as a string for the read/query surface, enriched with the
    /// transient render state: `"rendering"` while an offline render is in flight,
    /// `"failed: <msg>"` if it crashed — otherwise the stored-bounce status
    /// (`none` / `clean` / `dirty`). Lets an agent distinguish *never bounced*,
    /// *currently rendering*, and *render crashed*.
    pub fn bounce_status_str(&self, id: awsm_audio_schema::SampleId) -> String {
        match self.render_state.borrow().get(&id) {
            Some(RenderState::Rendering) => return "rendering".to_string(),
            Some(RenderState::Failed(msg)) => return format!("failed: {msg}"),
            None => {}
        }
        self.bounce_status(id).as_str().to_string()
    }

    pub fn bounce_status(&self, id: awsm_audio_schema::SampleId) -> BounceStatus {
        let samples = self.samples.borrow();
        let Some(st) = samples.iter().find(|s| s.sample.id == id) else {
            return BounceStatus::None;
        };
        match &st.sample.bounce {
            None => BounceStatus::None,
            Some(b) if b.source_hash == self.deep_source_hash(id) => BounceStatus::Clean,
            Some(_) => BounceStatus::Dirty,
        }
    }

    /// Every bounceable Sound, with its bounce status + bounced duration — for the
    /// Assets panel. (Arrangements aren't bounceable.)
    pub fn assets_list(
        &self,
    ) -> Vec<(
        awsm_audio_schema::SampleId,
        String,
        BounceStatus,
        Option<f64>,
    )> {
        use awsm_audio_schema::SampleKind;
        let ids: Vec<(awsm_audio_schema::SampleId, String)> = self
            .samples
            .borrow()
            .iter()
            .filter(|s| s.sample.kind == SampleKind::Sound)
            .map(|s| (s.sample.id, s.sample.name.clone()))
            .collect();
        ids.into_iter()
            .map(|(id, name)| (id, name, self.bounce_status(id), self.bounce_duration(id)))
            .collect()
    }

    /// Build the offline-render job for a Sound — the shared setup behind both
    /// [`bounce_sample`](Self::bounce_sample) and
    /// [`export_active_wav`](Self::export_active_wav). Returns the job + the
    /// sample's display name, or `None` (setting a status message) when it can't
    /// render (an Arrangement, a live mic/stream source, or no player).
    /// Extra render time past the loop so note release tails fold back onto the
    /// loop start (sequencer-driven Sounds).
    const RELEASE_TAIL: f64 = 3.0;
    /// Default render window for a continuous / one-shot (non-sequencer) Sound,
    /// when no `duration_secs` override is given.
    const DEFAULT_GRAPH_SECS: f64 = 6.0;
    /// Floor on any bounce length.
    const MIN_BOUNCE_SECS: f64 = 0.25;

    /// What an un-overridden [`bounce`](Self::bounce_sample) would render for Sound
    /// `id`, and why — the queryable form of [`bounce_job_for`]'s duration logic
    /// (kept in lockstep with it). Powers `get_render_plan` so the auto-duration
    /// rules don't have to be reverse-engineered.
    pub fn render_plan(
        &self,
        sample: Option<awsm_audio_schema::SampleId>,
    ) -> awsm_audio_editor_protocol::RenderPlanInfo {
        use awsm_audio_editor_protocol::RenderPlanInfo;
        use awsm_audio_schema::{NodeKind, SampleKind};
        let plain = |duration_secs: f64, reason: &str| RenderPlanInfo {
            duration_secs,
            is_sequence: false,
            loop_secs: None,
            reason: reason.to_string(),
        };
        self.commit_active();
        let lib = self.to_library();
        let id = sample.unwrap_or(*self.root.borrow());
        let Some(sample) = lib.sample(id) else {
            return plain(0.0, "no such sample");
        };
        if sample.kind == SampleKind::Arrangement {
            return plain(
                0.0,
                "arrangement: renders its clip timeline (or the loop/export markers), not a fixed window",
            );
        }
        let has_live_source = sample.graph.nodes.iter().any(|n| {
            matches!(
                n.kind,
                NodeKind::MediaStreamSource(_) | NodeKind::MediaElementSource(_)
            )
        });
        if has_live_source {
            return plain(
                0.0,
                "not renderable offline: has a live mic / stream source",
            );
        }
        if awsm_audio_player::document::is_sequence(&sample.graph) {
            let sp = awsm_audio_player::document::sequence_parts(&lib, id);
            // Output-terminated but nothing sequenced (e.g. a buffer source /
            // oscillator straight into an Output): that's a continuous one-shot,
            // not a zero-length loop — mirror the play path's guard so the plan
            // never reports a bogus 0.05s "loop".
            let has_parts =
                !(sp.triggers.is_empty() && sp.control.is_empty()) && sp.loop_secs > 0.0;
            if has_parts {
                let loop_len = sp.loop_secs.max(0.05);
                let duration = (loop_len + Self::RELEASE_TAIL).max(Self::MIN_BOUNCE_SECS);
                RenderPlanInfo {
                    duration_secs: duration,
                    is_sequence: true,
                    loop_secs: Some(loop_len),
                    reason: format!(
                        "sequencer-driven: renders the {loop_len:.3}s song loop + a \
                         {:.1}s release tail, then STORES exactly the {loop_len:.3}s \
                         loop (tails fold onto the loop start so clips tile \
                         seamlessly). Pass duration_secs to instead render and store \
                         an exact unfolded span.",
                        Self::RELEASE_TAIL
                    ),
                }
            } else {
                plain(
                    Self::DEFAULT_GRAPH_SECS.max(Self::MIN_BOUNCE_SECS),
                    &format!(
                        "continuous / one-shot graph (an Output-terminated graph with \
                         no sequenced notes or control lanes): renders a fixed {:.1}s \
                         default window. Pass duration_secs to capture a specific span.",
                        Self::DEFAULT_GRAPH_SECS
                    ),
                )
            }
        } else {
            RenderPlanInfo {
                duration_secs: Self::DEFAULT_GRAPH_SECS.max(Self::MIN_BOUNCE_SECS),
                is_sequence: false,
                loop_secs: None,
                reason: format!(
                    "continuous / one-shot graph (no sequencer wired into the audible \
                     path): renders a fixed {:.1}s default window. Pass duration_secs \
                     to capture a specific span.",
                    Self::DEFAULT_GRAPH_SECS
                ),
            }
        }
    }

    fn bounce_job_for(
        &self,
        id: awsm_audio_schema::SampleId,
        duration_override: Option<f64>,
    ) -> Option<(awsm_audio_player::bounce::BounceJob, String)> {
        use awsm_audio_schema::{NodeKind, SampleKind};
        self.commit_active();
        let lib = self.to_library();
        let sample = lib.sample(id).cloned()?;
        if sample.kind == SampleKind::Arrangement {
            return None;
        }
        // Mic/stream sounds can't render offline.
        let has_live_source = sample.graph.nodes.iter().any(|n| {
            matches!(
                n.kind,
                NodeKind::MediaStreamSource(_) | NodeKind::MediaElementSource(_)
            )
        });
        if has_live_source {
            self.status.set(Some(
                "Can't render a Sound with a live mic / stream source.".into(),
            ));
            return None;
        }
        if !self.ensure_player() {
            return None;
        }
        // Compile the Sound exactly as it would play. A sequence renders as an
        // exact loop region (so a looping clip repeats bit-for-bit); the render
        // runs `RELEASE_TAIL` longer so note tails can be folded back onto the
        // loop start. A non-sequence Sound renders as a one-shot.
        let song = awsm_audio_player::document::is_sequence(&sample.graph);
        let (graph, parts, control, duration, loop_secs) = if song {
            let sp = awsm_audio_player::document::sequence_parts(&lib, id);
            // Output-terminated but nothing sequenced (e.g. a loaded buffer
            // source straight into an Output): a continuous one-shot, NOT a
            // zero-length loop — without this guard the fold truncated such a
            // bounce to 0.05s regardless of the source's real length. Mirrors
            // the play path's empty-parts check.
            let has_parts =
                !(sp.triggers.is_empty() && sp.control.is_empty()) && sp.loop_secs > 0.0;
            if has_parts {
                let loop_len = sp.loop_secs.max(0.05);
                (
                    sp.graph,
                    sp.triggers,
                    sp.control,
                    loop_len + Self::RELEASE_TAIL,
                    Some(loop_len),
                )
            } else {
                (
                    sp.graph,
                    Vec::new(),
                    Vec::new(),
                    Self::DEFAULT_GRAPH_SECS,
                    None,
                )
            }
        } else {
            (
                awsm_audio_schema::flatten(&lib, id),
                Vec::new(),
                Vec::new(),
                Self::DEFAULT_GRAPH_SECS,
                None,
            )
        };
        // An explicit override wins (e.g. capture a fixed span of a procedural /
        // worklet source that otherwise renders only a tiny default) — and it is
        // literal: the stored bounce is exactly that span, with no loop-fold
        // truncation back to the song loop.
        let (duration, loop_secs) = match duration_override {
            Some(d) => (d.max(0.05), None),
            None => (duration, loop_secs),
        };
        let job = self.player.borrow().as_ref()?.bounce_job(
            graph,
            parts,
            control,
            duration.max(Self::MIN_BOUNCE_SECS),
            loop_secs,
        );
        Some((job, sample.name.clone()))
    }

    /// Render a Sound (or the project root when `None`) offline to PCM, without
    /// storing it — the shared entry point for the MCP WAV readbacks
    /// (`RenderWav`/`WavStats`/`Waveform`). Reuses [`bounce_job_for`] +
    /// [`awsm_audio_player::bounce::render`], the same path Bounce/export use; an
    /// optional `sample_rate` overrides the bounce rate.
    /// PCM for a stats / waveform readback. `bounced = false` renders the **live
    /// graph** fresh (what the sound is right now — the sound-design view), at the
    /// `duration_secs` window or the auto length. `bounced = true` returns the
    /// **stored bounced asset's** PCM (what plays in an arrangement), or an
    /// explicit "not yet bounced" error if there is none — the caller chooses,
    /// so there's no stale-vs-fresh guessing.
    pub async fn readback_pcm(
        &self,
        sample: Option<awsm_audio_schema::SampleId>,
        bounced: bool,
        duration_secs: Option<f64>,
    ) -> Result<(Vec<Vec<f32>>, u32), String> {
        if bounced {
            let id = sample.unwrap_or_else(|| *self.root.borrow());
            return self.stored_bounce_pcm(id).ok_or_else(|| {
                "not yet bounced — call bounce first, or use bounced=false to \
                 measure the live graph"
                    .to_string()
            });
        }
        self.render_pcm(sample, None, duration_secs).await
    }

    /// The decoded PCM of a Sound's stored bounce, if it has one backed by inline
    /// PCM (the normal case for a bounce). `None` for an un-bounced sample or a
    /// non-PCM asset source.
    fn stored_bounce_pcm(&self, id: awsm_audio_schema::SampleId) -> Option<(Vec<Vec<f32>>, u32)> {
        let asset_id = {
            let samples = self.samples.borrow();
            samples
                .iter()
                .find(|s| s.sample.id == id)?
                .sample
                .bounce
                .as_ref()?
                .asset
        };
        let assets = self.buffer_assets.borrow();
        match &assets.get(&asset_id)?.source {
            awsm_audio_schema::AudioSource::Pcm {
                sample_rate,
                channels,
            } => Some((channels.clone(), *sample_rate as u32)),
            _ => None,
        }
    }

    pub async fn render_pcm(
        &self,
        sample: Option<awsm_audio_schema::SampleId>,
        sample_rate: Option<f32>,
        duration_secs: Option<f64>,
    ) -> Result<(Vec<Vec<f32>>, u32), String> {
        let id = sample.unwrap_or_else(|| *self.root.borrow());
        // Arrangements render through their clip timeline, not the bounce graph.
        if self.arrangement_for(id).is_some() {
            return self.render_arrangement_pcm(id).await;
        }
        let (mut job, _label) = self
            .bounce_job_for(id, duration_secs)
            .ok_or_else(|| "nothing to render".to_string())?;
        if let Some(sr) = sample_rate {
            job.sample_rate = sr;
        }
        awsm_audio_player::bounce::render(job)
            .await
            .map_err(|e| format!("{e}"))
    }

    /// Offline-render the Arrangement sample `id` over its effective window (the
    /// loop/export markers if set, else the whole timeline) to PCM — the analog of
    /// [`render_pcm`](Self::render_pcm) for arrangements. Clips must already be
    /// bounced (their PCM lives in the player); unbounced clips are skipped.
    pub async fn render_arrangement_pcm(
        &self,
        id: awsm_audio_schema::SampleId,
    ) -> Result<(Vec<Vec<f32>>, u32), String> {
        let arr = self
            .arrangement_for(id)
            .ok_or_else(|| "not an arrangement".to_string())?;
        let (start, end) = arr.range();
        let duration = (end - start).max(0.05);
        let clips = awsm_audio_player::document::audio_clip_parts(&self.to_library(), id, start);
        if clips.is_empty() {
            return Err("nothing to render (bounce a Sound and drop it on a track)".into());
        }
        if !self.ensure_player() {
            return Err("audio player unavailable".into());
        }
        let (buffers, sr) = {
            let p = self.player.borrow();
            let p = p
                .as_ref()
                .ok_or_else(|| "audio player unavailable".to_string())?;
            (p.clip_buffers(), p.sample_rate() as f32)
        };
        awsm_audio_player::bounce::render_clips(clips, buffers, sr, duration)
            .await
            .map_err(|e| format!("{e}"))
    }

    /// Per-track peak/rms of the active arrangement, each track rendered in
    /// isolation (soloed) over the effective window — the per-stem mix readback so
    /// an agent can spot the hot track instead of rescaling everything. Skips empty
    /// tracks (peak/rms 0). Async (one offline render per track).
    pub async fn arrangement_track_stats(
        &self,
    ) -> Result<Vec<awsm_audio_editor_protocol::TrackStats>, String> {
        use awsm_audio_editor_protocol::{TrackStats, WavStats};
        self.commit_active();
        let id = *self.active.borrow();
        let arr = self
            .arrangement_for(id)
            .ok_or_else(|| "active sample is not an arrangement".to_string())?;
        let (start, end) = arr.range();
        let duration = (end - start).max(0.05);
        if !self.ensure_player() {
            return Err("audio player unavailable".into());
        }
        let (buffers, sr) = {
            let p = self.player.borrow();
            let p = p
                .as_ref()
                .ok_or_else(|| "audio player unavailable".to_string())?;
            (p.clip_buffers(), p.sample_rate() as f32)
        };
        let base_lib = self.to_library();
        let mut out = Vec::with_capacity(arr.tracks.len());
        for (t, track) in arr.tracks.iter().enumerate() {
            let (peak, rms) = if track.clips.is_empty() {
                (0.0, 0.0)
            } else {
                // Clone the document and make track `t` the only audible one
                // (solo it, clear others, force-unmute it) so audio_clip_parts —
                // which honors mute/solo — yields just this stem.
                let mut lib = base_lib.clone();
                if let Some(a) = lib.sample_mut(id).map(|s| &mut s.arrangement) {
                    for (i, tr) in a.tracks.iter_mut().enumerate() {
                        tr.solo = i == t;
                        if i == t {
                            tr.mute = false;
                        }
                    }
                }
                let clips = awsm_audio_player::document::audio_clip_parts(&lib, id, start);
                if clips.is_empty() {
                    (0.0, 0.0)
                } else {
                    let (channels, rate) = awsm_audio_player::bounce::render_clips(
                        clips,
                        buffers.clone(),
                        sr,
                        duration,
                    )
                    .await
                    .map_err(|e| format!("{e}"))?;
                    let s = WavStats::from_pcm(&channels, rate);
                    (s.peak, s.rms)
                }
            };
            out.push(TrackStats {
                track: t,
                name: track.name.clone(),
                peak,
                rms,
                clips: track.clips.len(),
            });
        }
        Ok(out)
    }

    /// Peak/rms/readback stats for arrangement time ranges. If `sections` is
    /// empty, use the arrangement's saved sections; if those are empty too, use
    /// the effective export/playback range.
    pub async fn arrangement_section_stats(
        &self,
        sections: Vec<awsm_audio_schema::ArrSection>,
    ) -> Result<Vec<awsm_audio_editor_protocol::SectionStats>, String> {
        use awsm_audio_editor_protocol::{SectionStats, WavStats};
        self.commit_active();
        let id = *self.active.borrow();
        let arr = self
            .arrangement_for(id)
            .ok_or_else(|| "active sample is not an arrangement".to_string())?;
        let ranges = if !sections.is_empty() {
            sections
        } else if !arr.sections.is_empty() {
            arr.sections.clone()
        } else {
            let (start, end) = arr.range();
            vec![awsm_audio_schema::ArrSection {
                name: "range".into(),
                start,
                end,
            }]
        };
        if !self.ensure_player() {
            return Err("audio player unavailable".into());
        }
        let (buffers, sr) = {
            let p = self.player.borrow();
            let p = p
                .as_ref()
                .ok_or_else(|| "audio player unavailable".to_string())?;
            (p.clip_buffers(), p.sample_rate() as f32)
        };
        let lib = self.to_library();
        let mut out = Vec::with_capacity(ranges.len());
        for section in ranges {
            let start = section.start.clamp(0.0, arr.length_secs);
            let end = section.end.clamp(0.0, arr.length_secs);
            if end <= start {
                continue;
            }
            let clips = awsm_audio_player::document::audio_clip_parts(&lib, id, start);
            let duration = (end - start).max(0.05);
            let stats = if clips.is_empty() {
                WavStats::from_pcm(&[], sr as u32)
            } else {
                let (channels, rate) =
                    awsm_audio_player::bounce::render_clips(clips, buffers.clone(), sr, duration)
                        .await
                        .map_err(|e| format!("{e}"))?;
                WavStats::from_pcm(&channels, rate)
            };
            out.push(SectionStats {
                name: section.name,
                start,
                end,
                peak: stats.peak,
                rms: stats.rms,
                clipping: stats.clipping,
                spectral_centroid_hz: stats.spectral_centroid_hz,
                brightness: stats.brightness,
            });
        }
        Ok(out)
    }

    /// Render a Sound to an audio buffer (offline) and store it as the sample's
    /// bounce — so arrangement clips can play + draw it. Async; bumps `samples_rev`
    /// when done. No-op for an Arrangement sample.
    pub fn bounce_sample(&self, id: awsm_audio_schema::SampleId, duration_secs: Option<f64>) {
        use awsm_audio_schema::{AudioSource, BufferAsset};
        let Some((job, name)) = self.bounce_job_for(id, duration_secs) else {
            return;
        };
        let hash = self.deep_source_hash(id);
        let ctrl = self.clone();
        self.status.set(Some(format!("Bouncing “{name}”…")));
        self.render_state
            .borrow_mut()
            .insert(id, RenderState::Rendering);
        wasm_bindgen_futures::spawn_local(async move {
            match awsm_audio_player::bounce::render(job).await {
                Ok((channels, sr)) => {
                    ctrl.render_state.borrow_mut().remove(&id);
                    let asset_id = awsm_audio_schema::AssetId::new();
                    let asset = BufferAsset {
                        id: asset_id,
                        label: Some(format!("{name} (bounce)")),
                        source: AudioSource::Pcm {
                            sample_rate: sr as f32,
                            channels: channels.clone(),
                        },
                    };
                    ctrl.buffer_assets.borrow_mut().insert(asset_id, asset);
                    if let Some(p) = ctrl.player.borrow_mut().as_mut() {
                        let _ = p.store_pcm(asset_id, sr as f32, &channels);
                    }
                    // Point the sample at the new bounce + remember its source hash.
                    if let Some(s) = ctrl
                        .samples
                        .borrow_mut()
                        .iter_mut()
                        .find(|s| s.sample.id == id)
                    {
                        s.sample.bounce = Some(awsm_audio_schema::Bounce {
                            asset: asset_id,
                            source_hash: hash,
                        });
                    }
                    ctrl.status.set(None);
                    ctrl.samples_rev.replace_with(|r| r.wrapping_add(1));
                }
                Err(e) => {
                    tracing::error!("bounce failed: {e}");
                    ctrl.render_state
                        .borrow_mut()
                        .insert(id, RenderState::Failed(e.to_string()));
                    ctrl.status.set(Some(format!("Bounce failed: {e}")));
                    ctrl.samples_rev.replace_with(|r| r.wrapping_add(1));
                }
            }
        });
    }

    /// Offline-render the **active** sample and download it as a `.wav`. For a
    /// Sound this is the Bounce render path; for an Arrangement it renders the
    /// effective window (the loop/export markers if set, else start-to-finish).
    /// The same offline render in both cases — no real-time capture.
    pub fn export_active_wav(&self) {
        let id = *self.active.borrow();
        let is_arrangement = self.samples.borrow().iter().any(|s| {
            s.sample.id == id && s.sample.kind == awsm_audio_schema::SampleKind::Arrangement
        });
        let name = self
            .samples
            .borrow()
            .iter()
            .find(|s| s.sample.id == id)
            .map(|s| s.sample.name.clone())
            .unwrap_or_else(|| "export".into());

        let ctrl = self.clone();
        self.status.set(Some(format!("Exporting “{name}”…")));
        wasm_bindgen_futures::spawn_local(async move {
            let rendered = if is_arrangement {
                ctrl.render_arrangement_pcm(id).await
            } else {
                ctrl.render_pcm(Some(id), None, None).await
            };
            match rendered {
                Ok((channels, sr)) => {
                    let wav = crate::util::encode_wav(&channels, sr);
                    let filename = format!("{}.wav", sanitize_filename(&name));
                    if let Err(e) = crate::util::download_bytes(&filename, &wav, "audio/wav") {
                        tracing::error!("download wav: {e:?}");
                        ctrl.status.set(Some("Export failed to download.".into()));
                    } else {
                        ctrl.status.set(None);
                    }
                }
                Err(e) => {
                    tracing::error!("export failed: {e}");
                    ctrl.status.set(Some(format!("Export failed: {e}")));
                }
            }
        });
    }

    /// Downsampled `(min, max)` peaks of a Sound's bounced buffer (for drawing a
    /// clip waveform). `n` is the number of peak columns wanted.
    pub fn bounce_peaks(&self, source: awsm_audio_schema::SampleId, n: usize) -> Vec<(f32, f32)> {
        use awsm_audio_schema::AudioSource;
        let Some(asset) = self.bounce_asset(source) else {
            return Vec::new();
        };
        let bufs = self.buffer_assets.borrow();
        let Some(ba) = bufs.get(&asset) else {
            return Vec::new();
        };
        let AudioSource::Pcm { channels, .. } = &ba.source else {
            return Vec::new();
        };
        let Some(data) = channels.first() else {
            return Vec::new();
        };
        let n = n.max(1);
        let len = data.len();
        if len == 0 {
            return Vec::new();
        }
        let step = (len as f64 / n as f64).max(1.0);
        (0..n)
            .map(|i| {
                let a = (i as f64 * step) as usize;
                let b = (((i + 1) as f64 * step) as usize).min(len);
                let mut lo = 0.0f32;
                let mut hi = 0.0f32;
                for &s in &data[a..b.max(a)] {
                    lo = lo.min(s);
                    hi = hi.max(s);
                }
                (lo, hi)
            })
            .collect()
    }

    /// Peaks over a *window* `[start, start+len]` seconds of a Sound's bounce —
    /// so a trimmed/offset clip draws only the part of the buffer it actually
    /// plays (not the whole thing stretched). Out-of-range samples read as 0.
    pub fn bounce_peaks_window(
        &self,
        source: awsm_audio_schema::SampleId,
        start: f64,
        len: f64,
        n: usize,
    ) -> Vec<(f32, f32)> {
        use awsm_audio_schema::AudioSource;
        let Some(asset) = self.bounce_asset(source) else {
            return Vec::new();
        };
        let bufs = self.buffer_assets.borrow();
        let Some(ba) = bufs.get(&asset) else {
            return Vec::new();
        };
        let AudioSource::Pcm {
            channels,
            sample_rate,
        } = &ba.source
        else {
            return Vec::new();
        };
        let Some(data) = channels.first() else {
            return Vec::new();
        };
        let sr = (*sample_rate as f64).max(1.0);
        let total = data.len();
        if total == 0 || len <= 0.0 {
            return Vec::new();
        }
        let n = n.max(1);
        let start_s = start * sr;
        let span_s = len * sr;
        let step = (span_s / n as f64).max(1.0);
        (0..n)
            .map(|i| {
                let a = (start_s + i as f64 * step) as isize;
                let b = (start_s + (i + 1) as f64 * step) as isize;
                let mut lo = 0.0f32;
                let mut hi = 0.0f32;
                let mut k = a.max(0);
                while k < b && (k as usize) < total {
                    let s = data[k as usize];
                    lo = lo.min(s);
                    hi = hi.max(s);
                    k += 1;
                }
                (lo, hi)
            })
            .collect()
    }

    /// The live-tweakable param targets of an input (inlet), as `(node, param)`
    /// pairs — `Some` only if *every* wire leaving the inlet drives a param on a
    /// non-Sample node (so its id is stable in the played graph and the player
    /// holds its AudioParam). `None` if any wire feeds an audio input or a
    /// Sample-ref (whose inner ids are remapped per flatten) — those need a
    /// rebuild. An input wired to nothing yields `Some(empty)` (a no-op sweep).
    fn live_param_targets(&self, inlet: NodeId) -> Option<Vec<(NodeId, String)>> {
        let mut targets = Vec::new();
        for c in self
            .connections
            .lock_ref()
            .iter()
            .filter(|c| c.from.id == inlet)
        {
            match &c.sink {
                ConnSink::Param(p) => {
                    if matches!(&*c.to.kind.borrow(), awsm_audio_schema::NodeKind::Sample(_)) {
                        return None;
                    }
                    targets.push((c.to.id, p.0.clone()));
                }
                ConnSink::Input(_) => return None,
                ConnSink::Trigger => return None,
            }
        }
        Some(targets)
    }

    /// Glide a live AudioParam of the playing graph to `value` (~20 ms), if the
    /// engine is running. The MIDI CC live-sweep path (see [`handle_midi_cc`]).
    pub fn set_param_live(&self, node: NodeId, param: &str, value: f32) {
        if let Some(p) = self.player.borrow().as_ref() {
            p.set_param_live(node, param, value, 0.02);
        }
    }

    /// Stop playback (keeps the engine + master chain alive).
    pub fn stop(&self) {
        self.cancel_song_loop();
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.stop();
        }
        self.playing.set_neq(false);
        self.paused.set_neq(false);
        self.status.set(None);
        // Return to where this play session began (the play origin).
        let origin = self.play_origin.get();
        self.arrange_start.set(origin);
        self.arrange_playhead.set(origin);
    }

    /// Pause: stop the audio but hold position so Play resumes from here. (For
    /// arrangements this is sample-accurate; a Sound/Sequence resumes from its
    /// start since there's no scrub point to seek to.)
    pub fn pause(&self) {
        if !self.playing.get() {
            return;
        }
        // Capture the live arrangement position before tearing down.
        let head = self
            .arrangement_playhead_secs()
            .unwrap_or_else(|| self.arrange_start.get())
            .max(0.0);
        self.cancel_song_loop();
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.stop();
        }
        self.playing.set_neq(false);
        self.paused.set_neq(true);
        self.status.set(None);
        self.arrange_start.set(head);
        self.arrange_playhead.set(head);
    }

    /// Play/pause toggle — Spacebar + the main transport button.
    pub fn toggle_play_pause(&self) {
        if self.playing.get() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Set looping; if currently playing, rebuild so it takes effect now.
    pub fn set_looping(&self, on: bool) {
        self.looping.set_neq(on);
        if self.playing.get() {
            self.play();
        }
    }

    /// Current output peak level (0..1) — a reliable "is it making sound" probe.
    pub fn audio_peak(&self) -> f32 {
        self.player
            .borrow()
            .as_ref()
            .map(|p| p.peak())
            .unwrap_or(0.0)
    }

    /// Time-domain scope samples (0..255) of an Analyser node in the live graph,
    /// for its per-node oscilloscope. Empty when not playing / not an Analyser.
    pub fn analyser_scope(&self, node: NodeId) -> Vec<u8> {
        self.player
            .borrow()
            .as_ref()
            .map(|p| p.scope(node))
            .unwrap_or_default()
    }

    /// The audio context state (`"none"` if the engine isn't created yet).
    pub fn audio_state(&self) -> String {
        self.player
            .borrow()
            .as_ref()
            .map(|p| p.context_state())
            .unwrap_or_else(|| "none".to_string())
    }

    /// Copy the latest waveform samples into `buf`; returns false if nothing is
    /// sounding (neither the transport nor any held/ringing note voice).
    pub fn read_waveform(&self, buf: &mut [u8]) -> bool {
        match self.player.borrow().as_ref() {
            Some(p) if self.playing.get() || p.voice_count() > 0 => {
                p.read_waveform(buf);
                true
            }
            _ => false,
        }
    }

    /// The latest time-domain waveform (0..=255, 128 = silence), or empty if the
    /// engine isn't running. An external-inspection seam (waveform visualizer /
    /// MCP probe).
    pub fn audio_waveform(&self) -> Vec<u8> {
        match self.player.borrow().as_ref() {
            Some(p) => {
                let mut buf = vec![128u8; p.waveform_len()];
                p.read_waveform(&mut buf);
                buf
            }
            None => Vec::new(),
        }
    }

    // ==================================================================
    // Envelope breakpoint dragging (inspector plot). Live edits preview via
    // `env_drag`; only the release commits a `SetAutomation` (one undo step).
    // ==================================================================

    /// Begin dragging breakpoint `index` of `node`'s `key` param.
    pub fn begin_env_drag(&self, node: NodeId, key: &str, index: usize) {
        let Some(n) = self.node_by_id(node) else {
            return;
        };
        let events = crate::fields::audio_params(&n.kind.borrow())
            .into_iter()
            .find(|p| p.key == key)
            .map(|p| p.automation)
            .unwrap_or_default();
        if index < events.len() {
            self.env_drag.set(Some(EnvDrag {
                node,
                key: key.to_string(),
                index,
                events,
            }));
        }
    }

    /// Update the dragged breakpoint's value+time (preview only).
    pub fn update_env_drag(&self, value: f32, time: f64) {
        if let Some(mut d) = self.env_drag.get_cloned() {
            if let Some(ev) = d.events.get(d.index) {
                let updated = crate::fields::set_event_vt(ev, value, time.max(0.0));
                d.events[d.index] = updated;
                self.env_drag.set(Some(d));
            }
        }
    }

    /// Commit the dragged envelope (sorted by time) as one `SetAutomation`.
    pub fn commit_env_drag(&self) {
        if let Some(mut d) = self.env_drag.get_cloned() {
            d.events.sort_by(|a, b| {
                crate::fields::event_time(a)
                    .partial_cmp(&crate::fields::event_time(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            self.env_drag.set(None);
            self.dispatch(EditorCommand::SetAutomation {
                id: d.node,
                param: d.key,
                events: d.events,
            });
        }
    }

    /// Show / hide the node-type help modal.
    pub fn show_help(&self, doc: crate::catalog::NodeDoc) {
        self.help.set(Some(doc));
    }
    pub fn close_help(&self) {
        self.help.set(None);
    }

    /// Open / close the "Load example" browser modal.
    pub fn open_examples(&self) {
        self.examples_open.set(true);
    }
    pub fn close_examples(&self) {
        self.examples_open.set(false);
    }

    /// Open / close the sample picker modal (the scalable replacement for the
    /// inline tab strip — a filterable list of the current view's samples).
    pub fn open_sample_picker(&self) {
        self.sample_picker_open.set(true);
    }
    pub fn close_sample_picker(&self) {
        self.sample_picker_open.set(false);
    }

    /// Open / close the page Help / onboarding modal.
    pub fn open_help(&self) {
        self.open_help_at(0);
    }
    /// Open the Help modal showing tab `tab` (the help modal reads `help_tab` to
    /// pick its initial tab — e.g. jump straight to the MCP section).
    pub fn open_help_at(&self, tab: usize) {
        self.help_tab.set(tab);
        self.help_open.set(true);
    }
    pub fn close_help_page(&self) {
        self.help_open.set(false);
    }

    /// Load an editor-served example by name and close the browser.
    pub fn load_example(&self, name: &str) {
        if let Some(base) = example_project_base(name) {
            let ctrl = self.clone();
            let base = base.to_string();
            self.examples_open.set(false);
            wasm_bindgen_futures::spawn_local(async move {
                let url = format!("{}/project.toml", base.trim_end_matches('/'));
                match fetch_text(&url).await {
                    Ok(toml) => {
                        match toml::from_str::<crate::controller::snapshot::EditorProject>(&toml) {
                            Ok(project) => ctrl.open_project_with_asset_base(project, Some(base)),
                            Err(e) => ctrl
                                .status
                                .set(Some(format!("Example project parse failed: {e}"))),
                        }
                    }
                    Err(e) => ctrl
                        .status
                        .set(Some(format!("Example project fetch failed: {e:?}"))),
                }
            });
        } else if let Some(url) = example_library_url(name) {
            let ctrl = self.clone();
            let asset_base = example_asset_base();
            self.examples_open.set(false);
            wasm_bindgen_futures::spawn_local(async move {
                match fetch_text(url).await {
                    Ok(toml) => match toml::from_str::<awsm_audio_schema::SampleLibrary>(&toml) {
                        Ok(lib) => {
                            *ctrl.asset_base_path.borrow_mut() = Some(asset_base.to_string());
                            ctrl.load_library_inner(lib, None);
                        }
                        Err(e) => ctrl
                            .status
                            .set(Some(format!("Example library parse failed: {e}"))),
                    },
                    Err(e) => ctrl
                        .status
                        .set(Some(format!("Example library fetch failed: {e:?}"))),
                }
            });
        } else {
            self.examples_open.set(false);
            self.status.set(Some(format!("Unknown example: {name}")));
        }
    }

    /// Replace the canvas with the root sample of `lib`, auto-laying-out its
    /// nodes (the schema carries no positions). Used by the Examples menu and
    /// the `editor_load_toml` seam. Clears the undo history (a fresh document).
    pub fn load_library(&self, lib: awsm_audio_schema::SampleLibrary) {
        *self.asset_base_path.borrow_mut() = None;
        self.load_library_inner(lib, None);
    }

    /// Open a full editor project (library + saved layout + camera), restoring
    /// node positions and view exactly (Open of a saved `.toml`).
    pub fn open_project(&self, project: crate::controller::snapshot::EditorProject) {
        self.open_project_with_asset_base(project, None);
    }

    fn open_project_with_asset_base(
        &self,
        project: crate::controller::snapshot::EditorProject,
        asset_base_path: Option<String>,
    ) {
        *self.asset_base_path.borrow_mut() = asset_base_path;
        let view = (project.layout, project.pan_x, project.pan_y, project.zoom);
        self.load_library_inner(project.library, Some(view));
    }

    /// Shared load path. `view` = saved (layout, pan_x, pan_y, zoom) to restore
    /// exactly; `None` auto-lays-out and resets the camera (examples / bare libs).
    fn load_library_inner(
        &self,
        lib: awsm_audio_schema::SampleLibrary,
        view: Option<(Vec<snapshot::NodeLayout>, f64, f64, f64)>,
    ) {
        self.stop();
        if lib.samples.is_empty() {
            return;
        }
        // Fresh document: reset the arrangement scrub position.
        self.arrange_start.set(0.0);
        self.arrange_playhead.set(-1.0);
        let root_id = lib
            .root
            .filter(|r| lib.sample(*r).is_some())
            .unwrap_or(lib.samples[0].id);
        // Flat layout (across all samples) → per-node positions.
        let pos: std::collections::HashMap<NodeId, (f64, f64)> = view
            .as_ref()
            .map(|(layout, ..)| layout.iter().map(|l| (l.id, (l.x, l.y))).collect())
            .unwrap_or_default();
        // Build the sample store, distributing layout per sample (auto-laying-out
        // any node without a saved position).
        *self.samples.borrow_mut() = lib
            .samples
            .iter()
            .map(|s| {
                let fallback = layout::auto_layout(&s.graph);
                let layout = s
                    .graph
                    .nodes
                    .iter()
                    .map(|n| {
                        let p = pos
                            .get(&n.id)
                            .copied()
                            .or_else(|| fallback.get(&n.id).copied())
                            .unwrap_or((80.0, 80.0));
                        (n.id, p)
                    })
                    .collect();
                StoredSample {
                    sample: s.clone(),
                    layout,
                }
            })
            .collect();
        *self.root.borrow_mut() = root_id;
        *self.active.borrow_mut() = root_id;
        // Show the view the loaded root belongs to.
        self.view.set(
            self.samples
                .borrow()
                .iter()
                .find(|s| s.sample.id == root_id)
                .map(|s| s.sample.kind)
                .unwrap_or_default(),
        );
        self.status.set(None);
        self.load_sample_onto_canvas(root_id);
        match &view {
            Some((_, pan_x, pan_y, zoom)) => {
                self.pan.set((*pan_x, *pan_y));
                self.zoom.set(*zoom);
            }
            None => {
                self.pan.set((0.0, 0.0));
                self.zoom.set(1.0);
            }
        }
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
        let sample = lib
            .sample(root_id)
            .cloned()
            .unwrap_or(lib.samples[0].clone());
        self.undo_stack.borrow_mut().clear();
        self.redo_stack.borrow_mut().clear();
        self.refresh_undo_flags();
        // Restore the spatial listener (default if absent) + sync the player.
        *self.listener.borrow_mut() = lib.listener.clone().unwrap_or_default();
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.set_listener(Some(self.listener.borrow().clone()));
        }
        // Warn about any node referencing an asset that isn't embedded or
        // already loaded this session (it'll play silently/identity).
        self.warn_missing_assets(&sample.graph, &lib.assets);
        // Load embedded assets (async compile/decode).
        self.load_wasm_assets(lib.assets.wasm_modules.clone());
        self.load_buffer_assets(lib.assets.buffers.clone());
    }

    /// Merge another library's samples + embedded assets into the open project —
    /// the `Request::ImportSamples` handler (per-sample patch import; the whole-
    /// document counterpart is [`open_project`](Self::open_project)). Samples
    /// whose ids already exist are rejected up front (no partial import); assets
    /// dedupe by id inside the loaders. Returns the imported `(id, name, kind)`s.
    pub fn import_library(
        &self,
        lib: awsm_audio_schema::SampleLibrary,
    ) -> Result<
        Vec<(
            awsm_audio_schema::SampleId,
            String,
            awsm_audio_schema::SampleKind,
        )>,
        String,
    > {
        if lib.samples.is_empty() {
            return Err("library has no samples".to_string());
        }
        {
            let samples = self.samples.borrow();
            for s in &lib.samples {
                if samples.iter().any(|st| st.sample.id == s.id) {
                    return Err(format!(
                        "sample {} (\"{}\") already exists in this project — \
                         import once, then fork copies with duplicate_sample",
                        s.id, s.name
                    ));
                }
            }
        }
        let mut imported = Vec::new();
        {
            let mut samples = self.samples.borrow_mut();
            for s in &lib.samples {
                let fallback = layout::auto_layout(&s.graph);
                let layout = s
                    .graph
                    .nodes
                    .iter()
                    .map(|n| (n.id, fallback.get(&n.id).copied().unwrap_or((80.0, 80.0))))
                    .collect();
                imported.push((s.id, s.name.clone(), s.kind));
                samples.push(StoredSample {
                    sample: s.clone(),
                    layout,
                });
            }
        }
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
        // Embedded assets: compile/decode async, same as project open.
        self.load_wasm_assets(lib.assets.wasm_modules.clone());
        self.load_buffer_assets(lib.assets.buffers.clone());
        Ok(imported)
    }

    /// Log a warning for each node that references an asset which is neither
    /// embedded in `assets` nor already loaded this session.
    fn warn_missing_assets(
        &self,
        graph: &awsm_audio_schema::Graph,
        assets: &awsm_audio_schema::AssetTable,
    ) {
        use awsm_audio_schema::NodeKind;
        let wasm = self.wasm_assets.borrow();
        let bufs = self.buffer_assets.borrow();
        for node in &graph.nodes {
            match &node.kind {
                NodeKind::AudioWorklet(w) => {
                    if let Some(id) = w.module {
                        if !assets.wasm_modules.iter().any(|a| a.id == id)
                            && !wasm.contains_key(&id)
                        {
                            tracing::warn!("worklet node references missing wasm module {id}");
                        }
                    }
                }
                NodeKind::AudioBufferSource(b) => {
                    if let Some(id) = b.buffer {
                        if !assets.buffers.iter().any(|a| a.id == id) && !bufs.contains_key(&id) {
                            tracing::warn!("buffer-source references missing buffer {id}");
                        }
                    }
                }
                NodeKind::Convolver(c) => {
                    if let Some(id) = c.buffer {
                        if !assets.buffers.iter().any(|a| a.id == id) && !bufs.contains_key(&id) {
                            tracing::warn!("convolver references missing IR buffer {id}");
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Register + decode embedded audio buffers so buffer-source / convolver
    /// nodes work after Open. Encoded (base64) clips are re-decoded; URL clips
    /// are fetched + decoded; inline PCM is uploaded directly.
    fn load_buffer_assets(&self, assets: Vec<awsm_audio_schema::BufferAsset>) {
        use awsm_audio_schema::AudioSource;
        for asset in assets {
            self.buffer_assets
                .borrow_mut()
                .insert(asset.id, asset.clone());
            if !self.ensure_player() {
                return;
            }
            let id = asset.id;
            let player = self.player.clone();
            let ctrl = self.clone();
            match asset.source {
                AudioSource::Encoded(b64) => {
                    let Ok(bytes) =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64)
                    else {
                        tracing::error!("decode buffer base64");
                        continue;
                    };
                    wasm_bindgen_futures::spawn_local(async move {
                        let arr = js_sys::Uint8Array::from(bytes.as_slice());
                        let buf: js_sys::ArrayBuffer = arr.buffer().unchecked_into();
                        ctrl.decode_and_store(&player, id, &buf).await;
                    });
                }
                AudioSource::Url(url) => {
                    wasm_bindgen_futures::spawn_local(async move {
                        match fetch_bytes(&url).await {
                            Ok(buf) => ctrl.decode_and_store(&player, id, &buf).await,
                            Err(e) => tracing::error!("fetch buffer url {url}: {e:?}"),
                        }
                    });
                }
                AudioSource::Pcm {
                    sample_rate,
                    channels,
                } => {
                    if let Some(p) = player.borrow_mut().as_mut() {
                        if let Err(e) = p.store_pcm(id, sample_rate, &channels) {
                            tracing::error!("store pcm: {e}");
                        }
                    }
                    self.samples_rev.replace_with(|r| r.wrapping_add(1));
                    if self.playing.get() {
                        self.play();
                    }
                }
                // Project-relative path, or a relative/absolute URL. Directory
                // opens normally rehydrate these to inline bytes first; bundled
                // examples keep the path and resolve it against their copied
                // example directory.
                AudioSource::Path(path) => {
                    let base = self.asset_base_path.borrow().clone();
                    let url = resolve_asset_url(base.as_deref(), &path);
                    wasm_bindgen_futures::spawn_local(async move {
                        match fetch_bytes(&url).await {
                            Ok(buf) => ctrl.decode_and_store(&player, id, &buf).await,
                            Err(e) => tracing::error!("fetch buffer path {url}: {e:?}"),
                        }
                    });
                }
            }
        }
    }

    /// Decode an encoded-audio ArrayBuffer and register it under `id`, replaying
    /// if currently playing.
    async fn decode_and_store(
        &self,
        player: &Rc<RefCell<Option<awsm_audio_player::Player>>>,
        id: awsm_audio_schema::AssetId,
        array_buf: &js_sys::ArrayBuffer,
    ) {
        let promise = match player
            .borrow()
            .as_ref()
            .and_then(|p| p.decode(array_buf).ok())
        {
            Some(pr) => pr,
            None => return,
        };
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(v) => {
                let buffer: web_sys::AudioBuffer = v.unchecked_into();
                // Cache the decoded PCM back onto the in-memory asset. Buffers
                // loaded from a project (Encoded/Path) arrive without raw samples,
                // but the arrange-view waveform peaks read `AudioSource::Pcm` — so a
                // reloaded bounce would otherwise draw blank. (In-session bounces
                // are already Pcm; this makes a reloaded one behave the same.)
                let nch = buffer.number_of_channels();
                let mut channels: Vec<Vec<f32>> = Vec::with_capacity(nch as usize);
                for ch in 0..nch {
                    if let Ok(data) = buffer.get_channel_data(ch) {
                        channels.push(data);
                    }
                }
                if !channels.is_empty() {
                    if let Some(ba) = self.buffer_assets.borrow_mut().get_mut(&id) {
                        ba.source = awsm_audio_schema::AudioSource::Pcm {
                            sample_rate: buffer.sample_rate(),
                            channels,
                        };
                    }
                }
                if let Some(p) = player.borrow_mut().as_mut() {
                    p.store_buffer(id, buffer);
                }
                self.samples_rev.replace_with(|r| r.wrapping_add(1));
                if self.playing.get() {
                    self.play();
                }
            }
            Err(e) => tracing::error!("decode embedded buffer: {e:?}"),
        }
    }

    /// Display name + source kind for a loaded WASM module, by asset id. `None`
    /// if no module is registered for that id (the picker then reads "none").
    pub fn wasm_module_info(
        &self,
        id: awsm_audio_schema::AssetId,
    ) -> Option<(String, &'static str)> {
        use awsm_audio_schema::WasmSource;
        self.wasm_assets.borrow().get(&id).map(|a| {
            let label = a
                .label
                .clone()
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "module".to_string());
            let kind = match &a.source {
                WasmSource::Base64(_) => "embedded",
                WasmSource::Url(_) => "url",
                WasmSource::Path(_) => "file",
            };
            (label, kind)
        })
    }

    /// Register + compile WASM modules so AudioWorklet nodes work after Open —
    /// decoding inline base64 or fetching a URL, then compiling + storing each.
    fn load_wasm_assets(&self, assets: Vec<awsm_audio_schema::WasmAsset>) {
        for asset in assets {
            self.wasm_assets
                .borrow_mut()
                .insert(asset.id, asset.clone());
            if !self.ensure_player() {
                return;
            }
            let id = asset.id;
            let source = asset.source.clone();
            let player = self.player.clone();
            let ctrl = self.clone();
            wasm_bindgen_futures::spawn_local(async move {
                // Resolve the module bytes (decode base64 or fetch the URL).
                let bytes: Vec<u8> = match source {
                    awsm_audio_schema::WasmSource::Base64(b64) => {
                        match base64::Engine::decode(
                            &base64::engine::general_purpose::STANDARD,
                            &b64,
                        ) {
                            Ok(b) => b,
                            Err(e) => {
                                tracing::error!("decode wasm base64: {e}");
                                return;
                            }
                        }
                    }
                    awsm_audio_schema::WasmSource::Url(url) => match fetch_bytes(&url).await {
                        Ok(buf) => js_sys::Uint8Array::new(&buf).to_vec(),
                        Err(e) => {
                            tracing::error!("fetch wasm url {url}: {e:?}");
                            return;
                        }
                    },
                    awsm_audio_schema::WasmSource::Path(path) => {
                        let base = ctrl.asset_base_path.borrow().clone();
                        let url = resolve_asset_url(base.as_deref(), &path);
                        match fetch_bytes(&url).await {
                            Ok(buf) => js_sys::Uint8Array::new(&buf).to_vec(),
                            Err(e) => {
                                tracing::error!("fetch wasm path {url}: {e:?}");
                                return;
                            }
                        }
                    }
                };
                if !ctrl.ensure_worklet_shim(&player).await {
                    return;
                }
                let u8 = js_sys::Uint8Array::from(bytes.as_slice());
                match wasm_bindgen_futures::JsFuture::from(
                    awsm_audio_player::Player::compile_module(&u8),
                )
                .await
                {
                    Ok(m) => {
                        if let Some(p) = player.borrow_mut().as_mut() {
                            p.store_module(id, m.unchecked_into());
                        }
                        // If already playing, re-instantiate so it's audible now.
                        if ctrl.playing.get() {
                            ctrl.play();
                        }
                    }
                    Err(e) => tracing::error!("compile embedded wasm: {e:?}"),
                }
            });
        }
    }

    /// Rebuild the canvas (nodes + connections) from a schema graph, placing
    /// nodes via `position`. Shared by load + undo/redo restore.
    fn rebuild(&self, graph: &awsm_audio_schema::Graph, position: impl Fn(NodeId) -> (f64, f64)) {
        use awsm_audio_schema::{ConnectionSink, ConnectionSource, PortId};
        use std::collections::HashMap;
        self.help.set(None);
        self.context_menu.set(None);
        self.pending.set(None);
        *self.drag.borrow_mut() = None;
        self.connections.lock_mut().clear();

        // Boundary nodes get fresh editor ids (they're derived from PortDecls,
        // not schema nodes); map port id → its boundary node for wiring.
        let mut inlet_map: HashMap<PortId, Rc<EditorNode>> = HashMap::new();
        let mut outlet_map: HashMap<PortId, Rc<EditorNode>> = HashMap::new();
        {
            let mut nodes = self.nodes.lock_mut();
            nodes.clear();
            for node in &graph.nodes {
                let en = EditorNode::new(node.id, node.kind.clone(), position(node.id));
                if let Some(l) = &node.label {
                    en.label.set(l.clone());
                }
                nodes.push_cloned(en);
            }
            for (i, p) in graph.inlets.iter().enumerate() {
                let en = EditorNode::boundary(
                    NodeId::new(),
                    BoundaryPort::Inlet,
                    p.id.0.clone(),
                    (40.0, 80.0 + i as f64 * 90.0),
                );
                en.default.set(p.default);
                inlet_map.insert(p.id.clone(), en.clone());
                nodes.push_cloned(en);
            }
            for (i, p) in graph.outlets.iter().enumerate() {
                let en = EditorNode::boundary(
                    NodeId::new(),
                    BoundaryPort::Outlet,
                    p.id.0.clone(),
                    (620.0, 80.0 + i as f64 * 90.0),
                );
                outlet_map.insert(p.id.clone(), en.clone());
                nodes.push_cloned(en);
            }
        }

        for conn in &graph.connections {
            let (from_node, from_output): (Option<Rc<EditorNode>>, u32) = match &conn.from {
                ConnectionSource::NodeOutput { node, output } => (self.node_by_id(*node), *output),
                ConnectionSource::Inlet { port } => (inlet_map.get(port).cloned(), 0),
                // A keyed sequencer output → resolve the key back to its port index
                // on the source node (so add/remove of sounds never mis-binds).
                ConnectionSource::SeqOut { node, key } => {
                    let n = self.node_by_id(*node);
                    let idx = n
                        .as_ref()
                        .and_then(|n| crate::ports::seq_index_of(&n.kind.borrow(), key));
                    match idx {
                        Some(i) => (n, i),
                        None => (None, 0), // key no longer exists — drop the wire
                    }
                }
            };
            let (to_node, sink): (Option<Rc<EditorNode>>, ConnSink) = match &conn.to {
                ConnectionSink::NodeInput { node, input } => {
                    (self.node_by_id(*node), ConnSink::Input(*input))
                }
                ConnectionSink::NodeParam { node, param } => {
                    (self.node_by_id(*node), ConnSink::Param(param.clone()))
                }
                ConnectionSink::Outlet { port } => {
                    (outlet_map.get(port).cloned(), ConnSink::Input(0))
                }
                ConnectionSink::Trigger { node } => (self.node_by_id(*node), ConnSink::Trigger),
            };
            if let (Some(f), Some(t)) = (from_node, to_node) {
                self.connections
                    .lock_mut()
                    .push_cloned(Rc::new(EditorConnection {
                        // Honour a wire id the document carried (stable identity
                        // across save/load); else mint a fresh one.
                        id: conn.id.unwrap_or_else(ConnId::new_v4),
                        from: f,
                        from_output,
                        to: t,
                        sink,
                    }));
            }
        }
        // Fresh nodes start unselected.
        self.inspected.set(None);
    }

    /// Recompute the inspected node from the current per-node selection flags
    /// (used after restoring a snapshot, which sets flags directly).
    fn recompute_inspected(&self) {
        let selected: Vec<NodeId> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.selected.get())
            .map(|n| n.id)
            .collect();
        self.inspected.set(if selected.len() == 1 {
            Some(selected[0])
        } else {
            None
        });
    }

    // ==================================================================
    // Undo / redo — snapshot the whole document before each edit. Simpler and
    // more robust than per-command inverses for a graph this size.
    // ==================================================================

    const UNDO_LIMIT: usize = 64;

    /// Record the current state for undo (called before non-transient edits).
    fn push_undo(&self) {
        let mut undo = self.undo_stack.borrow_mut();
        undo.push(self.snapshot());
        let len = undo.len();
        if len > Self::UNDO_LIMIT {
            undo.drain(0..len - Self::UNDO_LIMIT);
        }
        drop(undo);
        self.redo_stack.borrow_mut().clear();
        self.refresh_undo_flags();
    }

    /// Restore the previous state.
    pub fn undo(&self) {
        let Some(prev) = self.undo_stack.borrow_mut().pop() else {
            return;
        };
        self.redo_stack.borrow_mut().push(self.snapshot());
        self.restore(&prev);
        self.refresh_undo_flags();
    }

    /// Re-apply the last undone state.
    pub fn redo(&self) {
        let Some(next) = self.redo_stack.borrow_mut().pop() else {
            return;
        };
        self.undo_stack.borrow_mut().push(self.snapshot());
        self.restore(&next);
        self.refresh_undo_flags();
    }

    fn restore(&self, snap: &EditorSnapshot) {
        let positions: std::collections::HashMap<NodeId, (f64, f64)> =
            snap.layout.iter().map(|l| (l.id, (l.x, l.y))).collect();
        self.rebuild(&snap.graph, |id| {
            positions.get(&id).copied().unwrap_or((80.0, 80.0))
        });
        self.pan.set((snap.pan_x, snap.pan_y));
        self.zoom.set(snap.zoom);
        let selected = &snap.selection;
        for n in self.nodes.lock_ref().iter() {
            n.selected.set_neq(selected.contains(&n.id));
        }
        // Arrangement edits live on the sample, not the canvas — restore them onto
        // the active Arrangement sample so undo/redo covers timeline changes.
        if let Some(arr) = &snap.arrangement {
            let active = *self.active.borrow();
            if let Some(st) = self
                .samples
                .borrow_mut()
                .iter_mut()
                .find(|s| s.sample.id == active)
            {
                st.sample.arrangement = arr.clone();
            }
            self.samples_rev.replace_with(|r| r.wrapping_add(1));
        }
        self.recompute_inspected();
    }

    fn refresh_undo_flags(&self) {
        self.can_undo.set_neq(!self.undo_stack.borrow().is_empty());
        self.can_redo.set_neq(!self.redo_stack.borrow().is_empty());
    }

    /// Open the right-click context menu on a node.
    pub fn open_context_menu(&self, node: NodeId, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::Node(node), x, y)));
    }
    /// Open the context menu on a wire.
    pub fn open_wire_menu(&self, wire: ConnId, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::Wire(wire), x, y)));
    }
    /// Open the context menu on an arrangement clip.
    pub fn open_clip_menu(&self, track: usize, clip: usize, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::Clip { track, clip }, x, y)));
    }
    /// Open the context menu on an Assets-panel Sound.
    pub fn open_sound_menu(&self, id: awsm_audio_schema::SampleId, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::Sound(id), x, y)));
    }
    /// Open the context menu on empty lane space at `(track, secs)`.
    pub fn open_lane_menu(&self, track: usize, secs: f64, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::Lane { track, secs }, x, y)));
    }
    /// Open the context menu on an arrangement track gain automation point.
    pub fn open_track_gain_point_menu(&self, track: usize, index: usize, x: f64, y: f64) {
        self.context_menu
            .set(Some((ContextTarget::TrackGainPoint { track, index }, x, y)));
    }
    pub fn close_context_menu(&self) {
        self.context_menu.set(None);
    }

    /// Drop a Sound's clip on the selected track at the current playhead — the
    /// double-click / context-menu "Place at playhead" action. Goes through the
    /// `AddClip` command so it's MCP-drivable.
    pub fn place_sound_at_playhead(&self, id: awsm_audio_schema::SampleId) {
        let track = self.selected_track.get();
        let start = self.arrange_start_secs();
        self.dispatch(EditorCommand::EditArrange {
            op: ArrangeOp::AddClip {
                track,
                start,
                source: id,
                length: None,
            },
        });
    }

    // ==================================================================
    // Audio file loading: decode a picked file into the player's buffer
    // registry, then point the node's buffer at it.
    // ==================================================================

    /// Ensure the audio engine exists (creating its context needs a gesture,
    /// which a file pick / play click satisfies).
    fn ensure_player(&self) -> bool {
        let mut slot = self.player.borrow_mut();
        if slot.is_none() {
            match awsm_audio_player::Player::new() {
                Ok(mut p) => {
                    p.set_listener(Some(self.listener.borrow().clone()));
                    *slot = Some(p);
                }
                Err(e) => {
                    tracing::error!("audio init failed: {e}");
                    return false;
                }
            }
        }
        true
    }

    /// Set the spatial listener position; syncs the player + replays if playing.
    pub fn set_listener_position(&self, x: f32, y: f32, z: f32) {
        self.dispatch(EditorCommand::SetListener { x, y, z });
    }
    fn set_listener_position_impl(&self, x: f32, y: f32, z: f32) {
        {
            let mut l = self.listener.borrow_mut();
            l.position_x.value = x;
            l.position_y.value = y;
            l.position_z.value = z;
        }
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.set_listener(Some(self.listener.borrow().clone()));
        }
        if self.playing.get() {
            self.play();
        }
    }

    /// Request microphone access (getUserMedia); on grant, store the stream and
    /// replay so any MediaStream source node taps the live input.
    pub fn request_mic(&self) {
        if !self.ensure_player() {
            return;
        }
        let player = self.player.clone();
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let promise = match player.borrow().as_ref().map(|p| p.request_mic()) {
                Some(Ok(pr)) => pr,
                _ => {
                    tracing::error!("mic request failed to start");
                    return;
                }
            };
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(v) => {
                    if let Some(p) = player.borrow_mut().as_mut() {
                        p.set_mic(v.unchecked_into());
                    }
                    // Start (or restart) so the live input is routed now.
                    ctrl.play();
                }
                Err(e) => tracing::error!("microphone denied / unavailable: {e:?}"),
            }
        });
    }

    pub fn load_audio_file(&self, node: NodeId, file: web_sys::File) {
        if !self.ensure_player() {
            return;
        }
        let asset = awsm_audio_schema::AssetId::new();
        let label = file.name();
        let player = self.player.clone();
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            // Read the file → ArrayBuffer.
            let array_buf = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("read file: {e:?}");
                    return;
                }
            };
            let array_buf: js_sys::ArrayBuffer = array_buf.unchecked_into();
            // Snapshot the encoded bytes BEFORE decode (decodeAudioData detaches
            // the buffer) so the project can be saved self-contained.
            let bytes = js_sys::Uint8Array::new(&array_buf).to_vec();
            let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
            ctrl.buffer_assets.borrow_mut().insert(
                asset,
                awsm_audio_schema::BufferAsset {
                    id: asset,
                    label: Some(label),
                    source: awsm_audio_schema::AudioSource::Encoded(b64),
                },
            );
            // Decode → AudioBuffer (using the player's context).
            let promise = {
                let slot = player.borrow();
                match slot.as_ref().and_then(|p| p.decode(&array_buf).ok()) {
                    Some(pr) => pr,
                    None => return,
                }
            };
            match wasm_bindgen_futures::JsFuture::from(promise).await {
                Ok(v) => {
                    let buffer: web_sys::AudioBuffer = v.unchecked_into();
                    if let Some(p) = player.borrow_mut().as_mut() {
                        p.store_buffer(asset, buffer);
                    }
                    ctrl.set_node_buffer(node, asset);
                }
                Err(e) => {
                    tracing::error!("decodeAudioData failed (unsupported format?): {e:?}");
                }
            }
        });
    }

    /// Load audio from `url` into `node`'s buffer (the MCP `load_audio` tool).
    /// Fetches + decodes off the editor link; caches the decoded PCM onto the
    /// asset so offline renders (bounce / `render_wav`) see it. Returns the
    /// decoded buffer's shape.
    pub async fn load_audio_url(
        &self,
        node: NodeId,
        url: String,
        label: Option<String>,
    ) -> Result<awsm_audio_editor_protocol::AudioInfo, String> {
        use awsm_audio_schema::{AudioSource, BufferAsset, NodeKind};
        if !self.ensure_player() {
            return Err("audio context unavailable".into());
        }
        let buffer_node = self
            .node_by_id(node)
            .map(|n| {
                matches!(
                    &*n.kind.borrow(),
                    NodeKind::AudioBufferSource(_) | NodeKind::Convolver(_)
                )
            })
            .unwrap_or(false);
        if !buffer_node {
            return Err(format!(
                "node {node} is not an audio_buffer_source or convolver"
            ));
        }
        let array_buf = fetch_bytes(&url)
            .await
            .map_err(|e| format!("fetch {url}: {e:?}"))?;
        let promise = {
            let slot = self.player.borrow();
            slot.as_ref()
                .and_then(|p| p.decode(&array_buf).ok())
                .ok_or_else(|| "decode unavailable".to_string())?
        };
        let decoded = wasm_bindgen_futures::JsFuture::from(promise)
            .await
            .map_err(|e| format!("decodeAudioData failed (unsupported format?): {e:?}"))?;
        let buffer: web_sys::AudioBuffer = decoded.unchecked_into();
        let sample_rate = buffer.sample_rate();
        let nch = buffer.number_of_channels();
        let duration_secs = buffer.duration();
        let mut channels: Vec<Vec<f32>> = Vec::with_capacity(nch as usize);
        for ch in 0..nch {
            if let Ok(d) = buffer.get_channel_data(ch) {
                channels.push(d);
            }
        }
        if channels.is_empty() {
            return Err("decoded buffer has no channels".into());
        }
        let asset = awsm_audio_schema::AssetId::new();
        self.buffer_assets.borrow_mut().insert(
            asset,
            BufferAsset {
                id: asset,
                label,
                source: AudioSource::Pcm {
                    sample_rate,
                    channels,
                },
            },
        );
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.store_buffer(asset, buffer);
        }
        self.set_node_buffer(node, asset);
        Ok(awsm_audio_editor_protocol::AudioInfo {
            asset_id: asset.to_string(),
            duration_secs,
            sample_rate,
            channels: nch as usize,
        })
    }

    /// Point an `AudioBufferSource` (or `Convolver` IR) node at a decoded buffer.
    fn set_node_buffer(&self, node: NodeId, asset: awsm_audio_schema::AssetId) {
        use awsm_audio_schema::NodeKind;
        if let Some(n) = self.node_by_id(node) {
            match &mut *n.kind.borrow_mut() {
                NodeKind::AudioBufferSource(b) => b.buffer = Some(asset),
                NodeKind::Convolver(c) => c.buffer = Some(asset),
                _ => {}
            }
        }
        if self.playing.get() {
            self.play();
        }
    }

    // ==================================================================
    // WASM AudioWorklet modules: load a .wasm, discover its params, and
    // attach it to a node (compiled module lives in the player).
    // ==================================================================

    /// Load a picked `.wasm` into `node` (an AudioWorklet): read the bytes, then
    /// hand off to [`attach_wasm_bytes`](Self::attach_wasm_bytes).
    pub fn load_wasm_file(&self, node: NodeId, file: web_sys::File) {
        let label = file
            .name()
            .strip_suffix(".wasm")
            .map(str::to_string)
            .unwrap_or_else(|| file.name());
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let array_buf = match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("read .wasm: {e:?}");
                    return;
                }
            };
            let array_buf: js_sys::ArrayBuffer = array_buf.unchecked_into();
            let bytes = js_sys::Uint8Array::new(&array_buf).to_vec();
            ctrl.attach_wasm_bytes(node, bytes, label);
        });
    }

    /// Attach raw `.wasm` bytes to an AudioWorklet node: ensure the shim, compile,
    /// discover params, register (compiled in the player, base64 source here), and
    /// wire it onto the node. Shared by the file picker and the MCP/command seam.
    pub fn attach_wasm_bytes(&self, node: NodeId, bytes: Vec<u8>, label: String) {
        let ctrl = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            if let Err(e) = ctrl.attach_wasm_bytes_async(node, bytes, label).await {
                tracing::error!("attach wasm failed: {e}");
            }
        });
    }

    /// Fallible async variant of [`attach_wasm_bytes`]. Compiles the module,
    /// discovers its params, registers it, and wires it onto the node, returning
    /// the compile/ABI error (a human-readable string) instead of only logging
    /// it. The MCP `AttachWasm` path awaits this so a bad module surfaces to the
    /// agent; the file picker keeps the fire-and-forget [`attach_wasm_bytes`]
    /// wrapper.
    pub async fn attach_wasm_bytes_async(
        &self,
        node: NodeId,
        bytes: Vec<u8>,
        label: String,
    ) -> Result<(), String> {
        if !self.ensure_player() {
            return Err("audio player unavailable".into());
        }
        let asset = awsm_audio_schema::AssetId::new();
        let player = self.player.clone();
        if !self.ensure_worklet_shim(&player).await {
            return Err("worklet shim failed to load".into());
        }
        let u8 = js_sys::Uint8Array::from(bytes.as_slice());
        let module = match wasm_bindgen_futures::JsFuture::from(
            awsm_audio_player::Player::compile_module(&u8),
        )
        .await
        {
            Ok(m) => m.unchecked_into::<js_sys::WebAssembly::Module>(),
            Err(e) => {
                return Err(format!(
                    "WebAssembly.compile failed (not a valid module?): {e:?}"
                ));
            }
        };

        // Discover params (instantiate once on the main thread).
        let params = discover_params(&module);

        // Register: compiled module in the player, serializable source here.
        if let Some(p) = player.borrow_mut().as_mut() {
            p.store_module(asset, module);
        }
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
        self.wasm_assets.borrow_mut().insert(
            asset,
            awsm_audio_schema::WasmAsset {
                id: asset,
                label: Some(label.clone()),
                source: awsm_audio_schema::WasmSource::Base64(b64),
            },
        );
        self.set_node_module(node, asset, label, params);
        Ok(())
    }

    /// Ensure the worklet shim has been added to the player's context. Returns
    /// false on failure. Must not hold a player borrow across the await.
    async fn ensure_worklet_shim(
        &self,
        player: &Rc<RefCell<Option<awsm_audio_player::Player>>>,
    ) -> bool {
        let ready = player
            .borrow()
            .as_ref()
            .map(|p| p.worklet_ready())
            .unwrap_or(false);
        if ready {
            return true;
        }
        let promise = match player.borrow().as_ref().map(|p| p.add_worklet_shim()) {
            Some(Ok(pr)) => pr,
            _ => return false,
        };
        if let Err(e) = wasm_bindgen_futures::JsFuture::from(promise).await {
            tracing::error!("worklet addModule failed: {e:?}");
            return false;
        }
        if let Some(p) = player.borrow_mut().as_mut() {
            p.mark_worklet_ready();
        }
        true
    }

    /// Attach a compiled module + discovered params to an AudioWorklet node and
    /// re-render it (new params change its fields, inlets, and inspector).
    fn set_node_module(
        &self,
        node: NodeId,
        asset: awsm_audio_schema::AssetId,
        label: String,
        params: Vec<awsm_audio_schema::WorkletParam>,
    ) {
        if let Some(n) = self.node_by_id(node) {
            if let awsm_audio_schema::NodeKind::AudioWorklet(w) = &mut *n.kind.borrow_mut() {
                w.module = Some(asset);
                w.processor_name = label;
                w.parameters = params;
            }
        }
        self.touch_node(node);
        self.inspector_rev.replace_with(|r| r.wrapping_add(1));
        if self.playing.get() {
            self.play();
        }
    }

    /// Force a single node's card to re-render (its `kind` changed structurally).
    fn touch_node(&self, id: NodeId) {
        let mut nodes = self.nodes.lock_mut();
        if let Some(i) = nodes.iter().position(|n| n.id == id) {
            if let Some(n) = nodes.get(i).cloned() {
                nodes.set_cloned(i, n);
            }
        }
    }

    /// Remove every selected node (Delete key).
    pub fn delete_selected(&self) {
        let ids: Vec<NodeId> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.selected.get())
            .map(|n| n.id)
            .collect();
        for id in ids {
            self.dispatch(EditorCommand::RemoveNode { id });
        }
    }

    // ==================================================================
    // Editor UX: selection group move, select-all, new, view, clipboard, rename
    // ==================================================================

    /// Grab offsets `(id, wx-x, wy-y)` for every selected node (falls back to
    /// the grabbed node alone), so a drag moves the whole selection rigidly.
    pub fn selected_drag_items(
        &self,
        grabbed: NodeId,
        wx: f64,
        wy: f64,
    ) -> Vec<(NodeId, f64, f64)> {
        let nodes = self.nodes.lock_ref();
        let mut items: Vec<(NodeId, f64, f64)> = nodes
            .iter()
            .filter(|n| n.selected.get())
            .map(|n| {
                let (nx, ny) = n.pos.get();
                (n.id, wx - nx, wy - ny)
            })
            .collect();
        if items.is_empty() {
            if let Some(n) = nodes.iter().find(|n| n.id == grabbed) {
                let (nx, ny) = n.pos.get();
                items.push((grabbed, wx - nx, wy - ny));
            }
        }
        items
    }

    /// Select every node.
    pub fn select_all(&self) {
        let ids: Vec<NodeId> = self.nodes.lock_ref().iter().map(|n| n.id).collect();
        self.dispatch(EditorCommand::SelectNodes {
            ids,
            additive: false,
        });
    }

    /// Clear to a brand-new single-sample project.
    pub fn new_project(&self) {
        self.stop();
        self.connections.lock_mut().clear();
        self.nodes.lock_mut().clear();
        self.set_selection(&[], false);
        let sample = awsm_audio_schema::Sample::new("main");
        let id = sample.id;
        *self.samples.borrow_mut() = vec![StoredSample {
            sample,
            layout: Vec::new(),
        }];
        *self.active.borrow_mut() = id;
        *self.root.borrow_mut() = id;
        self.view.set(awsm_audio_schema::SampleKind::Sound);
        self.status.set(None);
        self.undo_stack.borrow_mut().clear();
        self.redo_stack.borrow_mut().clear();
        self.refresh_undo_flags();
        self.pan.set((0.0, 0.0));
        self.zoom.set(1.0);
        self.samples_rev.replace_with(|r| r.wrapping_add(1));
    }

    /// Reset the camera (pan to origin, zoom 1).
    pub fn reset_view(&self) {
        self.pan.set((0.0, 0.0));
        self.zoom.set(1.0);
    }

    /// Pan/zoom so all nodes fit the viewport with padding.
    pub fn zoom_to_fit(&self) {
        let nodes = self.nodes.lock_ref();
        if nodes.is_empty() {
            self.reset_view();
            return;
        }
        let (mut minx, mut miny, mut maxx, mut maxy) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for n in nodes.iter() {
            let (x, y) = n.pos.get();
            let h = crate::ports::node_height(&n.kind.borrow());
            minx = minx.min(x);
            miny = miny.min(y);
            maxx = maxx.max(x + crate::ports::NODE_WIDTH);
            maxy = maxy.max(y + h);
        }
        drop(nodes);
        let (vw, vh) = self
            .viewport
            .borrow()
            .as_ref()
            .map(|el| {
                let r = el.get_bounding_client_rect();
                (r.width(), r.height())
            })
            .filter(|(w, h)| *w > 1.0 && *h > 1.0)
            .unwrap_or((800.0, 600.0));
        let pad = 60.0;
        let (bw, bh) = ((maxx - minx).max(1.0), (maxy - miny).max(1.0));
        let zoom = ((vw - pad) / bw).min((vh - pad) / bh).clamp(0.25, 1.5);
        // Center the bounding box: world center maps to viewport center.
        let (cx, cy) = ((minx + maxx) / 2.0, (miny + maxy) / 2.0);
        self.zoom.set(zoom);
        self.pan.set((vw / 2.0 - cx * zoom, vh / 2.0 - cy * zoom));
    }

    /// Copy the current selection (nodes + internal wires) to the clipboard.
    pub fn copy_selection(&self) {
        let nodes = self.nodes.lock_ref();
        let selected: Vec<&Rc<EditorNode>> = nodes.iter().filter(|n| n.selected.get()).collect();
        if selected.is_empty() {
            return;
        }
        let minx = selected
            .iter()
            .map(|n| n.pos.get().0)
            .fold(f64::MAX, f64::min);
        let miny = selected
            .iter()
            .map(|n| n.pos.get().1)
            .fold(f64::MAX, f64::min);
        let index: std::collections::HashMap<NodeId, usize> = selected
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let clip_nodes = selected
            .iter()
            .map(|n| {
                let (x, y) = n.pos.get();
                (
                    n.kind.borrow().clone(),
                    n.label.get_cloned(),
                    x - minx,
                    y - miny,
                )
            })
            .collect();
        let wires = self
            .connections
            .lock_ref()
            .iter()
            .filter_map(|c| {
                let from = *index.get(&c.from.id)?;
                let to = *index.get(&c.to.id)?;
                Some((from, c.from_output, to, c.sink.clone()))
            })
            .collect();
        *self.clipboard.borrow_mut() = Some(Clipboard {
            nodes: clip_nodes,
            wires,
        });
    }

    /// Paste the clipboard near the viewport center, selecting the new nodes.
    pub fn paste_clipboard(&self) {
        let Some(clip) = self.clipboard.borrow().clone() else {
            return;
        };
        if clip.nodes.is_empty() {
            return;
        }
        self.dispatch(EditorCommand::Paste { clip });
    }
    fn paste_impl(&self, clip: Clipboard) {
        if clip.nodes.is_empty() {
            return;
        }
        let (cx, cy) = self.world_center();
        let mut new_ids: Vec<NodeId> = Vec::with_capacity(clip.nodes.len());
        {
            let mut nodes = self.nodes.lock_mut();
            for (kind, label, dx, dy) in &clip.nodes {
                let id = NodeId::new();
                let en = EditorNode::new(id, kind.clone(), (cx - 60.0 + dx, cy - 40.0 + dy));
                if !label.is_empty() {
                    en.label.set(label.clone());
                }
                nodes.push_cloned(en);
                new_ids.push(id);
            }
        }
        for (from, from_output, to, sink) in &clip.wires {
            if let (Some(f), Some(t)) = (new_ids.get(*from), new_ids.get(*to)) {
                self.add_connection(*f, *from_output, *t, sink.clone());
            }
        }
        self.set_selection(&new_ids, false);
    }

    /// Push a connection between resolved nodes (used by paste).
    fn add_connection(&self, from: NodeId, from_output: u32, to: NodeId, sink: ConnSink) {
        if let (Some(f), Some(t)) = (self.node_by_id(from), self.node_by_id(to)) {
            self.connections
                .lock_mut()
                .push_cloned(Rc::new(EditorConnection {
                    id: ConnId::new_v4(),
                    from: f,
                    from_output,
                    to: t,
                    sink,
                }));
        }
    }

    /// Rename a node (empty clears it back to the type name).
    pub fn rename_node(&self, id: NodeId, label: String) {
        self.dispatch(EditorCommand::RenameNode { id, label });
    }
    fn rename_node_impl(&self, id: NodeId, label: String) {
        if let Some(n) = self.node_by_id(id) {
            n.label.set(label);
        }
    }

    // ==================================================================
    // Lookups
    // ==================================================================

    pub fn node_by_id(&self, id: NodeId) -> Option<Rc<EditorNode>> {
        self.nodes.lock_ref().iter().find(|n| n.id == id).cloned()
    }

    /// Select every node whose box intersects the world rectangle.
    pub fn select_in_box(&self, x0: f64, y0: f64, x1: f64, y1: f64) {
        let (lx, rx) = (x0.min(x1), x0.max(x1));
        let (ty, by) = (y0.min(y1), y0.max(y1));
        let ids: Vec<NodeId> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| {
                let (nx, ny) = n.pos.get();
                let w = crate::ports::NODE_WIDTH;
                let h = crate::ports::node_height(&n.kind.borrow());
                nx < rx && nx + w > lx && ny < by && ny + h > ty
            })
            .map(|n| n.id)
            .collect();
        self.set_selection(&ids, false);
    }

    fn set_selection(&self, ids: &[NodeId], additive: bool) {
        for n in self.nodes.lock_ref().iter() {
            let want = ids.contains(&n.id);
            if additive {
                if want {
                    n.selected.set_neq(true);
                }
            } else {
                n.selected.set_neq(want);
            }
        }
        // The inspector shows a node only when it's the sole selection.
        let selected: Vec<NodeId> = self
            .nodes
            .lock_ref()
            .iter()
            .filter(|n| n.selected.get())
            .map(|n| n.id)
            .collect();
        self.inspected.set(if selected.len() == 1 {
            Some(selected[0])
        } else {
            None
        });
    }
}

// ======================================================================
// WASM worklet param discovery (free fns — JS interop over the module ABI).
// ======================================================================

/// Instantiate `module` once (main thread, no imports) and read its parameter
/// descriptors. A module without the discovery exports simply yields no params.
fn discover_params(module: &js_sys::WebAssembly::Module) -> Vec<awsm_audio_schema::WorkletParam> {
    use awsm_audio_schema::{AudioParam, ParamId, WorkletParam};
    let imports = js_sys::Object::new();
    let inst = match js_sys::WebAssembly::Instance::new(module, &imports) {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("instantiate for discovery: {e:?}");
            return vec![];
        }
    };
    let exports = inst.exports();
    let count = call_f64(&exports, "param_count", None).unwrap_or(0.0) as usize;
    if count == 0 {
        return vec![];
    }
    let Some(mem) = js_sys::Reflect::get(&exports, &JsValue::from_str("memory"))
        .ok()
        .and_then(|m| m.dyn_into::<js_sys::WebAssembly::Memory>().ok())
    else {
        return vec![];
    };
    let view = js_sys::Uint8Array::new(&mem.buffer());
    (0..count)
        .map(|i| {
            let fi = i as f64;
            let name = read_name(&view, &exports, fi).unwrap_or_else(|| format!("p{i}"));
            let min = call_f64(&exports, "param_min", Some(fi)).unwrap_or(0.0) as f32;
            let max = call_f64(&exports, "param_max", Some(fi)).unwrap_or(1.0) as f32;
            let def = call_f64(&exports, "param_default", Some(fi)).unwrap_or(0.0) as f32;
            WorkletParam {
                name: ParamId(name),
                min,
                max,
                param: AudioParam::new(def),
            }
        })
        .collect()
}

/// Call an exported function returning a number (0 or 1 numeric arg).
fn call_f64(exports: &js_sys::Object, name: &str, arg: Option<f64>) -> Option<f64> {
    let f: js_sys::Function = js_sys::Reflect::get(exports, &JsValue::from_str(name))
        .ok()?
        .dyn_into()
        .ok()?;
    let res = match arg {
        None => f.call0(exports),
        Some(a) => f.call1(exports, &JsValue::from_f64(a)),
    }
    .ok()?;
    res.as_f64()
}

/// Fetch a URL into an `ArrayBuffer` (for URL-sourced buffer / wasm assets).
/// A filesystem-safe download name for a Sound (`"Wobble Bass"` → `"Wobble-Bass"`),
/// falling back to `"sound"` when nothing usable remains.
/// Project an editor [`fields::Field`](crate::fields::Field) into the
/// serializable [`FieldInfo`](command::FieldInfo) the MCP discovery queries
/// (`Catalog` / `NodeFields`) return — so an agent learns a node's `set_field`
/// keys, control type, and current value without knowing the schema.
fn field_info(f: &crate::fields::Field) -> command::FieldInfo {
    use crate::fields::Control;
    let (control, value_num, value_text, options) = match &f.control {
        Control::Number(n) => ("number".to_string(), Some(*n), None, Vec::new()),
        Control::Choice { value, options } => (
            "choice".to_string(),
            None,
            Some(value.clone()),
            options.iter().map(|s| s.to_string()).collect(),
        ),
        Control::Bool(b) => (
            "bool".to_string(),
            Some(if *b { 1.0 } else { 0.0 }),
            None,
            Vec::new(),
        ),
    };
    command::FieldInfo {
        key: f.key.to_string(),
        label: f.label.to_string(),
        control,
        value_num,
        value_text,
        options,
        modulatable: f.modulation.is_some(),
    }
}

fn sanitize_filename(name: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        "sound".to_string()
    } else {
        s
    }
}

fn resolve_asset_url(base: Option<&str>, path: &str) -> String {
    let path = path.trim();
    let lower = path.to_ascii_lowercase();
    if lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("data:")
        || lower.starts_with("blob:")
        || path.starts_with('/')
    {
        return path.to_string();
    }
    match base.map(str::trim).filter(|base| !base.is_empty()) {
        Some(base) => format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        ),
        None => path.to_string(),
    }
}

fn example_project_base(key: &str) -> Option<&'static str> {
    match key {
        "arrangement" => Some("examples/arrangement"),
        "song" => Some("examples/sequenced-song"),
        _ => None,
    }
}

fn example_library_url(key: &str) -> Option<&'static str> {
    match key {
        "acidrack" => Some("examples/acidrack.toml"),
        "bell" => Some("examples/bell.toml"),
        "chord" => Some("examples/chord.toml"),
        "crush" => Some("examples/crush.toml"),
        "drive" => Some("examples/drive.toml"),
        "fire" => Some("examples/fire.toml"),
        "hihat" => Some("examples/hihat.toml"),
        "kick" => Some("examples/kick.toml"),
        "laser" => Some("examples/laser.toml"),
        "nested" => Some("examples/nested.toml"),
        "rain" => Some("examples/rain.toml"),
        "ringmod" => Some("examples/ringmod.toml"),
        "rocket" => Some("examples/rocket.toml"),
        "siren" => Some("examples/siren.toml"),
        "spatial" => Some("examples/spatial.toml"),
        "wind" => Some("examples/wind.toml"),
        "wobble" => Some("examples/wobble.toml"),
        _ => None,
    }
}

fn example_asset_base() -> &'static str {
    "examples"
}

async fn fetch_text(url: &str) -> Result<String, JsValue> {
    let win = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(win.fetch_with_str(url))
        .await?
        .dyn_into()?;
    if !resp.ok() {
        return Err(JsValue::from_str(&format!(
            "HTTP {} {}",
            resp.status(),
            resp.status_text()
        )));
    }
    let text = wasm_bindgen_futures::JsFuture::from(resp.text()?).await?;
    text.as_string()
        .ok_or_else(|| JsValue::from_str("response text was not a string"))
}

async fn fetch_bytes(url: &str) -> Result<js_sys::ArrayBuffer, JsValue> {
    let win = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let resp: web_sys::Response = wasm_bindgen_futures::JsFuture::from(win.fetch_with_str(url))
        .await?
        .dyn_into()?;
    if !resp.ok() {
        return Err(JsValue::from_str(&format!(
            "HTTP {} {}",
            resp.status(),
            resp.status_text()
        )));
    }
    let buf = wasm_bindgen_futures::JsFuture::from(resp.array_buffer()?).await?;
    Ok(buf.unchecked_into())
}

/// Read a UTF-8 param name from the module's memory via `param_name_ptr/len`.
fn read_name(view: &js_sys::Uint8Array, exports: &js_sys::Object, i: f64) -> Option<String> {
    let ptr = call_f64(exports, "param_name_ptr", Some(i))? as u32;
    let len = call_f64(exports, "param_name_len", Some(i))? as u32;
    if len == 0 || len > 256 {
        return None;
    }
    let mut bytes = vec![0u8; len as usize];
    view.subarray(ptr, ptr + len).copy_to(&mut bytes);
    String::from_utf8(bytes).ok()
}

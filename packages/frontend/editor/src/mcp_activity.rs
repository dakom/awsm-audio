//! "Current work" surfaces for the MCP agent — so watching the editor while an
//! agent drives it reads as a *live build*, not an opaque pulse.
//!
//! Three independently-toggleable surfaces, all fed from one place ([`begin`],
//! called per served [`Request`] in [`crate::remote::serve_one`]):
//!
//! 1. **Live action label** — the top-bar 🤖 chip says *what* is happening right
//!    now ("Adding Oscillator", "Bouncing “Bass”", "Connecting nodes") instead of
//!    a generic "working…". Driven by [`CURRENT`].
//! 2. **Activity feed** — a compact rolling log of recent actions ([`FEED`]). Off
//!    by default (it can crowd the canvas); the label + spotlight already convey
//!    the gist.
//! 3. **Auto-follow / spotlight** — pulls the canvas to whatever sample the agent
//!    touches (via [`EditorController::open_sample`](crate::controller::EditorController::open_sample),
//!    which also flips the view, so a bounce/edit on an *Arrangement* opens the
//!    arranger — not just Sounds) and flashes the affected node ([`SPOTLIGHT`]).
//!
//! The three toggles persist in `localStorage`, **keyed by a per-tab session
//! id** — each browser tab is an independent project, so two tabs must never
//! stomp each other's settings. The id is minted once per tab and kept in
//! `sessionStorage` (per-tab by nature, survives reloads of that tab). Reads
//! fall back to an unsuffixed global key, which every write also updates — so a
//! brand-new tab inherits your last-set preference, then owns its own copy.

use std::cell::Cell;

use futures_signals::signal::Mutable;
use futures_signals::signal_vec::{MutableVec, SignalVec};
use wasm_bindgen_futures::spawn_local;

use awsm_audio_editor_protocol::schema::{NodeId, SampleId, SampleKind};
use awsm_audio_editor_protocol::{
    ArrangeOp, EditorCommand, EditorQuery, Request, Response, SongOp,
};

use crate::controller::controller;
use crate::ports::kind_label;

/// How long a freshly-touched node keeps its spotlight glow before it fades.
const SPOTLIGHT_MS: u32 = 1100;
/// Most recent actions kept in the feed (oldest dropped past this).
const FEED_MAX: usize = 8;

// Base localStorage key names. Persisted per tab as `<key>.<tab id>` with the
// bare key as the inherited-default fallback (see module docs + [`tab_id`]).
const KEY_LABEL: &str = "awsm.mcp.show_action_label";
const KEY_FOLLOW: &str = "awsm.mcp.auto_follow";
const KEY_FEED: &str = "awsm.mcp.show_feed";
/// sessionStorage slot holding this tab's minted session id.
const KEY_TAB_ID: &str = "awsm.tab_id";

thread_local! {
    // ---- settings (persisted; defaults: label + follow on, feed off) ----
    static SHOW_LABEL: Mutable<bool> = Mutable::new(load_setting(KEY_LABEL, true));
    static AUTO_FOLLOW: Mutable<bool> = Mutable::new(load_setting(KEY_FOLLOW, true));
    static SHOW_FEED: Mutable<bool> = Mutable::new(load_setting(KEY_FEED, false));

    // ---- live state ----
    /// Human label of the action in flight; drives the chip text. Cleared on idle.
    static CURRENT: Mutable<Option<String>> = Mutable::new(None);
    /// Rolling recent-actions log, newest first (capped at [`FEED_MAX`]).
    static FEED: MutableVec<String> = MutableVec::new();
    /// The node currently glowing (agent just touched it), if any.
    static SPOTLIGHT: Mutable<Option<NodeId>> = Mutable::new(None);
    /// Guards the spotlight auto-clear timeout against a newer spotlight.
    static SPOTLIGHT_GEN: Cell<u64> = const { Cell::new(0) };
}

// ---------------------------------------------------------------------------
// Settings: reactive accessors (for binding) + persisting setters.
// ---------------------------------------------------------------------------

/// Live "show the action label on the chip" toggle.
pub fn show_label() -> Mutable<bool> {
    SHOW_LABEL.with(|m| m.clone())
}
/// Live "follow the agent + spotlight touched nodes" toggle.
pub fn auto_follow() -> Mutable<bool> {
    AUTO_FOLLOW.with(|m| m.clone())
}
/// Live "show the rolling activity feed" toggle.
pub fn show_feed() -> Mutable<bool> {
    SHOW_FEED.with(|m| m.clone())
}

/// Set + persist the action-label toggle.
pub fn set_show_label(on: bool) {
    SHOW_LABEL.with(|m| m.set_neq(on));
    save_setting(KEY_LABEL, on);
}
/// Set + persist the auto-follow toggle. Turning it off also drops any live glow.
pub fn set_auto_follow(on: bool) {
    AUTO_FOLLOW.with(|m| m.set_neq(on));
    if !on {
        SPOTLIGHT.with(|m| m.set_neq(None));
    }
    save_setting(KEY_FOLLOW, on);
}
/// Set + persist the feed toggle. Turning it off clears the backlog.
pub fn set_show_feed(on: bool) {
    SHOW_FEED.with(|m| m.set_neq(on));
    if !on {
        FEED.with(|f| f.lock_mut().clear());
    }
    save_setting(KEY_FEED, on);
}

// ---------------------------------------------------------------------------
// Live-state accessors (for the UI).
// ---------------------------------------------------------------------------

/// The label of the action in flight (or `None` between actions / when idle).
pub fn current() -> Mutable<Option<String>> {
    CURRENT.with(|m| m.clone())
}
/// The spotlighted node, if any (each node compares its own id to this).
pub fn spotlight() -> Mutable<Option<NodeId>> {
    SPOTLIGHT.with(|m| m.clone())
}
/// The activity-feed stream (action labels), newest first.
pub fn feed_signal() -> impl SignalVec<Item = String> {
    FEED.with(|f| f.signal_vec_cloned())
}

// ---------------------------------------------------------------------------
// The per-request entry points (called from `remote::serve_one`).
// ---------------------------------------------------------------------------

/// Record the start of serving one MCP request: set the live label, push a feed
/// entry, and (when auto-follow is on) pull the canvas to the touched sample and
/// flash the named node. A no-op for noisy/transient requests (drags, selections,
/// camera moves, snapshot reads) and when every surface is off.
pub fn begin(req: &Request) {
    let any = SHOW_LABEL.with(|m| m.get())
        || SHOW_FEED.with(|m| m.get())
        || AUTO_FOLLOW.with(|m| m.get());
    if !any {
        return;
    }
    let Some(info) = describe(req) else {
        return;
    };

    if SHOW_LABEL.with(|m| m.get()) {
        CURRENT.with(|m| m.set(Some(info.label.clone())));
    }
    if SHOW_FEED.with(|m| m.get()) {
        push_feed(info.label);
    }
    if AUTO_FOLLOW.with(|m| m.get()) {
        if let Some(sample) = info.follow {
            follow_to(sample);
        }
        if let Some(node) = info.spotlight {
            set_spotlight(node);
        }
    }
}

/// Spotlight a node the agent *just created* (its id isn't known until the
/// command returns). No-op unless auto-follow is on.
pub fn note_created(node: NodeId) {
    if AUTO_FOLLOW.with(|m| m.get()) {
        set_spotlight(node);
    }
}

/// The agent has gone idle — clear the live label and any lingering glow.
pub fn idle() {
    CURRENT.with(|m| m.set_neq(None));
    SPOTLIGHT.with(|m| m.set_neq(None));
    SPOTLIGHT_GEN.with(|g| g.set(g.get().wrapping_add(1)));
}

// ---------------------------------------------------------------------------
// Internals.
// ---------------------------------------------------------------------------

/// Jump the canvas to `sample`. Uses `open_sample`, which sets the view to the
/// sample's kind too — so following an Arrangement opens the arranger, not just
/// the Sounds canvas. When the sample is *already* active, still align the view:
/// an Arrangement created over MCP becomes active while the body is on Sounds,
/// and its timeline edits would otherwise be invisible.
fn follow_to(sample: SampleId) {
    let ctrl = controller();
    if ctrl.active_sample() == sample {
        if let Some(kind) = ctrl.sample_kind(sample) {
            if ctrl.view.get() != kind {
                ctrl.view.set(kind);
            }
        }
        return;
    }
    ctrl.open_sample(sample);
}

/// Light a node's spotlight glow and arm a timeout to fade it (unless a newer
/// spotlight supersedes it first).
fn set_spotlight(node: NodeId) {
    SPOTLIGHT.with(|m| m.set(Some(node)));
    let generation = SPOTLIGHT_GEN.with(|g| {
        let n = g.get().wrapping_add(1);
        g.set(n);
        n
    });
    spawn_local(async move {
        gloo_timers::future::TimeoutFuture::new(SPOTLIGHT_MS).await;
        if SPOTLIGHT_GEN.with(|g| g.get()) == generation {
            SPOTLIGHT.with(|m| m.set_neq(None));
        }
    });
}

/// Append a feed line (newest first), dropping the oldest past [`FEED_MAX`].
fn push_feed(text: String) {
    FEED.with(|f| {
        let mut v = f.lock_mut();
        v.insert_cloned(0, text);
        while v.len() > FEED_MAX {
            let last = v.len() - 1;
            v.remove(last);
        }
    });
}

/// What one served request should surface: a human `label`, an optional sample to
/// follow the canvas to, and an optional existing node to spotlight.
struct ActionInfo {
    label: String,
    follow: Option<SampleId>,
    spotlight: Option<NodeId>,
}

fn plain(label: impl Into<String>) -> Option<ActionInfo> {
    Some(ActionInfo {
        label: label.into(),
        follow: None,
        spotlight: None,
    })
}

fn at_node(label: impl Into<String>, node: NodeId) -> Option<ActionInfo> {
    Some(ActionInfo {
        label: label.into(),
        follow: None,
        spotlight: Some(node),
    })
}

/// Map a served request to its surfaced action (or `None` to stay silent).
fn describe(req: &Request) -> Option<ActionInfo> {
    match req {
        Request::Dispatch(cmd) => describe_cmd(cmd),
        Request::DispatchBatch(cmds) => {
            let mut described = cmds.iter().filter_map(describe_cmd);
            let first = described.next()?;
            // One meaningful command → describe it; several → summarize the burst.
            match described.count() {
                0 => Some(first),
                rest => plain(format!("Applying {} changes", rest + 1)),
            }
        }
        Request::Play => plain("Playing"),
        Request::Stop => plain("Stopping"),
        Request::SetActiveSample { sample } => Some(ActionInfo {
            label: format!("Opening {}", sample_label(*sample)),
            follow: Some(*sample),
            spotlight: None,
        }),
        Request::RenderWav { sample, .. } => Some(ActionInfo {
            label: format!("Rendering {}", opt_sample_label(*sample)),
            follow: *sample,
            spotlight: None,
        }),
        Request::AttachWasm { node, label, .. } => at_node(
            if label.is_empty() {
                "Compiling DSP module".to_string()
            } else {
                format!("Compiling {label}")
            },
            *node,
        ),
        Request::LoadAudio { node, label, .. } => at_node(
            format!(
                "Loading {}",
                label.clone().unwrap_or_else(|| "audio".to_string())
            ),
            *node,
        ),
        Request::ImportSamples { .. } => plain("Importing samples"),
        Request::Query(q) => describe_query(q),
    }
}

fn describe_cmd(cmd: &EditorCommand) -> Option<ActionInfo> {
    use EditorCommand as C;
    match cmd {
        C::AddNode { kind, .. } => plain(format!("Adding {}", kind_label(kind))),
        C::RemoveNode { .. } => plain("Removing a node"),
        C::CloneNode { .. } => plain("Duplicating a node"),
        C::SetField { id, key, .. } => at_node(format!("Setting {key}"), *id),
        C::SetAutomation { id, param, .. } => at_node(format!("Automating {param}"), *id),
        C::Connect { to, .. } => at_node("Connecting nodes", *to),
        C::Modulate { to, .. } => at_node("Adding modulation", *to),
        C::Bind { to, .. } => at_node("Binding the sequencer", *to),
        C::Disconnect { .. } => plain("Removing a wire"),
        C::EditSong { node, op } => at_node(song_op_label(op), *node),
        C::EditControl { node, .. } => at_node("Editing automation", *node),
        // Arrangement ops edit the *active* sample — follow to it so the arranger
        // view opens (the op is invisible from the Sounds canvas otherwise).
        C::EditArrange { op } => Some(ActionInfo {
            label: arrange_op_label(op).to_string(),
            follow: Some(controller().active_sample()),
            spotlight: None,
        }),
        C::Bounce { sample, .. } => Some(ActionInfo {
            label: format!("Bouncing {}", sample_label(*sample)),
            follow: Some(*sample),
            spotlight: None,
        }),
        C::AddSample { kind } => plain(match kind {
            SampleKind::Sound => "Creating a new sound",
            SampleKind::Arrangement => "Creating a new arrangement",
        }),
        C::RemoveSample { .. } => plain("Deleting a sample"),
        C::CloneSample { .. } => plain("Duplicating a sample"),
        C::RenameSample { name, .. } => plain(format!("Renaming to \u{201c}{name}\u{201d}")),
        C::SetSampleNotes { .. } => plain("Updating notes"),
        C::SetRoot { .. } => plain("Setting the main sound"),
        C::AddBoundary { .. } => plain("Adding a port"),
        C::AddSampleRef { sample, .. } => plain(format!("Placing {}", sample_label(*sample))),
        C::SetSampleRef { node, .. } => at_node("Repointing a sub-sound", *node),
        C::RenameNode { id, label } => at_node(
            if label.is_empty() {
                "Renaming a node".to_string()
            } else {
                format!("Renaming to \u{201c}{label}\u{201d}")
            },
            *id,
        ),
        C::SetInputDefault { node, .. } => at_node("Setting an input default", *node),
        C::SetInputValue { node, .. } => at_node("Setting an input", *node),
        C::SetListener { .. } => plain("Moving the listener"),
        C::Encapsulate { .. } => plain("Grouping nodes"),
        C::Paste { .. } => plain("Pasting"),
        // Continuous / view-only gestures: narrating these would just spam.
        C::MoveNode { .. } | C::SelectNodes { .. } | C::ClearSelection | C::SetCamera { .. } => {
            None
        }
    }
}

fn describe_query(q: &EditorQuery) -> Option<ActionInfo> {
    // Only the offline-render readbacks are worth surfacing (they take real time);
    // plain snapshot/list reads stay silent so the feed isn't drowned in polling.
    match q {
        EditorQuery::WavStats { sample, .. } | EditorQuery::Waveform { sample, .. } => {
            plain(format!("Analyzing {}", opt_sample_label(*sample)))
        }
        _ => None,
    }
}

fn song_op_label(op: &SongOp) -> &'static str {
    match op {
        SongOp::AddNote { .. } => "Adding a note",
        SongOp::RemoveNote { .. } => "Removing a note",
        SongOp::UpdateNote { .. } => "Editing a note",
        SongOp::SetTrackEvents { .. } => "Writing a pattern",
        SongOp::AddTrack => "Adding a track",
        SongOp::SetBpm(_) => "Setting the tempo",
        _ => "Editing the sequence",
    }
}

fn arrange_op_label(op: &ArrangeOp) -> &'static str {
    match op {
        ArrangeOp::AddClip { .. } | ArrangeOp::PasteClip { .. } | ArrangeOp::PasteClips { .. } => {
            "Placing a clip"
        }
        ArrangeOp::AddTrack => "Adding a track",
        ArrangeOp::SetBpm(_) => "Setting the tempo",
        ArrangeOp::MoveClip { .. }
        | ArrangeOp::ResizeClip { .. }
        | ArrangeOp::StretchClip { .. }
        | ArrangeOp::TrimStart { .. }
        | ArrangeOp::SetClipOffset { .. } => "Adjusting a clip",
        ArrangeOp::RemoveClip { .. } | ArrangeOp::Clear => "Removing clips",
        _ => "Editing the arrangement",
    }
}

/// `"“Bass”"`-style label for a sample id (falls back gracefully).
fn sample_label(id: SampleId) -> String {
    controller()
        .sample_name(id)
        .map(|n| format!("\u{201c}{n}\u{201d}"))
        .unwrap_or_else(|| "a sound".to_string())
}

/// Like [`sample_label`] but for an optional id (`None` = the project root).
fn opt_sample_label(sample: Option<SampleId>) -> String {
    match sample {
        Some(id) => sample_label(id),
        None => "the main sound".to_string(),
    }
}

/// Map a created object's id back to its surface for [`note_created`].
pub fn created_node_id(resp: &Response) -> Option<NodeId> {
    match resp {
        Response::Created { id } => id.parse().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Storage helpers: per-tab keyed localStorage (see module docs).
// ---------------------------------------------------------------------------

fn local_storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

/// This tab's session id, minted on first use and kept in `sessionStorage` so it
/// survives reloads of this tab but is never shared with other tabs. Synchronous,
/// so it's always resolvable by the time the lazy settings thread-locals init.
fn tab_id() -> Option<String> {
    let storage = web_sys::window().and_then(|w| w.session_storage().ok().flatten())?;
    if let Ok(Some(id)) = storage.get_item(KEY_TAB_ID) {
        if !id.is_empty() {
            return Some(id);
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = storage.set_item(KEY_TAB_ID, &id);
    Some(id)
}

/// Read a setting: this tab's own key first, then the bare global key (the seed a
/// new tab inherits), then `default`.
fn load_setting(key: &str, default: bool) -> bool {
    let Some(s) = local_storage() else {
        return default;
    };
    let per_tab = tab_id().and_then(|id| s.get_item(&format!("{key}.{id}")).ok().flatten());
    per_tab
        .or_else(|| s.get_item(key).ok().flatten())
        .map(|v| v == "1")
        .unwrap_or(default)
}

/// Write a setting to this tab's own key, and to the bare global key as the seed
/// future tabs inherit. Other tabs' keys are untouched, so they never stomp.
fn save_setting(key: &str, value: bool) {
    if let Some(s) = local_storage() {
        let v = if value { "1" } else { "0" };
        if let Some(id) = tab_id() {
            let _ = s.set_item(&format!("{key}.{id}"), v);
        }
        let _ = s.set_item(key, v);
    }
}

//! The serializable read-back structures: a node's [`NodeLayout`], the full
//! [`EditorSnapshot`] (graph + view-state), and the on-disk [`EditorProject`].
//!
//! These are pure data over `awsm_audio_schema`. The `impl EditorController`
//! blocks that *build* them (`snapshot`, `to_project`, …) stay in the editor
//! crate — they reference live controller internals.

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{Arrangement, Graph, NodeId, SampleLibrary};

/// World position of one node — the layout the schema deliberately omits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeLayout {
    pub id: NodeId,
    pub x: f64,
    pub y: f64,
}

/// A complete, serializable view of the editor: the audio graph plus the
/// view-state needed to reconstruct the canvas exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorSnapshot {
    pub graph: Graph,
    pub layout: Vec<NodeLayout>,
    pub pan_x: f64,
    pub pan_y: f64,
    pub zoom: f64,
    pub selection: Vec<NodeId>,
    /// The active sample's Arrangement, if it is one — so undo/redo covers
    /// timeline edits, which don't live on the node canvas. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrangement: Option<Arrangement>,
}

fn one() -> f64 {
    1.0
}

/// The on-disk editor *project*: the portable [`SampleLibrary`] (graph + embedded
/// assets the player consumes) plus editor-only extras (node layout + camera) so
/// reopening restores the canvas exactly. A bare `SampleLibrary` (e.g. an
/// example) also opens — it just gets auto-laid-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorProject {
    pub library: SampleLibrary,
    #[serde(default)]
    pub layout: Vec<NodeLayout>,
    #[serde(default)]
    pub pan_x: f64,
    #[serde(default)]
    pub pan_y: f64,
    #[serde(default = "one")]
    pub zoom: f64,
}

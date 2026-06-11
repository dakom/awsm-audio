//! Edges of the audio graph. WebAudio lets a node output feed either another
//! node's audio input *or* an [`AudioParam`](crate::AudioParam) (modulation),
//! and ports are addressed by integer index (`connect(dest, output, input)`).
//! [`Connection`] models all of that, plus the sample-boundary inlets/outlets
//! that make a graph composable.

use serde::{Deserialize, Serialize};

use crate::ids::{NodeId, PortId};
use crate::param::ParamId;

/// Stable identity of one sequencer output (a "sound" or a control lane), e.g.
/// `"t0"` (a melodic track), `"t2:n36"` (a drum note), or `"cutoff"` (a control
/// lane). Wires from a sequencer bind to this — never a port index — so adding,
/// removing, and reordering sounds never silently re-routes anything.
pub type SeqKey = String;

/// What a connection *source* emits — the upstream signal kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Emit {
    /// Sample-rate audio (a node output or an inlet boundary).
    Audio,
    /// A note/trigger stream from a Note Sequencer.
    Trigger,
    /// A control/automation stream from a Control Sequencer.
    Control,
}

/// What a connection *sink* accepts — the downstream port kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accept {
    /// An audio input (or an outlet boundary).
    Audio,
    /// An automatable [`AudioParam`](crate::AudioParam).
    Param,
    /// A "play this node as an instrument" trigger inlet.
    Trigger,
}

/// The wiring rules, as one small matrix. Audio can drive an audio input *or*
/// modulate a param; a note trigger can only fire a trigger inlet; a control
/// stream can only drive a param. Everything else is illegal.
pub fn can_connect(emit: Emit, accept: Accept) -> bool {
    matches!(
        (emit, accept),
        (Emit::Audio, Accept::Audio)
            | (Emit::Audio, Accept::Param)
            | (Emit::Trigger, Accept::Trigger)
            | (Emit::Control, Accept::Param)
    )
}

/// A directed edge from a signal source to a signal sink.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// Stable wire id — the same id the editor's `disconnect` command expects, so
    /// a snapshot's wires can be removed surgically (without tearing down an
    /// endpoint node). Present in editor snapshots; omitted (and `None`) in the
    /// portable saved document, where wires are pure `from`/`to` edges. When a
    /// loaded document *does* carry one, the editor honours it so the wire keeps
    /// a stable identity across save/load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "schemars", schemars(with = "Option<String>"))]
    pub id: Option<uuid::Uuid>,
    pub from: ConnectionSource,
    pub to: ConnectionSink,
}

impl Connection {
    /// Node-output → node-input on output/input index 0 (the common case).
    pub fn node_to_node(from: NodeId, to: NodeId) -> Self {
        Self {
            id: None,
            from: ConnectionSource::NodeOutput {
                node: from,
                output: 0,
            },
            to: ConnectionSink::NodeInput { node: to, input: 0 },
        }
    }

    /// Node-output → another node's param (modulation), output index 0.
    pub fn node_to_param(from: NodeId, to: NodeId, param: impl Into<ParamId>) -> Self {
        Self {
            id: None,
            from: ConnectionSource::NodeOutput {
                node: from,
                output: 0,
            },
            to: ConnectionSink::NodeParam {
                node: to,
                param: param.into(),
            },
        }
    }
}

/// The upstream end of a [`Connection`] — something that emits a signal.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "from")]
pub enum ConnectionSource {
    /// One of a node's audio outputs (index 0 unless it's a splitter/worklet).
    NodeOutput {
        node: NodeId,
        #[serde(default)]
        output: u32,
    },
    /// The graph's own inlet — the signal entering this sample from its host.
    Inlet { port: PortId },
    /// A sequencer output, addressed by stable identity. Carries no audio — it's
    /// a *trigger/control binding* the scheduler reads, not a pipe in the audio
    /// graph. A note-sequencer's `SeqOut` drives a [`Trigger`](ConnectionSink::Trigger);
    /// a control-sequencer's drives a [`NodeParam`](ConnectionSink::NodeParam).
    SeqOut { node: NodeId, key: SeqKey },
}

/// The downstream end of a [`Connection`] — something that receives a signal.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "to")]
pub enum ConnectionSink {
    /// One of a node's audio inputs (index 0 unless it's a merger/worklet).
    NodeInput {
        node: NodeId,
        #[serde(default)]
        input: u32,
    },
    /// A node's automatable param — i.e. modulation, `connect(param)`.
    NodeParam { node: NodeId, param: ParamId },
    /// The graph's own outlet — the signal leaving this sample to its host.
    Outlet { port: PortId },
    /// "Play this node as an instrument." The downstream end of a note-sequencer
    /// trigger wire; the scheduler spawns a voice of `node` per event.
    /// Not an audio connection.
    Trigger { node: NodeId },
}

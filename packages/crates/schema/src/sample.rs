//! The [`Sample`] — a reusable, triggerable unit of audio. It wraps a [`Graph`]
//! and publishes the surface a host uses to play and embed it: its named
//! inlets/outlets (declared on the graph — its inputs/outputs) and a
//! [`TriggerSpec`].

use serde::{Deserialize, Serialize};

use crate::arrangement::{Arrangement, Bounce};
use crate::graph::Graph;
use crate::ids::{NodeId, SampleId};

/// Which editor surface a sample is edited on. There is no instrument/sequence
/// distinction: a **Sound** is any node graph (a synth patch, an FX chain, or a
/// full sequencer-driven song — what it does is determined by its content and
/// the typed port matrix, not a category). An **Arrangement** is the DAW
/// timeline surface, whose data lives in [`Sample::arrangement`].
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleKind {
    /// A node graph, edited on the canvas. The default. (Older documents may say
    /// `instrument` or `sequence`; both load as a Sound.)
    #[default]
    #[serde(alias = "instrument", alias = "sequence")]
    Sound,
    /// A DAW-style timeline (Arrange surface): clips on tracks. The data lives in
    /// [`Sample::arrangement`]; the `graph` is unused.
    Arrangement,
}

/// A named, self-contained audio unit.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    pub id: SampleId,
    pub name: String,
    /// Whether this is an Instrument (Instruments view) or a Sequence
    /// (arrangement, Sequences view).
    #[serde(default)]
    pub kind: SampleKind,
    pub graph: Graph,
    /// How the sample responds to note-on / note-off.
    #[serde(default)]
    pub trigger: TriggerSpec,
    /// Timeline data — meaningful only when `kind == Arrangement` (otherwise
    /// empty and skipped on save).
    #[serde(default, skip_serializing_if = "Arrangement::is_empty")]
    pub arrangement: Arrangement,
    /// The sample's rendered audio, if it has been bounced (Sounds only). Lets
    /// arrangements play it as an audio clip; goes stale when the graph changes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounce: Option<Bounce>,
    /// Free-form working notes ("impact variant", "keeper", "needs shorter
    /// tail") — annotation metadata, never interpreted by the player.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

impl Sample {
    /// An empty Sound (node graph) with a fresh id and the given name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: SampleId::new(),
            name: name.into(),
            kind: SampleKind::Sound,
            graph: Graph::default(),
            trigger: TriggerSpec::default(),
            arrangement: Arrangement::default(),
            bounce: None,
            notes: String::new(),
        }
    }

    /// An empty timeline arrangement with a fresh id and the given name.
    pub fn new_arrangement(name: impl Into<String>) -> Self {
        Self {
            kind: SampleKind::Arrangement,
            ..Self::new(name)
        }
    }
}

/// How a sample is played as an instrument. Note-on starts the listed source
/// nodes; note-off lets them run for `release` seconds (envelope tail) before
/// stopping. Envelopes, velocity, and polyphony are layered on later via
/// [`SampleParam`] automation — this captures the minimal gate behavior.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TriggerSpec {
    /// Source nodes (oscillators, buffer sources, …) started on note-on. Empty
    /// means "every source node in the graph".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<NodeId>,
    /// Release tail in seconds applied after note-off before sources stop.
    #[serde(default)]
    pub release: f64,
}

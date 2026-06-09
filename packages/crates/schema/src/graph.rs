//! The node graph: a bag of [`Node`]s, the [`Connection`]s between them, and
//! the named boundary ports ([`PortDecl`]) that let the whole graph be wired
//! into a parent as a single composable unit.

use serde::{Deserialize, Serialize};

use crate::connection::Connection;
use crate::ids::{NodeId, PortId};
use crate::nodes::Node;

/// A directed audio graph. Order of `nodes`/`connections` is preserved but not
/// semantically meaningful; identity is by id.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<Node>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<Connection>,
    /// Boundary inputs — signals the host feeds in. Their order defines the
    /// input indices a parent [`SampleRef`](crate::SampleRef) sees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inlets: Vec<PortDecl>,
    /// Boundary outputs — signals the sample emits. Their order defines the
    /// output indices a parent [`SampleRef`](crate::SampleRef) sees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlets: Vec<PortDecl>,
}

impl Graph {
    /// Look up a node by id.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// Add a node and return its id.
    pub fn push_node(&mut self, node: Node) -> NodeId {
        let id = node.id;
        self.nodes.push(node);
        id
    }

    /// Add a connection.
    pub fn connect(&mut self, connection: Connection) {
        self.connections.push(connection);
    }

    /// The signal a source endpoint emits (see [`Emit`](crate::connection::Emit)).
    /// `None` if it references a node not in this graph (boundary endpoints
    /// always resolve).
    pub fn source_emit(
        &self,
        src: &crate::connection::ConnectionSource,
    ) -> Option<crate::connection::Emit> {
        use crate::connection::{ConnectionSource, Emit};
        use crate::nodes::NodeKind;
        match src {
            // Audio outputs are always audio; sequencers publish via `SeqOut`.
            ConnectionSource::NodeOutput { .. } | ConnectionSource::Inlet { .. } => {
                Some(Emit::Audio)
            }
            ConnectionSource::SeqOut { node, .. } => self.node(*node).map(|n| match n.kind {
                NodeKind::ControlSequencer(_) => Emit::Control,
                _ => Emit::Trigger,
            }),
        }
    }

    /// The port kind a sink endpoint accepts (see
    /// [`Accept`](crate::connection::Accept)). `None` if it references a node not
    /// in this graph (boundary endpoints always resolve).
    pub fn sink_accept(
        &self,
        sink: &crate::connection::ConnectionSink,
    ) -> Option<crate::connection::Accept> {
        use crate::connection::{Accept, ConnectionSink};
        match sink {
            ConnectionSink::NodeInput { node, .. } => self.node(*node).map(|_| Accept::Audio),
            ConnectionSink::NodeParam { node, .. } => self.node(*node).map(|_| Accept::Param),
            ConnectionSink::Trigger { node } => self.node(*node).map(|_| Accept::Trigger),
            ConnectionSink::Outlet { .. } => Some(Accept::Audio),
        }
    }

    /// Whether a connection's endpoints are signal-compatible (see
    /// [`can_connect`](crate::connection::can_connect)). Returns `true` when
    /// either endpoint is unresolved — dangling references are a *separate*
    /// defect that [`validate`](crate::SampleLibrary::validate) reports on its own.
    pub fn can_wire(&self, c: &Connection) -> bool {
        match (self.source_emit(&c.from), self.sink_accept(&c.to)) {
            (Some(emit), Some(accept)) => crate::connection::can_connect(emit, accept),
            _ => true,
        }
    }

    /// A copy of this graph with every oscillator transposed by `semitones`
    /// (frequency value + frequency automation scaled by `2^(semitones/12)`).
    /// Turns a patch into a pitched instrument without mutating the document —
    /// used by live keyboard play and the song sequencer (one per note).
    pub fn transposed(&self, semitones: i32) -> Graph {
        use crate::nodes::NodeKind;
        use crate::param::AutomationEvent;
        if semitones == 0 {
            return self.clone();
        }
        let factor = 2f32.powf(semitones as f32 / 12.0);
        let mut g = self.clone();
        for node in &mut g.nodes {
            if let NodeKind::Oscillator(o) = &mut node.kind {
                o.frequency.value *= factor;
                for ev in &mut o.frequency.automation {
                    match ev {
                        AutomationEvent::SetValue { value, .. }
                        | AutomationEvent::LinearRamp { value, .. }
                        | AutomationEvent::ExponentialRamp { value, .. } => *value *= factor,
                        AutomationEvent::SetTarget { target, .. } => *target *= factor,
                        _ => {}
                    }
                }
            }
        }
        g
    }
}

/// Declares one boundary port (inlet or outlet) of a [`Graph`].
///
/// An **inlet** is a named input. A parent can give it a value (which sets the
/// inner [`AudioParam`](crate::AudioParam) it drives — like editing that field)
/// or wire a signal into it (which *adds* to the param, the native WebAudio
/// modulation behavior). `default` is the value when nothing is set/wired.
/// (Outlets ignore `default`.)
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortDecl {
    pub id: PortId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub default: f32,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl PortDecl {
    pub fn new(id: impl Into<PortId>) -> Self {
        Self {
            id: id.into(),
            label: None,
            default: 0.0,
        }
    }
}

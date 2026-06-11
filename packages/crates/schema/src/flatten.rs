//! Flatten a composite [`SampleLibrary`] into one plain [`Graph`] the player can
//! instantiate, by inlining every [`Sample`](crate::Sample)-reference node.
//!
//! Each [`SampleRef`](crate::SampleRef) node is replaced by a fresh copy of the
//! referenced sample's graph (all node ids remapped, so a sample can be embedded
//! many times). The sub-graph's boundary ports are spliced into the parent:
//! whatever fed the ref node's input *i* now drives the sub-graph's inlet *i*,
//! and whatever the sub-graph's outlet *j* emits now feeds the ref node's
//! output *j* consumers. Nesting is resolved by repeating until no refs remain.

use std::collections::HashMap;

use crate::connection::{Connection, ConnectionSink, ConnectionSource};
use crate::graph::Graph;
use crate::ids::{NodeId, PortId, SampleId};
use crate::library::SampleLibrary;
use crate::nodes::{ConstantSourceNode, Node, NodeKind};
use crate::param::AudioParam;

impl SampleLibrary {
    /// Flatten the root sample (inlining all nested samples) into one graph.
    pub fn flatten_root(&self) -> Option<Graph> {
        self.root
            .or_else(|| self.samples.first().map(|s| s.id))
            .map(|r| flatten(self, r))
    }
}

/// Flatten `root` within `lib` into a ref-free [`Graph`].
pub fn flatten(lib: &SampleLibrary, root: SampleId) -> Graph {
    let Some(sample) = lib.sample(root) else {
        return Graph::default();
    };
    let mut g = sample.graph.clone();
    // The top-level graph has no host, so its own boundary ports become baked-in
    // values: capture the inlet defaults first, then drop the port declarations.
    let top_inlets: Vec<(PortId, f32)> =
        g.inlets.iter().map(|p| (p.id.clone(), p.default)).collect();
    g.inlets.clear();
    g.outlets.clear();

    // Inline one ref node per pass until none remain (nested refs surface as
    // new ref nodes after their parent is inlined). The guard bounds pathological
    // input (cycles are also caught by `validate`).
    let mut guard = 0;
    while let Some(idx) = g
        .nodes
        .iter()
        .position(|n| matches!(n.kind, NodeKind::Sample(_)))
    {
        guard += 1;
        if guard > 512 {
            break;
        }
        inline_one(lib, &mut g, idx);
    }

    apply_top_inlets(&mut g, &top_inlets);
    g
}

/// Resolve the root sample's own inlet-sourced connections using the inlet
/// defaults: a `→ param` inlet sets that param's value; a `→ audio in` inlet
/// becomes a constant source feeding the input. Without this the root's inputs
/// would be inert when the sample is played at the top level (it has no host to
/// drive them), and MIDI/standalone tweaks of an input would do nothing.
fn apply_top_inlets(g: &mut Graph, top_inlets: &[(PortId, f32)]) {
    let value_of = |port: &PortId| -> f32 {
        top_inlets
            .iter()
            .find(|(id, _)| id == port)
            .map(|(_, v)| *v)
            .unwrap_or(0.0)
    };
    // Collect inlet-sourced connections, then strip them from the graph.
    let mut pending: Vec<(PortId, ConnectionSink)> = Vec::new();
    g.connections.retain(|c| {
        if let ConnectionSource::Inlet { port } = &c.from {
            pending.push((port.clone(), c.to.clone()));
            false
        } else {
            true
        }
    });
    for (port, sink) in pending {
        let v = value_of(&port);
        match sink {
            ConnectionSink::NodeParam { node, param } => {
                set_inlined_param(g, node, &param.0, v);
            }
            ConnectionSink::NodeInput { node, input } => {
                if v != 0.0 {
                    let k = Node::new(NodeKind::ConstantSource(ConstantSourceNode {
                        offset: AudioParam::new(v),
                    }));
                    let kid = k.id;
                    g.nodes.push(k);
                    g.connections.push(Connection {
                        id: None,
                        from: ConnectionSource::NodeOutput {
                            node: kid,
                            output: 0,
                        },
                        to: ConnectionSink::NodeInput { node, input },
                    });
                }
            }
            ConnectionSink::Outlet { .. } => {}
            // Sequencer triggers don't carry signal — nothing to splice.
            ConnectionSink::Trigger { .. } => {}
        }
    }
}

fn remap_source(src: &ConnectionSource, idmap: &HashMap<NodeId, NodeId>) -> ConnectionSource {
    match src {
        ConnectionSource::NodeOutput { node, output } => ConnectionSource::NodeOutput {
            node: idmap.get(node).copied().unwrap_or(*node),
            output: *output,
        },
        other => other.clone(),
    }
}

fn inline_one(lib: &SampleLibrary, g: &mut Graph, idx: usize) {
    let node = g.nodes.remove(idx);
    let sid = node.id;
    let NodeKind::Sample(sref) = &node.kind else {
        return;
    };
    let Some(sample) = lib.sample(sref.sample) else {
        return; // dangling reference → just drop the node
    };
    let sub = &sample.graph;

    // Fresh ids for every sub-graph node (so a sample can be inlined N times).
    let idmap: HashMap<NodeId, NodeId> = sub.nodes.iter().map(|n| (n.id, NodeId::new())).collect();
    let inlet_ids: Vec<PortId> = sub.inlets.iter().map(|p| p.id.clone()).collect();
    let outlet_ids: Vec<PortId> = sub.outlets.iter().map(|p| p.id.clone()).collect();

    // Detach the parent's connections touching the ref node, recording what fed
    // each input and what consumed each output.
    let mut into_input: HashMap<u32, ConnectionSource> = HashMap::new();
    let mut out_consumers: Vec<(u32, ConnectionSink)> = Vec::new();
    g.connections.retain(|c| {
        let mut touches = false;
        if let ConnectionSink::NodeInput { node, input } = &c.to {
            if *node == sid {
                into_input.insert(*input, c.from.clone());
                touches = true;
            }
        }
        if let ConnectionSource::NodeOutput { node, output } = &c.from {
            if *node == sid {
                out_consumers.push((*output, c.to.clone()));
                touches = true;
            }
        }
        !touches
    });

    // The parent signal wired into an inlet (by name), if any.
    let wired_inlet = |port: &PortId| -> Option<ConnectionSource> {
        inlet_ids
            .iter()
            .position(|p| p == port)
            .and_then(|i| into_input.get(&(i as u32)).cloned())
    };
    // The per-instance value for an inlet: a SampleRef input override, else the
    // inlet's declared default.
    let inlet_value = |port: &PortId| -> f32 {
        sref.inputs
            .iter()
            .find(|iv| iv.port == *port)
            .map(|iv| iv.value)
            .or_else(|| sub.inlets.iter().find(|p| p.id == *port).map(|p| p.default))
            .unwrap_or(0.0)
    };
    // For each outlet, the internal source feeding it (a wired inlet passes
    // straight through to whatever consumes the ref's output).
    let mut outlet_source: HashMap<u32, ConnectionSource> = HashMap::new();
    for c in &sub.connections {
        if let ConnectionSink::Outlet { port } = &c.to {
            if let Some(j) = outlet_ids.iter().position(|p| p == port) {
                let src = match &c.from {
                    ConnectionSource::NodeOutput { .. } => Some(remap_source(&c.from, &idmap)),
                    ConnectionSource::Inlet { port } => wired_inlet(port),
                    ConnectionSource::SeqOut { .. } => None, // not an audio source
                };
                if let Some(src) = src {
                    outlet_source.insert(j as u32, src);
                }
            }
        }
    }

    // Splice in the sub-graph nodes (remapped).
    for n in &sub.nodes {
        let mut nn = n.clone();
        nn.id = idmap[&n.id];
        g.nodes.push(nn);
    }

    // Splice the sub-graph's connections, resolving inlets:
    //  - inlet → param: a wired signal modulates it (native WebAudio: sums with
    //    the param's value); a typed input value sets the param's value.
    //  - inlet → audio input: a wired signal is spliced; a typed value becomes
    //    a constant source feeding the input.
    for c in &sub.connections {
        match &c.to {
            ConnectionSink::Outlet { .. } => continue, // rewired below
            // Sequencing wires don't appear inside an instrument's graph; ignore.
            ConnectionSink::Trigger { .. } => continue,
            ConnectionSink::NodeInput { node, input } => {
                let to_node = idmap[node];
                let to = ConnectionSink::NodeInput {
                    node: to_node,
                    input: *input,
                };
                match &c.from {
                    ConnectionSource::NodeOutput { .. } => g.connections.push(Connection {
                        id: None,
                        from: remap_source(&c.from, &idmap),
                        to,
                    }),
                    ConnectionSource::Inlet { port } => {
                        if let Some(from) = wired_inlet(port) {
                            g.connections.push(Connection { id: None, from, to });
                        } else {
                            let v = inlet_value(port);
                            if v != 0.0 {
                                let k = Node::new(NodeKind::ConstantSource(ConstantSourceNode {
                                    offset: AudioParam::new(v),
                                }));
                                let kid = k.id;
                                g.nodes.push(k);
                                g.connections.push(Connection {
                                    id: None,
                                    from: ConnectionSource::NodeOutput {
                                        node: kid,
                                        output: 0,
                                    },
                                    to,
                                });
                            }
                        }
                    }
                    ConnectionSource::SeqOut { .. } => {} // not an audio source
                }
            }
            ConnectionSink::NodeParam { node, param } => {
                let to_node = idmap[node];
                match &c.from {
                    ConnectionSource::NodeOutput { .. } => g.connections.push(Connection {
                        id: None,
                        from: remap_source(&c.from, &idmap),
                        to: ConnectionSink::NodeParam {
                            node: to_node,
                            param: param.clone(),
                        },
                    }),
                    ConnectionSource::Inlet { port } => {
                        if let Some(from) = wired_inlet(port) {
                            // A wired signal modulates the param — native
                            // WebAudio: it sums with the param's value.
                            g.connections.push(Connection {
                                id: None,
                                from,
                                to: ConnectionSink::NodeParam {
                                    node: to_node,
                                    param: param.clone(),
                                },
                            });
                        } else {
                            // A typed input value sets the param's value.
                            set_inlined_param(g, to_node, &param.0, inlet_value(port));
                        }
                    }
                    ConnectionSource::SeqOut { .. } => {} // not an audio source
                }
            }
        }
    }
    // Rewire the ref node's output consumers to the outlet sources.
    for (j, sink) in out_consumers {
        if let Some(src) = outlet_source.get(&j) {
            g.connections.push(Connection {
                id: None,
                from: src.clone(),
                to: sink,
            });
        }
    }
}

/// Set a freshly-inlined node's named param value (used to apply an inlet's
/// typed value, or to zero a param's base before a "set" signal drives it).
fn set_inlined_param(g: &mut Graph, id: NodeId, param: &str, value: f32) {
    if let Some(n) = g.nodes.iter_mut().find(|n| n.id == id) {
        n.kind.set_param_value(param, value);
    }
}

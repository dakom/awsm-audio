//! [`Clipboard`] — the serde-friendly copy/paste payload (also the `Paste`
//! command's argument, so a paste is an MCP-drivable command).

use serde::{Deserialize, Serialize};

use awsm_audio_schema::NodeKind;

use crate::node::ConnSink;

/// A copy/paste payload (also the `Paste` command's argument, so a paste is an
/// MCP-drivable command): nodes (kind + label + relative position) and the wires
/// among them (endpoints are indices into `nodes`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Clipboard {
    pub nodes: Vec<(NodeKind, String, f64, f64)>,
    pub wires: Vec<(usize, u32, usize, ConnSink)>,
}

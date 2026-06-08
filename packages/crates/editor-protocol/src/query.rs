//! The **read** half of the controller surface — the counterpart to
//! [`EditorCommand`](crate::EditorCommand). A serde-tagged query the
//! MCP/WebTransport transport (or a headless driver) sends to inspect editor
//! state; the controller answers with a [`QueryResult`].

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{Arrangement, SampleId, SampleKind};

use crate::snapshot::{EditorProject, EditorSnapshot};

/// A serde-tagged query an MCP/WebTransport transport (or a headless driver)
/// sends to inspect editor state; the controller answers with a [`QueryResult`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "query", content = "args")]
pub enum EditorQuery {
    /// The full editor snapshot (graph + layout + camera + selection + arrangement).
    Snapshot,
    /// The saveable project (library + layout + camera).
    Project,
    /// Every sample (id, name, kind, root/active flags).
    Samples,
    /// Every bounceable Sound with its bounce status + bounced duration.
    Assets,
    /// One Sound's bounce status.
    BounceStatus { sample: SampleId },
    /// The active sample's arrangement (if it is one).
    Arrangement,
    /// Live transport state (playing / peak / playhead / audio-context state).
    Transport,
    /// Cheap numeric stats of a Sound's offline render.
    WavStats {
        #[serde(default)]
        sample: Option<SampleId>,
    },
    /// A downsampled min/max envelope (`buckets` columns) of a Sound's render,
    /// so an agent can reason about the waveform shape in text.
    Waveform {
        #[serde(default)]
        sample: Option<SampleId>,
        buckets: u32,
    },
}

/// The answer to an [`EditorQuery`]. Serialized back to the caller; also
/// `Deserialize` so the native MCP server can decode it off the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "result", content = "data")]
pub enum QueryResult {
    Snapshot(Box<EditorSnapshot>),
    Project(Box<EditorProject>),
    Samples(Vec<SampleInfo>),
    Assets(Vec<AssetInfo>),
    BounceStatus(String),
    Arrangement(Option<Arrangement>),
    Transport(TransportInfo),
    WavStats(WavStats),
    Waveform(WaveformEnvelope),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleInfo {
    pub id: SampleId,
    pub name: String,
    pub kind: SampleKind,
    pub is_root: bool,
    pub is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    pub id: SampleId,
    pub name: String,
    /// `"none"` / `"clean"` / `"dirty"`.
    pub bounce: String,
    pub duration_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportInfo {
    pub playing: bool,
    pub peak: f32,
    pub playhead: f64,
    pub audio_state: String,
}

/// Cheap numeric stats of a Sound's offline render.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WavStats {
    pub duration_secs: f64,
    pub peak: f32,
    pub rms: f32,
    pub channels: u32,
    pub sample_rate: u32,
}

/// Per-bucket min/max of a mono-summed render, normalized to [-1, 1].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformEnvelope {
    pub sample_rate: u32,
    pub duration_secs: f64,
    /// `min[i] <= max[i]`, one pair per bucket, left-to-right in time.
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

//! The **read** half of the controller surface — the counterpart to
//! [`EditorCommand`](crate::EditorCommand). A serde-tagged query the
//! MCP/WebSocket transport (or a headless driver) sends to inspect editor
//! state; the controller answers with a [`QueryResult`].

use serde::{Deserialize, Serialize};

use awsm_audio_schema::{Arrangement, NodeId, NodeKind, SampleId, SampleKind};

use crate::snapshot::{EditorProject, EditorSnapshot};

/// A serde-tagged query an MCP/WebSocket transport (or a headless driver)
/// sends to inspect editor state; the controller answers with a [`QueryResult`].
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    /// The palette catalog: every creatable node kind with a ready-to-use default
    /// value and its editable field keys — so `add_node` / `set_field` need no
    /// schema knowledge. This is the discovery entry point for graph building.
    Catalog,
    /// The editable fields of one live node (key, control, current value, range,
    /// whether it's modulation-targetable). Covers worklet nodes whose params are
    /// discovered at runtime, so `set_field` keys are always discoverable.
    NodeFields { node: NodeId },
}

/// The answer to an [`EditorQuery`]. Serialized back to the caller; also
/// `Deserialize` so the native MCP server can decode it off the wire.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    Catalog(Vec<NodeKindInfo>),
    NodeFields(Vec<FieldInfo>),
}

/// One editable setting of a node — the keys/ranges `set_field` accepts. Mirrors
/// the editor's `fields` reflection so an agent can edit a node without knowing
/// its schema.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    /// The `set_field` key.
    pub key: String,
    /// Human label shown in the inspector.
    pub label: String,
    /// `"number"` | `"choice"` | `"bool"`.
    pub control: String,
    /// Current value for number/bool controls (bool is `0.0`/`1.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_num: Option<f64>,
    /// Current value for a `"choice"` control.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_text: Option<String>,
    /// Allowed values for a `"choice"` control.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// True if a signal can be wired to this field (a modulation inlet).
    pub modulatable: bool,
}

/// A creatable node kind, surfaced for discovery so an agent can `add_node`
/// without knowing the schema. Pass `kind` (the tag string) or `example` (the
/// full default value) to `add_node`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeKindInfo {
    /// The serde tag — e.g. `"oscillator"`, `"biquad_filter"`. Pass this string to
    /// `add_node`, or copy `example` verbatim.
    pub kind: String,
    /// Human label (e.g. `"Oscillator"`).
    pub label: String,
    /// Palette section (`"Sources"`, `"Effects"`, `"Sequencing"`, …).
    pub section: String,
    /// One-paragraph plain-language description of what this node does and when to
    /// reach for it (the editor's node-help text).
    pub description: String,
    /// MDN reference page for the underlying WebAudio interface (empty for the
    /// sequencer/composite kinds that have no direct WebAudio node).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mdn: String,
    /// A ready-to-use default value — the exact JSON `add_node`'s `kind` accepts,
    /// e.g. `{"kind":"oscillator","props":{…}}`.
    pub example: NodeKind,
    /// Editable field keys (`set_field` targets) with control type + current value.
    pub fields: Vec<FieldInfo>,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleInfo {
    pub id: SampleId,
    pub name: String,
    pub kind: SampleKind,
    pub is_root: bool,
    pub is_active: bool,
    /// Bounce state for a Sound: `"none"` / `"clean"` / `"dirty"`. `None` for an
    /// Arrangement (not bounceable). Mirrors `AssetInfo.bounce` so `list_samples`
    /// is a one-stop view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounce: Option<String>,
    /// Bounced duration in seconds, if this Sound has a bounce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    pub id: SampleId,
    pub name: String,
    /// `"none"` / `"clean"` / `"dirty"`.
    pub bounce: String,
    pub duration_secs: Option<f64>,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransportInfo {
    pub playing: bool,
    pub peak: f32,
    pub playhead: f64,
    pub audio_state: String,
}

/// Cheap numeric stats of a Sound's offline render.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WavStats {
    pub duration_secs: f64,
    pub peak: f32,
    pub rms: f32,
    pub channels: u32,
    pub sample_rate: u32,
}

impl WavStats {
    /// Compute stats over rendered PCM (`channels[ch][frame]`): peak = max abs
    /// across all channels; rms = sqrt(mean of squares); duration = frames / rate.
    /// Pure f32/f64 math — natively testable, no audio/DOM deps.
    pub fn from_pcm(channels: &[Vec<f32>], sample_rate: u32) -> Self {
        let frames = channels.iter().map(|c| c.len()).max().unwrap_or(0);
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut count = 0u64;
        for ch in channels {
            for &s in ch {
                peak = peak.max(s.abs());
                sum_sq += (s as f64) * (s as f64);
                count += 1;
            }
        }
        let rms = if count > 0 {
            (sum_sq / count as f64).sqrt() as f32
        } else {
            0.0
        };
        let duration_secs = if sample_rate > 0 {
            frames as f64 / sample_rate as f64
        } else {
            0.0
        };
        Self {
            duration_secs,
            peak,
            rms,
            channels: channels.len() as u32,
            sample_rate,
        }
    }
}

/// Per-bucket min/max of a mono-summed render, normalized to [-1, 1].
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformEnvelope {
    pub sample_rate: u32,
    pub duration_secs: f64,
    /// `min[i] <= max[i]`, one pair per bucket, left-to-right in time.
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}

impl WaveformEnvelope {
    /// Down-sample rendered PCM into `buckets` min/max columns over the channel
    /// mean (so it stays in [-1, 1]). Pure math — natively testable.
    pub fn from_pcm(channels: &[Vec<f32>], sample_rate: u32, buckets: u32) -> Self {
        let frames = channels.iter().map(|c| c.len()).max().unwrap_or(0);
        let n = channels.len().max(1) as f32;
        // Channel mean per frame.
        let mono: Vec<f32> = (0..frames)
            .map(|i| {
                let s: f32 = channels
                    .iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum();
                s / n
            })
            .collect();

        let buckets = buckets.max(1) as usize;
        let mut min = Vec::with_capacity(buckets);
        let mut max = Vec::with_capacity(buckets);
        for b in 0..buckets {
            let start = b * frames / buckets;
            let end = ((b + 1) * frames / buckets).clamp(start, frames);
            let slice = &mono[start..end];
            if slice.is_empty() {
                min.push(0.0);
                max.push(0.0);
            } else {
                let mut lo = f32::INFINITY;
                let mut hi = f32::NEG_INFINITY;
                for &s in slice {
                    lo = lo.min(s);
                    hi = hi.max(s);
                }
                min.push(lo.clamp(-1.0, 1.0));
                max.push(hi.clamp(-1.0, 1.0));
            }
        }
        let duration_secs = if sample_rate > 0 {
            frames as f64 / sample_rate as f64
        } else {
            0.0
        };
        Self {
            sample_rate,
            duration_secs,
            min,
            max,
        }
    }
}

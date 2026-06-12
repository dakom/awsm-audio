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
    /// The auto-computed render length for a Sound (and *why*), so the surprising
    /// duration rules are queryable before bouncing. `None` = the project root.
    RenderPlan {
        #[serde(default)]
        sample: Option<SampleId>,
    },
    /// The active sample's arrangement (if it is one).
    Arrangement,
    /// Per-track peak/rms of the active arrangement, each rendered in isolation
    /// (solo) — so an agent can see which stem is hot without rescaling blindly.
    ArrangementTrackStats,
    /// Live transport state (playing / peak / playhead / audio-context state).
    Transport,
    /// Cheap numeric stats of a Sound. `bounced = false` (default) renders the
    /// *live graph* fresh; `bounced = true` reports the *stored bounced asset*
    /// (errors "not yet bounced" if there is none). `duration_secs` sets the
    /// live-render window (ignored when `bounced`).
    WavStats {
        #[serde(default)]
        sample: Option<SampleId>,
        #[serde(default)]
        bounced: bool,
        #[serde(default)]
        duration_secs: Option<f64>,
    },
    /// A downsampled min/max envelope (`buckets` columns) of a Sound. Same
    /// `bounced` live-vs-stored choice as [`WavStats`](Self::WavStats).
    Waveform {
        #[serde(default)]
        sample: Option<SampleId>,
        buckets: u32,
        #[serde(default)]
        bounced: bool,
        #[serde(default)]
        duration_secs: Option<f64>,
    },
    /// The palette catalog: every creatable node kind with a ready-to-use default
    /// value and its editable field keys — so `add_node` / `set_field` need no
    /// schema knowledge. This is the discovery entry point for graph building.
    Catalog,
    /// The editable fields of one live node (key, control, current value, range,
    /// whether it's modulation-targetable). Covers worklet nodes whose params are
    /// discovered at runtime, so `set_field` keys are always discoverable.
    NodeFields { node: NodeId },
    /// Every modulatable/automatable param across the ACTIVE canvas, grouped by
    /// node — the graph-wide "what can I modulate or automate?" view (the
    /// per-node form is `NodeFields`).
    ModulationTargets,
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
    RenderPlan(RenderPlanInfo),
    Arrangement(Option<Arrangement>),
    ArrangementTrackStats(Vec<TrackStats>),
    Transport(TransportInfo),
    WavStats(WavStats),
    Waveform(WaveformEnvelope),
    Catalog(Vec<NodeKindInfo>),
    NodeFields(Vec<FieldInfo>),
    ModulationTargets(Vec<ModTargetInfo>),
}

/// One node's modulatable params (the graph-wide automation/modulation map).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModTargetInfo {
    pub node: NodeId,
    /// The node's display label (custom label, else its kind name).
    pub label: String,
    /// The kind tag (e.g. `"biquad_filter"`).
    pub kind: String,
    /// Param keys accepted by `set_automation` / `modulate` on this node.
    pub params: Vec<String>,
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
    /// Bounce state for a Sound: `"none"` / `"clean"` / `"stale"`. `None` for an
    /// Arrangement (not bounceable). Mirrors `AssetInfo.bounce` so `list_samples`
    /// is a one-stop view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bounce: Option<String>,
    /// Bounced duration in seconds, if this Sound has a bounce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<f64>,
    /// Free-form working notes set via `set_sample_notes` (annotation only).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetInfo {
    pub id: SampleId,
    pub name: String,
    /// `"none"` / `"clean"` / `"stale"`.
    pub bounce: String,
    pub duration_secs: Option<f64>,
}

/// What `bounce` / `render_wav` would render for a Sound, and why — so the
/// auto-duration rules (the single most surprising part of the system) are
/// inspectable up front instead of reverse-engineered.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderPlanInfo {
    /// The length (seconds) an un-overridden bounce will render.
    pub duration_secs: f64,
    /// Whether the Sound is sequencer-driven (renders its song-loop length) vs a
    /// continuous/one-shot graph (renders a fixed default window).
    pub is_sequence: bool,
    /// If sequencer-driven, the loop length (seconds) the render repeats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_secs: Option<f64>,
    /// Plain-language explanation of how `duration_secs` was derived, and how to
    /// override it (`duration_secs` on bounce/render_wav).
    pub reason: String,
}

/// Peak/rms of one arrangement track, rendered in isolation — the per-stem mix
/// readback behind `arrangement_track_stats`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackStats {
    pub track: usize,
    pub name: String,
    /// Peak absolute sample of this track alone (>1.0 = it clips on its own).
    pub peak: f32,
    /// RMS level of this track alone.
    pub rms: f32,
    /// How many clips are on the track.
    pub clips: usize,
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
    /// True when `peak > 1.0` — the render clips (distorts) and needs the level
    /// brought down. Saves the caller having to know that 1.0 is the ceiling.
    pub clipping: bool,
    /// Peak / RMS (linear) — high for spiky transient material, low for dense /
    /// compressed material. `0` when silent.
    #[serde(default)]
    pub crest_factor: f32,
    /// Mean sample value across all channels — a non-zero offset wastes headroom
    /// and clicks at clip edges.
    #[serde(default)]
    pub dc_offset: f32,
    /// Leading silence in seconds (below the -60 dBFS floor) — a sound effect
    /// usually wants a tight (~0) start.
    #[serde(default)]
    pub leading_silence_secs: f64,
    /// Trailing silence in seconds (below the -60 dBFS floor).
    #[serde(default)]
    pub trailing_silence_secs: f64,
    /// Time from the first non-silent frame to the absolute peak — approximate
    /// attack (short = percussive, long = swelling).
    #[serde(default)]
    pub attack_secs: f64,
    /// Time from the absolute peak to the last non-silent frame — approximate
    /// decay / tail length.
    #[serde(default)]
    pub decay_secs: f64,
    /// Estimated inter-sample (true) peak via 4× cubic-Hermite oversampling —
    /// can exceed `peak` when the waveform crests between samples. An estimate,
    /// not an ITU-R BS.1770 measurement.
    #[serde(default)]
    pub true_peak: f32,
}

/// The -60 dBFS amplitude floor for the silence / attack / decay readbacks
/// (10^(-60/20)).
const SILENCE_FLOOR: f32 = 0.001;

impl WavStats {
    /// Compute stats over rendered PCM (`channels[ch][frame]`): peak = max abs
    /// across all channels; rms = sqrt(mean of squares); duration = frames / rate;
    /// plus the transient/tail readbacks (crest factor, DC offset, lead/trail
    /// silence, approximate attack/decay) against a -60 dBFS floor.
    /// Pure f32/f64 math — natively testable, no audio/DOM deps.
    pub fn from_pcm(channels: &[Vec<f32>], sample_rate: u32) -> Self {
        let frames = channels.iter().map(|c| c.len()).max().unwrap_or(0);
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut sum = 0.0f64;
        let mut count = 0u64;
        for ch in channels {
            for &s in ch {
                peak = peak.max(s.abs());
                sum_sq += (s as f64) * (s as f64);
                sum += s as f64;
                count += 1;
            }
        }
        let rms = if count > 0 {
            (sum_sq / count as f64).sqrt() as f32
        } else {
            0.0
        };
        let dc_offset = if count > 0 {
            (sum / count as f64) as f32
        } else {
            0.0
        };
        let duration_secs = if sample_rate > 0 {
            frames as f64 / sample_rate as f64
        } else {
            0.0
        };

        // Frame-wise envelope (max |s| across channels per frame) for the
        // silence / attack / decay readbacks.
        let frame_abs = |i: usize| -> f32 {
            channels
                .iter()
                .map(|c| c.get(i).copied().unwrap_or(0.0).abs())
                .fold(0.0, f32::max)
        };
        let secs = |n: usize| -> f64 {
            if sample_rate > 0 {
                n as f64 / sample_rate as f64
            } else {
                0.0
            }
        };
        let first_loud = (0..frames).find(|&i| frame_abs(i) >= SILENCE_FLOOR);
        let last_loud = (0..frames).rev().find(|&i| frame_abs(i) >= SILENCE_FLOOR);
        let peak_at = (0..frames).max_by(|&a, &b| frame_abs(a).total_cmp(&frame_abs(b)));

        // Inter-sample (true) peak estimate: 4× oversample each channel with a
        // 4-point cubic Hermite (Catmull-Rom) and track the max magnitude.
        let mut true_peak = peak;
        for ch in channels {
            let n = ch.len();
            let at = |i: isize| -> f32 {
                if i < 0 || i as usize >= n {
                    0.0
                } else {
                    ch[i as usize]
                }
            };
            for i in 0..n as isize {
                let (y0, y1, y2, y3) = (at(i - 1), at(i), at(i + 1), at(i + 2));
                for k in 1..4 {
                    let t = k as f32 / 4.0;
                    let a = 0.5 * (3.0 * (y1 - y2) + y3 - y0);
                    let b = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
                    let c = 0.5 * (y2 - y0);
                    let v = ((a * t + b) * t + c) * t + y1;
                    true_peak = true_peak.max(v.abs());
                }
            }
        }
        let (leading_silence_secs, trailing_silence_secs, attack_secs, decay_secs) =
            match (first_loud, last_loud, peak_at) {
                (Some(first), Some(last), Some(at)) => (
                    secs(first),
                    secs(frames - 1 - last),
                    secs(at.saturating_sub(first)),
                    secs(last.saturating_sub(at)),
                ),
                // Entirely silent (or empty) — everything is "silence".
                _ => (duration_secs, duration_secs, 0.0, 0.0),
            };

        Self {
            duration_secs,
            peak,
            rms,
            channels: channels.len() as u32,
            sample_rate,
            clipping: peak > 1.0,
            crest_factor: if rms > 0.0 { peak / rms } else { 0.0 },
            dc_offset,
            leading_silence_secs,
            trailing_silence_secs,
            attack_secs,
            decay_secs,
            true_peak,
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

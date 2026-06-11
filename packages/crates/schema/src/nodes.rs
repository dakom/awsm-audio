//! Every WebAudio node type, as authorable data.
//!
//! Each struct carries the node's construction-time config (non-automatable
//! fields like `type` or `maxDelayTime`) plus its [`AudioParam`]s as named
//! fields. `Default` impls reproduce the platform's documented default values,
//! so the editor can spawn a ready-to-use node from `NodeKind::Oscillator(_)`.
//!
//! Modulation targets and audio connections live in the graph's
//! [`Connection`](crate::Connection) list, not on the nodes.

use serde::{Deserialize, Serialize};

use crate::enums::*;
use crate::ids::{AssetId, PortId, SampleId};
use crate::param::{AudioParam, ParamId};

/// A node instance in a [`Graph`](crate::Graph).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: crate::ids::NodeId,
    /// Optional human label (editor only; no runtime role).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub kind: NodeKind,
}

impl Node {
    pub fn new(kind: NodeKind) -> Self {
        Self {
            id: crate::ids::NodeId::new(),
            label: None,
            kind,
        }
    }
}

/// The discriminated set of node types: every primitive WebAudio node, plus
/// [`Sample`](NodeKind::Sample) for nesting another sample as a sub-graph.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "props")]
pub enum NodeKind {
    // ---- sources ----
    Oscillator(OscillatorNode),
    AudioBufferSource(AudioBufferSourceNode),
    ConstantSource(ConstantSourceNode),
    Noise(NoiseNode),
    MediaElementSource(MediaElementSourceNode),
    MediaStreamSource(MediaStreamSourceNode),

    // ---- effects / processing ----
    Gain(GainNode),
    BiquadFilter(BiquadFilterNode),
    IirFilter(IirFilterNode),
    Delay(DelayNode),
    DynamicsCompressor(DynamicsCompressorNode),
    WaveShaper(WaveShaperNode),
    Convolver(ConvolverNode),

    // ---- spatialization ----
    Panner(PannerNode),
    StereoPanner(StereoPannerNode),

    // ---- analysis ----
    Analyser(AnalyserNode),

    // ---- routing ----
    ChannelSplitter(ChannelSplitterNode),
    ChannelMerger(ChannelMergerNode),

    // ---- extensibility ----
    AudioWorklet(AudioWorkletNode),

    // ---- destination ----
    /// The graph's audible output (speakers), plain stereo.
    Output(OutputNode),
    /// The graph's audible output placed in 3D space (an HRTF panner). The
    /// `position_*` params are the natural "player-adjustable" controls.
    SpatialOutput(SpatialOutputNode),

    // ---- composition ----
    /// Instantiate another sample as a sub-graph (see [`SampleRef`]).
    Sample(SampleRef),

    // ---- sequencing (Sequences view; emit no audio of their own) ----
    /// Plays a [`Song`](crate::Song): each distinct sound is a keyed output that
    /// triggers a wired instrument. See [`NoteSequencerNode`].
    NoteSequencer(NoteSequencerNode),
    /// Automates params over time: each lane is a keyed output wired to a param.
    /// See [`ControlSequencerNode`].
    ControlSequencer(ControlSequencerNode),
    /// A summing bus — a named unity gain (WebAudio sums all inputs).
    Bus(BusNode),
}

impl NodeKind {
    /// Set a named `AudioParam`'s base value (used by macro-param bindings during
    /// flattening). Unknown names are ignored.
    pub fn set_param_value(&mut self, param: &str, value: f32) {
        use NodeKind::*;
        let target: Option<&mut AudioParam> = match self {
            Oscillator(o) => match param {
                "frequency" => Some(&mut o.frequency),
                "detune" => Some(&mut o.detune),
                _ => None,
            },
            ConstantSource(c) if param == "offset" => Some(&mut c.offset),
            AudioBufferSource(b) => match param {
                "playbackRate" => Some(&mut b.playback_rate),
                "detune" => Some(&mut b.detune),
                _ => None,
            },
            Gain(g) if param == "gain" => Some(&mut g.gain),
            BiquadFilter(b) => match param {
                "frequency" => Some(&mut b.frequency),
                "detune" => Some(&mut b.detune),
                "Q" => Some(&mut b.q),
                "gain" => Some(&mut b.gain),
                _ => None,
            },
            Delay(d) if param == "delayTime" => Some(&mut d.delay_time),
            DynamicsCompressor(c) => match param {
                "threshold" => Some(&mut c.threshold),
                "knee" => Some(&mut c.knee),
                "ratio" => Some(&mut c.ratio),
                "attack" => Some(&mut c.attack),
                "release" => Some(&mut c.release),
                _ => None,
            },
            StereoPanner(p) if param == "pan" => Some(&mut p.pan),
            Panner(p) => match param {
                "positionX" => Some(&mut p.position_x),
                "positionY" => Some(&mut p.position_y),
                "positionZ" => Some(&mut p.position_z),
                "orientationX" => Some(&mut p.orientation_x),
                "orientationY" => Some(&mut p.orientation_y),
                "orientationZ" => Some(&mut p.orientation_z),
                _ => None,
            },
            Output(o) if param == "gain" => Some(&mut o.gain),
            SpatialOutput(o) => match param {
                "gain" => Some(&mut o.gain),
                "positionX" => Some(&mut o.position_x),
                "positionY" => Some(&mut o.position_y),
                "positionZ" => Some(&mut o.position_z),
                _ => None,
            },
            AudioWorklet(w) => {
                if let Some(wp) = w.parameters.iter_mut().find(|p| p.name.0 == param) {
                    wp.param.value = value;
                }
                None
            }
            _ => None,
        };
        if let Some(p) = target {
            p.value = value;
        }
    }
}

// ======================================================================
// Sources
// ======================================================================

/// `OscillatorNode`. When `oscillator_type` is
/// [`Custom`](OscillatorType::Custom), `harmonics` gives the amplitude of each
/// partial (harmonic 1, 2, 3, …); the player builds a `PeriodicWave` from them.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OscillatorNode {
    // Scalars first (TOML: scalar keys must precede sub-tables).
    #[serde(rename = "type")]
    pub oscillator_type: OscillatorType,
    /// Partial amplitudes for a `Custom` wave. Empty for the built-in waveforms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harmonics: Vec<f32>,
    pub frequency: AudioParam,
    pub detune: AudioParam,
}

impl Default for OscillatorNode {
    fn default() -> Self {
        Self {
            oscillator_type: OscillatorType::Sine,
            harmonics: Vec::new(),
            frequency: AudioParam::new(440.0),
            detune: AudioParam::new(0.0),
        }
    }
}

/// `AudioBufferSourceNode`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioBufferSourceNode {
    // Scalars first (TOML: scalar keys must precede sub-tables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<AssetId>,
    #[serde(rename = "loop", default)]
    pub looping: bool,
    #[serde(default)]
    pub loop_start: f64,
    #[serde(default)]
    pub loop_end: f64,
    pub playback_rate: AudioParam,
    pub detune: AudioParam,
}

impl Default for AudioBufferSourceNode {
    fn default() -> Self {
        Self {
            buffer: None,
            looping: false,
            loop_start: 0.0,
            loop_end: 0.0,
            playback_rate: AudioParam::new(1.0),
            detune: AudioParam::new(0.0),
        }
    }
}

/// A synthesized noise source. Not a native WebAudio node — the player
/// generates a seeded buffer per [`NoiseFlavor`] and loops it through an
/// `AudioBufferSourceNode`. The recipe (not the samples) is what's stored, so
/// it's tiny and reproducible.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoiseNode {
    pub flavor: NoiseFlavor,
    /// PRNG seed — same seed reproduces the exact texture.
    ///
    /// Clamped on deserialize to [`MAX_NOISE_SEED`] (`i64::MAX`). The seed is
    /// opaque entropy, but the authored on-disk format is TOML, whose only
    /// integer type is `i64` — a `u64` above `i64::MAX` makes saving fail with
    /// "out-of-range value for u64 type". Bounding it here means any document we
    /// hold in memory always round-trips through TOML.
    #[serde(deserialize_with = "deserialize_clamped_seed")]
    pub seed: u64,
    /// Buffer length in seconds (looped).
    pub seconds: f32,
    /// Generate two decorrelated channels for stereo width.
    #[serde(default)]
    pub stereo: bool,
    /// Events per second for `Dust` / `Velvet` flavors.
    #[serde(default)]
    pub density: f32,
    /// Use a Gaussian (vs uniform) distribution for the continuous colors.
    #[serde(default)]
    pub gaussian: bool,
}

/// The largest noise [`seed`](NoiseNode::seed) that survives a TOML round-trip
/// (TOML integers are `i64`). Seeds are clamped to this on input.
pub const MAX_NOISE_SEED: u64 = i64::MAX as u64;

/// Deserialize a noise seed, clamping it into the TOML-representable range so a
/// later save can't fail. See [`NoiseNode::seed`].
fn deserialize_clamped_seed<'de, D>(de: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = u64::deserialize(de)?;
    Ok(raw.min(MAX_NOISE_SEED))
}

impl Default for NoiseNode {
    fn default() -> Self {
        Self {
            flavor: NoiseFlavor::White,
            seed: 1,
            seconds: 2.0,
            stereo: false,
            density: 800.0,
            gaussian: false,
        }
    }
}

/// `ConstantSourceNode` — a steady DC offset, handy as a control source.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConstantSourceNode {
    pub offset: AudioParam,
}

impl Default for ConstantSourceNode {
    fn default() -> Self {
        Self {
            offset: AudioParam::new(1.0),
        }
    }
}

/// `MediaElementAudioSourceNode` — pulls audio from an `<audio>`/`<video>`
/// element. Referenced here by media URL; the player owns the element.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaElementSourceNode {
    pub src: String,
}

/// `MediaStreamAudioSourceNode` — pulls from a live `MediaStream` (mic, etc.).
/// The stream is bound at runtime; `label` is just an authoring hint for which
/// device/role the editor should request.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MediaStreamSourceNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

// ======================================================================
// Effects / processing
// ======================================================================

/// `GainNode`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GainNode {
    pub gain: AudioParam,
}

impl Default for GainNode {
    fn default() -> Self {
        Self {
            gain: AudioParam::new(1.0),
        }
    }
}

/// `BiquadFilterNode`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BiquadFilterNode {
    #[serde(rename = "type")]
    pub filter_type: BiquadFilterType,
    pub frequency: AudioParam,
    pub detune: AudioParam,
    #[serde(rename = "Q")]
    pub q: AudioParam,
    pub gain: AudioParam,
}

impl Default for BiquadFilterNode {
    fn default() -> Self {
        Self {
            filter_type: BiquadFilterType::Lowpass,
            frequency: AudioParam::new(350.0),
            detune: AudioParam::new(0.0),
            q: AudioParam::new(1.0),
            gain: AudioParam::new(0.0),
        }
    }
}

/// `IIRFilterNode` — fixed feedforward/feedback coefficients, no automatable
/// params.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IirFilterNode {
    pub feedforward: Vec<f64>,
    pub feedback: Vec<f64>,
}

impl Default for IirFilterNode {
    /// A gentle one-pole low-pass, so a freshly added node is audible and its
    /// coefficient fields are populated to edit from.
    fn default() -> Self {
        Self {
            feedforward: vec![0.2, 0.2],
            feedback: vec![1.0, -0.6],
        }
    }
}

/// `DelayNode`. `max_delay_time` is a construction-time ceiling.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelayNode {
    // Scalar first (TOML: scalar keys must precede sub-tables).
    pub max_delay_time: f64,
    pub delay_time: AudioParam,
}

impl Default for DelayNode {
    fn default() -> Self {
        Self {
            max_delay_time: 1.0,
            delay_time: AudioParam::new(0.0),
        }
    }
}

/// `DynamicsCompressorNode`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DynamicsCompressorNode {
    pub threshold: AudioParam,
    pub knee: AudioParam,
    pub ratio: AudioParam,
    pub attack: AudioParam,
    pub release: AudioParam,
}

impl Default for DynamicsCompressorNode {
    fn default() -> Self {
        Self {
            threshold: AudioParam::new(-24.0),
            knee: AudioParam::new(30.0),
            ratio: AudioParam::new(12.0),
            attack: AudioParam::new(0.003),
            release: AudioParam::new(0.25),
        }
    }
}

/// `WaveShaperNode` — distortion. The player generates the shaping curve from
/// `shape` (the character) and `amount` (the intensity: 0 ≈ gentle).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WaveShaperNode {
    #[serde(default)]
    pub amount: f32,
    #[serde(default)]
    pub shape: WaveShaperShape,
    #[serde(default)]
    pub oversample: OverSampleType,
    /// A user-drawn transfer curve (output values in -1..1 across input -1..1),
    /// used when `shape` is [`WaveShaperShape::Custom`]. Empty otherwise. Mirrors
    /// the custom oscillator's inline `harmonics`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curve: Vec<f32>,
}

impl Default for WaveShaperNode {
    fn default() -> Self {
        Self {
            amount: 2.0,
            shape: WaveShaperShape::Tanh,
            oversample: OverSampleType::None,
            curve: Vec::new(),
        }
    }
}

/// `ConvolverNode` — convolution reverb from an impulse-response `buffer`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConvolverNode {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer: Option<AssetId>,
    #[serde(default)]
    pub disable_normalization: bool,
    /// When no impulse-response `buffer` is loaded, the length (seconds) of the
    /// synthesized decaying-noise reverb the player generates. Bigger = a larger
    /// space / longer tail.
    #[serde(default = "default_reverb_seconds")]
    pub reverb_seconds: f32,
}

fn default_reverb_seconds() -> f32 {
    2.0
}

impl Default for ConvolverNode {
    fn default() -> Self {
        Self {
            buffer: None,
            disable_normalization: false,
            reverb_seconds: default_reverb_seconds(),
        }
    }
}

// ======================================================================
// Spatialization
// ======================================================================

/// `PannerNode` — full 3D positional audio (paired with the context
/// [`Listener`](crate::Listener)).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PannerNode {
    pub panning_model: PanningModelType,
    pub distance_model: DistanceModelType,
    pub ref_distance: f64,
    pub max_distance: f64,
    pub rolloff_factor: f64,
    pub cone_inner_angle: f64,
    pub cone_outer_angle: f64,
    pub cone_outer_gain: f64,
    pub position_x: AudioParam,
    pub position_y: AudioParam,
    pub position_z: AudioParam,
    pub orientation_x: AudioParam,
    pub orientation_y: AudioParam,
    pub orientation_z: AudioParam,
}

impl Default for PannerNode {
    fn default() -> Self {
        Self {
            panning_model: PanningModelType::EqualPower,
            distance_model: DistanceModelType::Inverse,
            ref_distance: 1.0,
            max_distance: 10000.0,
            rolloff_factor: 1.0,
            cone_inner_angle: 360.0,
            cone_outer_angle: 360.0,
            cone_outer_gain: 0.0,
            position_x: AudioParam::new(0.0),
            position_y: AudioParam::new(0.0),
            position_z: AudioParam::new(0.0),
            orientation_x: AudioParam::new(1.0),
            orientation_y: AudioParam::new(0.0),
            orientation_z: AudioParam::new(0.0),
        }
    }
}

/// `StereoPannerNode` — simple equal-power L/R pan in `[-1, 1]`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StereoPannerNode {
    pub pan: AudioParam,
}

impl Default for StereoPannerNode {
    fn default() -> Self {
        Self {
            pan: AudioParam::new(0.0),
        }
    }
}

// ======================================================================
// Analysis
// ======================================================================

/// `AnalyserNode` — passes audio through unchanged while exposing FFT/time
/// data. Analysis is a runtime read; only its config is authored.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnalyserNode {
    pub fft_size: u32,
    pub min_decibels: f64,
    pub max_decibels: f64,
    pub smoothing_time_constant: f64,
}

impl Default for AnalyserNode {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            min_decibels: -100.0,
            max_decibels: -30.0,
            smoothing_time_constant: 0.8,
        }
    }
}

// ======================================================================
// Routing
// ======================================================================

/// `ChannelSplitterNode` — fans one input out to `number_of_outputs` mono
/// outputs.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelSplitterNode {
    pub number_of_outputs: u32,
}

impl Default for ChannelSplitterNode {
    fn default() -> Self {
        Self {
            number_of_outputs: 6,
        }
    }
}

/// `ChannelMergerNode` — combines `number_of_inputs` mono inputs into one
/// multi-channel output.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMergerNode {
    pub number_of_inputs: u32,
}

impl Default for ChannelMergerNode {
    fn default() -> Self {
        Self {
            number_of_inputs: 6,
        }
    }
}

// ======================================================================
// Extensibility
// ======================================================================

/// `AudioWorkletNode` — a WASM-backed DSP processor. The runtime always loads a
/// single generic shim (`registerProcessor`), then instantiates the referenced
/// [`module`](Self::module) WASM (a [`WasmAsset`](crate::WasmAsset)) inside it.
/// The module must export the awsm-audio worklet ABI (mono `process`, memory
/// scratch pointers, and a parameter-discovery interface).
///
/// Parameters are *discovered* from the module (name + range + default) and then
/// mapped onto the shim's fixed bank of generic `AudioParam`s — so a worklet's
/// params behave like any other node's: editable, automatable (envelopes), and
/// modulation-wire targets.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AudioWorkletNode {
    /// The WASM module providing the DSP, referenced by id. `None` = a silent
    /// pass-through until a module is assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<AssetId>,
    /// Optional display name for the loaded processor (e.g. the file stem).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub processor_name: String,
    /// Parameters discovered from the module, carrying their editable value /
    /// automation. Order matches the module's discovery order (bank slot index).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<WorkletParam>,
}

/// A parameter discovered from an [`AudioWorkletNode`]'s WASM module: a name and
/// display range plus its automatable [`AudioParam`] state.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkletParam {
    pub name: ParamId,
    /// Display range from the module's descriptor (editor knob bounds).
    pub min: f32,
    pub max: f32,
    pub param: AudioParam,
}

// ======================================================================
// Destination
// ======================================================================

/// The graph's audible output (a plain stereo sink). Whatever feeds it reaches
/// the speakers, scaled by `gain`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputNode {
    pub gain: AudioParam,
}

impl Default for OutputNode {
    fn default() -> Self {
        Self {
            gain: AudioParam::new(1.0),
        }
    }
}

/// The graph's audible output, placed in 3D space via an HRTF
/// [`PannerNode`](PannerNode) at `position_*`. The position is the natural
/// "player-adjustable" control: a runtime/game can move the sound by writing
/// these params without touching the rest of the graph.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpatialOutputNode {
    pub gain: AudioParam,
    pub position_x: AudioParam,
    pub position_y: AudioParam,
    pub position_z: AudioParam,
}

impl Default for SpatialOutputNode {
    fn default() -> Self {
        Self {
            gain: AudioParam::new(1.0),
            position_x: AudioParam::new(0.0),
            position_y: AudioParam::new(0.0),
            position_z: AudioParam::new(0.0),
        }
    }
}

// ======================================================================
// Composition
// ======================================================================

/// A reference instantiating another [`Sample`](crate::Sample) as a sub-graph.
/// The referenced sample's inlets/outlets become this node's numbered
/// inputs/outputs (in declaration order); each inlet's value can be set here.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleRef {
    pub sample: SampleId,
    /// Per-instance values for the referenced sample's inlets. An entry
    /// overrides that inlet's [`default`](crate::PortDecl::default) for this
    /// instance; unwired inlets without an entry use the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputValue>,
}

/// Sets one inlet (input) of a referenced sample to a fixed value for this
/// instance. (`port` is the referenced sample's inlet name.)
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputValue {
    pub port: PortId,
    pub value: f32,
}

// ======================================================================
// Sequencing — these nodes emit no audio; they drive instruments/params via
// keyed `SeqOut` connections that the player's scheduler reads.
// ======================================================================

use crate::connection::SeqKey;
use crate::song::Song;

/// How a [`NoteSequencerNode`] turns its tracks into sounding outputs. This is a
/// whole-node property — every track in the node is the same kind — so the
/// editor presents the two modes as two distinct palette nodes (a "Melodic
/// Sequencer" and a "Drum Sequencer").
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SequencerMode {
    /// Each track is one pitched instrument: the whole track is a single output,
    /// and a note's pitch transposes that instrument.
    #[default]
    Melodic,
    /// Each track is a drum kit: every distinct note row becomes its own output
    /// (kick, snare, hat…), so the note number picks the *sound*, not a pitch.
    Drum,
}

impl SequencerMode {
    pub fn is_drum(self) -> bool {
        matches!(self, SequencerMode::Drum)
    }
}

/// Plays a [`Song`]. Its [`mode`](NoteSequencerNode::mode) decides how tracks map
/// to keyed outputs: `Melodic` → one output per track (played pitched); `Drum` →
/// one output per distinct note (each its own kit piece). Wire each output to an
/// instrument; the scheduler spawns a voice of it per note. `start` seeks
/// (beats); `looping` repeats.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NoteSequencerNode {
    /// Melodic vs drum — fixed for the node; all its tracks follow it.
    #[serde(default)]
    pub mode: SequencerMode,
    #[serde(default)]
    pub song: Song,
    /// Playback-window start, in beats (also the piano-roll's left marker). Play
    /// begins here, so you can isolate a section.
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub start: f64,
    /// Playback-window stop, in beats (the right marker). `None` plays to the end
    /// of the song's content / authored length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<f64>,
    /// Authored song length in beats — how far the piano-roll grid extends so you
    /// can place notes past the current content. `0` means "auto" (fit content).
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub length: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub looping: bool,
    /// One per sound, in stable order. Synced from the song (append-only, keyed),
    /// so wires by `key` survive edits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<SoundOut>,
}

/// One sounding output of a [`NoteSequencerNode`] — a melodic track or a single
/// drum note — wired (by `key`) to an instrument.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SoundOut {
    /// Stable identity used by wires (e.g. `"t0"` or `"t2:n36"`).
    pub key: SeqKey,
    /// Index into [`Song::tracks`](crate::Song::tracks).
    pub track: usize,
    /// `Some(note)` for a single drum sound; `None` for a whole melodic track.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<u8>,
    /// Display label (e.g. "Lead", "Kick").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub transpose: i32,
    #[serde(default = "one_f32")]
    pub gain: f32,
}

/// Automates parameters over time: each [`ControlLane`] is a keyed output wired
/// to a node param. The scheduler applies each lane as timed automation.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlSequencerNode {
    /// Tempo (BPM) for the lanes' beat positions.
    pub bpm: f64,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub start: f64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub looping: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<ControlLane>,
}

impl Default for ControlSequencerNode {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            start: 0.0,
            looping: false,
            lanes: Vec::new(),
        }
    }
}

/// One automation lane: a value-over-time curve (in beats) = one keyed output.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlLane {
    pub key: SeqKey,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub points: Vec<ControlPoint>,
}

/// How a control lane segment reaches a point from the previous one.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Curve {
    /// Hold the previous value, then jump (a stair-step).
    Step,
    /// Straight line.
    #[default]
    Linear,
    /// Exponential ramp (can't cross zero; values are clamped away from 0).
    Exponential,
    /// Eased S-curve (smoothstep).
    Smooth,
}

/// A breakpoint in a [`ControlLane`]: `value` at `beat`, with the `curve`
/// describing how the lane reaches it from the previous point.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlPoint {
    pub beat: f64,
    pub value: f32,
    #[serde(default, skip_serializing_if = "is_default_curve")]
    pub curve: Curve,
}

fn is_default_curve(c: &Curve) -> bool {
    *c == Curve::default()
}

/// A summing bus — a named unity gain (any WebAudio node sums its inputs).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusNode {
    /// Output level (linear); 1.0 = unity.
    #[serde(default = "one_f32")]
    pub gain: f32,
}

impl Default for BusNode {
    fn default() -> Self {
        Self { gain: 1.0 }
    }
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn one_f32() -> f32 {
    1.0
}

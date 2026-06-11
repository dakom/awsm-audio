//! Editable settings for a node, derived from its [`NodeKind`].
//!
//! [`fields`] reflects a node into a flat list of labelled [`Field`]s the UI
//! renders as number inputs / dropdowns / checkboxes. [`apply`] writes one back
//! by key. Both halves are exhaustive over the kinds that have authorable
//! scalar/enum/bool settings; topology-changing config (channel counts, worklet
//! I/O) and asset references are intentionally left out for now.

use std::cell::RefCell;
use std::collections::HashSet;

use awsm_audio_schema::*;

// `FieldValue` (the serializable `SetField` payload) now lives in the shared
// protocol crate; the live `Field`/`Control` reflection below stays here.
pub use awsm_audio_editor_protocol::FieldValue;

thread_local! {
    /// Interned param names. WASM-worklet params are discovered at runtime, but
    /// [`Field`]/[`ParamInfo`] (and the inspector) key on `&'static str`. We leak
    /// each *distinct* name once and reuse it, so reflection over a node many
    /// times never grows the set.
    static INTERN: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

/// Stable `&'static str` for a (possibly dynamic) name, leaked at most once per
/// distinct string.
fn intern(s: &str) -> &'static str {
    INTERN.with(|set| {
        let mut set = set.borrow_mut();
        if let Some(v) = set.get(s) {
            return *v;
        }
        let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
        set.insert(leaked);
        leaked
    })
}

/// What control to render for a field.
pub enum Control {
    Number(f64),
    Choice {
        value: String,
        options: &'static [&'static str],
    },
    Bool(bool),
}

/// One labelled, editable setting. `modulation` is the WebAudio param name a
/// signal can be wired to (a modulation inlet appears on this field's row); it's
/// `None` for non-automatable settings (type, oversample, coefficients, …).
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub control: Control,
    pub modulation: Option<&'static str>,
}

fn num(key: &'static str, label: &'static str, v: f32) -> Field {
    Field {
        key,
        label,
        control: Control::Number(v as f64),
        modulation: None,
    }
}
/// An automatable number field: editable scalar that also exposes a modulation
/// inlet wired to the WebAudio param `m`.
fn amod(key: &'static str, label: &'static str, v: f32, m: &'static str) -> Field {
    Field {
        key,
        label,
        control: Control::Number(v as f64),
        modulation: Some(m),
    }
}
fn numf(key: &'static str, label: &'static str, v: f64) -> Field {
    Field {
        key,
        label,
        control: Control::Number(v),
        modulation: None,
    }
}
fn boolean(key: &'static str, label: &'static str, v: bool) -> Field {
    Field {
        key,
        label,
        control: Control::Bool(v),
        modulation: None,
    }
}
fn choice(
    key: &'static str,
    label: &'static str,
    value: &str,
    options: &'static [&'static str],
) -> Field {
    Field {
        key,
        label,
        control: Control::Choice {
            value: value.to_string(),
            options,
        },
        modulation: None,
    }
}
/// A free-text field (a Choice with no options) for editing a list of numbers
/// (comma/space separated) — IIR coefficients, custom-oscillator harmonics.
fn list_field(key: &'static str, label: &'static str, value: &str) -> Field {
    Field {
        key,
        label,
        control: Control::Choice {
            value: value.to_string(),
            options: &[],
        },
        modulation: None,
    }
}
fn join_f32(v: &[f32]) -> String {
    v.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
fn join_f64(v: &[f64]) -> String {
    v.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
fn parse_f32_list(s: &str) -> Vec<f32> {
    s.split([',', ' ', '\t', '\n'])
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect()
}
fn parse_f64_list(s: &str) -> Vec<f64> {
    s.split([',', ' ', '\t', '\n'])
        .filter(|t| !t.is_empty())
        .filter_map(|t| t.parse().ok())
        .collect()
}

const OSC_TYPES: &[&str] = &["sine", "square", "sawtooth", "triangle", "custom"];
const NOISE_FLAVORS: &[&str] = &["white", "pink", "brown", "blue", "violet", "dust", "velvet"];
const BIQUAD_TYPES: &[&str] = &[
    "lowpass",
    "highpass",
    "bandpass",
    "lowshelf",
    "highshelf",
    "peaking",
    "notch",
    "allpass",
];
const OVERSAMPLE: &[&str] = &["none", "2x", "4x"];
const WAVESHAPER_SHAPES: &[&str] = &["tanh", "hard_clip", "fold", "custom"];
const PANNING_MODELS: &[&str] = &["equalpower", "HRTF"];
const DISTANCE_MODELS: &[&str] = &["linear", "inverse", "exponential"];
const FFT_SIZES: &[&str] = &[
    "32", "64", "128", "256", "512", "1024", "2048", "4096", "8192",
];

/// The editable settings for `kind`, in display order. Empty = nothing to edit.
pub fn fields(kind: &NodeKind) -> Vec<Field> {
    match kind {
        NodeKind::Oscillator(o) => {
            let mut v = vec![
                choice("type", "type", osc_type_str(o.oscillator_type), OSC_TYPES),
                amod("frequency", "freq (Hz)", o.frequency.value, "frequency"),
                amod("detune", "detune", o.detune.value, "detune"),
            ];
            // A custom wave is authored as a list of partial amplitudes.
            if o.oscillator_type == OscillatorType::Custom {
                v.push(list_field(
                    "harmonics",
                    "harmonics",
                    &join_f32(&o.harmonics),
                ));
            }
            v
        }
        NodeKind::ConstantSource(c) => vec![amod("offset", "offset", c.offset.value, "offset")],
        NodeKind::Noise(n) => vec![
            choice(
                "flavor",
                "flavor",
                noise_flavor_str(n.flavor),
                NOISE_FLAVORS,
            ),
            numf("seed", "seed", n.seed as f64),
            num("seconds", "seconds", n.seconds),
            boolean("stereo", "stereo", n.stereo),
            num("density", "density", n.density),
            boolean("gaussian", "gaussian", n.gaussian),
        ],
        NodeKind::AudioBufferSource(b) => vec![
            amod(
                "playback_rate",
                "rate",
                b.playback_rate.value,
                "playbackRate",
            ),
            amod("detune", "detune", b.detune.value, "detune"),
            boolean("loop", "loop", b.looping),
        ],
        NodeKind::Gain(g) => vec![amod("gain", "gain", g.gain.value, "gain")],
        NodeKind::BiquadFilter(b) => vec![
            choice("type", "type", biquad_type_str(b.filter_type), BIQUAD_TYPES),
            amod("frequency", "freq (Hz)", b.frequency.value, "frequency"),
            amod("detune", "detune", b.detune.value, "detune"),
            amod("Q", "Q", b.q.value, "Q"),
            amod("gain", "gain (dB)", b.gain.value, "gain"),
        ],
        NodeKind::Delay(d) => vec![
            numf("max_delay_time", "max delay", d.max_delay_time),
            amod("delay_time", "delay (s)", d.delay_time.value, "delayTime"),
        ],
        NodeKind::DynamicsCompressor(c) => vec![
            amod("threshold", "threshold", c.threshold.value, "threshold"),
            amod("knee", "knee", c.knee.value, "knee"),
            amod("ratio", "ratio", c.ratio.value, "ratio"),
            amod("attack", "attack", c.attack.value, "attack"),
            amod("release", "release", c.release.value, "release"),
        ],
        NodeKind::WaveShaper(w) => vec![
            choice(
                "shape",
                "shape",
                waveshaper_shape_str(w.shape),
                WAVESHAPER_SHAPES,
            ),
            num("amount", "amount", w.amount),
            choice(
                "oversample",
                "oversample",
                oversample_str(w.oversample),
                OVERSAMPLE,
            ),
        ],
        NodeKind::Convolver(c) => vec![
            num("reverb_seconds", "reverb (s)", c.reverb_seconds),
            boolean(
                "disable_normalization",
                "no normalize",
                c.disable_normalization,
            ),
        ],
        NodeKind::StereoPanner(p) => vec![amod("pan", "pan", p.pan.value, "pan")],
        NodeKind::Panner(p) => vec![
            choice(
                "panning_model",
                "model",
                panning_model_str(p.panning_model),
                PANNING_MODELS,
            ),
            choice(
                "distance_model",
                "distance",
                distance_model_str(p.distance_model),
                DISTANCE_MODELS,
            ),
            amod("position_x", "pos x", p.position_x.value, "positionX"),
            amod("position_y", "pos y", p.position_y.value, "positionY"),
            amod("position_z", "pos z", p.position_z.value, "positionZ"),
        ],
        NodeKind::Analyser(a) => vec![
            choice("fft_size", "fft size", &a.fft_size.to_string(), FFT_SIZES),
            numf("min_decibels", "min dB", a.min_decibels),
            numf("max_decibels", "max dB", a.max_decibels),
            numf(
                "smoothing_time_constant",
                "smoothing",
                a.smoothing_time_constant,
            ),
        ],
        NodeKind::Output(o) => vec![amod("gain", "gain", o.gain.value, "gain")],
        NodeKind::SpatialOutput(o) => vec![
            amod("gain", "gain", o.gain.value, "gain"),
            amod("position_x", "pos x", o.position_x.value, "positionX"),
            amod("position_y", "pos y", o.position_y.value, "positionY"),
            amod("position_z", "pos z", o.position_z.value, "positionZ"),
        ],
        NodeKind::MediaElementSource(m) => vec![Field {
            key: "src",
            label: "src url",
            control: Control::Choice {
                value: m.src.clone(),
                options: &[],
            },
            modulation: None,
        }],
        // WASM worklet: its discovered params become editable, modulatable fields.
        NodeKind::AudioWorklet(w) => w
            .parameters
            .iter()
            .map(|p| {
                let name = intern(&p.name.0);
                amod(name, name, p.param.value, name)
            })
            .collect(),
        NodeKind::IirFilter(f) => vec![
            list_field("feedforward", "feedforward", &join_f64(&f.feedforward)),
            list_field("feedback", "feedback", &join_f64(&f.feedback)),
        ],
        // Routing fan-out / fan-in: an editable channel count (changes the ports).
        NodeKind::ChannelSplitter(s) => vec![num(
            "number_of_outputs",
            "outputs",
            s.number_of_outputs as f32,
        )],
        NodeKind::ChannelMerger(m) => {
            vec![num("number_of_inputs", "inputs", m.number_of_inputs as f32)]
        }
        // No scalar settings (topology / assets only). The sequencer's settings
        // live in custom inspector panels, not field rows.
        NodeKind::MediaStreamSource(_)
        | NodeKind::Sample(_)
        | NodeKind::NoteSequencer(_)
        | NodeKind::ControlSequencer(_)
        | NodeKind::Bus(_) => vec![],
    }
}

/// The index, within `fields(kind)`, of the field whose modulation inlet drives
/// the WebAudio param `param` — i.e. which body row carries that param's dot.
/// The single source of truth shared by the node renderer and the wire renderer
/// so a dot and the wire landing on it always coincide.
pub fn param_row_index(kind: &NodeKind, param: &str) -> Option<usize> {
    fields(kind)
        .iter()
        .position(|f| f.modulation == Some(param))
}

/// Write one field back into `kind`. Unknown keys / mismatched value types are
/// ignored (the UI only ever sends matching ones).
pub fn apply(kind: &mut NodeKind, key: &str, value: &FieldValue) {
    let n = || as_num(value);
    let b = || as_bool(value);
    let t = || as_text(value);
    match kind {
        NodeKind::Oscillator(o) => match key {
            "type" => {
                if let Some(s) = t() {
                    o.oscillator_type = parse_osc(&s)
                }
            }
            "frequency" => {
                if let Some(v) = n() {
                    o.frequency.value = v as f32
                }
            }
            "detune" => {
                if let Some(v) = n() {
                    o.detune.value = v as f32
                }
            }
            "harmonics" => {
                if let Some(s) = t() {
                    o.harmonics = parse_f32_list(&s)
                }
            }
            _ => {}
        },
        NodeKind::IirFilter(f) => match key {
            "feedforward" => {
                if let Some(s) = t() {
                    f.feedforward = parse_f64_list(&s)
                }
            }
            "feedback" => {
                if let Some(s) = t() {
                    f.feedback = parse_f64_list(&s)
                }
            }
            _ => {}
        },
        // Single-field nodes: the command always carries that field's key.
        NodeKind::ConstantSource(c) => {
            if let Some(v) = n() {
                c.offset.value = v as f32;
            }
        }
        NodeKind::Noise(nz) => match key {
            "flavor" => {
                if let Some(s) = t() {
                    nz.flavor = parse_noise_flavor(&s)
                }
            }
            "seed" => {
                if let Some(v) = n() {
                    // Clamp to the TOML-savable range (see `NoiseNode::seed`).
                    nz.seed = (v.max(0.0) as u64).min(awsm_audio_schema::MAX_NOISE_SEED)
                }
            }
            "seconds" => {
                if let Some(v) = n() {
                    nz.seconds = v as f32
                }
            }
            "stereo" => {
                if let Some(v) = b() {
                    nz.stereo = v
                }
            }
            "density" => {
                if let Some(v) = n() {
                    nz.density = v as f32
                }
            }
            "gaussian" => {
                if let Some(v) = b() {
                    nz.gaussian = v
                }
            }
            _ => {}
        },
        NodeKind::AudioBufferSource(s) => match key {
            "playback_rate" => {
                if let Some(v) = n() {
                    s.playback_rate.value = v as f32
                }
            }
            "detune" => {
                if let Some(v) = n() {
                    s.detune.value = v as f32
                }
            }
            "loop" => {
                if let Some(v) = b() {
                    s.looping = v
                }
            }
            _ => {}
        },
        NodeKind::Gain(g) => {
            if let Some(v) = n() {
                g.gain.value = v as f32;
            }
        }
        NodeKind::BiquadFilter(f) => match key {
            "type" => {
                if let Some(s) = t() {
                    f.filter_type = parse_biquad(&s)
                }
            }
            "frequency" => {
                if let Some(v) = n() {
                    f.frequency.value = v as f32
                }
            }
            "detune" => {
                if let Some(v) = n() {
                    f.detune.value = v as f32
                }
            }
            "Q" => {
                if let Some(v) = n() {
                    f.q.value = v as f32
                }
            }
            "gain" => {
                if let Some(v) = n() {
                    f.gain.value = v as f32
                }
            }
            _ => {}
        },
        NodeKind::Delay(d) => match key {
            "max_delay_time" => {
                if let Some(v) = n() {
                    d.max_delay_time = v
                }
            }
            "delay_time" => {
                if let Some(v) = n() {
                    d.delay_time.value = v as f32
                }
            }
            _ => {}
        },
        NodeKind::DynamicsCompressor(c) => match key {
            "threshold" => {
                if let Some(v) = n() {
                    c.threshold.value = v as f32
                }
            }
            "knee" => {
                if let Some(v) = n() {
                    c.knee.value = v as f32
                }
            }
            "ratio" => {
                if let Some(v) = n() {
                    c.ratio.value = v as f32
                }
            }
            "attack" => {
                if let Some(v) = n() {
                    c.attack.value = v as f32
                }
            }
            "release" => {
                if let Some(v) = n() {
                    c.release.value = v as f32
                }
            }
            _ => {}
        },
        NodeKind::WaveShaper(w) => match key {
            "shape" => {
                if let Some(s) = t() {
                    w.shape = parse_waveshaper_shape(&s);
                }
            }
            "amount" => {
                if let Some(v) = n() {
                    w.amount = v as f32;
                }
            }
            "oversample" => {
                if let Some(s) = t() {
                    w.oversample = parse_oversample(&s);
                }
            }
            "curve" => {
                if let Some(s) = t() {
                    w.curve = parse_f32_list(&s);
                }
            }
            _ => {}
        },
        NodeKind::Convolver(c) => match key {
            "disable_normalization" => {
                if let Some(v) = b() {
                    c.disable_normalization = v;
                }
            }
            "reverb_seconds" => {
                if let Some(v) = n() {
                    c.reverb_seconds = (v as f32).max(0.05);
                }
            }
            _ => {}
        },
        NodeKind::ChannelSplitter(s) => {
            if let Some(v) = n() {
                s.number_of_outputs = (v as u32).clamp(1, 32);
            }
        }
        NodeKind::ChannelMerger(m) => {
            if let Some(v) = n() {
                m.number_of_inputs = (v as u32).clamp(1, 32);
            }
        }
        NodeKind::StereoPanner(p) => {
            if let Some(v) = n() {
                p.pan.value = v as f32;
            }
        }
        NodeKind::Panner(p) => match key {
            "panning_model" => {
                if let Some(s) = t() {
                    p.panning_model = parse_panning(&s)
                }
            }
            "distance_model" => {
                if let Some(s) = t() {
                    p.distance_model = parse_distance(&s)
                }
            }
            "position_x" => {
                if let Some(v) = n() {
                    p.position_x.value = v as f32
                }
            }
            "position_y" => {
                if let Some(v) = n() {
                    p.position_y.value = v as f32
                }
            }
            "position_z" => {
                if let Some(v) = n() {
                    p.position_z.value = v as f32
                }
            }
            _ => {}
        },
        NodeKind::Analyser(a) => match key {
            "fft_size" => {
                if let Some(s) = t() {
                    if let Ok(v) = s.parse() {
                        a.fft_size = v
                    }
                }
            }
            "min_decibels" => {
                if let Some(v) = n() {
                    a.min_decibels = v
                }
            }
            "max_decibels" => {
                if let Some(v) = n() {
                    a.max_decibels = v
                }
            }
            "smoothing_time_constant" => {
                if let Some(v) = n() {
                    a.smoothing_time_constant = v
                }
            }
            _ => {}
        },
        NodeKind::MediaElementSource(m) => {
            if let Some(s) = t() {
                m.src = s;
            }
        }
        NodeKind::Output(o) if key == "gain" => {
            if let Some(v) = n() {
                o.gain.value = v as f32
            }
        }
        NodeKind::SpatialOutput(o) => match key {
            "gain" => {
                if let Some(v) = n() {
                    o.gain.value = v as f32
                }
            }
            "position_x" => {
                if let Some(v) = n() {
                    o.position_x.value = v as f32
                }
            }
            "position_y" => {
                if let Some(v) = n() {
                    o.position_y.value = v as f32
                }
            }
            "position_z" => {
                if let Some(v) = n() {
                    o.position_z.value = v as f32
                }
            }
            _ => {}
        },
        NodeKind::AudioWorklet(w) => {
            if let (Some(v), Some(p)) = (n(), w.parameters.iter_mut().find(|p| p.name.0 == key)) {
                p.param.value = v as f32;
            }
        }
        _ => {}
    }
}

fn as_num(v: &FieldValue) -> Option<f64> {
    match v {
        FieldValue::Num(n) => Some(*n),
        _ => None,
    }
}
fn as_bool(v: &FieldValue) -> Option<bool> {
    match v {
        FieldValue::Bool(b) => Some(*b),
        _ => None,
    }
}
fn as_text(v: &FieldValue) -> Option<String> {
    match v {
        FieldValue::Text(s) => Some(s.clone()),
        _ => None,
    }
}

fn noise_flavor_str(f: NoiseFlavor) -> &'static str {
    match f {
        NoiseFlavor::White => "white",
        NoiseFlavor::Pink => "pink",
        NoiseFlavor::Brown => "brown",
        NoiseFlavor::Blue => "blue",
        NoiseFlavor::Violet => "violet",
        NoiseFlavor::Dust => "dust",
        NoiseFlavor::Velvet => "velvet",
    }
}
fn parse_noise_flavor(s: &str) -> NoiseFlavor {
    match s {
        "pink" => NoiseFlavor::Pink,
        "brown" => NoiseFlavor::Brown,
        "blue" => NoiseFlavor::Blue,
        "violet" => NoiseFlavor::Violet,
        "dust" => NoiseFlavor::Dust,
        "velvet" => NoiseFlavor::Velvet,
        _ => NoiseFlavor::White,
    }
}

fn osc_type_str(t: OscillatorType) -> &'static str {
    match t {
        OscillatorType::Sine => "sine",
        OscillatorType::Square => "square",
        OscillatorType::Sawtooth => "sawtooth",
        OscillatorType::Triangle => "triangle",
        OscillatorType::Custom => "custom",
    }
}
fn parse_osc(s: &str) -> OscillatorType {
    match s {
        "square" => OscillatorType::Square,
        "sawtooth" => OscillatorType::Sawtooth,
        "triangle" => OscillatorType::Triangle,
        "custom" => OscillatorType::Custom,
        _ => OscillatorType::Sine,
    }
}
fn biquad_type_str(t: BiquadFilterType) -> &'static str {
    match t {
        BiquadFilterType::Lowpass => "lowpass",
        BiquadFilterType::Highpass => "highpass",
        BiquadFilterType::Bandpass => "bandpass",
        BiquadFilterType::Lowshelf => "lowshelf",
        BiquadFilterType::Highshelf => "highshelf",
        BiquadFilterType::Peaking => "peaking",
        BiquadFilterType::Notch => "notch",
        BiquadFilterType::Allpass => "allpass",
    }
}
fn parse_biquad(s: &str) -> BiquadFilterType {
    match s {
        "highpass" => BiquadFilterType::Highpass,
        "bandpass" => BiquadFilterType::Bandpass,
        "lowshelf" => BiquadFilterType::Lowshelf,
        "highshelf" => BiquadFilterType::Highshelf,
        "peaking" => BiquadFilterType::Peaking,
        "notch" => BiquadFilterType::Notch,
        "allpass" => BiquadFilterType::Allpass,
        _ => BiquadFilterType::Lowpass,
    }
}
fn oversample_str(t: OverSampleType) -> &'static str {
    match t {
        OverSampleType::None => "none",
        OverSampleType::X2 => "2x",
        OverSampleType::X4 => "4x",
    }
}
fn parse_oversample(s: &str) -> OverSampleType {
    match s {
        "2x" => OverSampleType::X2,
        "4x" => OverSampleType::X4,
        _ => OverSampleType::None,
    }
}
fn waveshaper_shape_str(s: WaveShaperShape) -> &'static str {
    match s {
        WaveShaperShape::Tanh => "tanh",
        WaveShaperShape::HardClip => "hard_clip",
        WaveShaperShape::Fold => "fold",
        WaveShaperShape::Custom => "custom",
    }
}
fn parse_waveshaper_shape(s: &str) -> WaveShaperShape {
    match s {
        "hard_clip" => WaveShaperShape::HardClip,
        "fold" => WaveShaperShape::Fold,
        "custom" => WaveShaperShape::Custom,
        _ => WaveShaperShape::Tanh,
    }
}
fn panning_model_str(t: PanningModelType) -> &'static str {
    match t {
        PanningModelType::EqualPower => "equalpower",
        PanningModelType::Hrtf => "HRTF",
    }
}
fn parse_panning(s: &str) -> PanningModelType {
    match s {
        "HRTF" => PanningModelType::Hrtf,
        _ => PanningModelType::EqualPower,
    }
}
fn distance_model_str(t: DistanceModelType) -> &'static str {
    match t {
        DistanceModelType::Linear => "linear",
        DistanceModelType::Inverse => "inverse",
        DistanceModelType::Exponential => "exponential",
    }
}
fn parse_distance(s: &str) -> DistanceModelType {
    match s {
        "linear" => DistanceModelType::Linear,
        "exponential" => DistanceModelType::Exponential,
        _ => DistanceModelType::Inverse,
    }
}

// ======================================================================
// Automation (envelope) access — used by the inspector.
// ======================================================================

/// A node's automatable AudioParam, for the inspector.
pub struct ParamInfo {
    pub key: &'static str,
    pub label: &'static str,
    pub value: f32,
    pub automation: Vec<AutomationEvent>,
}

fn pi(key: &'static str, label: &'static str, p: &AudioParam) -> ParamInfo {
    ParamInfo {
        key,
        label,
        value: p.value,
        automation: p.automation.clone(),
    }
}

/// Every automatable AudioParam on `kind` (name, label, value, automation).
pub fn audio_params(kind: &NodeKind) -> Vec<ParamInfo> {
    match kind {
        NodeKind::Oscillator(o) => vec![
            pi("frequency", "freq (Hz)", &o.frequency),
            pi("detune", "detune", &o.detune),
        ],
        NodeKind::ConstantSource(c) => vec![pi("offset", "offset", &c.offset)],
        NodeKind::AudioBufferSource(b) => vec![
            pi("playbackRate", "rate", &b.playback_rate),
            pi("detune", "detune", &b.detune),
        ],
        NodeKind::Gain(g) => vec![pi("gain", "gain", &g.gain)],
        NodeKind::BiquadFilter(b) => vec![
            pi("frequency", "freq (Hz)", &b.frequency),
            pi("detune", "detune", &b.detune),
            pi("Q", "Q", &b.q),
            pi("gain", "gain (dB)", &b.gain),
        ],
        NodeKind::Delay(d) => vec![pi("delayTime", "delay (s)", &d.delay_time)],
        NodeKind::DynamicsCompressor(c) => vec![
            pi("threshold", "threshold", &c.threshold),
            pi("knee", "knee", &c.knee),
            pi("ratio", "ratio", &c.ratio),
            pi("attack", "attack", &c.attack),
            pi("release", "release", &c.release),
        ],
        NodeKind::StereoPanner(p) => vec![pi("pan", "pan", &p.pan)],
        NodeKind::Panner(p) => vec![
            pi("positionX", "pos x", &p.position_x),
            pi("positionY", "pos y", &p.position_y),
            pi("positionZ", "pos z", &p.position_z),
        ],
        NodeKind::Output(o) => vec![pi("gain", "gain", &o.gain)],
        NodeKind::SpatialOutput(o) => vec![
            pi("gain", "gain", &o.gain),
            pi("positionX", "pos x", &o.position_x),
            pi("positionY", "pos y", &o.position_y),
            pi("positionZ", "pos z", &o.position_z),
        ],
        NodeKind::AudioWorklet(w) => w
            .parameters
            .iter()
            .map(|p| pi(intern(&p.name.0), intern(&p.name.0), &p.param))
            .collect(),
        _ => vec![],
    }
}

/// Replace the automation timeline of a named AudioParam on `kind`.
pub fn set_automation(kind: &mut NodeKind, key: &str, events: Vec<AutomationEvent>) {
    let target: Option<&mut AudioParam> = match kind {
        NodeKind::Oscillator(o) => match key {
            "frequency" => Some(&mut o.frequency),
            "detune" => Some(&mut o.detune),
            _ => None,
        },
        NodeKind::ConstantSource(c) if key == "offset" => Some(&mut c.offset),
        NodeKind::AudioBufferSource(b) => match key {
            "playbackRate" => Some(&mut b.playback_rate),
            "detune" => Some(&mut b.detune),
            _ => None,
        },
        NodeKind::Gain(g) if key == "gain" => Some(&mut g.gain),
        NodeKind::BiquadFilter(b) => match key {
            "frequency" => Some(&mut b.frequency),
            "detune" => Some(&mut b.detune),
            "Q" => Some(&mut b.q),
            "gain" => Some(&mut b.gain),
            _ => None,
        },
        NodeKind::Delay(d) if key == "delayTime" => Some(&mut d.delay_time),
        NodeKind::DynamicsCompressor(c) => match key {
            "threshold" => Some(&mut c.threshold),
            "knee" => Some(&mut c.knee),
            "ratio" => Some(&mut c.ratio),
            "attack" => Some(&mut c.attack),
            "release" => Some(&mut c.release),
            _ => None,
        },
        NodeKind::StereoPanner(p) if key == "pan" => Some(&mut p.pan),
        NodeKind::Panner(p) => match key {
            "positionX" => Some(&mut p.position_x),
            "positionY" => Some(&mut p.position_y),
            "positionZ" => Some(&mut p.position_z),
            _ => None,
        },
        NodeKind::Output(o) if key == "gain" => Some(&mut o.gain),
        NodeKind::SpatialOutput(o) => match key {
            "gain" => Some(&mut o.gain),
            "positionX" => Some(&mut o.position_x),
            "positionY" => Some(&mut o.position_y),
            "positionZ" => Some(&mut o.position_z),
            _ => None,
        },
        NodeKind::AudioWorklet(w) => w
            .parameters
            .iter_mut()
            .find(|p| p.name.0 == key)
            .map(|p| &mut p.param),
        _ => None,
    };
    if let Some(p) = target {
        p.automation = events;
    }
}

// ======================================================================
// AutomationEvent accessors — used by the graphical envelope editor.
// ======================================================================

/// The time of an event on the timeline (start_time for target/curve).
pub fn event_time(ev: &AutomationEvent) -> f64 {
    match ev {
        AutomationEvent::SetValue { time, .. }
        | AutomationEvent::LinearRamp { time, .. }
        | AutomationEvent::ExponentialRamp { time, .. }
        | AutomationEvent::CancelScheduled { time }
        | AutomationEvent::CancelAndHold { time } => *time,
        AutomationEvent::SetTarget { start_time, .. }
        | AutomationEvent::SetValueCurve { start_time, .. } => *start_time,
    }
}

/// The target value of an event, when it has one (set / ramps / target).
pub fn event_value(ev: &AutomationEvent) -> Option<f32> {
    match ev {
        AutomationEvent::SetValue { value, .. }
        | AutomationEvent::LinearRamp { value, .. }
        | AutomationEvent::ExponentialRamp { value, .. } => Some(*value),
        AutomationEvent::SetTarget { target, .. } => Some(*target),
        _ => None,
    }
}

/// Return `ev` with its value+time updated, preserving its kind (no-op for the
/// kinds the graphical editor doesn't move).
pub fn set_event_vt(ev: &AutomationEvent, value: f32, time: f64) -> AutomationEvent {
    match ev {
        AutomationEvent::SetValue { .. } => AutomationEvent::SetValue { value, time },
        AutomationEvent::LinearRamp { .. } => AutomationEvent::LinearRamp { value, time },
        AutomationEvent::ExponentialRamp { .. } => AutomationEvent::ExponentialRamp { value, time },
        other => other.clone(),
    }
}

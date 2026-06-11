//! The palette catalog: which node kinds exist, grouped into sections, plus a
//! one-paragraph description of what each does (shown in the `?` help modal).

use awsm_audio_schema::NodeKind;

use crate::ports::kind_label;

/// A named group of node kinds for the palette.
pub struct Section {
    pub name: &'static str,
    pub kinds: Vec<NodeKind>,
}

/// The palette, grouped. Each kind is constructed with WebAudio defaults.
pub fn sections() -> Vec<Section> {
    vec![
        Section {
            name: "Sources",
            kinds: vec![
                NodeKind::Oscillator(Default::default()),
                NodeKind::Noise(Default::default()),
                NodeKind::ConstantSource(Default::default()),
                NodeKind::AudioBufferSource(Default::default()),
                NodeKind::MediaElementSource(Default::default()),
                NodeKind::MediaStreamSource(Default::default()),
            ],
        },
        Section {
            name: "Effects",
            kinds: vec![
                NodeKind::Gain(Default::default()),
                NodeKind::BiquadFilter(Default::default()),
                NodeKind::IirFilter(Default::default()),
                NodeKind::Delay(Default::default()),
                NodeKind::DynamicsCompressor(Default::default()),
                NodeKind::WaveShaper(Default::default()),
                NodeKind::Convolver(Default::default()),
            ],
        },
        Section {
            name: "Spatial",
            kinds: vec![
                NodeKind::StereoPanner(Default::default()),
                NodeKind::Panner(Default::default()),
            ],
        },
        Section {
            name: "Routing",
            kinds: vec![
                NodeKind::ChannelSplitter(Default::default()),
                NodeKind::ChannelMerger(Default::default()),
            ],
        },
        Section {
            name: "Analysis",
            kinds: vec![NodeKind::Analyser(Default::default())],
        },
        Section {
            name: "Output",
            kinds: vec![
                NodeKind::Output(Default::default()),
                NodeKind::SpatialOutput(Default::default()),
            ],
        },
        Section {
            name: "Sequencing",
            kinds: vec![
                // Two single-mode sequencers: melodic (pitched instrument tracks)
                // and drum (each note row is its own percussion output). Both are a
                // NoteSequencer under the hood, distinguished by `mode`.
                NodeKind::NoteSequencer(awsm_audio_schema::NoteSequencerNode {
                    mode: awsm_audio_schema::SequencerMode::Melodic,
                    ..Default::default()
                }),
                NodeKind::NoteSequencer(awsm_audio_schema::NoteSequencerNode {
                    mode: awsm_audio_schema::SequencerMode::Drum,
                    ..Default::default()
                }),
                NodeKind::ControlSequencer(Default::default()),
                NodeKind::Bus(Default::default()),
            ],
        },
        Section {
            name: "Advanced",
            kinds: vec![NodeKind::AudioWorklet(Default::default())],
        },
    ]
}

/// Help shown in the node info modal.
#[derive(Clone, Copy)]
pub struct NodeDoc {
    pub title: &'static str,
    pub body: &'static str,
    /// MDN reference page for the underlying WebAudio interface.
    pub mdn: &'static str,
}

/// Help for a node kind.
pub fn doc(kind: &NodeKind) -> NodeDoc {
    NodeDoc {
        title: kind_label(kind),
        body: doc_body(kind),
        mdn: mdn_url(kind),
    }
}

/// MDN page for the node's WebAudio interface.
fn mdn_url(kind: &NodeKind) -> &'static str {
    macro_rules! u {
        ($iface:literal) => {
            concat!("https://developer.mozilla.org/en-US/docs/Web/API/", $iface)
        };
    }
    match kind {
        NodeKind::Oscillator(_) => u!("OscillatorNode"),
        NodeKind::ConstantSource(_) => u!("ConstantSourceNode"),
        // Noise is synthesized into a buffer source; the closest reference.
        NodeKind::Noise(_) => u!("AudioBufferSourceNode"),
        NodeKind::AudioBufferSource(_) => u!("AudioBufferSourceNode"),
        NodeKind::MediaElementSource(_) => u!("MediaElementAudioSourceNode"),
        NodeKind::MediaStreamSource(_) => u!("MediaStreamAudioSourceNode"),
        NodeKind::Gain(_) => u!("GainNode"),
        NodeKind::BiquadFilter(_) => u!("BiquadFilterNode"),
        NodeKind::IirFilter(_) => u!("IIRFilterNode"),
        NodeKind::Delay(_) => u!("DelayNode"),
        NodeKind::DynamicsCompressor(_) => u!("DynamicsCompressorNode"),
        NodeKind::WaveShaper(_) => u!("WaveShaperNode"),
        NodeKind::Convolver(_) => u!("ConvolverNode"),
        NodeKind::StereoPanner(_) => u!("StereoPannerNode"),
        NodeKind::Panner(_) => u!("PannerNode"),
        NodeKind::Analyser(_) => u!("AnalyserNode"),
        NodeKind::ChannelSplitter(_) => u!("ChannelSplitterNode"),
        NodeKind::ChannelMerger(_) => u!("ChannelMergerNode"),
        NodeKind::AudioWorklet(_) => u!("AudioWorkletNode"),
        NodeKind::Output(_) => u!("AudioDestinationNode"),
        NodeKind::SpatialOutput(_) => u!("PannerNode"),
        NodeKind::Sample(_) => u!("AudioNode"),
        // No direct WebAudio interface — it's a sequencer concept.
        NodeKind::NoteSequencer(_) | NodeKind::ControlSequencer(_) => {
            "https://developer.mozilla.org/en-US/docs/Web/API/Web_Audio_API"
        }
        NodeKind::Bus(_) => "https://developer.mozilla.org/en-US/docs/Web/API/GainNode",
    }
}

fn doc_body(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::Oscillator(_) => {
            "Generates a periodic tone (sine, square, sawtooth, triangle, or 'custom' — a \
             wave built from a list of harmonic amplitudes). 'frequency' sets the pitch in \
             Hz; 'detune' shifts it in cents. The building block for synthesized sound."
        }
        NodeKind::ConstantSource(_) => {
            "A constant-value source — the 'float' primitive. It continuously outputs a \
             single number, set by 'offset' (the value held on its output, e.g. 0.5 or \
             440). Wire that output into another node's parameter to set or bias it (e.g. \
             a steady filter cutoff or detune), into a sample inlet to feed a fixed value \
             in, or into an audio input as a DC signal. 'offset' is itself a modulatable \
             parameter, so you can also drive it from an envelope or LFO."
        }
        NodeKind::Noise(_) => {
            "Synthesized noise — the raw material for organic/textural sounds (rain, fire, \
             wind, surf). Pick a flavor: white/pink/brown/blue/violet are continuous colors \
             (different spectral tilt), while dust and velvet are sparse impulses (droplets, \
             crackle) set by 'density'. 'seed' makes the texture reproducible. Combine with \
             filters and an amplitude envelope to shape it."
        }
        NodeKind::AudioBufferSource(_) => {
            "Plays back a loaded audio clip. Pick an audio file (wav/mp3/flac/…); 'rate' \
             changes speed/pitch and 'loop' repeats it."
        }
        NodeKind::MediaElementSource(_) => {
            "Pulls audio from an <audio>/<video> element by URL, so you can route external \
             media through the graph."
        }
        NodeKind::MediaStreamSource(_) => {
            "Pulls audio from a live MediaStream (e.g. the microphone). The stream is bound \
             at runtime."
        }
        NodeKind::Gain(_) => {
            "Multiplies the signal by 'gain' — the basic volume/level control. Wire a \
             control signal into its gain param for tremolo or envelopes."
        }
        NodeKind::BiquadFilter(_) => {
            "A common second-order filter: lowpass, highpass, bandpass, shelf, peaking, \
             notch, or allpass. 'frequency' is the cutoff/center; 'Q' the resonance."
        }
        NodeKind::IirFilter(_) => {
            "A general infinite-impulse-response filter defined by raw feedforward/feedback \
             coefficients (comma-separated lists in the inspector). Defaults to a gentle \
             one-pole low-pass."
        }
        NodeKind::Delay(_) => {
            "Delays the signal by 'delay' seconds (up to 'max delay'). Feed its output back \
             through a gain for echo/feedback effects."
        }
        NodeKind::DynamicsCompressor(_) => {
            "Reduces dynamic range: signal above 'threshold' is attenuated by 'ratio', with \
             'attack'/'release' shaping how fast it reacts. Good for glue and loudness."
        }
        NodeKind::WaveShaper(_) => {
            "Non-linear distortion/saturation. Pick a 'shape' (tanh = warm, hard clip = \
             aggressive, fold = metallic wavefolder); 'amount' is the drive. Choose \
             'custom' to draw your own transfer curve in the inspector (input → output)."
        }
        NodeKind::Convolver(_) => {
            "Convolves the signal with an impulse response — the standard way to do reverb \
             or model a space. Load an IR audio file on the node; without one it uses a \
             synthetic decaying-noise reverb."
        }
        NodeKind::StereoPanner(_) => {
            "Positions the signal in the stereo field with a single 'pan' control from -1 \
             (left) to +1 (right)."
        }
        NodeKind::Panner(_) => {
            "Full 3D positional audio: places the source in space relative to a listener, \
             with distance attenuation and directional cones."
        }
        NodeKind::Analyser(_) => {
            "Passes audio through unchanged while exposing real-time frequency/time-domain \
             data — used for visualizers like the waveform below."
        }
        NodeKind::ChannelSplitter(_) => {
            "Splits a multi-channel signal into separate mono outputs, one per channel, for \
             independent processing."
        }
        NodeKind::ChannelMerger(_) => {
            "Combines several mono inputs into one multi-channel output — the inverse of the \
             splitter."
        }
        NodeKind::AudioWorklet(_) => {
            "Runs your own DSP from a .wasm module (stereo in → stereo out). Pick a .wasm that \
             exports the awsm-audio worklet ABI; its parameters are auto-discovered and become \
             editable, automatable, modulation-targetable knobs like any other node. The escape \
             hatch for anything the built-in nodes can't do. Reach for it when you need DSP no \
             built-in node provides: chorus / flanger (modulated delay), phaser (all-pass \
             chain), bitcrusher, ring modulator, custom grain / spectral effects. To author one, \
             read the awsm-audio://docs/worklet-abi resource (crate API at \
             https://docs.rs/awsm-audio-worklet/latest), get a Cargo.toml from the \
             worklet_cargo_toml tool, build to wasm32, and attach with attach_wasm."
        }
        NodeKind::Output(_) => {
            "The audible output (speakers) — a plain stereo sink. Wire your final mix into it \
             and set its gain. (Graphs without an Output node still play: any unconnected node \
             auto-routes to the speakers.) For 3D placement, use a Spatial Output instead."
        }
        NodeKind::SpatialOutput(_) => {
            "The audible output placed in 3D space (HRTF). Wire your final mix in, then set the \
             position (x/y/z) — those positions are the natural thing a game/runtime adjusts to \
             move the sound around the listener, without touching the rest of the graph."
        }
        NodeKind::Sample(_) => {
            "An instance of another sample, embedded as a sub-graph — how larger patches are \
             composed from smaller reusable ones."
        }
        NodeKind::NoteSequencer(s) if s.mode.is_drum() => {
            "A Drum Sequencer. Each track is a drum kit: every distinct note row (kick, snare, \
             hat…) becomes its own output you wire to a separate instrument. Draw hits in the \
             piano roll. Makes no sound itself — it triggers the instruments you wire to it."
        }
        NodeKind::NoteSequencer(_) => {
            "A Melodic Sequencer. Each track is one pitched instrument: the track is a single \
             output, and each note sets the pitch. Load a .mid or draw notes in the piano roll. \
             Makes no sound itself — it triggers the instruments you wire to it."
        }
        NodeKind::ControlSequencer(_) => {
            "Automates parameters over time. Each lane is an output you wire to a node's \
             parameter; the lane's value-over-time drives it during playback."
        }
        NodeKind::Bus(_) => {
            "A summing bus (a unity gain). Wire several sounds into it, then route the bus \
             into effects and the Output."
        }
    }
}

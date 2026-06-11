//! Reference sound-design compositions, built from the schema. These double as
//! the editor's "Examples" menu and as the `examples/*.toml` files (generated
//! by the `generate_example_files` test). Each returns a one-sample
//! [`SampleLibrary`] whose terminal nodes the player routes to the speakers.
//!
//! They lean on automation (envelopes / sweeps) and the [`NoiseNode`] to cover
//! a range: a tonal additive bell, layered-noise rain & fire, a pitch-swept
//! laser, and a long evolving rocket launch.

use crate::*;

/// All built-in examples as `(name, library)`.
pub fn all() -> Vec<(&'static str, SampleLibrary)> {
    vec![
        ("bell", bell()),
        ("rain", rain()),
        ("fire", fire()),
        ("laser", laser()),
        ("rocket", rocket()),
        ("kick", kick()),
        ("hihat", hihat()),
        ("siren", siren()),
        ("wobble", wobble()),
        ("wind", wind()),
        ("spatial", spatial()),
        ("crush", crush()),
        ("drive", drive()),
        ("ringmod", ringmod()),
        ("nested", nested()),
        ("chord", chord()),
        ("acidrack", acidrack()),
        ("song", song()),
        ("arrangement", arrangement()),
    ]
}

// ======================================================================
// Nested — a composite: a reusable "voice" sub-sample (inlet → low-pass →
// outlet) referenced by a root that drives it from a saw and trims the level.
// Demonstrates SampleRef composition (flattened by the player).
// ======================================================================

fn nested() -> SampleLibrary {
    use crate::{ConnectionSink, ConnectionSource, PortDecl, PortId, SampleRef};

    // Sub-sample: inlet "in" → low-pass → outlet "out".
    let mut voice = Sample::new("Voice");
    let lp = voice
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(800.0), 1.0)));
    voice.graph.inlets.push(PortDecl::new("in"));
    voice.graph.outlets.push(PortDecl::new("out"));
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("in"),
        },
        to: ConnectionSink::NodeInput { node: lp, input: 0 },
    });
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: lp,
            output: 0,
        },
        to: ConnectionSink::Outlet {
            port: PortId::from("out"),
        },
    });

    // Root: saw → Sample(Voice) → gain.
    let mut root = Sample::new("Nested");
    let saw = root
        .graph
        .push_node(Node::new(osc(p(110.0), OscillatorType::Sawtooth)));
    let voice_ref = root.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: voice.id,
        inputs: vec![],
    })));
    let g = root.graph.push_node(Node::new(gain(p(0.5))));
    root.graph.connect(Connection::node_to_node(saw, voice_ref));
    root.graph.connect(Connection::node_to_node(voice_ref, g));

    SampleLibrary {
        root: Some(root.id),
        samples: vec![root, voice],
        ..Default::default()
    }
}

// ======================================================================
// Chord Stab — ONE reusable plucked "Voice" sub-sample (saw → resonant
// low-pass → percussive amp envelope), exposing `pitch` + `cutoff` macros,
// referenced four times to voice an Am7 chord. Each instance overrides the
// macros (its note + brightness) and is panned across the stereo field; the
// whole stack runs through a `drive` WASM worklet for glue. Showcases macro
// overrides driving real, audible per-instance differences from one sample.
// ======================================================================

fn chord() -> SampleLibrary {
    use crate::{ConnectionSink, ConnectionSource, InputValue, PortDecl, PortId, SampleRef};

    // --- Reusable voice: saw → low-pass → pluck envelope → outlet, with two
    // control inputs (pitch, cutoff) that *set* the osc + filter frequency. ---
    let mut voice = Sample::new("Voice");
    let saw = voice
        .graph
        .push_node(Node::new(osc(p(220.0), OscillatorType::Sawtooth)));
    let lp = voice
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(2000.0), 7.0)));
    // Moving envelope: a fast pluck that rings for ~1.1s.
    let amp = voice
        .graph
        .push_node(Node::new(gain(env_perc(0.4, 0.005, 1.1))));
    // Inputs: a value drives (sets) the bound param; `default` is the standalone
    // value, which also matches the node's base so the voice sounds right alone.
    voice.graph.inlets.push(PortDecl {
        id: PortId::from("pitch"),
        label: None,
        default: 220.0,
    });
    voice.graph.inlets.push(PortDecl {
        id: PortId::from("cutoff"),
        label: None,
        default: 2000.0,
    });
    voice.graph.outlets.push(PortDecl::new("out"));
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("pitch"),
        },
        to: ConnectionSink::NodeParam {
            node: saw,
            param: "frequency".into(),
        },
    });
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("cutoff"),
        },
        to: ConnectionSink::NodeParam {
            node: lp,
            param: "frequency".into(),
        },
    });
    voice.graph.connect(Connection::node_to_node(saw, lp));
    voice.graph.connect(Connection::node_to_node(lp, amp));
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: amp,
            output: 0,
        },
        to: ConnectionSink::Outlet {
            port: PortId::from("out"),
        },
    });

    // --- Root: four voices (Am7: A C E G), each panned, into a drive bus. ---
    let mut root = Sample::new("Chord Stab");
    let mut assets = Vec::new();
    let mix = root.graph.push_node(Node::new(gain(p(0.5))));

    // (frequency, cutoff, pan) per chord tone — brightness rises with pitch.
    let tones = [
        (220.00_f32, 1400.0_f32, -0.6_f32), // A3
        (261.63, 1900.0, -0.2),             // C4
        (329.63, 2600.0, 0.25),             // E4
        (392.00, 3400.0, 0.6),              // G4
    ];
    for (freq, cutoff, pan) in tones {
        let v = root.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
            sample: voice.id,
            inputs: vec![
                InputValue {
                    port: PortId::from("pitch"),
                    value: freq,
                },
                InputValue {
                    port: PortId::from("cutoff"),
                    value: cutoff,
                },
            ],
        })));
        let pan_node = root
            .graph
            .push_node(Node::new(NodeKind::StereoPanner(StereoPannerNode {
                pan: p(pan),
            })));
        root.graph.connect(Connection::node_to_node(v, pan_node));
        root.graph.connect(Connection::node_to_node(pan_node, mix));
    }

    // Glue the stack through a tanh overdrive worklet, then master trim.
    let dr = wmod(
        &mut root,
        "drive",
        DRIVE_WASM,
        vec![
            wparam("drive", 1.0, 30.0, 6.0),
            wparam("mix", 0.0, 1.0, 0.85),
            wparam("level", 0.0, 1.0, 0.7),
        ],
        &mut assets,
    );
    let master = root.graph.push_node(Node::new(gain(p(0.9))));
    root.graph.connect(Connection::node_to_node(mix, dr));
    root.graph.connect(Connection::node_to_node(dr, master));

    let mut l = SampleLibrary {
        root: Some(root.id),
        samples: vec![root, voice],
        ..Default::default()
    };
    l.assets.wasm_modules = assets;
    l
}

// ======================================================================
// Acid Rack — a reusable two-worklet "FX Rack" sub-sample (inlet →
// bitcrusher → ring modulator → outlet) exposing `crush` + `ring` macros,
// dropped into a 303-style acid line: detuned saws → FX rack (overridden
// grittier) → resonant low-pass whose cutoff is BOTH envelope-swept and
// LFO-wobbled at once → amp envelope, with a parallel slap delay. Shows two
// WASM worklets nested behind a boundary, macro overrides, and stacked moving
// envelopes (sweep + LFO + pluck) on one parameter.
// ======================================================================

fn acidrack() -> SampleLibrary {
    use crate::{ConnectionSink, ConnectionSource, InputValue, PortDecl, PortId, SampleRef};

    let mut assets = Vec::new();

    // --- FX rack sub-sample: audio inlet → bitcrusher → ringmod → outlet, plus
    // two control inputs (crush → bits, ring → freq) that set those params. ---
    let mut fx = Sample::new("FX Rack");
    fx.graph.inlets.push(PortDecl::new("in"));
    fx.graph.inlets.push(PortDecl {
        id: PortId::from("crush"),
        label: None,
        default: 8.0,
    });
    fx.graph.inlets.push(PortDecl {
        id: PortId::from("ring"),
        label: None,
        default: 220.0,
    });
    fx.graph.outlets.push(PortDecl::new("out"));
    let cr = wmod(
        &mut fx,
        "bitcrusher",
        BITCRUSHER_WASM,
        vec![
            wparam("bits", 1.0, 16.0, 8.0),
            wparam("reduction", 1.0, 32.0, 4.0),
        ],
        &mut assets,
    );
    let rm = wmod(
        &mut fx,
        "ringmod",
        RINGMOD_WASM,
        vec![
            wparam("freq", 20.0, 2000.0, 220.0),
            wparam("mix", 0.0, 1.0, 0.6),
        ],
        &mut assets,
    );
    fx.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("in"),
        },
        to: ConnectionSink::NodeInput { node: cr, input: 0 },
    });
    fx.graph.connect(Connection::node_to_node(cr, rm));
    fx.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: rm,
            output: 0,
        },
        to: ConnectionSink::Outlet {
            port: PortId::from("out"),
        },
    });
    // Control inputs set the worklet params.
    fx.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("crush"),
        },
        to: ConnectionSink::NodeParam {
            node: cr,
            param: "bits".into(),
        },
    });
    fx.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("ring"),
        },
        to: ConnectionSink::NodeParam {
            node: rm,
            param: "freq".into(),
        },
    });

    // --- Root: detuned saws → FX rack (grittier overrides) → swept filter. ---
    let mut root = Sample::new("Acid Rack");
    let o1 = root
        .graph
        .push_node(Node::new(osc(p(110.0), OscillatorType::Sawtooth)));
    let o2 = root
        .graph
        .push_node(Node::new(osc_detuned(110.0, OscillatorType::Sawtooth, 8.0)));
    let pre = root.graph.push_node(Node::new(gain(p(0.5))));
    let fx_ref = root.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: fx.id,
        inputs: vec![
            InputValue {
                port: PortId::from("crush"),
                value: 4.0,
            },
            InputValue {
                port: PortId::from("ring"),
                value: 160.0,
            },
        ],
    })));
    // Resonant low-pass: cutoff exponentially dives 3500→350 (the acid pluck)
    // AND is wobbled by the LFO below — two moving envelopes on one param.
    let lp = root.graph.push_node(Node::new(biquad(
        BiquadFilterType::Lowpass,
        exp_sweep(3500.0, 350.0, 0.7),
        11.0,
    )));
    let amp = root
        .graph
        .push_node(Node::new(gain(env_perc(0.6, 0.01, 1.3))));
    // Parallel slap echo for space.
    let delay = root.graph.push_node(Node::new(NodeKind::Delay(DelayNode {
        max_delay_time: 0.5,
        delay_time: p(0.19),
    })));
    let echo = root.graph.push_node(Node::new(gain(p(0.3))));
    let master = root.graph.push_node(Node::new(gain(p(0.9))));

    root.graph.connect(Connection::node_to_node(o1, pre));
    root.graph.connect(Connection::node_to_node(o2, pre));
    root.graph.connect(Connection::node_to_node(pre, fx_ref));
    root.graph.connect(Connection::node_to_node(fx_ref, lp));
    root.graph.connect(Connection::node_to_node(lp, amp));
    root.graph.connect(Connection::node_to_node(amp, master));
    root.graph.connect(Connection::node_to_node(amp, delay));
    root.graph.connect(Connection::node_to_node(delay, echo));
    root.graph.connect(Connection::node_to_node(echo, master));

    // LFO (5 Hz) → depth → low-pass cutoff (adds on top of the sweep).
    let lfo = root
        .graph
        .push_node(Node::new(osc(p(5.0), OscillatorType::Sine)));
    let depth = root.graph.push_node(Node::new(gain(p(700.0))));
    root.graph.connect(Connection::node_to_node(lfo, depth));
    root.graph.connect(modulate(depth, lp, "frequency"));

    let mut l = SampleLibrary {
        root: Some(root.id),
        samples: vec![root, fx],
        ..Default::default()
    };
    l.assets.wasm_modules = assets;
    l
}

/// Bundled example WASM DSP modules, embedded so the worklet examples are
/// self-contained. Built from `packages/worklets/*`.
const BITCRUSHER_WASM: &[u8] = include_bytes!("../assets/bitcrusher.wasm");
const DRIVE_WASM: &[u8] = include_bytes!("../assets/drive.wasm");
const RINGMOD_WASM: &[u8] = include_bytes!("../assets/ringmod.wasm");

// ---- small construction helpers ----

fn lib(sample: Sample) -> SampleLibrary {
    SampleLibrary {
        root: Some(sample.id),
        samples: vec![sample],
        ..Default::default()
    }
}

fn p(value: f32) -> AudioParam {
    AudioParam::new(value)
}

/// Percussive amplitude envelope: fast exp attack to `peak`, exp decay to ~0.
fn env_perc(peak: f32, attack: f64, decay: f64) -> AudioParam {
    AudioParam {
        value: 0.0001,
        automation_rate: None,
        automation: vec![
            AutomationEvent::SetValue {
                value: 0.0001,
                time: 0.0,
            },
            AutomationEvent::ExponentialRamp {
                value: peak,
                time: attack,
            },
            AutomationEvent::ExponentialRamp {
                value: 0.0001,
                time: attack + decay,
            },
        ],
    }
}

/// Linear swell up to `peak` over `secs`, then hold. Linear (not exponential)
/// so the build-up is audible throughout instead of silent until the very end.
fn env_swell(peak: f32, secs: f64) -> AudioParam {
    AudioParam {
        value: 0.0,
        automation_rate: None,
        automation: vec![
            AutomationEvent::SetValue {
                value: 0.0,
                time: 0.0,
            },
            AutomationEvent::LinearRamp {
                value: peak,
                time: secs,
            },
        ],
    }
}

/// Exponential parameter sweep `start → end` over `secs` (e.g. a pitch dive).
fn exp_sweep(start: f32, end: f32, secs: f64) -> AudioParam {
    AudioParam {
        value: start,
        automation_rate: None,
        automation: vec![
            AutomationEvent::SetValue {
                value: start,
                time: 0.0,
            },
            AutomationEvent::ExponentialRamp {
                value: end,
                time: secs,
            },
        ],
    }
}

/// Linear parameter ramp `start → end` over `secs` (e.g. a rising filter cutoff).
fn lin_ramp(start: f32, end: f32, secs: f64) -> AudioParam {
    AudioParam {
        value: start,
        automation_rate: None,
        automation: vec![
            AutomationEvent::SetValue {
                value: start,
                time: 0.0,
            },
            AutomationEvent::LinearRamp {
                value: end,
                time: secs,
            },
        ],
    }
}

fn osc(freq: AudioParam, ty: OscillatorType) -> NodeKind {
    NodeKind::Oscillator(OscillatorNode {
        oscillator_type: ty,
        harmonics: Vec::new(),
        frequency: freq,
        detune: p(0.0),
    })
}

fn gain(g: AudioParam) -> NodeKind {
    NodeKind::Gain(GainNode { gain: g })
}

fn biquad(ty: BiquadFilterType, frequency: AudioParam, q: f32) -> NodeKind {
    NodeKind::BiquadFilter(BiquadFilterNode {
        filter_type: ty,
        frequency,
        detune: p(0.0),
        q: p(q),
        gain: p(0.0),
    })
}

/// A modulation wire: `from`'s output drives `to`'s named param. Pair an LFO
/// with a `gain` "depth" node to scale the ±1 oscillator to the param's range.
fn modulate(from: NodeId, to: NodeId, param: &str) -> Connection {
    Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: from,
            output: 0,
        },
        to: ConnectionSink::NodeParam {
            node: to,
            param: ParamId::from(param),
        },
    }
}

fn noise(flavor: NoiseFlavor, seconds: f32, seed: u64, density: f32) -> NodeKind {
    NodeKind::Noise(NoiseNode {
        flavor,
        seed,
        seconds,
        stereo: true,
        density,
        gaussian: false,
    })
}

// ======================================================================
// Bell — additive synthesis from inharmonic partials, each with its own
// exponential decay (higher/dissonant partials fade faster).
// ======================================================================

fn bell() -> SampleLibrary {
    let mut s = Sample::new("Bell");
    let f0 = 440.0_f32;
    // (ratio, peak amplitude, decay seconds) — classic bell-ish inharmonic set.
    let partials = [
        (0.56, 0.50, 3.2),
        (0.92, 0.35, 2.6),
        (1.19, 0.30, 2.2),
        (1.71, 0.22, 1.6),
        (2.00, 0.45, 3.0), // strike tone
        (2.74, 0.18, 1.2),
        (3.00, 0.15, 1.0),
    ];

    let mix = s.graph.push_node(Node::new(gain(p(0.55))));
    for (ratio, amp, decay) in partials {
        let o = s
            .graph
            .push_node(Node::new(osc(p(f0 * ratio), OscillatorType::Sine)));
        let g = s
            .graph
            .push_node(Node::new(gain(env_perc(amp, 0.004, decay))));
        s.graph.connect(Connection::node_to_node(o, g));
        s.graph.connect(Connection::node_to_node(g, mix));
    }
    lib(s)
}

// ======================================================================
// Rain — three noise layers: brown body (low rush), pink hiss, and dust
// droplets through a band-pass, each filtered + leveled into a mix. Loops.
// ======================================================================

fn rain() -> SampleLibrary {
    let mut s = Sample::new("Rain");
    let mix = s.graph.push_node(Node::new(gain(p(0.9))));

    // Low body.
    let body = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Brown, 3.0, 11, 0.0)));
    let body_lp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(900.0), 0.7)));
    let body_g = s.graph.push_node(Node::new(gain(p(0.5))));
    s.graph.connect(Connection::node_to_node(body, body_lp));
    s.graph.connect(Connection::node_to_node(body_lp, body_g));
    s.graph.connect(Connection::node_to_node(body_g, mix));

    // Airy hiss.
    let hiss = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Pink, 3.0, 22, 0.0)));
    let hiss_hp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Highpass,
        p(2000.0),
        0.7,
    )));
    let hiss_g = s.graph.push_node(Node::new(gain(p(0.18))));
    s.graph.connect(Connection::node_to_node(hiss, hiss_hp));
    s.graph.connect(Connection::node_to_node(hiss_hp, hiss_g));
    s.graph.connect(Connection::node_to_node(hiss_g, mix));

    // Individual droplets.
    let drops = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Dust, 3.0, 33, 1200.0)));
    let drops_bp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Bandpass,
        p(3000.0),
        1.5,
    )));
    let drops_g = s.graph.push_node(Node::new(gain(p(0.5))));
    s.graph.connect(Connection::node_to_node(drops, drops_bp));
    s.graph.connect(Connection::node_to_node(drops_bp, drops_g));
    s.graph.connect(Connection::node_to_node(drops_g, mix));

    lib(s)
}

// ======================================================================
// Fire — low brown roar + sparse mid crackle + faint airy hiss. Loops.
// ======================================================================

fn fire() -> SampleLibrary {
    let mut s = Sample::new("Fire");
    let mix = s.graph.push_node(Node::new(gain(p(0.9))));

    // Roar.
    let roar = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Brown, 3.0, 7, 0.0)));
    let roar_lp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(420.0), 0.6)));
    let roar_g = s.graph.push_node(Node::new(gain(p(0.55))));
    s.graph.connect(Connection::node_to_node(roar, roar_lp));
    s.graph.connect(Connection::node_to_node(roar_lp, roar_g));
    s.graph.connect(Connection::node_to_node(roar_g, mix));

    // Crackle (sparse dust through a resonant band-pass = popping).
    let crackle = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Dust, 3.0, 99, 220.0)));
    let crackle_bp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Bandpass,
        p(1800.0),
        3.0,
    )));
    let crackle_g = s.graph.push_node(Node::new(gain(p(0.7))));
    s.graph
        .connect(Connection::node_to_node(crackle, crackle_bp));
    s.graph
        .connect(Connection::node_to_node(crackle_bp, crackle_g));
    s.graph.connect(Connection::node_to_node(crackle_g, mix));

    // Faint hiss.
    let hiss = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::White, 3.0, 55, 0.0)));
    let hiss_hp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Highpass,
        p(3000.0),
        0.7,
    )));
    let hiss_g = s.graph.push_node(Node::new(gain(p(0.08))));
    s.graph.connect(Connection::node_to_node(hiss, hiss_hp));
    s.graph.connect(Connection::node_to_node(hiss_hp, hiss_g));
    s.graph.connect(Connection::node_to_node(hiss_g, mix));

    lib(s)
}

// ======================================================================
// Laser — two detuned saws diving in pitch through a low-pass, with a fast
// percussive amp envelope. One-shot "pew".
// ======================================================================

fn laser() -> SampleLibrary {
    let mut s = Sample::new("Laser");
    let amp = s
        .graph
        .push_node(Node::new(gain(env_perc(0.45, 0.005, 0.38))));
    let lp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(3500.0), 4.0)));
    s.graph.connect(Connection::node_to_node(lp, amp));

    let o1 = s.graph.push_node(Node::new(osc(
        exp_sweep(1800.0, 120.0, 0.35),
        OscillatorType::Sawtooth,
    )));
    let o2 = s.graph.push_node(Node::new(osc(
        exp_sweep(900.0, 80.0, 0.35),
        OscillatorType::Square,
    )));
    s.graph.connect(Connection::node_to_node(o1, lp));
    s.graph.connect(Connection::node_to_node(o2, lp));
    lib(s)
}

// ======================================================================
// Rocket — brown-noise roar through a low-pass whose cutoff climbs over ~5s,
// plus a rising sub rumble, under a slow amplitude swell. Long one-shot.
// ======================================================================

fn rocket() -> SampleLibrary {
    let mut s = Sample::new("Rocket");
    let mix = s.graph.push_node(Node::new(gain(env_swell(0.9, 4.0))));

    // Roar: brown noise, cutoff opening up as it "throttles up".
    let roar = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Brown, 5.0, 3, 0.0)));
    let roar_lp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Lowpass,
        lin_ramp(180.0, 6000.0, 5.0),
        1.2,
    )));
    s.graph.connect(Connection::node_to_node(roar, roar_lp));
    s.graph.connect(Connection::node_to_node(roar_lp, mix));

    // Sub rumble: low sine rising in pitch.
    let sub = s.graph.push_node(Node::new(osc(
        lin_ramp(38.0, 120.0, 5.0),
        OscillatorType::Sine,
    )));
    let sub_g = s.graph.push_node(Node::new(gain(p(0.5))));
    s.graph.connect(Connection::node_to_node(sub, sub_g));
    s.graph.connect(Connection::node_to_node(sub_g, mix));

    lib(s)
}

// ======================================================================
// Kick drum — a sine with a fast downward pitch sweep + a punchy amp decay.
// ======================================================================

fn kick() -> SampleLibrary {
    let mut s = Sample::new("Kick");
    let osc = s.graph.push_node(Node::new(osc(
        exp_sweep(150.0, 50.0, 0.12),
        OscillatorType::Sine,
    )));
    let amp = s
        .graph
        .push_node(Node::new(gain(env_perc(0.95, 0.002, 0.28))));
    s.graph.connect(Connection::node_to_node(osc, amp));
    lib(s)
}

// ======================================================================
// Hi-hat — high-passed white noise with an ultra-short decay (a metallic tick).
// ======================================================================

fn hihat() -> SampleLibrary {
    let mut s = Sample::new("Hi-hat");
    let n = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::White, 1.0, 5, 0.0)));
    let hp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Highpass,
        p(7000.0),
        0.8,
    )));
    let amp = s
        .graph
        .push_node(Node::new(gain(env_perc(0.7, 0.001, 0.06))));
    s.graph.connect(Connection::node_to_node(n, hp));
    s.graph.connect(Connection::node_to_node(hp, amp));
    lib(s)
}

// ======================================================================
// Siren — a tone whose pitch is swept up and down by a slow triangle LFO
// (LFO → "depth" gain → oscillator frequency). The first modulation example.
// ======================================================================

fn siren() -> SampleLibrary {
    let mut s = Sample::new("Siren");
    let tone = s
        .graph
        .push_node(Node::new(osc(p(600.0), OscillatorType::Sawtooth)));
    let amp = s.graph.push_node(Node::new(gain(p(0.3))));
    s.graph.connect(Connection::node_to_node(tone, amp));

    // LFO (0.5 Hz triangle) scaled to ±250 Hz, driving the tone's frequency.
    let lfo = s
        .graph
        .push_node(Node::new(osc(p(0.5), OscillatorType::Triangle)));
    let depth = s.graph.push_node(Node::new(gain(p(250.0))));
    s.graph.connect(Connection::node_to_node(lfo, depth));
    s.graph.connect(modulate(depth, tone, "frequency"));
    lib(s)
}

// ======================================================================
// Wobble bass — a resonant low-pass on a saw, its cutoff wobbled by an LFO.
// The classic "dubstep" sound. Loops.
// ======================================================================

fn wobble() -> SampleLibrary {
    let mut s = Sample::new("Wobble Bass");
    let saw = s
        .graph
        .push_node(Node::new(osc(p(55.0), OscillatorType::Sawtooth)));
    let lp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(700.0), 9.0)));
    let amp = s.graph.push_node(Node::new(gain(p(0.5))));
    s.graph.connect(Connection::node_to_node(saw, lp));
    s.graph.connect(Connection::node_to_node(lp, amp));

    // LFO (4 Hz) scaled to ±550 Hz, wobbling the cutoff.
    let lfo = s
        .graph
        .push_node(Node::new(osc(p(4.0), OscillatorType::Sine)));
    let depth = s.graph.push_node(Node::new(gain(p(550.0))));
    s.graph.connect(Connection::node_to_node(lfo, depth));
    s.graph.connect(modulate(depth, lp, "frequency"));
    lib(s)
}

// ======================================================================
// Wind — pink noise through a resonant band-pass whose center frequency is
// swept slowly by an LFO, giving a howling, gusting quality. Loops.
// ======================================================================

fn wind() -> SampleLibrary {
    let mut s = Sample::new("Wind");
    let n = s
        .graph
        .push_node(Node::new(noise(NoiseFlavor::Pink, 3.0, 17, 0.0)));
    let bp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Bandpass, p(600.0), 3.0)));
    let amp = s.graph.push_node(Node::new(gain(p(1.0))));
    s.graph.connect(Connection::node_to_node(n, bp));
    s.graph.connect(Connection::node_to_node(bp, amp));

    // Slow LFO (0.15 Hz) scaled to ±450 Hz, sweeping the band-pass center.
    let lfo = s
        .graph
        .push_node(Node::new(osc(p(0.15), OscillatorType::Sine)));
    let depth = s.graph.push_node(Node::new(gain(p(450.0))));
    s.graph.connect(Connection::node_to_node(lfo, depth));
    s.graph.connect(modulate(depth, bp, "frequency"));
    lib(s)
}

// ======================================================================
// Spatial — a tone routed through a spatial Output node whose X position
// sweeps left→right in 3D (HRTF). Demonstrates the Output node + its
// position as the runtime-adjustable control.
// ======================================================================

fn spatial() -> SampleLibrary {
    let mut s = Sample::new("Spatial");
    let tone = s
        .graph
        .push_node(Node::new(osc(p(330.0), OscillatorType::Triangle)));
    let g = s.graph.push_node(Node::new(gain(p(0.5))));
    let out = s
        .graph
        .push_node(Node::new(NodeKind::SpatialOutput(SpatialOutputNode {
            gain: p(1.0),
            // Pan from far-left to far-right over 4s, a bit in front of the listener.
            position_x: lin_ramp(-6.0, 6.0, 4.0),
            position_y: p(0.0),
            position_z: p(-1.5),
        })));
    s.graph.connect(Connection::node_to_node(tone, g));
    s.graph.connect(Connection::node_to_node(g, out));
    lib(s)
}

// ======================================================================
// WASM AudioWorklet showcases — an oscillator through a bundled .wasm DSP
// module, with the module's discovered params exposed. Each embeds its WASM
// inline so the project is self-contained.
// ======================================================================

fn wparam(name: &str, min: f32, max: f32, value: f32) -> WorkletParam {
    WorkletParam {
        name: ParamId(name.to_string()),
        min,
        max,
        param: p(value),
    }
}

/// Push an AudioWorklet node backed by `wasm` into `s`, recording the module in
/// `assets`. `params` must match the module's discovery order (bank slot index).
fn wmod(
    s: &mut Sample,
    label: &str,
    wasm: &[u8],
    params: Vec<WorkletParam>,
    assets: &mut Vec<WasmAsset>,
) -> NodeId {
    use base64::Engine as _;
    let module_id = AssetId::new();
    let node = s
        .graph
        .push_node(Node::new(NodeKind::AudioWorklet(AudioWorkletNode {
            module: Some(module_id),
            processor_name: label.to_string(),
            parameters: params,
        })));
    assets.push(WasmAsset {
        id: module_id,
        label: Some(label.to_string()),
        source: WasmSource::Base64(base64::engine::general_purpose::STANDARD.encode(wasm)),
    });
    node
}

/// A `lib` with embedded WASM module assets.
fn lib_with(sample: Sample, wasm_modules: Vec<WasmAsset>) -> SampleLibrary {
    let mut l = lib(sample);
    l.assets.wasm_modules = wasm_modules;
    l
}

/// A detuned oscillator (cents), for thickening unison patches.
fn osc_detuned(freq: f32, ty: OscillatorType, cents: f32) -> NodeKind {
    NodeKind::Oscillator(OscillatorNode {
        oscillator_type: ty,
        harmonics: Vec::new(),
        frequency: p(freq),
        detune: p(cents),
    })
}

/// Crush — a lo-fi pluck: sawtooth → bitcrusher → resonant low-pass with a
/// downward cutoff sweep → percussive amp envelope.
fn crush() -> SampleLibrary {
    let mut s = Sample::new("Crush");
    let mut assets = Vec::new();
    let saw = s
        .graph
        .push_node(Node::new(osc(p(110.0), OscillatorType::Sawtooth)));
    let cr = wmod(
        &mut s,
        "bitcrusher",
        BITCRUSHER_WASM,
        vec![
            wparam("bits", 1.0, 16.0, 5.0),
            wparam("reduction", 1.0, 32.0, 6.0),
        ],
        &mut assets,
    );
    let lp = s.graph.push_node(Node::new(biquad(
        BiquadFilterType::Lowpass,
        exp_sweep(3500.0, 400.0, 0.5),
        6.0,
    )));
    let amp = s
        .graph
        .push_node(Node::new(gain(env_perc(0.7, 0.005, 0.6))));
    s.graph.connect(Connection::node_to_node(saw, cr));
    s.graph.connect(Connection::node_to_node(cr, lp));
    s.graph.connect(Connection::node_to_node(lp, amp));
    lib_with(s, assets)
}

/// Drive — an overdriven lead: two detuned saws → tanh overdrive → low-pass →
/// amp envelope.
fn drive() -> SampleLibrary {
    let mut s = Sample::new("Drive");
    let mut assets = Vec::new();
    let o1 = s
        .graph
        .push_node(Node::new(osc(p(110.0), OscillatorType::Sawtooth)));
    let o2 = s.graph.push_node(Node::new(osc_detuned(
        110.0,
        OscillatorType::Sawtooth,
        11.0,
    )));
    let pre = s.graph.push_node(Node::new(gain(p(0.5))));
    let dr = wmod(
        &mut s,
        "drive",
        DRIVE_WASM,
        vec![
            wparam("drive", 1.0, 30.0, 14.0),
            wparam("mix", 0.0, 1.0, 1.0),
            wparam("level", 0.0, 1.0, 0.6),
        ],
        &mut assets,
    );
    let lp = s
        .graph
        .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(2200.0), 0.8)));
    let amp = s.graph.push_node(Node::new(gain(env_perc(0.7, 0.01, 1.4))));
    s.graph.connect(Connection::node_to_node(o1, pre));
    s.graph.connect(Connection::node_to_node(o2, pre));
    s.graph.connect(Connection::node_to_node(pre, dr));
    s.graph.connect(Connection::node_to_node(dr, lp));
    s.graph.connect(Connection::node_to_node(lp, amp));
    lib_with(s, assets)
}

/// Ring Mod — a clangorous bell: sine → ring modulator → percussive amp
/// envelope, with a slow LFO sweeping the modulator's `freq` param (modulation
/// wired straight into a WASM worklet parameter).
fn ringmod() -> SampleLibrary {
    let mut s = Sample::new("Ring Mod");
    let mut assets = Vec::new();
    let carrier = s
        .graph
        .push_node(Node::new(osc(p(440.0), OscillatorType::Sine)));
    let rm = wmod(
        &mut s,
        "ringmod",
        RINGMOD_WASM,
        vec![
            wparam("freq", 20.0, 2000.0, 220.0),
            wparam("mix", 0.0, 1.0, 1.0),
        ],
        &mut assets,
    );
    let amp = s
        .graph
        .push_node(Node::new(gain(env_perc(0.6, 0.005, 1.6))));
    s.graph.connect(Connection::node_to_node(carrier, rm));
    s.graph.connect(Connection::node_to_node(rm, amp));

    // LFO → depth → ringmod.freq: the modulator pitch sweeps ±120 Hz at 6 Hz.
    let lfo = s
        .graph
        .push_node(Node::new(osc(p(6.0), OscillatorType::Sine)));
    let depth = s.graph.push_node(Node::new(gain(p(120.0))));
    s.graph.connect(Connection::node_to_node(lfo, depth));
    s.graph.connect(modulate(depth, rm, "freq"));

    lib_with(s, assets)
}

// ======================================================================
// Sequenced Song — the flagship arrangement (Sequences view). A Note Sequencer
// holds a 2-bar groove: a melodic bass track + a drum track. Each distinct sound
// is its own keyed output, wired to its own instrument (a Sample-ref), and all
// instruments sum through a Bus into the Output. Demonstrates the whole
// keyed-trigger model: per-sound routing, drums vs. melodic, a bus + output.
// ======================================================================

fn song() -> SampleLibrary {
    // --- Instruments (each a self-contained Instrument sample). Their oscillator
    // base = MIDI note 60, so the sequencer's note numbers map to true pitches. ---
    let mut bass = Sample::new("Bass");
    {
        let saw = bass
            .graph
            .push_node(Node::new(osc(p(261.63), OscillatorType::Sawtooth)));
        let lp = bass
            .graph
            .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(600.0), 4.0)));
        let amp = bass
            .graph
            .push_node(Node::new(gain(env_perc(0.7, 0.005, 0.35))));
        bass.graph.connect(Connection::node_to_node(saw, lp));
        bass.graph.connect(Connection::node_to_node(lp, amp));
    }

    let mut kick = Sample::new("Kick");
    {
        let o = kick.graph.push_node(Node::new(osc(
            exp_sweep(150.0, 50.0, 0.12),
            OscillatorType::Sine,
        )));
        let amp = kick
            .graph
            .push_node(Node::new(gain(env_perc(0.95, 0.002, 0.28))));
        kick.graph.connect(Connection::node_to_node(o, amp));
    }

    let mut snare = Sample::new("Snare");
    {
        let n = snare
            .graph
            .push_node(Node::new(noise(NoiseFlavor::White, 1.0, 7, 0.0)));
        let hp = snare.graph.push_node(Node::new(biquad(
            BiquadFilterType::Highpass,
            p(1500.0),
            0.8,
        )));
        let amp = snare
            .graph
            .push_node(Node::new(gain(env_perc(0.6, 0.001, 0.18))));
        snare.graph.connect(Connection::node_to_node(n, hp));
        snare.graph.connect(Connection::node_to_node(hp, amp));
    }

    let mut hat = Sample::new("Hi-hat");
    {
        let n = hat
            .graph
            .push_node(Node::new(noise(NoiseFlavor::White, 1.0, 9, 0.0)));
        let hp = hat.graph.push_node(Node::new(biquad(
            BiquadFilterType::Highpass,
            p(8000.0),
            0.8,
        )));
        let amp = hat
            .graph
            .push_node(Node::new(gain(env_perc(0.5, 0.001, 0.05))));
        hat.graph.connect(Connection::node_to_node(n, hp));
        hat.graph.connect(Connection::node_to_node(hp, amp));
    }

    // --- The song: a melodic bass track + a drum track over 2 bars (8 beats). ---
    let nv = |start: f64, note: u8, vel: u8| NoteEvent {
        start,
        length: 0.45,
        note,
        velocity: vel,
    };
    // Bassline (C2 D#2 G2 …) — eighth-ish notes walking the bar.
    let bass_track = Track {
        name: "Bass".into(),
        channel: 0,
        events: vec![
            nv(0.0, 36, 110),
            nv(1.0, 36, 90),
            nv(1.5, 43, 90),
            nv(2.0, 41, 100),
            nv(3.0, 39, 90),
            nv(4.0, 36, 110),
            nv(5.0, 36, 90),
            nv(5.5, 43, 90),
            nv(6.0, 41, 100),
            nv(7.0, 31, 95),
        ],
    };
    // Drums: kick (36) on the quarters, snare (38) on the backbeat, hat (42) on
    // every eighth.
    let mut drum_events = Vec::new();
    for beat in 0..8 {
        drum_events.push(nv(beat as f64, 36, 110)); // kick
        drum_events.push(nv(beat as f64 + 0.5, 42, 70)); // hat off-beat
        drum_events.push(nv(beat as f64, 42, 90)); // hat on-beat
    }
    for &beat in &[2.0, 6.0] {
        drum_events.push(nv(beat, 38, 100)); // snare backbeat
    }
    let drum_track = Track {
        name: "Drums".into(),
        channel: 9,
        events: drum_events,
    };

    // A Melodic Sequencer (Bass) and a Drum Sequencer (the kit) — two single-type
    // nodes, the way the editor's palette now creates them. The melodic node has
    // one output for its track; the drum node has one output per percussion note.
    let mel_node = NodeKind::NoteSequencer(NoteSequencerNode {
        mode: SequencerMode::Melodic,
        song: Song {
            bpm: 110.0,
            tempo_map: Vec::new(),
            tracks: vec![bass_track],
        },
        // 2 bars = 8 beats: the musical loop window, so a looped bounce repeats
        // exactly on the bar (not at the last note's tail).
        length: 8.0,
        looping: true,
        outputs: vec![SoundOut {
            key: "t0".into(),
            track: 0,
            note: None,
            label: "Bass".into(),
            transpose: 0,
            gain: 1.0,
        }],
        ..Default::default()
    });
    let drum_node = NodeKind::NoteSequencer(NoteSequencerNode {
        mode: SequencerMode::Drum,
        song: Song {
            bpm: 110.0,
            tempo_map: Vec::new(),
            tracks: vec![drum_track],
        },
        length: 8.0,
        looping: true,
        outputs: vec![
            SoundOut {
                key: "t0:n36".into(),
                track: 0,
                note: Some(36),
                label: "Kick".into(),
                transpose: 0,
                gain: 1.0,
            },
            SoundOut {
                key: "t0:n38".into(),
                track: 0,
                note: Some(38),
                label: "Snare".into(),
                transpose: 0,
                gain: 1.0,
            },
            SoundOut {
                key: "t0:n42".into(),
                track: 0,
                note: Some(42),
                label: "Hat".into(),
                transpose: 0,
                gain: 1.0,
            },
        ],
        ..Default::default()
    });

    // --- The arrangement (a Sequence sample): sequencer + 4 instrument-refs →
    // bus → output, with one keyed trigger wire per sound. ---
    let mut arr = Sample::new("Sequenced Song");
    let seq_mel = arr.graph.push_node(Node::new(mel_node));
    let seq_drum = arr.graph.push_node(Node::new(drum_node));
    let r = |arr: &mut Sample, id| {
        arr.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
            sample: id,
            inputs: Vec::new(),
        })))
    };
    let bass_ref = r(&mut arr, bass.id);
    let kick_ref = r(&mut arr, kick.id);
    let snare_ref = r(&mut arr, snare.id);
    let hat_ref = r(&mut arr, hat.id);
    let bus = arr
        .graph
        .push_node(Node::new(NodeKind::Bus(BusNode { gain: 0.8 })));
    let out = arr
        .graph
        .push_node(Node::new(NodeKind::Output(OutputNode::default())));

    // Trigger wires: each keyed sound output → its instrument's trigger inlet.
    let trig = |arr: &mut Sample, seq, key: &str, to| {
        arr.graph.connect(Connection {
            id: None,
            from: ConnectionSource::SeqOut {
                node: seq,
                key: key.to_string(),
            },
            to: ConnectionSink::Trigger { node: to },
        });
    };
    trig(&mut arr, seq_mel, "t0", bass_ref);
    trig(&mut arr, seq_drum, "t0:n36", kick_ref);
    trig(&mut arr, seq_drum, "t0:n38", snare_ref);
    trig(&mut arr, seq_drum, "t0:n42", hat_ref);

    // Audio: every instrument-ref → bus → output.
    for inst in [bass_ref, kick_ref, snare_ref, hat_ref] {
        arr.graph.connect(Connection::node_to_node(inst, bus));
    }
    arr.graph.connect(Connection::node_to_node(bus, out));

    SampleLibrary {
        root: Some(arr.id),
        samples: vec![arr, bass, kick, snare, hat],
        ..Default::default()
    }
}

// ======================================================================
// Arrangement — the DAW timeline (Arrange view): three instruments laid out as
// tracks of clips. Bass + Pad are melodic (notes are pitches); Drums is a one-
// shot kit (notes play at their written sound). Demonstrates clips on tracks.
// ======================================================================

fn arrangement() -> SampleLibrary {
    // Bounce-only arrangements: a couple of Sounds plus an (initially empty)
    // arrangement. Bounce a Sound, then drop audio clips on the timeline.
    // (Phase 6 ships a fully-arranged example with bounced clips.)
    let mut bass = Sample::new("Bass");
    {
        let saw = bass
            .graph
            .push_node(Node::new(osc(p(110.0), OscillatorType::Sawtooth)));
        let lp = bass
            .graph
            .push_node(Node::new(biquad(BiquadFilterType::Lowpass, p(600.0), 4.0)));
        let amp = bass
            .graph
            .push_node(Node::new(gain(env_perc(0.7, 0.005, 0.4))));
        bass.graph.connect(Connection::node_to_node(saw, lp));
        bass.graph.connect(Connection::node_to_node(lp, amp));
    }

    let mut kick = Sample::new("Kick");
    {
        let o = kick.graph.push_node(Node::new(osc(
            exp_sweep(150.0, 50.0, 0.12),
            OscillatorType::Sine,
        )));
        let amp = kick
            .graph
            .push_node(Node::new(gain(env_perc(0.95, 0.002, 0.28))));
        kick.graph.connect(Connection::node_to_node(o, amp));
    }

    let song = Sample::new_arrangement("Arrangement");
    SampleLibrary {
        root: Some(song.id),
        samples: vec![song, bass, kick],
        ..Default::default()
    }
}

//! Round-trip + validation tests. These double as worked examples of how the
//! pieces fit: build a leaf "voice" sample, expose a macro knob, then compose it
//! into a parent via a sample reference.

use crate::*;

/// A subtractive "voice": oscillator → filter → gain → outlet, with a "cutoff"
/// macro driving the filter frequency and a trigger that starts the oscillator.
fn voice_sample() -> Sample {
    let mut s = Sample::new("voice");

    let osc = s
        .graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode {
            oscillator_type: OscillatorType::Sawtooth,
            frequency: AudioParam::new(220.0),
            ..Default::default()
        })));
    let filter = s.graph.push_node(Node::new(NodeKind::BiquadFilter(
        BiquadFilterNode::default(),
    )));
    let gain = s
        .graph
        .push_node(Node::new(NodeKind::Gain(GainNode::default())));

    s.graph.outlets.push(PortDecl::new("out"));
    s.graph.connect(Connection::node_to_node(osc, filter));
    s.graph.connect(Connection::node_to_node(filter, gain));
    s.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: gain,
            output: 0,
        },
        to: ConnectionSink::Outlet { port: "out".into() },
    });

    // A "cutoff" input that sets the filter's frequency (default 800).
    s.graph.inlets.push(PortDecl {
        id: "cutoff".into(),
        label: None,
        default: 800.0,
    });
    s.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: "cutoff".into(),
        },
        to: ConnectionSink::NodeParam {
            node: filter,
            param: ParamId::FREQUENCY.into(),
        },
    });

    // Note-on starts the oscillator.
    s.trigger.sources.push(osc);
    s
}

/// Library: the voice plus a composite that references it twice through a gain
/// bus — a small demonstration of nesting/composition.
fn composite_library() -> SampleLibrary {
    let voice = voice_sample();
    let voice_id = voice.id;

    let mut master = Sample::new("master");
    let bus = master
        .graph
        .push_node(Node::new(NodeKind::Gain(GainNode::default())));
    let v1 = master
        .graph
        .push_node(Node::new(NodeKind::Sample(SampleRef {
            sample: voice_id,
            inputs: vec![InputValue {
                port: "cutoff".into(),
                value: 1200.0,
            }],
        })));
    let v2 = master
        .graph
        .push_node(Node::new(NodeKind::Sample(SampleRef {
            sample: voice_id,
            inputs: vec![],
        })));
    master.graph.outlets.push(PortDecl::new("out"));
    master.graph.connect(Connection::node_to_node(v1, bus));
    master.graph.connect(Connection::node_to_node(v2, bus));
    master.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: bus,
            output: 0,
        },
        to: ConnectionSink::Outlet { port: "out".into() },
    });
    let master_id = master.id;

    SampleLibrary {
        samples: vec![voice, master],
        root: Some(master_id),
        ..Default::default()
    }
}

#[test]
fn toml_round_trips() {
    let lib = composite_library();
    let text = toml::to_string_pretty(&lib).unwrap();
    let back: SampleLibrary = toml::from_str(&text).unwrap();
    assert_eq!(lib, back);
}

#[test]
fn full_fidelity_audio_param_round_trips_through_toml() {
    // Exercises the field-ordering fix: a param with both an automation_rate
    // scalar and an automation array-of-tables in the same table.
    let mut s = Sample::new("automated");
    let osc = s
        .graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode {
            frequency: AudioParam {
                value: 440.0,
                automation_rate: Some(AutomationRate::ARate),
                automation: vec![
                    AutomationEvent::SetValue {
                        value: 440.0,
                        time: 0.0,
                    },
                    AutomationEvent::ExponentialRamp {
                        value: 880.0,
                        time: 0.5,
                    },
                    AutomationEvent::SetValueCurve {
                        values: vec![0.0, 0.5, 1.0],
                        start_time: 0.5,
                        duration: 0.25,
                    },
                ],
            },
            ..Default::default()
        })));
    s.trigger.sources.push(osc);

    let lib = SampleLibrary {
        samples: vec![s],
        ..Default::default()
    };
    let text = toml::to_string_pretty(&lib).unwrap();
    let back: SampleLibrary = toml::from_str(&text).unwrap();
    assert_eq!(lib, back);
}

#[test]
fn valid_library_has_no_errors() {
    let lib = composite_library();
    assert_eq!(lib.validate(), vec![]);
}

#[test]
fn modulation_connection_targets_a_param() {
    // An LFO (low-frequency oscillator) modulating a gain's gain param.
    let mut s = Sample::new("tremolo");
    let lfo = s
        .graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode {
            frequency: AudioParam::new(5.0),
            ..Default::default()
        })));
    let gain = s
        .graph
        .push_node(Node::new(NodeKind::Gain(GainNode::default())));
    s.graph
        .connect(Connection::node_to_param(lfo, gain, ParamId::GAIN));

    let lib = SampleLibrary {
        samples: vec![s],
        ..Default::default()
    };
    assert_eq!(lib.validate(), vec![]);
}

#[test]
fn detects_dangling_node_reference() {
    let mut s = Sample::new("broken");
    let real = s
        .graph
        .push_node(Node::new(NodeKind::Gain(GainNode::default())));
    let ghost = NodeId::new();
    s.graph.connect(Connection::node_to_node(real, ghost));

    let lib = SampleLibrary {
        samples: vec![s],
        ..Default::default()
    };
    let errs = lib.validate();
    assert!(errs
        .iter()
        .any(|e| matches!(e, SchemaError::UnknownNode { node, .. } if *node == ghost)));
}

#[test]
fn detects_custom_oscillator_without_wave() {
    let mut s = Sample::new("bad-osc");
    s.graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode {
            oscillator_type: OscillatorType::Custom,

            ..Default::default()
        })));
    let lib = SampleLibrary {
        samples: vec![s],
        ..Default::default()
    };
    assert!(lib
        .validate()
        .iter()
        .any(|e| matches!(e, SchemaError::MissingPeriodicWave { .. })));
}

#[test]
fn detects_sample_reference_cycle() {
    // Two samples that each reference the other.
    let mut a = Sample::new("a");
    let mut b = Sample::new("b");
    let (a_id, b_id) = (a.id, b.id);
    a.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: b_id,
        inputs: vec![],
    })));
    b.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: a_id,
        inputs: vec![],
    })));

    let lib = SampleLibrary {
        samples: vec![a, b],
        ..Default::default()
    };
    assert!(lib
        .validate()
        .iter()
        .any(|e| matches!(e, SchemaError::SampleCycle { .. })));
}

#[test]
fn examples_are_valid() {
    for (name, lib) in crate::examples::all() {
        assert_eq!(lib.validate(), vec![], "example `{name}` should be valid");
        // Round-trips through TOML.
        let text = toml::to_string_pretty(&lib).unwrap();
        let back: SampleLibrary = toml::from_str(&text).unwrap();
        assert_eq!(lib, back, "example `{name}` should round-trip");
    }
}

/// Writes the canonical `examples/*.toml` files. Run explicitly with
/// `WRITE_EXAMPLES=1 cargo test -p awsm-audio-schema write_example_files`.
#[test]
fn write_example_files() {
    if std::env::var("WRITE_EXAMPLES").is_err() {
        return;
    }
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../examples");
    std::fs::create_dir_all(dir).unwrap();
    for (name, lib) in crate::examples::all() {
        let text = toml::to_string_pretty(&lib).unwrap();
        std::fs::write(format!("{dir}/{name}.toml"), text).unwrap();
    }
}

#[test]
fn enum_serializes_to_webaudio_strings() {
    // toml has no bare-scalar document, so serialize through a tiny wrapper and
    // check the rendered key = "value" line.
    #[derive(serde::Serialize)]
    struct W<T> {
        v: T,
    }

    assert_eq!(
        toml::to_string(&W {
            v: OverSampleType::X2
        })
        .unwrap()
        .trim(),
        r#"v = "2x""#
    );
    assert_eq!(
        toml::to_string(&W {
            v: PanningModelType::Hrtf
        })
        .unwrap()
        .trim(),
        r#"v = "HRTF""#
    );
    assert_eq!(
        toml::to_string(&W {
            v: ChannelCountMode::ClampedMax
        })
        .unwrap()
        .trim(),
        r#"v = "clamped-max""#
    );
}

#[test]
fn nested_example_flattens_to_ref_free_graph() {
    let lib = examples::all()
        .into_iter()
        .find(|(n, _)| *n == "nested")
        .map(|(_, l)| l)
        .expect("nested example");
    let flat = lib.flatten_root().expect("flatten");
    // No Sample-reference nodes remain.
    assert!(
        !flat
            .nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Sample(_))),
        "flattened graph still has Sample refs"
    );
    // saw + low-pass (inlined) + gain = 3 nodes.
    assert_eq!(flat.nodes.len(), 3);
    // The inlined low-pass survived.
    assert!(flat
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::BiquadFilter(_))));
    // No boundary (Inlet/Outlet) connections remain at the top level.
    for c in &flat.connections {
        assert!(!matches!(c.from, ConnectionSource::Inlet { .. }));
        assert!(!matches!(c.to, ConnectionSink::Outlet { .. }));
    }
    // Two real wires: saw→lowpass, lowpass→gain.
    assert_eq!(flat.connections.len(), 2);
}

#[test]
fn input_value_sets_param_on_flatten() {
    // Sub-sample: a gain node with a "level" input that sets its `gain` (default
    // 0.5), plus an outlet.
    let mut sub = Sample::new("sub");
    let g = sub.graph.push_node(Node::new(NodeKind::Gain(GainNode {
        gain: AudioParam::new(0.5),
    })));
    sub.graph.inlets.push(PortDecl {
        id: "level".into(),
        label: None,
        default: 0.5,
    });
    sub.graph.outlets.push(PortDecl::new("out"));
    sub.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: "level".into(),
        },
        to: ConnectionSink::NodeParam {
            node: g,
            param: "gain".into(),
        },
    });
    sub.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput { node: g, output: 0 },
        to: ConnectionSink::Outlet {
            port: PortId::from("out"),
        },
    });

    // Root: references the sub with the "level" input set to 0.9.
    let mut root = Sample::new("root");
    root.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: sub.id,
        inputs: vec![InputValue {
            port: "level".into(),
            value: 0.9,
        }],
    })));
    let lib = SampleLibrary {
        root: Some(root.id),
        samples: vec![root, sub],
        ..Default::default()
    };

    let flat = lib.flatten_root().expect("flatten");
    let gain = flat
        .nodes
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::Gain(g) => Some(g.gain.value),
            _ => None,
        })
        .expect("inlined gain");
    assert!((gain - 0.9).abs() < 1e-6, "input value not applied: {gain}");
}

#[test]
fn inlet_to_param_survives_flatten() {
    // Sub-sample "voice": an oscillator (base 220) whose `frequency` AudioParam
    // is driven by an inlet "freq"; the oscillator also feeds an outlet.
    let mut voice = Sample::new("voice");
    let osc = voice
        .graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode {
            oscillator_type: OscillatorType::Sine,
            frequency: AudioParam::new(220.0),
            detune: AudioParam::new(0.0),
            ..Default::default()
        })));
    voice.graph.inlets.push(PortDecl::new("freq"));
    voice.graph.outlets.push(PortDecl::new("out"));
    // inlet "freq" -> osc.frequency (a NodeParam sink = modulation).
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: PortId::from("freq"),
        },
        to: ConnectionSink::NodeParam {
            node: osc,
            param: ParamId::from("frequency"),
        },
    });
    voice.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput {
            node: osc,
            output: 0,
        },
        to: ConnectionSink::Outlet {
            port: PortId::from("out"),
        },
    });

    // Root: a ConstantSource feeding the voice ref's input 0 (the "freq" inlet).
    let mut root = Sample::new("root");
    let k = root
        .graph
        .push_node(Node::new(NodeKind::ConstantSource(ConstantSourceNode {
            offset: AudioParam::new(300.0),
        })));
    let vref = root.graph.push_node(Node::new(NodeKind::Sample(SampleRef {
        sample: voice.id,
        inputs: vec![],
    })));
    root.graph.connect(Connection {
        id: None,
        from: ConnectionSource::NodeOutput { node: k, output: 0 },
        to: ConnectionSink::NodeInput {
            node: vref,
            input: 0,
        },
    });

    let lib = SampleLibrary {
        root: Some(root.id),
        samples: vec![root, voice],
        ..Default::default()
    };
    let flat = lib.flatten_root().expect("flatten");

    // The inlined oscillator + constant.
    let osc_id = flat
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::Oscillator(_)))
        .expect("osc inlined")
        .id;
    let k_id = flat
        .nodes
        .iter()
        .find(|n| matches!(n.kind, NodeKind::ConstantSource(_)))
        .expect("constant inlined")
        .id;

    // After flattening, the constant must drive the oscillator's frequency param.
    let driven = flat.connections.iter().any(|c| {
        matches!(&c.from, ConnectionSource::NodeOutput { node, .. } if *node == k_id)
            && matches!(&c.to, ConnectionSink::NodeParam { node, param }
                if *node == osc_id && param.0 == "frequency")
    });
    assert!(
        driven,
        "inlet->param connection lost on flatten; connections = {:#?}",
        flat.connections
    );
}

#[test]
fn root_inlet_default_bakes_into_param() {
    // A root sample played standalone: an inlet "cutoff" (default 800) drives a
    // biquad's `frequency`. At the top level the inlet has no host, so its
    // default must be baked into the param value and the inlet connection
    // dropped (it would otherwise be inert).
    let mut root = Sample::new("root");
    let bq = root
        .graph
        .push_node(Node::new(NodeKind::BiquadFilter(BiquadFilterNode {
            frequency: AudioParam::new(350.0),
            ..Default::default()
        })));
    root.graph.inlets.push(PortDecl {
        id: "cutoff".into(),
        label: None,
        default: 800.0,
    });
    root.graph.connect(Connection {
        id: None,
        from: ConnectionSource::Inlet {
            port: "cutoff".into(),
        },
        to: ConnectionSink::NodeParam {
            node: bq,
            param: "frequency".into(),
        },
    });

    let lib = SampleLibrary {
        root: Some(root.id),
        samples: vec![root],
        ..Default::default()
    };
    let flat = lib.flatten_root().expect("flatten");

    let freq = flat
        .nodes
        .iter()
        .find_map(|n| match &n.kind {
            NodeKind::BiquadFilter(b) => Some(b.frequency.value),
            _ => None,
        })
        .expect("biquad present");
    assert!(
        (freq - 800.0).abs() < 1e-6,
        "root inlet default not baked into param: {freq}"
    );
    assert!(
        !flat
            .connections
            .iter()
            .any(|c| matches!(&c.from, ConnectionSource::Inlet { .. })),
        "top-level inlet connection should be resolved away"
    );
}

#[test]
fn song_and_midisong_round_trip_through_toml() {
    let song = Song {
        bpm: 128.0,
        tempo_map: vec![],
        tracks: vec![
            Track {
                name: "Lead".into(),
                channel: 0,
                events: vec![
                    NoteEvent {
                        start: 0.0,
                        length: 1.0,
                        note: 60,
                        velocity: 100,
                    },
                    NoteEvent {
                        start: 1.0,
                        length: 0.5,
                        note: 64,
                        velocity: 90,
                    },
                ],
            },
            Track {
                name: "Drums".into(),
                channel: 9,
                events: vec![NoteEvent {
                    start: 0.0,
                    length: 0.25,
                    note: 36,
                    velocity: 110,
                }],
            },
        ],
    };
    // Last note ends at 1.0 + 0.5 = 1.5 beats.
    assert!((song.duration_beats() - 1.5).abs() < 1e-9);
    // 1 beat at 128 BPM = 60/128 s.
    assert!((song.beats_to_secs(1.0) - 60.0 / 128.0).abs() < 1e-9);

    let mut graph = Graph::default();
    graph.push_node(Node::new(NodeKind::NoteSequencer(NoteSequencerNode {
        song,
        mode: SequencerMode::Drum,
        start: 4.0,
        end: Some(20.0),
        length: 32.0,
        looping: true,
        outputs: vec![
            SoundOut {
                key: "t0".into(),
                track: 0,
                note: None,
                label: "Lead".into(),
                transpose: 12,
                gain: 0.8,
            },
            SoundOut {
                key: "t1:n36".into(),
                track: 1,
                note: Some(36),
                label: "Kick".into(),
                transpose: 0,
                gain: 1.0,
            },
        ],
    })));

    let toml = toml::to_string(&graph).expect("serialize");
    let back: Graph = toml::from_str(&toml).expect("deserialize");
    assert_eq!(
        graph, back,
        "NoteSequencer graph did not round-trip:\n{toml}"
    );
}

#[test]
fn keyed_connections_and_sample_kind_round_trip() {
    use crate::{ConnectionSink, ConnectionSource, SampleKind};
    let mut g = Graph::default();
    let seq = g.push_node(Node::new(NodeKind::NoteSequencer(
        NoteSequencerNode::default(),
    )));
    let inst = g.push_node(Node::new(NodeKind::Bus(BusNode::default())));
    g.connect(Connection {
        id: None,
        from: ConnectionSource::SeqOut {
            node: seq,
            key: "t0".into(),
        },
        to: ConnectionSink::Trigger { node: inst },
    });
    let back: Graph = toml::from_str(&toml::to_string(&g).unwrap()).unwrap();
    assert_eq!(g, back, "keyed SeqOut→Trigger did not round-trip");

    let s = Sample::new("arr");
    assert_eq!(s.kind, SampleKind::Sound);
    let back: Sample = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
    assert_eq!(s, back);

    // Older documents that named the kind "sequence"/"instrument" still load.
    let legacy_text = toml::to_string(&s)
        .unwrap()
        .replace("\"sound\"", "\"sequence\"");
    let legacy: Sample = toml::from_str(&legacy_text).unwrap();
    assert_eq!(legacy.kind, SampleKind::Sound);
}

#[test]
// `midly::Track` is a `Vec`; building it with sequential `push`es here is the
// clearest form. (Silences a newer-clippy `vec_init_then_push` than the repo's
// pinned CI toolchain — see docs/plans/MCP-STATUS.md.)
#[allow(clippy::vec_init_then_push)]
fn parse_smf_round_trips_notes_and_tempo() {
    use midly::num::{u15, u24, u28, u4, u7};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, Track as SmfTrack, TrackEvent,
        TrackEventKind,
    };

    // 96 ticks per quarter; 500_000 µs/quarter = 120 BPM.
    let ppq = 96u16;
    let mut t = SmfTrack::new();
    t.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000))),
    });
    t.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Lead")),
    });
    // Note 60 on at tick 0, off one quarter (96 ticks) later.
    t.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOn {
                key: u7::new(60),
                vel: u7::new(100),
            },
        },
    });
    t.push(TrackEvent {
        delta: u28::new(96),
        kind: TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOff {
                key: u7::new(60),
                vel: u7::new(0),
            },
        },
    });
    t.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });

    let smf = Smf {
        header: Header::new(Format::SingleTrack, Timing::Metrical(u15::new(ppq))),
        tracks: vec![t],
    };
    let mut bytes = Vec::new();
    smf.write(&mut bytes).expect("write smf");

    let song = parse_smf(&bytes).expect("parse");
    assert!((song.bpm - 120.0).abs() < 1e-6, "bpm = {}", song.bpm);
    assert_eq!(song.tracks.len(), 1);
    let tr = &song.tracks[0];
    assert_eq!(tr.name, "Lead");
    assert_eq!(tr.channel, 0);
    assert_eq!(tr.events.len(), 1);
    let n = &tr.events[0];
    assert_eq!(n.note, 60);
    assert_eq!(n.velocity, 100);
    assert!((n.start - 0.0).abs() < 1e-9);
    // 96 ticks / 96 ppq = 1 beat.
    assert!((n.length - 1.0).abs() < 1e-9, "length = {}", n.length);
}

#[test]
fn tempo_map_beats_secs_round_trip() {
    // 120 BPM for the first 4 beats, then 60 BPM.
    let song = Song {
        bpm: 120.0,
        tempo_map: vec![
            TempoChange {
                beat: 0.0,
                bpm: 120.0,
            },
            TempoChange {
                beat: 4.0,
                bpm: 60.0,
            },
        ],
        tracks: vec![],
    };
    // 4 beats @120 = 2.0s; +2 beats @60 = +2.0s → 4.0s at beat 6.
    assert!(
        (song.beats_to_secs(4.0) - 2.0).abs() < 1e-9,
        "{}",
        song.beats_to_secs(4.0)
    );
    assert!(
        (song.beats_to_secs(6.0) - 4.0).abs() < 1e-9,
        "{}",
        song.beats_to_secs(6.0)
    );
    // Inverse.
    assert!(
        (song.secs_to_beats(2.0) - 4.0).abs() < 1e-9,
        "{}",
        song.secs_to_beats(2.0)
    );
    assert!(
        (song.secs_to_beats(4.0) - 6.0).abs() < 1e-9,
        "{}",
        song.secs_to_beats(4.0)
    );
    // Constant-tempo path still linear.
    let flat = Song {
        bpm: 120.0,
        ..Default::default()
    };
    assert!((flat.beats_to_secs(2.0) - 1.0).abs() < 1e-9);
    assert!((flat.secs_to_beats(1.0) - 2.0).abs() < 1e-9);
}

#[test]
fn arrangement_round_trips() {
    let src = SampleId::new();
    let mut s = Sample::new_arrangement("Song");
    s.arrangement.bpm = 128.0;
    s.arrangement.length_secs = 16.0;
    s.arrangement.tracks.push(ArrTrack {
        name: "Bass".into(),
        gain: 0.9,
        mute: false,
        solo: false,
        clips: vec![Clip {
            start: 2.0,
            length: 4.0,
            source: src,
            offset: 0.5,
            gain: 1.0,
            looping: true,
            speed: 1.5,
            name: "intro".into(),
        }],
    });
    let back: Sample = toml::from_str(&toml::to_string(&s).unwrap()).unwrap();
    assert_eq!(s, back);
    assert_eq!(back.kind, SampleKind::Arrangement);
    assert_eq!(back.arrangement.tracks[0].clips[0].source, src);

    // A bounce round-trips on a Sound.
    let mut snd = Sample::new("lead");
    snd.bounce = Some(Bounce {
        asset: AssetId::new(),
        source_hash: 42,
    });
    let back: Sample = toml::from_str(&toml::to_string(&snd).unwrap()).unwrap();
    assert_eq!(back.bounce.unwrap().source_hash, 42);

    // A clean Sound serializes neither an arrangement nor a bounce.
    let t = toml::to_string(&Sample::new("clean")).unwrap();
    assert!(!t.contains("arrangement") && !t.contains("bounce"));
}

#[test]
fn port_matrix_rules() {
    use crate::connection::{can_connect, Accept, Emit};
    // Audio drives audio inputs and modulates params, but not triggers.
    assert!(can_connect(Emit::Audio, Accept::Audio));
    assert!(can_connect(Emit::Audio, Accept::Param));
    assert!(!can_connect(Emit::Audio, Accept::Trigger));
    // A note trigger only fires a trigger inlet.
    assert!(can_connect(Emit::Trigger, Accept::Trigger));
    assert!(!can_connect(Emit::Trigger, Accept::Audio));
    assert!(!can_connect(Emit::Trigger, Accept::Param));
    // A control stream only drives a param.
    assert!(can_connect(Emit::Control, Accept::Param));
    assert!(!can_connect(Emit::Control, Accept::Audio));
    assert!(!can_connect(Emit::Control, Accept::Trigger));
}

#[test]
fn validate_rejects_incompatible_wire() {
    // A control-sequencer output wired into an audio input must be flagged.
    let mut s = Sample::new("bad");
    let osc = s
        .graph
        .push_node(Node::new(NodeKind::Oscillator(OscillatorNode::default())));
    let ctl = s.graph.push_node(Node::new(NodeKind::ControlSequencer(
        ControlSequencerNode::default(),
    )));
    s.graph.connect(Connection {
        id: None,
        from: ConnectionSource::SeqOut {
            node: ctl,
            key: "lane".into(),
        },
        to: ConnectionSink::NodeInput {
            node: osc,
            input: 0,
        },
    });
    let lib = SampleLibrary {
        root: Some(s.id),
        samples: vec![s],
        ..Default::default()
    };
    assert!(lib
        .validate()
        .iter()
        .any(|e| matches!(e, SchemaError::IncompatibleWire { .. })));
}

#[test]
fn node_kind_from_tag_round_trips_every_tag() {
    // Every bare tag must construct a kind whose serde tag is that same string —
    // this is what guarantees "a kind-name string works wherever a NodeKind is
    // accepted" (the MCP add_node / add_chain bare-string path) never drifts from
    // the serde representation.
    for tag in NodeKind::all_tags() {
        let kind = NodeKind::from_tag(tag)
            .unwrap_or_else(|| panic!("from_tag({tag}) returned None for a listed tag"));
        let v = toml::Value::try_from(&kind).expect("serialize");
        assert_eq!(
            v.get("kind").and_then(|k| k.as_str()),
            Some(*tag),
            "from_tag({tag}) serialized with a different tag"
        );
    }
    // Unknown tags (and `sample`, which needs a target id) are rejected.
    assert!(NodeKind::from_tag("sample").is_none());
    assert!(NodeKind::from_tag("wibble").is_none());
}

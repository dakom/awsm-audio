//! Serde round-trip / wire-shape tests — the unattended coverage that the wire
//! shapes are stable and that the reply types (`QueryResult`, `Response`) decode
//! as well as encode. Values that don't derive `PartialEq` are compared by their
//! serialized JSON form (serialize → deserialize → serialize, assert equal).

use awsm_audio_schema::{Clip, NodeId, NodeKind, NoteEvent, SampleId};
use serde_json::Value;

use crate::{
    ArrangeOp, Clipboard, EditorCommand, EditorProject, EditorQuery, FieldInfo, FieldValue,
    NodeKindInfo, NodeLayout, QueryResult, RenderHandle, Request, Response, SampleInfo, SongOp,
    TransportInfo, WavStats, WaveformEnvelope,
};

/// The directory-save path serializes an [`EditorProject`] with `to_string_pretty`
/// and writes `project.toml`. TOML rejects a scalar key emitted after a table, so
/// the project's scalar view-state (pan/zoom) must precede its table fields
/// (library/layout) — otherwise the save errors out and leaves an empty folder.
#[test]
fn editor_project_serializes_to_toml() {
    for (name, library) in awsm_audio_schema::examples::all() {
        let project = EditorProject {
            library,
            layout: vec![NodeLayout {
                id: NodeId::new(),
                x: 1.0,
                y: 2.0,
            }],
            pan_x: 3.0,
            pan_y: 4.0,
            zoom: 1.5,
        };
        // Mirrors `EditorController::save_to_dir`.
        let toml = toml::to_string_pretty(&project)
            .unwrap_or_else(|e| panic!("save serializes project.toml for '{name}': {e}"));
        let back: EditorProject = toml::from_str(&toml)
            .unwrap_or_else(|e| panic!("reload parses project.toml for '{name}': {e}"));
        assert_eq!(
            serde_json::to_value(&project).unwrap(),
            serde_json::to_value(&back).unwrap(),
            "project.toml round-trip mismatch for '{name}'"
        );
    }
}

/// JSON round-trip: encode → decode → re-encode and assert the two JSON values
/// match. Proves both directions of the wire codec without needing `PartialEq`.
fn json_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let first = serde_json::to_value(value).expect("encode");
    let decoded: T = serde_json::from_value(first.clone()).expect("decode");
    let second = serde_json::to_value(&decoded).expect("re-encode");
    assert_eq!(first, second, "json round-trip mismatch");
}

/// TOML round-trip: encode → decode → re-encode-as-JSON and compare. The
/// editor's `editor_dispatch_toml` seam depends on the TOML form being stable.
fn toml_round_trip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let toml_str = toml::to_string(value).expect("encode toml");
    let decoded: T = toml::from_str(&toml_str).expect("decode toml");
    let a = serde_json::to_value(value).expect("json a");
    let b = serde_json::to_value(&decoded).expect("json b");
    assert_eq!(a, b, "toml round-trip mismatch:\n{toml_str}");
}

fn sample_commands() -> Vec<EditorCommand> {
    let n = NodeId::new();
    vec![
        EditorCommand::AddNode {
            kind: NodeKind::Gain(Default::default()),
            x: 10.0,
            y: 20.0,
        },
        EditorCommand::Connect {
            from: n,
            from_output: 0,
            to: NodeId::new(),
            to_input: 1,
        },
        EditorCommand::SetField {
            id: n,
            key: "gain".into(),
            value: FieldValue::Num(0.5),
        },
        EditorCommand::EditSong {
            node: n,
            op: SongOp::AddNote {
                track: 0,
                event: NoteEvent {
                    start: 0.0,
                    length: 1.0,
                    note: 60,
                    velocity: 100,
                },
            },
        },
        EditorCommand::EditArrange {
            op: ArrangeOp::AddClip {
                track: 0,
                start: 0.0,
                source: SampleId::new(),
                length: Some(2.5),
            },
        },
        EditorCommand::Paste {
            clip: Clipboard::default(),
        },
    ]
}

#[test]
fn editor_command_json_and_toml_round_trip() {
    for cmd in sample_commands() {
        json_round_trip(&cmd);
        // TOML is the seam `editor_dispatch_toml` uses; a bare enum variant must
        // round-trip through it too.
        toml_round_trip(&cmd);
    }
}

#[test]
fn editor_query_round_trip() {
    let queries = [
        EditorQuery::Snapshot,
        EditorQuery::Samples,
        EditorQuery::BounceStatus {
            sample: SampleId::new(),
        },
        EditorQuery::Transport,
        EditorQuery::WavStats { sample: None },
        EditorQuery::Waveform {
            sample: Some(SampleId::new()),
            buckets: 256,
        },
    ];
    for q in &queries {
        json_round_trip(q);
        toml_round_trip(q);
    }
}

#[test]
fn query_result_round_trip() {
    let results = vec![
        QueryResult::Samples(vec![SampleInfo {
            id: SampleId::new(),
            name: "main".into(),
            kind: awsm_audio_schema::SampleKind::Sound,
            is_root: true,
            is_active: true,
            bounce: Some("clean".into()),
            duration_secs: Some(2.5),
        }]),
        QueryResult::BounceStatus("clean".into()),
        QueryResult::Transport(TransportInfo {
            playing: true,
            peak: 0.5,
            playhead: 1.25,
            audio_state: "running".into(),
        }),
        QueryResult::WavStats(WavStats {
            duration_secs: 2.0,
            peak: 0.9,
            rms: 0.6,
            channels: 2,
            sample_rate: 48_000,
        }),
        QueryResult::Waveform(WaveformEnvelope {
            sample_rate: 48_000,
            duration_secs: 2.0,
            min: vec![-1.0, -0.5],
            max: vec![1.0, 0.5],
        }),
    ];
    for r in &results {
        json_round_trip(r);
    }
}

#[test]
fn catalog_and_node_fields_round_trip() {
    let catalog = QueryResult::Catalog(vec![NodeKindInfo {
        kind: "oscillator".into(),
        label: "Oscillator".into(),
        section: "Sources".into(),
        description: "A periodic tone generator.".into(),
        mdn: "https://developer.mozilla.org/en-US/docs/Web/API/OscillatorNode".into(),
        example: NodeKind::Gain(Default::default()),
        fields: vec![
            FieldInfo {
                key: "type".into(),
                label: "type".into(),
                control: "choice".into(),
                value_num: None,
                value_text: Some("sine".into()),
                options: vec!["sine".into(), "square".into()],
                modulatable: false,
            },
            FieldInfo {
                key: "frequency".into(),
                label: "freq (Hz)".into(),
                control: "number".into(),
                value_num: Some(440.0),
                value_text: None,
                options: vec![],
                modulatable: true,
            },
        ],
    }]);
    json_round_trip(&catalog);

    let q = EditorQuery::NodeFields {
        node: NodeId::new(),
    };
    json_round_trip(&q);
    toml_round_trip(&q);
}

#[test]
fn request_round_trip() {
    let requests = vec![
        Request::Dispatch(EditorCommand::ClearSelection),
        Request::DispatchBatch(sample_commands()),
        Request::Query(EditorQuery::Snapshot),
        Request::Play,
        Request::Stop,
        Request::SetActiveSample {
            sample: SampleId::new(),
        },
        Request::RenderWav {
            sample: Some(SampleId::new()),
            sample_rate: Some(44_100.0),
            duration_secs: Some(8.0),
        },
        Request::RenderWav {
            sample: None,
            sample_rate: None,
            duration_secs: None,
        },
        Request::AttachWasm {
            node: NodeId::new(),
            wasm_base64: "AGFzbQEAAAA=".into(),
            label: "gain".into(),
        },
    ];
    for r in &requests {
        json_round_trip(r);
    }
}

#[test]
fn response_round_trip() {
    let responses = vec![
        Response::Ok,
        Response::Err("boom".into()),
        Response::Render(RenderHandle {
            render_id: "1f2e3d4c-5b6a-7980-1234-567890abcdef".into(),
            byte_len: 44,
            duration_secs: 1.5,
            peak: 0.9,
            rms: 0.5,
        }),
        Response::Query(Box::new(QueryResult::BounceStatus("dirty".into()))),
    ];
    for r in &responses {
        json_round_trip(r);
    }
}

/// Pin the externally-tagged `Request` JSON shape the morning-checklist `/debug`
/// payloads rely on.
#[test]
fn request_wire_shape() {
    let v = serde_json::to_value(Request::Play).unwrap();
    assert_eq!(v, Value::String("Play".into()));

    let v = serde_json::to_value(Request::RenderWav {
        sample: None,
        sample_rate: None,
        duration_secs: None,
    })
    .unwrap();
    // `RenderWav` with all fields skipped serializes to an empty object.
    assert_eq!(v, serde_json::json!({ "RenderWav": {} }));

    // The inner `EditorQuery` is adjacently tagged by "query"/"args".
    let v = serde_json::to_value(Request::Query(EditorQuery::Samples)).unwrap();
    assert_eq!(v, serde_json::json!({ "Query": { "query": "samples" } }));
}

/// `Clip` is reachable through `ArrangeOp::PasteClip`; make sure it round-trips.
#[test]
fn arrange_paste_clip_round_trip() {
    let cmd = EditorCommand::EditArrange {
        op: ArrangeOp::PasteClip {
            track: 1,
            clip: Clip::default(),
        },
    };
    json_round_trip(&cmd);
}

/// The loop/export markers op round-trips (set + clear).
#[test]
fn arrange_set_markers_round_trip() {
    for op in [
        ArrangeOp::SetMarkers {
            start: Some(2.0),
            end: Some(8.5),
        },
        ArrangeOp::SetMarkers {
            start: None,
            end: None,
        },
    ] {
        let cmd = EditorCommand::EditArrange { op };
        json_round_trip(&cmd);
        toml_round_trip(&cmd);
    }
}

// ── pure WAV-math helpers (the unattended coverage for the readback numbers) ──

fn sine(freq: f32, secs: f32, rate: u32) -> Vec<Vec<f32>> {
    let n = (secs * rate as f32) as usize;
    let ch: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / rate as f32;
            (2.0 * core::f32::consts::PI * freq * t).sin()
        })
        .collect();
    vec![ch]
}

#[test]
fn wav_stats_of_unit_sine() {
    // A full-scale 1 kHz sine: peak ≈ 1.0, rms ≈ 1/√2 ≈ 0.707, duration ≈ 1.0s.
    let s = WavStats::from_pcm(&sine(1000.0, 1.0, 48_000), 48_000);
    assert_eq!(s.channels, 1);
    assert_eq!(s.sample_rate, 48_000);
    assert!(
        (s.duration_secs - 1.0).abs() < 0.01,
        "duration {}",
        s.duration_secs
    );
    assert!((s.peak - 1.0).abs() < 0.01, "peak {}", s.peak);
    assert!((s.rms - 0.707).abs() < 0.02, "rms {}", s.rms);
}

#[test]
fn wav_stats_of_silence() {
    let s = WavStats::from_pcm(&[vec![0.0f32; 1000]], 44_100);
    assert_eq!(s.peak, 0.0);
    assert_eq!(s.rms, 0.0);
}

#[test]
fn waveform_buckets_within_bounds() {
    let w = WaveformEnvelope::from_pcm(&sine(1000.0, 1.0, 48_000), 48_000, 16);
    assert_eq!(w.min.len(), 16);
    assert_eq!(w.max.len(), 16);
    for i in 0..16 {
        assert!(w.min[i] <= w.max[i], "min>max at {i}");
        assert!(w.min[i] >= -1.0 && w.max[i] <= 1.0, "out of range at {i}");
    }
}

#[test]
fn waveform_of_ramp_is_monotonic() {
    // A 0→1 ramp: each later bucket's max should not decrease.
    let n = 16_000usize;
    let ramp: Vec<f32> = (0..n).map(|i| i as f32 / n as f32).collect();
    let w = WaveformEnvelope::from_pcm(&[ramp], 16_000, 8);
    for i in 1..w.max.len() {
        assert!(w.max[i] >= w.max[i - 1], "bucket {i} not monotonic");
    }
}

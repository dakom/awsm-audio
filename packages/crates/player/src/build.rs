//! Instantiate a pure-data [`Graph`] onto a live `web_sys::AudioContext`.
//!
//! Each schema [`Node`] becomes a real WebAudio node; the [`Connection`]s wire
//! them output→input. Any node whose output feeds nothing ("terminal") is
//! routed into `master`, which the [`Player`](crate::Player) has already wired
//! to the analyser + speakers. The few nodes we can't yet materialize (media
//! element/stream sources, nested samples) become silent gain pass-throughs so
//! the rest of the graph still connects and plays.

use anyhow::Result;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::{JsCast, JsValue};
use web_sys::{
    AudioBuffer, AudioNode, AudioParam, AudioScheduledSourceNode, BaseAudioContext, GainNode,
};

use awsm_audio_schema::{
    AssetId, AudioParam as SchemaParam, AutomationEvent, ConnectionSink, ConnectionSource, Graph,
    NodeId, NodeKind,
};

use crate::worklet;

thread_local! {
    /// Interned worklet param names: [`Built::params`] keys on `&'static str`,
    /// but worklet params are discovered at runtime. Leak each distinct name once
    /// (idempotent across rebuilds).
    static INTERN: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

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

/// Apply a schema [`AudioParam`](SchemaParam) — its base value plus the
/// automation timeline — to a live web_sys [`AudioParam`]. Automation times are
/// relative to `t0` (the note-on / play moment, in context time).
fn apply_param(p: &AudioParam, sp: &SchemaParam, t0: f64) {
    p.set_value(sp.value);
    for ev in &sp.automation {
        let r = match ev {
            AutomationEvent::SetValue { value, time } => p.set_value_at_time(*value, t0 + *time),
            AutomationEvent::LinearRamp { value, time } => {
                p.linear_ramp_to_value_at_time(*value, t0 + *time)
            }
            AutomationEvent::ExponentialRamp { value, time } => {
                // Exponential ramps can't reach 0; clamp to a tiny positive.
                let v = if *value == 0.0 { 1e-5 } else { *value };
                p.exponential_ramp_to_value_at_time(v, t0 + *time)
            }
            AutomationEvent::SetTarget {
                target,
                start_time,
                time_constant,
            } => p.set_target_at_time(*target, t0 + *start_time, *time_constant),
            AutomationEvent::SetValueCurve {
                values,
                start_time,
                duration,
            } => {
                let mut v = values.clone();
                p.set_value_curve_at_time(&mut v, t0 + *start_time, *duration)
            }
            // `cancelAndHoldAtTime` isn't in stable web-sys; approximate with a
            // plain cancel (drops scheduled events from `time` onward).
            AutomationEvent::CancelScheduled { time } | AutomationEvent::CancelAndHold { time } => {
                p.cancel_scheduled_values(t0 + *time)
            }
        };
        if let Err(e) = r {
            tracing::error!("automation event failed: {e:?}");
        }
    }
}

fn js_err(label: &str, e: wasm_bindgen::JsValue) -> anyhow::Error {
    anyhow::anyhow!("{label}: {e:?}")
}

/// Apply the context-level spatial listener position + orientation (base values;
/// web-sys exposes the older setters, not the per-axis AudioParams).
pub fn apply_listener(ctx: &BaseAudioContext, l: &awsm_audio_schema::Listener, _t0: f64) {
    let lis = ctx.listener();
    #[allow(deprecated)]
    {
        lis.set_position(
            l.position_x.value as f64,
            l.position_y.value as f64,
            l.position_z.value as f64,
        );
        lis.set_orientation(
            l.forward_x.value as f64,
            l.forward_y.value as f64,
            l.forward_z.value as f64,
            l.up_x.value as f64,
            l.up_y.value as f64,
            l.up_z.value as f64,
        );
    }
}

/// Build `graph` into `ctx`, routing terminals into `master`. Returns the
/// created audio nodes (kept alive by the caller) and the schedulable sources
/// (oscillators etc.) that still need `start()`.
#[allow(clippy::too_many_arguments)]
pub fn build_graph(
    ctx: &BaseAudioContext,
    graph: &Graph,
    master: &GainNode,
    buffers: &HashMap<AssetId, AudioBuffer>,
    modules: &HashMap<AssetId, js_sys::WebAssembly::Module>,
    mic: Option<&web_sys::MediaStream>,
    worklet_ready: bool,
    looping: bool,
    t0: f64,
) -> Result<BuiltGraph> {
    let mut by_id: Vec<(NodeId, AudioNode)> = Vec::with_capacity(graph.nodes.len());
    // Per-node automatable params, keyed by their WebAudio name, for modulation.
    let mut params_by_id: Vec<(NodeId, Vec<(&'static str, AudioParam)>)> = Vec::new();
    let mut inner: Vec<AudioNode> = Vec::with_capacity(graph.nodes.len());
    let mut sources: Vec<AudioScheduledSourceNode> = Vec::new();
    // Sink nodes (Output): their chain end always connects to master, and they
    // are excluded from the generic terminal auto-route.
    let mut sinks: Vec<AudioNode> = Vec::new();
    let mut sink_ids: HashSet<NodeId> = HashSet::new();

    for node in &graph.nodes {
        let built = build_node(
            ctx,
            &node.kind,
            buffers,
            modules,
            mic,
            worklet_ready,
            looping,
            t0,
        )?;
        by_id.push((node.id, built.node.clone()));
        params_by_id.push((node.id, built.params));
        inner.push(built.node);
        if let Some(s) = built.source {
            sources.push(s);
        }
        if let Some(sink_out) = built.sink_out {
            sink_ids.insert(node.id);
            inner.push(sink_out.clone());
            sinks.push(sink_out);
        }
    }

    let node_of = |id: NodeId| by_id.iter().find(|(nid, _)| *nid == id).map(|(_, n)| n);
    let param_of = |id: NodeId, name: &str| {
        params_by_id
            .iter()
            .find(|(nid, _)| *nid == id)
            .and_then(|(_, ps)| ps.iter().find(|(n, _)| *n == name).map(|(_, p)| p.clone()))
    };

    // Any node that feeds *something* (audio input or a param) is "driven" and
    // so is not auto-routed to the speakers.
    let mut driven: HashSet<NodeId> = HashSet::new();
    for conn in &graph.connections {
        let ConnectionSource::NodeOutput { node: from, output } = &conn.from else {
            continue;
        };
        match &conn.to {
            ConnectionSink::NodeInput { node: to, input } => {
                if let (Some(src), Some(dst)) = (node_of(*from), node_of(*to)) {
                    src.connect_with_audio_node_and_output_and_input(dst, *output, *input)
                        .map_err(|e| js_err("connect", e))?;
                    driven.insert(*from);
                }
            }
            ConnectionSink::NodeParam { node: to, param } => {
                if let (Some(src), Some(p)) = (node_of(*from), param_of(*to, &param.0)) {
                    src.connect_with_audio_param(&p)
                        .map_err(|e| js_err("connect-param", e))?;
                    driven.insert(*from);
                }
            }
            ConnectionSink::Outlet { .. } => {}
            // Triggers aren't audio edges — the scheduler consumes them.
            ConnectionSink::Trigger { .. } => {}
        }
    }

    // Explicit Output sinks always reach the speakers (via their panner/gain).
    for sink in &sinks {
        sink.connect_with_audio_node(master)
            .map_err(|e| js_err("connect-sink", e))?;
    }

    // Other terminal nodes (output feeds nothing) auto-route to the master bus,
    // so graphs without an explicit Output node still play.
    for (id, node) in &by_id {
        if !driven.contains(id) && !sink_ids.contains(id) {
            node.connect_with_audio_node(master)
                .map_err(|e| js_err("connect-master", e))?;
        }
    }

    Ok(BuiltGraph {
        inner,
        sources,
        params: params_by_id,
        nodes: by_id,
    })
}

/// The materialized graph handed back to the [`Player`](crate::Player): the live
/// nodes (kept alive until stop), the schedulable sources, and the per-node
/// automatable params by WebAudio name — the latter so the player can nudge a
/// param live (MIDI CC sweeps) without rebuilding the whole graph.
pub struct BuiltGraph {
    pub inner: Vec<AudioNode>,
    pub sources: Vec<AudioScheduledSourceNode>,
    pub params: Vec<(NodeId, Vec<(&'static str, AudioParam)>)>,
    /// Every node's live `AudioNode` keyed by id — so the arrangement scheduler
    /// can find an instrument-ref's voice-bus gain to spawn triggered voices into.
    pub nodes: Vec<(NodeId, AudioNode)>,
}

/// A materialized node: the audio node, its schedulable source handle (if a
/// source), and its automatable params (by WebAudio name) for modulation.
struct Built {
    node: AudioNode,
    source: Option<AudioScheduledSourceNode>,
    params: Vec<(&'static str, AudioParam)>,
    /// For sink nodes (Output): the end of the internal chain that connects to
    /// the master bus. `None` for ordinary nodes.
    sink_out: Option<AudioNode>,
}

/// Materialize one node. Returns its `AudioNode` plus, for source nodes, the
/// schedulable handle to `start()`.
#[allow(clippy::too_many_arguments)]
fn build_node(
    ctx: &BaseAudioContext,
    kind: &NodeKind,
    buffers: &HashMap<AssetId, AudioBuffer>,
    modules: &HashMap<AssetId, js_sys::WebAssembly::Module>,
    mic: Option<&web_sys::MediaStream>,
    worklet_ready: bool,
    looping: bool,
    t0: f64,
) -> Result<Built> {
    let plain = |node: AudioNode, source, params| Built {
        node,
        source,
        params,
        sink_out: None,
    };

    Ok(match kind {
        NodeKind::Oscillator(o) => {
            let n = ctx
                .create_oscillator()
                .map_err(|e| js_err("oscillator", e))?;
            // A `custom` waveform is selected *only* via setPeriodicWave — assigning
            // `type = "custom"` directly throws ("cannot be set directly to
            // 'custom'") and, in the offline context, kills the whole render. So
            // for Custom we build the PeriodicWave from the harmonic amplitudes
            // (imag = sine series, index 0 = DC) and never touch `set_type`; for
            // every other type we set it normally.
            if o.oscillator_type == awsm_audio_schema::OscillatorType::Custom {
                if o.harmonics.is_empty() {
                    // No harmonics to build a wave from — fall back to the default
                    // (sine) rather than the illegal `custom` assignment.
                    n.set_type(web_sys::OscillatorType::Sine);
                } else {
                    let mut real = vec![0.0f32; o.harmonics.len() + 1];
                    let mut imag = vec![0.0f32; o.harmonics.len() + 1];
                    for (i, h) in o.harmonics.iter().enumerate() {
                        imag[i + 1] = *h;
                    }
                    let wave = ctx
                        .create_periodic_wave(&mut real, &mut imag)
                        .map_err(|e| js_err("oscillator periodic wave", e))?;
                    n.set_periodic_wave(&wave);
                }
            } else {
                n.set_type(osc_type(o.oscillator_type));
            }
            apply_param(&n.frequency(), &o.frequency, t0);
            apply_param(&n.detune(), &o.detune, t0);
            let src: AudioScheduledSourceNode = n.clone().unchecked_into();
            let params = vec![("frequency", n.frequency()), ("detune", n.detune())];
            plain(n.unchecked_into(), Some(src), params)
        }
        NodeKind::ConstantSource(c) => {
            let n = ctx
                .create_constant_source()
                .map_err(|e| js_err("constant", e))?;
            apply_param(&n.offset(), &c.offset, t0);
            let src: AudioScheduledSourceNode = n.clone().unchecked_into();
            let params = vec![("offset", n.offset())];
            plain(n.unchecked_into(), Some(src), params)
        }
        NodeKind::Noise(nz) => {
            let sr = ctx.sample_rate();
            let len = ((nz.seconds.max(0.05) * sr) as usize).max(1);
            let channels = if nz.stereo { 2u32 } else { 1u32 };
            let buffer = ctx
                .create_buffer(channels, len as u32, sr)
                .map_err(|e| js_err("noise buffer", e))?;
            for ch in 0..channels {
                // Decorrelate channels by perturbing the seed.
                let seed = nz
                    .seed
                    .wrapping_add(u64::from(ch).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                let data =
                    crate::noise::generate(nz.flavor, seed, len, sr, nz.density, nz.gaussian);
                buffer
                    .copy_to_channel(&data, ch as i32)
                    .map_err(|e| js_err("noise copy", e))?;
            }
            let n = ctx
                .create_buffer_source()
                .map_err(|e| js_err("noise source", e))?;
            n.set_buffer(Some(&buffer));
            n.set_loop(true);
            let src: AudioScheduledSourceNode = n.clone().unchecked_into();
            plain(n.unchecked_into(), Some(src), vec![])
        }
        NodeKind::AudioBufferSource(b) => {
            let n = ctx
                .create_buffer_source()
                .map_err(|e| js_err("buffer-source", e))?;
            apply_param(&n.playback_rate(), &b.playback_rate, t0);
            apply_param(&n.detune(), &b.detune, t0);
            n.set_loop(looping || b.looping);
            // Attach the decoded clip, if one has been loaded for this asset.
            if let Some(buf) = b.buffer.as_ref().and_then(|id| buffers.get(id)) {
                n.set_buffer(Some(buf));
            }
            let src: AudioScheduledSourceNode = n.clone().unchecked_into();
            let params = vec![("playbackRate", n.playback_rate()), ("detune", n.detune())];
            plain(n.unchecked_into(), Some(src), params)
        }
        NodeKind::Gain(g) => {
            let n = ctx.create_gain().map_err(|e| js_err("gain", e))?;
            apply_param(&n.gain(), &g.gain, t0);
            let params = vec![("gain", n.gain())];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::BiquadFilter(b) => {
            let n = ctx
                .create_biquad_filter()
                .map_err(|e| js_err("biquad", e))?;
            n.set_type(biquad_type(b.filter_type));
            apply_param(&n.frequency(), &b.frequency, t0);
            apply_param(&n.detune(), &b.detune, t0);
            apply_param(&n.q(), &b.q, t0);
            apply_param(&n.gain(), &b.gain, t0);
            let params = vec![
                ("frequency", n.frequency()),
                ("detune", n.detune()),
                ("Q", n.q()),
                ("gain", n.gain()),
            ];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::Delay(d) => {
            let n = ctx
                .create_delay_with_max_delay_time(d.max_delay_time)
                .map_err(|e| js_err("delay", e))?;
            apply_param(&n.delay_time(), &d.delay_time, t0);
            let params = vec![("delayTime", n.delay_time())];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::DynamicsCompressor(c) => {
            let n = ctx
                .create_dynamics_compressor()
                .map_err(|e| js_err("compressor", e))?;
            apply_param(&n.threshold(), &c.threshold, t0);
            apply_param(&n.knee(), &c.knee, t0);
            apply_param(&n.ratio(), &c.ratio, t0);
            apply_param(&n.attack(), &c.attack, t0);
            apply_param(&n.release(), &c.release, t0);
            let params = vec![
                ("threshold", n.threshold()),
                ("knee", n.knee()),
                ("ratio", n.ratio()),
                ("attack", n.attack()),
                ("release", n.release()),
            ];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::WaveShaper(ws) => {
            let n = ctx
                .create_wave_shaper()
                .map_err(|e| js_err("waveshaper", e))?;
            n.set_oversample(oversample(ws.oversample));
            // A user-drawn curve (Custom) is resampled to the WebAudio table;
            // otherwise the curve is generated from the shape + amount.
            let mut curve =
                if ws.shape == awsm_audio_schema::WaveShaperShape::Custom && !ws.curve.is_empty() {
                    resample_curve(&ws.curve, 1024)
                } else {
                    distortion_curve(ws.shape, ws.amount)
                };
            #[allow(deprecated)] // the slice-based setter is the one web-sys exposes
            n.set_curve(Some(&mut curve));
            plain(n.unchecked_into(), None, vec![])
        }
        NodeKind::Convolver(cv) => {
            let n = ctx.create_convolver().map_err(|e| js_err("convolver", e))?;
            n.set_normalize(!cv.disable_normalization);
            // Use a loaded IR if present, else a synthetic decaying-noise reverb.
            if let Some(buf) = cv.buffer.as_ref().and_then(|id| buffers.get(id)) {
                n.set_buffer(Some(buf));
            } else {
                let ir = default_impulse(ctx, cv.reverb_seconds.clamp(0.05, 20.0))?;
                n.set_buffer(Some(&ir));
            }
            plain(n.unchecked_into(), None, vec![])
        }
        NodeKind::StereoPanner(p) => {
            let n = ctx
                .create_stereo_panner()
                .map_err(|e| js_err("stereo-panner", e))?;
            apply_param(&n.pan(), &p.pan, t0);
            let params = vec![("pan", n.pan())];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::Panner(pn) => {
            let n = ctx.create_panner().map_err(|e| js_err("panner", e))?;
            n.set_panning_model(panning_model(pn.panning_model));
            n.set_distance_model(distance_model(pn.distance_model));
            n.set_ref_distance(pn.ref_distance);
            n.set_max_distance(pn.max_distance);
            n.set_rolloff_factor(pn.rolloff_factor);
            n.set_cone_inner_angle(pn.cone_inner_angle);
            n.set_cone_outer_angle(pn.cone_outer_angle);
            n.set_cone_outer_gain(pn.cone_outer_gain);
            apply_param(&n.position_x(), &pn.position_x, t0);
            apply_param(&n.position_y(), &pn.position_y, t0);
            apply_param(&n.position_z(), &pn.position_z, t0);
            apply_param(&n.orientation_x(), &pn.orientation_x, t0);
            apply_param(&n.orientation_y(), &pn.orientation_y, t0);
            apply_param(&n.orientation_z(), &pn.orientation_z, t0);
            let params = vec![
                ("positionX", n.position_x()),
                ("positionY", n.position_y()),
                ("positionZ", n.position_z()),
                ("orientationX", n.orientation_x()),
                ("orientationY", n.orientation_y()),
                ("orientationZ", n.orientation_z()),
            ];
            plain(n.unchecked_into(), None, params)
        }
        NodeKind::Analyser(a) => {
            let n = ctx.create_analyser().map_err(|e| js_err("analyser", e))?;
            n.set_fft_size(a.fft_size);
            // maxDecibels must stay > minDecibels (WebAudio throws otherwise);
            // set max first, and only if the pair is valid.
            if a.max_decibels > a.min_decibels {
                n.set_max_decibels(a.max_decibels);
                n.set_min_decibels(a.min_decibels);
            }
            n.set_smoothing_time_constant(a.smoothing_time_constant.clamp(0.0, 1.0));
            plain(n.unchecked_into(), None, vec![])
        }
        NodeKind::ChannelSplitter(s) => {
            let n = ctx
                .create_channel_splitter_with_number_of_outputs(s.number_of_outputs)
                .map_err(|e| js_err("splitter", e))?;
            plain(n.unchecked_into(), None, vec![])
        }
        NodeKind::ChannelMerger(m) => {
            let n = ctx
                .create_channel_merger_with_number_of_inputs(m.number_of_inputs)
                .map_err(|e| js_err("merger", e))?;
            plain(n.unchecked_into(), None, vec![])
        }
        NodeKind::Output(o) => {
            // input gain → [master, wired by caller].
            let g = ctx.create_gain().map_err(|e| js_err("output gain", e))?;
            apply_param(&g.gain(), &o.gain, t0);
            Built {
                node: g.clone().unchecked_into(),
                source: None,
                params: vec![("gain", g.gain())],
                sink_out: Some(g.unchecked_into()),
            }
        }
        NodeKind::SpatialOutput(o) => {
            // input gain → HRTF panner → [master, wired by caller].
            let g = ctx.create_gain().map_err(|e| js_err("output gain", e))?;
            apply_param(&g.gain(), &o.gain, t0);
            let panner = ctx
                .create_panner()
                .map_err(|e| js_err("output panner", e))?;
            panner.set_panning_model(web_sys::PanningModelType::Hrtf);
            apply_param(&panner.position_x(), &o.position_x, t0);
            apply_param(&panner.position_y(), &o.position_y, t0);
            apply_param(&panner.position_z(), &o.position_z, t0);
            g.connect_with_audio_node(&panner)
                .map_err(|e| js_err("output g→panner", e))?;
            Built {
                node: g.clone().unchecked_into(),
                source: None,
                params: vec![
                    ("gain", g.gain()),
                    ("positionX", panner.position_x()),
                    ("positionY", panner.position_y()),
                    ("positionZ", panner.position_z()),
                ],
                sink_out: Some(panner.unchecked_into()),
            }
        }
        // WASM worklet: instantiate the module behind the generic shim, mapping
        // its discovered params onto the shim's AudioParam bank. Falls back to a
        // silent pass-through until both the shim and the module are loaded.
        NodeKind::AudioWorklet(w) => match w.module.as_ref().and_then(|id| modules.get(id)) {
            Some(module) if worklet_ready => build_worklet(ctx, w, module, t0)?,
            _ => {
                let g = ctx.create_gain().map_err(|e| js_err("worklet gain", e))?;
                plain(g.unchecked_into(), None, vec![])
            }
        },
        NodeKind::IirFilter(iir) => {
            // create_iir_filter needs valid coeff arrays; fall back to a gentle
            // one-pole low-pass when unset.
            let (ff, fb) = if !iir.feedforward.is_empty() && !iir.feedback.is_empty() {
                (iir.feedforward.clone(), iir.feedback.clone())
            } else {
                (vec![0.2, 0.2], vec![1.0, -0.6])
            };
            let ff = js_sys::Float64Array::from(ff.as_slice());
            let fb = js_sys::Float64Array::from(fb.as_slice());
            let n = ctx
                .create_iir_filter(ff.as_ref(), fb.as_ref())
                .map_err(|e| js_err("iir", e))?;
            plain(n.unchecked_into(), None, vec![])
        }
        // Media element: play a URL through an <audio> element. Online-only
        // (offline contexts have no media elements) → else silent.
        NodeKind::MediaElementSource(m) => match ctx.dyn_ref::<web_sys::AudioContext>() {
            Some(ac) if !m.src.is_empty() => {
                media_element_source(ac, &m.src).or_else(|_| plain_gain(ctx))?
            }
            _ => plain_gain(ctx)?,
        },
        // Microphone: tap the live input stream (acquired by the editor). Online
        // + stream present → else silent.
        NodeKind::MediaStreamSource(_) => match (ctx.dyn_ref::<web_sys::AudioContext>(), mic) {
            (Some(ac), Some(stream)) => match ac.create_media_stream_source(stream) {
                Ok(n) => plain(n.unchecked_into(), None, vec![]),
                Err(_) => plain_gain(ctx)?,
            },
            _ => plain_gain(ctx)?,
        },
        // Nested samples are inlined into the graph before build; a stray one is
        // a silent pass-through.
        NodeKind::Sample(_) => plain_gain(ctx)?,
        // Sequencers emit no audio of their own (they drive instruments/params
        // via the scheduler); in an audio graph they're silent pass-throughs.
        NodeKind::NoteSequencer(_) | NodeKind::ControlSequencer(_) => plain_gain(ctx)?,
        // A bus is a (named) unity gain — every WebAudio node sums its inputs.
        NodeKind::Bus(b) => {
            let g = ctx.create_gain().map_err(|e| js_err("bus", e))?;
            g.gain().set_value(b.gain);
            plain(g.unchecked_into(), None, vec![])
        }
    })
}

/// A silent gain pass-through (placeholder for nodes that can't materialize).
fn plain_gain(ctx: &BaseAudioContext) -> Result<Built> {
    let g = ctx.create_gain().map_err(|e| js_err("gain", e))?;
    Ok(Built {
        node: g.unchecked_into(),
        source: None,
        params: vec![],
        sink_out: None,
    })
}

/// Create an `<audio>` element for `src`, start it looping, and tap it with a
/// MediaElementAudioSourceNode (which keeps the element alive).
fn media_element_source(ac: &web_sys::AudioContext, src: &str) -> Result<Built> {
    let doc = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| anyhow::anyhow!("no document"))?;
    let el: web_sys::HtmlMediaElement = doc
        .create_element("audio")
        .map_err(|e| js_err("create audio", e))?
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("not a media element"))?;
    el.set_src(src);
    el.set_cross_origin(Some("anonymous"));
    el.set_loop(true);
    let _ = el.play();
    let n = ac
        .create_media_element_source(&el)
        .map_err(|e| js_err("media element source", e))?;
    Ok(Built {
        node: n.unchecked_into(),
        source: None,
        params: vec![],
        sink_out: None,
    })
}

/// Instantiate a WASM worklet node: create the generic shim processor with the
/// compiled `module` in its `processorOptions`, then map the node's discovered
/// params onto the shim's `p0..` AudioParam bank (so they're automatable and
/// modulation targets, keyed by their real names).
fn build_worklet(
    ctx: &BaseAudioContext,
    w: &awsm_audio_schema::AudioWorkletNode,
    module: &js_sys::WebAssembly::Module,
    t0: f64,
) -> Result<Built> {
    let opts = web_sys::AudioWorkletNodeOptions::new();
    opts.set_number_of_inputs(1);
    opts.set_number_of_outputs(1);
    // Stereo output (the shim/ABI process 2 channels).
    let out_ch = js_sys::Array::of1(&JsValue::from_f64(2.0));
    opts.set_output_channel_count(&out_ch);

    // processorOptions = { module }.
    let po = js_sys::Object::new();
    js_sys::Reflect::set(&po, &JsValue::from_str("module"), module)
        .map_err(|e| js_err("processorOptions.module", e))?;
    opts.set_processor_options(Some(&po));

    let node = web_sys::AudioWorkletNode::new_with_options(ctx, worklet::PROCESSOR_NAME, &opts)
        .map_err(|e| js_err("AudioWorkletNode", e))?;

    let pmap = node
        .parameters()
        .map_err(|e| js_err("worklet parameters", e))?;
    let mut params: Vec<(&'static str, AudioParam)> = Vec::new();
    for (i, wp) in w.parameters.iter().enumerate() {
        if i >= worklet::PARAM_BANK {
            break;
        }
        if let Some(ap) = pmap.get(&format!("p{i}")) {
            apply_param(&ap, &wp.param, t0);
            params.push((intern(&wp.name.0), ap));
        }
    }

    Ok(Built {
        node: node.unchecked_into(),
        source: None,
        params,
        sink_out: None,
    })
}

fn osc_type(t: awsm_audio_schema::OscillatorType) -> web_sys::OscillatorType {
    use awsm_audio_schema::OscillatorType as S;
    use web_sys::OscillatorType as W;
    match t {
        S::Sine => W::Sine,
        S::Square => W::Square,
        S::Sawtooth => W::Sawtooth,
        S::Triangle => W::Triangle,
        S::Custom => W::Custom,
    }
}

fn biquad_type(t: awsm_audio_schema::BiquadFilterType) -> web_sys::BiquadFilterType {
    use awsm_audio_schema::BiquadFilterType as S;
    use web_sys::BiquadFilterType as W;
    match t {
        S::Lowpass => W::Lowpass,
        S::Highpass => W::Highpass,
        S::Bandpass => W::Bandpass,
        S::Lowshelf => W::Lowshelf,
        S::Highshelf => W::Highshelf,
        S::Peaking => W::Peaking,
        S::Notch => W::Notch,
        S::Allpass => W::Allpass,
    }
}

fn panning_model(t: awsm_audio_schema::PanningModelType) -> web_sys::PanningModelType {
    use awsm_audio_schema::PanningModelType as S;
    use web_sys::PanningModelType as W;
    match t {
        S::EqualPower => W::Equalpower,
        S::Hrtf => W::Hrtf,
    }
}

fn distance_model(t: awsm_audio_schema::DistanceModelType) -> web_sys::DistanceModelType {
    use awsm_audio_schema::DistanceModelType as S;
    use web_sys::DistanceModelType as W;
    match t {
        S::Linear => W::Linear,
        S::Inverse => W::Inverse,
        S::Exponential => W::Exponential,
    }
}

fn oversample(t: awsm_audio_schema::OverSampleType) -> web_sys::OverSampleType {
    use awsm_audio_schema::OverSampleType as S;
    use web_sys::OverSampleType as W;
    match t {
        S::None => W::None,
        S::X2 => W::N2x,
        S::X4 => W::N4x,
    }
}

/// A WaveShaper distortion curve from a `shape` (character) + `amount`
/// (intensity, 0 ≈ gentle). 1024 samples across the input range [-1, 1].
fn distortion_curve(shape: awsm_audio_schema::WaveShaperShape, amount: f32) -> Vec<f32> {
    use awsm_audio_schema::WaveShaperShape as S;
    let k = amount.max(0.0);
    let drive = 1.0 + k;
    let n = 1024usize;
    (0..n)
        .map(|i| {
            let x = (i as f32 / (n - 1) as f32) * 2.0 - 1.0;
            match shape {
                // Smooth saturation; at amount 0 it's ~linear.
                S::Tanh => (drive * x).tanh() / drive.tanh(),
                // Hard clip: linear gain then clamp.
                S::HardClip => (drive * x).clamp(-1.0, 1.0),
                // Sine wavefolder: folds back on itself as drive rises.
                S::Fold => (drive * x * std::f32::consts::FRAC_PI_2).sin(),
                // Custom with no drawn curve falls back to gentle tanh.
                S::Custom => (drive * x).tanh() / drive.tanh(),
            }
        })
        .collect()
}

/// Resample a user-drawn transfer curve (output values across input -1..1) to
/// `n` evenly-spaced samples via linear interpolation, clamped to [-1, 1].
fn resample_curve(points: &[f32], n: usize) -> Vec<f32> {
    if points.is_empty() {
        return vec![0.0; n];
    }
    if points.len() == 1 {
        return vec![points[0].clamp(-1.0, 1.0); n];
    }
    let last = points.len() - 1;
    (0..n)
        .map(|i| {
            let pos = i as f32 / (n - 1) as f32 * last as f32;
            let lo = pos.floor() as usize;
            let hi = (lo + 1).min(last);
            let frac = pos - lo as f32;
            (points[lo] + (points[hi] - points[lo]) * frac).clamp(-1.0, 1.0)
        })
        .collect()
}

/// A synthetic impulse response (exponentially-decaying noise) for a Convolver
/// with no IR loaded — a quick, usable plate-ish reverb.
fn default_impulse(ctx: &BaseAudioContext, seconds: f32) -> Result<AudioBuffer> {
    let sr = ctx.sample_rate();
    let len = ((sr * seconds) as usize).max(1);
    let buffer = ctx
        .create_buffer(2, len as u32, sr)
        .map_err(|e| js_err("ir buffer", e))?;
    for ch in 0..2u32 {
        // Deterministic per-channel noise with an exponential decay envelope.
        let data: Vec<f32> = crate::noise::generate(
            awsm_audio_schema::NoiseFlavor::White,
            0x51F0_C0DE ^ u64::from(ch).wrapping_mul(0x9E37_79B9),
            len,
            sr,
            0.0,
            false,
        )
        .iter()
        .enumerate()
        .map(|(i, &s)| {
            let t = i as f32 / len as f32;
            s * (-6.0 * t).exp()
        })
        .collect();
        buffer
            .copy_to_channel(&data, ch as i32)
            .map_err(|e| js_err("ir copy", e))?;
    }
    Ok(buffer)
}

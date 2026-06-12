//! A runnable demo of [`awsm_audio_player::document`] — the same engine the
//! editor drives. It builds a couple of small local fixtures, [`Player::register`]s
//! their assets, and plays any of them with
//! [`Player::play_document`]: a plain Sound, a sequencer Sequence, or an Arrangement.
//!
//! Clicking a card opens a **per-sound player** whose live controls are built
//! from [`Player::live_params`] — so a spatial sound gets X/Y/Z sliders, an
//! oscillator gets a frequency slider, a filter gets cutoff/Q, etc. Each slider
//! drives [`Player::set_param_live`] without restarting the sound.
//!
//! Run it locally: `task example-dev` (or `trunk serve` here). It is not deployed.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use dominator::{clone, events, html, stylesheet, with_node, Dom};
use futures_signals::signal::{Mutable, SignalExt};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

use awsm_audio_player::document::{classify, PlayKind, PlayOptions, Playback, SoundShape};
use awsm_audio_player::Player;
use awsm_audio_schema::{
    ArrTrack, AssetId, AudioParam, Bounce, Clip, Connection, GainNode, Node, NodeId, NodeKind,
    OscillatorNode, Sample, SampleId, SampleLibrary,
};

/// One playable project in the picker.
struct Project {
    name: String,
    lib: SampleLibrary,
    target: SampleId,
    kind: PlayKind,
}

/// What the open sound turned out to be (measured for Sounds, known for the rest).
#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// Not measured yet.
    Unknown,
    /// A Sound that decays to silence after N seconds — loopable, frees on end.
    OneShot(f64),
    /// A Sound that runs forever — no loop, no natural end.
    Drone,
    /// A Sequence / Arrangement — timed content the engine loops natively.
    Timed,
}

impl Shape {
    /// Does Loop make sense? (Not for a free-running drone.)
    fn loopable(self) -> bool {
        matches!(self, Shape::OneShot(_) | Shape::Timed)
    }
    fn label(self) -> String {
        match self {
            Shape::Unknown => "measuring…".to_string(),
            Shape::OneShot(s) => format!("one-shot · {s:.1} s"),
            Shape::Drone => "sustaining (drone)".to_string(),
            Shape::Timed => "loops".to_string(),
        }
    }
}

/// A live-controllable param surfaced in the open sound's modal — discovered via
/// [`Player::live_params`], ranged + initialized for a slider.
struct ParamCtl {
    node: NodeId,
    /// Owning node's kind name (e.g. "Oscillator", "Spatial Output").
    kind: &'static str,
    /// WebAudio param name (e.g. "frequency", "positionX").
    param: &'static str,
    min: f64,
    max: f64,
    step: f64,
    /// The sound's current value (slider start).
    value: f64,
}

/// The whole demo's state. Single-threaded wasm, so plain `RefCell`s.
struct App {
    /// Lazily created on the first play (an `AudioContext` needs a user gesture).
    player: RefCell<Option<Player>>,
    /// The active playback handle (drives looping / end-detection).
    playback: RefCell<Option<Playback>>,
    projects: Vec<Project>,
    /// True while `register` is in flight (the player is taken out of its cell).
    loading: Mutable<bool>,
    /// Which project's player modal is open.
    open: Mutable<Option<usize>>,
    /// Live controls for the open sound (populated after it starts playing).
    controls: Mutable<Rc<Vec<ParamCtl>>>,
    /// The open sound's measured shape (one-shot / drone / timed). Cached per open
    /// so Replay doesn't re-measure.
    shape: Mutable<Shape>,
    /// The latest value the user set for each param, so a Replay / Loop re-play
    /// keeps your tweaks instead of snapping back to the document's defaults.
    /// Keyed by the stable node id + param name; cleared when a new sound opens.
    live_values: RefCell<HashMap<(NodeId, &'static str), f32>>,
    looping: Mutable<bool>,
    gain: Mutable<f64>,
    status: Mutable<String>,
}

impl App {
    fn new() -> Rc<Self> {
        let mut projects: Vec<Project> = Vec::new();

        // A single-oscillator Sound (frequency control demo).
        let (lib, target) = live_tone();
        projects.push(Project {
            kind: classify(&lib, target),
            name: "live tone".to_string(),
            lib,
            target,
        });
        // A fabricated arrangement (the built-in one is empty).
        let (lib, target) = arrangement_demo();
        projects.push(Project {
            kind: classify(&lib, target),
            name: "arrangement demo".to_string(),
            lib,
            target,
        });

        Rc::new(Self {
            player: RefCell::new(None),
            playback: RefCell::new(None),
            projects,
            loading: Mutable::new(false),
            open: Mutable::new(None),
            controls: Mutable::new(Rc::new(Vec::new())),
            shape: Mutable::new(Shape::Unknown),
            live_values: RefCell::new(HashMap::new()),
            looping: Mutable::new(false),
            gain: Mutable::new(1.0),
            status: Mutable::new(String::new()),
        })
    }

    /// Open a sound's player modal (and start it). A fresh sound starts from its
    /// own defaults, so drop any preserved tweaks from a previous one.
    fn open(self: &Rc<Self>, idx: usize) {
        self.looping.set(false);
        self.open.set(Some(idx));
        self.controls.set(Rc::new(Vec::new()));
        self.shape.set(Shape::Unknown);
        self.live_values.borrow_mut().clear();
        self.play(idx);
    }

    /// Close the modal (and stop playback).
    fn close(&self) {
        self.stop();
        self.open.set(None);
        self.controls.set(Rc::new(Vec::new()));
        self.live_values.borrow_mut().clear();
    }

    /// Register the open project's assets, play it, then surface its live controls.
    fn play(self: &Rc<Self>, idx: usize) {
        if self.loading.get() {
            return;
        }
        // Create + resume the AudioContext *synchronously, within this click* — the
        // browser's autoplay policy only starts audio from a user gesture, and the
        // async register below would otherwise miss that window.
        {
            let mut slot = self.player.borrow_mut();
            if slot.is_none() {
                match Player::new() {
                    Ok(p) => *slot = Some(p),
                    Err(e) => {
                        tracing::error!("audio init failed: {e}");
                        self.status.set(format!("audio init failed: {e}"));
                        return;
                    }
                }
            }
            if let Some(p) = slot.as_ref() {
                p.resume();
            }
        }

        self.loading.set(true);
        let app = self.clone();
        spawn_local(async move {
            let (name, lib, target, looping, gain) = {
                let p = &app.projects[idx];
                (
                    p.name.clone(),
                    p.lib.clone(),
                    p.target,
                    app.looping.get(),
                    app.gain.get() as f32,
                )
            };
            app.status.set(format!(
                "Loading “{name}” — decoding / compiling / bouncing…"
            ));

            // The context was ensured synchronously above.
            let mut player = app.player.borrow_mut().take().expect("player ensured");

            // Prepare every dependency, maximally concurrent.
            if let Err(e) = player.register(&lib).await {
                return app.abort(Some(player), format!("register failed: {e}"));
            }

            let kind = classify(&lib, target);

            // Work out the sound's shape (cached per open). A Sound is measured —
            // one-shot (decays) or drone (sustains); a Sequence/Arrangement is Timed.
            let shape = match (kind, app.shape.get()) {
                (PlayKind::Sound, Shape::Unknown) => {
                    app.status.set(format!("Measuring “{name}”…"));
                    match player.measure_sound(&lib, target, 20.0).await {
                        Ok(SoundShape::OneShot { secs }) => Shape::OneShot(secs),
                        Ok(SoundShape::Sustaining) => Shape::Drone,
                        Err(e) => {
                            tracing::error!("measure failed: {e}");
                            Shape::Drone
                        }
                    }
                }
                (PlayKind::Sound, cached) => cached,
                _ => Shape::Timed,
            };

            // A one-shot gets a duration so `ended()` fires (→ free / loop); a
            // Sequence/Arrangement loops natively via the engine.
            let opts = PlayOptions {
                looping: matches!(shape, Shape::Timed) && looping,
                duration_secs: if let Shape::OneShot(s) = shape {
                    Some(s)
                } else {
                    None
                },
                ..Default::default()
            };
            match player.play_document(&lib, target, opts) {
                Ok(pb) => *app.playback.borrow_mut() = Some(pb),
                Err(e) => return app.abort(Some(player), format!("play failed: {e}")),
            }
            player.set_master_gain(gain);
            app.shape.set_neq(shape);

            // Re-apply the user's tweaks to the freshly-built graph (audio) — every
            // play, including a loop re-fire, so the loop keeps your settings.
            for ((node, param), v) in app.live_values.borrow().iter() {
                player.set_param_live(*node, param, *v, 0.0);
            }

            // Build the control sliders ONCE per open. Rebuilding them on a Replay
            // / loop re-fire would recreate the <input> you're dragging and kill the
            // drag — so only build when there are none yet (a fresh sound). The
            // sliders persist across re-fires; their values feed the graph above.
            if app.controls.get_cloned().is_empty() {
                let mut ctls: Vec<ParamCtl> = Vec::new();
                for (node, params) in player.live_params() {
                    let kind_name = lib
                        .sample(target)
                        .and_then(|s| s.graph.node(node))
                        .map(|n| node_kind_name(&n.kind))
                        .unwrap_or("Node");
                    for param in params {
                        let (min, max, step) = param_spec(param);
                        let value = player
                            .param_value(node, param)
                            .map(|v| v as f64)
                            .unwrap_or((min + max) / 2.0)
                            .clamp(min, max);
                        ctls.push(ParamCtl {
                            node,
                            kind: kind_name,
                            param,
                            min,
                            max,
                            step,
                            value,
                        });
                    }
                }
                app.controls.set(Rc::new(ctls));
            }

            *app.player.borrow_mut() = Some(player);
            app.status.set(format!(
                "▶ {} · drag a slider to control it live",
                kind_label(kind)
            ));
            app.loading.set(false);
        });
    }

    /// Restore the player + clear the loading flag on a failed play.
    fn abort(&self, player: Option<Player>, msg: String) {
        if let Some(p) = player {
            *self.player.borrow_mut() = Some(p);
        }
        tracing::error!("{msg}");
        self.status.set(msg);
        self.loading.set(false);
    }

    fn stop(&self) {
        if let Some(p) = self.player.borrow_mut().as_mut() {
            p.stop();
        }
        *self.playback.borrow_mut() = None;
    }

    fn set_param(&self, node: NodeId, param: &'static str, value: f32) {
        // Remember it so a Replay / Loop re-play keeps the tweak.
        self.live_values.borrow_mut().insert((node, param), value);
        if let Some(p) = self.player.borrow().as_ref() {
            p.set_param_live(node, param, value, 0.02);
        }
    }

    /// Per-frame: re-arm a Sequence/Arrangement loop near its boundary (engine),
    /// or — when a one-shot's `ended()` fires — either re-fire it (host-driven
    /// loop) or stop and note that a game would free it here.
    fn tick(self: &Rc<Self>) {
        let now = match self.player.borrow().as_ref() {
            Some(p) => p.current_time(),
            None => return,
        };
        let ended = {
            let mut slot = self.playback.borrow_mut();
            let Some(pb) = slot.as_mut() else { return };
            if pb.ended(now) {
                true
            } else {
                if let Some(p) = self.player.borrow_mut().as_mut() {
                    let _ = p.loop_tick(pb, now);
                }
                false
            }
        };
        if ended {
            if self.looping.get() && matches!(self.shape.get(), Shape::OneShot(_)) {
                // Loop a one-shot by re-firing it — keeps your live tweaks.
                if let Some(idx) = self.open.get() {
                    self.play(idx);
                }
            } else {
                self.stop();
                self.status
                    .set("finished — a game would free its resources here".to_string());
            }
        }
    }
}

// ─────────────────────────────── fabricated demos ───────────────────────────

/// A single 440 Hz oscillator → gain Sound (frequency-control demo).
fn live_tone() -> (SampleLibrary, SampleId) {
    let osc = Node::new(NodeKind::Oscillator(OscillatorNode::default()));
    let osc_id = osc.id;
    let gain = Node::new(NodeKind::Gain(GainNode {
        gain: AudioParam::new(0.22),
    }));
    let gain_id = gain.id;

    let mut sound = Sample::new("Live tone");
    let target = sound.id;
    sound.graph.nodes.push(osc);
    sound.graph.nodes.push(gain);
    sound
        .graph
        .connections
        .push(Connection::node_to_node(osc_id, gain_id));

    let lib = SampleLibrary {
        root: Some(target),
        samples: vec![sound],
        ..Default::default()
    };
    (lib, target)
}

/// A 440 Hz tone bounced and tiled into an 8-step rhythm — exercises both
/// `register`'s bounce phase and arrangement clip playback.
fn arrangement_demo() -> (SampleLibrary, SampleId) {
    let mut tone = Sample::new("Tone (440 Hz)");
    let tone_id = tone.id;
    tone.graph
        .nodes
        .push(Node::new(NodeKind::Oscillator(OscillatorNode::default())));
    tone.bounce = Some(Bounce {
        asset: AssetId::new(),
        source_hash: 0,
    });

    let mut arr = Sample::new_arrangement("Arrangement demo");
    let arr_id = arr.id;
    arr.arrangement.bpm = 120.0;
    arr.arrangement.length_secs = 4.0;
    let clips = (0..8)
        .map(|i| Clip {
            start: i as f64 * 0.5,
            length: 0.25,
            source: tone_id,
            offset: 0.0,
            gain: 0.6,
            looping: false,
            speed: 1.0,
            name: String::new(),
        })
        .collect();
    arr.arrangement.tracks.push(ArrTrack {
        name: "Tone".to_string(),
        gain: 1.0,
        mute: false,
        solo: false,
        gain_automation: Vec::new(),
        clips,
    });

    let lib = SampleLibrary {
        root: Some(arr_id),
        samples: vec![tone, arr],
        ..Default::default()
    };
    (lib, arr_id)
}

// ─────────────────────────────── labels + ranges ────────────────────────────

fn kind_label(kind: PlayKind) -> &'static str {
    match kind {
        PlayKind::Sound => "Sound",
        PlayKind::Sequence => "Sequence",
        PlayKind::Arrangement => "Arrangement",
    }
}

fn kind_color(kind: PlayKind) -> &'static str {
    match kind {
        PlayKind::Sound => "#3aa0ff",
        PlayKind::Sequence => "#9d6bff",
        PlayKind::Arrangement => "#27c08a",
    }
}

fn node_kind_name(k: &NodeKind) -> &'static str {
    match k {
        NodeKind::Oscillator(_) => "Oscillator",
        NodeKind::Gain(_) => "Gain",
        NodeKind::BiquadFilter(_) => "Filter",
        NodeKind::Panner(_) => "Panner",
        NodeKind::SpatialOutput(_) => "Spatial Output",
        NodeKind::AudioBufferSource(_) => "Buffer",
        NodeKind::AudioWorklet(_) => "Worklet",
        _ => "Node",
    }
}

/// `(min, max, step)` for a param's slider — sensible musical ranges.
fn param_spec(name: &str) -> (f64, f64, f64) {
    match name {
        "frequency" => (20.0, 4000.0, 1.0),
        "detune" => (-1200.0, 1200.0, 1.0),
        "gain" => (0.0, 2.0, 0.01),
        "Q" => (0.0001, 24.0, 0.01),
        "positionX" | "positionY" | "positionZ" => (-12.0, 12.0, 0.1),
        "playbackRate" => (0.25, 4.0, 0.01),
        _ => (0.0, 1.0, 0.01),
    }
}

// ──────────────────────────────────── UI ────────────────────────────────────

const SURFACE: &str = "#1c2027";
const SURFACE_2: &str = "#242a33";
const BORDER: &str = "#2c333d";
const ACCENT: &str = "#5b9dff";
const TEXT: &str = "#e6e8ec";
const MUTED: &str = "#98a1ad";

/// Global base styles + hover states (`:hover` can't be inline).
fn install_styles() {
    stylesheet!("*", { .style("box-sizing", "border-box") });
    stylesheet!("body", {
        .style("font-family", "ui-sans-serif, system-ui, -apple-system, sans-serif")
        .style("background", "#14171c")
        .style("color", TEXT)
    });
    stylesheet!(".card", { .style("transition", "border-color .12s, transform .12s, background .12s") });
    stylesheet!(".card:hover", {
        .style("border-color", ACCENT)
        .style("background", SURFACE_2)
        .style("transform", "translateY(-1px)")
    });
    stylesheet!(".tbtn", { .style("transition", "background .12s, border-color .12s") });
    stylesheet!(".tbtn:hover", { .style("background", SURFACE_2).style("border-color", "#445063") });
    stylesheet!("input[type=range]", {
        .style("accent-color", ACCENT)
        .style("width", "100%")
        .style("height", "18px")
        .style("cursor", "pointer")
    });
    stylesheet!("input[type=checkbox]", { .style("accent-color", ACCENT).style("cursor", "pointer") });
}

fn view(app: &Rc<App>) -> Dom {
    html!("div", {
        .style("max-width", "760px")
        .style("margin", "0 auto")
        .style("padding", "40px 24px 80px")
        .child(html!("h1", {
            .style("margin", "0 0 6px")
            .style("font-size", "24px")
            .style("font-weight", "680")
            .style("letter-spacing", "-0.01em")
            .text("awsm-audio · player example")
        }))
        .child(html!("p", {
            .style("margin", "0 0 22px")
            .style("color", MUTED)
            .style("font-size", "14px")
            .style("line-height", "1.55")
            .style("max-width", "62ch")
            .text("The same engine the editor uses. Click a project to open its player: \
                   register() loads its assets (decode + compile + bounce, concurrently), \
                   play_document() plays it, and live_params() builds a slider for \
                   everything you can tweak on the fly.")
        }))
        .child(legend())
        .child(html!("div", {
            .style("display", "grid")
            .style("grid-template-columns", "repeat(auto-fill, minmax(190px, 1fr))")
            .style("gap", "10px")
            .style("margin-top", "20px")
            .children((0..app.projects.len()).map(clone!(app => move |i| card(&app, i))))
        }))
        // The per-sound player modal.
        .child(html!("div", {
            .child_signal(app.open.signal().map(clone!(app => move |o| o.map(|idx| modal(&app, idx)))))
        }))
    })
}

fn legend() -> Dom {
    let item = |kind: PlayKind, blurb: &'static str| {
        html!("div", {
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("gap", "8px")
            .child(badge(kind))
            .child(html!("span", { .style("font-size", "12.5px").style("color", MUTED).text(blurb) }))
        })
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "8px")
        .style("padding", "14px 16px")
        .style("background", SURFACE)
        .style("border", &format!("1px solid {BORDER}"))
        .style("border-radius", "10px")
        .child(item(PlayKind::Sound, "a plain patch, auditioned"))
        .child(item(PlayKind::Sequence, "a Note Sequencer driving instruments into an Output"))
        .child(item(PlayKind::Arrangement, "bounced audio clips on a timeline"))
    })
}

fn badge(kind: PlayKind) -> Dom {
    html!("span", {
        .style("font-size", "10px")
        .style("font-weight", "700")
        .style("letter-spacing", "0.02em")
        .style("text-transform", "uppercase")
        .style("padding", "2px 8px")
        .style("border-radius", "20px")
        .style("color", "#0e1116")
        .style("background", kind_color(kind))
        .style("white-space", "nowrap")
        .text(kind_label(kind))
    })
}

fn card(app: &Rc<App>, idx: usize) -> Dom {
    let proj = &app.projects[idx];
    let kind = proj.kind;
    html!("button", {
        .class("card")
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("justify-content", "space-between")
        .style("align-items", "flex-start")
        .style("gap", "12px")
        .style("min-height", "78px")
        .style("padding", "13px 14px")
        .style("cursor", "pointer")
        .style("border-radius", "11px")
        .style("border", &format!("1px solid {BORDER}"))
        .style("background", SURFACE)
        .style("color", "inherit")
        .style("text-align", "left")
        .style("font", "inherit")
        .child(html!("span", { .style("font-size", "14px").style("font-weight", "600").text(&proj.name) }))
        .child(badge(kind))
        .event(clone!(app => move |_: events::Click| app.open(idx)))
    })
}

fn modal(app: &Rc<App>, idx: usize) -> Dom {
    let proj = &app.projects[idx];
    let kind = proj.kind;
    let name = proj.name.clone();
    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        .style("z-index", "100")
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("padding", "20px")
        // Backdrop (click to close).
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("background", "rgba(8,10,14,0.66)")
            .style("backdrop-filter", "blur(2px)")
            .event(clone!(app => move |_: events::Click| app.close()))
        }))
        // Panel.
        .child(html!("div", {
            .style("position", "relative")
            .style("width", "min(480px, 100%)")
            .style("max-height", "86vh")
            .style("overflow-y", "auto")
            .style("background", SURFACE)
            .style("border", &format!("1px solid {BORDER}"))
            .style("border-radius", "14px")
            .style("padding", "20px 22px 22px")
            .style("box-shadow", "0 24px 80px rgba(0,0,0,0.55)")
            // Header.
            .child(html!("div", {
                .style("display", "flex")
                .style("align-items", "center")
                .style("justify-content", "space-between")
                .style("gap", "12px")
                .child(html!("div", {
                    .style("display", "flex").style("align-items", "center").style("gap", "10px").style("min-width", "0")
                    .child(html!("span", {
                        .style("font-size", "17px").style("font-weight", "660")
                        .style("overflow", "hidden").style("text-overflow", "ellipsis").style("white-space", "nowrap")
                        .text(&name)
                    }))
                    .child(badge(kind))
                }))
                .child(html!("button", {
                    .class("tbtn")
                    .style("flex", "0 0 auto")
                    .style("width", "28px").style("height", "28px")
                    .style("display", "inline-flex").style("align-items", "center").style("justify-content", "center")
                    .style("border", &format!("1px solid {BORDER}")).style("border-radius", "7px")
                    .style("background", "transparent").style("color", MUTED)
                    .style("font-size", "15px").style("cursor", "pointer")
                    .text("✕")
                    .event(clone!(app => move |_: events::Click| app.close()))
                }))
            }))
            .child(divider())
            // Transport: Replay + (shape-aware) Loop + a measured-shape readout.
            // Loop shows only where it means something: a one-shot (re-fired on
            // end) or timed content (engine re-arm) — never a free-running drone.
            .child(html!("div", {
                .style("display", "flex").style("align-items", "center").style("gap", "16px")
                .child(html!("button", {
                    .class("tbtn")
                    .style("padding", "6px 13px").style("cursor", "pointer").style("border-radius", "8px")
                    .style("border", &format!("1px solid {BORDER}")).style("background", SURFACE_2).style("color", "inherit")
                    .style("font", "inherit").style("font-size", "13px")
                    .text("↻ Replay")
                    .event(clone!(app => move |_: events::Click| app.play(idx)))
                }))
                .child(html!("div", {
                    .style("display", "flex").style("align-items", "center").style("gap", "14px")
                    .child_signal(app.shape.signal().map(clone!(app => move |shape| Some(html!("div", {
                        .style("display", "flex").style("align-items", "center").style("gap", "14px")
                        .apply(clone!(app => move |b| if shape.loopable() {
                            b.child(html!("label", {
                                .style("display", "inline-flex").style("align-items", "center").style("gap", "7px")
                                .style("cursor", "pointer").style("font-size", "13px").style("color", MUTED)
                                .child(html!("input" => web_sys::HtmlInputElement, {
                                    .attr("type", "checkbox")
                                    .apply(|b| if app.looping.get() { b.attr("checked", "") } else { b })
                                    .with_node!(el => {
                                        // Re-play with the new loop setting (tweaks preserved).
                                        .event(clone!(app => move |_: events::Change| {
                                            app.looping.set(el.checked());
                                            app.play(idx);
                                        }))
                                    })
                                }))
                                .text("Loop")
                            }))
                        } else { b }))
                        .apply(move |b| if matches!(shape, Shape::Unknown | Shape::Timed) {
                            b
                        } else {
                            b.child(html!("span", {
                                .style("font-size", "12px").style("color", MUTED)
                                .text(&shape.label())
                            }))
                        })
                    })))))
                }))
            }))
            // Master gain — always present.
            .child(section_label("Mix"))
            .child(slider_row(
                "Master gain", 0.0, 2.0, 0.01, app.gain.get(),
                clone!(app => move |v| {
                    app.gain.set(v);
                    if let Some(p) = app.player.borrow().as_ref() { p.set_master_gain(v as f32); }
                }),
            ))
            // Discovered per-param sliders (frequency / position / Q / …).
            .child(html!("div", {
                .child_signal(app.controls.signal_cloned().map(clone!(app => move |ctls| {
                    Some(if ctls.is_empty() {
                        html!("div", {
                            .style("margin-top", "12px").style("font-size", "12.5px").style("color", MUTED)
                            .text("No adjustable params on this sound.")
                        })
                    } else {
                        html!("div", {
                            .child(section_label("Live controls — drag while it plays"))
                            .children(ctls.iter().map(|c| param_slider(&app, c)))
                        })
                    })
                })))
            }))
            // Status.
            .child(html!("div", {
                .style("margin-top", "14px").style("font-size", "12px").style("color", MUTED)
                .style("min-height", "16px")
                .text_signal(app.status.signal_cloned())
            }))
        }))
    })
}

fn divider() -> Dom {
    html!("div", {
        .style("height", "1px")
        .style("background", BORDER)
        .style("margin", "14px -22px")
    })
}

fn section_label(text: &str) -> Dom {
    html!("div", {
        .style("font-size", "11px")
        .style("font-weight", "700")
        .style("letter-spacing", "0.04em")
        .style("text-transform", "uppercase")
        .style("color", MUTED)
        .style("margin", "16px 0 4px")
        .text(text)
    })
}

fn param_slider(app: &Rc<App>, c: &ParamCtl) -> Dom {
    let (node, param) = (c.node, c.param);
    let label = format!("{} · {}", c.kind, c.param);
    slider_row(
        &label,
        c.min,
        c.max,
        c.step,
        c.value,
        clone!(app => move |v| {
            app.set_param(node, param, v as f32);
            app.status.set(format!("{param} → {v:.2}"));
        }),
    )
}

/// A labeled `<input type=range>` row with a live value readout.
fn slider_row(
    label: &str,
    min: f64,
    max: f64,
    step: f64,
    value: f64,
    mut on_input: impl FnMut(f64) + 'static,
) -> Dom {
    let readout = Mutable::new(format!("{value:.2}"));
    html!("div", {
        .style("display", "grid")
        .style("grid-template-columns", "minmax(110px, 38%) 1fr 58px")
        .style("align-items", "center")
        .style("gap", "12px")
        .style("margin-top", "9px")
        .child(html!("span", {
            .style("font-size", "12.5px").style("color", "#c3cad3")
            .style("overflow", "hidden").style("text-overflow", "ellipsis").style("white-space", "nowrap")
            .text(label)
        }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "range")
            .attr("min", &min.to_string())
            .attr("max", &max.to_string())
            .attr("step", &step.to_string())
            .attr("value", &value.to_string())
            .with_node!(el => {
                .event(clone!(readout => move |_: events::Input| {
                    if let Ok(v) = el.value().parse::<f64>() {
                        readout.set(format!("{v:.2}"));
                        on_input(v);
                    }
                }))
            })
        }))
        .child(html!("span", {
            .style("font-size", "11.5px").style("color", MUTED).style("text-align", "right")
            .style("font-variant-numeric", "tabular-nums")
            .text_signal(readout.signal_cloned())
        }))
    })
}

// ─────────────────────────────── rAF + boot ─────────────────────────────────

/// Keeps the self-rescheduling rAF closure alive.
type RafHolder = Rc<RefCell<Option<Closure<dyn FnMut()>>>>;

/// Drive [`App::tick`] every animation frame (the host-side loop/auto-stop pump).
fn start_raf(app: Rc<App>) {
    let holder: RafHolder = Rc::new(RefCell::new(None));
    let holder2 = holder.clone();
    *holder.borrow_mut() = Some(Closure::wrap(Box::new(move || {
        app.tick();
        request_frame(holder2.borrow().as_ref().unwrap());
    }) as Box<dyn FnMut()>));
    request_frame(holder.borrow().as_ref().unwrap());
}

fn request_frame(cb: &Closure<dyn FnMut()>) {
    let _ = web_sys::window()
        .expect("window")
        .request_animation_frame(cb.as_ref().unchecked_ref());
}

fn main() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    install_styles();
    let app = App::new();
    start_raf(app.clone());
    dominator::append_dom(&dominator::body(), view(&app));
}

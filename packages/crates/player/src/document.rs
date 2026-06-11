//! Play a whole [`SampleLibrary`] document — the same engine the editor drives,
//! exposed so any application can load a saved project and play it.
//!
//! A project saved from the awsm-audio editor is a [`SampleLibrary`]: a set of
//! [`Sample`](awsm_audio_schema::Sample)s (node-graph **Sounds** and DAW-style
//! **Arrangements**) plus a shared asset table (WASM DSP modules + audio buffers).
//! This module turns that document into sound with three steps:
//!
//! 1. **Load** the document (parse the `.toml`/`.json` with `awsm-audio-schema`).
//! 2. [`Player::register`] — prepare *every* dependency concurrently: decode audio,
//!    compile WASM, and bounce Sounds, all in flight at once.
//! 3. [`Player::play_document`] — play anything on demand: a plain Sound, a
//!    sequencer-driven sequence, or an arrangement, with the same control the editor
//!    has (`set_param_live`, `set_master_gain`, looping, scrub, a forced duration).
//!
//! ```no_run
//! # use awsm_audio_player::{Player, document::PlayOptions};
//! # use awsm_audio_schema::{SampleLibrary, SampleId};
//! // `lib` is parsed from a saved `.toml`/`.json` with `awsm-audio-schema`.
//! # async fn demo(lib: &SampleLibrary, target: SampleId) -> anyhow::Result<()> {
//! let mut player = Player::new()?;
//! player.register(lib).await?;                  // decode + compile + bounce, concurrently
//! let _playback = player.play_document(lib, target, PlayOptions::default())?;
//! # Ok(()) }
//! ```
//!
//! ## Live control — drive a playing sound from application state
//!
//! Once a sound is playing, you can steer any of its automatable params *without*
//! restarting it — the use case for game/app audio: a prewired sound routed to a
//! [`SpatialOutput`](awsm_audio_schema::NodeKind::SpatialOutput) whose position you
//! move from an entity's transform, an oscillator whose pitch you bend from a
//! gauge, a filter you open as something charges. Use
//! [`Player::set_param_live`] with the param's [`NodeId`](awsm_audio_schema::NodeId)
//! (read from the document's graph) — it glides smoothly and never rebuilds the graph, so the sound keeps
//! ringing:
//!
//! ```no_run
//! # use awsm_audio_player::{Player, document::PlayOptions};
//! # use awsm_audio_schema::{NodeKind, SampleLibrary, SampleId};
//! # async fn demo(lib: &SampleLibrary, target: SampleId) -> anyhow::Result<()> {
//! let mut player = Player::new()?;
//! player.register(lib).await?;
//! player.play_document(lib, target, PlayOptions { looping: true, ..Default::default() })?;
//!
//! // Find the nodes you want to drive (ids are stable in the document).
//! let graph = &lib.sample(target).unwrap().graph;
//! let spatial = graph.nodes.iter().find(|n| matches!(n.kind, NodeKind::SpatialOutput(_)));
//! let osc = graph.nodes.iter().find(|n| matches!(n.kind, NodeKind::Oscillator(_)));
//!
//! // …then, every frame / on every game event:
//! if let Some(s) = spatial {
//!     player.set_param_live(s.id, "positionX", /* entity.x */ 2.5, 0.05);
//!     player.set_param_live(s.id, "positionZ", /* entity.z */ -1.0, 0.05);
//! }
//! if let Some(o) = osc {
//!     player.set_param_live(o.id, "frequency", /* 220 + gun_power */ 880.0, 0.02);
//! }
//! # Ok(()) }
//! ```
//!
//! Not sure what's adjustable? [`Player::live_params`] lists every controllable
//! `(node, param)` in the playing graph — no document spelunking needed. See
//! [`Player::set_param_live`] for the full param list per node kind. (For the
//! *listener* — the ears — set [`Player::set_listener`].) `Player::set_master_gain`
//! rides the whole mix live.
//!
//! ## One-shot vs drone — when does a Sound end?
//!
//! A node-graph Sound either **decays to silence on its own** (a one-shot — a
//! drum hit, a gunshot, a plucked note) or **runs until you stop it** (a drone /
//! pad / engine loop). Which one it is isn't always obvious from the graph: an
//! oscillator runs forever, but an amplitude envelope can bring it to silence. The
//! reliable test is to render it and see if it goes quiet — [`Player::measure_sound`]
//! does exactly that and returns a [`SoundShape`].
//!
//! That distinction is what a game needs to **free resources when a sound
//! finishes** and to know what's loopable:
//!
//! ```no_run
//! # use awsm_audio_player::{Player, document::{PlayOptions, SoundShape}};
//! # use awsm_audio_schema::{SampleLibrary, SampleId};
//! # async fn demo(player: &mut Player, lib: &SampleLibrary, target: SampleId) -> anyhow::Result<()> {
//! player.register(lib).await?;
//! let shape = player.measure_sound(lib, target, 20.0).await?;   // measure once, at load
//! let mut pb = player.play_document(lib, target,
//!     PlayOptions { duration_secs: shape.secs(), ..Default::default() })?;
//!
//! // each frame:
//! let now = player.current_time();
//! if pb.ended(now) {
//!     // one-shot finished → free it, OR re-play to loop:
//!     // pb = player.play_document(lib, target, PlayOptions { duration_secs: shape.secs(), ..Default::default() })?;
//! }
//! # let _ = &mut pb; Ok(()) }
//! ```
//!
//! So yes: once a Sound is known to be a one-shot, you loop it by re-firing it
//! when [`Playback::ended`] reports done (the same host-driven model as
//! [`Player::loop_tick`] for sequences/arrangements). A drone has no end, so you
//! play it open-ended and stop it explicitly.
//!
//! The assembly functions ([`sequence_parts`], [`audio_clip_parts`], [`classify`],
//! [`is_sequence`]) are pure over `awsm-audio-schema` types and public, so the editor
//! and a standalone app share *exactly* the same code path.

use anyhow::Result;
use base64::Engine as _;
use futures::future::FutureExt;
use futures::stream::{FuturesUnordered, StreamExt};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use awsm_audio_schema::{
    AssetId, AudioSource, ConnectionSink, ConnectionSource, Graph, NodeKind, SampleId, SampleKind,
    SampleLibrary, WasmSource,
};

use crate::{bounce, AudioClipPart, ControlLanePart, Player, SongVoiceSpec, TriggerPart};

/// Extra render time past a sequence loop length so note releases / reverb tails can
/// ring out and be folded back across the loop seam when bouncing (mirrors the
/// editor's bounce).
const RELEASE_TAIL: f64 = 3.0;

/// Default render length (seconds) for a one-shot Sound with no natural end.
const DEFAULT_SOUND_SECS: f64 = 6.0;

/// What playing a sample means — chosen from the document, not from editor state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayKind {
    /// A plain node graph (no sequencer / Output): auditioned to the speakers.
    Sound,
    /// A node graph that *performs* — a Note Sequencer drives instrument voices
    /// and/or a Control Sequencer automates params, mixed into an Output.
    Sequence,
    /// A DAW-style timeline of bounced audio clips.
    Arrangement,
}

/// Whether a Sound ends on its own — measured by [`Player::measure_sound`].
///
/// A node graph's "endedness" is a property of its sources *and* envelopes: a
/// non-looping buffer ends; an oscillator / noise / looping buffer runs forever
/// **unless** an amplitude envelope decays the output to silence (the common
/// "one-shot synth": a noise burst or a plucked tone). The reliable way to know
/// is to render it and see if it goes quiet — which is exactly what this reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SoundShape {
    /// Decays to silence on its own after `secs`. A **one-shot**: pass `secs` as
    /// `PlayOptions::duration_secs` so [`Playback::ended`] fires when it finishes —
    /// then free its resources, or re-play to loop it.
    OneShot { secs: f64 },
    /// Still going at the measurement window's end — a **drone** / sustaining pad /
    /// looping source. It won't stop on its own; play it open-ended and stop it
    /// explicitly.
    Sustaining,
}

impl SoundShape {
    /// The one-shot length in seconds, or `None` for a sustaining sound. Handy as
    /// `PlayOptions { duration_secs: shape.secs(), .. }`.
    pub fn secs(self) -> Option<f64> {
        match self {
            SoundShape::OneShot { secs } => Some(secs),
            SoundShape::Sustaining => None,
        }
    }
}

/// How to play a document target. `Default` is: play once, no scrub, natural
/// length.
#[derive(Debug, Clone, Copy)]
pub struct PlayOptions {
    /// Repeat the content (sequences/arrangements re-arm at the loop boundary; a Sound
    /// graph loops its sources).
    pub looping: bool,
    /// Force a fixed length (seconds) for a source that has no natural end — a
    /// drone oscillator, a procedural worklet. `None` uses the natural length.
    pub duration_secs: Option<f64>,
    /// Start an arrangement this many seconds into the timeline (the editor's
    /// scrub). Ignored for Sounds/sequences.
    pub seek_secs: f64,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            looping: false,
            duration_secs: None,
            seek_secs: 0.0,
        }
    }
}

/// A handle to an in-progress document playback. Hold it to drive looping (the
/// host re-arms the next pass at the loop boundary — see [`Player::loop_tick`])
/// and to know when non-looping content ends (see [`Playback::ended`]).
///
/// Looping is **host-driven by default** (no internal timers): call
/// [`Player::loop_tick`] from your animation frame / interval. The pieces you'd
/// need to roll your own (the loop length and the next boundary) are exposed via
/// [`Playback::content_secs`] and [`Playback::next_loop_at`].
pub struct Playback {
    /// What kind of content this is.
    pub kind: PlayKind,
    /// Whether it repeats.
    pub looping: bool,
    /// Context time (seconds) playback began.
    pub started_at: f64,
    /// Natural content length in seconds (loop window / last-note end /
    /// arrangement length / the forced `duration_secs`). `None` for an open-ended
    /// Sound left to run until stopped.
    content_secs: Option<f64>,
    /// Context time of the next loop boundary; `INFINITY` when not looping.
    next_at: f64,
    /// Assembled sequence parts, kept so a loop pass re-schedules without re-deriving.
    sequence: Option<SequenceParts>,
    /// Assembled arrangement clips, kept for the same reason.
    clips: Vec<AudioClipPart>,
}

impl Playback {
    /// The natural content length in seconds, if the content has a defined end.
    pub fn content_secs(&self) -> Option<f64> {
        self.content_secs
    }

    /// Context time of the next loop boundary (`INFINITY` when not looping).
    pub fn next_loop_at(&self) -> f64 {
        self.next_at
    }

    /// Whether non-looping content has finished by context time `now` (a small
    /// tail is allowed for releases). Always `false` while looping or open-ended.
    ///
    /// This is the **resource-freeing signal**: poll it (e.g. each frame) and when
    /// it flips true the sound is done — drop the [`Playback`] (and the [`Player`]
    /// if it was a one-off voice) to reclaim its nodes, or re-call
    /// [`play_document`](Player::play_document) to loop it. A one-shot Sound only
    /// reports `ended` once it has a `content_secs` — set it from
    /// [`measure_sound`](Player::measure_sound); a drone (no `content_secs`) never
    /// ends on its own.
    pub fn ended(&self, now: f64) -> bool {
        if self.looping {
            return false;
        }
        match self.content_secs {
            Some(secs) => now >= self.started_at + secs + 0.25,
            None => false,
        }
    }
}

/// Decide what playing `target` means, purely from the document.
pub fn classify(lib: &SampleLibrary, target: SampleId) -> PlayKind {
    match lib.sample(target) {
        Some(s) if s.kind == SampleKind::Arrangement => PlayKind::Arrangement,
        Some(s) if is_sequence(&s.graph) => PlayKind::Sequence,
        _ => PlayKind::Sound,
    }
}

/// Whether a graph *performs* (a sequence) rather than just auditions: it has a note /
/// control trigger wire, or a wired Output marking a finished mix.
pub fn is_sequence(graph: &Graph) -> bool {
    graph
        .connections
        .iter()
        .any(|c| matches!(c.to, ConnectionSink::Trigger { .. }))
        || graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Output(_) | NodeKind::SpatialOutput(_)))
}

/// The schedulable pieces of a sequencer-driven sequence, derived from the document.
pub struct SequenceParts {
    /// The sample's own graph (instrument-refs kept as voice buses, NOT flattened).
    pub graph: Graph,
    /// One [`TriggerPart`] per note-sequencer → instrument trigger wire.
    pub triggers: Vec<TriggerPart>,
    /// One [`ControlLanePart`] per control-sequencer → param wire.
    pub control: Vec<ControlLanePart>,
    /// Loop length in seconds: the explicit playback window if any sequencer sets
    /// one, else the last note's end. `0.0` if there's nothing to play.
    pub loop_secs: f64,
}

/// Resolve a sequence Sound (`id`) into its schedulable trigger + control parts.
///
/// Walks the sample's graph: every `SeqOut → Trigger` wire becomes a
/// [`TriggerPart`] (the sequencer's notes resolved to seconds/transpose/velocity,
/// targeting the flattened instrument the wire fires); every `SeqOut → Param` wire
/// becomes a [`ControlLanePart`]. This is the exact assembly the editor's live
/// transport and offline bounce both use.
pub fn sequence_parts(lib: &SampleLibrary, id: SampleId) -> SequenceParts {
    let Some(sample) = lib.sample(id) else {
        return SequenceParts {
            graph: Graph::default(),
            triggers: Vec::new(),
            control: Vec::new(),
            loop_secs: 0.0,
        };
    };
    let g = sample.graph.clone();
    let mut triggers = Vec::new();
    let mut control = Vec::new();
    // Loop length = the longest explicit playback window, else the content end
    // (last note). Exactly what the live transport loops over, so a loop-bounce of
    // this length repeats bit-for-bit.
    let mut content_end = 0.0f64;
    let mut window_secs = 0.0f64;
    for c in &g.connections {
        match (&c.from, &c.to) {
            // Note sequencer → trigger an instrument-ref voice bus.
            (ConnectionSource::SeqOut { node: seqn, key }, ConnectionSink::Trigger { node }) => {
                let Some(NodeKind::NoteSequencer(ms)) = g.node(*seqn).map(|n| &n.kind) else {
                    continue;
                };
                let Some(outp) = ms.outputs.iter().find(|o| &o.key == key) else {
                    continue;
                };
                let Some(track) = ms.song.tracks.get(outp.track) else {
                    continue;
                };
                let Some(NodeKind::Sample(sref)) = g.node(*node).map(|n| &n.kind) else {
                    continue;
                };
                let instrument = awsm_audio_schema::flatten(lib, sref.sample);
                if instrument.nodes.is_empty() {
                    continue;
                }
                let base = ms.song.beats_to_secs(ms.start);
                let mut notes = Vec::new();
                for ev in &track.events {
                    if let Some(n) = outp.note {
                        if ev.note != n {
                            continue;
                        }
                    }
                    if ev.start < ms.start {
                        continue;
                    }
                    if let Some(end) = ms.end {
                        if ev.start >= end {
                            continue;
                        }
                    }
                    let semitones = if outp.note.is_some() {
                        outp.transpose
                    } else {
                        ev.note as i32 - 60 + outp.transpose
                    };
                    let end = ms.song.beats_to_secs(ev.start + ev.length) - base;
                    content_end = content_end.max(end);
                    notes.push(SongVoiceSpec {
                        start: ms.song.beats_to_secs(ev.start) - base,
                        end,
                        semitones,
                        velocity: ((ev.velocity as f32 / 127.0) * outp.gain).clamp(0.0, 1.0),
                    });
                }
                // Loop window = explicit stop, else the authored song length (bars).
                // Keeps a bounce's loop period identical to the live transport's.
                let win_beats = ms.end.or((ms.length > 0.0).then_some(ms.length));
                if let Some(win_end) = win_beats {
                    let win = ms.song.beats_to_secs(win_end) - base;
                    window_secs = window_secs.max(win.max(0.05));
                }
                if !notes.is_empty() {
                    triggers.push(TriggerPart {
                        target: *node,
                        instrument,
                        notes,
                    });
                }
            }
            // Control sequencer → automate a node param.
            (
                ConnectionSource::SeqOut { node: seqn, key },
                ConnectionSink::NodeParam { node, param },
            ) => {
                let Some(NodeKind::ControlSequencer(cs)) = g.node(*seqn).map(|n| &n.kind) else {
                    continue;
                };
                let Some(lane) = cs.lanes.iter().find(|l| &l.key == key) else {
                    continue;
                };
                let bpm = if cs.bpm > 0.0 { cs.bpm } else { 120.0 };
                let points = lane
                    .points
                    .iter()
                    .map(|p| (p.beat * 60.0 / bpm, p.value, p.curve))
                    .collect();
                control.push(ControlLanePart {
                    target: *node,
                    param: param.0.clone(),
                    points,
                });
            }
            _ => {}
        }
    }
    let loop_secs = if window_secs > 0.0 {
        window_secs
    } else {
        content_end
    };
    SequenceParts {
        graph: g,
        triggers,
        control,
        loop_secs,
    }
}

/// Resolve an Arrangement sample (`id`) into schedulable audio clips, seek-adjusted
/// so each clip's audio is relative to `seek` seconds on the timeline. Honors track
/// mute / solo (solo is exclusive) and resolves each clip's source Sound to its
/// bounced buffer. Returns empty if `id` isn't an Arrangement.
pub fn audio_clip_parts(lib: &SampleLibrary, id: SampleId, seek: f64) -> Vec<AudioClipPart> {
    let Some(sample) = lib.sample(id) else {
        return Vec::new();
    };
    if sample.kind != SampleKind::Arrangement {
        return Vec::new();
    }
    let arr = &sample.arrangement;
    let seek = seek.max(0.0);
    let any_solo = arr.tracks.iter().any(|t| t.solo);
    let mut out = Vec::new();
    for track in &arr.tracks {
        if track.mute || (any_solo && !track.solo) {
            continue;
        }
        for clip in &track.clips {
            let Some(buffer) = lib
                .sample(clip.source)
                .and_then(|s| s.bounce.as_ref().map(|b| b.asset))
            else {
                continue; // source not bounced
            };
            let clip_end = clip.start + clip.length;
            if clip_end <= seek {
                continue; // entirely before the scrub point
            }
            let lead = (seek - clip.start).max(0.0); // part of the clip before seek
            let speed = if clip.speed > 0.0 {
                clip.speed as f64
            } else {
                1.0
            };
            out.push(AudioClipPart {
                buffer,
                start: (clip.start - seek).max(0.0),
                // Seeking `lead` timeline seconds advances the buffer by `lead*speed`.
                offset: clip.offset + lead * speed,
                length: (clip.length - lead).max(0.0),
                gain: clip.gain * track.gain,
                looping: clip.looping,
                speed,
            });
        }
    }
    out
}

/// One asset finished loading (decoded buffer or compiled module).
enum Loaded {
    Buffer(AssetId, web_sys::AudioBuffer),
    Module(AssetId, js_sys::WebAssembly::Module),
}

impl Player {
    /// Prepare **every** dependency a document needs, with maximum concurrency.
    ///
    /// Two `FuturesUnordered` pools, run back-to-back (the second depends on the
    /// first):
    /// 1. Decode every inline/encoded audio buffer (`decodeAudioData`) **and**
    ///    compile every WASM module (`WebAssembly.compile`) at once; raw-PCM
    ///    buffers store synchronously.
    /// 2. Bounce every Sound whose bounced buffer isn't already present — each an
    ///    independent `OfflineAudioContext` render, all in flight together — so
    ///    arrangements have audio for their clips.
    ///
    /// Results are stored into the player as each future resolves, so a later
    /// [`play_document`](Self::play_document) is synchronous. Idempotent: assets
    /// already registered are skipped, so it's cheap to call again after edits.
    ///
    /// `Url`/`Path` asset sources are **not** fetched here (they need a network /
    /// filesystem the player doesn't reach); rehydrate them to inline
    /// `Encoded`/`Base64`/`Pcm` before calling — the editor's loader already does.
    pub async fn register(&mut self, lib: &SampleLibrary) -> Result<()> {
        // ── Phase 0: load the generic worklet shim so AudioWorklet nodes can be
        // constructed (a worklet sound otherwise throws at play time). Best-effort.
        if !self.worklet_ready {
            if let Ok(promise) = self.add_worklet_shim() {
                let _ = JsFuture::from(promise).await;
                self.mark_worklet_ready();
            }
        }

        // ── Phase 1: decode audio + compile WASM concurrently ────────────────
        let b64 = base64::engine::general_purpose::STANDARD;
        let ctx = self.ctx.clone();
        let mut pool = FuturesUnordered::new();

        for asset in &lib.assets.buffers {
            if self.has_buffer(asset.id) {
                continue;
            }
            match &asset.source {
                AudioSource::Pcm {
                    sample_rate,
                    channels,
                } => {
                    // Synchronous — no decode needed.
                    self.store_pcm(asset.id, *sample_rate, channels)?;
                }
                AudioSource::Encoded(data) => {
                    let id = asset.id;
                    let ctx = ctx.clone();
                    let bytes = b64.decode(data)?;
                    pool.push(
                        async move {
                            let buf = decode_audio(&ctx, &bytes).await?;
                            Ok::<_, anyhow::Error>(Loaded::Buffer(id, buf))
                        }
                        .boxed_local(),
                    );
                }
                AudioSource::Url(_) | AudioSource::Path(_) => {
                    tracing::warn!(
                        "register: skipping non-inline audio asset {} (Url/Path — \
                         rehydrate to inline bytes first)",
                        asset.id
                    );
                }
            }
        }

        for asset in &lib.assets.wasm_modules {
            if self.has_module(&asset.id) {
                continue;
            }
            match &asset.source {
                WasmSource::Base64(data) => {
                    let id = asset.id;
                    let bytes = b64.decode(data)?;
                    pool.push(
                        async move {
                            let module = compile_wasm(&bytes).await?;
                            Ok::<_, anyhow::Error>(Loaded::Module(id, module))
                        }
                        .boxed_local(),
                    );
                }
                WasmSource::Url(_) | WasmSource::Path(_) => {
                    tracing::warn!(
                        "register: skipping non-inline wasm asset {} (Url/Path — \
                         rehydrate to inline bytes first)",
                        asset.id
                    );
                }
            }
        }

        while let Some(loaded) = pool.next().await {
            match loaded? {
                Loaded::Buffer(id, buf) => self.store_buffer(id, buf),
                Loaded::Module(id, m) => self.store_module(id, m),
            }
        }

        // ── Phase 2: bounce sounds whose buffer isn't present yet ────────────
        let mut bounces = FuturesUnordered::new();
        for sample in &lib.samples {
            if sample.kind != SampleKind::Sound {
                continue;
            }
            let Some(b) = &sample.bounce else { continue };
            if self.has_buffer(b.asset) {
                continue; // embedded / already decoded in phase 1
            }
            let Some(job) = self.bounce_job_for_document(lib, sample.id) else {
                continue;
            };
            let asset = b.asset;
            bounces.push(async move { (asset, bounce::render(job).await) });
        }
        while let Some((asset, result)) = bounces.next().await {
            match result {
                Ok((channels, sr)) => self.store_pcm(asset, sr as f32, &channels)?,
                Err(e) => tracing::error!("register: bounce of asset {asset} failed: {e}"),
            }
        }
        Ok(())
    }

    /// Build a [`BounceJob`](bounce::BounceJob) for Sound `id` from the document —
    /// the same shape the editor's `bounce_job_for` produces. `None` if the sample
    /// isn't a bounceable Sound.
    fn bounce_job_for_document(
        &self,
        lib: &SampleLibrary,
        id: SampleId,
    ) -> Option<bounce::BounceJob> {
        let sample = lib.sample(id)?;
        if sample.kind != SampleKind::Sound {
            return None;
        }
        let (graph, parts, control, duration, loop_secs) = if is_sequence(&sample.graph) {
            let sp = sequence_parts(lib, id);
            let loop_len = sp.loop_secs.max(0.05);
            (
                sp.graph,
                sp.triggers,
                sp.control,
                loop_len + RELEASE_TAIL,
                Some(loop_len),
            )
        } else {
            (
                awsm_audio_schema::flatten(lib, id),
                Vec::new(),
                Vec::new(),
                DEFAULT_SOUND_SECS,
                None,
            )
        };
        Some(self.bounce_job(graph, parts, control, duration, loop_secs))
    }

    /// Measure a **Sound's** natural shape: does it end on its own (a one-shot) or
    /// run forever (a drone)? Renders the sound offline (faster than realtime) for
    /// up to `max_secs` and trims trailing silence — if it came back shorter than
    /// the window, it decayed by itself.
    ///
    /// This is what lets a game decide whether a sound's resources can be freed
    /// when it finishes, and whether it's loopable. Assets must be
    /// [`register`](Self::register)ed first. `max_secs` bounds the render and
    /// classifies anything still sounding at the end as [`Sustaining`](SoundShape::Sustaining)
    /// (pick it a bit longer than your longest expected one-shot — e.g. 20–30 s).
    ///
    /// ```no_run
    /// # use awsm_audio_player::{Player, document::{PlayOptions, SoundShape}};
    /// # use awsm_audio_schema::{SampleLibrary, SampleId};
    /// # async fn demo(player: &mut Player, lib: &SampleLibrary, target: SampleId) -> anyhow::Result<()> {
    /// player.register(lib).await?;
    /// let shape = player.measure_sound(lib, target, 20.0).await?;
    /// // One-shot → ended() will fire; drone → play it open-ended.
    /// let pb = player.play_document(lib, target, PlayOptions { duration_secs: shape.secs(), ..Default::default() })?;
    /// // later, per frame: if pb.ended(player.current_time()) { /* free, or re-play to loop */ }
    /// # let _ = pb; Ok(()) }
    /// ```
    pub async fn measure_sound(
        &self,
        lib: &SampleLibrary,
        target: SampleId,
        max_secs: f64,
    ) -> Result<SoundShape> {
        let window = max_secs.max(0.2);
        let graph = awsm_audio_schema::flatten(lib, target);
        let job = self.bounce_job(graph, Vec::new(), Vec::new(), window, None);
        let (channels, sr) = bounce::render(job).await?;
        let frames = channels.iter().map(Vec::len).max().unwrap_or(0);
        let secs = frames as f64 / (sr.max(1) as f64);
        // `render` trims trailing silence; a buffer shorter than the window means
        // the sound decayed on its own → one-shot.
        Ok(if secs + 0.05 < window {
            SoundShape::OneShot { secs }
        } else {
            SoundShape::Sustaining
        })
    }

    /// Play a document target — a Sound, a sequencer Sequence, or an Arrangement —
    /// with one call. Assets must already be [`register`](Self::register)ed.
    ///
    /// Returns a [`Playback`] handle: keep it to drive looping
    /// ([`loop_tick`](Self::loop_tick)) and to detect the end of non-looping
    /// content ([`Playback::ended`]). Live control is unchanged —
    /// [`set_param_live`](Self::set_param_live), [`set_master_gain`](Self::set_master_gain).
    pub fn play_document(
        &mut self,
        lib: &SampleLibrary,
        target: SampleId,
        opts: PlayOptions,
    ) -> Result<Playback> {
        self.set_master_gain(1.0);
        let kind = classify(lib, target);
        match kind {
            PlayKind::Sound => {
                // A Sound is a free-running patch — it can't loop *seamlessly* in
                // the engine, so it plays one pass and looping is host-driven: set
                // `duration_secs` (from `measure_sound`) and watch `ended()`, then
                // free it or re-call `play_document` to loop.
                let graph = awsm_audio_schema::flatten(lib, target);
                self.play(&graph, false)?;
                Ok(Playback {
                    kind,
                    looping: false,
                    started_at: self.current_time(),
                    content_secs: opts.duration_secs,
                    next_at: f64::INFINITY,
                    sequence: None,
                    clips: Vec::new(),
                })
            }
            PlayKind::Sequence => {
                let sp = sequence_parts(lib, target);
                self.play_arrangement(&sp.graph, opts.looping)?;
                let at = self.current_time() + 0.1;
                self.schedule_triggers(&sp.triggers, at)?;
                self.schedule_control(&sp.control, at);
                let natural = (sp.loop_secs > 0.0).then(|| sp.loop_secs.max(0.05));
                let content_secs = opts.duration_secs.or(natural);
                let next_at = match (opts.looping, content_secs) {
                    (true, Some(secs)) => at + secs,
                    _ => f64::INFINITY,
                };
                Ok(Playback {
                    kind,
                    looping: opts.looping,
                    started_at: at,
                    content_secs,
                    next_at,
                    sequence: Some(sp),
                    clips: Vec::new(),
                })
            }
            PlayKind::Arrangement => {
                let seek = opts.seek_secs.max(0.0);
                let clips = audio_clip_parts(lib, target, seek);
                self.arrange_audio_begin();
                let at = self.current_time() + 0.1;
                self.schedule_audio_clips(&clips, at)?;
                let natural = lib
                    .sample(target)
                    .map(|s| (s.arrangement.length_secs - seek).max(0.1));
                let content_secs = opts.duration_secs.or(natural);
                let next_at = match (opts.looping, content_secs) {
                    (true, Some(secs)) => at + secs,
                    _ => f64::INFINITY,
                };
                Ok(Playback {
                    kind,
                    looping: opts.looping,
                    started_at: at,
                    content_secs,
                    next_at,
                    sequence: None,
                    clips,
                })
            }
        }
    }

    /// Re-arm a looping playback's next pass when its boundary is near. Call this
    /// periodically (e.g. from `requestAnimationFrame`) with the current context
    /// time ([`current_time`](Self::current_time)); it schedules the next loop of
    /// notes / clips a moment before the previous one ends and advances the handle.
    /// A no-op for non-looping or open-ended playback.
    ///
    /// This is the simple, default way to loop — no internal timers, no `Rc`. (If
    /// you'd rather drive it yourself, [`Playback::next_loop_at`] and
    /// [`Playback::content_secs`] expose everything you need.)
    pub fn loop_tick(&mut self, pb: &mut Playback, now: f64) -> Result<()> {
        if !pb.looping || !pb.next_at.is_finite() {
            return Ok(());
        }
        // Schedule the next pass ~0.25 s before it's due.
        if now < pb.next_at - 0.25 {
            return Ok(());
        }
        let start = pb.next_at;
        match pb.kind {
            PlayKind::Sequence => {
                if let Some(sp) = &pb.sequence {
                    self.schedule_triggers(&sp.triggers, start)?;
                    self.schedule_control(&sp.control, start);
                }
            }
            PlayKind::Arrangement => {
                self.schedule_audio_clips(&pb.clips, start)?;
            }
            // A Sound loops via its native source loop; nothing to re-arm.
            PlayKind::Sound => return Ok(()),
        }
        if let Some(secs) = pb.content_secs {
            pb.started_at = start;
            pb.next_at = start + secs;
        }
        Ok(())
    }
}

/// Decode encoded audio bytes (mp3/wav/flac/…) to an `AudioBuffer` on `ctx`.
async fn decode_audio(ctx: &web_sys::AudioContext, bytes: &[u8]) -> Result<web_sys::AudioBuffer> {
    // `decodeAudioData` detaches the ArrayBuffer, so hand it a fresh copy.
    let array = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    array.copy_from(bytes);
    let promise = ctx
        .decode_audio_data(&array.buffer())
        .map_err(|e| anyhow::anyhow!("decodeAudioData: {e:?}"))?;
    let value = JsFuture::from(promise)
        .await
        .map_err(|e| anyhow::anyhow!("decode await: {e:?}"))?;
    value
        .dyn_into::<web_sys::AudioBuffer>()
        .map_err(|_| anyhow::anyhow!("decodeAudioData did not return an AudioBuffer"))
}

/// Compile `.wasm` bytes to a `WebAssembly.Module`.
async fn compile_wasm(bytes: &[u8]) -> Result<js_sys::WebAssembly::Module> {
    let array = js_sys::Uint8Array::new_with_length(bytes.len() as u32);
    array.copy_from(bytes);
    let value = JsFuture::from(js_sys::WebAssembly::compile(&array))
        .await
        .map_err(|e| anyhow::anyhow!("WebAssembly.compile: {e:?}"))?;
    value
        .dyn_into::<js_sys::WebAssembly::Module>()
        .map_err(|_| anyhow::anyhow!("WebAssembly.compile did not return a Module"))
}

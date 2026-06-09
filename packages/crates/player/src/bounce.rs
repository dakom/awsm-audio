//! Offline rendering — "bounce" a Sound's playable graph (plus its scheduled note
//! voices and control automation) to PCM via an `OfflineAudioContext`. Runs faster
//! than realtime and is deterministic, so the editor can freeze a Sound into an
//! audio clip.

use std::collections::HashMap;

use anyhow::Result;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AudioBuffer, AudioNode, AudioScheduledSourceNode, BaseAudioContext, OfflineAudioContext,
};

use awsm_audio_schema::{AssetId, Graph};

use crate::{
    apply_control, build, spawn_voices, AudioClipPart, ControlLanePart, TriggerPart, Voice,
};

/// Inputs for an offline bounce, gathered from the live [`Player`](crate::Player)
/// up front so the render future owns everything (no borrow across `await`).
pub struct BounceJob {
    pub graph: Graph,
    pub parts: Vec<TriggerPart>,
    pub control: Vec<ControlLanePart>,
    pub duration_secs: f64,
    /// If set, the Sound is a loop of exactly this many seconds. The render runs
    /// for `duration_secs` (a bit longer, to capture release tails), then the
    /// part that spills past `loop_secs` is folded back onto the start and the
    /// buffer is truncated to `loop_secs` — so the notes' tails ring across the
    /// seam exactly as they do when the Sound loops live, and a native
    /// `AudioBufferSourceNode` loop is bit-for-bit seamless (no crossfade).
    pub loop_secs: Option<f64>,
    pub sample_rate: f32,
    pub buffers: HashMap<AssetId, AudioBuffer>,
    pub modules: HashMap<AssetId, js_sys::WebAssembly::Module>,
    /// The worklet-shim JS source, registered onto the offline context so worklet
    /// nodes render (best-effort; a non-worklet Sound bounces fine without it).
    pub shim_source: String,
}

/// Render `job` to per-channel PCM at its sample rate, with trailing silence
/// trimmed (so a one-shot/decaying Sound auto-detects its own length).
pub async fn render(job: BounceJob) -> Result<(Vec<Vec<f32>>, u32)> {
    let sr = job.sample_rate.max(8000.0);
    let frames = (job.duration_secs.max(0.05) * sr as f64).ceil() as u32;
    let octx =
        OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(2, frames, sr)
            .map_err(|e| anyhow::anyhow!("offline ctx: {e:?}"))?;
    let base: &BaseAudioContext = octx.as_ref();

    // Best-effort: register the worklet shim so worklet nodes render offline.
    let worklet_ready = add_shim(base, &job.shim_source).await.unwrap_or(false);

    let master = base
        .create_gain()
        .map_err(|e| anyhow::anyhow!("offline master: {e:?}"))?;
    master
        .connect_with_audio_node(&base.destination())
        .map_err(|e| anyhow::anyhow!("offline master→dest: {e:?}"))?;

    // Build the playable graph (instrument-ref nodes stay as voice buses).
    let built = build::build_graph(
        base,
        &job.graph,
        &master,
        &job.buffers,
        &job.modules,
        None,
        worklet_ready,
        false,
        0.0,
    )?;
    for s in &built.sources {
        let _ = s.start();
    }

    // Scheduled note voices + control automation, from time 0.
    let mut voices: Vec<Voice> = Vec::new();
    spawn_voices(
        base,
        &built.nodes,
        &job.buffers,
        &job.modules,
        worklet_ready,
        None,
        &job.parts,
        0.0,
        &mut voices,
        usize::MAX,
    )?;
    apply_control(&built.params, &job.control, 0.0);

    // Render. Everything above must stay alive until the promise resolves.
    let promise = octx
        .start_rendering()
        .map_err(|e| anyhow::anyhow!("startRendering: {e:?}"))?;
    let rendered = JsFuture::from(promise)
        .await
        .map_err(|e| anyhow::anyhow!("offline render: {e:?}"))?;
    let buffer: AudioBuffer = rendered
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("render result is not an AudioBuffer"))?;

    // (built / voices / octx kept alive to here.)
    let _ = (&built.inner, &voices);

    let nch = buffer.number_of_channels() as usize;
    let mut channels: Vec<Vec<f32>> = Vec::with_capacity(nch);
    for ch in 0..nch {
        channels.push(
            buffer
                .get_channel_data(ch as u32)
                .map_err(|e| anyhow::anyhow!("channel {ch}: {e:?}"))?,
        );
    }
    match job.loop_secs {
        // Seamless loop: fold the wrap-around tail back, truncate to the period.
        Some(loop_secs) if loop_secs > 0.0 => {
            fold_loop_tail(&mut channels, (loop_secs * sr as f64).round() as usize);
        }
        // One-shot: trim trailing silence so the clip auto-sizes.
        _ => trim_trailing_silence(&mut channels, sr as u32),
    }
    Ok((channels, sr as u32))
}

/// Render a set of timeline audio clips (an Arrangement's bounced-Sound clips)
/// offline to PCM. Each clip is scheduled exactly as live playback does
/// ([`Player::schedule_audio_clips`](crate::Player::schedule_audio_clips)) —
/// buffer source → gain → master — but into an `OfflineAudioContext` at `at = 0`.
/// `clips` are already window-relative (the caller seek-adjusts them to the export
/// start). Runs for exactly `duration_secs` (the export window); no trailing-silence
/// trim, since the timeline length is authored.
pub async fn render_clips(
    clips: Vec<AudioClipPart>,
    buffers: std::collections::HashMap<AssetId, AudioBuffer>,
    sample_rate: f32,
    duration_secs: f64,
) -> Result<(Vec<Vec<f32>>, u32)> {
    let sr = sample_rate.max(8000.0);
    let frames = (duration_secs.max(0.05) * sr as f64).ceil() as u32;
    let octx =
        OfflineAudioContext::new_with_number_of_channels_and_length_and_sample_rate(2, frames, sr)
            .map_err(|e| anyhow::anyhow!("offline ctx: {e:?}"))?;
    let base: &BaseAudioContext = octx.as_ref();

    let master = base
        .create_gain()
        .map_err(|e| anyhow::anyhow!("offline master: {e:?}"))?;
    master
        .connect_with_audio_node(&base.destination())
        .map_err(|e| anyhow::anyhow!("offline master→dest: {e:?}"))?;

    // Every node must stay alive until the render promise resolves.
    let mut keep: Vec<AudioNode> = Vec::new();
    for c in &clips {
        let Some(buf) = buffers.get(&c.buffer) else {
            continue;
        };
        let dur = c.length.max(0.0);
        if dur <= 0.0 {
            continue;
        }
        let off = c.offset.max(0.0);
        let speed = if c.speed > 0.0 { c.speed } else { 1.0 };
        let when = c.start.max(0.0);
        let buf_dur = buf.duration();
        let span = dur * speed; // buffer seconds consumed
        let stretched = c.looping && span > (buf_dur - off) + 1e-3;

        let src = base
            .create_buffer_source()
            .map_err(|e| anyhow::anyhow!("buffer source: {e:?}"))?;
        src.set_buffer(Some(buf));
        let g = base
            .create_gain()
            .map_err(|e| anyhow::anyhow!("clip gain: {e:?}"))?;
        g.gain().set_value(c.gain);
        if (speed - 1.0).abs() > 1e-6 {
            src.playback_rate().set_value(speed as f32);
        }
        src.connect_with_audio_node(&g)
            .map_err(|e| anyhow::anyhow!("clip src→gain: {e:?}"))?;
        g.connect_with_audio_node(&master)
            .map_err(|e| anyhow::anyhow!("clip gain→master: {e:?}"))?;

        let sched: AudioScheduledSourceNode = src.clone().unchecked_into();
        if stretched {
            // Native loop over the bounced loop region (seamless — the bounce
            // folded its wrap-around tail back onto the start).
            src.set_loop(true);
            src.set_loop_start(off);
            src.set_loop_end(buf_dur);
            let _ = src.start_with_when_and_grain_offset(when, off);
            let _ = sched.stop_with_when(when + dur);
        } else {
            let _ = src.start_with_when_and_grain_offset_and_grain_duration(when, off, span);
        }
        keep.push(src.unchecked_into());
        keep.push(g.unchecked_into());
    }

    let promise = octx
        .start_rendering()
        .map_err(|e| anyhow::anyhow!("startRendering: {e:?}"))?;
    let rendered = JsFuture::from(promise)
        .await
        .map_err(|e| anyhow::anyhow!("offline render: {e:?}"))?;
    let buffer: AudioBuffer = rendered
        .dyn_into()
        .map_err(|_| anyhow::anyhow!("render result is not an AudioBuffer"))?;
    let _ = &keep; // kept alive to here

    let nch = buffer.number_of_channels() as usize;
    let mut channels: Vec<Vec<f32>> = Vec::with_capacity(nch);
    for ch in 0..nch {
        channels.push(
            buffer
                .get_channel_data(ch as u32)
                .map_err(|e| anyhow::anyhow!("channel {ch}: {e:?}"))?,
        );
    }
    Ok((channels, sr as u32))
}

/// Bake a seamless loop: any audio that spilled past the loop point (release
/// tails of notes near the end) is summed back onto the start (wrapping modulo
/// `period`), then each channel is truncated to exactly `period` samples. This
/// mirrors live looping, where a pass's tails ring on into the next pass.
fn fold_loop_tail(channels: &mut [Vec<f32>], period: usize) {
    if period == 0 {
        return;
    }
    for ch in channels.iter_mut() {
        if ch.len() > period {
            for i in period..ch.len() {
                ch[(i - period) % period] += ch[i];
            }
        }
        ch.truncate(period);
    }
}

/// Register the worklet shim onto an offline context. Returns whether it loaded.
async fn add_shim(base: &BaseAudioContext, source: &str) -> Result<bool> {
    let parts = js_sys::Array::new();
    parts.push(&JsValue::from_str(source));
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type("text/javascript");
    let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &bag)
        .map_err(|e| anyhow::anyhow!("blob: {e:?}"))?;
    let url = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| anyhow::anyhow!("blob url: {e:?}"))?;
    let wl = base
        .audio_worklet()
        .map_err(|e| anyhow::anyhow!("audioWorklet: {e:?}"))?;
    let p = wl
        .add_module(&url)
        .map_err(|e| anyhow::anyhow!("addModule: {e:?}"))?;
    JsFuture::from(p)
        .await
        .map_err(|e| anyhow::anyhow!("addModule await: {e:?}"))?;
    Ok(true)
}

/// Trim trailing samples below ~ -60 dB across all channels (keep a 30 ms pad), so
/// a decaying Sound's bounce ends where the audio actually does.
fn trim_trailing_silence(channels: &mut [Vec<f32>], sample_rate: u32) {
    const THRESH: f32 = 0.001; // ~ -60 dB
    let len = channels.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut last = 0usize;
    for i in 0..len {
        if channels
            .iter()
            .any(|c| c.get(i).is_some_and(|s| s.abs() > THRESH))
        {
            last = i;
        }
    }
    let pad = (sample_rate as usize) * 30 / 1000;
    let keep = (last + pad + 1).min(len);
    for c in channels.iter_mut() {
        c.truncate(keep);
    }
}

//! Remote-control link: the editor dials *out* to the native MCP server over a
//! WebSocket (`<origin>/editor`) and serves its requests by calling the
//! [`EditorController`](crate::controller) directly.
//!
//! Started two ways: automatically when the page is loaded with
//! `?mcp=<control-origin>` (e.g. `?mcp=http://127.0.0.1:9171`, optionally
//! `&pair=<code>`), or on demand via the top-bar MCP button → connect modal
//! (pre-filled with [`default_origin`], or the `?mcp=` origin, plus an optional
//! pairing code). Connect / disconnect surface as status toasts and a reactive
//! [`status`] signal the UI reflects.
//!
//! The link is one ordered WebSocket. The server sends [`WsServerMsg::Request`]
//! frames; we serve each and reply with a [`WsClientMsg::Response`] carrying the
//! same `id`. Editor push events go up as [`WsClientMsg::Event`]. All outbound
//! frames funnel through a single writer (an mpsc drained in [`run`]) so
//! concurrent replies/events never interleave.
//!
//! awsm-audio specifics: the controller's `dispatch`/`query` are **synchronous**,
//! so the interpreter ([`dispatch`]) is sync. Only the offline-render readbacks
//! (`RenderWav`/`WavStats`/`Waveform`) and `AttachWasm` await, so those take a
//! dedicated async branch in [`serve_one`].

use std::cell::{Cell, RefCell};

use base64::Engine;
use futures::channel::mpsc;
use futures::{FutureExt, SinkExt, StreamExt};
use futures_signals::signal::{Mutable, SignalExt};
use gloo_net::websocket::futures::WebSocket;
use gloo_net::websocket::Message;
use wasm_bindgen_futures::spawn_local;

use awsm_audio_editor_protocol::schema::SampleId;
use awsm_audio_editor_protocol::{
    EditorEvent, EditorQuery, QueryResult, RenderHandle, Request, Response, WavStats,
    WaveformEnvelope, WsClientMsg, WsServerMsg,
};

use crate::controller::controller;

/// Outbound-frame sender: every reply / event funnels through this to the single
/// writer in [`run`].
type LinkTx = mpsc::UnboundedSender<WsClientMsg>;

/// The MCP server's control origin the connect modal pre-fills when no `?mcp=`
/// param was supplied. Baked from `MCP_DEFAULT_ORIGIN` at build time (sourced from
/// `taskfiles/config.yml` → `URL_MCP_DEFAULT`), falling back to the loopback dev
/// default. The server is always local, so this is the same in dev and prod.
pub fn default_origin() -> &'static str {
    option_env!("MCP_DEFAULT_ORIGIN").unwrap_or("http://127.0.0.1:9171")
}

/// The link's connection state. The top-bar button + modal reflect this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteStatus {
    Disconnected,
    Connecting,
    Connected,
}

/// How long the "agent working" pulse lingers after the last request completes,
/// so a burst of quick mutations reads as one continuous pulse rather than a
/// flicker (and the user sees clearly when the agent has gone idle).
const WORKING_COOLDOWN_MS: u32 = 450;

thread_local! {
    static STATUS: Mutable<RemoteStatus> = Mutable::new(RemoteStatus::Disconnected);
    static ORIGIN: Mutable<String> = Mutable::new(default_origin().to_string());
    /// Pairing code to claim a specific agent (from `?pair=` or the modal). Empty
    /// unless the server needs disambiguation between multiple tabs/agents.
    static PAIR: Mutable<String> = Mutable::new(String::new());
    /// Set when the server replies `PairingRequired`, so the modal can prompt.
    static PAIRING_NEEDED: Mutable<bool> = Mutable::new(false);
    /// Outbound frame sender for the live link; `None` when disconnected. Kept so
    /// the UI can `disconnect()` and `submit_pair_code()` over the open socket.
    static SESSION: RefCell<Option<LinkTx>> = const { RefCell::new(None) };
    /// True while the MCP agent is actively serving requests (drives the UI pulse).
    static WORKING: Mutable<bool> = Mutable::new(false);
    /// Count of in-flight requests (a long render keeps the pulse lit).
    static IN_FLIGHT: Cell<u32> = const { Cell::new(0) };
    /// Bumped whenever activity starts/stops; lets a queued cooldown cancel itself.
    static IDLE_GEN: Cell<u64> = const { Cell::new(0) };
}

/// Reactive connection status (for the UI button). Consumed by the top-bar MCP
/// button + connect modal.
pub fn status() -> Mutable<RemoteStatus> {
    STATUS.with(|s| s.clone())
}

/// Reactive "agent working" flag — true while the MCP agent is serving requests
/// (plus a short cooldown). The top-bar MCP indicator pulses on this so the user
/// knows when it's safe to edit / that a render is done.
pub fn working() -> Mutable<bool> {
    WORKING.with(|w| w.clone())
}

/// Mark the start of serving one MCP request: light the pulse, cancel any pending
/// idle cooldown.
fn activity_begin() {
    IN_FLIGHT.with(|c| c.set(c.get() + 1));
    IDLE_GEN.with(|g| g.set(g.get().wrapping_add(1)));
    WORKING.with(|w| w.set_neq(true));
}

/// Mark one request done. When the last one finishes, keep the pulse lit for a
/// short cooldown, then clear it if still idle (so bursts don't flicker).
async fn activity_end() {
    let remaining = IN_FLIGHT.with(|c| {
        let n = c.get().saturating_sub(1);
        c.set(n);
        n
    });
    if remaining != 0 {
        return;
    }
    let gen = IDLE_GEN.with(|g| {
        let n = g.get().wrapping_add(1);
        g.set(n);
        n
    });
    gloo_timers::future::TimeoutFuture::new(WORKING_COOLDOWN_MS).await;
    // Still idle and no newer activity since we queued? Then we're truly done.
    if IDLE_GEN.with(|g| g.get()) == gen && IN_FLIGHT.with(|c| c.get()) == 0 {
        WORKING.with(|w| w.set_neq(false));
    }
}

/// Force the pulse off (on disconnect) so a stale "working" never lingers.
fn activity_reset() {
    IN_FLIGHT.with(|c| c.set(0));
    IDLE_GEN.with(|g| g.set(g.get().wrapping_add(1)));
    WORKING.with(|w| w.set_neq(false));
}

/// The control origin the modal pre-fills (defaults to [`default_origin`];
/// overwritten by `?mcp=` or the last connect attempt).
pub fn origin() -> Mutable<String> {
    ORIGIN.with(|s| s.clone())
}

/// The pairing code the modal binds to (from `?pair=` or typed in). Empty means
/// "rely on auto-pairing".
pub fn pair() -> Mutable<String> {
    PAIR.with(|s| s.clone())
}

/// True when the server asked for a pairing code (the modal reveals its field).
pub fn pairing_needed() -> Mutable<bool> {
    PAIRING_NEEDED.with(|s| s.clone())
}

/// Send a pairing code over the live link (or stash it for the next connect).
/// Lets the user pair an already-open socket after a `PairingRequired`.
pub fn submit_pair_code(code: String) {
    let code = code.trim().to_string();
    PAIR.with(|p| p.set(code.clone()));
    if code.is_empty() {
        return;
    }
    let sent = SESSION.with(|s| {
        s.borrow()
            .as_ref()
            .map(|tx| {
                tx.unbounded_send(WsClientMsg::Pair { code: code.clone() })
                    .is_ok()
            })
            .unwrap_or(false)
    });
    if sent {
        PAIRING_NEEDED.with(|n| n.set_neq(false));
    } else {
        // Not connected yet — connect; `run` sends the stashed code on attach.
        connect(origin().get_cloned());
    }
}

/// Surface a short message on the editor's status line.
fn toast(msg: impl Into<String>) {
    controller().status.set(Some(msg.into()));
}

/// Connect to the MCP server at `control_origin`. No-op if already connecting or
/// connected. Surfaces connect / disconnect / failure on the status line and
/// drives the [`status`] signal.
pub fn connect(control_origin: String) {
    let status = status();
    if status.get() != RemoteStatus::Disconnected {
        return; // already connecting or connected
    }
    origin().set(control_origin.clone());
    status.set(RemoteStatus::Connecting);

    spawn_local(async move {
        let result = run(control_origin).await;
        SESSION.with(|s| *s.borrow_mut() = None);
        PAIRING_NEEDED.with(|n| n.set_neq(false));
        activity_reset(); // never leave a stale "working" pulse after the link drops
        let was_connected = status.get() == RemoteStatus::Connected;
        status.set(RemoteStatus::Disconnected);
        match (was_connected, result) {
            // Dropped after a successful connect (server stopped, or user clicked
            // disconnect) — informational, not an error.
            (true, res) => {
                if let Err(e) = res {
                    tracing::warn!("mcp link ended: {e}");
                }
                toast("MCP disconnected");
            }
            // Never got connected — the connect itself failed (server down, …).
            (false, Err(e)) => toast(format!("MCP connect failed: {e}")),
            (false, Ok(())) => {} // run() only returns Ok via the link closing
        }
    });
}

/// Disconnect the live link (drops the outbound sender, which ends [`run`]). No-op
/// when not connected. The "MCP disconnected" message is emitted by the connect
/// task once `run` unwinds. Consumed by the connect UI.
pub fn disconnect() {
    SESSION.with(|s| *s.borrow_mut() = None);
}

/// Push an editor → agent event over the link (status toast, selection change).
/// No-op when no link is attached. The MCP server relays it to the bound agent as
/// a logging notification. Best-effort — failures are silent.
pub fn notify_event(event: EditorEvent) {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.unbounded_send(WsClientMsg::Event(event));
        }
    });
}

/// Forward the editor's status-line messages to the bound agent as `toast`
/// events. Call once at startup. (No-op until/unless a link attaches.)
pub fn start_event_forwarding() {
    let status = controller().status.signal_cloned();
    spawn_local(status.for_each(|msg| {
        if let Some(message) = msg {
            notify_event(EditorEvent {
                kind: "toast".to_string(),
                level: Some("info".to_string()),
                message: Some(message),
                nodes: None,
            });
        }
        async {}
    }));
}

/// Derive the `/editor` WebSocket URL from a control origin (`http`→`ws`,
/// `https`→`wss`). The server is loopback HTTP, so this is normally `ws://`.
fn ws_url(control_origin: &str) -> String {
    let origin = control_origin.trim_end_matches('/');
    let ws = if let Some(rest) = origin.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = origin.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{origin}")
    };
    format!("{ws}/editor")
}

async fn run(control_origin: String) -> Result<(), String> {
    let url = ws_url(&control_origin);
    tracing::info!("mcp: connecting to {url}");
    let ws = WebSocket::open(&url).map_err(|e| format!("ws open: {e}"))?;
    let (mut sink, mut stream) = ws.split();

    // Outbound frames funnel through one writer (drained below) so concurrent
    // replies/events never interleave a half-written frame.
    let (out_tx, mut out_rx) = mpsc::unbounded::<WsClientMsg>();
    SESSION.with(|s| *s.borrow_mut() = Some(out_tx));
    status().set(RemoteStatus::Connected);
    PAIRING_NEEDED.with(|n| n.set_neq(false));
    toast("MCP connected");
    tracing::info!("mcp: attached");

    // If a pairing code is set, claim our agent up front.
    let pair_code = PAIR.with(|p| p.get_cloned());
    if !pair_code.is_empty() {
        send_frame(WsClientMsg::Pair { code: pair_code });
    }

    loop {
        futures::select! {
            inbound = stream.next().fuse() => match inbound {
                Some(Ok(Message::Text(txt))) => match serde_json::from_str::<WsServerMsg>(&txt) {
                    Ok(WsServerMsg::Request { id, req }) => spawn_local(serve_one(id, req)),
                    Ok(WsServerMsg::PairingRequired) => {
                        PAIRING_NEEDED.with(|n| n.set_neq(true));
                        toast("MCP: enter the pairing code shown by your agent");
                    }
                    Ok(WsServerMsg::Detached) => {
                        toast("MCP: detached (another tab paired)");
                        return Ok(());
                    }
                    Err(e) => tracing::warn!("mcp: bad frame: {e}"),
                },
                Some(Ok(_)) => {} // non-text frame; ignore
                Some(Err(e)) => return Err(format!("ws read: {e}")),
                None => return Ok(()), // socket closed by server
            },
            outbound = out_rx.next().fuse() => match outbound {
                Some(frame) => {
                    let txt = serde_json::to_string(&frame)
                        .map_err(|e| format!("encode frame: {e}"))?;
                    sink.send(Message::Text(txt))
                        .await
                        .map_err(|e| format!("ws send: {e}"))?;
                }
                None => return Ok(()), // outbound sender dropped → disconnect()
            },
        }
    }
}

/// Queue an outbound frame on the live link (best-effort).
fn send_frame(frame: WsClientMsg) {
    SESSION.with(|s| {
        if let Some(tx) = s.borrow().as_ref() {
            let _ = tx.unbounded_send(frame);
        }
    });
}

/// Serve one decoded request and reply with the matching `id`. Cheap requests go
/// through the sync [`dispatch`]; the offline-render readbacks and `AttachWasm`
/// take the async branch.
async fn serve_one(id: u64, req: Request) {
    activity_begin();
    let resp = match req {
        Request::RenderWav {
            sample,
            sample_rate,
            duration_secs,
        } => render_wav(sample, sample_rate, duration_secs).await,
        Request::AttachWasm {
            node,
            wasm_base64,
            label,
        } => attach_wasm(node, wasm_base64, label).await,
        Request::LoadAudio { node, url, label } => load_audio(node, url, label).await,
        Request::Query(q) if is_wav_query(&q) => render_query(q).await,
        req => dispatch(req),
    };
    send_frame(WsClientMsg::Response { id, resp });
    activity_end().await;
}

/// True for the readback queries that require an async offline render (routed to
/// [`render_query`] instead of the sync [`dispatch`]).
fn is_wav_query(q: &EditorQuery) -> bool {
    matches!(
        q,
        EditorQuery::WavStats { .. } | EditorQuery::Waveform { .. }
    )
}

/// Interpret a (synchronous) request against the live controller. All editor
/// mutation flows through `EditorController`; this layer only translates. The
/// render/attach requests never reach here (filtered in [`serve_one`]).
fn dispatch(req: Request) -> Response {
    let ctrl = controller();
    match req {
        Request::Dispatch(cmd) => {
            ctrl.dispatch(cmd);
            Response::Ok
        }
        Request::DispatchBatch(cmds) => {
            for c in cmds {
                ctrl.dispatch(c);
            }
            Response::Ok
        }
        Request::Query(q) => Response::Query(Box::new(ctrl.query(q))),
        Request::Play => {
            ctrl.play();
            Response::Ok
        }
        Request::Stop => {
            ctrl.stop();
            Response::Ok
        }
        Request::SetActiveSample { sample } => {
            ctrl.switch_sample(sample);
            Response::Ok
        }
        Request::RenderWav { .. } | Request::AttachWasm { .. } | Request::LoadAudio { .. } => {
            unreachable!("RenderWav/AttachWasm/LoadAudio are served on the async branch")
        }
    }
}

/// Offline-render a Sound, upload the `.wav` to the server over a **dedicated
/// HTTP POST** (the bytes never ride the control link), and reply with a small
/// [`RenderHandle`]. Posting *before* replying guarantees the server has the
/// bytes by the time it sees the handle.
async fn render_wav(
    sample: Option<SampleId>,
    sample_rate: Option<f32>,
    duration_secs: Option<f64>,
) -> Response {
    let ctrl = controller();
    let (channels, rate) = match ctrl.render_pcm(sample, sample_rate, duration_secs).await {
        Ok(pcm) => pcm,
        Err(e) => return Response::Err(e),
    };
    let wav = crate::util::encode_wav(&channels, rate);
    let byte_len = wav.len();
    let stats = WavStats::from_pcm(&channels, rate);
    let render_id = uuid::Uuid::new_v4().to_string();
    if let Err(e) = upload_render(&render_id, wav).await {
        return Response::Err(format!("render upload failed: {e}"));
    }
    Response::Render(RenderHandle {
        render_id,
        byte_len,
        duration_secs: stats.duration_secs,
        peak: stats.peak,
        rms: stats.rms,
    })
}

/// POST the rendered `.wav` to `<origin>/renders/<render_id>` over plain HTTP —
/// a separate connection from the control link, so a large render never blocks
/// small frames.
async fn upload_render(render_id: &str, wav: Vec<u8>) -> Result<(), String> {
    let origin = ORIGIN.with(|o| o.get_cloned());
    let url = format!("{}/renders/{render_id}", origin.trim_end_matches('/'));
    let body = js_sys::Uint8Array::from(wav.as_slice());
    let resp = gloo_net::http::Request::post(&url)
        .header("content-type", "application/octet-stream")
        .body(body)
        .map_err(|e| format!("build request: {e}"))?
        .send()
        .await
        .map_err(|e| format!("send: {e}"))?;
    if !resp.ok() {
        return Err(format!("server returned HTTP {}", resp.status()));
    }
    Ok(())
}

/// Offline-render a Sound and compute the numeric `WavStats` / `Waveform`
/// readback (the analog of the renderer's pixel readbacks).
async fn render_query(q: EditorQuery) -> Response {
    let (sample, want_waveform, buckets) = match &q {
        EditorQuery::WavStats { sample } => (*sample, false, 0),
        EditorQuery::Waveform { sample, buckets } => (*sample, true, *buckets),
        _ => unreachable!("render_query only handles the WAV queries"),
    };
    let ctrl = controller();
    match ctrl.render_pcm(sample, None, None).await {
        Ok((channels, rate)) => {
            let qr = if want_waveform {
                QueryResult::Waveform(WaveformEnvelope::from_pcm(&channels, rate, buckets))
            } else {
                QueryResult::WavStats(WavStats::from_pcm(&channels, rate))
            };
            Response::Query(Box::new(qr))
        }
        Err(e) => Response::Err(e),
    }
}

/// Load an external audio file (by URL) into a buffer-source / convolver node.
async fn load_audio(
    node: awsm_audio_editor_protocol::schema::NodeId,
    url: String,
    label: Option<String>,
) -> Response {
    match controller().load_audio_url(node, url, label).await {
        Ok(info) => Response::AudioLoaded(info),
        Err(e) => Response::Err(e),
    }
}

/// Decode + attach a compiled WASM DSP module to an AudioWorklet node, awaiting
/// the compile so a bad module's error surfaces back to the agent.
async fn attach_wasm(
    node: awsm_audio_editor_protocol::schema::NodeId,
    wasm_base64: String,
    label: String,
) -> Response {
    let bytes = match base64::engine::general_purpose::STANDARD.decode(wasm_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return Response::Err(format!("bad base64: {e}")),
    };
    let label = if label.is_empty() {
        "module".to_string()
    } else {
        label
    };
    match controller()
        .attach_wasm_bytes_async(node, bytes, label)
        .await
    {
        Ok(()) => Response::Ok,
        Err(e) => Response::Err(e),
    }
}

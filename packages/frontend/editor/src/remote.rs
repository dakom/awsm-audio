//! Remote-control link: the editor dials *out* to the native MCP server over
//! WebTransport (QUIC) and serves its requests by calling the
//! [`EditorController`](crate::controller) directly.
//!
//! Started two ways: automatically when the page is loaded with
//! `?mcp=<control-origin>` (e.g. `?mcp=http://127.0.0.1:9171`), or on demand via
//! the top-bar MCP button → connect modal (pre-filled with [`default_origin`], or
//! the `?mcp=` origin if one was supplied, and editable there). Connect /
//! disconnect surface as status toasts and a reactive [`status`] signal the UI
//! reflects.
//!
//! Flow: fetch `<control-origin>/control` → `{ quic_url, cert_hash }` → open a
//! WebTransport session pinning that self-signed cert hash → loop accepting
//! server-initiated bidirectional streams, one [`Request`] each, replying with a
//! [`Response`] on the same stream (framing by stream-finish).
//!
//! awsm-audio specifics: the controller's `dispatch`/`query` are **synchronous**,
//! so the interpreter ([`dispatch`]) is sync. Only the offline-render readbacks
//! (`RenderWav`/`WavStats`/`Waveform`) and `AttachWasm` await, so those take a
//! dedicated async branch in [`serve_one`].

use std::cell::{Cell, RefCell};

use base64::Engine;
use futures_signals::signal::Mutable;
use serde::Deserialize;
use wasm_bindgen_futures::spawn_local;
use web_transport::{ClientBuilder, RecvStream, SendStream, Session};

use awsm_audio_editor_protocol::schema::SampleId;
use awsm_audio_editor_protocol::{
    EditorEvent, EditorQuery, QueryResult, Request, Response, WavStats, WaveformEnvelope,
};

use crate::controller::controller;

/// Cap on a single inbound request (bounds memory if a peer streams without
/// finishing). Requests are small; 16 MiB is far outside the legitimate range.
const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;

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
    /// The live session, kept so the UI can `disconnect()` it.
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
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

/// Surface a short message on the editor's status line.
fn toast(msg: impl Into<String>) {
    controller().status.set(Some(msg.into()));
}

#[derive(Deserialize)]
struct ControlInfo {
    quic_url: String,
    cert_hash: String,
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
            // Never got connected — the connect itself failed (server down, bad
            // cert, …).
            (false, Err(e)) => toast(format!("MCP connect failed: {e}")),
            (false, Ok(())) => {} // run() only returns Ok via the accept loop ending
        }
    });
}

/// Disconnect the live link (closes the WebTransport session). No-op when not
/// connected. The "MCP disconnected" message is emitted by the connect task once
/// the accept loop unwinds. Consumed by the connect UI.
pub fn disconnect() {
    SESSION.with(|s| {
        if let Some(session) = s.borrow().as_ref() {
            session.close(0, "client disconnect");
        }
    });
}

/// Push an editor → agent event over the link (status toast, selection change).
/// No-op when no link is attached. Each event rides its own unidirectional stream
/// (framed by stream-finish); the MCP server relays it to the agent as a logging
/// notification. Best-effort — failures are logged, never surfaced. Wired to the
/// controller's toast/selection emitters in the Phase 3.5 follow-up.
#[allow(dead_code)]
pub fn notify_event(event: EditorEvent) {
    let session = SESSION.with(|s| s.borrow().clone());
    let Some(session) = session else {
        return;
    };
    spawn_local(async move {
        if let Err(e) = send_event(session, &event).await {
            tracing::debug!("mcp notify failed: {e}");
        }
    });
}

async fn send_event(session: Session, ev: &EditorEvent) -> Result<(), String> {
    let mut send = session
        .open_uni()
        .await
        .map_err(|e| format!("open_uni: {e}"))?;
    let bytes = serde_json::to_vec(ev).map_err(|e| format!("encode event: {e}"))?;
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let n = send.write(buf).await.map_err(|e| format!("write: {e}"))?;
        buf = &buf[n..];
    }
    send.finish().map_err(|e| format!("finish: {e}"))?;
    Ok(())
}

async fn run(control_origin: String) -> Result<(), String> {
    let control_url = format!("{}/control", control_origin.trim_end_matches('/'));
    tracing::info!("mcp: fetching control info from {control_url}");

    let info: ControlInfo = gloo_net::http::Request::get(&control_url)
        .send()
        .await
        .map_err(|e| format!("control fetch: {e}"))?
        .json()
        .await
        .map_err(|e| format!("control decode: {e}"))?;

    let cert_hash = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(info.cert_hash.as_bytes())
        .map_err(|e| format!("bad cert hash: {e}"))?;

    let client = ClientBuilder::new()
        .with_server_certificate_hashes(vec![cert_hash])
        .map_err(|e| format!("client builder: {e}"))?;

    let url: url::Url = info
        .quic_url
        .parse()
        .map_err(|e| format!("bad quic url {}: {e}", info.quic_url))?;

    tracing::info!("mcp: connecting to {url}");
    let session = client
        .connect(url)
        .await
        .map_err(|e| format!("connect: {e}"))?;

    SESSION.with(|s| *s.borrow_mut() = Some(session.clone()));
    status().set(RemoteStatus::Connected);
    toast("MCP connected");
    tracing::info!("mcp: attached");

    loop {
        let (send, recv) = session
            .clone()
            .accept_bi()
            .await
            .map_err(|e| format!("accept_bi: {e}"))?;
        spawn_local(serve_one(send, recv));
    }
}

/// Read one request off a stream, serve it, and write the response back. Cheap
/// requests go through the sync [`dispatch`]; the offline-render readbacks and
/// `AttachWasm` take the async branch.
async fn serve_one(mut send: SendStream, mut recv: RecvStream) {
    activity_begin();
    let resp = match read_request(&mut recv).await {
        Ok(Request::RenderWav {
            sample,
            sample_rate,
        }) => render_wav(sample, sample_rate).await,
        Ok(Request::AttachWasm {
            node,
            wasm_base64,
            label,
        }) => attach_wasm(node, wasm_base64, label).await,
        Ok(Request::Query(q)) if is_wav_query(&q) => render_query(q).await,
        Ok(req) => dispatch(req),
        Err(e) => Response::Err(e),
    };
    if let Err(e) = reply(&mut send, &resp).await {
        tracing::warn!("mcp: reply failed: {e}");
    }
    activity_end().await;
}

async fn read_request(recv: &mut RecvStream) -> Result<Request, String> {
    let mut buf = Vec::new();
    while let Some(chunk) = recv
        .read(64 * 1024)
        .await
        .map_err(|e| format!("read: {e}"))?
    {
        buf.extend_from_slice(&chunk);
        if buf.len() > MAX_REQUEST_BYTES {
            return Err(format!("request exceeded {MAX_REQUEST_BYTES} bytes"));
        }
    }
    serde_json::from_slice(&buf).map_err(|e| format!("decode request: {e}"))
}

async fn reply(send: &mut SendStream, resp: &Response) -> Result<(), String> {
    let bytes = serde_json::to_vec(resp).map_err(|e| format!("encode response: {e}"))?;
    let mut buf = bytes.as_slice();
    while !buf.is_empty() {
        let n = send.write(buf).await.map_err(|e| format!("write: {e}"))?;
        buf = &buf[n..];
    }
    send.finish().map_err(|e| format!("finish: {e}"))?;
    Ok(())
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
        Request::RenderWav { .. } | Request::AttachWasm { .. } => {
            unreachable!("RenderWav/AttachWasm are served on the async branch")
        }
    }
}

/// Offline-render a Sound and return the `.wav` bytes.
async fn render_wav(sample: Option<SampleId>, sample_rate: Option<f32>) -> Response {
    let ctrl = controller();
    match ctrl.render_pcm(sample, sample_rate).await {
        Ok((channels, rate)) => Response::Wav(crate::util::encode_wav(&channels, rate)),
        Err(e) => Response::Err(e),
    }
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
    match ctrl.render_pcm(sample, None).await {
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

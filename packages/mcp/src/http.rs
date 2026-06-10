//! The HTTP surface:
//! - `GET  /editor`     — the editor's WebSocket upgrade (the live editor link).
//! - `POST /debug`      — raw request seam: a JSON [`Request`] body is relayed to
//!   an attached editor and its [`Response`] returned as JSON.
//! - `POST /renders/{id}` / `GET /renders/{id}.wav` — the render byte side-channel.
//! - `/mcp`             — the rmcp streamable-HTTP endpoint mounts onto this router.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::{Json, Router, extract::Path, extract::State, routing::get, routing::post};
use serde_json::{Value, json};
use tower_http::cors::{Any, CorsLayer};

use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};

use awsm_audio_editor_protocol::Request;

use crate::link::EditorLink;
use crate::mcp::EditorMcp;

/// Cap on retained render files (bounds temp-dir disk use). Renders past this are
/// evicted oldest-first and their files deleted.
const MAX_RETAINED_RENDERS: usize = 32;

/// On-disk path the editor's render upload lands at (and `render_wav` reads back).
/// Both sides agree on this naming so the tool needs no shared in-memory map.
pub(crate) fn render_path(render_id: &str) -> PathBuf {
    std::env::temp_dir().join(format!("awsm-audio-mcp-{render_id}.wav"))
}

#[derive(Clone)]
struct AppState {
    link: EditorLink,
    /// Insertion-ordered render ids for LRU eviction (see [`MAX_RETAINED_RENDERS`]).
    renders: Arc<Mutex<VecDeque<String>>>,
}

/// Serve the HTTP surface on `addr` until shutdown.
pub async fn serve(addr: SocketAddr, link: EditorLink) -> Result<()> {
    let state = AppState {
        link: link.clone(),
        renders: Arc::new(Mutex::new(VecDeque::new())),
    };

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any)
        // Private Network Access: let a public HTTPS page (e.g. the hosted editor)
        // reach this loopback server -- Chrome demands this opt-in on the preflight.
        .allow_private_network(true);

    // The rmcp MCP endpoint: a streamable-HTTP tower service mounted at /mcp.
    // A fresh handler is built per session, sharing the (Arc-backed) editor link.
    //
    // Long-lived sessions: rmcp's default drops a session after 5 min idle — far
    // too short for an interactive coding agent that sits idle between tool calls.
    // That idle "safety net" exists for servers behind proxies that silently drop
    // connections; we're loopback-only, so use a day-long timeout (still reclaims
    // a genuinely-dead session, but never an idle-but-live one).
    let mut session_manager = LocalSessionManager::default();
    session_manager.session_config.keep_alive = Some(Duration::from_secs(60 * 60 * 24));
    let mcp_link = link.clone();
    let mcp_service = StreamableHttpService::new(
        move || Ok(EditorMcp::new(mcp_link.clone())),
        Arc::new(session_manager),
        StreamableHttpServerConfig::default(),
    );

    let app = Router::new()
        // The editor dials out to this WebSocket for the live link.
        .route("/editor", get(editor_ws))
        .route("/debug", post(debug))
        // The render side-channel: the editor POSTs `.wav` bytes here (off the
        // control link); humans/tools GET them back.
        .route("/renders/{id}", post(render_upload).get(render_download))
        // The load side-channel: `load_audio` hosts agent-local audio files here
        // for the editor to fetch (off the control link).
        .route("/assets/{id}", get(asset_download))
        .nest_service("/mcp", mcp_service)
        .layer(cors)
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("http listening on http://{addr} (/mcp, /editor, /debug, /renders)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Upgrade the editor's `/editor` request to a WebSocket and hand it to the link.
async fn editor_ws(ws: WebSocketUpgrade, State(s): State<AppState>) -> impl IntoResponse {
    let link = s.link.clone();
    ws.on_upgrade(move |socket| crate::ws::handle_socket(socket, link))
}

/// Relay a raw [`Request`] (JSON body) to the most-recently-attached editor and
/// return its [`Response`]. A `RenderWav` here returns the `RenderHandle` JSON;
/// the editor has already POSTed the bytes to `/renders/<id>` (fetch them at
/// `/renders/<id>.wav`).
async fn debug(State(s): State<AppState>, Json(req): Json<Request>) -> Json<Value> {
    match s.link.debug_request(&req).await {
        Ok(resp) => Json(
            serde_json::to_value(&resp)
                .unwrap_or_else(|e| json!({ "encode_error": e.to_string() })),
        ),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}

/// `POST /renders/{id}` — the editor uploads a rendered `.wav` here (off the
/// control link). We write it to a temp file and remember the id for LRU
/// eviction.
async fn render_upload(
    State(s): State<AppState>,
    Path(id): Path<String>,
    body: Bytes,
) -> StatusCode {
    let path = render_path(&id);
    if let Err(e) = std::fs::write(&path, &body) {
        tracing::warn!("render upload write failed ({}): {e}", path.display());
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    tracing::debug!("render {id}: {} bytes → {}", body.len(), path.display());
    // Track for eviction; drop the oldest beyond the cap.
    let mut q = s.renders.lock().unwrap();
    q.push_back(id);
    while q.len() > MAX_RETAINED_RENDERS {
        if let Some(old) = q.pop_front() {
            let _ = std::fs::remove_file(render_path(&old));
        }
    }
    StatusCode::OK
}

/// `GET /renders/{id}.wav` — serve a previously-uploaded render (for humans /
/// tooling). The `{id}` segment carries the `.wav` suffix, which we strip.
async fn render_download(Path(file): Path<String>) -> impl IntoResponse {
    let id = file.strip_suffix(".wav").unwrap_or(&file);
    match std::fs::read(render_path(id)) {
        Ok(bytes) => ([(header::CONTENT_TYPE, "audio/wav")], bytes).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, "no such render").into_response(),
    }
}

/// `GET /assets/{id}` — serve an agent-supplied audio file for the editor to
/// fetch + `decodeAudioData` (the `load_audio` side-channel).
async fn asset_download(State(s): State<AppState>, Path(id): Path<String>) -> impl IntoResponse {
    match s.link.asset_bytes(&id) {
        Some((bytes, content_type)) => {
            ([(header::CONTENT_TYPE, content_type)], bytes).into_response()
        }
        None => (StatusCode::NOT_FOUND, "no such asset").into_response(),
    }
}

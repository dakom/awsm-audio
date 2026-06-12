//! awsm-audio-editor — bootstrap.
//!
//! A node-graph WebAudio editor. Boot installs the [`controller`] singleton
//! (the single command/query authority) before mounting any UI, so every panel
//! dispatches through it. The two `#[wasm_bindgen]` seams at the bottom are the
//! read/write entry points a future MCP transport drives — designed now, wired
//! to a server later.

mod catalog;
mod controller;
mod fields;
mod fs;
mod mcp_activity;
mod ports;
mod remote;
mod theme;
mod ui;
mod util;
mod widgets;

use dominator::stylesheet;
use wasm_bindgen::prelude::*;

fn main() {
    console_error_panic_hook::set_once();
    tracing_wasm::set_as_global_default();

    // Establish the command/query authority before any UI is mounted.
    controller::init();

    // Install the graphite/slate design tokens + base resets (shared with
    // awsm-renderer). Everything below references `var(--…)` into this block.
    theme::init();

    // Hovering a wire's hit-area thickens the visible line (a delete affordance).
    stylesheet!(".wire-hit:hover + .wire-line", {
        .style("stroke-width", "4.5")
    });

    // Example-browser cards lift on hover.
    stylesheet!(".ex-card:hover", {
        .style("background", "var(--bg-hover)")
        .style("border-color", "var(--accent-line)")
        .style("transform", "translateY(-1px)")
    });

    dominator::append_dom(&dominator::body(), ui::render());

    // Auto-attach to an MCP server when the page is loaded with
    // `?mcp=<host:port>` (e.g. `?mcp=127.0.0.1:9171`), optionally `&pair=<code>`
    // to claim a specific agent and `&tls=true` for a TLS-terminated server
    // (`wss`/`https`). Without `mcp` the link stays idle until the user connects
    // via the top-bar button.
    remote::start_event_forwarding();
    if let Some(code) = query_param("pair") {
        remote::pair().set(code);
    }
    if query_param("tls").is_some_and(|v| v == "true") {
        remote::tls().set(true);
    }
    if let Some(origin) = query_param("mcp") {
        // `connect` normalizes the authority and stores it; no need to set it here.
        remote::connect(origin);
    }
}

/// Parse a `<key>=<value>` query parameter from the page URL, if present.
fn query_param(key: &str) -> Option<String> {
    let search = web_sys::window()?.location().search().ok()?;
    let trimmed = search.strip_prefix('?').unwrap_or(&search);
    trimmed.split('&').find_map(|pair| {
        let (k, value) = pair.split_once('=')?;
        if k != key {
            return None;
        }
        let decoded = js_sys::decode_uri_component(value)
            .ok()
            .and_then(|s| s.as_string())
            .unwrap_or_else(|| value.to_string());
        (!decoded.is_empty()).then_some(decoded)
    })
}

/// External-inspection seam: the serializable editor snapshot as TOML. This is
/// exactly what an MCP/websocket transport (or a headless test driver) reads.
#[wasm_bindgen]
pub fn editor_snapshot_toml() -> String {
    toml::to_string_pretty(&controller::controller().snapshot())
        .unwrap_or_else(|e| format!("# error: {e}"))
}

/// External-dispatch seam: decode a TOML [`EditorCommand`] and dispatch it
/// through the controller — the write half of the future MCP transport.
#[wasm_bindgen]
pub fn editor_dispatch_toml(cmd_toml: &str) -> String {
    match toml::from_str::<controller::EditorCommand>(cmd_toml) {
        Ok(cmd) => {
            controller::controller().dispatch(cmd);
            "ok".to_string()
        }
        Err(err) => format!("decode error: {err}"),
    }
}

/// External-query seam: decode a TOML [`EditorQuery`] and answer with the
/// [`QueryResult`] as TOML — the read half of the future MCP transport (the
/// typed counterpart to the ad-hoc `editor_*` getters below).
#[wasm_bindgen]
pub fn editor_query_toml(query_toml: &str) -> String {
    match toml::from_str::<controller::EditorQuery>(query_toml) {
        Ok(q) => toml::to_string_pretty(&controller::controller().query(q))
            .unwrap_or_else(|e| format!("# error: {e}")),
        Err(err) => format!("decode error: {err}"),
    }
}

/// Load a built-in example by name (the keys from `examples::all()`).
#[wasm_bindgen]
pub fn editor_load_example(name: &str) -> String {
    controller::controller().load_example(name);
    "ok".to_string()
}

/// Serialize the full editor project (library + layout + camera) as TOML — what
/// Save writes.
#[wasm_bindgen]
pub fn editor_save_project_toml() -> String {
    toml::to_string_pretty(&controller::controller().to_project())
        .unwrap_or_else(|e| format!("# error: {e}"))
}

/// Open a full editor project (TOML), restoring layout + camera + assets.
#[wasm_bindgen]
pub fn editor_open_project_toml(project_toml: &str) -> String {
    match toml::from_str::<controller::snapshot::EditorProject>(project_toml) {
        Ok(p) => {
            controller::controller().open_project(p);
            "ok".to_string()
        }
        Err(e) => format!("decode error: {e}"),
    }
}

/// Load a whole `SampleLibrary` (TOML) into the editor, replacing the canvas.
#[wasm_bindgen]
pub fn editor_load_toml(library_toml: &str) -> String {
    match toml::from_str::<awsm_audio_schema::SampleLibrary>(library_toml) {
        Ok(lib) => {
            controller::controller().load_library(lib);
            "ok".to_string()
        }
        Err(err) => format!("decode error: {err}"),
    }
}

/// Attach a WASM DSP module (base64-encoded `.wasm`) to an AudioWorklet node by
/// id — the command-driven equivalent of the node's file picker, for an MCP
/// driver or headless test.
#[wasm_bindgen]
pub fn editor_attach_wasm(node_id: &str, wasm_base64: &str) -> String {
    let Ok(id) = node_id.parse::<awsm_audio_schema::NodeId>() else {
        return format!("bad node id: {node_id}");
    };
    let bytes =
        match base64::Engine::decode(&base64::engine::general_purpose::STANDARD, wasm_base64) {
            Ok(b) => b,
            Err(e) => return format!("bad base64: {e}"),
        };
    controller::controller().attach_wasm_bytes(id, bytes, "module".to_string());
    "ok".to_string()
}

/// Transport control seams (also useful for an MCP driver): start/stop the
/// current graph and probe the live output.
#[wasm_bindgen]
pub fn editor_play() {
    controller::controller().play();
}

#[wasm_bindgen]
pub fn editor_stop() {
    controller::controller().stop();
}

/// Current output peak level (0..1).
#[wasm_bindgen]
pub fn editor_audio_peak() -> f32 {
    controller::controller().audio_peak()
}

/// The piano-roll playhead position (beats; `-1` = hidden). Inspection seam.
#[wasm_bindgen]
pub fn editor_playhead() -> f64 {
    controller::controller().playhead.get()
}

/// Audio context state: `"none"` / `"suspended"` / `"running"` / `"closed"`.
#[wasm_bindgen]
pub fn editor_audio_state() -> String {
    controller::controller().audio_state()
}

/// Latest output waveform samples (0..=255, 128 = silence). Empty if not running.
#[wasm_bindgen]
pub fn editor_waveform() -> Vec<u8> {
    controller::controller().audio_waveform()
}

/// Export the active sample to a `.wav` (offline render — a Sound's bounce, or an
/// Arrangement's clip timeline over its marked/whole window).
#[wasm_bindgen]
pub fn editor_export_wav() {
    controller::controller().export_active_wav();
}

/// Set the spatial listener position — the "player"-side control a runtime moves
/// to reposition the whole scene relative to the listener.
#[wasm_bindgen]
pub fn editor_set_listener(x: f32, y: f32, z: f32) {
    controller::controller().set_listener_position(x, y, z);
}

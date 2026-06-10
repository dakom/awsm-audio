# Plan: MCP WebSocket migration + off-link renders + session isolation + distribution

> **Status: implemented (2026-06-10).** All three phases landed. Build gates 1–3
> pass: `task lint` green, protocol tests pass, a headless smoke test of the HTTP
> surface (ws upgrade → 101, `/renders` POST→GET, `/debug` → "no editor attached"),
> and `dist build` produces a working `awsm-audio-mcp` binary. **Still requires the
> user** before a release: (a) create the public `dakom/homebrew-tap` repo + a
> `HOMEBREW_TAP_TOKEN` repo secret, (b) tag `v0.1.0` to trigger the release
> workflow, and (c) the *live* parts of build gate 2 (browser attach, render
> upload, multi-tab pairing/isolation) — these need a real browser + MCP client.

The `awsm-audio-mcp` server was built over WebTransport. This plan replaced the
transport with a WebSocket, moved render bytes off the link, made agent↔tab
routing fully isolated, and set up binary distribution. Every decision below is
**locked**. The phases were followed in order; each ends with a **build gate**.

---

## Current state (the facts that shape this)

- **Inverted, stateless topology.** The browser editor dials *out* to the server;
  the **server** initiates `Request`s to the browser (render, dispatch, query),
  and the browser replies with a `Response`. The browser also pushes
  `EditorEvent`s. The browser holds all document truth; the server is a bridge.
- **Two listeners** (`packages/mcp/src/main.rs`): client-port **9171** (HTTP/TCP:
  rmcp `/mcp` + `/control` + `/debug`) and browser-port **9172**
  (WebTransport/QUIC/UDP, the editor dials in).
- **WebTransport machinery:** self-signed cert (`cert.rs`: `rcgen`/`sha2`), QUIC
  endpoint (`quic.rs`: `quinn`/`rustls`), `/control` hands the editor
  `{ quic_url, cert_hash }` to pin. Frontend: `packages/frontend/editor/src/remote.rs`.
- **Render flow:** `render_wav` → WAV bytes ride the link → server writes a temp
  file → agent gets *path + byte count* (never bytes). `wav_stats` / `waveform`
  return small text. (`mcp.rs` `wav()` ~line 1026.)
- **No per-agent routing yet:** every `EditorMcp` shares the one `link`
  (`http.rs` `move || Ok(EditorMcp::new(mcp_link.clone()))`); `EditorLink` holds a
  single `Option<Session>` (`link.rs:24`) that each new connection silently
  overwrites — a latent multi-tab bug.
- **Push events are implemented but unwired:** `remote::notify_event` exists
  (`#[allow(dead_code)]`) but isn't called from the controller's toast/selection
  emitters, so agents don't yet receive live `EditorEvent`s. Wire it during
  Phase 2 (emit `{kind:"toast",level,message}` and `{kind:"selection",nodes}`
  where the controller sets `status` / changes selection).

## What changes & why

1. **WebTransport → WebSocket.** *Rationale: simplicity.* A localhost JSON link
   needs neither QUIC streams nor UDP, and WebTransport forces TLS → the whole
   self-signed-cert subsystem. `ws://127.0.0.1` needs none of it. One ordered ws
   on the existing HTTP port; the QUIC/UDP listener and `/control` are deleted.
2. **Renders off the link.** WAV bytes ride a dedicated **HTTP POST**; the link
   carries only a small `RenderHandle`. Keeps the link byte-light (and makes #1's
   single-channel design a non-issue).
3. **Per-session isolation.** Each agent's `EditorMcp` is bound to one editor
   `session_id`; no shared link or id space → it is *structurally impossible* for
   the server to cross agent↔tab wires. Pairing: auto-bind when unambiguous, else
   a code entered editor-side via `?pair=` or the connect modal.
4. **Distribution via `dist`.** Prebuilt binaries on GitHub Releases; `curl|sh`
   canonical, Homebrew alias. Not crates.io.

**Order:** Phase 1 (renders off-link) → Phase 2 (WebSocket + isolation) →
Phase 3 (distribution). Phase 1 is transport-agnostic and makes the link
byte-light *before* the transport swap, so Phase 2 has no large-payload concerns.

## Invariants (must not change)

- The browser holds the truth; the server is a stateless bridge.
- The link stays inverted: server is the requester, browser the responder, plus a
  browser→server event push.
- The agent never receives audio bytes — only a file path + `wav_stats` /
  `waveform`. Preserve the `"wrote N bytes to <path>"` result.

## Hard constraint (settled — do not revisit)

The SPA cannot serve render bytes at a URL the agent fetches: a browser tab is a
client, not a server. Service Workers only intercept same-origin browser
requests; they're unreachable by an external agent (which hits Cloudflare's edge,
not this user's IndexedDB). So a listening process must sit in the byte path —
the MCP server. Routing bytes through it over a *separate HTTP connection*
(Phase 1, streamed not buffered) keeps the link byte-light and works in every
browser. The only server-bypassing alternative (browser writes disk via File
System Access API) is Chromium-only — rejected.

---

## Phase 1 — Renders off the control link (HTTP side-channel)

**Goal:** `render_wav` bytes travel browser → server over a dedicated HTTP POST.
The link carries only a small `RenderHandle`. No browser-side store / no
IndexedDB — the browser renders, encodes, POSTs, discards. Agent result unchanged
(`"wrote N bytes to <path>"`), plus a human-clickable URL.

**Flow** (browser POSTs *first*, then replies — so by the time the server's tool
sees the handle, the bytes are already stored; no upload/await coordination):
```
agent → render_wav tool → server sends Request::RenderWav over the link
  browser: render PCM → encode WAV → POST http://<origin>/renders/<uuid> (HTTP, NOT the link)
           ↳ on 200 → reply Response::Render(RenderHandle{ render_id, byte_len, … }) over the link
  server POST /renders/:id handler: write temp file awsm-audio-mcp-<id>.wav, record in LRU map
  server render_wav tool: gets handle (bytes already stored) → returns path + byte count to agent
human (optional): GET http://<origin>/renders/<id>.wav to play
```

### Steps

1. **Protocol** (`packages/crates/editor-protocol/src/transport.rs`):
   - Add `RenderHandle { render_id: String, byte_len: usize, duration_secs: f64,
     peak: f32, rms: f32 }` (`Serialize`/`Deserialize` + `schemars` behind the
     existing feature). Compute `peak`/`rms`/`duration` with the existing
     `WavStats::from_pcm` math.
   - **Replace** `Response::Wav(Vec<u8>)` with `Response::Render(RenderHandle)`
     (nothing else consumes `Wav` — only the render path + `wav()`).
2. **Server HTTP routes** (`packages/mcp/src/http.rs`):
   - `POST /renders/:id` — stream the body to `std::env::temp_dir()/
     awsm-audio-mcp-<id>.wav`; insert `id → PathBuf` into a
     `renders: Arc<Mutex<LruMap>>` in `AppState`, **bounded to the last 32**
     (delete the evicted file). Return 200.
   - `GET /renders/:id.wav` — stream the cached file; 404 if unknown/evicted.
   - CORS/PNA already permit cross-origin loopback POST (`http.rs:46-52`).
3. **Server tool** (`packages/mcp/src/mcp.rs`, `wav()` ~line 1026): expect
   `Response::Render(handle)`; look up the path by `handle.render_id`; return
   `"wrote {byte_len} bytes to {path}"` + the `/renders/<id>.wav` URL.
   `wav_stats` / `waveform` are already byte-free — leave them.
4. **Browser render path** (`remote.rs` `render_wav()` ~line 364): after
   `encode_wav`, mint `render_id` (**uuid v4** — globally unique, multi-tab/
   concurrent-safe), `POST <origin>/renders/<id>` with the WAV bytes
   (`gloo_net::http::Request::post(...).body(bytes)`), await 200, then return
   `Response::Render(RenderHandle{ … })`. `render_query` (stats/waveform)
   unchanged.

### Build gate 1

- `task lint` green.
- Live: `render_wav` returns a path; the file exists, plays, and
  `GET /renders/<id>.wav` serves it.
- Logs confirm the WAV bytes went over `POST /renders/:id`, **not** the link.

---

## Phase 2 — WebTransport → WebSocket + full session isolation

**Goal:** one WebSocket per tab on the existing HTTP port; delete the cert
subsystem and the QUIC port; bind each agent to one tab so cross-talk is
impossible.

### Envelope (`editor-protocol/src/transport.rs`)

One ordered channel, so correlation is explicit. JSON **text** frames.
```rust
/// Server → browser.
#[derive(Serialize, Deserialize)]
pub enum WsServerMsg {
    Request { id: u64, req: Request },
    /// Ambiguous binding + no code supplied → editor prompts for a pairing code.
    PairingRequired,
    /// This socket's binding was taken over (e.g. page reload) → editor shows disconnected.
    Detached,
}

/// Browser → server.
#[derive(Serialize, Deserialize)]
pub enum WsClientMsg {
    /// Optional first frame to claim a binding when pairing is required.
    Pair { code: String },
    Response { id: u64, resp: Response },
    Event(EditorEvent),
}
```

### Sessions, pairing & isolation (the core of this phase)

Two independent session namespaces exist: the **tab** side (one `session_id` per
`/editor` ws) and the **agent** side (rmcp's `LocalSessionManager` already gives
each agent its own `Mcp-Session-Id` + `EditorMcp`). Nothing binds them by
default. The binding rules:

- **Each agent's `EditorMcp` is bound to exactly one editor `Connection`** (not
  the shared global link). It routes every `request()` only to that connection's
  writer + pending map, and filters events to that session. No shared id space →
  crossing is structurally impossible.
- **Auto-bind when unambiguous:** exactly one unbound tab + one unbound agent →
  bind automatically, **no code**. This is the common 1:1 case.
- **Pairing code when ambiguous** (more than one unbound tab or agent): the server
  mints a short code per *agent* MCP session (**4-char Crockford base32**, no
  ambiguous chars). When an unbound agent issues a tool call and binding is
  ambiguous, the tool returns a structured error: *"Pairing required — open the
  editor with `?pair=<CODE>` or enter `<CODE>` in the connect modal."* The agent
  relays it; the human enters the code editor-side; the editor sends
  `WsClientMsg::Pair{code}`; the server binds that tab to the agent session owning
  the code.
- **Code entry is editor-side, two ways** (mirrors the existing `?mcp=<origin>`):
  `?mcp=<origin>&pair=<CODE>` in the URL (auto-connects), or the connect-modal
  code field if `PairingRequired` arrives and no `pair=` was set.
- **Takeover/reconnect:** a reconnect that re-presents the same binding (page
  reload carrying the same `pair=`, or the same sole auto-bind slot) re-attaches
  to its agent. A *different* connection cannot seize a bound session — it goes
  through normal pairing. A displaced socket gets `Detached` and shows
  disconnected; its in-flight requests fail cleanly.

### Server steps

1. **`main.rs`:** delete cert generation, the `rustls` crypto-provider install
   (line 73), the QUIC endpoint + `accept_loop`, and the `--browser-port` arg.
   One listener remains (client-port). Update `long_about`.
2. **Delete `cert.rs` and `quic.rs`.**
3. **`http.rs`:** add `GET /editor` as an axum WebSocket upgrade
   (`axum::extract::ws`); on connect, create a `Connection`, register it, spawn
   its read loop. **Remove `/control`** (the editor derives the ws URL from its
   origin). Keep `/debug`, `/mcp`, and Phase-1 `/renders/*`.
4. **`link.rs`:** rewrite from `Session`/`open_bi` to a ws mux with
   **per-connection scoping**:
   - `Connection { session_id, pending: Mutex<HashMap<u64, oneshot::Sender<Response>>>,
     next_id, tx: mpsc::Sender<WsServerMsg> }`. One **writer task** per connection
     solely owns the ws sink (no concurrent writes — see stability note).
   - `EditorLink` tracks connections + agent→connection bindings + the
     process-wide event broadcast.
   - `request(&Request)` on a bound `EditorMcp`: snapshot its `Connection`, alloc
     an id *in that connection's space*, insert the oneshot, send, await — with a
     **120 s timeout** (covers long renders) and the "no editor bound" error path.
   - Read loop: `Response{id,resp}` → complete that connection's oneshot;
     `Event(ev)` → broadcast (filtered per binding); `Pair{code}` → bind.
   - On socket close: drain that connection's pending map (fail in-flight); if
     bound, free the agent binding.
5. **`Cargo.toml`:** remove `web-transport`, `rustls`, `rcgen`, `sha2`, `time`
   (verify `time` is only in `cert.rs`); keep `base64` (used by `AttachWasm`); add
   the `ws` feature to `axum`.

### Browser steps (`remote.rs`)

1. Replace `web-transport` `ClientBuilder`/`Session` with
   `gloo_net::websocket::futures::WebSocket` (add `websocket` + `futures` to
   `gloo-net`; drop `web-transport` + `url`).
2. `connect()`: drop the `/control` fetch + cert-hash decode. Build the ws URL
   from the **control origin** (always `http://127.0.0.1:<port>` → `ws://…/editor`;
   loopback `ws://` is not mixed-content blocked, same rule the current http
   control fetch relies on, PNA opt-in already set). If a pairing code is set,
   connect to `/editor` and send `WsClientMsg::Pair{code}` as the first frame. On
   `PairingRequired` with no code, surface the modal's code field, then reconnect.
3. Serve loop: read frames → `Request{id,req}` → `spawn_local` `serve_one` → send
   `Response{id,resp}`. **Funnel all sends through one writer** (an `mpsc` to a
   single task) so concurrent replies/events never interleave a half-written
   frame.
4. `notify_event` sends `WsClientMsg::Event` — and **wire it** to the controller's
   toast/selection emitters (the unwired follow-up noted above).
5. **Pairing UI:** parse `&pair=<code>` into a `PAIR` mutable next to `ORIGIN`;
   add a code field to the connect modal (`mcp_modal.rs`), pre-filled from it.
   Keep the existing `activity_begin/end` pulse + status UX (transport-independent).

### Config / tasks / docs

- `taskfiles/config.yml`: remove `PORT_MCP_QUIC_DEV`; keep `PORT_MCP_HTTP_DEV`
  (9171) + `URL_MCP_DEFAULT`.
- `taskfiles/mcp.yml`: `serve` drops `--browser-port`.
- `.mcp.json` unchanged (`http://127.0.0.1:9171/mcp`).
- Update module docs in `main.rs` / `link.rs` / `remote.rs` / `transport.rs`
  (they describe WebTransport/streams).

### Build gate 2

- `task lint` green.
- Editor attaches over `ws://…/editor`; attach probe (`Stop`) round-trips; a
  dispatch + query + a live event all work; Phase-1 `render_wav` still returns a
  path.
- **Load/correlation:** fire many concurrent requests (a `dispatch_batch` +
  parallel queries while a render is in flight); every response maps to the right
  request, no corrupt frames, no hang.
- **Disconnect drain:** kill the tab mid-request; the in-flight tool call errors
  promptly (no hang); the 120 s timeout is the backstop.
- **Isolation:** two agents bound to two tabs — agent 1's request only reaches
  tab 1, agent 2's only tab 2; events route to the bound agent only; no
  cross-talk under load.
- **Pairing:** one tab + one agent auto-binds (no code); a second tab/agent
  requires the code (URL or modal) and cannot attach without it.
- `cargo tree -p awsm-audio-mcp` shows no `quinn`/`rustls`/`rcgen`/`web-transport`.

---

## Phase 3 — Distribution via `dist` (cargo-dist)

**Goal:** prebuilt binaries on GitHub Releases; `curl|sh` canonical.

`awsm-audio-mcp` + `awsm-audio-editor-protocol` are `publish = false`, so
crates.io would force publishing the internal protocol crate — **not** the path.
Prebuilt binaries build from the workspace, so this is a non-issue.

### Steps

1. `dist init` — targets: **macOS arm64 + x86_64, Linux x86_64, Windows
   x86_64-msvc** (skip Linux arm64 + WinGet/Scoop for v1). Installers: `shell`,
   `powershell`, `homebrew`.
2. Create the tap repo `dakom/homebrew-tap`; point `dist` at it.
3. Commit the generated `.github/workflows/release.yml`; a `v*` tag → build all
   targets → publish binaries + checksums + installers → update the tap.
4. README install lines:
   - `curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dakom/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.sh | sh`
   - `powershell -ExecutionPolicy Bypass -c "irm https://github.com/dakom/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.ps1 | iex"`
   - `brew install dakom/tap/awsm-audio-mcp`
   - footnote: `cargo install --git https://github.com/dakom/awsm-audio awsm-audio-mcp`
5. `dist` packages only the `awsm-audio-mcp` bin; frontends (Cloudflare Pages) and
   library crates (crates.io via `taskfiles/publish.yml`) are untouched.

### Build gate 3

- `dist build` succeeds for the host target; `awsm-audio-mcp --help` runs.
- `dist plan` shows all platform artifacts + installers.
- A fresh-machine `curl|sh` install puts `awsm-audio-mcp` on PATH and it serves.

---

## Performance & stability (why this is ≥ WebTransport under busy traffic)

**Efficiency — equal/better for this workload** (given Phase 1 keeps frames
small): a long render runs async in the browser and never holds the wire — the
link is busy only for the µs of each tiny frame, so slow requests don't block
others. QUIC's wins (per-stream loss independence, no TCP head-of-line) are
network-loss features that don't exist on loopback (no loss, GB/s, µs latency).
MCP traffic peaks at tens of small calls/sec — orders of magnitude from any
ceiling. ws also has lower per-request overhead (no stream open/close).

**Stability — equal/better, with two disciplines:** (1) **exactly one writer task
per sink** — the one new failure mode vs. QUIC's separate streams is interleaved
frames; the single-writer funnel removes it. (2) **In-flight drain + 120 s
timeout** so no tool call hangs. ws message framing (one JSON object per frame)
is *simpler/safer* than the current `finish()`/`read_to_end` framing, and gives
total event ordering QUIC uni-streams lacked.

**Isolation:** per-connection scoping isn't cosmetic — with a shared id space a
stale frame from an old tab carrying `id=5` could complete a different
connection's pending `id=5`. Per-connection maps + per-agent binding make
cross-talk structurally impossible.

---

## Key files

- `packages/mcp/src/main.rs` — listeners/args (loses cert + browser-port).
- `packages/mcp/src/quic.rs`, `cert.rs` — **deleted** (Phase 2).
- `packages/mcp/src/link.rs` — Session→ws-mux rewrite; per-connection scoping;
  agent→connection binding; 120 s request timeout.
- `packages/mcp/src/http.rs` — `/editor` ws upgrade; `/renders/*`; drop `/control`.
- `packages/mcp/src/mcp.rs` — `wav()` consumes `RenderHandle`; per-session binding
  + pairing-required tool error.
- `packages/crates/editor-protocol/src/transport.rs` — `RenderHandle`,
  `Response::Render`, `WsServerMsg`/`WsClientMsg`.
- `packages/frontend/editor/src/remote.rs` — ws client; render POST; wire events;
  pairing.
- `packages/frontend/editor/src/ui/mcp_modal.rs` — pairing-code field.
- `packages/mcp/Cargo.toml` — drop web-transport/rustls/rcgen/sha2/time; +axum `ws`.
- `packages/frontend/editor/Cargo.toml` — drop web-transport/url; +gloo-net `websocket`.
- `taskfiles/config.yml`, `taskfiles/mcp.yml` — drop the QUIC port.

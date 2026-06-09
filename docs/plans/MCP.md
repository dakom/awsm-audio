# Plan: MCP server for awsm-audio

A start-to-finish, self-contained plan to give **awsm-audio** the same MCP
control surface that **awsm-renderer** already has: an AI agent (or any MCP
client) drives the in-browser editor over a local WebTransport link.

This document is the spec. Follow the phases in order; each ends with a build
gate you must pass before moving on. Where a file is a near-verbatim port of an
awsm-renderer file, the full source is inlined here so you never need to open the
sibling repo — but the canonical reference lives at
`../awsm-renderer/packages/mcp/` and `../awsm-renderer/packages/crates/editor-protocol/`
if anything is ambiguous.

---

## 0. Architecture & how it mirrors awsm-renderer

### The topology (identical to the renderer)

```
┌─────────────┐   stdio/HTTP    ┌──────────────────────────┐   WebTransport (QUIC)   ┌────────────────────┐
│  MCP client │ ───────────────▶│  native MCP server       │ ◀────── dials out ──────│  browser editor    │
│  (Claude)   │   rmcp /mcp     │  (tokio + quinn + rmcp +  │                         │  (WASM)            │
│             │                 │   axum + rcgen)           │  server open_bi:        │                    │
│             │                 │                           │   Request → Response    │  EditorController   │
└─────────────┘                 │  packages/mcp            │  editor open_uni:       │  (the audio truth) │
                                └──────────────────────────┘   EditorEvent (push)    └────────────────────┘
                                          ▲                                                     ▲
                                          │           shared, pure-data vocabulary             │
                                          └──────── packages/crates/editor-protocol ───────────┘
                                       (EditorCommand / EditorQuery / QueryResult /
                                        Request / Response / EditorEvent + WAV types)
```

Key invariants, copied from the renderer (`../awsm-renderer/packages/mcp/src/link.rs`,
`.../remote.rs`, `.../transport.rs`):

- **The browser holds the truth.** The native server is a stateless bridge. All
  editor state lives in the WASM `EditorController` singleton. The server never
  models the document; it forwards `Request`s and relays `Response`s.
- **The editor dials out.** The server is a QUIC *listener*; the editor is the
  WebTransport *client* (`ClientBuilder::connect`). This is what lets a hosted
  HTTPS editor page reach a `localhost` server (cert-hash pinning + Private
  Network Access CORS).
- **One request per server-initiated bidi stream.** The server `open_bi`s,
  writes one JSON-encoded `Request`, `finish()`es; the editor `accept_bi`s,
  dispatches against the controller, replies with one `Response` on the *same*
  stream. **No request ids** — stream identity *is* the correlation; framing is
  by stream-finish (write whole message → `finish()`; read to end → decode).
- **Push events ride their own uni streams.** The editor `open_uni`s one
  `EditorEvent` per stream (toasts, selection); the server relays each as an MCP
  logging notification.
- **At most one editor attached.** `EditorLink = Arc<Mutex<Option<Session>>>`.
- **Wire format is JSON** (`serde_json`) end-to-end. (The renderer's comments
  mention bitcode in one place but the shipped code uses `serde_json` — we use
  JSON too: it round-trips the same serde types the editor's TOML seams already
  use, and keeps WAV/PCM debugging legible.)

### What's different here from the renderer (read before you start)

1. **`dispatch`/`query` are synchronous in awsm-audio.** The audio
   `EditorController::dispatch(cmd)` returns `()` and `query(q)` returns
   `QueryResult` directly (see `packages/frontend/editor/src/controller/mod.rs:1087`
   and `:1302`). The renderer's are `async`. So the editor-side interpreter is
   *simpler* — no `.await` on dispatch/query. The **only** async editor path is
   WAV rendering (offline bounce), which the remote handler awaits before
   replying.

2. **The vocabulary types currently live *inside* the editor crate**, not in a
   shared crate. The renderer already factored these into
   `awsm-editor-protocol`. Phase 1 is the port of that refactor: move
   `EditorCommand`, `EditorQuery`, `QueryResult`, and their data dependencies out
   of `packages/frontend/editor/` into a new `awsm-audio-editor-protocol` crate.

3. **"Read pixels" becomes "read WAV."** The renderer's readback is
   `ScenePng → Response::Png(Vec<u8>)`, surfaced as an *image* the agent sees.
   Audio's analog is rendering a Sound offline to a `.wav`. Because an agent
   can't "hear" bytes, we expose **three** readbacks (decision locked in):
   - `RenderWav` → raw `.wav` bytes (server saves to a temp file, returns path +
     summary, exactly like the renderer saves PNGs in `/debug`);
   - `WavStats` → `{ duration_secs, peak, rms, channels, sample_rate }` (the
     cheap numeric readback, analog of `canvas_stats`);
   - `Waveform` → a downsampled min/max envelope (N buckets) the agent reasons
     over in text (the "see the shape" readback).

4. **Ports.** awsm-audio deliberately lives in the 91xx block
   (`taskfiles/config.yml`: `PORT_EDITOR_DEV: 9170`) to never collide with the
   renderer (9079–9087). The MCP server uses:
   - `PORT_MCP_HTTP_DEV: 9171` (TCP — rmcp `/mcp` + `/control` + `/debug`)
   - `PORT_MCP_QUIC_DEV: 9172` (UDP — WebTransport)

### Decisions locked in (from planning Q&A)

- **Protocol crate: full typed extraction** — match the renderer; the new crate
  owns the typed vocabulary, the editor keeps the interpreter.
- **Connect UX: full parity** — `?mcp=<origin>` auto-connect **and** a top-bar
  MCP button + connect/disconnect modal + status toasts.
- **WAV readback: all three** — `RenderWav` bytes + `WavStats` + `Waveform`.

### Verification strategy (read before you start)

This work is split across a **native** half (the protocol crate + the MCP
server) and a **wasm/browser** half (the editor's `remote.rs` + UI). They verify
very differently, and the live end-to-end round-trip **cannot** run unattended in
a terminal (WebTransport with `serverCertificateHashes` requires a real Chrome
tab, hand-attached). So:

- **Authoritative gate at every phase boundary: `task lint`** — it runs
  `cargo fmt --all -- --check` **and** `cargo clippy --all --all-features --tests
  -D warnings`, which type-checks the *entire* workspace (including the wasm
  crates, compiled for the host target, and all tests). This is the real gate.
  Do **not** treat `cargo build` as the gate.
- **Native `cargo test`** wherever the logic is pure: the protocol crate
  (serde round-trip / wire-shape tests — see §1.6), the cert
  (`GeneratedCert::new` + hash), and any pure WAV-math helpers you can keep
  native. Write these as you go; they're the bulk of your unattended coverage.
- **Headless server check is real and in-scope**: the MCP server boots and serves
  `GET /control` *without any editor attached* — that's curl-able unattended
  (§2.8). Keep it as a live gate.
- **Browser / MCP-client / WAV-audio round-trips are deferred** to a morning
  checklist. Do **not** attempt to launch a browser or attach an MCP client
  unattended. Instead, write the exact steps into `docs/plans/MCP-STATUS.md`
  (§7) for a human to run in the morning, and note in that file what *is*
  natively covered vs. what still needs the live editor.
- **Commit after each green gate** (`task lint` + the phase's native tests both
  green) on a dedicated branch. Small, frequent commits. It's fine to stop
  mid-arc — leave `MCP-STATUS.md` describing what's done, what's tested, and the
  exact next step.

---

## 1. Phase 1 — extract the shared protocol crate

Goal: a new pure-data crate `awsm-audio-editor-protocol` that compiles for both
`wasm32-unknown-unknown` (editor) and native (server), owning the
command/query/transport vocabulary. The editor keeps only the *interpreter*
(applying commands to the live controller, building snapshots). This mirrors
`../awsm-renderer/packages/crates/editor-protocol/src/lib.rs`.

### 1.1 Create the crate

`packages/crates/editor-protocol/Cargo.toml`:

```toml
[package]
name = "awsm-audio-editor-protocol"
description = "Shared serializable command/query/transport vocabulary for driving the awsm-audio editor over MCP/WebTransport."
version.workspace = true
edition.workspace = true
license.workspace = true
authors.workspace = true
rust-version.workspace = true
repository.workspace = true

[dependencies]
awsm-audio-schema = { workspace = true }
serde = { workspace = true }
uuid = { workspace = true }            # ConnId = Uuid
```

Add to root `Cargo.toml`:

- `members`: add `"packages/crates/editor-protocol"`.
- `[workspace.dependencies]`: add
  `awsm-audio-editor-protocol = { path = "packages/crates/editor-protocol", version = "0.1.0" }`
  (in the internal-crates block alongside `awsm-audio-schema`).
- Confirm `uuid` is a workspace dep with `features = ["v4"]` (the schema crate
  already uses it — promote it to `[workspace.dependencies]` if it isn't there
  yet, then have schema use `{ workspace = true }`).

> ⚠️ **rustfmt:** per the repo's `rustfmt-drift` memory, do **not** run a broad
> `cargo fmt`. Format only the new files you create (`rustfmt path/to/file.rs`)
> or rely on your editor's per-file formatting. The repo's `task lint` runs
> `cargo fmt --all -- --check`; keep new files already-formatted so it passes
> without rewrapping existing code.

### 1.2 Move the vocabulary types into the crate

The audio vocabulary today is spread across the editor crate. Move the
**pure-data** pieces; leave the interpreter behind. Source → destination:

| Type(s) | Currently in | Move to |
|---|---|---|
| `EditorCommand`, `SongOp`, `ControlOp`, `ArrangeOp`, `PlacedClip` | `controller/command.rs` | `editor-protocol/src/command.rs` |
| `EditorQuery`, `QueryResult`, `SampleInfo`, `AssetInfo`, `TransportInfo` | `controller/command.rs` | `editor-protocol/src/query.rs` |
| `EditorSnapshot`, `EditorProject`, `NodeLayout` | `controller/snapshot.rs` | `editor-protocol/src/snapshot.rs` |
| `FieldValue` (the enum **only**) | `fields.rs:40` | `editor-protocol/src/field.rs` |
| `BoundaryPort` (enum), `ConnId` (`= Uuid`) | `controller/node.rs:41,83` | `editor-protocol/src/node.rs` |
| `Clipboard` | `controller/mod.rs:259` | `editor-protocol/src/clipboard.rs` |

Notes / gotchas while moving:

- **`FieldValue`** (`fields.rs:40`) is `enum { Num(f64), Text(String), Bool(bool) }`
  — trivially pure. Leave the *non-serializable* siblings (`Control`, `Field`,
  the `fn num/.../audio_params` builders) in `fields.rs`; only `FieldValue`
  moves. `fields.rs` then does `use awsm_audio_editor_protocol::FieldValue;`.
- **`BoundaryPort` + `ConnId`** (`controller/node.rs`) move; the *live* node
  structs in `node.rs` (the ones holding `Mutable`/reactive fields) **stay**.
  Re-export from the protocol crate and `use` them back in `node.rs`.
- **`Clipboard`** (`controller/mod.rs:259`) — inspect its fields; it's the paste
  payload, so it must already be serde-serializable. Move the struct + any small
  helper structs it contains (e.g. clipboard node/edge records). If it
  references live node types, introduce a serde-friendly mirror (it should
  already be one, since `EditorCommand::Paste { clip: Clipboard }` round-trips
  through TOML today).
- **`EditorSnapshot` / `EditorProject` / `NodeLayout`** (`controller/snapshot.rs`)
  are pure data over `awsm_audio_schema` (`Graph`, `SampleLibrary`,
  `Arrangement`, `NodeId`) — move the **struct definitions** to the protocol
  crate. The `impl EditorController { fn snapshot(), fn to_project(), ... }`
  blocks in `snapshot.rs` that *build* these stay in the editor (they reference
  controller internals). Keep `serde(default)` / `default = "one"` attributes
  intact; move the tiny `fn one() -> f64 { 1.0 }` helper alongside the structs.
- **Serde attributes are load-bearing** — preserve them exactly. The TOML seams
  in `main.rs` depend on:
  - `EditorCommand`: `#[serde(rename_all = "snake_case", tag = "cmd", content = "args")]`
  - `SongOp`/`ControlOp`/`ArrangeOp`: `tag = "op", content = "args"`
  - `EditorQuery`: `tag = "query", content = "args"`
  - `QueryResult`: `tag = "result", content = "data"`
- `QueryResult` currently derives only `Serialize` (it's a reply). For the wire
  it must also `Deserialize` so the **server** can decode it. Add `Deserialize`
  to `QueryResult` and the structs it contains (`SampleInfo`, `AssetInfo`,
  `TransportInfo`) — and `EditorSnapshot`/`EditorProject` already derive both.

### 1.3 Add the transport envelopes + WAV types

`editor-protocol/src/transport.rs` (port of the renderer's `transport.rs`,
adapted for audio):

```rust
//! The request/response envelope exchanged over the WebTransport link between
//! the native MCP server and the in-browser editor. One request per
//! server-initiated bidi stream; the editor replies on the same stream. No
//! request-id correlation — stream identity is the correlation, framing is by
//! stream-finish. JSON-encoded.

use serde::{Deserialize, Serialize};

use awsm_audio_schema::SampleId;

use crate::{EditorCommand, EditorQuery, QueryResult};

/// Server → editor. What the editor should do / report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Apply a mutation through `EditorController::dispatch`.
    Dispatch(EditorCommand),
    /// Apply a list of mutations in order (one round-trip). awsm-audio has no
    /// batch-undo API yet, so the editor applies these sequentially.
    DispatchBatch(Vec<EditorCommand>),
    /// Run a read-only `EditorQuery`.
    Query(EditorQuery),
    /// Transport control (the `editor_play`/`editor_stop` seams).
    Play,
    Stop,
    /// Render a Sound offline to a `.wav` (raw bytes). `sample = None` renders
    /// the project root. Optional `sample_rate` overrides the bounce rate.
    RenderWav {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample: Option<SampleId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sample_rate: Option<f32>,
    },
}

/// Editor → server **push** event (unsolicited channel). One per uni stream.
/// Relayed to the agent as an MCP logging notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EditorEvent {
    /// `"toast"` | `"selection"` | `"transport"`.
    pub kind: String,
    /// Toast severity (`"info"` | `"warning"` | `"error"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Selected node ids for `kind == "selection"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nodes: Option<Vec<String>>,
}

/// Editor → server. The reply to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// A mutation / control op succeeded with no payload.
    Ok,
    /// A query result (boxed — `QueryResult::Snapshot` is large).
    Query(Box<QueryResult>),
    /// Raw `.wav` file bytes (RIFF/WAVE container).
    Wav(Vec<u8>),
    /// The request failed; the string is a human-readable reason.
    Err(String),
}
```

Add the WAV readback variants to **`EditorQuery`** and **`QueryResult`**
(`editor-protocol/src/query.rs`). These are pure-numeric, computed in the editor
from rendered PCM:

```rust
// in EditorQuery:
    /// Cheap numeric stats of a Sound's offline render.
    WavStats { #[serde(default)] sample: Option<SampleId> },
    /// A downsampled min/max envelope (`buckets` columns) of a Sound's render,
    /// so an agent can reason about the waveform shape in text.
    Waveform { #[serde(default)] sample: Option<SampleId>, buckets: u32 },

// in QueryResult:
    WavStats(WavStats),
    Waveform(WaveformEnvelope),
```

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WavStats {
    pub duration_secs: f64,
    pub peak: f32,
    pub rms: f32,
    pub channels: u32,
    pub sample_rate: u32,
}

/// Per-bucket min/max of a mono-summed render, normalized to [-1, 1].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformEnvelope {
    pub sample_rate: u32,
    pub duration_secs: f64,
    /// `min[i] <= max[i]`, one pair per bucket, left-to-right in time.
    pub min: Vec<f32>,
    pub max: Vec<f32>,
}
```

> `WavStats`/`Waveform` are queries (not `RenderWav`) because they return small
> structured data through the existing `QueryResult` path. `RenderWav` returns
> bytes through `Response::Wav`. All three internally call the same offline
> render (§3.4).

### 1.4 `editor-protocol/src/lib.rs`

Mirror the renderer's re-export hub
(`../awsm-renderer/packages/crates/editor-protocol/src/lib.rs`):

```rust
//! Shared, serializable command/query/transport vocabulary for driving the
//! awsm-audio editor remotely (MCP / WebTransport) and from headless tests.
//!
//! Pure data — no DOM, audio, reactive, or async deps — so it compiles for both
//! the editor's wasm target and the native MCP server. Heavy payloads (the audio
//! graph, samples, arrangement) live in `awsm_audio_schema`, which this crate
//! re-exports so callers have one import path. This crate is the *vocabulary*;
//! the editor crate is the *interpreter*.

mod clipboard;
mod command;
mod field;
mod node;
mod query;
mod snapshot;
mod transport;

pub use awsm_audio_schema as schema;

pub use clipboard::Clipboard;
pub use command::{ArrangeOp, ControlOp, EditorCommand, PlacedClip, SongOp};
pub use field::FieldValue;
pub use node::{BoundaryPort, ConnId};
pub use query::{
    AssetInfo, EditorQuery, QueryResult, SampleInfo, TransportInfo, WavStats, WaveformEnvelope,
};
pub use snapshot::{EditorProject, EditorSnapshot, NodeLayout};
pub use transport::{EditorEvent, Request, Response};
```

### 1.5 Rewire the editor crate to import from the protocol crate

In `packages/frontend/editor/Cargo.toml` add:
`awsm-audio-editor-protocol = { workspace = true }`.

Then make the editor *re-export* the moved types from where the rest of the code
already imports them, so the change is mostly mechanical:

- `controller/command.rs` becomes a thin re-export module:
  `pub use awsm_audio_editor_protocol::{EditorCommand, EditorQuery, QueryResult, SongOp, ControlOp, ArrangeOp, PlacedClip, SampleInfo, AssetInfo, TransportInfo};`
  (keep the file so `controller::EditorCommand` paths in `main.rs` still resolve;
  alternatively update the `pub use` in `controller/mod.rs`).
- `controller/snapshot.rs`: delete the struct defs, add
  `pub use awsm_audio_editor_protocol::{EditorSnapshot, EditorProject, NodeLayout};`
  and **keep** the `impl EditorController { fn snapshot()/to_project()/... }`
  blocks (they now build the imported structs).
- `fields.rs`: `pub use awsm_audio_editor_protocol::FieldValue;` (remove the local
  `enum FieldValue`).
- `controller/node.rs`: `pub use awsm_audio_editor_protocol::{BoundaryPort, ConnId};`
  (remove local defs).
- `controller/mod.rs`: `pub use awsm_audio_editor_protocol::Clipboard;` (remove
  local def); fix the `Clipboard` construction sites to use the moved type.
- Audit every `use crate::fields::FieldValue`, `use super::node::{BoundaryPort, ConnId}`,
  etc. — they should resolve through the re-exports without edits. Where a site
  imported a type *and* an interpreter helper from the same module, split the
  imports.

The `main.rs` TOML seams (`editor_dispatch_toml`, `editor_query_toml`,
`editor_snapshot_toml`, `editor_open_project_toml`) keep working verbatim because
the types they name (`controller::EditorCommand`, etc.) still resolve via the
re-exports, and the serde tags are unchanged.

### 1.6 Native tests + build gate

Add serde round-trip tests in `editor-protocol/src/` (these are your unattended
coverage that the wire shapes are stable and that `QueryResult` decodes):

- `Request`/`Response` round-trip for each variant incl. `RenderWav`,
  `Response::Wav`, `Response::Query(QueryResult::*)`.
- `EditorCommand` round-trips through **both** JSON and TOML (the editor's
  `editor_dispatch_toml` seam depends on the TOML form), for a representative
  spread: `AddNode`, `Connect`, `SetField { value: FieldValue::Num(..) }`,
  `EditSong { op: SongOp::AddNote { .. } }`, `EditArrange { op: ArrangeOp::AddClip { .. } }`,
  `Paste { clip: Clipboard }`.
- `EditorQuery`/`QueryResult` round-trip incl. the new `WavStats`/`Waveform`.

Gate:

```
cargo test  -p awsm-audio-editor-protocol                          # serde round-trips green
task lint                                                          # AUTHORITATIVE: fmt --check + clippy --all --tests -D warnings
cargo build -p awsm-audio-editor --target wasm32-unknown-unknown   # confirm the editor still compiles to wasm (not browser-run)
```

`task lint` is the real gate (it type-checks the whole workspace). Fix only
*new*-file formatting — don't let it rewrap existing files (see the rustfmt
warning above). Commit when green.

---

## 2. Phase 2 — the native MCP server crate

Goal: `packages/mcp` — a `tokio` binary that (a) generates a self-signed cert,
(b) listens for the editor's WebTransport connection, (c) serves the rmcp `/mcp`
endpoint + a `/control` cert-hash discovery endpoint + a `/debug` raw-request
seam. This is a near-verbatim port of `../awsm-renderer/packages/mcp/`.

### 2.1 `packages/mcp/Cargo.toml`

```toml
[package]
name = "awsm-audio-mcp"
description = "Native MCP server for the awsm-audio editor (WebTransport link + rmcp)."
version.workspace = true
edition.workspace = true       # NOTE: see rust-version bump below
license.workspace = true
authors.workspace = true
repository.workspace = true

[[bin]]
name = "awsm-audio-mcp"
path = "src/main.rs"

[dependencies]
awsm-audio-editor-protocol = { workspace = true }
awsm-audio-schema = { workspace = true }

tokio = { version = "1.47", features = ["full"] }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
serde = { workspace = true }
serde_json = { workspace = true }

# WebTransport (QUIC) — quinn on native, self-signed dev cert.
web-transport = "0.10.5"
rustls = "0.23"
rcgen = "0.14"
sha2 = "0.10"
base64 = { workspace = true }
time = { version = "0.3", features = ["formatting"] }

# HTTP control surface.
axum = "0.8"
tower-http = { version = "0.6", features = ["cors"] }

# The rmcp /mcp endpoint.
rmcp = { version = "1", features = ["server", "macros", "schemars", "transport-streamable-http-server"] }
uuid = { workspace = true }
```

Add `"packages/mcp"` to root `members`.

> ⚠️ **MSRV bump.** rmcp 1.x requires edition-2024 / Rust ≥ 1.85. The audio
> workspace currently sets `rust-version = "1.80.0"` and `edition = "2021"` at
> the workspace level. **Do not** change the workspace defaults (the wasm crates
> are fine on 2021). Instead, in `packages/mcp/Cargo.toml` set crate-local
> overrides: `edition = "2024"` and `rust-version = "1.85"` (literal, not
> `.workspace = true`). The renderer's MCP crate does exactly this; its workspace
> root sets 1.85 globally, but here we scope it to the one native crate so the
> wasm side keeps its lower MSRV.
>
> The `panic = 'abort'` release profile and `lto = true` are fine for a native
> binary. The wasm-only `[patch.crates-io] dominator` patch does not affect this
> crate.

### 2.2 `packages/mcp/src/cert.rs`

Verbatim port (`../awsm-renderer/packages/mcp/src/cert.rs`). Generates a P-256
self-signed cert (10-day validity — WebTransport requires ≤ 14 days for
`serverCertificateHashes`), in-memory only, and exposes the base64url SHA-256 of
the DER for the browser to pin.

```rust
use anyhow::{Context, Result};
use base64::Engine;
use rcgen::{
    CertificateParams, DistinguishedName, DnType, KeyPair, PKCS_ECDSA_P256_SHA256,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

pub struct GeneratedCert {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
}

impl GeneratedCert {
    pub fn new(hostname: &str) -> Result<Self> {
        let key_pair =
            KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).context("generate P-256 key pair")?;
        let mut dname = DistinguishedName::new();
        dname.push(DnType::CommonName, "awsm-audio-mcp self-signed");
        let mut params =
            CertificateParams::new(vec![hostname.to_string()]).context("certificate params")?;
        params.distinguished_name = dname;
        let now = time::OffsetDateTime::now_utc();
        params.not_before = now;
        params.not_after = now + time::Duration::days(10);
        let cert = params.self_signed(&key_pair).context("self-sign certificate")?;
        Ok(Self {
            cert_der: cert.der().to_vec(),
            key_der: key_pair.serialize_der(),
        })
    }

    /// `base64url(SHA-256(DER))` — the value the browser pins.
    pub fn hash_base64url(&self) -> String {
        let digest = Sha256::digest(&self.cert_der);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
    }

    pub fn rustls_cert(&self) -> CertificateDer<'static> {
        CertificateDer::from(self.cert_der.clone())
    }

    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::from(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}
```

### 2.3 `packages/mcp/src/quic.rs`

Verbatim port (`../awsm-renderer/packages/mcp/src/quic.rs`), swapping the crate
names: `use awsm_audio_editor_protocol::{EditorEvent, Request};`. Builds a
TLS-1.3 WebTransport-ALPN QUIC endpoint, accepts editor sessions, installs each
as the active link, spawns the uni-stream event reader, and proves the round-trip
with one probe request on attach. **Use `Request::Stop`** (or any cheap request)
for the attach probe instead of the renderer's `Request::Mode`. Full source:

```rust
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use web_transport::quinn::quinn::{
    crypto::rustls::QuicServerConfig, Endpoint, Incoming, ServerConfig,
};
use web_transport::Session;

use awsm_audio_editor_protocol::EditorEvent;

use crate::cert::GeneratedCert;
use crate::link::{self, EditorLink};

pub fn build_endpoint(cert: &GeneratedCert, addr: SocketAddr) -> Result<Endpoint> {
    let mut tls = rustls::ServerConfig::builder_with_provider(
        web_transport::quinn::crypto::default_provider(),
    )
    .with_protocol_versions(&[&rustls::version::TLS13])
    .context("set TLS 1.3")?
    .with_no_client_auth()
    .with_single_cert(vec![cert.rustls_cert()], cert.rustls_key())
    .context("install single cert")?;
    tls.alpn_protocols = vec![web_transport::quinn::ALPN.as_bytes().to_vec()];
    tls.max_early_data_size = u32::MAX;

    let qsc: QuicServerConfig = tls.try_into().context("build QUIC server config")?;
    let server_config = ServerConfig::with_crypto(Arc::new(qsc));
    let endpoint = Endpoint::server(server_config, addr).context("bind QUIC endpoint")?;
    Ok(endpoint)
}

pub async fn accept_loop(endpoint: Endpoint, link: EditorLink) {
    loop {
        let Some(incoming) = endpoint.accept().await else {
            tracing::warn!("QUIC endpoint closed");
            break;
        };
        let link = link.clone();
        tokio::spawn(async move {
            match accept_session(incoming).await {
                Ok(session) => {
                    tracing::info!("editor attached");
                    link.set(Some(session.clone())).await;
                    tokio::spawn(read_event_stream(session.clone(), link.clone()));
                }
                Err(e) => tracing::error!("accept failed: {e:#}"),
            }
        });
    }
}

async fn read_event_stream(session: Session, link: EditorLink) {
    loop {
        let mut recv = match session.accept_uni().await {
            Ok(recv) => recv,
            Err(e) => {
                tracing::debug!("editor push channel closed: {e}");
                break;
            }
        };
        let mut buf = Vec::new();
        let ok = loop {
            match recv.read(64 * 1024).await {
                Ok(Some(chunk)) => {
                    buf.extend_from_slice(&chunk);
                    if buf.len() > 1024 * 1024 {
                        tracing::warn!("editor event exceeded 1 MiB; dropping");
                        break false;
                    }
                }
                Ok(None) => break true,
                Err(e) => {
                    tracing::debug!("editor event read error: {e}");
                    break false;
                }
            }
        };
        if !ok {
            continue;
        }
        match serde_json::from_slice::<EditorEvent>(&buf) {
            Ok(ev) => link.publish_event(ev),
            Err(e) => tracing::warn!("bad editor event: {e}"),
        }
    }
}

async fn accept_session(incoming: Incoming) -> Result<Session> {
    let conn = incoming.await.context("await incoming connection")?;
    let req = web_transport::quinn::Request::accept(conn)
        .await
        .context("WebTransport handshake")?;
    let session = req.ok().await.context("WebTransport session")?;
    Ok(session.into())
}
```

(Note: `link` import kept for `EditorLink`; drop the `link::request` probe the
renderer had, or keep a `Request::Stop` probe — either is fine.)

### 2.4 `packages/mcp/src/link.rs`

Verbatim port (`../awsm-renderer/packages/mcp/src/link.rs`), swapping crate names
to `awsm_audio_editor_protocol::{EditorEvent, Request, Response}`. Holds the
single attached `Session`, a `broadcast::Sender<EditorEvent>` fan-out, and the
`request(&Request) -> Response` exchange (open_bi → write JSON → finish → read to
end → decode). Keep `MAX_RESPONSE_BYTES = 64 * 1024 * 1024` (WAV renders can be
multi-MB). The body is identical to the renderer's — copy it and change only the
`use` line.

### 2.5 `packages/mcp/src/http.rs`

Port of `../awsm-renderer/packages/mcp/src/http.rs`. The axum router with:

- `GET /control` → `{ "quic_url": "https://127.0.0.1:<quic_port>", "cert_hash": "<base64url>" }`
- `POST /debug` → relay a raw JSON `Request` to the editor, return the
  `Response` as JSON; for `Response::Wav(bytes)` write to a temp file and return
  `{ "Wav": { "bytes": N, "saved": true, "path": "/tmp/awsm-audio-mcp-last.wav" } }`
  (the audio analog of the renderer's PNG temp-file handling).
- `nest_service("/mcp", mcp_service)` — the rmcp `StreamableHttpService` built
  with `move || Ok(EditorMcp::new(mcp_link.clone()))`,
  `LocalSessionManager::default().into()`, `StreamableHttpServerConfig::default()`.
- CORS: `allow_origin(Any)`, `allow_methods(Any)`, `allow_headers(Any)`,
  **`allow_private_network(true)`** (required so a hosted HTTPS editor page can
  reach the loopback server — Chrome's Private Network Access preflight).

Swap the `/debug` PNG arm for a WAV arm:

```rust
async fn debug(State(s): State<AppState>, Json(req): Json<Request>) -> Json<Value> {
    match s.link.request(&req).await {
        Ok(Response::Wav(bytes)) => {
            let path = std::env::temp_dir().join("awsm-audio-mcp-last.wav");
            let saved = std::fs::write(&path, &bytes).is_ok();
            Json(json!({ "Wav": { "bytes": bytes.len(), "saved": saved, "path": path.to_string_lossy() } }))
        }
        Ok(resp) => Json(serde_json::to_value(&resp).unwrap_or_else(|e| json!({ "encode_error": e.to_string() }))),
        Err(e) => Json(json!({ "error": e.to_string() })),
    }
}
```

### 2.6 `packages/mcp/src/mcp.rs` — the rmcp tool layer

Port the structure of `../awsm-renderer/packages/mcp/src/mcp.rs` but with
*audio* tools. The skeleton (helpers are identical):

```rust
#[derive(Clone)]
pub struct EditorMcp {
    link: EditorLink,
    tool_router: ToolRouter<EditorMcp>,
}

#[tool_router]
impl EditorMcp {
    pub fn new(link: EditorLink) -> Self {
        Self { link, tool_router: Self::tool_router() }
    }

    // ── helpers (verbatim from the renderer) ──
    async fn req(&self, r: Request) -> Result<Response, McpError> {
        self.link.request(&r).await.map_err(|e| McpError::internal_error(e.to_string(), None))
    }
    async fn dispatch(&self, cmd: EditorCommand) -> Result<CallToolResult, McpError> {
        match self.req(Request::Dispatch(cmd)).await? {
            Response::Ok => Ok(text("ok")),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }
    async fn query(&self, q: EditorQuery) -> Result<CallToolResult, McpError> {
        match self.req(Request::Query(q)).await? {
            Response::Query(qr) => Ok(text(
                serde_json::to_string_pretty(&*qr).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}")),
            )),
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }
    /// RenderWav → save the `.wav` to a temp file, return the path + a one-line
    /// summary. (An agent can't hear bytes; the human/tooling opens the file.)
    async fn wav(&self, r: Request) -> Result<CallToolResult, McpError> {
        match self.req(r).await? {
            Response::Wav(bytes) => {
                let path = std::env::temp_dir().join("awsm-audio-mcp-last.wav");
                std::fs::write(&path, &bytes)
                    .map_err(|e| McpError::internal_error(format!("write wav: {e}"), None))?;
                Ok(text(format!("wrote {} bytes to {}", bytes.len(), path.display())))
            }
            Response::Err(e) => Err(McpError::internal_error(e, None)),
            other => Err(unexpected(other)),
        }
    }
}

fn text(s: impl Into<String>) -> CallToolResult { CallToolResult::success(vec![Content::text(s.into())]) }
fn unexpected(resp: Response) -> McpError { McpError::internal_error(format!("unexpected response: {resp:?}"), None) }
```

Tools to expose (each is a thin `#[tool]` wrapper that builds an `EditorCommand`
/ `EditorQuery` and calls a helper). Use rmcp `Parameters<T>` structs (with
`schemars`) for arguments. Group them:

**Discovery / read**
- `get_snapshot` → `query(EditorQuery::Snapshot)`
- `list_samples` → `query(EditorQuery::Samples)`
- `list_assets` → `query(EditorQuery::Assets)`
- `get_arrangement` → `query(EditorQuery::Arrangement)`
- `get_transport` → `query(EditorQuery::Transport)`
- `get_bounce_status` → `query(EditorQuery::BounceStatus { sample })`

**Audio readback (the WAV surface)**
- `render_wav { sample?, sample_rate? }` → `wav(Request::RenderWav { .. })`
- `wav_stats { sample? }` → `query(EditorQuery::WavStats { sample })`
- `waveform { sample?, buckets }` → `query(EditorQuery::Waveform { sample, buckets })`

**Transport**
- `play` → `req(Request::Play)` → ok
- `stop` → `req(Request::Stop)` → ok

**Graph mutation** (one per `EditorCommand`, or a subset + escape hatches)
- `add_node { kind, x, y }`, `remove_node { id }`, `clone_node { id }`,
  `move_node`, `rename_node`, `set_field { id, key, value }`,
  `set_automation`, `connect`, `modulate`, `bind`, `disconnect`,
  `add_sample { kind }`, `remove_sample`, `clone_sample`, `rename_sample`,
  `set_root`, `add_boundary`, `add_sample_ref`, `set_sample_ref`,
  `set_input_default`, `set_input_value`, `set_listener`, `bounce { sample }`.
- Sequencer/arrangement sub-edits: expose `edit_song { node, op }`,
  `edit_control { node, op }`, `edit_arrange { op }` taking the typed `SongOp`/
  `ControlOp`/`ArrangeOp` (let schemars derive the arg schema from the protocol
  enums), **plus** the escape hatches below.

**Escape hatches** (so the agent is never blocked on a missing typed tool)
- `dispatch_command { command }` — take a JSON `EditorCommand`, dispatch it.
- `dispatch_batch { commands }` — `Request::DispatchBatch`.
- `run_query { query }` — take a JSON `EditorQuery`, return the `QueryResult`.

> Pragmatic scope note: you can ship Phase 2 with the **escape hatches + the
> readback tools + transport** first (that alone makes the editor fully
> drivable), then add the ergonomic typed wrappers. Don't block the build on
> having all ~30 wrappers.

**`ServerHandler`** (via `#[tool_handler]`): port the renderer's `get_info`
(build `ServerInfo` from `Default`, enable tools/resources/prompts, set
`instructions` to an audio-flavored blurb: "Drive the awsm-audio node-graph
WebAudio editor. Call get_snapshot to discover node/sample ids, mutate with the
graph/sequencer/arrangement tools (or dispatch_command), bounce a Sound and call
render_wav / wav_stats / waveform to inspect the result."). Implement
`on_initialized` to forward `EditorEvent`s (subscribe via
`link.subscribe_events()`) as MCP logging notifications — same shape as the
renderer. `list_resources`/`read_resource`/`list_prompts`/`get_prompt` are
optional; stub or port a couple of audio recipes later.

### 2.7 `packages/mcp/src/main.rs`

Port of `../awsm-renderer/packages/mcp/src/main.rs`:

```rust
mod cert;
mod http;
mod link;
mod mcp;
mod quic;

use std::net::{Ipv6Addr, SocketAddr};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::cert::GeneratedCert;
use crate::link::EditorLink;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,awsm_audio_mcp=debug".into()),
        )
        .init();

    // rustls needs a process-wide default crypto provider for quinn's TLS.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let args = parse_args(); // --http-port 9171 --quic-port 9172

    let cert = Arc::new(GeneratedCert::new("localhost").context("generate dev cert")?);
    tracing::info!("dev cert hash (base64url): {}", cert.hash_base64url());

    let link = EditorLink::shared();

    let quic_addr = SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), args.quic_port);
    let endpoint = quic::build_endpoint(&cert, quic_addr).context("build QUIC endpoint")?;
    tracing::info!("WebTransport (QUIC) listening on udp/{}", args.quic_port);
    tokio::spawn(quic::accept_loop(endpoint, link.clone()));

    let http_addr = SocketAddr::from(([127, 0, 0, 1], args.http_port));
    http::serve(http_addr, cert, args.quic_port, link)
        .await
        .context("control http server")?;
    Ok(())
}
```

Include the renderer's tiny `parse_args()` (`--http-port` default `9171`,
`--quic-port` default `9172`) and `Args` struct.

### 2.8 Native tests + headless gate

Add a native test for the cert (`GeneratedCert::new("localhost")` succeeds and
`hash_base64url()` is non-empty, valid base64url, decoding to 32 bytes).

The server's boot + `/control` is genuinely unattended-runnable (no browser, no
editor needed) — keep it as a live gate. Run the server in the background, curl,
then kill it:

```
cargo test  -p awsm-audio-mcp                                 # cert test green
task lint                                                            # AUTHORITATIVE
cargo run   -p awsm-audio-mcp -- --http-port 9171 --quic-port 9172 &   # background
sleep 2
curl -s http://127.0.0.1:9171/control     # → {"quic_url":"https://127.0.0.1:9172","cert_hash":"..."}
kill %1
```

The server boots, logs the cert hash + "listening on udp/9172", and `/control`
returns the URL + hash. Anything past this (attaching an editor, `/debug`
round-trips) needs the browser → **morning checklist (§7)**. Commit when green.

---

## 3. Phase 3 — the editor's WebTransport client (`remote.rs`)

Goal: the editor dials out to the server, serves its `Request`s against the live
`EditorController`, and pushes `EditorEvent`s. Port of
`../awsm-renderer/packages/frontend/editor/src/remote.rs`.

### 3.1 Add deps to `packages/frontend/editor/Cargo.toml`

```toml
web-transport = "0.10.5"
base64 = { workspace = true }       # already present
gloo-net = { version = "0.6", features = ["http"] }
url = "2"
serde = { workspace = true }        # already present
serde_json = { workspace = true }   # add if not present
```

### 3.2 `packages/frontend/editor/src/remote.rs`

Port the renderer's `remote.rs` (control-info fetch → cert-hash decode →
`ClientBuilder::with_server_certificate_hashes` → `connect` → accept_bi loop).
The framing helpers (`read_request`, `reply`, `send_event`) are verbatim. The
**only** awsm-audio-specific part is `dispatch(req)` (§3.3). Default origin:

```rust
pub fn default_origin() -> &'static str {
    option_env!("MCP_DEFAULT_ORIGIN").unwrap_or("http://127.0.0.1:9171")
}
```

Add `mod remote;` to `main.rs`.

### 3.3 The dispatch interpreter (awsm-audio specific — sync controller)

```rust
use awsm_audio_editor_protocol::{Request, Response};
use crate::controller::controller;

fn dispatch(req: Request) -> Response {
    let ctrl = controller();
    match req {
        Request::Dispatch(cmd) => { ctrl.dispatch(cmd); Response::Ok }       // sync, returns ()
        Request::DispatchBatch(cmds) => { for c in cmds { ctrl.dispatch(c); } Response::Ok }
        Request::Query(q) => Response::Query(Box::new(ctrl.query(q))),       // sync, returns QueryResult
        Request::Play => { ctrl.play(); Response::Ok }
        Request::Stop => { ctrl.stop(); Response::Ok }
        Request::RenderWav { .. } => unreachable!("handled on the async path"),
    }
}
```

Because `RenderWav` (and `WavStats`/`Waveform`) must **await** an offline render,
`serve_one` splits the path: cheap requests go through the sync `dispatch` above;
the WAV requests go through an async branch:

```rust
async fn serve_one(mut send: SendStream, mut recv: RecvStream) {
    let resp = match read_request(&mut recv).await {
        Ok(Request::RenderWav { sample, sample_rate }) =>
            render_wav(sample, sample_rate).await,
        Ok(Request::Query(q @ (EditorQuery::WavStats { .. } | EditorQuery::Waveform { .. }))) =>
            render_query(q).await,          // see §3.4
        Ok(req) => dispatch(req),           // sync
        Err(e) => Response::Err(e),
    };
    if let Err(e) = reply(&mut send, &resp).await { tracing::warn!("mcp: reply failed: {e}"); }
}
```

(Match the two WAV-query variants explicitly so the rest of `EditorQuery` still
flows through the sync `ctrl.query`.)

### 3.4 The offline-render path (reuse the existing bounce machinery)

The controller already renders Sounds offline:
`bounce_job_for(id) -> Option<(BounceJob, String)>`
(`controller/mod.rs:3069`), `awsm_audio_player::bounce::render(job).await ->
Result<(Vec<Vec<f32>>, u32)>` (`player/src/bounce.rs:41`), and
`export_active_wav` (`controller/mod.rs:3178`) already chains render +
`util::encode_wav`. Factor a single reusable async entry on the controller:

```rust
// controller/mod.rs
/// Render a Sound (or the root when `None`) offline to PCM, without storing it.
pub async fn render_pcm(&self, sample: Option<SampleId>, sample_rate: Option<f32>)
    -> anyhow::Result<(Vec<Vec<f32>>, u32)>
{
    let id = sample.unwrap_or_else(|| self.root_sample_id());
    let (mut job, _label) = self.bounce_job_for(id).ok_or_else(|| anyhow!("nothing to render"))?;
    if let Some(sr) = sample_rate { job.sample_rate = sr; }
    awsm_audio_player::bounce::render(job).await
}
```

Then in `remote.rs`:

```rust
async fn render_wav(sample: Option<SampleId>, sr: Option<f32>) -> Response {
    match controller().render_pcm(sample, sr).await {
        Ok((channels, rate)) => Response::Wav(crate::util::encode_wav(&channels, rate)),
        Err(e) => Response::Err(format!("{e}")),
    }
}

async fn render_query(q: EditorQuery) -> Response {
    let (sample, want_waveform, buckets) = match &q {
        EditorQuery::WavStats { sample } => (*sample, false, 0),
        EditorQuery::Waveform { sample, buckets } => (*sample, true, *buckets),
        _ => unreachable!(),
    };
    match controller().render_pcm(sample, None).await {
        Ok((channels, rate)) => {
            let qr = if want_waveform { compute_waveform(&channels, rate, buckets) }
                     else            { compute_wav_stats(&channels, rate) };
            Response::Query(Box::new(qr))
        }
        Err(e) => Response::Err(format!("{e}")),
    }
}
```

`compute_wav_stats` and `compute_waveform` are small pure functions over
`Vec<Vec<f32>>` (peak = max abs across channels; rms = sqrt(mean(sq)); duration =
frames / rate; envelope = per-bucket min/max of the mono sum). Put them in
`remote.rs` or a `controller` helper. They return the new `QueryResult::WavStats`
/ `QueryResult::Waveform` variants (defined in Phase 1).

> If `bounce_job_for` / `root_sample_id` have slightly different names/signatures,
> adapt — the point is to reuse the existing offline-render path, not re-derive
> it. Grep `controller/mod.rs` for `bounce_job_for`, `bounce_sample`,
> `export_active_wav`, and the root/active sample accessors.

### 3.5 Push events (optional but in-parity)

Wire `notify_event` (verbatim from the renderer) and call it from the controller
at the points that emit toasts / selection changes, so the agent gets live
notifications. This can be a thin follow-up; the request/response path works
without it. Map: status toasts → `EditorEvent { kind: "toast", level, message }`;
selection set → `kind: "selection", nodes`.

### 3.6 Gate

Keep the pure WAV-math helpers (`compute_wav_stats`, `compute_waveform`)
native-testable if you can — e.g. put them in a small module with `#[cfg(test)]`
unit tests over synthetic `Vec<Vec<f32>>` (a 1 kHz sine → known peak ≈ 1.0, rms ≈
0.707; a ramp → monotonic envelope). If they must live in the wasm editor crate,
still add the tests (they run under `task lint`'s `--tests` type-check; they
execute when run on host since they're pure `f32` math with no web-sys).

```
cargo test  -p awsm-audio-editor-protocol        # still green
task lint                                         # AUTHORITATIVE — type-checks remote.rs + the new controller method
cargo build -p awsm-audio-editor --target wasm32-unknown-unknown   # confirm wasm compile
```

The live connect + WAV round-trip is browser-only → **morning checklist (§7)**.
Commit when green.

---

## 4. Phase 4 — editor connect UX (full parity)

Port the renderer's connect surface. In `remote.rs` (already mostly there from
§3.2): the `RemoteStatus { Disconnected, Connecting, Connected }` enum, the
thread-local `STATUS`/`ORIGIN`/`SESSION`, `status()`, `origin()`, `connect()`,
`disconnect()`, and the toast-on-connect/disconnect/error behavior — all
verbatim from the renderer (they use `Mutable`/toasts; awsm-audio already uses
`futures-signals::Mutable` and has a status line — adapt `Toast::info/error` to
the editor's existing status mechanism, e.g. `controller().status.set(Some(..))`,
or add a small toast helper).

UI:

1. **`?mcp=<origin>` auto-connect.** In `main.rs` (or a boot hook), after
   `controller::init()`, read `window().location().search()`, parse `mcp=`, and
   if present call `remote::connect(origin)`. Mirror the renderer's query-param
   parsing.
2. **Top-bar MCP button + modal.** Add a button to the editor's top bar
   (`ui/mod.rs` / wherever the toolbar lives) bound to `remote::status()`:
   - Disconnected → "MCP" button opens a connect modal pre-filled with
     `remote::origin()` (editable); on submit → `remote::connect(origin)`.
   - Connecting → spinner/disabled.
   - Connected → "MCP ✓" → click disconnects (`remote::disconnect()`).
   Reuse the editor's existing modal/widget patterns (it already has an
   example-browser modal and a help modal — copy that structure). Use the shared
   graphite/slate tokens (`var(--…)`) per the `design-system` memory.

Gate:

```
task lint                                                          # AUTHORITATIVE
cargo build -p awsm-audio-editor --target wasm32-unknown-unknown   # confirm wasm compile
```

Loading the dev server to see the button render + modal open is browser-only →
**morning checklist (§7)**. Commit when green.

---

## 5. Phase 5 — Taskfile + config wiring

### 5.1 `taskfiles/config.yml`

Add to `vars` (in the 91xx block, after `PORT_EDITOR_DEV: 9170`):

```yaml
  PORT_MCP_HTTP_DEV: 9171   # TCP — rmcp /mcp + /control + /debug
  PORT_MCP_QUIC_DEV: 9172   # UDP — WebTransport link the editor dials into

  # The default MCP server origin the editor dials out to (always local).
  URL_MCP_DEFAULT: "http://127.0.0.1:{{.PORT_MCP_HTTP_DEV}}"
```

### 5.2 `taskfiles/mcp.yml` (new)

```yaml
version: "3"

includes:
  config:
    taskfile: ./config.yml
    flatten: true

tasks:
  serve:
    desc: "MCP server: WebTransport (QUIC) link the editor dials into + rmcp /mcp + /control + /debug."
    cmds:
      - >
        cargo run -p awsm-audio-mcp --
        --http-port {{.PORT_MCP_HTTP_DEV}}
        --quic-port {{.PORT_MCP_QUIC_DEV}}

  build:
    desc: "Build the MCP server binary (debug)."
    cmds:
      - cargo build -p awsm-audio-mcp
```

### 5.3 `taskfiles/frontend/editor.yml`

Add `MCP_DEFAULT_ORIGIN` to the `dev` (and `build`) task `env:` so the editor
bakes the right default origin (the renderer does this):

```yaml
    env:
      ROOT_BASE_URI_PATH: "{{.DEV_ROOT_BASE_URI_PATH}}"
      MCP_DEFAULT_ORIGIN: "{{.URL_MCP_DEFAULT}}"
```

### 5.4 Root `Taskfile.yml`

- Add the include:
  ```yaml
  includes:
    mcp:
      taskfile: ./taskfiles/mcp.yml
  ```
- Add a convenience task:
  ```yaml
  mcp-dev:
    desc: "Editor + MCP server. Open http://localhost:9170/?mcp=http://127.0.0.1:9171 to attach."
    deps:
      - editor:dev
      - mcp:serve
  ```
  (Use `deps` for parallel, matching the renderer's `mcp-dev`; or document
  running `task editor:dev` and `task mcp:serve` in two terminals.)

---

## 6. Phase 6 — worklet authoring over MCP (agent compiles, sends base64)

Goal: let an agent author a **WASM DSP worklet** end-to-end over MCP. Unlike the
renderer's custom materials (WGSL compiled *in-browser* by wgpu), an awsm-audio
worklet is a **native Rust → wasm** crate, so compilation is a local toolchain
step. **Decision locked in: the agent compiles** (via its own Bash/cargo) and
ships the resulting `.wasm` bytes (base64) to the editor through an MCP tool. The
MCP server just relays bytes; the agent gets cargo's compile errors *for free* in
its own tool output (no server-side diagnostics plumbing needed).

### 6.1 The pipeline the agent follows (document this in an MCP resource)

The ABI is already fully specified by the `awsm-audio-worklet` crate
(`packages/crates/worklet/src/lib.rs`) and the example crates
(`packages/worklets/{bitcrusher,drive,ringmod}`). The flow:

1. **Author a crate** — `crate-type = ["cdylib"]`, `#![no_std]`, depends on
   `awsm-audio-worklet`. Implement `Processor` (`const PARAMS: &[ParamDesc]`,
   `fn new(sample_rate) -> Self`, `fn process(input: &[&[f32]], output: &mut
   [&mut [f32]], params: &Params)`) and call `awsm_worklet!(MyType)` once at crate
   root. Mono-per-channel, stereo (`CHANNELS = 2`), ≤ `MAX_FRAMES` (128) frames
   per quantum, ≤ `MAX_PARAMS` (32) params, **no allocation in `process`**. Use
   the crate's `math::{sin,tanh}` (avoids pulling `f32::sin` symbols on wasm).
2. **Compile** — `cargo build -p <crate> --target wasm32-unknown-unknown
   --release` → `target/wasm32-unknown-unknown/release/<crate>.wasm`.
3. **Attach** — call the `attach_wasm` MCP tool with the AudioWorklet node id +
   the `.wasm` (path or base64). The editor compiles the module, **discovers its
   params** (name/min/max/default from the ABI exports), stores it, and binds it
   to the node. The params then appear in the next `get_snapshot` as
   `AudioWorkletNode.parameters` (`WorkletParam`), editable / automatable /
   modulation-targetable like any node field.

Expose this as an MCP **resource** (`awsm-audio://docs/worklet-abi`, returning the
above + a minimal `Gain` example) and an **`author_worklet` prompt** — the audio
analog of the renderer's material-recipe resources. This is what lets the agent
author a correct crate without reading the repo.

### 6.2 Protocol crate — add the attach variant (Phase 1 crate)

In `editor-protocol/src/transport.rs`, add to `Request`:

```rust
    /// Attach a compiled WASM DSP module (base64-encoded `.wasm`) to an
    /// AudioWorklet node. Carries bytes (not an `EditorCommand`) for the same
    /// reason PNGs are a `Response`, not a command: large binary stays out of the
    /// command/undo stream. The editor compiles + discovers params + binds it.
    AttachWasm {
        node: NodeId,           // awsm_audio_schema::NodeId
        wasm_base64: String,    // standard base64, with padding
        #[serde(default)]
        label: String,
    },
```

(Import `NodeId` from `awsm_audio_schema` at the top of `transport.rs`.) No new
`Response` variant is needed — attach replies `Response::Ok` on success or
`Response::Err(compile error)` on failure. The discovered params are read back via
the existing `get_snapshot`. Add a serde round-trip test for `AttachWasm` to the
§1.6 suite.

### 6.3 Editor side — handle `AttachWasm` on the async branch (Phase 3 remote.rs)

`attach_wasm_bytes` (`controller/mod.rs:4490`) is **async** (it compiles the
module, awaits `Player::compile_module`, discovers params, stores the module +
`WasmAsset{source: Base64}`, and calls `set_node_module`). So `AttachWasm` joins
`RenderWav`/`WavStats`/`Waveform` on the async branch of `serve_one` (§3.3),
**not** the sync `dispatch`:

```rust
// in serve_one's match, alongside the WAV arms:
Ok(Request::AttachWasm { node, wasm_base64, label }) =>
    attach_wasm(node, wasm_base64, label).await,
```

```rust
async fn attach_wasm(node: NodeId, wasm_base64: String, label: String) -> Response {
    let bytes = match base64::engine::general_purpose::STANDARD.decode(wasm_base64.as_bytes()) {
        Ok(b) => b,
        Err(e) => return Response::Err(format!("bad base64: {e}")),
    };
    // attach_wasm_bytes is fire-and-forget today (spawns its own task). For MCP we
    // want to await the compile + report failure, so factor a fallible async
    // variant out of it — `attach_wasm_bytes_async(node, bytes, label) ->
    // Result<(), String>` — that does the same work but returns the
    // compile/discover error instead of only `tracing::error!`-ing it. Reuse the
    // existing body (compile_module → discover_params → store_module →
    // set_node_module); just thread the errors out.
    match controller().attach_wasm_bytes_async(node, bytes, label).await {
        Ok(()) => Response::Ok,
        Err(e) => Response::Err(e),
    }
}
```

> Refactor note: today `attach_wasm_bytes` swallows errors (logs + early-returns)
> because it's driven by a file picker where there's no caller to report to. For
> MCP we need the compile result. Keep the existing `attach_wasm_bytes` (the file
> picker uses it) but extract the inner async body into
> `attach_wasm_bytes_async(... ) -> Result<(), String>` and have the old method
> call it (`spawn_local` + log on `Err`). The MCP path awaits the `Result`.

### 6.4 MCP server — the `attach_wasm` tool (Phase 2 mcp.rs)

Add a tool that accepts **either** a path (read + base64 by the server) **or**
inline base64 (agent already encoded it). Path is the ergonomic default — the
agent passes the build output path and the server slurps it:

```rust
#[derive(Deserialize, schemars::JsonSchema)]
struct AttachWasmParams {
    /// AudioWorklet node id (UUID). Create one first with add_node(kind: audio_worklet).
    node: String,
    /// Path to the compiled .wasm (e.g. target/wasm32-unknown-unknown/release/foo.wasm).
    #[serde(default)]
    wasm_path: Option<String>,
    /// Or inline base64 (standard, padded) if you already encoded it.
    #[serde(default)]
    wasm_base64: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

#[tool(description = "Attach a compiled WASM DSP module to an AudioWorklet node. \
    Author a crate against awsm-audio-worklet (see the awsm-audio://docs/worklet-abi resource), \
    `cargo build --target wasm32-unknown-unknown --release`, then pass the .wasm path here. \
    On success the node's discovered params show up in get_snapshot.")]
async fn attach_wasm(&self, Parameters(p): Parameters<AttachWasmParams>) -> Result<CallToolResult, McpError> {
    let node = parse_node(&p.node)?;
    let wasm_base64 = match (p.wasm_base64, p.wasm_path) {
        (Some(b64), _) => b64,
        (None, Some(path)) => {
            let bytes = std::fs::read(&path)
                .map_err(|e| McpError::internal_error(format!("read {path}: {e}"), None))?;
            base64::engine::general_purpose::STANDARD.encode(bytes)
        }
        (None, None) => return Err(McpError::invalid_params("need wasm_path or wasm_base64", None)),
    };
    let label = p.label.unwrap_or_else(|| "module".to_string());
    match self.req(Request::AttachWasm { node, wasm_base64, label }).await? {
        Response::Ok => Ok(text("ok — params discovered; call get_snapshot to see them")),
        Response::Err(e) => Err(McpError::internal_error(e, None)),  // surfaces cargo-compiled-but-invalid-ABI errors
        other => Err(unexpected(other)),
    }
}
```

Also add the worklet docs to the `ServerHandler::list_resources`/`read_resource`
(the `awsm-audio://docs/worklet-abi` resource) and a `author_worklet` entry in
`list_prompts`/`get_prompt`.

### 6.5 Gate

```
cargo test  -p awsm-audio-editor-protocol     # AttachWasm round-trip green
task lint                                      # AUTHORITATIVE
cargo build -p awsm-audio-editor --target wasm32-unknown-unknown   # wasm compile
cargo build -p awsm-audio-mcp           # server compiles the new tool
```

The live attach (agent authors → `cargo build` → `attach_wasm` → param discovery
→ audible) is browser-only → **morning checklist (§7)**. As an *unattended*
sanity step you **can** still verify the agent-author + compile half: write a
trivial `Gain` worklet in a scratch crate, `cargo build --target
wasm32-unknown-unknown --release`, and confirm a `.wasm` is produced (proves the
ABI crate + toolchain path the agent will follow). Note it in `MCP-STATUS.md`.
Commit when green.

---

## 7. Phase 7 — live verification (DEFERRED to a morning checklist)

⚠️ **Do not attempt this unattended.** The live round-trip needs a hand-attached
Chrome tab (WebTransport `serverCertificateHashes` is Chrome-only) and an MCP
client. Instead, **write `docs/plans/MCP-STATUS.md`** containing:

1. A short status summary: which phases are done, what native tests cover, what's
   committed (with the branch name), and the exact next step if you stopped
   mid-arc.
2. The morning checklist below, verbatim, so a human can run it.

### Morning checklist (paste into `MCP-STATUS.md`)

1. **Start the server:** `task mcp:serve` → logs the cert hash and
   "WebTransport (QUIC) listening on udp/9172".
2. **Start the editor:** `task editor:dev` → serves on `:9170`.
3. **Attach:** open `http://localhost:9170/?mcp=http://127.0.0.1:9171` in
   **Chrome**. Server logs "editor attached"; the top-bar MCP button shows
   connected.
4. **Raw round-trip via `/debug`** (server up, editor attached):
   ```
   curl -s -X POST http://127.0.0.1:9171/debug \
     -H 'content-type: application/json' -d '{"Play":null}'        # → "Ok"
   curl -s -X POST http://127.0.0.1:9171/debug \
     -H 'content-type: application/json' -d '{"Query":{"query":"samples"}}'
   ```
   → `Ok`, then the sample list as JSON. (The `Request` enum is externally
   tagged; the inner `EditorQuery` uses `tag:"query"`. The exact JSON shapes are
   pinned by the §1.6 serde tests — copy a payload from a test if unsure.)
5. **WAV render:**
   ```
   curl -s -X POST http://127.0.0.1:9171/debug \
     -H 'content-type: application/json' -d '{"RenderWav":{}}'
   ```
   → `{ "Wav": { "bytes": N, "saved": true, "path": "/tmp/awsm-audio-mcp-last.wav" } }`.
   Play `/tmp/awsm-audio-mcp-last.wav` to confirm it's the root Sound.
6. **MCP client:** point an MCP client (e.g. Claude Code) at
   `http://127.0.0.1:9171/mcp` (streamable HTTP). Confirm `get_snapshot`,
   `list_samples`, `render_wav`, `wav_stats`, `waveform`, and a mutation
   (`add_node`) all work, and that a follow-up `get_snapshot` reflects the
   mutation.
7. **Connect UX:** confirm the top-bar button + modal connect/disconnect, and
   that loading without `?mcp=` and connecting via the modal also works.
8. **Worklet authoring (Phase 6):** via the MCP client, `add_node` an
   `audio_worklet` node; author a trivial `Gain` crate against
   `awsm-audio-worklet`; `cargo build -p <crate> --target wasm32-unknown-unknown
   --release`; call `attach_wasm { node, wasm_path }`; confirm `get_snapshot`
   shows the discovered `gain` param, then wire it and `render_wav` to hear it.
   Also confirm a deliberately-broken module returns the compile/ABI error.

If any step fails, the fix is almost always in `remote.rs` (`dispatch`/framing)
or a serde-shape mismatch — add/adjust a §1.6 round-trip test to pin it, then
re-verify.

---

## 8. File inventory (what you'll create / touch)

**New crate — `packages/crates/editor-protocol/`**
- `Cargo.toml`
- `src/lib.rs`, `src/command.rs`, `src/query.rs`, `src/snapshot.rs`,
  `src/field.rs`, `src/node.rs`, `src/clipboard.rs`, `src/transport.rs`

**New crate — `packages/mcp/`**
- `Cargo.toml`
- `src/main.rs`, `src/cert.rs`, `src/quic.rs`, `src/link.rs`, `src/http.rs`,
  `src/mcp.rs`

**Editor crate — `packages/frontend/editor/`**
- `Cargo.toml` (add deps), `src/main.rs` (add `mod remote;` + `?mcp=` autoconnect)
- `src/remote.rs` (new; incl. the `AttachWasm` async arm)
- `src/controller/command.rs`, `src/controller/snapshot.rs`,
  `src/controller/node.rs`, `src/controller/mod.rs`, `src/fields.rs`
  (strip moved types → re-export from protocol crate; add `render_pcm`; extract
  `attach_wasm_bytes_async(...) -> Result<(), String>` from `attach_wasm_bytes`)
- `src/ui/…` (MCP button + modal)

**Workspace / config**
- `Cargo.toml` (members + workspace deps + promote `uuid`)
- `taskfiles/config.yml`, `taskfiles/mcp.yml` (new),
  `taskfiles/frontend/editor.yml`, `Taskfile.yml`

**Docs**
- `docs/plans/MCP-STATUS.md` (new) — status summary + the deferred morning
  checklist (§7).

---

## 9. Gotchas & cross-references

- **rustfmt drift** (memory `rustfmt-drift`): never `cargo fmt --all`; format only
  new files. `task lint`'s `--check` must still pass.
- **MSRV split**: only `packages/mcp` is edition-2024 / 1.85 (crate-local). The
  wasm crates stay edition-2021 / 1.80.
- **Chrome only** for the WebTransport client (`serverCertificateHashes`).
- **10-day cert**: regenerated each server start; nothing persists. If a tab was
  open across a restart, reconnect (the hash changed).
- **`allow_private_network(true)`** is mandatory for the hosted (HTTPS) editor to
  reach the loopback server; without it Chrome blocks the preflight.
- **JSON wire shape**: `Request`/`Response` are externally-tagged Rust enums;
  the inner `EditorCommand`/`EditorQuery` use their own `tag/content` attrs.
  Confirm exact shapes from the derives when hand-writing `/debug` payloads.
- **Reference implementation**: every server/quic/link/cert/http/remote file here
  is a port of `../awsm-renderer/packages/{mcp,crates/editor-protocol,frontend/editor}/`.
  When in doubt, diff against the sibling.
- **Sync vs async**: the audio controller's `dispatch`/`query` are sync; only the
  offline render is async. Don't accidentally make the whole interpreter async.

---

## Definition of done

**Unattended / overnight (must be green + committed):**
- [ ] `awsm-audio-editor-protocol` compiles native + wasm; serde round-trip
      tests pass; editor re-exports the moved types and still builds to wasm.
- [ ] `awsm-audio-mcp` builds; cert test passes; it boots and `GET
      /control` returns URL + hash (headless curl).
- [ ] Editor `remote.rs` + connect UX + the new `render_pcm` controller method
      compile to wasm; pure WAV-math helpers have native unit tests.
- [ ] Worklet path compiles: `Request::AttachWasm` round-trips (test);
      `attach_wasm_bytes_async` + the `attach_wasm` tool + the
      `awsm-audio://docs/worklet-abi` resource build; a scratch `Gain` worklet builds to
      `.wasm` (proves the author→compile half).
- [ ] `task lint` passes at every committed checkpoint; no existing files
      rewrapped.
- [ ] `docs/plans/MCP-STATUS.md` written: status summary + morning checklist +
      what's natively covered vs. browser-deferred + exact next step.

**Deferred to the morning checklist (browser/MCP — do NOT attempt unattended):**
- [ ] Editor connects via `?mcp=`; top-bar button + modal work.
- [ ] `/debug` round-trips a query, a mutation, and `RenderWav` (file on disk).
- [ ] `wav_stats` + `waveform` return correct numbers for a known Sound.
- [ ] An MCP client at `/mcp` can discover, mutate, and read back WAV.
- [ ] Agent authors a worklet crate → `cargo build` → `attach_wasm` → params
      discovered in `get_snapshot` → audible; a broken module returns the error.
```

# awsm-audio

A node-graph WebAudio editor + player, in Rust → WebAssembly. Build, parameterize,
and play WebAudio graphs in a Max/MSP–style canvas — including your own DSP
compiled to WASM.

## Quick start

```sh
task dev      # build the example worklets, then run the editor dev server
```

Then open the printed localhost URL. `task lint` runs fmt + clippy;
`task build` produces a production bundle; `task worklets` rebuilds the bundled
example WASM modules.

## Workspace

All crates are prefixed `awsm-audio-*`.

| Crate | What it is |
| --- | --- |
| `packages/crates/schema` | Pure-data types for every WebAudio node, full-fidelity `AudioParam` automation, composable samples, assets. Round-trips through TOML. The audio "truth". |
| `packages/crates/player` | Instantiates a schema `Graph` onto a live `AudioContext` (or `OfflineAudioContext` for WAV export). Owns transport, the analyser, noise generation, and the WASM-worklet shim. |
| `packages/crates/worklet` | `awsm-audio-worklet` — write a custom DSP processor for the AudioWorklet node (a `Processor` trait + `awsm_worklet!` macro). |
| `packages/frontend/editor` | The `dominator`/`trunk` reactive UI. |
| `packages/worklets/*` | Example worklet processors (bitcrusher, drive, ringmod). |

## Architecture

Every editor mutation flows through a single **`EditorController`** — UI event
handlers translate gestures into a serde-derived `EditorCommand` and call
`dispatch`; nothing mutates state any other way. A serializable snapshot is the
read half. This single command/query surface is exactly the seam the **MCP
server** drives (see below), so an AI agent and a human watching the same tab stay
in sync. The JS-callable bridges (`editor_dispatch_toml`, `editor_snapshot_toml`,
`editor_play`, `editor_export_wav`, `editor_attach_wasm`, …) live in
`editor/src/main.rs`.

The player auto-routes any terminal node (one whose output feeds nothing) into a
master gain → analyser → speakers, so a graph plays without an explicit Output
node. An **Output** node is an explicit sink; **Spatial Output** routes through an
HRTF panner positioned in 3D.

## Driving the editor from an AI agent (MCP)

The editor can be driven by any MCP-capable agent (Claude Code, Claude Desktop,
Codex, …): build node graphs, edit fields, author + attach WASM DSP worklets,
drive the sequencer and arrangement timeline, render Sounds/arrangements offline,
and read back WAV stats / waveform envelopes. Great for agent-in-the-loop sound
design.

```
agent (MCP client) ──HTTP /mcp──▶ awsm-audio-mcp ──WebSocket /editor──▶ editor (browser tab)
                                  (packages/mcp)    editor dials out    → EditorController
```

A native server ([`packages/mcp`](packages/mcp), the `awsm-audio-mcp` binary)
exposes MCP tools over streamable-HTTP and relays each one to a running editor tab
over a plain WebSocket that **the editor dials out to** (a browser tab can't be a
server). Each agent is bound to one editor tab, so requests, responses, and events
can never cross between sessions. The whole tool vocabulary is strongly typed —
`tools/list` publishes the exact JSON Schema for every node kind, command, and
query.

The loop has **three pieces, all required**: the **MCP server**, an attached
**editor tab** (the audio truth — without it, tool calls return *"no editor
attached"*), and your **agent**. Set them up in that order:

### Install the server

Prebuilt binaries — no toolchain needed:

```bash
# macOS / Linux
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/dakom/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.sh | sh
# Windows (PowerShell)
powershell -ExecutionPolicy Bypass -c "irm https://github.com/dakom/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.ps1 | iex"
```

From source: `cargo install --git https://github.com/dakom/awsm-audio awsm-audio-mcp`,
or `task mcp:install` from a clone (builds release → `~/.cargo/bin`).

(Maintainers: cutting a new release is one `git tag` — see
[docs/RELEASING.md](docs/RELEASING.md).)

### Quick start

1. **Start the editor + the MCP server.** Easiest — both together from the repo:

   ```bash
   task mcp-dev          # editor:dev + mcp:serve  (or run each in its own terminal)
   ```

   | Service | Address |
   | --- | --- |
   | Editor (Trunk) | `http://localhost:9170` |
   | MCP server (HTTP) | `http://127.0.0.1:9171` (`/mcp`, `/editor` ws, `/debug`, `/renders`) |

   Or, with the server installed, just run `awsm-audio-mcp` from any directory
   (defaults to port 9171) and `task editor:dev` for the tab.

   > Works in any modern browser — the link is a plain loopback WebSocket (no TLS,
   > no certificate, no Chromium-only APIs).

2. **Attach the editor** to the server: click the **MCP** button in the top bar,
   or load the editor with `?mcp=` to auto-connect:

   ```
   http://localhost:9170/?mcp=127.0.0.1:9171
   ```

   The `?mcp=` value is a bare `host:port` (the link defaults to plain
   `ws`/`http`, since the server is normally local). For a TLS-terminated remote
   server, add `&tls=true` to use `wss`/`https` (or tick the box in the connect
   modal).

   The server logs `editor attached`; the top-bar button shows **MCP ✓** and a
   **🤖 working… / idle** chip tells you when it's safe to edit / when a render is
   done. (No connection → the editor runs normally with zero remote overhead.)

3. **Point your agent at the server.** A ready-to-use [`.mcp.json`](.mcp.json) is
   in the repo root:

   ```json
   {
     "mcpServers": {
       "awsm-audio": { "type": "http", "url": "http://127.0.0.1:9171/mcp" }
     }
   }
   ```

   - **Claude Code / Claude Desktop**: a project-root `.mcp.json` is picked up
     automatically — restart the agent in this directory.
   - **Other MCP clients**: register a streamable-HTTP MCP server at
     `http://127.0.0.1:9171/mcp`.

### What the agent can do

Start with `get_snapshot` and `list_node_kinds` (every creatable kind with its
default value, editable field keys, and a plain-language description). Then:

- **Build** — `add_node`, `connect`, `set_field`, `remove_node`,
  `set_active_sample` (edit a sub-sample / instrument).
- **Load samples** — `load_audio` brings an external WAV/mp3/flac (a local file
  path *or* a URL) into an `audio_buffer_source` / `convolver` node. Bytes ride a
  dedicated HTTP side-channel, never the editor link.
- **Sequence / arrange** — `dispatch_command` with `edit_song` / `edit_control` /
  `edit_arrange`; `bounce`; `set_arrangement_markers` (loop/export region).
- **Worklets** — author a crate against `awsm-audio-worklet`, `cargo build
  --target wasm32-unknown-unknown --release`, then `attach_wasm { node, wasm_path }`.
- **Render / read back** — `render_wav` (offline render of a Sound *or* an
  arrangement timeline → `.wav` on disk), `wav_stats`, `waveform`, `get_transport`.
- **Escape hatches** — `dispatch_command` / `dispatch_batch` / `run_query` accept
  any `EditorCommand` / `EditorQuery` (typed schema, string-tolerant), so the
  entire surface is reachable even without a dedicated tool.

Two MCP resources document the rest: `awsm-audio://docs/vocabulary` (the
command/query JSON shapes) and `awsm-audio://docs/worklet-abi` (authoring a
worklet).

> **Multiple tabs / agents.** With exactly one editor tab and one agent the two
> auto-pair. When more than one of either is connected, the agent returns a short
> **pairing code** — open the editor with `?mcp=…&pair=<code>` (or type the code
> into the MCP connect modal) to bind that tab to that agent.

## Nodes

Sources (oscillator, buffer, constant, noise, media element/stream), effects
(gain, biquad, IIR, delay, compressor, waveshaper, convolver), spatial (panner,
stereo panner, spatial output), routing (channel splitter/merger), analysis,
and the WASM **AudioWorklet**. Most params are envelope-automatable and
modulation-wire targets.

## Tips & tricks

**Wiring.** Drag from an **output port** (right edge) to an **input port** (left
edge) for audio. Drag an output to one of the smaller **param inlets** (left
edge, e.g. *modulate frequency*) to modulate that parameter. Right-click a wire
to delete it.

**Inputs — a sample's parameters.** Drop an **Input** node, name it (select it →
the inspector), and wire it to a node's parameter. That input becomes a named
port on the parent's `Sample` node, where you can give it a **value** (per
instance) or MIDI-map it. This is the one way to "control a sub-sample from
outside" — there's no separate "macro" concept.

- An input's **value sets** the inner parameter — it's just that param's value
  for this instance (the same as editing the field). So a voice's `pitch` input
  set to 330 plays at 330.
- A **wire** carrying a signal into a parameter — e.g. an LFO → a filter cutoff,
  or a signal fed into a sample's input port — is **additive**: the signal sums
  with the parameter's value (`computed = value + Σ inputs`). This is the native
  WebAudio model — a connection can't *replace* a param, only add to it. To make
  a wired input fully *be* the value, set that parameter's field to `0` so
  `0 + signal = signal`.

**The "float" primitive** is the **Constant Source** node: set its `offset` and
wire the output anywhere (a param, an input, an audio input). Handy as a DC bias
or a base for LFO modulation.

**Composition.** Each project is a set of **samples** (tabs along the top). A
`Sample` node embeds another sample; the player flattens the whole tree at play
time. Select nodes and press **Ctrl/Cmd-G** to *encapsulate* them into a new
sub-sample with auto-generated inputs/outputs. **Play auditions the active tab**,
so you can work on a sub-sample in isolation.

**Editor.** Drag a palette item onto the canvas to drop it at the cursor (or
click to add at center). **Backspace/Delete** removes the selection;
**Ctrl/Cmd-C/V/D** copy/paste/duplicate; **Ctrl/Cmd-A** select-all;
**Shift-drag** box-selects; the wheel zooms toward the cursor; **Fit** frames the
graph. Right-click a node for **Clone**/**Delete**. Each node's **?** opens MDN-
linked help.

**Visualizing nodes.** Selecting a node shows its envelopes plus, where it
helps, a live picture of what it does: a **Wave Shaper**'s transfer curve, an
**IIR Filter**'s magnitude response, and — for a **custom oscillator** (type
`custom`) — a **drawable harmonics editor**: drag across the bars to paint the
partial amplitudes (bar 0 = fundamental). The player builds a `PeriodicWave`
from them, so you're sketching the timbre directly.

**Playing & MIDI.** The computer keyboard is a one-octave piano (`z`-row white
keys, `s`-row black keys) — it transposes the patch, and it's **polyphonic**:
hold several keys for a chord, each note rings until you release it. Click
**MIDI** to enable Web MIDI: note-on/note-off play polyphonically (note 60 =
unison), **velocity scales amplitude**, and the patch is auditioned per voice.
Map a hardware knob to any input with **MIDI-learn**: in the inputs panel (shown
when nothing is selected) click an input's **MIDI** chip, then turn a CC — it
binds (shown as `CC#n`) and that control then drives the input. Click the chip
again to unbind. CC moves (and dragging an input's value) **sweep live** — the
change glides into the sounding param without rebuilding, across every held
note, so filter sweeps and the like are smooth and click-free.

**Songs (the sequencer).** Drop a **MIDI Song** node to play a whole multi-track
song through instruments you've built (see the **Sequenced Song** example). Select
it and either **load a `.mid` file** or **add a track** and author notes in the
**piano roll**: drag empty grid to draw a note (drag right for length), drag a
note's body to **move** it, drag its right edge to **resize**, **scroll** over it
to set **velocity** (brighter = louder), and click it to delete. Tabs along the
top switch between the song's tracks; a **playhead** sweeps the grid during
playback. Each **part** is an output port bound to a track — **wire that port to
an instrument** (a `Sample` node, or any node) and that part plays it; set a
part's **transpose**/**gain**, and the node's **tempo**, **start** (a beat to
seek to), and **loop** (loops seamlessly). Press **play** to perform the song —
every note becomes a polyphonic voice of its instrument, transposed to pitch and
scaled by velocity. Imported `.mid` files honor **mid-song tempo changes**. A
part can be flagged **drums** (auto for General-MIDI channel 10): its piano-roll
rows are labeled with GM percussion names, and a **per-note drum map** lets each
note play its own instrument sample (build a Kick/Snare/Hat sample and assign
them) — unmapped notes fall back to the wired instrument played pitched. The MIDI
Song node makes no sound itself — it triggers the instruments wired to it.

## Writing a WASM worklet

Implement `Processor` and invoke `awsm_worklet!` once; compile as a `cdylib` to
`wasm32-unknown-unknown` and load the `.wasm` into an AudioWorklet node. Its
parameters are auto-discovered and become editable, automatable, modulation-
targetable knobs. Processing is **stereo** (`CHANNELS = 2`); a mono input is
duplicated to both channels.

```rust
use awsm_audio_worklet::*;

struct Gain;
impl Processor for Gain {
    const PARAMS: &'static [ParamDesc] = &[ParamDesc::new("gain", 0.0, 2.0, 1.0)];
    fn new(_sample_rate: f32) -> Self { Gain }
    fn process(&mut self, input: &[&[f32]], output: &mut [&mut [f32]], params: &Params) {
        let g = params.get(0);
        for ch in 0..output.len() {
            for i in 0..output[ch].len() {
                output[ch][i] = input[ch].get(i).copied().unwrap_or(0.0) * g;
            }
        }
    }
}
awsm_worklet!(Gain);
```

### The worklet ABI

A generic shim is registered once per context; it instantiates your module and
drives it a render quantum at a time. The macro generates these exports (the
shim only requires `memory` + `process`):

- `memory` — the module's linear memory.
- `init(sample_rate: f32, max_frames: u32)` — called once.
- `input_ptr() -> u32` / `output_ptr() -> u32` — base of planar f32 scratch,
  `CHANNELS * MAX_FRAMES` long (channel `c` at `c * MAX_FRAMES`).
- `params_ptr() -> u32` — f32 array, one slot per discovered param (k-rate).
- `process(frames: u32)` — read input + params, write output.
- `channels() -> u32` — channel count (2).
- Discovery: `param_count()`, `param_name_ptr(i)/param_name_len(i)`,
  `param_min(i)/param_max(i)/param_default(i)`.

Modules must be import-free so the shim can instantiate them with no imports.

## Persistence

**Save** writes a self-contained project (the portable `SampleLibrary` — graph +
embedded WASM modules + embedded audio clips — plus editor layout + camera).
**Open** restores it exactly; a bare `SampleLibrary` (e.g. an exported example)
also opens and auto-lays-out. **Export** renders the graph offline to a WAV.

## Status

Verification is preview-driven (an internal headless browser). CI runs
fmt + clippy (wasm + host) + schema tests. Browser-integration tests
(`wasm-bindgen-test`) are not yet wired.

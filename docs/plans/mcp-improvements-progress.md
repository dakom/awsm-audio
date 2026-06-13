# mcp-improvements.md — implementation progress

Tracking the full P0–P3 set from `docs/plans/mcp-improvements.md`. One line per
item. Marked TODO / DONE (with a one-line note on what changed) / RATIONALE (if a
closest-safe behavior was chosen instead).

**Hard constraints (apply to every change):**
- STYLE- and SOURCE-AGNOSTIC: neutral mechanisms only. No genres, feels, swing
  ratios, groove presets, or specific sample sources/URLs baked into the server.
- SFX/sound-design-first: the single-Sound / one-shot / no-arrangement path is
  first-class. `[music]` items are arrangement-only and must not complicate SFX.

## Orientation (how the server is built / run / tested)

- Workspace of Rust crates. The MCP server is `packages/mcp` (native, `awsm-audio-mcp`),
  a thin tool layer over an **EditorLink** WebSocket to the wasm editor.
- The editor (`packages/frontend/editor`) holds the document and applies
  `EditorCommand`s; the player (`packages/crates/player`) renders audio (wasm-only).
- `EditorCommand`/`EditorQuery`/`FieldValue`/`FieldInfo` live in the shared
  `packages/crates/editor-protocol` crate. Schema types in `packages/crates/schema`.
- **Key architecture fact:** the editor's live `self.nodes` is ONLY the *active*
  sample's graph; non-active samples sit in `self.samples` as stored schema graphs.
  `node_by_id` sees only the active canvas — the root cause of the P0 silent no-ops.
- The MCP→editor bridge (`editor/src/remote.rs::dispatch`) returns `Response::Ok`
  even when `ctrl.dispatch` matched no node — the literal silent-failure surface.
- `fields::apply` (editor) already writes AudioParam base `.value`; `player::build::apply_param`
  applies `set_value(base)` then the automation timeline. So in the ACTIVE canvas
  set_field already renders; the experienced #1/#2/#3 no-ops are the active-canvas gap
  plus the readback gap (snapshot/get_node_fields show base, never the automated value).
- Tests: CI runs `cargo fmt --check`, `cargo clippy` (wasm32 + host), `cargo test -p
  awsm-audio-schema`. The editor crate **host-compiles and runs unit tests**
  (`EditorController::new()` is web_sys-free), so controller-level regression tests are
  feasible. MCP unit tests are parse/normalize tests (no live editor).
- Build/verify command for this work:
  `cargo test -p awsm-audio-schema -p awsm-audio-mcp -p awsm-audio-editor-protocol -p awsm-audio-editor`
  plus `cargo clippy --workspace -- -D warnings` and the wasm32 clippy.
- Docs/instructions live as consts in `packages/mcp/src/mcp.rs`: `info.instructions`,
  `VOCABULARY_DOC` (generated index via `vocabulary_doc()`), `WORKLET_ABI_DOC`,
  `TRACK_WORKFLOW_DOC`, `INSTRUMENTS_DOC`, served as `awsm-audio://docs/*` resources.

## Checklist

### P0 — Correctness bugs (silent, data-losing)
- [x] #1 `set_field` AudioParam no-op + no effective-value readback — DONE: root cause
  was the active-canvas restriction (live `nodes` = active sample only), not a missing
  write-through (`fields::apply` + `player::apply_param` already render the base). Added
  `EditorController::dispatch_remote` (acts by node id across samples; errors if absent),
  wired into `remote.rs`. Effective-value readback: `FieldInfo.automation` surfaced in
  `get_node_fields`/`NodeFields` (now cross-sample). Tests in `controller/mod.rs::dispatch_remote_tests`.
- [x] #2 `edit_song`/`set_output_gain` no-op off the active canvas — DONE: same
  `dispatch_remote` cross-sample routing covers `EditSong`/`SetField`; bridge no longer
  returns `ok` on a miss. Regression test `set_field_applies_cross_sample`.
- [x] #3 `output` node `gain` silently locked — DONE: already honored by `fields::apply`
  in the active canvas; cross-sample fix + regression test `output_gain_applies_cross_sample`.
  Documented set_field's AudioParam-base semantics in the tool description + instructions.

### P1 — Footguns / guardrails
- [x] #4 No clip/level warning at bounce for hot stacks — DONE: the bounce result now
  includes `suggested_gain` (~0.95/peak) + a `hint` whenever the bounce clips, and
  `true_peak`. Pure `suggested_gain_for_peak` (tested). Chose the report's suggested-gain
  hint over auto-normalize-on-bounce: the latter needs editor support to rescale a stored
  bounce (no such primitive), and the hint is neutral + leaves *where* to apply it to the agent.
- [x] #5 [doc] Pitch-tracking limitation undocumented — DONE: new "Pitch tracking"
  section in INSTRUMENTS_DOC + a section in WORKLET_ABI_DOC. States plainly only
  oscillators transpose (verified against `schema::Graph::transposed`); samples/
  worklets/noise are fixed-pitch under the sequencer. (Opt-in pitch-track *path* is a
  larger code change tracked separately; the limitation itself is now documented.)
- [x] #6 [doc] Oscillator base `frequency` is the reference at note 60 — DONE:
  documented precisely in INSTRUMENTS_DOC (authored freq = pitch at MIDI 60, notes
  transpose relative; that's why it looks "ignored"), and in the set_field description.

### P1 — Discoverability & feedback
- [x] #13 [doc] High-character tools framed as last-resort — DONE: added a
  capability-neutral nudge to info.instructions ('built-ins first' is about EFFORT,
  not uniqueness; for distinctive/organic briefs reach for real samples/worklets up
  front). Points at tools only — no genre/source/sound named.
- [x] #14 [doc+code] No perceptual feedback — DONE: (doc) preview-checkpoint + "stats
  measure level, not character/feel/intended-object" caveat in instructions and the
  wav_stats description; (code) added neutral perceptual descriptors to WavStats —
  `spectral_flatness` (tonal↔noise) and `zero_crossing_rate` — computed in the pure,
  natively-tested `WavStats::from_pcm`. Test `perceptual_descriptors_separate_tone_from_noise`.
- [x] #15 [doc] Capabilities aren't discoverable — DONE: capability line in
  info.instructions (Rust→wasm worklet toolchain + load_audio path/CORS-open URL both
  work, "step-zero options"); load_audio description notes CORS-open URLs work.

### P2 — Ergonomics / productivity
- [x] #7 `get_snapshot detail:"ids"` reports default param values, not authored ones —
  DONE: the misleading "defaults" were the #1/#2 no-op; with that fixed `value` is the
  authored base. `AudioParam` already serializes `automation` (skip-if-empty), so the
  effective value is inspectable; `slim_snapshot` (ids mode) keeps it. Clarified in the
  get_snapshot description that a non-empty `automation` overrides the base `value`.
  Regression test `snapshot_preserves_param_automation`.
- [x] #8 No per-placement variation — DONE: added `bounce_variations` (clone K times,
  re-seed every noise source with a distinct deterministic seed via `variation_seed`,
  bounce each; returns per-variation stats + suggested_gain; restores active sample).
  SFX-first, no arrangement needed. Pure helpers `variation_seed` + `noise_node_ids`
  tested. Variation source is stochastic (noise); a no-noise Sound yields identical
  clones (noted in the tool description).
- [x] #9 [music] No swing/shuffle timing primitive — DONE: added `swing_track` tool
  (grid_beats + ratio) backed by the pure `apply_swing` (delays off-grid notes by
  `(2*ratio-1)*grid`). Style-agnostic: the server only does the offset math, no
  preset. Tests `swing_delays_offbeats_only`, `swing_straight_ratio_is_identity`.
- [ ] #10 No arrangement-level / output-stage processing (master insert chain) — TODO
- [x] #11 Batch param setting — DONE: added `set_automations` (many `{node,param,events}`
  in one DispatchBatch round-trip, per-item ok/error; cross-sample via dispatch_remote).
  Inline AudioParam values in add_node/add_chain props already stick now that #1 is fixed.
- [x] #12 [music] `set_track_gains` (plural) — DONE (already present): the batch tool
  exists (mcp.rs ~1915) and the arrangement recommendation hint already references it
  (mcp.rs ~2843). Verified; the report's complaint is resolved in current code.

### P3 — Smaller notes
- [x] P3-a DC offset from asymmetric saturation — DONE (doc, the report-sanctioned
  option): WaveShaper catalog help now notes asymmetric shaping adds DC, that
  wav_stats.dc_offset reports it, and that a highpass biquad_filter (~5–20 Hz) after
  the shaper removes it (a built-in already expresses a DC blocker). Avoided a render-path
  change (wasm-only, untestable here) for the closest-safe neutral mechanism.
- [ ] P3-b bounce auto-duration / get_render_plan praise — no change needed (verify + note) — TODO
- [ ] P3-c worklet workflow praise — no change needed except pitch-tracking (covered by #5) — TODO
- [x] P3-d `load_audio` source-agnostic note — DONE: description now says which audio
  to load is the agent's decision (server endorses no source) + URLs work from CORS-open hosts.
- [ ] P3-e `verify_arrangement` praise — no change needed (verify + note) — TODO

## Notes / decisions log

(append per-iteration)

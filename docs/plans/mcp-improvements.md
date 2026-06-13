# awsm-audio MCP — Improvement Recommendations

Feedback gathered while building a full 68s track ("Rubber Planet") end-to-end
through the MCP: ~6 synth/sampled instruments, a worklet, a 5-track arrangement.
Ordered by severity. The **P0 silent-failure bug cost the most time by far** — it
made a finished-sounding track quietly wrong (no LFO movement, 100% wet reverb,
runaway delay) with zero error signal, and was only caught by chance much later.

**Cross-cutting design principle (from the handoff author):** keep the server
**style- and source-agnostic** — provide neutral mechanisms and capabilities only.
All musical-style decisions (genre, feel, swing ratios, arrangement) and all
sample-source choices (which URLs/files to load) must come from the *agent*; never
bake them into the server, its presets, or its docs. The items below are written to
respect this — implement the mechanism, not the taste.

**This tool is not just for songs.** Arguably its most powerful use is **sound-effect
and general sound-design generation** — one-shots, textures, risers, drones, UI/game
SFX, impulse responses, procedural/granular audio — where the unit of work is a single
Sound (a graph or a worklet) bounced to a clip, often with *no sequencer and no
arrangement at all*. "Build a track" is one use-case among many. Improvements should
serve the **single-Sound / SFX workflow as a first-class path**, and the
music/arrangement-specific items must not crowd it out. Items that only apply to
multi-clip musical arrangements are tagged **[music]** below; everything untagged
(especially all of P0, plus the worklet/`load_audio`/bounce/perceptual items) is
core to SFX work too.

---

## P0 — Correctness bugs (silent, data-losing)

### 1. `set_field` silently no-ops on AudioParam numeric values
**This is the big one.** `set_field` returns `{"ok"}` but does **not** change the
value when the target field is an AudioParam scalar:

- `gain` node `gain`, `biquad_filter` `frequency`/`Q`/`detune`/`gain`,
  `oscillator` `frequency`/`detune`, `delay` `delay_time`, `audio_worklet` params,
  `output` `gain`.

The node keeps its WebAudio default (filter freq `350`, Q `1.0`, osc freq `440`,
gain `1.0`). `get_node_fields` and `get_snapshot` then report the *default*, so the
only way to notice is to suspect it and diff against what you set.

Impact in practice (all silent): a "wobble" LFO depth stayed at ±1 Hz (no wobble);
reverb wet-send gains stayed at `1.0` (100% wet → washed-out mix); delay feedback
stayed at `1.0` (runaway buildup); oscillator detune/sub-octave never applied (thin
tone); a bandpass stayed at 350 Hz (muddy lead). The track sounded "synthetic and
broken" for reasons no tool output revealed.

**The workaround that works:** `set_automation` with a single
`[{event:"set_value",args:{value,time:0}}]`. So the capability exists — `set_field`
just isn't wired to it for these fields.

**Fixes (any of):**
- Make `set_field` on an AudioParam set its base value (write through to the same
  place `set_value@0` writes), OR
- If `set_field` genuinely can't set AudioParams, **return an error** instead of
  `ok` ("use set_automation for AudioParam `frequency`"), OR
- At minimum document it loudly and have `get_node_fields` flag which fields
  `set_field` actually mutates.

**Confusing follow-on:** even after `set_automation` fixes the *rendered* value,
`get_node_fields`/`get_snapshot` still show the old base (350, 1.0, …). There's no
way to read back the effective/automated value, which made verification hard (I had
to bounce + inspect the waveform to confirm). Please surface the effective value, or
at least show "base 350, automation: set_value 520@0".

### 2. `edit_song` / `set_output_gain` silently no-op off the active canvas
`edit_song` (and `set_output_gain` via it) only affects a node if that node's sample
is the **active** canvas. Called while another sample is active, it returns `ok` and
does nothing. I lost time when a re-issued `set_output_gain` "succeeded" 3× with no
effect because a different sample was active.

Inconsistent with `bounce`, `wav_stats`, `set_automation`, `set_track_gain`, which
all act by id regardless of active canvas. **Fix:** make `edit_song` act by node id
like the others (it already takes a `node` id!), or error if the node isn't on the
active canvas.

### 3. `output` node `gain` is silently locked
`set_field` on an `output` node's `gain` no-ops (stays 1.0) — a special case of #1,
but worth calling out because it's a natural first reach for trimming a part's level.
Either honor it or document that part-level trim must go through `set_output_gain` /
a pre-output gain node / arrangement `set_track_gain`.

---

## P1 — Footguns that should be guardrails

### 4. No clip/level warning at bounce time for hot stacks
Several voices bounced > 1.0 (a 3-note chord stab hit 3.7, a busy lead 3.2) because
N simultaneous voices sum. `bounce` does report `peak`/`clipping` (great!), but the
fix is always "scale the sequencer output_gain and re-bounce," found by trial. A
suggested-gain hint ("peak 3.7; try output_gain ≈ 0.25") or an optional
auto-normalize-on-bounce flag would save a lot of bounce/re-bounce loops.

### 5. Pitch-tracking limitation is undocumented and surprising
Only **oscillators** transpose to the played note; `audio_buffer_source` and
`audio_worklet` do **not** pitch-track. This is reasonable, but it's only learnable
by experiment and it fundamentally constrains design (you cannot build a melodic
sampler or a melodic Karplus-Strong worklet voice that follows the sequencer). Please
state it plainly in `docs/instruments` and `docs/worklet-abi`, and ideally offer a
path (e.g. a convention where a worklet/sample param named `frequency` or a
`pitch`-tagged param receives the note's pitch).

### 6. Oscillator base `frequency` is ignored for pitched voices — but you can't tell
Because the voice sets pitch from the note, the authored `frequency` value is dead
for a sequenced oscillator, yet the tools happily accept it and show it. Combined
with #1 (detune set_field no-op), it's very easy to build a "detuned supersaw" that
is actually a single un-detuned osc and never know. A note in the docs + the
effective-value readback (#1) would cover this.

---

## P1 — Discoverability & feedback (why an agent ships a wrong-sounding track without knowing)

Not bugs, but the reason the track was synthetic/grooveless for several rounds before
course-correction — and arguably the most "future use" items here. (Surfaced when the
user asked why the agent didn't reach for worklets/real samples until explicitly told
to. The answer: the agent followed the tooling's framing and graded itself with
metrics that can't perceive the problem.)

### 13. High-character tools are framed as last-resort, so agents under-use them
The server instructions say "compose built-in nodes first; reach for an audio_worklet
only for DSP no built-in expresses," and present `load_audio` similarly as a niche
affordance. That's good advice about *effort/complexity* — but an agent reads it as
*creative* guidance too and defaults to oscillator+filter+shaper combos that sound
synthetic. For sound-design-led briefs ("wacky", "unique", "organic", "signature"),
real samples and worklets are usually the *first* tools, not the last. **Fix:** add an
explicit line — "For distinctive/organic timbres, prefer real samples (`load_audio`)
and/or a worklet up front; the 'built-ins first' rule is about effort, not about how
unique the result can be." That one sentence would have changed the entire first pass
(the user had to explicitly prompt "explore AudioWorklet" and "make it organic" before
either was used — both were available from the start). Keep the nudge **capability-level and neutral** —
point at the *tools* (worklets, real samples), never at a genre, a sound, or a sample
source; the taste stays with the agent.

### 14. No perceptual feedback — `wav_stats` can't hear "synthetic" or "no groove"
An agent can't listen; it verifies with `wav_stats` (peak/RMS/clipping/centroid) and
will confidently declare work "done — peak 0.94, clean" while it's actually synthetic
and grooveless — or, for SFX, while a "laser" reads as a buzz or an "explosion" has no
weight. The metrics measure *level*, not *character*, *feel*, or *whether the sound is
the intended object*; the agent grades itself on a dashboard that can't see the real
problem. Ideas:
- Encourage a **short human-preview checkpoint** in the docs ("render a few seconds of
  the Sound and get the user's reaction before building out the full SFX set or
  arrangement") — cheapest, highest-impact fix.
- Optionally expose perceptual descriptors that *do* correlate with the failure modes:
  spectral flatness / harmonicity (synthetic vs. natural), an onset-timing histogram
  or swing-ratio estimate (timing feel), transient density, stereo width. These are
  neutral *measurements* the agent interprets — not style judgments the server makes.
- Even just a doc caveat — "these stats describe level, not musical quality; don't
  treat them as success criteria for a creative brief" — would calibrate the agent.

### 15. Capabilities aren't discoverable without probing
Whether the Rust→wasm toolchain (`cargo`, `wasm32-unknown-unknown`) is installed, and
whether `load_audio` from a URL works, are only knowable by trying. That uncertainty
makes the escape hatches *feel* expensive, so an agent avoids them. A short capability
line in the server instructions ("worklet builds available; `load_audio` url works
from CORS-open hosts") would make agents reach for them as step zero.

---

## P2 — Ergonomics / productivity

### 7. `get_snapshot detail:"ids"` reports default param values, not authored ones
The id-detail snapshot showed `frequency:350, Q:1.0` etc. that looked authoritative
but were just defaults. Either omit param values in `ids` mode or show the real
effective values; the half-truth actively misleads.

### 8. Tiling forces byte-identical repeats; there's no per-placement variation
Arrangement tiling places the *same bounced clip* N times, so every repeat is
identical. `humanize_track` helps *within* a loop, but the tiled copies are still
byte-identical to each other. A neutral mechanism for per-iteration variation (the
agent decides whether/how much to apply) would help, e.g.:
- `add_clips` with a `humanize`/`seed-per-clip` option, or
- a "bounce K variations" helper, or
- clip-level micro-offset/transpose jitter.
`clone_sequence_transform` is close but is per-Sound, not per-placement. This is *not*
only a music need: generating **N non-identical variations of a one-shot** (footsteps,
impacts, UI clicks, gunshots, debris) is a core SFX deliverable — a "bounce K seeded
variations of this Sound" helper would serve sound-design at least as much as
arrangement tiling, and needs no arrangement at all.

### 9. No swing/shuffle timing primitive **[music]**
I hand-authored every offbeat (e.g. nudging the `&` from `x.5` to `x.63`) to break the
rigid grid — a lot of manual math. A neutral timing-transform primitive — a `swing`
option on `humanize_track`/`set_track_events` taking a **ratio + target grid** — would
remove all of it. Keep it style-agnostic: the server only applies the offset math; the
agent decides the ratio, the grid, and whether to use it at all. (This was the single
biggest factor in making the track feel un-mechanical — but that's the agent's call,
not a built-in "groove" preset.)

### 10. No arrangement-level / output-stage processing
An arrangement can't host a node graph, so there's no master insert for
glue/limiter/saturation. With real (dynamic) drums the mix peaked transiently and I
had to scale tracks down, losing level. An output insert chain (even just a single
gain/compressor/waveshaper) would help — and it's not only a music concern: a final
limiter/EQ stage is just as useful for polishing an individual SFX or normalizing a
batch of one-shots. Today the only option is inserting a processor before each part's
output node.

### 11. Batch param setting
Authoring envelopes/params is one `set_automation` per param per node. A batch form
(many `{node,param,events}` in one call), and/or accepting AudioParam `value` inline
in `add_node`/`add_chain` props that actually sticks (see #1), would cut round-trips
dramatically. `dispatch_refs` is great for topology; params still trickle one-by-one.

### 12. `set_track_gains` (plural) is referenced but I only found singular **[music]**
A `verify_arrangement` recommendation said "reduce track gains (set_track_gains)" but
I only had `set_track_gain` (singular). Either expose the batch form or fix the hint.

---

## P3 — Smaller notes

- **DC offset from asymmetric saturation**: `verify_arrangement` flagged a small
  master DC offset (~-0.01) from tanh/asymmetric shapers. A built-in DC-blocker
  option on `wave_shaper` (or a note in its docs) would help; I ended up writing one
  inside a worklet.
- **`bounce` auto-duration for procedural sources** is well-documented and
  `get_render_plan` is excellent — no change needed, just praise. The
  `duration_secs` override + the sequencer-wrapper pattern both worked.
- **Worklet workflow is genuinely great**: `scaffold_worklet` → `cargo build` →
  `attach_wasm` worked first try; param discovery is slick; the ABI doc (use
  `math::tanh`, the `no_std` panic-handler note) is accurate and saved me. The only
  gap is the pitch-tracking limitation (#5).
- **`load_audio` via URL** worked great; the path/url split and "bytes never cross
  the link" model is clean. **Keep it source-agnostic** — please don't bake in,
  bundle, or endorse specific sample sources/URLs; which audio to load is the agent's
  decision. A neutral capability note that URL loading works from CORS-open hosts
  (#15) is all that's needed.
- **`verify_arrangement` is excellent** — the one-call bounce-report + per-track
  solo + master stats + recommendations is exactly the right pre-export tool. More
  of this pattern, please.

---

## Scope
Every item above (P0–P3) is in scope — please implement the full set, not a subset.
Where an item proposes a doc/instruction change rather than code (e.g. #5, #13, #15),
treat updating the server instructions / `docs/*` resources as the deliverable. If any
single item turns out to be genuinely infeasible, implement the closest safe behavior
(for the silent-failure bugs: at minimum return a clear error instead of `ok`) and
leave a short written rationale where it lived.

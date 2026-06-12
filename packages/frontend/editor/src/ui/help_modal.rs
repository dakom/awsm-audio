//! The page Help / onboarding modal. Opened from the transport's "? Help"
//! button; a tabbed, plain-language tour of how the editor works. Mounted once;
//! visible whenever the controller's `help_open` is set.

use dominator::{clone, events, html, Dom};
use futures_signals::signal::{Mutable, SignalExt};
use wasm_bindgen_futures::spawn_local;

use crate::controller::controller;

/// One help section: a heading plus its paragraphs (a paragraph starting with
/// "•" renders as an indented bullet).
type Section = (&'static str, Vec<&'static str>);
/// One help tab: a label and its ordered sections.
type Tab = (&'static str, Vec<Section>);

pub fn render() -> Dom {
    let ctrl = controller();
    html!("div", {
        .child_signal(ctrl.help_open.signal().map(|open| if open { Some(view()) } else { None }))
    })
}

/// Index of the "Using the MCP" tab — so other surfaces (the MCP connect modal)
/// can deep-link straight to it via [`open_help_at`](crate::controller::EditorController::open_help_at).
pub fn mcp_tab_index() -> usize {
    tabs()
        .iter()
        .position(|(label, _)| *label == "Using the MCP")
        .unwrap_or(0)
}

/// The tabs, each `(label, sections)` where a section is `(heading, paragraphs)`
/// and a paragraph starting with "•" renders as an indented bullet. Kept honest
/// with the actual UI — update alongside behavior changes.
fn tabs() -> Vec<Tab> {
    vec![
        (
            "Overview",
            vec![
                (
                    "What it is",
                    vec![
                        "A modular WebAudio studio. You wire a graph of nodes — \
                         oscillators, filters, gains, effects, even your own WASM \
                         DSP — and hear it instantly. There is no instrument vs. \
                         sequence distinction: everything is a Sound (a node graph). \
                         What a Sound does is decided by what's in it, and which \
                         wires are allowed is decided by a typed port matrix.",
                    ],
                ),
                (
                    "The two views",
                    vec![
                        "• Sounds — the node canvas. Build anything here: a synth, a \
                         drum, an FX chain, or a whole sequencer-driven song. Every \
                         node is always available.",
                        "• Arrange — a DAW-style audio timeline: bounce a Sound to an \
                         audio clip, then place clips on tracks (waveforms, \
                         sample-accurate scrub + blade).",
                    ],
                ),
                (
                    "A typical flow",
                    vec![
                        "1. In Sounds, build a patch or a sequencer-driven passage. \
                         2. ▶ Play to audition it (a Sound with a sequencer/Output \
                         plays as a song; the rest audition). 3. Switch to Arrange, \
                         Bounce the Sound in the Assets panel, and place its clip on a \
                         track. 4. Arrange clips, then Export to a .wav.",
                    ],
                ),
                (
                    "Samples & tabs",
                    vec![
                        "The tabs across the top are your Sounds; the view toggle \
                         switches the Sounds canvas and the Arrange timeline. “+” \
                         adds one, double-click a tab to rename, and ★ marks the one \
                         a saved project opens first.",
                    ],
                ),
                (
                    "Add & wire nodes",
                    vec![
                        "• Drag a node from the left palette onto the canvas (or \
                         click it to drop at center); the search box filters.",
                        "• Audio: drag a node's output port (right edge) onto \
                         another's input port (left edge).",
                        "• Modulation: drag an output onto a small parameter dot \
                         (left edge, on that param's row) to add a signal to it.",
                        "• Right-click a wire to delete it; right-click a node to \
                         clone or delete it. Select a node to edit it in the \
                         right-hand inspector; its ? opens reference docs.",
                    ],
                ),
            ],
        ),
        (
            "Using the MCP",
            vec![
                (
                    "What it is",
                    vec![
                        "awsm-audio ships an MCP server: it lets an AI agent (or any \
                         MCP client) drive this editor — add and wire nodes, sequence \
                         notes, bounce Sounds, arrange clips, and render audio — \
                         entirely through typed tool calls. The agent works the same \
                         canvas you do; you watch it build in real time.",
                        "The loop has three pieces, all required: the MCP server, an \
                         attached editor tab (this page — the audio truth), and your \
                         agent. Set them up in that order.",
                    ],
                ),
                (
                    "1 · Install the server",
                    vec![
                        "Prebuilt binaries — no toolchain needed.",
                        "• macOS / Linux:",
                        "$ curl --proto '=https' --tlsv1.2 -LsSf https://github.com/awsm-fun/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.sh | sh",
                        "• Windows (PowerShell):",
                        "$ powershell -ExecutionPolicy Bypass -c \"irm https://github.com/awsm-fun/awsm-audio/releases/latest/download/awsm-audio-mcp-installer.ps1 | iex\"",
                        "• From source (needs Rust):",
                        "$ cargo install --git https://github.com/awsm-fun/awsm-audio awsm-audio-mcp",
                    ],
                ),
                (
                    "2 · Run it in a terminal",
                    vec![
                        "Start the server and leave it running (it defaults to port \
                         9171):",
                        "$ awsm-audio-mcp",
                        "It listens on http://127.0.0.1:9171 — /mcp for agents, plus a \
                         WebSocket the editor dials out to. Pass --port to change it.",
                    ],
                ),
                (
                    "3 · Connect this editor",
                    vec![
                        "Two ways to attach the tab to a running server:",
                        "• Click the MCP button in the top bar and enter the server's \
                         host:port (e.g. 127.0.0.1:9171), then Connect.",
                        "• Or open the editor with a ?mcp= parameter to auto-connect:",
                        "$ http://localhost:9170/?mcp=127.0.0.1:9171",
                        "The ?mcp= value is a bare host:port. For a TLS-terminated \
                         remote server, add &tls=true (or tick the box in the connect \
                         modal). When attached, the MCP button shows “MCP ✓” and a \
                         🤖 working / idle chip tells you when the agent is editing.",
                    ],
                ),
                (
                    "4 · Point your agent at it",
                    vec![
                        "It's a streamable-HTTP MCP server, so every MCP client \
                         connects to the same URL:",
                        "$ http://127.0.0.1:9171/mcp",
                        "Register it the way your agent does — for example:",
                        "• Claude Code:",
                        "$ claude mcp add --transport http awsm-audio http://127.0.0.1:9171/mcp",
                        "• Codex: add it with `codex mcp add` (run `codex mcp --help` \
                         for the exact form), pointing at the URL above.",
                        "• Cursor / others: add an HTTP MCP server with that URL, or \
                         drop this into the agent's MCP config (e.g. an .mcp.json):",
                        "$ { \"mcpServers\": { \"awsm-audio\": { \"type\": \"http\", \"url\": \"http://127.0.0.1:9171/mcp\" } } }",
                        "Then just ask: “build me a techno loop”, “add a reverb to \
                         this”, “bounce it and lay out an arrangement”. The agent \
                         discovers every node and command from the server's typed \
                         schema — no guesswork.",
                    ],
                ),
                (
                    "5 · Watch it work (Live work display)",
                    vec![
                        "While the agent drives, the editor shows what's happening: \
                         the 🤖 chip names the current action (“Bouncing “Bass””, \
                         “Connecting nodes”), the canvas follows the agent to \
                         whatever sample it touches (opening the arranger for \
                         arrangements) and flashes the node it just changed, and an \
                         optional floating feed logs recent actions.",
                        "All three are toggles under “Live work display” in the MCP \
                         connect modal. The action label and follow are on by \
                         default; the feed is off (it can crowd the canvas). \
                         Settings persist per browser tab, so two open projects \
                         never fight over them.",
                    ],
                ),
            ],
        ),
        (
            "Sounds",
            vec![
                (
                    "Design a sound",
                    vec![
                        "Wire sources (oscillator, noise, buffer, media) through \
                         effects (filter, delay, compressor, wave shaper, convolver, \
                         a WASM worklet) to taste. An unconnected node still reaches \
                         the speakers, so you hear progress as you build. Every node \
                         is in the palette — the typed port matrix stops nonsensical \
                         wires (a control output won't land on an audio input).",
                    ],
                ),
                (
                    "Hear it",
                    vec![
                        "▶ Play is content-aware: a Sound with a sequencer or an \
                         Output node performs as a song; anything else auditions the \
                         patch (envelopes fire from the start; a drone sustains). ⟳ \
                         Loop repeats it.",
                    ],
                ),
                (
                    "Expose, reuse & nest",
                    vec![
                        "Drop an Input and wire it to a parameter to publish a named \
                         knob; an Output marks where audio leaves. Drop a Sound node \
                         to embed (or trigger) another Sound — wire audio through it, \
                         or fire it from a sequencer's trigger inlet. Select nodes and \
                         Ctrl/Cmd-G groups them into a new sub-Sound.",
                    ],
                ),
                (
                    "Sequence notes",
                    vec![
                        "Drop a Note Sequencer, load a .mid or open its piano roll, \
                         and draw notes. Each distinct sound is a keyed output (a \
                         melodic track = one output; a drum track = one per note). \
                         Drag an output onto a Sound's green trigger inlet so it plays \
                         that sound; run the audio through a Bus into an Output.",
                    ],
                ),
                (
                    "Piano roll",
                    vec![
                        "Drag on the grid to draw a note (drag right for length); \
                         click to delete, drag to move/resize, scroll over a note for \
                         velocity. The tab strip switches tracks; the melodic/drums \
                         button changes a track's type.",
                        "• “+ bar” / “− bar” lengthen or shorten the song.",
                        "• Drag the two top handles to set the play range (playback \
                         and looping limit to that window). (Also start / stop in the \
                         inspector.)",
                    ],
                ),
                (
                    "Automate parameters",
                    vec![
                        "Drop a Control Sequencer: each lane is an output you drag \
                         onto any node's parameter dot. Draw the lane's value over \
                         time (or import CC automation from a .mid) and it sweeps \
                         that parameter during playback.",
                        "• Click a point to cycle its curve — step (hold then jump), \
                         linear, exponential, or smooth (S-curve); the plot draws the \
                         real shape. Alt/⌘-click a point to delete it.",
                    ],
                ),
            ],
        ),
        (
            "Arrange",
            vec![
                (
                    "Bounce, then arrange",
                    vec![
                        "The Arrange view is an audio timeline. You don't sequence live \
                         here — you bounce a Sound to audio, then place that clip. The \
                         left Assets panel lists every Sound with its status: not \
                         bounced, bounced, or ● dirty (edited since its last bounce).",
                        "Click Bounce on a Sound to render it offline to an audio clip \
                         (its length is detected automatically). Edit the Sound later \
                         and it shows ‘dirty’ — hit Re-bounce to refresh.",
                    ],
                ),
                (
                    "Place & draw clips",
                    vec![
                        "Place a bounced Sound on a track three ways: (1) drag it from \
                         the Assets panel onto any lane; (2) right-click it → “Place at \
                         playhead” (drops on the selected track); (3) click it to arm \
                         the ✎ draw tool, then act on a lane. Double-click or right-click \
                         → “Go to” opens the Sound's graph to edit it.",
                        "• ↖ pointer (the default tool): drag a clip to move it (up/down \
                         to another track too); drag its right edge to trim the end or \
                         its left edge to trim the start (offsets into the buffer). You \
                         can't trim past the audio — unless the clip loops.",
                        "• ✎ draw: click an empty lane to drop the whole bounce, or drag \
                         to draw out a shorter slice — handy when a long Sound shouldn't \
                         fill the bar.",
                        "• ✂ blade: click a clip to split it (sample accurate).",
                        "• Snap (toolbar) cycles off → clip → beat → bar. “clip” snaps \
                         to other clips' edges only; “beat”/“bar” snap to the grid and \
                         still magnet to nearby clip edges; “off” is free.",
                        "• Right-click a clip: toggle Loop, Open source, or Delete. A \
                         looping clip repeats its buffer to fill whatever length you \
                         drag it to (its waveform shows the repeats).",
                        "• Select clips (click one, or drag a rectangle over several in \
                         pointer mode) then Copy / Cut / Paste (⌘/Ctrl+C / X / V) — \
                         paste keeps each clip's track + relative timing, landing at the \
                         playhead. Blade + copy/paste = rearrange a loop from one bounce.",
                        "• ⇔ stretch (S): drag a clip to play it faster / slower — it \
                         time-stretches to the new length (pitch shifts with speed; the \
                         clip shows e.g. 1.50×).",
                        "• Shortcuts: V pointer, D draw, B blade, S stretch; Delete \
                         removes the selected clip.",
                    ],
                ),
                (
                    "Tracks",
                    vec![
                        "Click a track header (or any lane) to select that track \
                         (highlighted) — it's the target for “Place at playhead”.",
                        "Each track has a name, a volume slider, mute, and solo. Solo \
                         is exclusive: if any track is soloed, only soloed tracks play \
                         (mute still wins). Snap cycles bar / beat / off.",
                    ],
                ),
                (
                    "Playback & scrub",
                    vec![
                        "Click or drag the ruler (or an empty lane) to move the playhead \
                         anywhere — playback (and the loop region) starts from there, \
                         sample-accurately, because clips are real audio.",
                        "Space (or ▶) plays / pauses — ⏸ Pause keeps the playhead where \
                         it is so Space resumes from there; ■ Stop returns to where \
                         playback started. ⟳ Loop repeats the region from the start to \
                         the timeline end; “± 4s” sets the timeline length.",
                    ],
                ),
            ],
        ),
        (
            "Nodes",
            vec![
                (
                    "Per-node help & editors",
                    vec![
                        "Every palette item and node has a ? that opens a short \
                         description with an MDN link. Several nodes have richer \
                         editors in the inspector:",
                    ],
                ),
                (
                    "Sound-shaping",
                    vec![
                        "• Wave Shaper: pick a shape (tanh / hard clip / fold) + \
                         drive, or choose “custom” and draw your own transfer curve.",
                        "• Oscillator: the “custom” type lets you draw the harmonic \
                         amplitudes to design a waveform.",
                        "• IIR Filter: use the designer (response + cutoff + Q) to \
                         generate coefficients, or type raw lists; a plot shows the \
                         magnitude response.",
                    ],
                ),
                (
                    "Routing & analysis",
                    vec![
                        "• Convolver: reverb from an impulse-response file, or leave \
                         it empty and set the reverb length for a synthesized space.",
                        "• Channel Splitter / Merger: set the channel count to fan a \
                         signal out to / in from separate channels.",
                        "• Analyser: a live oscilloscope of the signal passing \
                         through it (shows while playing).",
                    ],
                ),
                (
                    "Sources & DSP",
                    vec![
                        "• Buffer Source loads an audio clip; Media Element plays a \
                         URL; Media Stream taps the microphone.",
                        "• Audio Worklet loads a .wasm module (the awsm-audio worklet \
                         ABI); its parameters are auto-discovered and become \
                         editable, modulatable knobs.",
                        "• Any automatable parameter can be drawn as an envelope over \
                         time — click the plot to add breakpoints, drag to shape.",
                    ],
                ),
            ],
        ),
        (
            "Save & Export",
            vec![
                (
                    "Save / Open / New",
                    vec![
                        "Save writes the whole project as a hand-editable .toml \
                         (graph, layout, and embedded assets); Open restores it. New \
                         clears to an empty project.",
                    ],
                ),
                (
                    "Examples",
                    vec![
                        "Load example… browses the built-ins, grouped into Sequenced \
                         Songs, WASM AudioWorklet, and Built-in. Try “song” for a \
                         full arrangement to learn from.",
                    ],
                ),
                (
                    "Export to WAV",
                    vec![
                        "The ⤓ Export button renders the active sample offline and \
                         downloads a .wav. A Sound exports its full patch; an \
                         Arrangement renders its clip timeline — the whole thing, or \
                         just the marked region when loop/export markers are set on \
                         the ruler. Offline render is faster than realtime and \
                         deterministic.",
                    ],
                ),
                (
                    "Shortcuts",
                    vec![
                        "• Delete/Backspace: remove selection · Ctrl/Cmd-C/V/D: copy \
                         / paste / duplicate · Ctrl/Cmd-A: select all · Ctrl/Cmd-G: \
                         group.",
                        "• Shift-drag: box-select · wheel: zoom to cursor · Fit: \
                         frame the graph · Ctrl/Cmd-Z / Shift-Z: undo / redo.",
                    ],
                ),
            ],
        ),
    ]
}

fn view() -> Dom {
    let tabs = tabs();
    let active = Mutable::new(controller().help_tab.get());
    // Two siblings: a click-to-close backdrop behind the panel, and a
    // pointer-events-transparent centering layer holding the panel (so panel
    // clicks never bubble to close — dominator's Click propagation doesn't honor
    // stop_propagation reliably here).
    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        .style("z-index", "1200")
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("background", "oklch(0 0 0 / 0.62)")
            .style("backdrop-filter", "blur(2px)")
            .style_unchecked("-webkit-backdrop-filter", "blur(2px)")
            .event(|_: events::Click| controller().close_help_page())
        }))
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("pointer-events", "none")
            .child(html!("div", {
                .style("pointer-events", "auto")
                // The page disables text selection (UI chrome); re-enable it inside
                // the help panel so instructions/commands can be copied. WebKit/Blink
                // need the prefixed property, which `body` set to `none`.
                .style("user-select", "text")
                .style_unchecked("-webkit-user-select", "text")
                .style("width", "min(680px, 92vw)")
                .style("max-height", "84vh")
                .style("display", "flex")
                .style("flex-direction", "column")
                .style("margin", "0 16px")
                .style("border-radius", "12px")
                .style("background", "var(--bg-2)")
                .style("border", "1px solid oklch(0.5 0.06 230)")
                .style("box-shadow", "0 24px 70px oklch(0 0 0 / 0.6)")
                .style("font-size", "13.5px")
                .style("line-height", "1.6")
                // Header (fixed).
                .child(html!("div", {
                    .style("display", "flex")
                    .style("align-items", "center")
                    .style("justify-content", "space-between")
                    .style("padding", "18px 22px 10px")
                    .child(html!("div", {
                        .child(html!("h2", { .style("margin", "0").style("font-size", "17px").style("font-weight", "650").text("Using awsm-audio") }))
                        .child(html!("div", {
                            .style("margin-top", "2px")
                            .style("font-size", "12px")
                            .style("color", "var(--text-2)")
                            .text("A modular WebAudio studio — playable by you or by an AI agent.")
                        }))
                    }))
                    .child(html!("button", {
                        .style_unchecked("border", "none")
                        .style("background", "transparent")
                        .style("color", "var(--text-2)")
                        .style("font-size", "17px")
                        .style("cursor", "pointer")
                        .style("line-height", "1")
                        .text("×")
                        .event(|_: events::Click| controller().close_help_page())
                    }))
                }))
                // Tab bar (fixed).
                .child(html!("div", {
                    .style("display", "flex")
                    .style("flex-wrap", "wrap")
                    .style("gap", "4px")
                    .style("padding", "0 22px 10px")
                    .style("border-bottom", "1px solid var(--line)")
                    .children(tabs.iter().enumerate().map(|(i, (label, _))| {
                        html!("button", {
                            .style("padding", "5px 13px")
                            .style("cursor", "pointer")
                            .style("font-size", "13px")
                            .style("border-radius", "6px")
                            .style("color", "inherit")
                            .style_unchecked("border", "none")
                            .style_signal("background", active.signal().map(move |a| {
                                if a == i { "oklch(0.42 0.09 230)".to_string() } else { "transparent".to_string() }
                            }))
                            .style_signal("font-weight", active.signal().map(move |a| {
                                if a == i { "700".to_string() } else { "500".to_string() }
                            }))
                            .text(label)
                            .event(clone!(active => move |_: events::Click| active.set(i)))
                        })
                    }))
                }))
                // Body (scrolls); swaps with the active tab.
                .child(html!("div", {
                    .style("overflow-y", "auto")
                    .style("padding", "14px 22px 20px")
                    .child_signal(active.signal().map(clone!(tabs => move |a| {
                        let sections = tabs.get(a).map(|(_, s)| s.clone()).unwrap_or_default();
                        Some(html!("div", {
                            .children(sections.into_iter().map(|(heading, items)| section(heading, items)))
                        }))
                    })))
                }))
                // Footer (fixed): credits + repo link.
                .child(footer())
            }))
        }))
    })
}

/// The modal footer: attribution and a link to the source repository.
fn footer() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("gap", "12px")
        .style("flex-wrap", "wrap")
        .style("padding", "11px 22px")
        .style("border-top", "1px solid var(--line)")
        .style("font-size", "12px")
        .style("color", "var(--text-2)")
        .child(html!("span", {
            .text("Created by David Komer")
        }))
        .child(html!("a", {
            .attr("href", "https://github.com/awsm-fun/awsm-audio")
            .attr("target", "_blank")
            .attr("rel", "noopener noreferrer")
            .attr("title", "Source on GitHub")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("gap", "5px")
            .style("color", "var(--accent-bright)")
            .style("text-decoration", "none")
            .style("font-weight", "600")
            .child(html!("span", { .text("github.com/awsm-fun/awsm-audio") }))
            .child(html!("span", { .style("font-size", "11px").style("opacity", "0.85").text("↗") }))
        }))
    })
}

fn section(heading: &str, items: Vec<&'static str>) -> Dom {
    html!("div", {
        .style("margin-bottom", "14px")
        .child(html!("h3", {
            .style("margin", "0 0 5px")
            .style("font-size", "15px")
            .style("font-weight", "650")
            .style("color", "var(--accent-bright)")
            .text(heading)
        }))
        .children(items.into_iter().map(paragraph))
    })
}

/// One help paragraph. A leading "$ " marks a copyable code block (commands,
/// URLs, config); a leading "•" indents as a bullet; anything else is prose.
fn paragraph(t: &'static str) -> Dom {
    if let Some(code) = t.strip_prefix("$ ") {
        return code_block(code);
    }
    html!("p", {
        .style("margin", "0 0 5px")
        .style("color", "var(--text-1)")
        .style("padding-left", if t.starts_with('•') { "10px" } else { "0" })
        .text(t)
    })
}

/// A copyable code block: the monospaced command/URL plus a clipboard button in
/// its top-right corner that writes the text to the OS clipboard (the commands
/// are long and awkward to select by hand). The button flashes ✓ on success.
fn code_block(code: &'static str) -> Dom {
    let copied = Mutable::new(false);
    html!("div", {
        .style("position", "relative")
        .style("margin", "4px 0 8px")
        .child(html!("pre", {
            .style("margin", "0")
            // Extra right padding so long lines don't run under the copy button.
            .style("padding", "8px 40px 8px 11px")
            .style("background", "var(--bg-1)")
            .style("border", "1px solid var(--line)")
            .style("border-radius", "6px")
            .style("font-family", "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace")
            .style("font-size", "12px")
            .style("line-height", "1.5")
            .style("color", "var(--text-0)")
            .style("white-space", "pre-wrap")
            .style("overflow-wrap", "anywhere")
            .style("user-select", "all")
            .style_unchecked("-webkit-user-select", "all")
            .text(code)
        }))
        .child(html!("button", {
            .style("position", "absolute")
            .style("top", "6px")
            .style("right", "6px")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("width", "26px")
            .style("height", "26px")
            .style("padding", "0")
            .style("cursor", "pointer")
            .style("border-radius", "5px")
            .style("background", "var(--bg-2)")
            .style("border", "1px solid var(--line)")
            .style("font-size", "13px")
            .style("line-height", "1")
            .style_signal("color", copied.signal().map(|c| {
                if c { "var(--accent-bright)".to_string() } else { "var(--text-2)".to_string() }
            }))
            .attr("title", "Copy to clipboard")
            .text_signal(copied.signal().map(|c| if c { "✓" } else { "📋" }))
            .event(clone!(copied => move |_: events::Click| {
                copy_to_clipboard(code);
                copied.set(true);
                spawn_local(clone!(copied => async move {
                    gloo_timers::future::TimeoutFuture::new(1200).await;
                    copied.set(false);
                }));
            }))
        }))
    })
}

/// Write `text` to the OS clipboard (fire-and-forget; the returned promise is
/// driven to completion so the browser doesn't log an unhandled rejection).
fn copy_to_clipboard(text: &str) {
    if let Some(win) = web_sys::window() {
        let promise = win.navigator().clipboard().write_text(text);
        spawn_local(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
        });
    }
}

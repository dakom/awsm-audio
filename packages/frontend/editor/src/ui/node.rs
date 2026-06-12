//! A single node box: a draggable, selectable card with input ports down the
//! left edge, output ports down the right, and an inline list of editable
//! settings in its body. Every gesture/edit goes through the controller.

use std::rc::Rc;

use awsm_audio_schema::NodeKind;
use dominator::{clone, events, html, with_node, Dom, EventOptions};
use futures_signals::signal::SignalExt;
use wasm_bindgen::JsCast;

use crate::controller::{controller, DragState, EditorCommand, EditorNode};
use crate::fields::{self, Control, FieldValue};
use crate::ports::{self, PortSide, NODE_WIDTH, PORT_R};

pub fn render(node: Rc<EditorNode>) -> Dom {
    use crate::controller::BoundaryPort;
    let ctrl = controller();
    let kind = node.kind.borrow();
    // Port counts: boundary nodes have one port; Sample refs mirror the
    // referenced sample's inlets/outlets; everything else is static.
    let (ins, outs) = match node.boundary {
        Some(BoundaryPort::Inlet) => (0, 1),
        Some(BoundaryPort::Outlet) => (1, 0),
        None => match &*kind {
            NodeKind::Sample(sr) => controller().sample_io(sr.sample),
            other => ports::port_counts(other),
        },
    };
    let title = ports::kind_label(&kind);
    let node_fields = if node.boundary.is_some() {
        vec![]
    } else {
        fields::fields(&kind)
    };
    // Modulation inlet dots: one per automatable field, placed on that field's
    // body row (left-edge row `ins + field_index`) — the SAME index the wire
    // renderer uses, so a wire always lands exactly on the dot. (param name,
    // left-edge row index).
    let mod_dots: Vec<(awsm_audio_schema::ParamId, u32)> = node_fields
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            f.modulation
                .map(|m| (awsm_audio_schema::ParamId::from(m), ins + i as u32))
        })
        .collect();
    let min_height = ports::node_height_io(ins, outs, 0);
    // Audio-file picker: buffer sources load a clip; convolvers load an IR.
    let is_convolver = matches!(&*kind, NodeKind::Convolver(_));
    let wants_audio = matches!(
        &*kind,
        NodeKind::AudioBufferSource(_) | NodeKind::Convolver(_)
    );
    let is_worklet = matches!(&*kind, NodeKind::AudioWorklet(_));
    // The worklet's currently-bound module asset (if any), for the picker label.
    let worklet_module = if let NodeKind::AudioWorklet(w) = &*kind {
        w.module
    } else {
        None
    };
    let is_mic = matches!(&*kind, NodeKind::MediaStreamSource(_));
    let is_boundary = node.boundary.is_some();
    let sample_ref = if let NodeKind::Sample(sr) = &*kind {
        Some(sr.sample)
    } else {
        None
    };
    // For a Sample-ref node, the port tooltips are the referenced sample's
    // inlet/outlet names — so you can tell which input drives what.
    let (in_names, out_names) = match &*kind {
        NodeKind::Sample(sr) => controller().sample_port_names(sr.sample),
        // Sequencer outputs are labelled by their sound / lane / zone.
        NodeKind::NoteSequencer(s) => (
            Vec::new(),
            s.outputs.iter().map(|o| o.label.clone()).collect(),
        ),
        NodeKind::ControlSequencer(s) => (
            Vec::new(),
            s.lanes.iter().map(|l| l.label.clone()).collect(),
        ),
        _ => (Vec::new(), Vec::new()),
    };
    // A Sound-reference node can be triggered: its first input port is the
    // trigger inlet (where a sequencer's keyed output binds). It coexists with
    // the referenced sound's audio inlets.
    let trigger_inlet = sample_ref.is_some();
    let node_id = node.id;
    drop(kind);

    html!("div", {
        .class("node")
        .style("position", "absolute")
        .style("width", &format!("{NODE_WIDTH}px"))
        .style("min-height", &format!("{min_height}px"))
        .style("box-sizing", "border-box")
        .style("border-radius", "8px")
        .style("background", "var(--bg-2)")
        .style("box-shadow", "0 6px 18px oklch(0 0 0 / 0.4)")
        .style("font-size", "12px")
        // `user-select` needs a vendor prefix in older Safari, where dominator's
        // validator would reject the unprefixed form — set it unchecked.
        .style_unchecked("user-select", "none")
        .style("padding-bottom", "8px")
        .style_signal("left", node.pos.signal().map(|p| format!("{}px", p.0)))
        .style_signal("top", node.pos.signal().map(|p| format!("{}px", p.1)))
        .style_signal("border", node.selected.signal().map(|s| {
            if s {
                "1px solid var(--accent-bright)".to_string()
            } else {
                "1px solid var(--line-strong)".to_string()
            }
        }))
        .style_signal("box-shadow", node.selected.signal().map(|s| {
            if s {
                "0 0 0 2px var(--accent-line), 0 6px 18px oklch(0 0 0 / 0.4)".to_string()
            } else {
                "0 6px 18px oklch(0 0 0 / 0.4)".to_string()
            }
        }))
        // MCP auto-follow spotlight: a one-shot glow (no `forwards`, so it reverts
        // to the box-shadow above) when the agent touches this node.
        .style_signal("animation", crate::mcp_activity::spotlight().signal().map(move |spot| {
            if spot == Some(node_id) {
                "mcp-spotlight 1.1s ease-out".to_string()
            } else {
                "none".to_string()
            }
        }))
        // Press the body: select, then begin a (possibly group) move drag.
        .event(clone!(node, ctrl => move |e: events::PointerDown| {
            // A press on a port (wire endpoint) must NOT start a node drag. The
            // port's own `stop_propagation` isn't reliable in this dominator
            // build, so guard explicitly by inspecting the pressed element.
            if target_in_port(&e) {
                return;
            }
            let (wx, wy) = ctrl.client_to_world(e.x(), e.y());
            // Shift toggles into the selection; clicking an unselected node
            // selects just it; clicking an already-selected node keeps the
            // whole selection (so the group drags together).
            if e.shift_key() {
                ctrl.dispatch(EditorCommand::SelectNodes { ids: vec![node.id], additive: true });
            } else if !node.selected.get() {
                ctrl.dispatch(EditorCommand::SelectNodes { ids: vec![node.id], additive: false });
            }
            // Every selected node moves rigidly with the cursor.
            *ctrl.drag.borrow_mut() = Some(DragState::Node {
                items: ctrl.selected_drag_items(node.id, wx, wy),
            });
            e.stop_propagation();
        }))
        // Right-click: select the node and open its context menu.
        .event_with_options(&EventOptions::preventable(), clone!(node, ctrl => move |e: events::ContextMenu| {
            e.prevent_default();
            e.stop_propagation();
            ctrl.dispatch(EditorCommand::SelectNodes { ids: vec![node.id], additive: false });
            ctrl.open_context_menu(node.id, e.x(), e.y());
        }))
        // Header.
        .child(html!("div", {
            .style("height", &format!("{}px", ports::HEADER_H))
            .style("display", "flex")
            .style("align-items", "center")
            .style("padding", "0 10px")
            .style("border-radius", "8px 8px 0 0")
            .style("background", if is_boundary {
                "oklch(0.42 0.1 150)"  // boundary ports get a green header
            } else if sample_ref.is_some() {
                "oklch(0.4 0.08 300)"  // sample refs get a violet header
            } else {
                "var(--line)"
            })
            .style("font-weight", "600")
            .style("cursor", "grab")
            .text_signal(node.label.signal_cloned().map(move |l| {
                if l.trim().is_empty() { title.to_string() } else { l }
            }))
        }))
        // Editable settings. Field rows are a fixed PORT_ROW_H tall with no gap,
        // so each row sits on the same grid as the left-edge ports — that's what
        // keeps a param's modulation dot (and the wire landing on it) exactly on
        // its row, no matter the field's content.
        .child(html!("div", {
            .style("display", "flex")
            .style("flex-direction", "column")
            // Top padding aligns field row 0 to left-edge row `reserve` (the audio
            // inputs sit in rows 0..reserve). +2 centers the row on the port grid.
            // Sample-ref counts outlets too so its picker clears both edges.
            .apply(move |b| {
                let reserve = if sample_ref.is_some() { ins.max(outs) } else { ins };
                b.style(
                    "padding",
                    format!("{}px 10px 0", 2.0 + reserve as f64 * ports::PORT_ROW_H),
                )
            })
            .children(node_fields.into_iter().map(clone!(node => move |f| {
                field_row(node.clone(), f)
            })))
            // Buffer sources / convolvers get an audio-file picker.
            .apply(clone!(node => move |b| {
                if wants_audio {
                    b.child(file_row(node.clone(), if is_convolver { "impulse response" } else { "audio file" }))
                } else { b }
            }))
            // WASM worklets get a .wasm picker.
            .apply(clone!(node => move |b| {
                if is_worklet { b.child(wasm_file_row(node.clone(), worklet_module)) } else { b }
            }))
            // Media-stream sources get an "enable microphone" button.
            .apply(clone!(node => move |b| if is_mic { b.child(mic_row()) } else { b }))
            // Sample-ref nodes get a sample picker.
            .apply(clone!(node => move |b| {
                match sample_ref {
                    Some(current) => b.child(sample_picker(node.clone(), current)),
                    None => b,
                }
            }))
        }))
        // Audio input ports (left edge, rows 0..ins). On an instrument-ref in the
        // Sequences view, port 0 is the trigger inlet.
        .children((0..ins).map(clone!(node, in_names => move |i| {
            let name = if trigger_inlet && i == 0 {
                Some("trigger".to_string())
            } else {
                in_names.get(i as usize).cloned()
            };
            render_port(node.clone(), PortSide::In, i, name, trigger_inlet && i == 0)
        })))
        // Modulation inlet dots (left edge, on each automatable field's row).
        .children(mod_dots.into_iter().map(clone!(node => move |(param, row)| {
            render_param_inlet(node.clone(), row, param)
        })))
        // Output ports (right edge).
        .children((0..outs).map(clone!(node, out_names => move |j| {
            render_port(node.clone(), PortSide::Out, j, out_names.get(j as usize).cloned(), false)
        })))
    })
}

/// True if a pointer event's target is (or sits inside) a port dot — used to
/// keep a port press from also triggering the node-body move drag.
fn target_in_port(e: &events::PointerDown) -> bool {
    e.target()
        .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
        .and_then(|el| el.closest(".node-port").ok().flatten())
        .is_some()
}

/// A modulation inlet dot on the node's left edge at left-edge row `index` —
/// the same grid the audio inputs and the wire renderer use, so the dot and any
/// wire landing on it always coincide. Releasing a dragged wire here connects
/// the source into this node's `param`.
fn render_param_inlet(node: Rc<EditorNode>, index: u32, param: awsm_audio_schema::ParamId) -> Dom {
    let (ox, oy) = ports::port_offset(PortSide::In, index);
    html!("div", {
        .class("node-port")
        .class("node-port-in")
        // A wire here SUMS into the param (WebAudio params can't be "set" by a
        // connection — the signal adds to the param's own value).
        .attr("title", &format!("modulate {} (adds to its value)", param.0))
        .style("position", "absolute")
        // border-box so the 2px border is included — the dot's visual center
        // lands exactly on the (ox, oy) grid point the wire endpoint also uses.
        .style("box-sizing", "border-box")
        .style("width", &format!("{}px", PORT_R * 2.0))
        .style("height", &format!("{}px", PORT_R * 2.0))
        .style("left", &format!("{}px", ox - PORT_R))
        .style("top", &format!("{}px", oy - PORT_R))
        .style("border-radius", "50%")
        .style("background", "oklch(0.78 0.16 70)")
        .style("border", "2px solid var(--bg-1)")
        .style("cursor", "crosshair")
        .event(|e: events::PointerDown| e.stop_propagation())
        .event(clone!(node, param => move |_: events::PointerUp| {
            controller().commit_modulation(node.clone(), param.clone());
        }))
    })
}

/// One labelled control row inside a node. Fixed PORT_ROW_H tall (no gap) so the
/// row aligns with its modulation-inlet dot on the left-edge port grid.
fn field_row(node: Rc<EditorNode>, f: fields::Field) -> Dom {
    let key = f.key;
    html!("label", {
        .style("height", &format!("{}px", ports::PORT_ROW_H))
        .style("display", "flex")
        .style("align-items", "center")
        .style("justify-content", "space-between")
        .style("gap", "6px")
        .style("opacity", "0.9")
        // Don't let clicking a control start a node drag / selection.
        .event(|e: events::PointerDown| e.stop_propagation())
        .child(html!("span", {
            .style("opacity", "0.6")
            .style("white-space", "nowrap")
            .text(f.label)
        }))
        .child(match f.control {
            Control::Number(v) => number_input(node, key, v),
            Control::Bool(b) => bool_input(node, key, b),
            Control::Choice { value, options } => {
                if options.is_empty() {
                    text_input(node, key, &value)
                } else {
                    select_input(node, key, &value, options)
                }
            }
        })
    })
}

/// Audio-file picker for a buffer-source / convolver node. Picking a file
/// decodes it (via the controller) and points the node's buffer at the result.
fn file_row(node: Rc<EditorNode>, label: &str) -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "2px")
        .event(|e: events::PointerDown| e.stop_propagation())
        .child(html!("span", {
            .style("opacity", "0.6")
            .text(label)
        }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "file")
            .attr("accept", "audio/*,.wav,.mp3,.flac,.m4a,.ogg,.opus")
            .style("font-size", "11px")
            .style("width", "100%")
            .style("color", "var(--text-2)")
            .with_node!(input => {
                .event(clone!(node, input => move |_: events::Change| {
                    if let Some(file) = input.files().and_then(|f| f.get(0)) {
                        controller().load_audio_file(node.id, file);
                    }
                }))
            })
        }))
    })
}

/// `.wasm` DSP module picker for an AudioWorklet node. Picking a file reads its
/// bytes, stores them as an asset, discovers its params, and points the node at
/// it (all via the controller).
fn wasm_file_row(node: Rc<EditorNode>, module: Option<awsm_audio_schema::AssetId>) -> Dom {
    // What's currently bound: a loaded module's label, or "none".
    let loaded = module.and_then(|id| controller().wasm_module_info(id));
    let (status, present) = match loaded {
        Some((label, kind)) => (format!("✓ {label} ({kind})"), true),
        None => ("no module loaded".to_string(), false),
    };
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "2px")
        .event(|e: events::PointerDown| e.stop_propagation())
        .child(html!("span", {
            .style("opacity", "0.6")
            .text(".wasm module")
        }))
        .child(html!("span", {
            .style("font-size", "11px")
            .style("color", if present { "oklch(0.78 0.14 150)" } else { "var(--text-2)" })
            .text(&status)
        }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "file")
            .attr("accept", ".wasm,application/wasm")
            .style("font-size", "11px")
            .style("width", "100%")
            .style("color", "var(--text-2)")
            .with_node!(input => {
                .event(clone!(node, input => move |_: events::Change| {
                    if let Some(file) = input.files().and_then(|f| f.get(0)) {
                        controller().load_wasm_file(node.id, file);
                    }
                }))
            })
        }))
    })
}

/// Sample picker for a Sample-reference node: choose which sub-sample to embed.
fn sample_picker(node: Rc<EditorNode>, current: awsm_audio_schema::SampleId) -> Dom {
    let id = node.id;
    let others = controller().other_samples(controller().active_sample());
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("gap", "2px")
        .event(|e: events::PointerDown| e.stop_propagation())
        // No "sample" label: the dropdown value (and the named ports) say it all.
        .child(html!("select" => web_sys::HtmlSelectElement, {
            .style("width", "100%")
            .style("box-sizing", "border-box")
            .style("background", "var(--bg-2)")
            .style("color", "inherit")
            .style("border", "1px solid var(--line-strong)")
            .style("border-radius", "4px")
            .style("padding", "2px 4px")
            .style("font-size", "11.5px")
            .children(others.into_iter().map(move |(sid, name)| {
                let selected = sid == current;
                html!("option", {
                    .attr("value", &sid.to_string())
                    .apply(move |b| if selected { b.attr("selected", "") } else { b })
                    .text(&name)
                })
            }))
            .with_node!(select => {
                .event(clone!(select => move |_: events::Change| {
                    if let Ok(sid) = select.value().parse::<awsm_audio_schema::SampleId>() {
                        controller().set_sample_ref(id, sid);
                    }
                }))
            })
        }))
    })
}

/// "Enable microphone" button for a MediaStream source node.
fn mic_row() -> Dom {
    html!("button", {
        .style("margin-top", "2px")
        .style("padding", "3px 8px")
        .style("font-size", "11.5px")
        .style("border", "1px solid var(--line-strong)")
        .style("border-radius", "5px")
        .style("background", "var(--bg-2)")
        .style("color", "inherit")
        .style("cursor", "pointer")
        .text("🎤 Enable mic")
        .event(|e: events::PointerDown| e.stop_propagation())
        .event(|_: events::Click| controller().request_mic())
    })
}

fn number_input(node: Rc<EditorNode>, key: &'static str, value: f64) -> Dom {
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "number")
        .attr("step", "any")
        .attr("value", &format_num(value))
        .style("width", "72px")
        .style("box-sizing", "border-box")
        .style("background", "var(--bg-2)")
        .style("color", "inherit")
        .style("border", "1px solid var(--line-strong)")
        .style("border-radius", "4px")
        .style("padding", "2px 5px")
        .style("font-size", "11.5px")
        .with_node!(input => {
            .event(clone!(node, input => move |_: events::Input| {
                if let Ok(v) = input.value().parse::<f64>() {
                    controller().dispatch(EditorCommand::SetField {
                        id: node.id,
                        key: key.to_string(),
                        value: FieldValue::Num(v),
                    });
                }
            }))
        })
    })
}

fn text_input(node: Rc<EditorNode>, key: &'static str, value: &str) -> Dom {
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "text")
        .attr("value", value)
        .style("width", "92px")
        .style("box-sizing", "border-box")
        .style("background", "var(--bg-2)")
        .style("color", "inherit")
        .style("border", "1px solid var(--line-strong)")
        .style("border-radius", "4px")
        .style("padding", "2px 5px")
        .style("font-size", "11.5px")
        .with_node!(input => {
            .event(clone!(node, input => move |_: events::Input| {
                controller().dispatch(EditorCommand::SetField {
                    id: node.id,
                    key: key.to_string(),
                    value: FieldValue::Text(input.value()),
                });
            }))
        })
    })
}

fn bool_input(node: Rc<EditorNode>, key: &'static str, value: bool) -> Dom {
    html!("input" => web_sys::HtmlInputElement, {
        .attr("type", "checkbox")
        .apply(|b| if value { b.attr("checked", "") } else { b })
        .with_node!(input => {
            .event(clone!(node, input => move |_: events::Change| {
                controller().dispatch(EditorCommand::SetField {
                    id: node.id,
                    key: key.to_string(),
                    value: FieldValue::Bool(input.checked()),
                });
            }))
        })
    })
}

fn select_input(
    node: Rc<EditorNode>,
    key: &'static str,
    value: &str,
    options: &'static [&'static str],
) -> Dom {
    html!("select" => web_sys::HtmlSelectElement, {
        .style("width", "92px")
        .style("box-sizing", "border-box")
        .style("background", "var(--bg-2)")
        .style("color", "inherit")
        .style("border", "1px solid var(--line-strong)")
        .style("border-radius", "4px")
        .style("padding", "2px 4px")
        .style("font-size", "11.5px")
        .children(options.iter().map(|opt| {
            let selected = *opt == value;
            html!("option", {
                .attr("value", opt)
                .apply(move |b| if selected { b.attr("selected", "") } else { b })
                .text(opt)
            })
        }))
        .with_node!(select => {
            .event(clone!(node, select => move |_: events::Change| {
                controller().dispatch(EditorCommand::SetField {
                    id: node.id,
                    key: key.to_string(),
                    value: FieldValue::Text(select.value()),
                });
            }))
        })
    })
}

/// Trim a float to a compact display string.
fn format_num(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn render_port(
    node: Rc<EditorNode>,
    side: PortSide,
    index: u32,
    name: Option<String>,
    is_trigger: bool,
) -> Dom {
    let ctrl = controller();
    let (ox, oy) = ports::port_offset(side, index);

    html!("div", {
        .class("node-port")
        // Input ports also commit a dropped wire on pointer-up; tag them so the
        // canvas knows not to cancel the wire when a release lands here.
        .apply(move |b| if side == PortSide::In { b.class("node-port-in") } else { b })
        .style("position", "absolute")
        // border-box so the dot's visual center lands exactly on (ox, oy).
        .style("box-sizing", "border-box")
        .style("width", &format!("{}px", PORT_R * 2.0))
        .style("height", &format!("{}px", PORT_R * 2.0))
        .style("left", &format!("{}px", ox - PORT_R))
        .style("top", &format!("{}px", oy - PORT_R))
        .style("border-radius", "50%")
        // Trigger inlets are amber-green (matching trigger wires); audio is blue.
        .style("background", if is_trigger { "oklch(0.8 0.17 130)" } else { "var(--accent-bright)" })
        .style("border", "2px solid var(--bg-1)")
        .style("cursor", "crosshair")
        // A named port (a Sample-ref mirroring its sample's inlets/outlets) shows
        // its name inline next to the dot — inputs to the right, outputs to the
        // left — plus a tooltip. pointer-events:none so it never blocks wiring.
        .apply(move |b| match name.filter(|n| !n.is_empty()) {
            Some(n) => b.attr("title", &n).child(html!("span", {
                .style("position", "absolute")
                .style("top", "-2px")
                .apply(|s| if side == PortSide::In {
                    s.style("left", format!("{}px", PORT_R * 2.0 + 3.0))
                } else {
                    s.style("right", format!("{}px", PORT_R * 2.0 + 3.0))
                        .style("text-align", "right")
                })
                .style("font-size", "10.5px")
                .style("line-height", "1")
                .style("color", "var(--text-2)")
                .style("white-space", "nowrap")
                .style("pointer-events", "none")
                .text(&n)
            })),
            None => b,
        })
        // Press: never let it start a node-move/pan; on an output, begin a wire.
        .event(clone!(node, ctrl => move |e: events::PointerDown| {
            e.stop_propagation();
            if side == PortSide::Out {
                let world = ctrl.client_to_world(e.x(), e.y());
                ctrl.begin_wire(node.clone(), index, world);
            }
        }))
        // Release on an input commits a pending wire.
        .event(clone!(node, ctrl => move |_: events::PointerUp| {
            if side == PortSide::In {
                ctrl.commit_wire(node.clone(), index);
            }
        }))
    })
}

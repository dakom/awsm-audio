//! The editor UI tree. Pure view: every handler routes through the controller.
//!
//! Layout: a transport header on top, a body row of (palette sidebar | node
//! canvas), and the waveform strip along the bottom — with the help modal
//! overlaid on top of everything.

pub mod arrange;
pub mod canvas;
pub mod context_menu;
pub mod examples_modal;
pub mod help_modal;
pub mod inspector;
pub mod mcp_modal;
pub mod modal;
pub mod node;
pub mod palette;
pub mod piano_roll;
pub mod sample_picker_modal;
pub mod samples;
pub mod transport;
pub mod waveform;
pub mod wire;

use dominator::{clone, events, html, Dom, EventOptions};
use futures_signals::signal::SignalExt;

use crate::controller::controller;

/// True when focus is in a text input / select, so canvas shortcuts don't hijack
/// typing (rename, number fields, etc.).
fn is_editing() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|el| {
            matches!(
                el.tag_name().to_uppercase().as_str(),
                "INPUT" | "TEXTAREA" | "SELECT"
            )
        })
        .unwrap_or(false)
}

pub fn render() -> Dom {
    let ctrl = controller();

    html!("div", {
        .style("position", "absolute")
        .style("inset", "0")
        .style("display", "flex")
        .style("flex-direction", "column")
        // Delete/Backspace removes the selection (canvas has no text inputs of
        // its own; node-param inputs handle their own keys and don't bubble here
        // in a way that triggers this for normal typing).
        .global_event_with_options(&EventOptions::preventable(), clone!(ctrl => move |e: events::KeyDown| {
            let k = e.key();
            // Never hijack typing in a focused input/select.
            if is_editing() {
                return;
            }
            // Spacebar = play / pause (the universal transport shortcut).
            if k == " " || k == "Spacebar" {
                e.prevent_default();
                ctrl.toggle_play_pause();
                return;
            }
            // dominator's `ctrl_key()` already covers Cmd (meta) on macOS.
            let cmd = e.ctrl_key();
            if cmd && k.eq_ignore_ascii_case("z") {
                e.prevent_default();
                if e.shift_key() { ctrl.redo(); } else { ctrl.undo(); }
            } else if cmd && k.eq_ignore_ascii_case("a") {
                e.prevent_default();
                ctrl.select_all();
            } else if cmd && k.eq_ignore_ascii_case("c") {
                e.prevent_default();
                ctrl.copy_selection();
            } else if cmd && k.eq_ignore_ascii_case("v") {
                e.prevent_default();
                ctrl.paste_clipboard();
            } else if cmd && k.eq_ignore_ascii_case("d") {
                // Duplicate = copy + paste.
                e.prevent_default();
                ctrl.copy_selection();
                ctrl.paste_clipboard();
            } else if cmd && k.eq_ignore_ascii_case("g") {
                // Group = encapsulate selection into a sub-sample.
                e.prevent_default();
                ctrl.encapsulate_selection();
            } else if k == "Delete" || k == "Backspace" {
                // macOS's "delete" key reports as "Backspace"; accept both.
                e.prevent_default();
                ctrl.delete_selected();
            }
        }))
        .child(transport::render())
        .child(samples::render())
        // Body swaps by view: the Arrange timeline takes the whole area; the
        // Instruments / Sequences views show the palette + node canvas + inspector.
        .child_signal(controller().view.signal().map(|view| {
            Some(if view == awsm_audio_schema::SampleKind::Arrangement {
                arrange::render()
            } else {
                node_workspace()
            })
        }))
        .child(waveform::render())
        .child(context_menu::render())
        .child(modal::render())
        .child(examples_modal::render())
        .child(help_modal::render())
        .child(mcp_modal::render())
        .child(mcp_modal::feed_panel())
        .child(sample_picker_modal::render())
        .child(piano_roll::render())
    })
}

/// The node-editing workspace (Instruments / Sequences views): palette on the
/// left, node canvas in the middle, inspector on the right.
fn node_workspace() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex", "1")
        .style("min-height", "0")
        .child(palette::render())
        .child(html!("div", {
            .style("position", "relative")
            .style("flex", "1")
            .style("min-width", "0")
            .child(canvas::render())
        }))
        .child(inspector::render())
    })
}

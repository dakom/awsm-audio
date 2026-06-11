//! The branded top bar: the AwsmAudio mark on the left, file/edit actions in the
//! middle, and the transport cluster (play/stop/loop/record) on the right.
//! Buttons drive the controller's engine methods; their look reflects reactive
//! state. Styling resolves entirely through the design tokens in [`crate::theme`].

use dominator::{clone, events, html, Dom};
use futures_signals::map_ref;
use futures_signals::signal::SignalExt;

use crate::controller::controller;
use crate::theme::ACCENT_FG;
use crate::widgets::{brand, Btn, BtnSize, BtnVariant, Icon, IconBtn};

pub fn render() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "10px")
        .style("height", "48px")
        .style("padding", "0 12px")
        .style("background", "var(--bg-2)")
        .style("border-bottom", "1px solid var(--line)")
        .style("flex", "0 0 auto")
        .style("position", "relative")
        .style("z-index", "20")
        .child(brand())
        .child(vdivider())
        // Discovery + file actions.
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "6px")
            .child(Btn::new()
                .label("Examples")
                .icon("sparkle")
                .variant(BtnVariant::Solid)
                .size(BtnSize::Sm)
                .title("Browse built-in example projects")
                .on_click(|| controller().open_examples())
                .render())
            .child(IconBtn::new("doc").title("New empty project")
                .on_click(|| controller().new_project()).render())
            .child(IconBtn::new("folder").title("Open project directory")
                .on_click(open_project).render())
            .child(save_button())
            // Export the active sample to a .wav (offline render). Sounds export
            // via the Bounce path; Arrangements render their clip timeline (the
            // marked loop/export region if set, else start-to-finish).
            .child_signal(ctrl.view.signal().map(|view| {
                use awsm_audio_schema::SampleKind;
                let title = match view {
                    SampleKind::Arrangement => "Export this arrangement as a .wav",
                    SampleKind::Sound => "Export this Sound as a .wav",
                };
                Some(
                    IconBtn::new("download")
                        .title(title)
                        .on_click(|| controller().export_active_wav())
                        .render(),
                )
            }))
        }))
        .child(vdivider())
        // Edit / view actions.
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "2px")
            .child_signal(ctrl.can_undo.signal().map(|on| Some(
                IconBtn::new("undo").title("Undo (Ctrl/Cmd+Z)")
                    .style("opacity", if on { "1" } else { "0.35" })
                    .on_click(|| controller().undo()).render()
            )))
            .child_signal(ctrl.can_redo.signal().map(|on| Some(
                IconBtn::new("redo").title("Redo (Ctrl/Cmd+Shift+Z)")
                    .style("opacity", if on { "1" } else { "0.35" })
                    .on_click(|| controller().redo()).render()
            )))
            .child(IconBtn::new("fit").title("Fit all nodes in view")
                .on_click(|| controller().zoom_to_fit()).render())
        }))
        .child(vdivider())
        // MCP remote-control link (connect modal + reactive status).
        .child(crate::ui::mcp_modal::button())
        // Spacer.
        .child(html!("div", { .style("flex", "1") }))
        // Transient status / error message (e.g. "wire an Output to play").
        .child(html!("div", {
            .style("font-size", "12px")
            .style("color", "var(--warn)")
            .style("max-width", "360px")
            .style("text-align", "right")
            .style("line-height", "1.25")
            .style("white-space", "nowrap")
            .style("overflow", "hidden")
            .style("text-overflow", "ellipsis")
            .text_signal(ctrl.status.signal_cloned().map(|s| s.unwrap_or_default()))
        }))
        .child(vdivider())
        // Transport cluster.
        .child(html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "6px")
            .child(play_button())
            .child(stop_button())
            .child(loop_button())
        }))
        .child(vdivider())
        // Help — a prominent, labelled button (the old `?` icon, made discoverable).
        .child(help_button())
    })
}

/// The Help button: a rectangular, labelled call-to-action (keeping the `?`
/// icon) parked beside the transport so it's easy to find.
fn help_button() -> Dom {
    Btn::new()
        .label("Help")
        .icon("help")
        .variant(BtnVariant::Solid)
        .size(BtnSize::Md)
        .title("How to use this editor + the MCP")
        .on_click(|| controller().open_help())
        .render()
}

/// A 1px vertical rule separating top-bar groups.
fn vdivider() -> Dom {
    html!("div", {
        .style("width", "1px")
        .style("height", "22px")
        .style("background", "var(--line)")
        .style("flex", "0 0 auto")
    })
}

/// The primary Play / Pause control (also Spacebar). Accent-filled; the glyph +
/// label swap on the reactive `playing` state.
fn play_button() -> Dom {
    let ctrl = controller();
    html!("button", {
        .class("t")
        .class("focusring")
        .attr("title", "Play / Pause (Space)")
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("gap", "7px")
        .style("height", "30px")
        .style("min-width", "96px")
        .style("padding", "0 15px")
        .style("border", "0")
        .style("border-radius", "var(--r2)")
        .style("font-size", "12.5px")
        .style("font-weight", "600")
        .style("cursor", "pointer")
        .style("color", ACCENT_FG)
        .style_signal("background", ctrl.playing.signal().map(|p| {
            if p { "var(--warn)" } else { "var(--accent)" }
        }))
        .child_signal(ctrl.playing.signal().map(|p| Some(
            Icon::new(if p { "pause" } else { "play" }).size(15.0).color(ACCENT_FG).render()
        )))
        .child(html!("span", {
            .text_signal(ctrl.playing.signal().map(|p| if p { "Pause" } else { "Play" }))
        }))
        .event(clone!(ctrl => move |_: events::Click| ctrl.toggle_play_pause()))
    })
}

/// Stop — return the playhead to where playback started. Dims when there's
/// nothing to stop.
fn stop_button() -> Dom {
    let ctrl = controller();
    html!("button", {
        .class("t")
        .class("focusring")
        .attr("title", "Stop (return to start)")
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("justify-content", "center")
        .style("gap", "7px")
        .style("height", "30px")
        .style("padding", "0 13px")
        .style("border", "1px solid var(--line)")
        .style("border-radius", "var(--r2)")
        .style("background", "var(--bg-3)")
        .style("color", "var(--text-1)")
        .style("font-size", "12.5px")
        .style("font-weight", "540")
        .style("cursor", "pointer")
        .style_signal("opacity", map_ref! {
            let playing = ctrl.playing.signal(),
            let paused = ctrl.paused.signal() =>
            (if *playing || *paused { "1" } else { "0.5" }).to_string()
        })
        .child(Icon::new("stop").size(13.0).render())
        .child(html!("span", { .text("Stop") }))
        .event(clone!(ctrl => move |_: events::Click| ctrl.stop()))
    })
}

/// Loop toggle — accent-active when on. Rebuilds on the reactive `looping` state
/// so the active styling tracks it.
fn loop_button() -> Dom {
    let ctrl = controller();
    html!("div", {
        .style("display", "flex")
        .child_signal(ctrl.looping.signal().map(|on| Some(
            IconBtn::new("loop")
                .title("Loop playback")
                .active(on)
                .on_click(|| {
                    let c = controller();
                    let next = !c.looping.get();
                    c.set_looping(next);
                })
                .render()
        )))
    })
}

/// Save the project to a directory: a root `project.toml` plus an `assets/`
/// folder of real files (imported audio, bounced `.wav`s, WASM modules). Uses
/// the File System Access API (Chromium-only).
fn save_button() -> Dom {
    IconBtn::new("save")
        .title("Save project to a directory")
        .on_click(|| {
            wasm_bindgen_futures::spawn_local(async move {
                match crate::fs::ProjectDir::pick().await {
                    Ok(dir) => {
                        let ctrl = controller();
                        match ctrl.save_to_dir(&dir).await {
                            Ok(()) => ctrl.status.set(Some(format!("Saved to {}/", dir.name()))),
                            Err(e) => ctrl.status.set(Some(format!("Save failed: {e}"))),
                        }
                    }
                    // Cancelling the picker is a no-op, not an error.
                    Err(crate::fs::FsError::Cancelled) => {}
                    Err(e) => controller().status.set(Some(format!("Save failed: {e}"))),
                }
            });
        })
        .render()
}

/// Open a project from a directory (picks a folder, reads `project.toml` +
/// `assets/`). File System Access API (Chromium-only).
fn open_project() {
    wasm_bindgen_futures::spawn_local(async move {
        match crate::fs::ProjectDir::pick().await {
            Ok(dir) => {
                let ctrl = controller();
                match ctrl.load_from_dir(&dir).await {
                    Ok(()) => ctrl.status.set(Some(format!("Opened {}/", dir.name()))),
                    Err(e) => ctrl.status.set(Some(format!("Open failed: {e}"))),
                }
            }
            Err(crate::fs::FsError::Cancelled) => {}
            Err(e) => controller().status.set(Some(format!("Open failed: {e}"))),
        }
    });
}

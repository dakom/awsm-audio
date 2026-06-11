//! The Arrange view: a bounce-only audio timeline. The left **Assets** panel
//! lists every Sound with its bounce status; bounce one to render it to audio,
//! then place its clip on a track. Clips draw their waveform; drag to move
//! (across tracks), drag the right edge to trim, blade to split, right-click for
//! a menu, double-click to open the source Sound. Click/drag the ruler (or an
//! empty lane) to scrub the playhead — playback starts there.
//!
//! Pure view: every mutation routes through the controller as an
//! `EditorCommand::EditArrange` (or a bounce command), so it's MCP-drivable.

use std::cell::{Cell, RefCell};

use dominator::{clone, events, html, svg, with_node, Dom, EventOptions};
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};
use wasm_bindgen::JsCast;

use crate::controller::{controller, ArrangeOp, BounceStatus, EditorCommand};
use crate::theme::ACCENT_FG;
use crate::widgets::{Btn, BtnSize, BtnVariant, Icon};
use awsm_audio_schema::{Arrangement, SampleId};

const HDR_W: f64 = 150.0;
const ROW_BASE: f64 = 66.0;
const RULER_H: f64 = 22.0;
const PPS_BASE: f64 = 80.0;

/// Horizontal pixels per second, scaled by the horizontal zoom factor.
fn pps() -> f64 {
    PPS_BASE * ZOOM_X.with(|z| z.get())
}
/// Track row height in pixels, scaled by the vertical zoom factor.
fn row_h() -> f64 {
    ROW_BASE * ZOOM_Y.with(|z| z.get())
}
const ASSETS_W: f64 = 196.0;

/// How clip/playhead edits snap. `Clip` snaps to other clips' edges only; the
/// grid modes (`Beat`/`Bar`) snap to the grid *and* magnet to clip edges; `Off`
/// is free.
#[derive(Clone, Copy, PartialEq)]
enum Snap {
    Off,
    Clip,
    Beat,
    Bar,
}
impl Snap {
    fn label(self) -> &'static str {
        match self {
            Snap::Off => "snap: off",
            Snap::Clip => "snap: clip",
            Snap::Beat => "snap: beat",
            Snap::Bar => "snap: bar",
        }
    }
    fn next(self) -> Snap {
        match self {
            Snap::Off => Snap::Clip,
            Snap::Clip => Snap::Beat,
            Snap::Beat => Snap::Bar,
            Snap::Bar => Snap::Off,
        }
    }
}

/// Timeline tool.
#[derive(Clone, Copy, PartialEq)]
enum Tool {
    Pointer,
    Draw,
    Blade,
    Stretch,
}

/// A live clip drag, shared across lanes.
#[derive(Clone, Copy)]
struct DragInfo {
    origin: usize,
    clip: usize,
    kind: DragKind,
    /// Move: `clip.start - t` at grab. Create: the (snapped) anchor start.
    grab_off: f64,
    len: f64,
    /// Base buffer offset of the clip (for TrimStart).
    offset: f64,
    /// Base start of the clip on the timeline (for TrimStart).
    base_start: f64,
    /// Max allowed length when resizing (buffer remaining; ∞ if looping).
    max_len: f64,
    /// Base playback speed of the clip (for Stretch).
    base_speed: f64,
    /// Source Sound to place (Create drag only).
    source: Option<SampleId>,
}
#[derive(Clone, Copy, PartialEq)]
enum DragKind {
    Move,
    /// Right-edge trim (change length).
    Resize,
    /// Left-edge trim (move start later, keep right edge fixed).
    TrimStart,
    /// Draw a brand-new clip by dragging out its length.
    Create,
    /// Time-stretch: drag the right edge to change length + speed (same content).
    Stretch,
}

thread_local! {
    static SNAP: Mutable<Snap> = Mutable::new(Snap::Clip);
    static TOOL: Mutable<Tool> = Mutable::new(Tool::Pointer);
    /// The Sound selected in the Assets panel, placed by the Draw tool.
    static SOURCE: Mutable<Option<SampleId>> = Mutable::new(None);
    static DRAG: RefCell<Option<DragInfo>> = const { RefCell::new(None) };
    /// Live preview geometry of the dragged clip: (track, clip, start, len).
    static PREVIEW: Mutable<Option<(usize, usize, f64, f64)>> = Mutable::new(None);
    /// Live preview of a Create (draw) gesture: (track, start, len).
    static CREATE: Mutable<Option<(usize, f64, f64)>> = Mutable::new(None);
    /// Lane elements by track index, rebuilt each view() — pointer hit-testing.
    static LANES: RefCell<Vec<(usize, web_sys::Element)>> = const { RefCell::new(Vec::new()) };
    static SCRUB: Cell<bool> = const { Cell::new(false) };
    /// An asset being dragged from the panel onto the timeline: (id, name).
    static DRAGGING_ASSET: Mutable<Option<(SampleId, String)>> = Mutable::new(None);
    /// Cursor position (clientX, clientY) for the drag ghost.
    static DRAG_GHOST: Mutable<Option<(f64, f64)>> = Mutable::new(None);
    /// Live drop target while dragging an asset: (track, start, len) in seconds.
    static DROP: Mutable<Option<(usize, f64, f64)>> = Mutable::new(None);
    /// Snap candidate edges (clip starts/ends + 0) in seconds, rebuilt each view().
    static EDGES: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
    /// Blade hover preview: (track, secs) where a cut would land. None when the
    /// blade isn't hovering a clip.
    static BLADE: Mutable<Option<(usize, f64)>> = Mutable::new(None);
    /// Live marquee rectangle in client coords `(x0, y0, x1, y1)` — only set once a
    /// drag actually begins (so a plain click never shows a rectangle).
    static MARQUEE: Mutable<Option<(f64, f64, f64, f64)>> = Mutable::new(None);
    /// Pending marquee anchor (client coords) recorded on pointerdown in empty
    /// space; promoted to `MARQUEE` only after the pointer moves past a threshold.
    static MARQUEE_START: Cell<Option<(f64, f64)>> = const { Cell::new(None) };
    /// Horizontal / vertical zoom factors (1.0 = default).
    static ZOOM_X: Mutable<f64> = Mutable::new(1.0);
    static ZOOM_Y: Mutable<f64> = Mutable::new(1.0);
}

/// The clips whose rectangles intersect a client-space box (marquee select).
fn clips_in_rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<(usize, usize)> {
    let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
    let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
    let Some(arr) = controller().arrangement_view() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    LANES.with(|l| {
        for (ti, el) in l.borrow().iter() {
            let r = el.get_bounding_client_rect();
            if hi_y < r.top() || lo_y > r.bottom() {
                continue; // no vertical overlap with this lane
            }
            let Some(track) = arr.tracks.get(*ti) else {
                continue;
            };
            for (ci, clip) in track.clips.iter().enumerate() {
                let cx0 = r.left() + clip.start * pps();
                let cx1 = cx0 + (clip.length * pps()).max(2.0);
                if hi_x >= cx0 && lo_x <= cx1 {
                    out.push((*ti, ci));
                }
            }
        }
    });
    out
}

/// Nudge a zoom factor by `mul`, clamped to a sane range.
fn zoom_by(vertical: bool, mul: f64) {
    let cell = if vertical { &ZOOM_Y } else { &ZOOM_X };
    cell.with(|z| z.set((z.get() * mul).clamp(0.25, 8.0)));
}
fn zoom_reset() {
    ZOOM_X.with(|z| z.set(1.0));
    ZOOM_Y.with(|z| z.set(1.0));
}

/// A small square button used for the zoom −/+/reset controls.
fn zsq(label: &str, title: &str, f: impl Fn() + 'static) -> Dom {
    let hover = Mutable::new(false);
    html!("button", {
        .class("t")
        .style("display", "inline-flex").style("align-items", "center").style("justify-content", "center")
        .style("min-width", "24px").style("height", "24px").style("padding", "0 6px").style("cursor", "pointer")
        .style("border-radius", "var(--r1)").style("font-size", "12px").style("line-height", "1")
        .style("border", "1px solid var(--line)")
        .style_signal("background", hover.signal().map(|h| if h { "var(--bg-hover)" } else { "var(--bg-3)" }))
        .style_signal("color", hover.signal().map(|h| if h { "var(--text-0)" } else { "var(--text-1)" }))
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .attr("title", title).text(label)
        .event(move |_: events::Click| f())
    })
}

/// A glyph label inside the zoom group (non-interactive).
fn zglyph(glyph: &str, title: &str) -> Dom {
    html!("span", {
        .style("font-size", "12.5px").style("color", "var(--text-2)").style("margin-left", "4px")
        .attr("title", title).text(glyph)
    })
}

/// Copy the selected clips into the (controller-held) clip clipboard.
fn copy_selected_clip() {
    let sel = controller().selected_clips.get_cloned();
    if !sel.is_empty() {
        controller().copy_clips(&sel);
    }
}

/// Delete the selected clips.
fn delete_selected_clip() {
    let sel = controller().selected_clips.get_cloned();
    if sel.is_empty() {
        return;
    }
    controller().selected_clips.set(Vec::new());
    controller().delete_clips(&sel);
}

/// True when a text field is focused, so global key shortcuts shouldn't fire.
fn text_focus() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.active_element())
        .map(|e| {
            let t = e.tag_name().to_ascii_lowercase();
            t == "input" || t == "textarea" || t == "select"
        })
        .unwrap_or(false)
}

/// Magnet distance in seconds (~8px) for snapping a dragged edge to clip edges.
fn magnet_secs_dist() -> f64 {
    8.0 / pps()
}

fn snap_secs(t: f64, bpm: f64) -> f64 {
    let q = match SNAP.with(|m| m.get()) {
        // Off and Clip have no time grid (Clip snaps to clip edges via magnet).
        Snap::Off | Snap::Clip => 0.0,
        Snap::Beat => 60.0 / bpm.max(1.0),
        Snap::Bar => 4.0 * 60.0 / bpm.max(1.0),
    };
    if q <= 0.0 {
        t.max(0.0)
    } else {
        ((t / q).round() * q).max(0.0)
    }
}

/// Snap `t` for a *clip edge* drag. A nearby clip edge wins (the magnet) in every
/// mode except `Off`; otherwise fall back to the mode's grid (Beat/Bar) or free
/// (Clip/Off). `exclude` edges (the dragged clip's own) are ignored so it can't
/// stick to itself.
fn magnet_secs(t: f64, bpm: f64, exclude: &[f64]) -> f64 {
    // Off = totally free, no magnet, no grid.
    if SNAP.with(|m| m.get()) == Snap::Off {
        return t.max(0.0);
    }
    let mut best: Option<(f64, f64)> = None; // (edge, distance)
    EDGES.with(|e| {
        for &edge in e.borrow().iter() {
            if exclude.iter().any(|x| (x - edge).abs() < 1e-4) {
                continue;
            }
            let d = (edge - t).abs();
            if d < magnet_secs_dist() && best.map_or(true, |(_, bd)| d < bd) {
                best = Some((edge, d));
            }
        }
    });
    match best {
        Some((edge, _)) => edge.max(0.0),
        None => snap_secs(t, bpm),
    }
}

fn track_hue(i: usize) -> f64 {
    [250.0, 150.0, 60.0, 320.0, 200.0, 30.0][i % 6]
}

/// clientX → timeline seconds, using any registered lane (shared X origin).
fn secs_at(cx: f64) -> f64 {
    LANES.with(|l| {
        l.borrow()
            .first()
            .map(|(_, el)| ((cx - el.get_bounding_client_rect().left()) / pps()).max(0.0))
            .unwrap_or(0.0)
    })
}

/// The track whose lane rect strictly contains `(cx, cy)`, or `None` — used for
/// asset drops so a click on the panel doesn't fall through to a track.
fn lane_at(cx: f64, cy: f64) -> Option<usize> {
    LANES.with(|l| {
        l.borrow().iter().find_map(|(ti, el)| {
            let r = el.get_bounding_client_rect();
            (cx >= r.left() && cx <= r.right() && cy >= r.top() && cy <= r.bottom()).then_some(*ti)
        })
    })
}

/// clientY → the track lane it's over (nearest by centre otherwise).
fn track_at(cy: f64) -> Option<usize> {
    LANES.with(|l| {
        let v = l.borrow();
        let mut best: Option<(usize, f64)> = None;
        for (ti, el) in v.iter() {
            let r = el.get_bounding_client_rect();
            if cy >= r.top() && cy <= r.bottom() {
                return Some(*ti);
            }
            let dist = (cy - (r.top() + r.bottom()) / 2.0).abs();
            if best.map_or(true, |(_, d)| dist < d) {
                best = Some((*ti, dist));
            }
        }
        best.map(|(ti, _)| ti)
    })
}

pub fn render() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("flex", "1")
        .style("min-height", "0")
        .style("background", "var(--bg-0)")
        // Tool shortcuts (V pointer / D draw / B blade) + clip copy-paste-delete.
        .global_event(move |e: events::KeyDown| {
            if text_focus() {
                return;
            }
            // dominator's `ctrl_key()` already covers Cmd (meta) on macOS.
            let cmd = e.ctrl_key();
            match e.key().as_str() {
                "v" | "V" if !cmd => TOOL.with(|m| m.set(Tool::Pointer)),
                "d" | "D" if !cmd => TOOL.with(|m| m.set(Tool::Draw)),
                "b" | "B" if !cmd => TOOL.with(|m| m.set(Tool::Blade)),
                "s" | "S" if !cmd => TOOL.with(|m| m.set(Tool::Stretch)),
                "c" | "C" if cmd => copy_selected_clip(),
                "x" | "X" if cmd => { copy_selected_clip(); delete_selected_clip(); }
                "v" | "V" if cmd => controller().paste_clip(),
                "Backspace" | "Delete" => delete_selected_clip(),
                _ => {}
            }
        })
        // NB: selected_track / selected-clip changes are NOT in this map_ref — they
        // update via per-element style_signals. Rebuilding the whole view on every
        // selection change would destroy a header button mid-click (pointerdown sets
        // selected_track → rebuild → the button's `click` never lands).
        .child_signal(map_ref! {
            let _rev = controller().samples_rev.signal(),
            let snap = SNAP.with(|m| m.signal()),
            let tool = TOOL.with(|m| m.signal()),
            let _zx = ZOOM_X.with(|m| m.signal()),
            let _zy = ZOOM_Y.with(|m| m.signal()) =>
            // NB: SOURCE (asset selection) is NOT a dep — it drives only the asset-row
            // highlight, which is reactive via per-row style_signal. Rebuilding the
            // whole view on SOURCE change would destroy the Re-bounce button mid-click
            // (asset-row pointerdown sets SOURCE → rebuild → the button's `click` is lost).
            Some(match controller().arrangement_view() {
                Some(arr) => view(&arr, *snap, *tool),
                None => html!("div", { .style("padding", "16px").text("No arrangement.") }),
            })
        })
        // Floating ghost that follows the cursor while dragging an asset.
        .child(drag_ghost())
        // Marquee box-select rectangle.
        .child(marquee_overlay())
    })
}

/// The dashed rectangle drawn while marquee box-selecting clips.
fn marquee_overlay() -> Dom {
    html!("div", {
        .style("position", "fixed").style("z-index", "40").style("pointer-events", "none")
        .style("border", "1px solid var(--accent-bright)")
        .style("background", "oklch(0.6 0.12 230 / 0.18)")
        .style_signal("display", MARQUEE.with(|m| m.signal_ref(|r| {
            (if r.is_some() { "block" } else { "none" }).to_string()
        })))
        .style_signal("left", MARQUEE.with(|m| m.signal().map(|r| {
            format!("{}px", r.map_or(0.0, |(x0, _, x1, _)| x0.min(x1)))
        })))
        .style_signal("top", MARQUEE.with(|m| m.signal().map(|r| {
            format!("{}px", r.map_or(0.0, |(_, y0, _, y1)| y0.min(y1)))
        })))
        .style_signal("width", MARQUEE.with(|m| m.signal().map(|r| {
            format!("{}px", r.map_or(0.0, |(x0, _, x1, _)| (x1 - x0).abs()))
        })))
        .style_signal("height", MARQUEE.with(|m| m.signal().map(|r| {
            format!("{}px", r.map_or(0.0, |(_, y0, _, y1)| (y1 - y0).abs()))
        })))
    })
}

/// A fixed-position chip that tracks the cursor while an asset is dragged from
/// the panel onto the timeline.
fn drag_ghost() -> Dom {
    html!("div", {
        .style("position", "fixed")
        // Anchor to the viewport origin so `transform` is absolute (without an
        // explicit top/left a fixed box starts at its static flow position).
        .style("top", "0").style("left", "0")
        .style("z-index", "50")
        .style("pointer-events", "none")
        .style("padding", "2px 8px")
        .style("border-radius", "5px")
        .style("font-size", "11.5px").style("font-weight", "600")
        .style("background", "oklch(0.5 0.13 230 / 0.92)")
        .style("border", "1px solid var(--accent-bright)")
        .style("box-shadow", "0 2px 8px oklch(0 0 0 / 0.5)")
        .style("white-space", "nowrap")
        .style_signal("display", DRAGGING_ASSET.with(|m| m.signal_ref(|a| {
            (if a.is_some() { "block" } else { "none" }).to_string()
        })))
        .style_signal("transform", DRAG_GHOST.with(|m| m.signal().map(|p| {
            let (x, y) = p.unwrap_or((-9999.0, -9999.0));
            format!("translate({}px, {}px)", x + 12.0, y + 12.0)
        })))
        .text_signal(DRAGGING_ASSET.with(|m| m.signal_ref(|a| {
            a.as_ref().map(|(_, n)| n.clone()).unwrap_or_default()
        })))
    })
}

fn view(arr: &Arrangement, snap: Snap, tool: Tool) -> Dom {
    let total = arr.length_secs.max(8.0);
    LANES.with(|l| l.borrow_mut().clear());
    // Rebuild snap candidate edges: every clip start/end, plus the origin.
    EDGES.with(|e| {
        let mut v = vec![0.0];
        for t in &arr.tracks {
            for c in &t.clips {
                v.push(c.start);
                v.push(c.start + c.length);
            }
        }
        *e.borrow_mut() = v;
    });
    html!("div", {
        .style("display", "flex")
        .style("flex-direction", "column")
        .style("flex", "1")
        .style("min-height", "0")
        .child(toolbar(arr, snap, tool))
        .child(html!("div", {
            .style("display", "flex")
            .style("flex", "1")
            .style("min-height", "0")
            .child(assets_panel())
            .child(timeline(arr, total))
        }))
    })
}

fn timeline(arr: &Arrangement, total: f64) -> Dom {
    let bpm = arr.bpm;
    html!("div", {
        .style("overflow", "auto")
        .style("flex", "1")
        .style("min-height", "0")
        .style("position", "relative")
        .style("touch-action", "none")
        .child(ruler_row(total, bpm))
        .children(arr.tracks.iter().enumerate().map(|(i, _)| track_row(arr, i, total)))
        .child(add_track_row())
        // Playhead / scrub line (transform-positioned; no per-frame reflow).
        .child(html!("div", {
            .style("position", "absolute")
            .style("top", "0")
            .style("left", "0")
            .style("height", &format!("{}px", RULER_H + arr.tracks.len() as f64 * row_h()))
            .style("width", "2px")
            .style("margin-left", &format!("{HDR_W}px"))
            .style("background", "oklch(0.85 0.18 90)")
            .style("pointer-events", "none")
            .style("will-change", "transform")
            .style("z-index", "1")
            .style_signal("transform", controller().arrange_playhead.signal().map(|s| {
                format!("translateX({}px)", if s < 0.0 { -9999.0 } else { s * pps() })
            }))
            .style_signal("opacity", controller().arrange_playhead.signal().map(|s| {
                (if s < 0.0 { "0" } else { "0.9" }).to_string()
            }))
        }))
        // Loop/export region: a shaded band between the markers (when set).
        .apply(|d| match arr.has_markers().then(|| arr.range()) {
            Some((s, e)) => d.child(html!("div", {
                .style("position", "absolute")
                .style("top", "0")
                .style("left", "0")
                .style("margin-left", &format!("{HDR_W}px"))
                .style("height", &format!("{}px", RULER_H + arr.tracks.len() as f64 * row_h()))
                .style("transform", &format!("translateX({}px)", s * pps()))
                .style("width", &format!("{}px", ((e - s) * pps()).max(1.0)))
                .style("background", "oklch(0.8 0.16 90 / 0.1)")
                .style("border-left", "1.5px solid oklch(0.82 0.17 90)")
                .style("border-right", "1.5px solid oklch(0.82 0.17 90)")
                .style("box-sizing", "border-box")
                .style("pointer-events", "none")
                .style("z-index", "0")
            })),
            None => d,
        })
        // Move/resize/trim/draw + scrub track on the container (so a drag can
        // cross lanes). Asset-drag ghost movement is handled globally below.
        .global_event(move |e: events::PointerMove| {
            // Marquee: promote a pending anchor to an active box once the pointer
            // moves past a small threshold (so a click never box-selects).
            if let Some((x0, y0)) = MARQUEE_START.with(|m| m.get()) {
                if MARQUEE.with(|m| m.lock_ref().is_none())
                    && (e.x() - x0).abs() < 4.0
                    && (e.y() - y0).abs() < 4.0
                {
                    return; // not yet a drag
                }
                MARQUEE.with(|m| m.set(Some((x0, y0, e.x(), e.y()))));
                controller().selected_clips.set(clips_in_rect(x0, y0, e.x(), e.y()));
                return;
            }
            // An asset is being dragged from the panel: move its ghost + show a
            // drop rectangle in the lane under the cursor.
            if let Some((id, _)) = DRAGGING_ASSET.with(|m| m.lock_ref().clone()) {
                DRAG_GHOST.with(|m| m.set(Some((e.x(), e.y()))));
                let drop = lane_at(e.x(), e.y()).map(|track| {
                    let start = magnet_secs(secs_at(e.x()), bpm, &[]);
                    let len = controller().bounce_duration(id).unwrap_or(1.0).max(0.05);
                    (track, start, len)
                });
                DROP.with(|m| m.set(drop));
                return;
            }
            if SCRUB.with(|s| s.get()) {
                controller().set_arrange_start(snap_secs(secs_at(e.x()), bpm));
                return;
            }
            let Some(d) = DRAG.with(|c| *c.borrow()) else { return };
            let t = secs_at(e.x());
            match d.kind {
                DragKind::Move => {
                    // Magnet the clip's *leading* edge to nearby clip edges.
                    let raw = t + d.grab_off;
                    let own = [d.base_start, d.base_start + d.len];
                    let start = magnet_secs(raw, bpm, &own);
                    PREVIEW.with(|p| p.set(Some((d.origin, d.clip, start, d.len))));
                }
                DragKind::Resize => {
                    let start = d.base_start;
                    let own = [d.base_start];
                    let right = magnet_secs(t, bpm, &own);
                    // Can't extend past the buffer (unless the clip loops).
                    let len = (right - start).max(0.05).min(d.max_len);
                    PREVIEW.with(|p| p.set(Some((d.origin, d.clip, start, len))));
                }
                DragKind::TrimStart => {
                    let right = d.base_start + d.len;
                    // Lower bound keeps the buffer offset ≥ 0. The buffer advances by
                    // `speed` per timeline second, so `offset` worth of buffer is
                    // `offset / speed` timeline seconds of earlier material.
                    let lo = (d.base_start - d.offset / d.base_speed.max(0.01)).max(0.0);
                    let start = magnet_secs(t, bpm, &[right]).clamp(lo, right - 0.05);
                    PREVIEW.with(|p| p.set(Some((d.origin, d.clip, start, (right - start).max(0.05)))));
                }
                DragKind::Create => {
                    let end = magnet_secs(t, bpm, &[d.grab_off]);
                    let len = (end - d.grab_off).max(0.05);
                    CREATE.with(|p| p.set(Some((d.origin, d.grab_off, len))));
                }
                DragKind::Stretch => {
                    // Right edge moves; length changes, speed derived on release.
                    let right = magnet_secs(t, bpm, &[d.base_start]);
                    let len = (right - d.base_start).max(0.05);
                    PREVIEW.with(|p| p.set(Some((d.origin, d.clip, d.base_start, len))));
                }
            }
        })
        .global_event(move |e: events::PointerUp| {
            SCRUB.with(|s| s.set(false));
            // Finish a marquee. If it never became active (no drag), it was a
            // click on empty space: clear the selection + move the playhead.
            if let Some((x0, _)) = MARQUEE_START.with(|m| { let v = m.get(); m.set(None); v }) {
                let was_marquee = MARQUEE.with(|m| { let some = m.lock_ref().is_some(); m.set(None); some });
                if !was_marquee {
                    controller().selected_clips.set(Vec::new());
                    controller().set_arrange_start(snap_secs(secs_at(x0), bpm));
                }
                return;
            }
            // Finish an asset drag-from-panel: drop a clip where released.
            if let Some((id, _)) = DRAGGING_ASSET.with(|m| m.lock_ref().clone()) {
                DRAGGING_ASSET.with(|m| m.set(None));
                DRAG_GHOST.with(|m| m.set(None));
                DROP.with(|m| m.set(None));
                if let Some(track) = lane_at(e.x(), e.y()) {
                    controller().selected_track.set(track);
                    let start = magnet_secs(secs_at(e.x()), bpm, &[]);
                    controller().dispatch(EditorCommand::EditArrange {
                        op: ArrangeOp::AddClip { track, start, source: id, length: None },
                    });
                }
                return;
            }
            let Some(d) = DRAG.with(|c| c.borrow_mut().take()) else { return };
            if d.kind == DragKind::Create {
                let cv = CREATE.with(|p| { let v = p.get(); p.set(None); v });
                if let Some(src) = d.source {
                    // A real drag (>=0.15s) draws that length; a bare click drops
                    // the full bounce (length: None) at the anchor.
                    let (start, length) = match cv {
                        Some((_, s, l)) if l >= 0.15 => (s, Some(l)),
                        _ => (d.grab_off, None),
                    };
                    controller().dispatch(EditorCommand::EditArrange {
                        op: ArrangeOp::AddClip { track: d.origin, start, source: src, length },
                    });
                }
                return;
            }
            let pv = PREVIEW.with(|p| { let v = p.get(); p.set(None); v });
            let Some((_, _, start, len)) = pv else { return };
            // A Move drag that didn't actually move is just a click-select: don't
            // dispatch (avoids a no-op edit + undo entry, and keeps the index).
            if d.kind == DragKind::Move {
                let new_track = track_at(e.y()).unwrap_or(d.origin);
                let moved = new_track != d.origin || (start - d.base_start).abs() > 1e-4;
                if moved {
                    controller().dispatch(EditorCommand::EditArrange {
                        op: ArrangeOp::MoveClip { track: d.origin, clip: d.clip, new_track, start },
                    });
                    // Cross-track moves push to the new track's end — follow the
                    // selection to the clip's new index so the highlight is correct.
                    if new_track != d.origin {
                        let idx = controller()
                            .arrangement_view()
                            .and_then(|a| a.tracks.get(new_track).map(|t| t.clips.len().saturating_sub(1)));
                        controller().selected_clips.set(idx.map(|i| vec![(new_track, i)]).unwrap_or_default());
                    }
                }
                return;
            }
            let op = match d.kind {
                DragKind::Move => unreachable!(),
                DragKind::Resize => ArrangeOp::ResizeClip { track: d.origin, clip: d.clip, length: len },
                DragKind::TrimStart => {
                    // Buffer advances by `speed` per timeline second.
                    let offset = (d.offset + (start - d.base_start) * d.base_speed).max(0.0);
                    ArrangeOp::TrimStart { track: d.origin, clip: d.clip, start, offset }
                }
                DragKind::Stretch => {
                    // Keep the buffer content fixed; speed = content / new length.
                    let content = d.len * d.base_speed;
                    let speed = (content / len.max(0.01)).clamp(0.1, 10.0) as f32;
                    ArrangeOp::StretchClip { track: d.origin, clip: d.clip, length: len, speed }
                }
                DragKind::Create => unreachable!(),
            };
            controller().dispatch(EditorCommand::EditArrange { op });
        })
    })
}

/// A sticky-left "+ Add Track" row beneath the track list — the discoverable,
/// DAW-conventional place to add a track (mirrors the toolbar action).
fn add_track_row() -> Dom {
    let hover = Mutable::new(false);
    html!("div", {
        .style("display", "flex")
        .style("position", "sticky")
        .style("left", "0")
        .style("z-index", "2")
        .style("width", &format!("{HDR_W}px"))
        .child(html!("button", {
            .class("t")
            .style("display", "inline-flex")
            .style("align-items", "center")
            .style("gap", "6px")
            .style("width", "100%")
            .style("height", "30px")
            .style("padding", "0 10px")
            .style("cursor", "pointer")
            .style("border", "1px dashed var(--line-strong)")
            .style("border-radius", "var(--r2)")
            .style("margin", "6px")
            .style("font-size", "12px")
            .style("font-weight", "540")
            .style_signal("background", hover.signal().map(|h| if h { "var(--bg-hover)" } else { "transparent" }))
            .style_signal("color", hover.signal().map(|h| if h { "var(--text-0)" } else { "var(--text-2)" }))
            .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
            .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
            .attr("title", "Add a track to the arrangement")
            .child(crate::widgets::Icon::new("plus").size(14.0).render())
            .child(html!("span", { .text("Add Track") }))
            .event(|_: events::Click| controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::AddTrack }))
        }))
    })
}

/// A 1px vertical rule for separating toolbar groups.
fn tdivider() -> Dom {
    html!("div", {
        .style("width", "1px")
        .style("height", "22px")
        .style("background", "var(--line)")
        .style("flex", "0 0 auto")
        .style("margin", "0 2px")
    })
}

fn tbtn(label: &str, active: bool, title: &str, f: impl Fn() + 'static) -> Dom {
    let hover = Mutable::new(false);
    html!("button", {
        .class("t")
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("height", "28px")
        .style("padding", "0 11px")
        .style("cursor", "pointer")
        .style("border-radius", "var(--r2)")
        .style("font-size", "12px")
        .style("font-weight", if active { "600" } else { "520" })
        .style("white-space", "nowrap")
        .style("border", if active { "1px solid transparent" } else { "1px solid var(--line)" })
        .style("color", if active { ACCENT_FG } else { "var(--text-1)" })
        .style_signal("background", hover.signal().map(move |h| {
            if active { "var(--accent)" } else if h { "var(--bg-hover)" } else { "var(--bg-3)" }
        }))
        .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
        .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
        .apply(|b| if active { b } else {
            b.style_signal("color", hover.signal().map(|h| if h { "var(--text-0)" } else { "var(--text-1)" }))
        })
        .attr("title", title)
        .text(label)
        .event(move |_: events::Click| f())
    })
}

fn toolbar(arr: &Arrangement, snap: Snap, tool: Tool) -> Dom {
    let bpm = arr.bpm;
    let len = arr.length_secs;
    let markers = arr.has_markers().then(|| arr.range());
    let pick = |t: Tool| move || TOOL.with(|m| m.set(t));
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "8px")
        .style("padding", "8px 12px")
        .style("background", "var(--bg-2)")
        .style("border-bottom", "1px solid var(--line)")
        .style("flex-wrap", "wrap")
        .child(html!("span", { .class("kicker").text("BPM") }))
        .child(html!("input" => web_sys::HtmlInputElement, {
            .attr("type", "number")
            .attr("min", "20")
            .attr("max", "400")
            .attr("value", &format!("{}", bpm as i64))
            .style("width", "58px")
            .style("height", "28px")
            .style("padding", "0 8px")
            .style("border-radius", "var(--r2)")
            .style("border", "1px solid var(--line)")
            .style("background", "var(--bg-3)")
            .style("color", "var(--text-0)")
            .style("font-size", "12px")
            .with_node!(el => {
                .event(move |_: events::Input| {
                    if let Ok(v) = el.value().parse::<f64>() {
                        controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetBpm(v) });
                    }
                })
            })
        }))
        .child(html!("span", { .style("font-size", "12px").style("color", "var(--text-2)").style("margin-left", "6px").text(&format!("{}s", len as i64)) }))
        .child(tbtn("\u{2212} 4s", false, "Shorten the timeline", move || {
            controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetLengthSecs((len - 4.0).max(4.0)) });
        }))
        .child(tbtn("+ 4s", false, "Lengthen the timeline", move || {
            controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetLengthSecs(len + 4.0) });
        }))
        // Add Track — a structural action, kept prominent and apart from the
        // clip-editing tools on the right.
        .child(tdivider())
        .child(Btn::new()
            .label("Add Track")
            .icon("plus")
            .variant(BtnVariant::Solid)
            .size(BtnSize::Sm)
            .title("Add a track to the arrangement")
            .on_click(|| controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::AddTrack }))
            .render())
        // Loop/export markers: set the in/out points at the playhead; when set,
        // playback loops the region and Export renders exactly it.
        .child(tdivider())
        .child(tbtn("\u{27E6} in", false, "Set the loop/export START marker at the playhead", || controller().arrange_set_loop_in()))
        .child(tbtn("out \u{27E7}", false, "Set the loop/export END marker at the playhead", || controller().arrange_set_loop_out()))
        .apply(|d| match markers {
            Some((s, e)) => d
                .child(html!("span", {
                    .style("font-size", "11px")
                    .style("color", "var(--accent-bright)")
                    .style("white-space", "nowrap")
                    .text(&format!("loop {s:.1}\u{2013}{e:.1}s"))
                }))
                .child(tbtn("\u{2715}", true, "Clear the loop/export markers (whole timeline)", || controller().arrange_clear_loop())),
            None => d,
        })
        .child(html!("div", { .style("flex", "1") }))
        .child(tbtn(snap.label(), snap != Snap::Off, "Cycle snap: off → clip → beat → bar (clip = snap to clip edges; beat/bar also magnet to clips)", || {
            SNAP.with(|m| m.set(m.get().next()));
        }))
        .child(tbtn("\u{2196} pointer", tool == Tool::Pointer, "Pointer (V): move / trim clips, scrub the playhead", pick(Tool::Pointer)))
        .child(tbtn("\u{270E} draw", tool == Tool::Draw, "Draw (D): click a lane to drop the selected Sound, or drag to draw a shorter clip", pick(Tool::Draw)))
        .child(tbtn("\u{2702} blade", tool == Tool::Blade, "Blade (B): click a clip to split it", pick(Tool::Blade)))
        .child(tbtn("\u{21D4} stretch", tool == Tool::Stretch, "Time-stretch (S): drag a clip to play it faster / slower (pitch shifts)", pick(Tool::Stretch)))
        // Zoom: time (horizontal) and track height (vertical), grouped neatly.
        .child(html!("div", {
            .style("display", "inline-flex").style("align-items", "center").style("gap", "3px")
            .style("margin-left", "8px").style("padding", "2px 5px").style("border-radius", "7px")
            .style("border", "1px solid var(--line)").style("background", "var(--bg-1)")
            .child(html!("span", { .style("font-size", "11px").style("color", "var(--text-3)").style("margin-right", "2px").text("zoom") }))
            .child(zglyph("\u{2194}", "Time (horizontal) zoom"))
            .child(zsq("\u{2212}", "Zoom out (time)", || zoom_by(false, 1.0 / 1.25)))
            .child(zsq("+", "Zoom in (time)", || zoom_by(false, 1.25)))
            .child(zglyph("\u{2195}", "Track height (vertical) zoom"))
            .child(zsq("\u{2212}", "Zoom out (tracks)", || zoom_by(true, 1.0 / 1.25)))
            .child(zsq("+", "Zoom in (tracks)", || zoom_by(true, 1.25)))
            .child(zsq("1:1", "Reset zoom", zoom_reset))
        }))
    })
}

/// The left Assets panel: every Sound with its bounce status, a Bounce button,
/// and (when bounced) a mini waveform. Click a bounced Sound to select it for the
/// Place tool.
fn assets_panel() -> Dom {
    let list = controller().assets_list();
    html!("div", {
        .style("flex", "0 0 auto")
        .style("width", &format!("{ASSETS_W}px"))
        .style("box-sizing", "border-box")
        .style("overflow-y", "auto")
        .style("padding", "10px")
        .style("background", "var(--bg-1)")
        .style("border-right", "1px solid var(--line)")
        .child(html!("div", {
            .class("kicker")
            .style("margin-bottom", "3px")
            .text("Assets")
        }))
        .child(html!("div", {
            .style("font-size", "11.5px").style("line-height", "1.4")
            .style("color", "var(--text-2)").style("margin-bottom", "10px")
            .text("Bounce a Sound, then place it on a track.")
        }))
        .children(list.into_iter().map(move |(id, name, status, dur)| {
            asset_row(id, name, status, dur)
        }))
    })
}

/// True when a DOM event originated on (or inside) a `<button>`. Row-level
/// handlers use this to ignore presses on child buttons (e.g. Re-bounce): the
/// row's `pointerdown` starts an asset-drag, and on a real mouse that turns the
/// button press into a drag gesture so its `click` never fires — re-bounce then
/// silently does nothing.
fn event_from_button(target: Option<web_sys::EventTarget>) -> bool {
    target
        .and_then(|t| t.dyn_ref::<web_sys::Element>().cloned())
        .and_then(|el| el.closest("button").ok().flatten())
        .is_some()
}

fn asset_row(id: SampleId, name: String, status: BounceStatus, dur: Option<f64>) -> Dom {
    let (badge, badge_col) = match status {
        BounceStatus::None => ("not bounced", "var(--text-2)"),
        BounceStatus::Clean => ("bounced", "oklch(0.78 0.15 150)"),
        BounceStatus::Dirty => ("\u{25CF} dirty", "oklch(0.8 0.15 60)"),
    };
    let bounced = status != BounceStatus::None;
    let title = match dur {
        Some(d) => format!("Render this Sound to audio (~{d:.1}s)"),
        None => "Render this Sound to audio".to_string(),
    };
    html!("div", {
        .style("border-radius", "6px")
        // Selection highlight is reactive (not a view-rebuild dep) so clicking the
        // Re-bounce button doesn't tear down the row mid-click.
        .style_signal("border", SOURCE.with(|m| m.signal()).map(move |s| {
            if s == Some(id) { "1px solid var(--accent-bright)" } else { "1px solid var(--line)" }
        }))
        .style_signal("background", SOURCE.with(|m| m.signal()).map(move |s| {
            if s == Some(id) { "oklch(0.28 0.06 230)" } else { "var(--bg-2)" }
        }))
        .style("padding", "5px 6px")
        .style("margin-bottom", "6px")
        .style("cursor", if bounced { "grab" } else { "default" })
        // Click selects a bounced Sound for the Draw tool. Ignore clicks on the
        // Re-bounce button (its own handler runs).
        .event(move |e: events::Click| {
            if event_from_button(e.target()) {
                return;
            }
            if bounced {
                SOURCE.with(|m| m.set(Some(id)));
                TOOL.with(|m| m.set(Tool::Draw));
            }
        })
        // Press-and-drag a bounced Sound straight onto a lane. A press on the
        // Re-bounce button must NOT start a drag, or the gesture is treated as a
        // drag and the button's click never fires (re-bounce silently no-ops).
        .event(clone!(name => move |e: events::PointerDown| {
            if event_from_button(e.target()) {
                return;
            }
            if bounced {
                SOURCE.with(|m| m.set(Some(id)));
                DRAGGING_ASSET.with(|m| m.set(Some((id, name.clone()))));
                DRAG_GHOST.with(|m| m.set(Some((e.x(), e.y()))));
            }
        }))
        // Double-click jumps to the Sound's graph (edit it).
        .event(move |_: events::DoubleClick| controller().open_sample(id))
        // Right-click: Go to / Place at playhead.
        .event_with_options(&EventOptions::preventable(), move |e: events::ContextMenu| {
            e.prevent_default();
            controller().open_sound_menu(id, e.x(), e.y());
        })
        .child(html!("div", {
            .style("display", "flex").style("align-items", "center").style("gap", "4px")
            .child(html!("div", { .style("flex", "1").style("min-width", "0").style("font-size", "12.5px").style("font-weight", "600").style("color", "var(--text-0)").style("white-space", "nowrap").style("overflow", "hidden").style("text-overflow", "ellipsis").text(&name) }))
            .child(html!("span", { .style("font-size", "10.5px").style("font-weight", "600").style("color", badge_col).text(badge) }))
        }))
        // Mini waveform for a bounced Sound.
        .apply(move |b| if bounced {
            b.child(mini_waveform(id))
        } else { b })
        .child({
            let hover = Mutable::new(false);
            let dirty = status == BounceStatus::Dirty;
            html!("button", {
                .class("t")
                .style("margin-top", "6px").style("width", "100%").style("height", "26px")
                .style("display", "inline-flex").style("align-items", "center").style("justify-content", "center")
                .style("cursor", "pointer")
                .style("border-radius", "var(--r2)").style("font-size", "12px").style("font-weight", "540")
                .style("color", if dirty { "var(--warn)" } else { "var(--text-1)" })
                .style("border", if dirty { "1px solid color-mix(in oklch, var(--warn) 50%, transparent)" } else { "1px solid var(--line)" })
                .style_signal("background", hover.signal().map(move |h| {
                    if dirty { if h { "var(--warn-soft)" } else { "transparent" } }
                    else if h { "var(--bg-hover)" } else { "var(--bg-3)" }
                }))
                .event(clone!(hover => move |_: events::MouseEnter| hover.set_neq(true)))
                .event(clone!(hover => move |_: events::MouseLeave| hover.set_neq(false)))
                .text(match status { BounceStatus::None => "Bounce", _ => "Re-bounce" })
                .attr("title", &title)
                .event(move |e: events::Click| { e.stop_propagation(); controller().dispatch(EditorCommand::Bounce { sample: id, duration_secs: None }); })
            })
        })
    })
}

/// A small waveform thumbnail (whole bounce, stretched to fill the box).
fn mini_waveform(source: SampleId) -> Dom {
    peaks_svg(
        &controller().bounce_peaks(source, 240),
        "oklch(0.7 0.1 150)",
        30.0,
    )
}

/// Build an SVG waveform from a peaks list, stretched to fill its box (viewBox
/// is resolution-independent so it scales with any width).
fn peaks_svg(peaks: &[(f32, f32)], fill: &str, height: f64) -> Dom {
    let n = peaks.len().max(1);
    // Polygon: across the tops (1 - hi), back across the bottoms (1 - lo).
    let mut d = String::from("M");
    for (i, (_, hi)) in peaks.iter().enumerate() {
        d.push_str(&format!(" {} {:.3}", i, 1.0 - *hi as f64));
    }
    for (i, (lo, _)) in peaks.iter().enumerate().rev() {
        d.push_str(&format!(" L {} {:.3}", i, 1.0 - *lo as f64));
    }
    d.push_str(" Z");
    html!("div", {
        .style("height", &format!("{height}px"))
        .style("pointer-events", "none")
        .child(svg!("svg", {
            .attr("width", "100%")
            .attr("height", &format!("{height}px"))
            .attr("viewBox", &format!("0 0 {n} 2"))
            .attr("preserveAspectRatio", "none")
            .attr("style", "display:block")
            .child(svg!("path", { .attr("d", &d).attr("fill", fill) }))
        }))
    })
}

/// Split a clip into waveform tiles `(left_secs, width_secs, win_start, win_len)`:
/// one window per buffer pass. Non-looping = a single window of the played span;
/// looping = the head pass then repeated full passes until the length is filled.
fn clip_wave_tiles(
    offset: f64,
    length: f64,
    looping: bool,
    duration: f64,
    speed: f64,
) -> Vec<(f64, f64, f64, f64)> {
    let mut tiles = Vec::new();
    let speed = speed.max(0.01);
    if duration <= 0.0001 || length <= 0.0 {
        return tiles;
    }
    if !looping {
        // The clip occupies `length` on the grid but plays `length*speed` of buffer.
        let win = (length * speed)
            .min((duration - offset).max(0.0))
            .max(0.0001);
        tiles.push((0.0, length, offset, win));
        return tiles;
    }
    // Matches the player's loop region [offset, duration], scaled by speed: one
    // pass spans (duration-offset) buffer secs over (duration-offset)/speed grid.
    let pass_buf = (duration - offset).max(0.0001);
    let pass_tl = pass_buf / speed;
    let mut tau = 0.0;
    let mut guard = 0;
    while tau < length - 1e-6 && guard < 4096 {
        let seg = pass_tl.min(length - tau);
        tiles.push((tau, seg, offset, seg * speed));
        tau += seg;
        guard += 1;
    }
    tiles
}

/// The waveform fill of a placed clip: one or more SVG tiles reflecting the
/// clip's offset / length / loop (so it shows the real audio, repeated when
/// looping, never silence stretched).
fn clip_waveform(
    source: SampleId,
    offset: f64,
    length: f64,
    looping: bool,
    speed: f64,
    hue: f64,
) -> Dom {
    let duration = controller()
        .bounce_duration(source)
        .unwrap_or(length)
        .max(0.0001);
    let fill = format!("oklch(0.82 0.13 {hue})");
    let tiles = clip_wave_tiles(offset, length, looping, duration, speed);
    html!("div", {
        .style("position", "absolute").style("inset", "0")
        .style("pointer-events", "none").style("opacity", "0.85")
        .children(tiles.into_iter().map(move |(left, width, win_start, win_len)| {
            // ~80 buckets per displayed second, capped for sanity.
            let n = ((width * 80.0) as usize).clamp(8, 1200);
            let peaks = controller().bounce_peaks_window(source, win_start, win_len, n);
            html!("div", {
                .style("position", "absolute").style("top", "0").style("bottom", "0")
                .style("left", &format!("{}px", left * pps()))
                .style("width", &format!("{}px", width * pps()))
                .style("overflow", "hidden")
                // Loop seams: a faint divider at each tile's left edge (except first).
                .apply(|b| if left > 0.0 {
                    b.style("border-left", "1px solid oklch(0.9 0.05 90 / 0.35)")
                } else { b })
                .child(peaks_svg(&peaks, &fill, row_h() - 6.0))
            })
        }))
    })
}

fn ruler_row(total: f64, bpm: f64) -> Dom {
    let bars = ((total * bpm / (4.0 * 60.0)).ceil() as usize).max(1);
    let bar_secs = 4.0 * 60.0 / bpm.max(1.0);
    html!("div", {
        .style("display", "flex")
        .style("height", &format!("{RULER_H}px"))
        .style("position", "sticky")
        .style("top", "0")
        .style("z-index", "3")
        .child(html!("div", {
            .style("flex", "0 0 auto").style("width", &format!("{HDR_W}px")).style("box-sizing", "border-box")
            .style("position", "sticky").style("left", "0").style("z-index", "4")
            .style("background", "var(--bg-2)")
            .style("border-right", "1px solid var(--line)")
            .style("border-bottom", "1px solid var(--line)")
        }))
        .child(html!("div", {
            .style("position", "relative").style("flex", "0 0 auto")
            .style("width", &format!("{}px", total * pps()))
            .style("background", "var(--bg-2)")
            .style("border-bottom", "1px solid var(--line)")
            .style("cursor", "text")
            .style("touch-action", "none")
            .event(move |e: events::PointerDown| {
                SCRUB.with(|s| s.set(true));
                controller().set_arrange_start(snap_secs(secs_at(e.x()), bpm));
            })
            .children((0..bars).map(move |b| {
                html!("div", {
                    .style("position", "absolute")
                    .style("left", &format!("{}px", b as f64 * bar_secs * pps()))
                    .style("top", "0").style("bottom", "0").style("padding", "0 4px")
                    .style("border-left", "1px solid var(--line)")
                    .style("font-size", "10.5px").style("opacity", "0.6")
                    .text(&format!("{}", b + 1))
                })
            }))
        }))
    })
}

fn track_row(arr: &Arrangement, ti: usize, total: f64) -> Dom {
    let track = &arr.tracks[ti];
    let hue = track_hue(ti);
    let mute = track.mute;
    html!("div", {
        .style("display", "flex")
        .style("height", &format!("{}px", row_h()))
        // Track strips read as a distinct surface above the darker void below the
        // last track; a 1px rule separates adjacent rows.
        .style("background", "var(--bg-1)")
        .style("border-bottom", "1px solid var(--line)")
        .child(track_header(arr, ti))
        .child(lane(ti, total, hue, mute, &track.clips))
    })
}

fn track_header(arr: &Arrangement, ti: usize) -> Dom {
    let track = &arr.tracks[ti];
    let name = track.name.clone();
    let mute = track.mute;
    let solo = track.solo;
    let gain = track.gain;
    html!("div", {
        .style("flex", "0 0 auto").style("width", &format!("{HDR_W}px")).style("box-sizing", "border-box")
        .style("position", "sticky").style("left", "0").style("z-index", "2")
        .style("border-right", "1px solid var(--line)")
        .style("display", "flex").style("flex-direction", "column").style("gap", "3px").style("padding", "5px 6px")
        .style("cursor", "pointer")
        // Highlight the selected track (double-click-to-place target).
        .style_signal("background", controller().selected_track.signal().map(move |s| {
            (if s == ti { "oklch(0.26 0.04 230)" } else { "var(--bg-1)" }).to_string()
        }))
        .style_signal("box-shadow", controller().selected_track.signal().map(move |s| {
            (if s == ti { "inset 3px 0 0 var(--accent-bright)" } else { "none" }).to_string()
        }))
        .event(move |_: events::PointerDown| controller().selected_track.set(ti))
        .child(html!("div", {
            .style("display", "flex").style("gap", "4px").style("align-items", "center")
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("value", &name)
                .style("flex", "1").style("min-width", "0").style("padding", "2px 5px")
                .style("font-size", "11.5px").style("font-weight", "600").style("border-radius", "4px")
                .style("border", "1px solid var(--line)").style("background", "var(--bg-0)").style("color", "inherit")
                .with_node!(el => {
                    .event(move |_: events::Change| {
                        controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetTrackName { track: ti, name: el.value() } });
                    })
                })
            }))
            .child(html!("button", {
                .style_unchecked("border", "none").style("background", "transparent")
                .style("color", "oklch(0.6 0.06 25)").style("cursor", "pointer").style("font-size", "13px")
                .attr("title", "Delete track").text("\u{1F5D1}")
                .event(move |_: events::Click| controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::RemoveTrack { track: ti } }))
            }))
        }))
        .child(html!("div", {
            .style("display", "flex").style("gap", "4px").style("align-items", "center")
            .child(html!("button", {
                .style("padding", "1px 7px").style("cursor", "pointer").style("border-radius", "4px").style("font-size", "11px").style("color", "inherit")
                .style("border", "1px solid var(--line)")
                .style("background", if mute { "oklch(0.45 0.12 25)" } else { "var(--bg-2)" })
                .text(if mute { "muted" } else { "mute" })
                .event(move |_: events::Click| controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetTrackMute { track: ti, mute: !mute } }))
            }))
            .child(html!("button", {
                .style("padding", "1px 7px").style("cursor", "pointer").style("border-radius", "4px").style("font-size", "11px").style("color", "inherit")
                .style("border", "1px solid var(--line)")
                .style("background", if solo { "oklch(0.55 0.13 95)" } else { "var(--bg-2)" })
                .attr("title", "Solo: when any track is soloed, only soloed tracks play")
                .text(if solo { "soloed" } else { "solo" })
                .event(move |_: events::Click| controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetTrackSolo { track: ti, solo: !solo } }))
            }))
            .child(html!("input" => web_sys::HtmlInputElement, {
                .attr("type", "range").attr("min", "0").attr("max", "2").attr("step", "0.01")
                .attr("value", &format!("{gain}"))
                .style("flex", "1").style("min-width", "0")
                .attr("title", "Track volume (1.0 = unity; release to apply)")
                .with_node!(el => {
                    // Commit on `change` (drag release), NOT `input`: SetTrackGain
                    // bumps `samples_rev`, which rebuilds this whole view — doing
                    // that on every `input` tick recreates the slider mid-drag and
                    // the drag dies. The browser moves the thumb natively during
                    // the drag; we apply the value once, on release.
                    .event(move |_: events::Change| {
                        if let Ok(v) = el.value().parse::<f32>() {
                            controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SetTrackGain { track: ti, gain: v } });
                        }
                    })
                })
            }))
        }))
    })
}

fn lane(ti: usize, total: f64, hue: f64, mute: bool, clips: &[awsm_audio_schema::Clip]) -> Dom {
    let clips_owned: Vec<awsm_audio_schema::Clip> = clips.to_vec();
    let bpm = controller()
        .arrangement_view()
        .map(|a| a.bpm)
        .unwrap_or(120.0);
    // Grid lines drawn over the track strip (track_row's `--bg-1` fill shows
    // through the transparent gaps): bar lines crisp (`--line`), beat ticks faint
    // (`--bg-3`).
    let grid = format!(
        "repeating-linear-gradient(90deg, var(--line) 0 1px, transparent 1px {bar}px), repeating-linear-gradient(90deg, var(--bg-3) 0 1px, transparent 1px {beat}px)",
        bar = 4.0 * 60.0 / bpm.max(1.0) * pps(),
        beat = 60.0 / bpm.max(1.0) * pps(),
    );
    html!("div" => web_sys::HtmlElement, {
        .style("position", "relative").style("flex", "0 0 auto")
        .style("width", &format!("{}px", total * pps()))
        .style("height", &format!("{}px", row_h()))
        .style("touch-action", "none")
        .style("opacity", if mute { "0.5" } else { "1" })
        .style_unchecked("background", &grid)
        .after_inserted(move |el| { LANES.with(|l| l.borrow_mut().push((ti, el.unchecked_into()))); })
        .children(clips.iter().enumerate().map(move |(ci, clip)| clip_block(ti, ci, clip, hue)))
        // Live ghost for a draw (Create) gesture on this lane.
        .child(html!("div", {
            .style("position", "absolute").style("top", "3px").style("bottom", "3px")
            .style("border-radius", "5px").style("pointer-events", "none")
            .style("border", &format!("1px dashed oklch(0.85 0.13 {hue})"))
            .style("background", &format!("oklch(0.6 0.1 {hue} / 0.3)"))
            .style_signal("display", CREATE.with(|p| p.signal().map(move |c| {
                (if matches!(c, Some((t, _, _)) if t == ti) { "block" } else { "none" }).to_string()
            })))
            .style_signal("left", CREATE.with(|p| p.signal().map(move |c| {
                format!("{}px", c.filter(|(t, _, _)| *t == ti).map_or(0.0, |(_, s, _)| s * pps()))
            })))
            .style_signal("width", CREATE.with(|p| p.signal().map(move |c| {
                format!("{}px", c.filter(|(t, _, _)| *t == ti).map_or(0.0, |(_, _, l)| (l * pps()).max(2.0)))
            })))
        }))
        // Drop target while dragging an asset from the panel onto this lane.
        .child(html!("div", {
            .style("position", "absolute").style("top", "3px").style("bottom", "3px")
            .style("border-radius", "5px").style("pointer-events", "none").style("z-index", "2")
            .style("border", &format!("2px dashed oklch(0.85 0.13 {hue})"))
            .style("background", &format!("oklch(0.65 0.12 {hue} / 0.28)"))
            .style_signal("display", DROP.with(|p| p.signal().map(move |c| {
                (if matches!(c, Some((t, _, _)) if t == ti) { "block" } else { "none" }).to_string()
            })))
            .style_signal("left", DROP.with(|p| p.signal().map(move |c| {
                format!("{}px", c.filter(|(t, _, _)| *t == ti).map_or(0.0, |(_, s, _)| s * pps()))
            })))
            .style_signal("width", DROP.with(|p| p.signal().map(move |c| {
                format!("{}px", c.filter(|(t, _, _)| *t == ti).map_or(0.0, |(_, _, l)| (l * pps()).max(2.0)))
            })))
        }))
        // Blade hover indicator: where a click would cut, shown before cutting.
        .child(html!("div", {
            .style("position", "absolute").style("top", "0").style("bottom", "0")
            .style("width", "2px").style("pointer-events", "none").style("z-index", "3")
            .style("background", "oklch(0.95 0.22 25)")
            .style("box-shadow", "0 0 4px oklch(0.95 0.22 25)")
            .style_signal("display", map_ref! {
                let b = BLADE.with(|m| m.signal()),
                let tool = TOOL.with(|m| m.signal()) =>
                (if *tool == Tool::Blade && matches!(b, Some((t, _)) if *t == ti) { "block" } else { "none" }).to_string()
            })
            .style_signal("left", BLADE.with(|m| m.signal().map(move |b| {
                format!("{}px", b.filter(|(t, _)| *t == ti).map_or(0.0, |(_, s)| s * pps()))
            })))
        }))
        // Track the blade cut point while hovering a clip with the Blade tool.
        .event(move |e: events::PointerMove| {
            if TOOL.with(|m| m.get()) != Tool::Blade {
                return;
            }
            let over_clip = e
                .target()
                .and_then(|t| t.dyn_ref::<web_sys::Element>().cloned())
                .and_then(|el| el.get_attribute("data-clip"))
                .is_some();
            BLADE.with(|m| m.set(over_clip.then(|| (ti, secs_at(e.x())))));
        })
        .event(move |_: events::PointerLeave| BLADE.with(|m| m.set(None)))
        .event(clone!(clips_owned => move |e: events::PointerDown| {
            controller().selected_track.set(ti);
            let target = e.target().and_then(|t| t.dyn_ref::<web_sys::Element>().cloned());
            let clip_idx = target.as_ref().and_then(|el| el.get_attribute("data-clip")).and_then(|s| s.parse::<usize>().ok());
            let edge = target.as_ref().and_then(|el| el.get_attribute("data-edge"));
            let t = secs_at(e.x());
            let tool = TOOL.with(|m| m.get());
            match clip_idx.and_then(|i| clips_owned.get(i).map(|c| (i, c))) {
                Some((ci, c)) => {
                    if tool == Tool::Blade {
                        // Blade splits at the exact click for sample-fine cuts (no snap).
                        controller().dispatch(EditorCommand::EditArrange { op: ArrangeOp::SplitClip { track: ti, clip: ci, at: t } });
                        return;
                    }
                    // Click a clip → select just it (replace any prior selection).
                    controller().selected_clips.set(vec![(ti, ci)]);
                    // Stretch tool: drag the clip to change length + speed. Else the
                    // edge handles trim, and the body moves.
                    let kind = if tool == Tool::Stretch {
                        DragKind::Stretch
                    } else {
                        match edge.as_deref() {
                            Some("L") => DragKind::TrimStart,
                            Some(_) => DragKind::Resize,
                            None => DragKind::Move,
                        }
                    };
                    // Right-edge resize can't run past the buffer unless looping.
                    let max_len = if c.looping {
                        f64::INFINITY
                    } else {
                        // Buffer remaining is (duration − offset); at `speed` that
                        // covers (duration − offset) / speed timeline seconds.
                        let avail = (controller().bounce_duration(c.source).unwrap_or(c.length) - c.offset).max(0.0);
                        (avail / (c.speed as f64).max(0.01)).max(0.05)
                    };
                    DRAG.with(|d| *d.borrow_mut() = Some(DragInfo {
                        origin: ti, clip: ci, kind, grab_off: c.start - t, len: c.length,
                        offset: c.offset, base_start: c.start, max_len,
                        base_speed: c.speed as f64, source: None,
                    }));
                    PREVIEW.with(|p| p.set(Some((ti, ci, c.start, c.length))));
                }
                None => {
                    controller().selected_clips.set(Vec::new());
                    if tool == Tool::Draw {
                        if let Some(src) = SOURCE.with(|m| m.get()) {
                            // Start a draw: anchor the start; drag grows the length.
                            // A click without a drag drops the full bounce (see PointerUp).
                            // Magnet so a new clip lands flush against a neighbour.
                            let anchor = magnet_secs(t, bpm, &[]);
                            DRAG.with(|d| *d.borrow_mut() = Some(DragInfo {
                                origin: ti, clip: 0, kind: DragKind::Create,
                                grab_off: anchor, len: 0.0, offset: 0.0, base_start: anchor,
                                max_len: f64::INFINITY, base_speed: 1.0, source: Some(src),
                            }));
                        }
                    } else {
                        // Pointer tool: arm a marquee. It only becomes a box-select
                        // once the pointer actually moves (see PointerMove); a plain
                        // click sets the playhead instead (see PointerUp).
                        MARQUEE_START.with(|m| m.set(Some((e.x(), e.y()))));
                    }
                }
            }
        }))
        // Right-click empty lane → paste menu. Skip if the click landed on a clip
        // (its own menu handles that — dominator's stop_propagation is unreliable,
        // so we explicitly bail when the target sits inside a clip).
        .event_with_options(&EventOptions::preventable(), move |e: events::ContextMenu| {
            let on_clip = e
                .target()
                .and_then(|t| t.dyn_ref::<web_sys::Element>().cloned())
                .and_then(|el| el.closest("[data-clip]").ok().flatten())
                .is_some();
            if on_clip {
                return;
            }
            e.prevent_default();
            controller().selected_track.set(ti);
            controller().open_lane_menu(ti, snap_secs(secs_at(e.x()), bpm), e.x(), e.y());
        })
    })
}

fn clip_block(ti: usize, ci: usize, clip: &awsm_audio_schema::Clip, hue: f64) -> Dom {
    let name = if clip.name.is_empty() {
        format!("clip {}", ci + 1)
    } else {
        clip.name.clone()
    };
    let source = clip.source;
    let looping = clip.looping;
    let offset = clip.offset;
    let speed = clip.speed as f64;
    let base_start = clip.start;
    let base_len = clip.length;
    let speed_tag = if (clip.speed - 1.0).abs() > 0.01 {
        format!(" {:.2}\u{00d7}", clip.speed)
    } else {
        String::new()
    };
    html!("div", {
        .attr("data-clip", &ci.to_string())
        .style("position", "absolute")
        .style("top", "3px").style("bottom", "3px")
        .style("border-radius", "5px").style("box-sizing", "border-box")
        .style("border", &format!("1px solid oklch(0.72 0.13 {hue})"))
        .style("background", &format!("oklch(0.4 0.08 {hue} / 0.55)"))
        .style("overflow", "hidden").style("cursor", "grab")
        .style("font-size", "10.5px").style("user-select", "none")
        // Selected-clip highlight (copy / delete target).
        .style_signal("box-shadow", controller().selected_clips.signal_ref(move |s| {
            (if s.iter().any(|&(t, c)| t == ti && c == ci) { "0 0 0 2px oklch(0.92 0.16 90)" } else { "none" }).to_string()
        }))
        .style_signal("left", PREVIEW.with(|p| p.signal()).map(move |p| {
            let s = match p { Some((t, i, s, _)) if t == ti && i == ci => s, _ => base_start };
            format!("{}px", s * pps())
        }))
        .style_signal("width", PREVIEW.with(|p| p.signal()).map(move |p| {
            let l = match p { Some((t, i, _, l)) if t == ti && i == ci => l, _ => base_len };
            format!("{}px", (l * pps()).max(3.0))
        }))
        // Waveform fills the clip (windowed + tiled for trim / loop / speed).
        .child(clip_waveform(source, offset, base_len, looping, speed, hue))
        .child(html!("div", {
            .style("position", "relative").style("font-weight", "600").style("white-space", "nowrap")
            .style("pointer-events", "none").style("padding", "1px 4px")
            .style("text-shadow", "0 1px 2px oklch(0 0 0 / 0.7)")
            .text(&format!("{name}{speed_tag}"))
        }))
        // Visible loop toggle (top-right). Accent-filled when looping. Suppresses
        // the lane's drag/select pointerdown so clicking it never moves the clip.
        .child(html!("button", {
            .class("t")
            .attr("title", if looping { "Looping — click to stop looping" } else { "Loop this clip" })
            .style("position", "absolute")
            .style("top", "3px").style("right", "3px")
            .style("display", "inline-flex").style("align-items", "center").style("justify-content", "center")
            .style("width", "19px").style("height", "19px").style("padding", "0")
            .style("border-radius", "var(--r1)").style("cursor", "pointer")
            .style("border", if looping { "1px solid var(--accent-bright)" } else { "1px solid oklch(1 0 0 / 0.22)" })
            .style("background", if looping { "var(--accent)" } else { "oklch(0 0 0 / 0.4)" })
            .style("color", if looping { ACCENT_FG } else { "oklch(0.96 0 0)" })
            .child(Icon::new("loop").size(12.0).stroke_width(2.0).render())
            // Don't let the press bubble to the lane (which would start a drag).
            .event(move |e: events::PointerDown| e.stop_propagation())
            .event(move |e: events::Click| {
                e.stop_propagation();
                controller().dispatch(EditorCommand::EditArrange {
                    op: ArrangeOp::SetClipLoop { track: ti, clip: ci, looping: !looping },
                });
            })
        }))
        // Left-edge trim handle (move start in, keep right edge fixed).
        .child(html!("div", {
            .attr("data-clip", &ci.to_string()).attr("data-edge", "L")
            .style("position", "absolute").style("left", "0").style("top", "0").style("bottom", "0")
            .style("width", "7px").style("cursor", "ew-resize")
        }))
        // Right-edge resize handle.
        .child(html!("div", {
            .attr("data-clip", &ci.to_string()).attr("data-edge", "R")
            .style("position", "absolute").style("right", "0").style("top", "0").style("bottom", "0")
            .style("width", "7px").style("cursor", "ew-resize")
        }))
        .event_with_options(&EventOptions::preventable(), move |e: events::ContextMenu| {
            e.prevent_default();
            e.stop_propagation();
            // Right-clicking a clip that isn't part of the current selection selects
            // just it; right-clicking within a multi-selection keeps the group (so
            // the menu can act on the whole selection).
            let in_selection = controller().selected_clips().iter().any(|&(t, c)| t == ti && c == ci);
            if !in_selection {
                controller().selected_clips.set(vec![(ti, ci)]);
            }
            controller().open_clip_menu(ti, ci, e.x(), e.y());
        })
        .event(move |_: events::DoubleClick| controller().open_clip_source(ti, ci))
    })
}

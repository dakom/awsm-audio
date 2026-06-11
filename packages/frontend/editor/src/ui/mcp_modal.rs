//! The MCP connect surface: a top-bar button that reflects the remote link's
//! [`status`](crate::remote::status), plus a connect modal (an editable origin +
//! Connect). Mounted once in [`crate::ui`]; visible whenever [`OPEN`] is set.
//!
//! The button *always* opens the modal (never a silent toggle): disconnected →
//! "MCP" opens it to connect; connecting → disabled label; connected → "MCP ✓"
//! opens it to show the live connection + an explicit Disconnect button. The
//! origin pre-fills from [`remote::origin`](crate::remote::origin) (the `?mcp=`
//! value or the baked [`default_origin`](crate::remote::default_origin)).

use dominator::{clone, events, html, with_node, Dom};
use futures_signals::map_ref;
use futures_signals::signal::{Mutable, SignalExt};

use crate::remote::{self, RemoteStatus};
use crate::widgets::{Btn, BtnSize, BtnVariant};

thread_local! {
    /// Whether the connect modal is open. UI-only state, so it lives here rather
    /// than on the controller.
    static OPEN: Mutable<bool> = Mutable::new(false);
}

fn open_state() -> Mutable<bool> {
    OPEN.with(|o| o.clone())
}

/// The top-bar MCP button (reactive to the link status) plus, when connected, an
/// activity chip that pulses while the agent is working and reads "idle" when it's
/// safe to edit / the result is ready to export.
pub fn button() -> Dom {
    html!("div", {
        .style("display", "flex")
        .style("align-items", "center")
        .style("gap", "6px")
        .child_signal(remote::status().signal().map(|status| Some(status_button(status))))
        // The activity chip only exists while connected; it reflects `working`.
        .child_signal(map_ref! {
            let status = remote::status().signal(),
            let working = remote::working().signal() =>
            (*status == RemoteStatus::Connected).then(|| activity_chip(*working))
        })
    })
}

fn status_button(status: RemoteStatus) -> Dom {
    match status {
        RemoteStatus::Disconnected => Btn::new()
            .label("MCP")
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .title("Connect to an MCP server")
            .on_click(|| open_state().set(true))
            .render(),
        RemoteStatus::Connecting => Btn::new()
            .label("MCP…")
            .variant(BtnVariant::Ghost)
            .size(BtnSize::Sm)
            .title("Connecting…")
            .disabled(true)
            .on_click(|| {})
            .render(),
        RemoteStatus::Connected => Btn::new()
            .label("MCP ✓")
            .variant(BtnVariant::Primary)
            .size(BtnSize::Sm)
            .title("Connected — click for MCP connection options")
            .on_click(|| open_state().set(true))
            .render(),
    }
}

/// The "🤖 agent working… / idle" chip. Pulses (via the `mcp-pulse` keyframes in
/// [`crate::theme`]) while the agent is serving requests; calm + muted when idle.
fn activity_chip(working: bool) -> Dom {
    html!("div", {
        .style("display", "inline-flex")
        .style("align-items", "center")
        .style("gap", "5px")
        // Match the Sm MCP button's footprint exactly (height/font/radius).
        .style("height", "26px")
        .style("box-sizing", "border-box")
        .style("padding", "0 11px")
        .style("border-radius", "var(--r2)")
        .style("border-style", "solid")
        .style("border-width", "1px")
        .style("font-size", "12.5px")
        .style("font-weight", "550")
        .style("white-space", "nowrap")
        .style("user-select", "none")
        .apply(|d| if working {
            d.style("color", "var(--accent-bright)")
                .style("background", "oklch(0.7 0.13 230 / 0.16)")
                .style("border-color", "var(--accent-line)")
                // Pulse the whole chip's opacity while work is in flight.
                .style("animation", "mcp-pulse 1.1s ease-in-out infinite")
                .attr("title", "Agent is working — changes are landing live; wait for idle before editing or exporting.")
        } else {
            d.style("color", "var(--text-3)")
                .style("background", "transparent")
                .style("border-color", "var(--line)")
                .attr("title", "Agent idle — safe to edit / export.")
        })
        .child(html!("span", { .text("🤖") }))
        .child(html!("span", { .text(if working { "working…" } else { "idle" }) }))
    })
}

/// The status-aware banner at the top of the modal body: a live "connected to
/// <origin>" line (green dot) when attached, else the "how to connect" blurb.
fn connection_banner(status: RemoteStatus, origin: String) -> Dom {
    match status {
        RemoteStatus::Connected => html!("div", {
            .style("display", "flex")
            .style("align-items", "center")
            .style("gap", "8px")
            .style("margin", "0 0 14px")
            .style("padding", "9px 11px")
            .style("border-radius", "8px")
            .style("background", "oklch(0.7 0.15 150 / 0.12)")
            .style("border", "1px solid oklch(0.7 0.15 150 / 0.4)")
            .style("font-size", "13px")
            .style("color", "var(--text-1)")
            .child(html!("span", {
                .style("width", "9px")
                .style("height", "9px")
                .style("border-radius", "50%")
                .style("background", "oklch(0.72 0.17 150)")
                .style("flex", "none")
            }))
            .child(html!("span", {
                .child(html!("strong", { .text("Connected") }))
                .child(html!("span", {
                    .style("color", "var(--text-2)")
                    .text(&format!(" to {origin}"))
                }))
            }))
        }),
        RemoteStatus::Connecting => html!("p", {
            .style("margin", "0 0 14px")
            .style("font-size", "13px")
            .style("color", "var(--text-2)")
            .text(&format!("Connecting to {origin}…"))
        }),
        RemoteStatus::Disconnected => html!("p", {
            .style("margin", "0 0 14px")
            .style("font-size", "13px")
            .style("line-height", "1.5")
            .style("color", "var(--text-2)")
            .text("The editor dials out to a local MCP server over a WebSocket. \
                   Start it with `task mcp:serve`, then connect to its control origin.")
        }),
    }
}

/// The connect modal overlay (mounted once; shown when [`OPEN`] is set).
pub fn render() -> Dom {
    html!("div", {
        .child_signal(open_state().signal().map(|open| open.then(view)))
    })
}

fn view() -> Dom {
    // Seed the editable fields from the remembered origin + pairing code + TLS.
    let value = Mutable::new(remote::origin().get_cloned());
    let pair_value = Mutable::new(remote::pair().get_cloned());
    let tls_value = Mutable::new(remote::tls().get());

    let submit = clone!(value, pair_value, tls_value => move || {
        let origin = value.get_cloned().trim().to_string();
        let code = pair_value.get_cloned().trim().to_string();
        remote::pair().set(code.clone());
        remote::tls().set(tls_value.get());
        if remote::status().get() == RemoteStatus::Connected {
            // Already attached — just (re)claim a binding with the entered code.
            if !code.is_empty() {
                remote::submit_pair_code(code);
            }
        } else if !origin.is_empty() {
            // `run` sends the stashed pairing code on attach.
            remote::connect(origin);
        }
        open_state().set(false);
    });

    html!("div", {
        .style("position", "fixed")
        .style("inset", "0")
        .style("z-index", "1000")
        // Backdrop closes the modal.
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("background", "oklch(0 0 0 / 0.62)")
            .style("backdrop-filter", "blur(2px)")
            .style_unchecked("-webkit-backdrop-filter", "blur(2px)")
            .event(|_: events::Click| open_state().set(false))
        }))
        // Centering layer (transparent to pointer events; the panel re-enables).
        .child(html!("div", {
            .style("position", "absolute")
            .style("inset", "0")
            .style("display", "flex")
            .style("align-items", "center")
            .style("justify-content", "center")
            .style("pointer-events", "none")
            .child(html!("div", {
                .style("pointer-events", "auto")
                .style("width", "440px")
                .style("max-width", "calc(100vw - 40px)")
                .style("padding", "22px 24px")
                .style("border-radius", "12px")
                .style("background", "var(--bg-2)")
                .style("border", "1px solid var(--accent)")
                .style("box-shadow", "0 24px 70px oklch(0 0 0 / 0.6)")
                .child(html!("div", {
                    .style("display", "flex")
                    .style("align-items", "center")
                    .style("justify-content", "space-between")
                    .style("gap", "12px")
                    .style("margin", "0 0 6px")
                    .child(html!("h2", {
                        .style("margin", "0")
                        .style("font-size", "17px")
                        .style("font-weight", "650")
                        .text("Connect to MCP server")
                    }))
                    // Jump to the full MCP walkthrough (install / run / connect /
                    // point an agent at it) — closes this modal, opens the help guide.
                    .child(Btn::new()
                        .label("Help")
                        .icon("help")
                        .variant(BtnVariant::Ghost)
                        .size(BtnSize::Sm)
                        .title("How the MCP works — open the help guide")
                        .on_click(|| {
                            open_state().set(false);
                            crate::controller::controller()
                                .open_help_at(crate::ui::help_modal::mcp_tab_index());
                        })
                        .render())
                }))
                // Status-aware banner: when connected, show the live origin + a
                // green dot; otherwise the "how to connect" blurb.
                .child_signal(map_ref! {
                    let status = remote::status().signal(),
                    let origin = remote::origin().signal_cloned() =>
                    Some(connection_banner(*status, origin.clone()))
                })
                .child(html!("input" => web_sys::HtmlInputElement, {
                    .style("width", "100%")
                    .style("box-sizing", "border-box")
                    .style("padding", "8px 10px")
                    .style("font-size", "13.5px")
                    .style("border-radius", "8px")
                    .style("border", "1px solid var(--line)")
                    .style("background", "var(--bg-1)")
                    .style("color", "var(--text-1)")
                    .attr("type", "text")
                    .attr("spellcheck", "false")
                    .attr("placeholder", "127.0.0.1:9171")
                    .attr("value", &value.get_cloned())
                    .with_node!(input => {
                        .event(clone!(value, input => move |_: events::Input| {
                            value.set(input.value());
                        }))
                        .event(clone!(submit => move |e: events::KeyDown| {
                            if e.key() == "Enter" {
                                submit();
                            }
                        }))
                    })
                }))
                // Pairing code: only needed when more than one tab/agent is
                // connected (the server asks via `PairingRequired`). The field is
                // always available; the hint appears when it's actually required.
                .child(html!("input" => web_sys::HtmlInputElement, {
                    .style("width", "100%")
                    .style("box-sizing", "border-box")
                    .style("margin-top", "8px")
                    .style("padding", "8px 10px")
                    .style("font-size", "13.5px")
                    .style("border-radius", "8px")
                    .style("border", "1px solid var(--line)")
                    .style("background", "var(--bg-1)")
                    .style("color", "var(--text-1)")
                    .style("text-transform", "uppercase")
                    .attr("type", "text")
                    .attr("spellcheck", "false")
                    .attr("placeholder", "pairing code (only if prompted)")
                    .attr("value", &pair_value.get_cloned())
                    .with_node!(input => {
                        .event(clone!(pair_value, input => move |_: events::Input| {
                            pair_value.set(input.value());
                        }))
                        .event(clone!(submit => move |e: events::KeyDown| {
                            if e.key() == "Enter" {
                                submit();
                            }
                        }))
                    })
                }))
                .child(html!("p", {
                    .style("margin", "8px 0 0")
                    .style("font-size", "12px")
                    .style("line-height", "1.45")
                    .style("color", "var(--warning, var(--accent-bright))")
                    .visible_signal(remote::pairing_needed().signal())
                    .text("This server has more than one editor/agent connected — \
                           enter the pairing code your agent printed to attach this tab.")
                }))
                // TLS: off by default (the server is normally local plain HTTP).
                // Tick for a TLS-terminated remote server — same as `&tls=true`.
                .child(html!("label", {
                    .style("display", "flex")
                    .style("align-items", "center")
                    .style("gap", "8px")
                    .style("margin-top", "10px")
                    .style("font-size", "13px")
                    .style("color", "var(--text-2)")
                    .style("cursor", "pointer")
                    .style("user-select", "none")
                    .child(html!("input" => web_sys::HtmlInputElement, {
                        .attr("type", "checkbox")
                        .apply(clone!(tls_value => move |b| if tls_value.get() { b.attr("checked", "") } else { b }))
                        .with_node!(cb => {
                            .event(clone!(tls_value, cb => move |_: events::Change| {
                                tls_value.set(cb.checked());
                            }))
                        })
                    }))
                    .text("Use TLS (wss/https) — for a server behind HTTPS")
                }))
                // Footer actions, reactive to the link status: when connected, an
                // explicit Disconnect (so the button never silently toggles off);
                // otherwise Connect.
                .child(html!("div", {
                    .style("display", "flex")
                    .style("justify-content", "flex-end")
                    .style("gap", "8px")
                    .style("margin-top", "16px")
                    .child(Btn::new()
                        .label("Close")
                        .variant(BtnVariant::Ghost)
                        .size(BtnSize::Sm)
                        .on_click(|| open_state().set(false))
                        .render())
                    .child_signal(remote::status().signal().map(clone!(submit => move |status| {
                        Some(match status {
                            RemoteStatus::Connected => Btn::new()
                                .label("Disconnect")
                                .variant(BtnVariant::Danger)
                                .size(BtnSize::Sm)
                                .title("Drop the MCP link")
                                .on_click(|| {
                                    remote::disconnect();
                                    open_state().set(false);
                                })
                                .render(),
                            RemoteStatus::Connecting => Btn::new()
                                .label("Connecting…")
                                .variant(BtnVariant::Primary)
                                .size(BtnSize::Sm)
                                .disabled(true)
                                .on_click(|| {})
                                .render(),
                            RemoteStatus::Disconnected => Btn::new()
                                .label("Connect")
                                .variant(BtnVariant::Primary)
                                .size(BtnSize::Sm)
                                .on_click(clone!(submit => submit))
                                .render(),
                        })
                    })))
                }))
            }))
        }))
    })
}

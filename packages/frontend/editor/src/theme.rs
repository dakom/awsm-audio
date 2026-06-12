//! The editor's design system: a graphite/slate pro-DCC palette in OKLCH,
//! ported from the awsm-renderer editor so the two tools share a visual
//! language. Everything resolves through `:root` custom properties (the single
//! source of truth) — `--bg-*` surfaces, `--text-*` ramps, `--accent`
//! derivations, `--line*` rules — so a component only ever references
//! `var(--…)`, never a raw color.
//!
//! [`init`] installs the `:root` token block plus the base element resets and a
//! handful of utility classes (`.t`, `.mono`, `.kicker`, `.focusring`). Call it
//! once at boot, before any UI is mounted.

use dominator::stylesheet;

pub const FONT_BODY: &str =
    r#"ui-sans-serif, system-ui, -apple-system, "Segoe UI", "Helvetica Neue", sans-serif"#;
pub const FONT_MONO: &str = r#""JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace"#;

/// Foreground used on top of an accent fill (primary buttons, the brand mark).
pub const ACCENT_FG: &str = "oklch(0.16 0.02 255)";

pub fn init() {
    // ---- :root design tokens (graphite/slate, OKLCH) ----
    stylesheet!(":root", {
        .style("box-sizing", "border-box")
        .style("font-size", "13px")

        // accent — a single restrained azure.
        .style("--accent", "#5b8dd6")

        // surfaces (cool-neutral graphite)
        .style("--bg-0", "oklch(0.155 0.006 255)")   // deepest — viewport / app void
        .style("--bg-1", "oklch(0.196 0.006 255)")   // panel base
        .style("--bg-2", "oklch(0.228 0.007 255)")   // elevated / headers / toolbar
        .style("--bg-3", "oklch(0.150 0.006 255)")   // input well / inset
        .style("--bg-hover", "oklch(0.270 0.009 255)")
        .style("--bg-active", "oklch(0.305 0.010 255)")

        // lines
        .style("--line", "oklch(0.315 0.008 255)")
        .style("--line-soft", "oklch(0.262 0.007 255)")
        .style("--line-strong", "oklch(0.38 0.010 255)")

        // text
        .style("--text-0", "oklch(0.945 0.004 255)")
        .style("--text-1", "oklch(0.715 0.007 255)")
        .style("--text-2", "oklch(0.560 0.007 255)")
        .style("--text-3", "oklch(0.440 0.007 255)")

        // accent derivations
        .style("--accent-bright", "color-mix(in oklch, var(--accent) 78%, white)")
        .style("--accent-dim", "color-mix(in oklch, var(--accent) 82%, black)")
        .style("--accent-ghost", "color-mix(in oklch, var(--accent) 15%, transparent)")
        .style("--accent-line", "color-mix(in oklch, var(--accent) 42%, transparent)")

        // functional
        .style("--danger", "oklch(0.650 0.170 25)")
        .style("--danger-soft", "oklch(0.650 0.170 25 / 0.16)")
        .style("--danger-bright", "oklch(0.730 0.150 25)")
        .style("--ok", "oklch(0.740 0.130 150)")
        .style("--ok-soft", "oklch(0.740 0.130 150 / 0.14)")
        .style("--warn", "oklch(0.800 0.130 85)")
        .style("--warn-soft", "oklch(0.800 0.130 85 / 0.14)")

        // radii
        .style("--r1", "4px")
        .style("--r2", "6px")
        .style("--r3", "9px")
        .style("--r4", "13px")

        // type
        .style("--font", FONT_BODY)
        .style("--mono", FONT_MONO)

        // shadows
        .style("--shadow-1", "0 1px 2px oklch(0 0 0 / 0.35)")
        .style("--shadow-2", "0 6px 18px -6px oklch(0 0 0 / 0.55), 0 2px 6px oklch(0 0 0 / 0.35)")
        .style("--shadow-3", "0 18px 50px -12px oklch(0 0 0 / 0.70), 0 4px 12px oklch(0 0 0 / 0.4)")
    });

    stylesheet!("*, ::before, ::after", {
        .style("box-sizing", "inherit")
    });

    stylesheet!("html, body", {
        .style("height", "100%")
        .style("margin", "0")
        .style("padding", "0")
        .style("font-family", "var(--font)")
        .style("font-size", "13px")
        .style("line-height", "1.4")
        .style("background", "var(--bg-0)")
        .style("color", "var(--text-0)")
        .style("-webkit-font-smoothing", "antialiased")
        .style("text-rendering", "optimizeLegibility")
        .style("overflow", "hidden")
    });

    stylesheet!("input, button, select, textarea", {
        .style("font-family", "inherit")
    });

    // Normalize native button rendering so interaction states never fall back to
    // browser default white/blue styles after click/focus.
    stylesheet!("button", {
        .style("appearance", "none")
        .style("-webkit-appearance", "none")
        .style("background-color", "transparent")
        .style("background-image", "none")
        .style("color", "inherit")
        .style("font-family", "var(--font)")
    });

    stylesheet!("::selection", {
        .style("background", "var(--accent-ghost)")
    });

    // Slim pro-tool scrollbars.
    stylesheet!("::-webkit-scrollbar", {
        .style("width", "10px")
        .style("height", "10px")
    });
    stylesheet!("::-webkit-scrollbar-thumb", {
        .style("background", "oklch(0.34 0.008 255)")
        .style("border", "3px solid transparent")
        .style("background-clip", "padding-box")
        .style("border-radius", "10px")
    });
    stylesheet!("::-webkit-scrollbar-thumb:hover", {
        .style("background", "oklch(0.42 0.010 255)")
        .style("background-clip", "padding-box")
    });
    stylesheet!("::-webkit-scrollbar-corner", {
        .style("background", "transparent")
    });

    // Utility classes used directly by the editor chrome.
    stylesheet!(".mono", {
        .style("font-family", "var(--mono)")
        .style("font-feature-settings", "\"tnum\" 1")
    });
    stylesheet!(".kicker", {
        .style("font-size", "10.5px")
        .style("font-weight", "650")
        .style("letter-spacing", "0.09em")
        .style("text-transform", "uppercase")
        .style("color", "var(--text-2)")
        .style("user-select", "none")
    });
    // Transition utility: border/shadow/transform only — background/color update
    // instantly so selection/active states never lag.
    stylesheet!(".t", {
        .style("transition", "border-color .12s ease, box-shadow .12s ease, transform .12s ease")
    });
    // Keyboard-only focus ring (a bg spacer + accent ring) shown for
    // :focus-visible, so mouse clicks don't paint a ring but tab-nav does.
    stylesheet!(".focusring:focus-visible", {
        .style("outline", "none")
        .style("box-shadow", "0 0 0 1.5px var(--bg-1), 0 0 0 3px var(--accent-line)")
    });

    // `@keyframes` aren't a selector rule, so inject them as a raw <style>. Drives
    // the "🤖 agent working…" MCP pulse (see `ui::mcp_modal`).
    inject_keyframes("@keyframes mcp-pulse{0%,100%{opacity:1}50%{opacity:0.4}}");
    // The MCP auto-follow "spotlight": a glow that flares on the node the agent
    // just touched, then fades — so the eye catches where a change landed (see
    // `ui::node` + `mcp_activity`).
    inject_keyframes(
        "@keyframes mcp-spotlight{\
         0%{box-shadow:0 0 0 3px var(--accent-bright),0 0 22px 4px var(--accent-line),0 6px 18px oklch(0 0 0 / 0.4)}\
         100%{box-shadow:0 0 0 0 transparent,0 0 0 0 transparent,0 6px 18px oklch(0 0 0 / 0.4)}}",
    );
}

/// Append a raw CSS rule (e.g. an `@keyframes` block) to the document head.
fn inject_keyframes(css: &str) {
    if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
        if let (Ok(style), Some(head)) = (doc.create_element("style"), doc.head()) {
            style.set_text_content(Some(css));
            let _ = head.append_child(&style);
        }
    }
}

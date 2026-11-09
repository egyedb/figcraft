//! XD prototype interactions → REST interactions.
//!
//! `interactions/interactions.json` lists each layer's interactions under the layer's id: a
//! `triggerEvent` (`tap`, `hover`, `drag`, `time`, `keys-gamepad`), an `action`
//! (`artboard-transition`, `auto-animate`, `overlay-transition`, `previous-artboard`,
//! `scroll-transition`, `hyperlink`, `state-transition`, `audio-file-playback`) and its
//! `properties`: the `destination` (an artboard, a layer to scroll to, a state), the `transition`
//! (`dissolve`, `none`, `auto-animate`, `slide-left`, `push-up`…), its `duration` in seconds and
//! `easing`, and for overlays their `overlayPosition` on the artboard. `homeArtboard`, and
//! artboards with a `meta.ux.flowName`, are where prototypes start.

use serde_json::{Value, json};

use super::tree::{Cx, num, str_field};

/// Most interactions read from one layer.
const MAX_PER_LAYER: usize = 64;
/// Longest wait or animation kept (ms).
const MAX_MS: f64 = 600_000.0;

/// Layer `id`'s interactions as REST interactions (empty when it has none). `at`: the layer's
/// top-left on the canvas, `artboard`: the canvas place of the artboard it is on (manual
/// overlays sit relative to the layer that opens them).
pub(crate) fn interactions(cx: &mut Cx, id: &str, at: Option<(f64, f64)>) -> Vec<Value> {
    let Some(list) = cx.protos.get(id).copied() else { return Vec::new() };
    let mut out = Vec::new();
    for it in list.as_array().map(Vec::as_slice).unwrap_or_default().iter().take(MAX_PER_LAYER) {
        if let Some(i) = interaction(cx, it, at) {
            out.push(i);
        }
    }
    out
}

fn interaction(cx: &mut Cx, it: &Value, at: Option<(f64, f64)>) -> Option<Value> {
    let p = it.get("properties").cloned().unwrap_or(Value::Null);
    let trigger = match str_field(it, "triggerEvent") {
        Some("tap") => json!({"type": "ON_CLICK"}),
        Some("hover") => json!({"type": "ON_HOVER"}),
        Some("drag") => json!({"type": "ON_DRAG"}),
        Some("time") => json!({"type": "AFTER_TIMEOUT", "timeout": ms(num(&p, "delay").unwrap_or(0.0))}),
        Some("keys-gamepad") => {
            let device = str_field(&p, "keysGamepadDevice").unwrap_or("keyboard");
            match key_code(str_field(&p, "keysGamepad").unwrap_or("")).filter(|_| device == "keyboard") {
                Some(code) => json!({"type": "ON_KEY_DOWN", "device": "KEYBOARD", "keyCodes": [code]}),
                None => {
                    cx.warn("prototype triggers on gamepads or keys FigCraft doesn't name were skipped");
                    return None;
                }
            }
        }
        Some("voice") => {
            cx.warn("voice prototype triggers are not supported and were skipped");
            return None;
        }
        _ => {
            cx.warn("prototype triggers of unknown kinds were skipped");
            return None;
        }
    };
    let dest = str_field(&p, "destination").filter(|d| !d.is_empty());
    let transition = transition(&p);
    let action = match str_field(it, "action") {
        Some("artboard-transition") => {
            json!({"type": "NODE", "destinationId": dest?, "navigation": "NAVIGATE", "transition": transition,
                "preserveScrollPosition": p.get("preserveScrollPosition").and_then(Value::as_bool).unwrap_or(false)})
        }
        Some("auto-animate") => {
            let mut t = transition_with(&p, "SMART_ANIMATE");
            if t.is_null() {
                t = json!({"type": "SMART_ANIMATE", "duration": ms(0.3), "easing": {"type": "EASE_OUT"}});
            }
            json!({"type": "NODE", "destinationId": dest?, "navigation": "NAVIGATE", "transition": t,
                "preserveScrollPosition": p.get("preserveScrollPosition").and_then(Value::as_bool).unwrap_or(false)})
        }
        Some("overlay-transition") => {
            let dest = dest?;
            let mut a = json!({"type": "NODE", "destinationId": dest, "navigation": "OVERLAY", "transition": transition});
            // XD puts the overlay at a place on the artboard; FigCraft measures a manual overlay from
            // the layer that opens it.
            let pos = p.get("overlayPosition").and_then(|o| Some((num(o, "x")?, num(o, "y")?)));
            match (pos, at, cx.artboard_at) {
                (Some((x, y)), Some((lx, ly)), Some((ax, ay))) if x != 0.0 || y != 0.0 => {
                    a["overlayRelativePosition"] = json!({"x": ax + x - lx, "y": ay + y - ly});
                    cx.overlays.entry(dest.to_string()).or_insert("MANUAL");
                }
                _ => {
                    cx.overlays.entry(dest.to_string()).or_insert("TOP_LEFT");
                }
            }
            a
        }
        Some("previous-artboard") => json!({"type": "BACK"}),
        Some("scroll-transition") => json!({"type": "NODE", "destinationId": dest?, "navigation": "SCROLL_TO", "transition": transition}),
        Some("hyperlink") => {
            let url = str_field(&p, "hyperlinkUrl").filter(|u| !u.trim().is_empty())?;
            json!({"type": "URL", "url": url.chars().take(4096).collect::<String>(), "openInNewTab": true})
        }
        Some("state-transition") => {
            cx.warn("prototype links that change a component's state were skipped (states are not supported yet)");
            return None;
        }
        Some("audio-file-playback" | "speech-playback") => {
            cx.warn("prototype sounds and speech are not supported and were skipped");
            return None;
        }
        _ => {
            cx.warn("prototype actions of unknown kinds were skipped");
            return None;
        }
    };
    Some(json!({"trigger": trigger, "actions": [action]}))
}

/// Seconds as whole milliseconds, within [`MAX_MS`].
fn ms(secs: f64) -> f64 {
    (secs * 1000.0).round().clamp(0.0, MAX_MS)
}

/// The REST transition of `properties` (null for none: instant).
fn transition(p: &Value) -> Value {
    transition_with(p, "")
}

/// [`transition`], with `kind` in place of the named transition when it isn't empty.
fn transition_with(p: &Value, kind: &str) -> Value {
    let named = str_field(p, "transition").unwrap_or("none");
    let (ty, direction) = if kind.is_empty() {
        match named {
            "dissolve" => ("DISSOLVE", None),
            "auto-animate" => ("SMART_ANIMATE", None),
            "slide-left" => ("SLIDE_IN", Some("LEFT")),
            "slide-right" => ("SLIDE_IN", Some("RIGHT")),
            "slide-up" => ("SLIDE_IN", Some("TOP")),
            "slide-down" => ("SLIDE_IN", Some("BOTTOM")),
            "push-left" => ("PUSH", Some("LEFT")),
            "push-right" => ("PUSH", Some("RIGHT")),
            "push-up" => ("PUSH", Some("TOP")),
            "push-down" => ("PUSH", Some("BOTTOM")),
            _ => return Value::Null,
        }
    } else {
        (kind, None)
    };
    let easing = match str_field(p, "easing") {
        Some("linear") => "LINEAR",
        Some("ease-in") => "EASE_IN",
        Some("ease-in-out") => "EASE_IN_AND_OUT",
        // XD's anticipating and overshooting curves.
        Some("wind-up") => "EASE_IN_BACK",
        Some("snap") => "EASE_OUT_BACK",
        Some("bounce") => "BOUNCY",
        _ => "EASE_OUT",
    };
    let mut t = json!({"type": ty, "duration": ms(num(p, "duration").unwrap_or(0.3)), "easing": {"type": easing}});
    if let Some(d) = direction {
        t["direction"] = json!(d);
    }
    t
}

/// The key code of a key XD names by its symbol or label.
fn key_code(key: &str) -> Option<u32> {
    Some(match key {
        "\u{21a9}" | "\u{23ce}" | "Enter" | "Return" => 13,
        "\u{21e5}" | "Tab" => 9,
        "Esc" | "Escape" | "\u{1b}" => 27,
        "Space" | " " => 32,
        "\u{2190}" => 37,
        "\u{2191}" => 38,
        "\u{2192}" => 39,
        "\u{2193}" => 40,
        "\u{232b}" | "Backspace" => 8,
        "\u{2326}" | "Delete" => 46,
        k => {
            let mut chars = k.chars();
            let c = chars.next()?.to_ascii_uppercase();
            if chars.next().is_some() || !c.is_ascii_alphanumeric() {
                return None;
            }
            u32::from(c)
        }
    })
}

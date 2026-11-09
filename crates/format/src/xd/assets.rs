//! XD document assets → REST style definitions.
//!
//! The resources' `meta.ux.documentLibrary.elements` are the document's assets: colours
//! (`…element.color+dcx`), gradients (`…element.gradient+dcx`) and character styles
//! (`…element.characterstyle+dcx`), each with its value in `representations[0].content`. Older
//! files keep plain colours in `meta.ux.colorSwatches`. Colours and gradients become paint styles,
//! character styles text styles (their colour stays on the layers, as FigCraft text styles carry
//! none). Layers name the asset they use by `linkId`: on a fill's or stroke's colour, and on a
//! text's styled runs.

use std::collections::HashSet;

use serde_json::{Value, json};

use super::tree::{Cx, num, str_field};

/// Most assets read from one file.
const MAX_ASSETS: usize = 20_000;

/// The ids of the styles made, by kind: what layers may link to.
#[derive(Default)]
pub(crate) struct Styles {
    pub paint: HashSet<String>,
    pub text: HashSet<String>,
}

/// The REST `styleDefinitions` of the assets in `resources`, with their ids.
pub(crate) fn definitions(cx: &mut Cx, resources: &Value) -> (Vec<Value>, Styles) {
    let ux = resources.get("resources").and_then(|r| r.get("meta")).and_then(|m| m.get("ux"));
    let elements =
        ux.and_then(|u| u.get("documentLibrary")).and_then(|l| l.get("elements")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    let mut out = Vec::new();
    let mut ids = Styles::default();
    for e in elements.iter().take(MAX_ASSETS) {
        let Some(id) = str_field(e, "id").filter(|i| !i.is_empty() && i.len() <= 128) else { continue };
        if ids.paint.contains(id) || ids.text.contains(id) {
            continue;
        }
        let kind = str_field(e, "type").unwrap_or("");
        let rep = e.get("representations").and_then(Value::as_array).and_then(|r| r.first());
        let content = rep.and_then(|r| r.get("content"));
        // A display name (with its group, `Group/Name`) before the plain one.
        let named = str_field(e, "displayName")
            .or_else(|| str_field(e, "name"))
            .filter(|n| !n.trim().is_empty())
            .map(|n| n.chars().take(200).collect::<String>());
        if kind.ends_with(".color+dcx") {
            let Some(c) = content.and_then(super::style::color) else { continue };
            let name = named.unwrap_or_else(|| hex(&c));
            out.push(json!({"id": id, "name": name, "styleType": "FILL", "node": {"fills": [{"type": "SOLID", "color": c}]}}));
            ids.paint.insert(id.to_string());
        } else if kind.ends_with(".gradient+dcx") {
            let radial = rep.and_then(|r| str_field(r, "type")).is_some_and(|t| t.contains("radial"));
            let Some(paint) = content.and_then(|c| gradient(c, radial)) else { continue };
            let name = named.unwrap_or_else(|| format!("Gradient {}", out.len() + 1));
            out.push(json!({"id": id, "name": name, "styleType": "FILL", "node": {"fills": [paint]}}));
            ids.paint.insert(id.to_string());
        } else if kind.ends_with(".characterstyle+dcx") {
            let Some(c) = content else { continue };
            let (style, label) = text_style(cx, c);
            let name = named.unwrap_or(label);
            out.push(json!({"id": id, "name": name, "styleType": "TEXT", "node": {"type": "TEXT", "characters": "", "style": style}}));
            ids.text.insert(id.to_string());
        }
    }
    // Older files' swatches: colours with no name, linked to by nothing.
    let swatches = ux.and_then(|u| u.get("colorSwatches")).and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for (i, s) in swatches.iter().take(MAX_ASSETS).enumerate() {
        let Some(c) = super::style::color(s) else { continue };
        let id = format!("xd-swatch:{i}");
        out.push(json!({"id": id, "name": hex(&c), "styleType": "FILL", "node": {"fills": [{"type": "SOLID", "color": c}]}}));
        ids.paint.insert(id);
    }
    (out, ids)
}

/// A colour's name: `#RRGGBB` (with the alpha as a percentage when it isn't opaque).
fn hex(c: &Value) -> String {
    let ch = |k: &str| (c.get(k).and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
    let a = c.get("a").and_then(Value::as_f64).unwrap_or(1.0);
    let base = format!("#{:02X}{:02X}{:02X}", ch("r"), ch("g"), ch("b"));
    if a < 1.0 { format!("{base} {}%", (a.clamp(0.0, 1.0) * 100.0).round()) } else { base }
}

/// A gradient asset's stops (`stop`, `color`) as a REST gradient paint, left to right (a radial
/// one from the centre).
fn gradient(content: &Value, radial: bool) -> Option<Value> {
    let stops: Vec<Value> = content
        .as_array()?
        .iter()
        .take(64)
        .filter_map(|s| {
            let pos = s.get("stop").and_then(Value::as_f64).filter(|v| v.is_finite())?.clamp(0.0, 1.0);
            Some(json!({"position": pos, "color": super::style::color(s.get("color")?)?}))
        })
        .collect();
    if stops.len() < 2 {
        return None;
    }
    let (ty, handles) = if radial {
        ("GRADIENT_RADIAL", json!([{"x": 0.5, "y": 0.5}, {"x": 1.0, "y": 0.5}, {"x": 0.5, "y": 1.0}]))
    } else {
        ("GRADIENT_LINEAR", json!([{"x": 0.0, "y": 0.5}, {"x": 1.0, "y": 0.5}, {"x": 0.0, "y": 1.0}]))
    };
    Some(json!({"type": ty, "gradientHandlePositions": handles, "gradientStops": stops}))
}

/// A character style as a REST type style, and a name for it from its font and size.
fn text_style(cx: &mut Cx, c: &Value) -> (Value, String) {
    let ps = str_field(c, "postscriptName").unwrap_or("");
    let (family, style) =
        super::text::resolve(cx, ps, str_field(c, "fontFamily").unwrap_or(""), str_field(c, "fontStyle").unwrap_or(""), &Default::default());
    let size = num(c, "fontSize").filter(|s| *s > 0.0).map_or(12.0, |s| s.min(10_000.0));
    let mut s =
        json!({"fontFamily": family, "fontStyle": style, "fontSize": size, "letterSpacing": num(c, "charSpacing").unwrap_or(0.0) * size / 1000.0});
    if let Some(m) = s.as_object_mut() {
        if let Some(lh) = num(c, "lineSpacing").filter(|v| *v > 0.0) {
            m.insert("lineHeightPx".into(), json!(lh.min(1e5)));
            m.insert("lineHeightUnit".into(), json!("PIXELS"));
        }
        if let Some(sp) = num(c, "paragraphSpacing").filter(|v| *v > 0.0) {
            m.insert("paragraphSpacing".into(), json!(sp.min(1e4)));
        }
        let deco = if c.get("underline").and_then(Value::as_bool) == Some(true) {
            Some("UNDERLINE")
        } else if c.get("strikethrough").and_then(Value::as_bool) == Some(true) {
            Some("STRIKETHROUGH")
        } else {
            None
        };
        if let Some(d) = deco {
            m.insert("textDecoration".into(), json!(d));
        }
        let case = match str_field(c, "textTransform") {
            Some("uppercase") => Some("UPPER"),
            Some("lowercase") => Some("LOWER"),
            Some("titlecase") => Some("TITLE"),
            _ => None,
        };
        if let Some(k) = case {
            m.insert("textCase".into(), json!(k));
        }
    }
    let label = format!("{family} {style} {}", trim_number(size));
    (s, label)
}

/// `14` for 14.0, `14.5` for 14.5.
fn trim_number(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The style a paint of `style` links to (`fill` or `stroke`): its colour's `linkId`, when that
/// names a paint style made from an asset.
pub(crate) fn paint_link(cx: &Cx, paint: Option<&Value>) -> Option<String> {
    let p = paint?;
    if str_field(p, "type") != Some("solid") {
        return None;
    }
    let id = p.get("color").and_then(|c| str_field(c, "linkId")).filter(|i| !i.is_empty())?;
    cx.styles.paint.contains(id).then(|| id.to_string())
}

/// The text style a text layer links to: the character style all its runs name.
pub(crate) fn text_link(cx: &Cx, l: &Value) -> Option<String> {
    let runs = l.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("rangedStyles")).and_then(Value::as_array)?;
    let mut ids = runs.iter().map(|r| str_field(r, "linkId").unwrap_or(""));
    let first = ids.next()?;
    (!first.is_empty() && ids.all(|i| i == first) && cx.styles.text.contains(first)).then(|| first.to_string())
}

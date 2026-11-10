//! Layer styles (`lmfx` / `lfx2` / `lfxs`): shadows and glows become effects; a colour overlay
//! and a stroke are handed to the layer to apply as its fill and stroke.

use figcraft_psd::LayerRecord;
use figcraft_psd::descriptor::{Descriptor, Value as D};
use serde_json::{Value, json};

use super::Shared;
use super::layers::FileCx;
use super::util::{self, desc, enumv, flag, num};

/// A stroke from the layer style.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Stroke {
    pub color: [f64; 3],
    pub opacity: f64,
    pub width: f64,
    /// `INSIDE`, `CENTER` or `OUTSIDE`.
    pub align: &'static str,
}

/// What the layer style asks for.
#[derive(Clone, Debug, Default)]
pub(crate) struct Fx {
    /// REST effects.
    pub effects: Vec<Value>,
    /// A colour overlay: colour and opacity.
    pub overlay: Option<([f64; 3], f64)>,
    pub stroke: Option<Stroke>,
}

/// The layer style of `rec`.
pub(crate) fn parse(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord) -> Fx {
    let mut out = Fx::default();
    let Some(block) = rec.block(b"lmfx").or_else(|| rec.block(b"lfx2")).or_else(|| rec.block(b"lfxs")) else { return out };
    let Some(d) = block.data.get(4..).and_then(util::versioned) else { return out };
    if flag(&d, "masterFXSwitch") == Some(false) {
        return out;
    }
    let scale = num(&d, "Scl ").map_or(1.0, |s| (s / 100.0).clamp(0.0, 10.0));
    for (single, multi, kind) in [
        ("DrSh", "dropShadowMulti", Kind::DropShadow),
        ("IrSh", "innerShadowMulti", Kind::InnerShadow),
        ("OrGl", "", Kind::OuterGlow),
        ("IrGl", "", Kind::InnerGlow),
        ("SoFi", "solidFillMulti", Kind::Overlay),
        ("FrFX", "frameFXMulti", Kind::Stroke),
        ("GrFl", "gradientFillMulti", Kind::Unsupported("gradient overlays")),
        ("ebbl", "", Kind::Unsupported("bevels and embosses")),
        ("ChFX", "", Kind::Unsupported("satin effects")),
        ("patternFill", "", Kind::Unsupported("pattern overlays")),
    ] {
        let mut all: Vec<&Descriptor> = desc(&d, single).into_iter().collect();
        all.extend(util::list(&d, multi).iter().filter_map(|v| match v {
            D::Descriptor(x) => Some(x),
            _ => None,
        }));
        // Photoshop draws the list top to bottom: the last effect is the bottom one.
        for e in all.into_iter().rev() {
            if flag(e, "enab") != Some(true) {
                continue;
            }
            effect(sh, fx, e, kind, scale, &mut out);
        }
    }
    out
}

#[derive(Clone, Copy)]
enum Kind {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    Overlay,
    Stroke,
    Unsupported(&'static str),
}

fn effect(sh: &mut Shared, fx: &FileCx, e: &Descriptor, kind: Kind, scale: f64, out: &mut Fx) {
    let opacity = num(e, "Opct").map_or(1.0, |o| (o / 100.0).clamp(0.0, 1.0));
    let color = desc(e, "Clr ").and_then(util::color);
    let mode = util::blend_key(&enumv(e, "Md  ").unwrap_or_default());
    match kind {
        Kind::DropShadow | Kind::InnerShadow | Kind::OuterGlow | Kind::InnerGlow => {
            let Some(c) = color else {
                sh.note("glows and shadows with gradient or non-RGB colours were left out");
                return;
            };
            let size = num(e, "blur").unwrap_or(0.0).clamp(0.0, 1000.0) * scale;
            // Spread (or choke) is the share of the size that grows the shape before the blur.
            let spread = num(e, "Ckmt").unwrap_or(0.0).clamp(0.0, 100.0) / 100.0;
            let (dx, dy) = match kind {
                Kind::DropShadow | Kind::InnerShadow => {
                    let global = flag(e, "uglg").unwrap_or(true);
                    let angle = if global { fx.angle } else { num(e, "lagl").unwrap_or(fx.angle) }.to_radians();
                    let dist = num(e, "Dstn").unwrap_or(0.0).clamp(0.0, 30_000.0) * scale;
                    (-dist * angle.cos(), dist * angle.sin())
                }
                _ => (0.0, 0.0),
            };
            let ty = if matches!(kind, Kind::DropShadow | Kind::OuterGlow) { "DROP_SHADOW" } else { "INNER_SHADOW" };
            if matches!(kind, Kind::OuterGlow | Kind::InnerGlow) {
                sh.note("glows became shadows without an offset");
            }
            out.effects.push(json!({
                "type": ty,
                "visible": true,
                "color": util::rgba(c, opacity),
                "offset": {"x": dx, "y": dy},
                "radius": size * (1.0 - spread),
                "spread": size * spread,
                "blendMode": mode,
                "showShadowBehindNode": false,
            }));
        }
        Kind::Overlay => match color {
            Some(c) if out.overlay.is_none() => out.overlay = Some((c, opacity)),
            Some(_) => sh.note("extra colour overlays were left out"),
            None => sh.note("colour overlays in non-RGB colours were left out"),
        },
        Kind::Stroke => {
            let solid = enumv(e, "PntT").is_none_or(|p| p == "SClr");
            match color.filter(|_| solid) {
                Some(c) if out.stroke.is_none() => {
                    let align = match enumv(e, "Styl").as_deref() {
                        Some("InsF") => "INSIDE",
                        Some("CtrF") => "CENTER",
                        _ => "OUTSIDE",
                    };
                    let width = num(e, "Sz  ").unwrap_or(1.0).clamp(0.0, 1000.0) * scale;
                    out.stroke = Some(Stroke { color: c, opacity, width, align });
                }
                Some(_) => sh.note("extra strokes in layer styles were left out"),
                None => sh.note("gradient and pattern strokes in layer styles were left out"),
            }
        }
        Kind::Unsupported(what) => sh.note(match what {
            "gradient overlays" => "gradient overlays were left out",
            "bevels and embosses" => "bevels and embosses were left out",
            "satin effects" => "satin effects were left out",
            _ => "pattern overlays were left out",
        }),
    }
}

/// The style's stroke as REST `strokes` and stroke fields on `node`.
pub(crate) fn apply_stroke(node: &mut serde_json::Map<String, Value>, s: &Stroke) {
    node.insert("strokes".into(), json!([{"type": "SOLID", "color": util::rgba(s.color, 1.0), "opacity": s.opacity}]));
    node.insert("strokeWeight".into(), json!(s.width));
    node.insert("strokeAlign".into(), json!(s.align));
}

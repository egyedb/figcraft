//! XD styles → REST paints, strokes and effects.
//!
//! A layer's `style` holds its `fill` (one paint: `solid`, `pattern` for an image, `gradient` or
//! `none`), its `stroke` (paint, `width`, `align`, `cap`, `join`, `dash`), its `filters`
//! (`dropShadow`, `uxdesign#innerShadow`, `uxdesign#blur`), `opacity` and `blendMode`.

use serde_json::{Map as JsonMap, Value, json};

use super::tree::Cx;

/// Largest stroke width, shadow offset or blur kept.
const MAX_LEN: f64 = 1e5;

/// An XD colour as a REST colour: `{"mode": "RGB", "value": {"r", "g", "b"}, "alpha"}` with
/// channels in 0–255, or a 32-bit ARGB number (text runs: `{"value": 4284246910}`).
pub(crate) fn color(c: &Value) -> Option<Value> {
    let v = c.get("value")?;
    if let Some(argb) = v.as_u64() {
        let argb = u32::try_from(argb).ok()?;
        let ch = |shift: u32| f64::from((argb >> shift) & 0xff) / 255.0;
        return Some(json!({"r": ch(16), "g": ch(8), "b": ch(0), "a": ch(24)}));
    }
    let n = |k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite()).map(|x| (x / 255.0).clamp(0.0, 1.0));
    let a = c.get("alpha").and_then(Value::as_f64).filter(|x| x.is_finite()).map_or(1.0, |x| x.clamp(0.0, 1.0));
    Some(json!({"r": n("r")?, "g": n("g")?, "b": n("b")?, "a": a}))
}

/// Fill paint `f` as REST paints (empty for none or what isn't understood). `w`, `h`: the box the
/// paint fills, for gradients given in the layer's own units.
pub(crate) fn fills(cx: &mut Cx, f: Option<&Value>, w: f64, h: f64) -> Vec<Value> {
    let Some(f) = f else { return Vec::new() };
    match f.get("type").and_then(Value::as_str) {
        Some("solid") => f.get("color").and_then(color).map(|c| vec![json!({"type": "SOLID", "color": c})]).unwrap_or_default(),
        Some("pattern") => image(cx, f.get("pattern"), w, h).into_iter().collect(),
        Some("gradient") => gradient(cx, f.get("gradient"), w, h).into_iter().collect(),
        Some("none") | None => Vec::new(),
        Some(other) => {
            cx.warn(format!("{} fills are not supported yet and were skipped", clip(other)));
            Vec::new()
        }
    }
}

/// An image fill (`pattern`): the image `meta.ux.uid` names in `resources/`.
fn image(cx: &mut Cx, p: Option<&Value>, w: f64, h: f64) -> Option<Value> {
    let p = p?;
    let ux = p.get("meta").and_then(|m| m.get("ux"));
    let Some(uid) = ux.and_then(|u| u.get("uid")).and_then(Value::as_str) else {
        cx.warn("image fills without an image in the file were skipped");
        return None;
    };
    let filters = p.get("uxdesign#colorTransform").and_then(|t| t.get("params")).and_then(|params| color_filters(cx, params));
    // Both kinds XD writes keep the image's proportions and cover the shape.
    let mode = match ux.and_then(|u| u.get("scaleBehavior")).and_then(Value::as_str) {
        Some("fit" | "contain") => "FIT",
        Some("stretch") => "STRETCH",
        _ => "FILL",
    };
    let mut paint = match ux.and_then(|u| placed(p, u, w, h)) {
        Some(t) => json!({"type": "IMAGE", "scaleMode": "STRETCH", "imageRef": uid, "imageTransform": t}),
        None => json!({"type": "IMAGE", "scaleMode": mode, "imageRef": uid}),
    };
    if let Some(f) = filters {
        paint["filters"] = f;
    }
    Some(paint)
}

/// An image fill's colour adjustments (`h` hue, `s` saturation, `b` brightness, `c` contrast,
/// each 0 when untouched) as REST image filters, `None` when untouched. XD scales the colours
/// about mid-grey by the contrast and adds the brightness, `(v - 0.5)·(1 + c) + 0.5 + b`;
/// FigCraft multiplies by the exposure `E` first, `(v·E - 0.5)·k + 0.5`, which agrees for
/// `k = (1 + c) - 2b` and `E = (1 + c) / k` within its ranges. A brightness past 1 is a
/// percentage. Hue shifts have no filter and are left out with a note.
fn color_filters(cx: &mut Cx, params: &Value) -> Option<Value> {
    let n = |k: &str| params.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
    let (h, s, c) = (n("h"), n("s"), n("c"));
    let b = n("b");
    if [h, s, b, c].iter().all(|v| v.abs() < 1e-6) {
        return None;
    }
    if h.abs() >= 1e-6 {
        cx.warn("hue shifts of image fills were left out (FigCraft's image filters have none)");
    }
    let b = if b.abs() > 1.0 { b / 100.0 } else { b }.clamp(-1.0, 1.0);
    let contrast = (1.0 + c).clamp(0.0, 2.0);
    let k = (contrast - 2.0 * b).clamp(1e-6, 2.0);
    let e = (contrast / k).clamp(0.25, 4.0);
    let k = (contrast / e).clamp(0.0, 2.0);
    Some(json!({"saturation": s.clamp(-1.0, 1.0), "contrast": (k - 1.0).clamp(-1.0, 1.0), "exposure": (e.log2() / 2.0).clamp(-1.0, 1.0)}))
}

/// The image transform of a covering image moved or zoomed in its shape (`scale`, `offsetX`,
/// `offsetY` in `meta.ux`), or `None` when it isn't. XD draws the image at its covering size
/// times `scale`, centred, then moved by the offsets in units of its covering size. The REST
/// transform maps the shape's unit box into the image's.
fn placed(p: &Value, ux: &Value, w: f64, h: f64) -> Option<Value> {
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_f64).filter(|x| x.is_finite());
    if n(ux, "scale").is_none() && n(ux, "offsetX").is_none() && n(ux, "offsetY").is_none() {
        return None;
    }
    // The image's own size in pixels.
    let (pw, ph) = (n(p, "width").filter(|v| *v > 0.0)?, n(p, "height").filter(|v| *v > 0.0)?);
    if !(w > 0.0 && h > 0.0) {
        return None;
    }
    let cover = (w / pw).max(h / ph);
    let scale = n(ux, "scale").filter(|s| *s > 0.0).unwrap_or(1.0).clamp(0.01, 100.0);
    let (cw, ch) = (pw * cover, ph * cover);
    let (dw, dh) = (cw * scale, ch * scale);
    let x0 = (w - dw) / 2.0 + n(ux, "offsetX").unwrap_or(0.0).clamp(-100.0, 100.0) * cw;
    let y0 = (h - dh) / 2.0 + n(ux, "offsetY").unwrap_or(0.0).clamp(-100.0, 100.0) * ch;
    let t = [w / dw, h / dh, -x0 / dw, -y0 / dh];
    t.iter().all(|v| v.is_finite()).then(|| json!([[t[0], 0.0, t[2]], [0.0, t[1], t[3]]]))
}

/// A gradient fill: linear (`x1`…`y2`) or radial (`cx`, `cy`, `r`), its stops in
/// `meta.ux.gradientResources`. Points are shares of the box (`objectBoundingBox`) or the layer's
/// own units (`userSpaceOnUse`).
fn gradient(cx: &mut Cx, g: Option<&Value>, w: f64, h: f64) -> Option<Value> {
    let g = g?;
    let res = g.get("meta").and_then(|m| m.get("ux")).and_then(|u| u.get("gradientResources"));
    let stops: Vec<Value> = res
        .and_then(|r| r.get("stops"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(64)
        .filter_map(|s| {
            let pos = s.get("offset").and_then(Value::as_f64).filter(|o| o.is_finite())?.clamp(0.0, 1.0);
            Some(json!({"position": pos, "color": color(s.get("color")?)?}))
        })
        .collect();
    if stops.len() < 2 {
        cx.warn("gradients without stops were skipped");
        return None;
    }
    let user = g.get("units").and_then(Value::as_str) == Some("userSpaceOnUse");
    let scale = |x: f64, y: f64| -> (f64, f64) { if user { (if w > 0.0 { x / w } else { 0.0 }, if h > 0.0 { y / h } else { 0.0 }) } else { (x, y) } };
    let n = |k: &str| g.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).map(|v| v.clamp(-1e6, 1e6));
    let kind = res.and_then(|r| r.get("type")).and_then(Value::as_str).unwrap_or("linear");
    let handles = if kind == "radial" {
        // The circle's centre and two radii, through the gradient's own transform (as SVG's
        // `gradientTransform`: a stretched, turned or mirrored circle), in the gradient's units.
        let (c, r) = ((n("cx").unwrap_or(0.5), n("cy").unwrap_or(0.5)), n("r").unwrap_or(0.5));
        let t = g.get("transform").and_then(|t| {
            let k = |key: &str| t.get(key).and_then(Value::as_f64).filter(|v| v.is_finite() && v.abs() <= 1e6);
            Some([k("a")?, k("b")?, k("c")?, k("d")?, k("tx")?, k("ty")?])
        });
        let [a, b, c2, d, e, f] = t.unwrap_or([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        let through = |x: f64, y: f64| scale(a * x + c2 * y + e, b * x + d * y + f);
        vec![through(c.0, c.1), through(c.0 + r, c.1), through(c.0, c.1 + r)]
    } else {
        let a = scale(n("x1").unwrap_or(0.0), n("y1").unwrap_or(0.0));
        let b = scale(n("x2").unwrap_or(1.0), n("y2").unwrap_or(0.0));
        // The third handle sets the width: perpendicular to the line, half as long.
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        vec![a, b, (a.0 - dy / 2.0, a.1 + dx / 2.0)]
    };
    let ty = if kind == "radial" { "GRADIENT_RADIAL" } else { "GRADIENT_LINEAR" };
    let handles: Vec<Value> = handles.into_iter().map(|(x, y)| json!({"x": x, "y": y})).collect();
    Some(json!({"type": ty, "gradientHandlePositions": handles, "gradientStops": stops}))
}

/// Layer style `st` into `node`: fills, stroke, effects, opacity and blend mode. `w`, `h`: the
/// layer's box. `text`: a text layer, whose fill is its text colour and black when missing.
pub(crate) fn apply(cx: &mut Cx, st: Option<&Value>, w: f64, h: f64, text: bool, node: &mut JsonMap<String, Value>) {
    let fill = st.and_then(|s| s.get("fill"));
    let mut paints = fills(cx, fill, w, h);
    if text && fill.is_none() {
        paints.push(json!({"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}));
    }
    // Behind a background blur XD shows the layer's fill at the blur's fill opacity (often none:
    // frosted glass).
    if let Some(o) = st.and_then(backdrop_fill_opacity) {
        for p in &mut paints {
            let own = p.get("opacity").and_then(Value::as_f64).unwrap_or(1.0);
            p["opacity"] = json!((own * o).clamp(0.0, 1.0));
        }
    }
    let single = paints.len() == 1;
    node.insert("fills".into(), Value::Array(paints));
    if single && let Some(id) = super::assets::paint_link(cx, fill) {
        link(node, "fill", id);
    }
    let Some(st) = st else { return };
    stroke(cx, st.get("stroke"), w, h, node);
    if node.contains_key("strokes")
        && let Some(id) = super::assets::paint_link(cx, st.get("stroke"))
    {
        link(node, "stroke", id);
    }
    let effects = effects(cx, st.get("filters"));
    if !effects.is_empty() {
        node.insert("effects".into(), Value::Array(effects));
    }
    if let Some(o) = st.get("opacity").and_then(Value::as_f64).filter(|o| o.is_finite()) {
        node.insert("opacity".into(), json!(o.clamp(0.0, 1.0)));
    }
    if let Some(b) = st.get("blendMode").and_then(Value::as_str) {
        match blend(b) {
            Some(m) => {
                node.insert("blendMode".into(), json!(m));
            }
            None => cx.warn(format!("the {} blend mode is not supported yet; those layers blend normally", clip(b))),
        }
    }
}

/// Link `node`'s `slot` (`fill`, `stroke`, `text`) to style `id`.
pub(crate) fn link(node: &mut JsonMap<String, Value>, slot: &str, id: String) {
    let styles = node.entry("styles").or_insert_with(|| json!({}));
    if let Some(m) = styles.as_object_mut() {
        m.insert(slot.into(), json!(id));
    }
}

/// The fill opacity a visible background blur in style `st` gives the layer's fills, when it isn't 1.
fn backdrop_fill_opacity(st: &Value) -> Option<f64> {
    let filters = st.get("filters").and_then(Value::as_array)?;
    filters
        .iter()
        .filter(|f| f.get("visible").and_then(Value::as_bool) != Some(false) && f.get("type").and_then(Value::as_str) == Some("uxdesign#blur"))
        .filter_map(|f| f.get("params"))
        .filter(|p| p.get("backgroundEffect").and_then(Value::as_bool) == Some(true))
        .find_map(|p| p.get("fillOpacity").and_then(Value::as_f64))
        .filter(|o| o.is_finite() && *o < 1.0)
        .map(|o| o.clamp(0.0, 1.0))
}

/// An XD (CSS) blend mode as a REST one.
fn blend(b: &str) -> Option<&'static str> {
    Some(match b {
        "normal" | "pass-through" => "NORMAL",
        "multiply" => "MULTIPLY",
        "screen" => "SCREEN",
        "overlay" => "OVERLAY",
        "darken" => "DARKEN",
        "lighten" => "LIGHTEN",
        "color-dodge" => "COLOR_DODGE",
        "color-burn" => "COLOR_BURN",
        "hard-light" => "HARD_LIGHT",
        "soft-light" => "SOFT_LIGHT",
        "difference" => "DIFFERENCE",
        "exclusion" => "EXCLUSION",
        "hue" => "HUE",
        "saturation" => "SATURATION",
        "color" => "COLOR",
        "luminosity" => "LUMINOSITY",
        _ => return None,
    })
}

/// A stroke: its paint, width, alignment (`inside`, `outside`, else centred), caps, joins and dashes.
fn stroke(cx: &mut Cx, s: Option<&Value>, w: f64, h: f64, node: &mut JsonMap<String, Value>) {
    let Some(s) = s else { return };
    let paints = fills(cx, Some(s), w, h);
    let width = s.get("width").and_then(Value::as_f64).filter(|v| v.is_finite()).map_or(1.0, |v| v.clamp(0.0, MAX_LEN));
    if paints.is_empty() || width <= 0.0 {
        return;
    }
    node.insert("strokes".into(), Value::Array(paints));
    node.insert("strokeWeight".into(), json!(width));
    let align = match s.get("align").and_then(Value::as_str) {
        Some("inside") => "INSIDE",
        Some("outside") => "OUTSIDE",
        _ => "CENTER",
    };
    node.insert("strokeAlign".into(), json!(align));
    if let Some(cap) = s.get("cap").and_then(Value::as_str) {
        node.insert(
            "strokeCap".into(),
            json!(if cap == "round" {
                "ROUND"
            } else if cap == "square" {
                "SQUARE"
            } else {
                "NONE"
            }),
        );
    }
    if let Some(join) = s.get("join").and_then(Value::as_str) {
        node.insert(
            "strokeJoin".into(),
            json!(if join == "round" {
                "ROUND"
            } else if join == "bevel" {
                "BEVEL"
            } else {
                "MITER"
            }),
        );
    }
    let dashes: Vec<f64> = s
        .get("dash")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(64)
        .filter_map(Value::as_f64)
        .filter(|d| d.is_finite())
        .map(|d| d.clamp(0.0, MAX_LEN))
        .collect();
    if dashes.iter().any(|d| *d > 0.0) {
        // One value means dashes and gaps of that length, as in SVG.
        let dashes = if dashes.len() % 2 == 1 { dashes.repeat(2) } else { dashes };
        node.insert("strokeDashes".into(), json!(dashes));
    }
}

/// Filters as REST effects; hidden ones (`visible: false`) are left out.
fn effects(cx: &mut Cx, filters: Option<&Value>) -> Vec<Value> {
    let mut out = Vec::new();
    for f in filters.and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default().iter().take(64) {
        if f.get("visible").and_then(Value::as_bool) == Some(false) {
            continue;
        }
        let params = f.get("params");
        match f.get("type").and_then(Value::as_str) {
            Some("dropShadow") => out.extend(shadows(params.and_then(|p| p.get("dropShadows")), "DROP_SHADOW")),
            Some("uxdesign#innerShadow") => out.extend(shadows(params.and_then(|p| p.get("innerShadows")), "INNER_SHADOW")),
            Some("uxdesign#blur") => {
                let amount = params.and_then(|p| p.get("blurAmount")).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
                let background = params.and_then(|p| p.get("backgroundEffect")).and_then(Value::as_bool) == Some(true);
                let ty = if background { "BACKGROUND_BLUR" } else { "LAYER_BLUR" };
                let mut effect = json!({"type": ty, "visible": true, "radius": amount.clamp(0.0, MAX_LEN)});
                // XD's brightness of the blurred backdrop, -100 to 100 (its fill opacity goes on the
                // fills, see `apply`).
                let brightness = params.and_then(|p| p.get("brightnessAmount")).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0);
                if background && brightness != 0.0 {
                    effect["brightness"] = json!((brightness / 100.0).clamp(-1.0, 1.0));
                }
                out.push(effect);
            }
            Some(other) => cx.warn(format!("{} effects are not supported yet and were skipped", clip(other))),
            None => {}
        }
    }
    out
}

/// A list of XD shadows (`dx`, `dy`, `r` the blur, `color`) as REST shadows of type `ty`.
fn shadows(list: Option<&Value>, ty: &str) -> Vec<Value> {
    list.and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .take(64)
        .filter(|s| s.get("visible").and_then(Value::as_bool) != Some(false))
        .filter_map(|s| {
            let n = |k: &str| s.get(k).and_then(Value::as_f64).filter(|v| v.is_finite()).unwrap_or(0.0).clamp(-MAX_LEN, MAX_LEN);
            let c = s.get("color").and_then(color)?;
            Some(json!({"type": ty, "visible": true, "color": c, "offset": {"x": n("dx"), "y": n("dy")}, "radius": n("r").max(0.0), "spread": 0.0}))
        })
        .collect()
}

/// At most 64 characters of a name read from the file, for warnings.
pub(crate) fn clip(s: &str) -> String {
    s.chars().take(64).collect()
}

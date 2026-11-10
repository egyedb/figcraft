//! Gradients and patterns: gradient overlays (`GrFl`) and gradient fill layers (`GdFl`) become
//! gradient paints, pattern overlays (`patternFill`) and pattern fill layers (`PtFl`) tiled
//! image paints. On pixel layers they are painted into the pixels instead.
//!
//! A gradient (`Grad`) lists colour stops (`Clrs`: `Clr `, `Lctn` 0..4096, `Mdpn` the midpoint
//! to the next stop in percent) and opacity stops (`Trns`: `Opct`, `Lctn`, `Mdpn`). Its `Type`
//! is linear, radial, angle, reflected or diamond; `Angl` turns it (degrees, counter-clockwise);
//! `Scl ` scales it; `Ofst` moves its centre (percent of the box); `Rvrs` reverses it; `Algn`
//! lays it over the layer's bounds instead of the canvas. A linear gradient at 100% spans the
//! box along its angle; the others reach the box's corners.

use std::rc::Rc;

use figcraft_doc::geom::{Point, Rect, Vec2};
use figcraft_psd::descriptor::{Descriptor, Value as D};
use figcraft_psd::patterns::PsdPattern;
use serde_json::{Value, json};

use super::Shared;
use super::layers::FileCx;
use super::util::{self, desc, enumv, flag, num, text};

/// Most stops kept from one gradient.
const MAX_STOPS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Linear,
    Radial,
    Angle,
    Reflected,
    Diamond,
}

/// A gradient with its placement.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Grad {
    pub kind: Kind,
    /// Degrees, counter-clockwise.
    pub angle: f64,
    pub scale: f64,
    /// Over the layer's bounds (else the canvas).
    pub align: bool,
    /// The centre's offset in shares of the box.
    pub offset: (f64, f64),
    /// Position 0..1 and RGBA, in order.
    pub stops: Vec<(f64, [f64; 4])>,
    /// The overlay's or fill's opacity.
    pub opacity: f64,
}

/// A pattern overlay or fill.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pattern {
    pub id: String,
    pub name: String,
    pub scale: f64,
    pub align: bool,
    pub phase: (f64, f64),
    pub opacity: f64,
}

/// A gradient from an overlay or a fill layer's descriptor (`None` for noise gradients).
pub(crate) fn gradient(d: &Descriptor, opacity: f64) -> Option<Grad> {
    let g = desc(d, "Grad")?;
    if enumv(g, "GrdF").as_deref() == Some("ClNs") {
        return None;
    }
    let kind = match enumv(d, "Type").as_deref().map(str::trim) {
        Some("Rdl") => Kind::Radial,
        Some("Angl") => Kind::Angle,
        Some("Rflc") => Kind::Reflected,
        Some("Dmnd") => Kind::Diamond,
        _ => Kind::Linear,
    };
    let entries = |key: &str| -> Vec<(f64, f64, &Descriptor)> {
        let mut v: Vec<(f64, f64, &Descriptor)> = util::list(g, key)
            .iter()
            .filter_map(|s| match s {
                D::Descriptor(s) => {
                    Some(((num(s, "Lctn").unwrap_or(0.0) / 4096.0).clamp(0.0, 1.0), (num(s, "Mdpn").unwrap_or(50.0) / 100.0).clamp(0.05, 0.95), s))
                }
                _ => None,
            })
            .take(MAX_STOPS)
            .collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v
    };
    let colors: Vec<(f64, f64, [f64; 3])> =
        entries("Clrs").into_iter().map(|(p, m, s)| (p, m, desc(s, "Clr ").and_then(util::color).unwrap_or([0.0; 3]))).collect();
    let alphas: Vec<(f64, f64, f64)> =
        entries("Trns").into_iter().map(|(p, m, s)| (p, m, num(s, "Opct").map_or(1.0, |o| (o / 100.0).clamp(0.0, 1.0)))).collect();
    if colors.is_empty() {
        return None;
    }
    // Every position a colour, opacity or midpoint falls on, so the paint matches between them.
    let mut at: Vec<f64> = colors.iter().map(|c| c.0).chain(alphas.iter().map(|a| a.0)).collect();
    for w in colors.windows(2) {
        if let [a, b] = w {
            at.push(a.0 + (b.0 - a.0) * a.1);
        }
    }
    for w in alphas.windows(2) {
        if let [a, b] = w {
            at.push(a.0 + (b.0 - a.0) * a.1);
        }
    }
    at.extend([0.0, 1.0]);
    at.sort_by(f64::total_cmp);
    at.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
    let reverse = flag(d, "Rvrs").unwrap_or(false);
    let mut stops: Vec<(f64, [f64; 4])> = at
        .into_iter()
        .take(MAX_STOPS)
        .map(|p| {
            let c = sample(&colors.iter().map(|c| (c.0, c.1, c.2)).collect::<Vec<_>>(), p);
            let a = if alphas.is_empty() { 1.0 } else { sample(&alphas.iter().map(|a| (a.0, a.1, [a.2; 3])).collect::<Vec<_>>(), p)[0] };
            (if reverse { 1.0 - p } else { p }, [c[0], c[1], c[2], a])
        })
        .collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));
    let offset = desc(d, "Ofst").map_or((0.0, 0.0), |o| (num(o, "Hrzn").unwrap_or(0.0) / 100.0, num(o, "Vrtc").unwrap_or(0.0) / 100.0));
    Some(Grad {
        kind,
        angle: num(d, "Angl").unwrap_or(90.0).clamp(-3600.0, 3600.0),
        scale: num(d, "Scl ").map_or(1.0, |s| (s / 100.0).clamp(0.01, 100.0)),
        align: flag(d, "Algn").unwrap_or(true),
        offset: (offset.0.clamp(-10.0, 10.0), offset.1.clamp(-10.0, 10.0)),
        stops,
        opacity: opacity.clamp(0.0, 1.0),
    })
}

/// The value at `p` of stops `(position, midpoint, value)`: the midpoint is where the mix is half
/// way, as a share of the way to the next stop.
fn sample(stops: &[(f64, f64, [f64; 3])], p: f64) -> [f64; 3] {
    let Some(first) = stops.first() else { return [0.0; 3] };
    if p <= first.0 {
        return first.2;
    }
    for w in stops.windows(2) {
        if let [a, b] = w
            && p <= b.0
        {
            let span = (b.0 - a.0).max(1e-9);
            let t = ((p - a.0) / span).clamp(0.0, 1.0);
            // Bend t so that the midpoint lands on 0.5.
            let m = a.1.clamp(0.05, 0.95);
            let t = if t < m { 0.5 * t / m } else { 0.5 + 0.5 * (t - m) / (1.0 - m) };
            return [0, 1, 2].map(|i| a.2[i] + (b.2[i] - a.2[i]) * t);
        }
    }
    stops.last().map_or([0.0; 3], |s| s.2)
}

/// A pattern from an overlay or a fill layer's descriptor.
pub(crate) fn pattern(d: &Descriptor, opacity: f64) -> Option<Pattern> {
    let p = desc(d, "Ptrn")?;
    let phase = desc(d, "phase").map_or((0.0, 0.0), |ph| (num(ph, "Hrzn").unwrap_or(0.0), num(ph, "Vrtc").unwrap_or(0.0)));
    Some(Pattern {
        id: text(p, "Idnt")?,
        name: text(p, "Nm  ").unwrap_or_default(),
        scale: num(d, "Scl ").map_or(1.0, |s| (s / 100.0).clamp(0.01, 100.0)),
        align: flag(d, "Algn").unwrap_or(true),
        phase: (phase.0.clamp(-1e6, 1e6), phase.1.clamp(-1e6, 1e6)),
        opacity: opacity.clamp(0.0, 1.0),
    })
}

/// The box a gradient is laid over: the layer's bounds or the canvas.
pub(crate) fn reference(g: &Grad, layer: Rect, canvas: Rect) -> Rect {
    if g.align && layer.width() > 0.0 && layer.height() > 0.0 { layer } else { canvas }
}

/// The gradient's centre, direction and reach over box `r`.
fn geometry(g: &Grad, r: Rect) -> (Point, Vec2, f64) {
    let a = g.angle.to_radians();
    let dir = Vec2::new(a.cos(), -a.sin());
    let c = r.center() + Vec2::new(g.offset.0 * r.width(), g.offset.1 * r.height());
    let reach = match g.kind {
        // Half the box's extent along the angle.
        Kind::Linear | Kind::Reflected => (r.width() * a.cos().abs() + r.height() * a.sin().abs()) / 2.0,
        _ => r.width().hypot(r.height()) / 2.0,
    };
    (c, dir, (reach * g.scale).max(1e-6))
}

/// The gradient laid over box `r` as a REST paint of a node whose box is `node`.
pub(crate) fn paint(g: &Grad, r: Rect, node: Rect) -> Value {
    let (c, dir, reach) = geometry(g, r);
    let perp = Vec2::new(-dir.y, dir.x);
    let (ty, from, to, width, stops) = match g.kind {
        Kind::Linear => ("GRADIENT_LINEAR", c - dir * reach, c + dir * reach, c - dir * reach + perp * reach * 2.0, g.stops.clone()),
        Kind::Reflected => {
            // Mirrored about the centre, as one linear gradient across both sides.
            let mut s: Vec<(f64, [f64; 4])> = g.stops.iter().map(|(p, col)| (0.5 - p / 2.0, *col)).collect();
            s.extend(g.stops.iter().map(|(p, col)| (0.5 + p / 2.0, *col)));
            s.sort_by(|a, b| a.0.total_cmp(&b.0));
            ("GRADIENT_LINEAR", c - dir * reach, c + dir * reach, c - dir * reach + perp * reach * 2.0, s)
        }
        Kind::Radial => ("GRADIENT_RADIAL", c, c + dir * reach, c + perp * reach, g.stops.clone()),
        Kind::Angle => ("GRADIENT_ANGULAR", c, c + dir * reach, c + perp * reach, g.stops.clone()),
        Kind::Diamond => ("GRADIENT_DIAMOND", c, c + dir * reach, c + perp * reach, g.stops.clone()),
    };
    let unit = |p: Point| {
        let (w, h) = (node.width().max(1e-6), node.height().max(1e-6));
        json!({"x": (p.x - node.x0) / w, "y": (p.y - node.y0) / h})
    };
    json!({
        "type": ty,
        "gradientHandlePositions": [unit(from), unit(to), unit(width)],
        "gradientStops": stops.iter().map(|(p, c)| json!({"position": p, "color": {"r": c[0], "g": c[1], "b": c[2], "a": c[3]}})).collect::<Vec<_>>(),
        "opacity": g.opacity,
    })
}

/// The gradient's colour at point `p` over box `r`.
pub(crate) fn color_at(g: &Grad, r: Rect, p: Point) -> [f64; 4] {
    let (c, dir, reach) = geometry(g, r);
    let d = p - c;
    let along = d.dot(dir);
    let across = d.dot(Vec2::new(-dir.y, dir.x));
    let t = match g.kind {
        Kind::Linear => (along / reach + 1.0) / 2.0,
        Kind::Reflected => along.abs() / reach,
        Kind::Radial => d.hypot() / reach,
        Kind::Diamond => (along.abs() + across.abs()) / reach,
        Kind::Angle => {
            let turn = (-across).atan2(along) / std::f64::consts::TAU;
            turn.rem_euclid(1.0)
        }
    }
    .clamp(0.0, 1.0);
    let Some(first) = g.stops.first() else { return [0.0; 4] };
    if t <= first.0 {
        return first.1;
    }
    for w in g.stops.windows(2) {
        if let [a, b] = w
            && t <= b.0
        {
            let k = ((t - a.0) / (b.0 - a.0).max(1e-9)).clamp(0.0, 1.0);
            return [0, 1, 2, 3].map(|i| a.1[i] + (b.1[i] - a.1[i]) * k);
        }
    }
    g.stops.last().map_or([0.0; 4], |s| s.1)
}

/// A decoded pattern tile: its size and RGBA pixels.
pub(crate) struct Tile {
    pub w: u32,
    pub h: u32,
    pub rgba: Vec<u8>,
}

/// Pattern `id` of the file, decoded (`None` when the file doesn't have it).
pub(crate) fn tile(sh: &mut Shared, fx: &FileCx, id: &str) -> Option<Rc<Tile>> {
    let key = (fx.id, id.to_string());
    if let Some(t) = sh.tiles.get(&key) {
        return t.clone();
    }
    if !sh.tiles_read.contains(&fx.id) {
        sh.tiles_read.insert(fx.id);
        let mut budget = figcraft_psd::patterns::PatternBudget::default();
        for b in fx.file.global_blocks.iter().filter(|b| matches!(&b.key, b"Patt" | b"Pat2" | b"Pat3")) {
            for p in figcraft_psd::patterns::parse_pattern_block_with(&b.data, &mut budget).unwrap_or_default() {
                let t = to_rgba(&p).map(Rc::new);
                sh.tiles.insert((fx.id, p.id.clone()), t);
            }
        }
    }
    let found = sh.tiles.get(&key).cloned().flatten();
    if found.is_none() {
        sh.tiles.insert(key, None);
    }
    found
}

/// A pattern's planes as RGBA (gray, indexed, RGB and CMYK at 8 or 16 bits).
fn to_rgba(p: &PsdPattern) -> Option<Tile> {
    let n = (p.width as usize).checked_mul(p.height as usize)?;
    if n == 0 || n > 64 << 20 {
        return None;
    }
    let bytes = match p.depth {
        8 => 1,
        16 => 2,
        _ => return None,
    };
    let sample = |plane: &[u8], i: usize| plane.get(i * bytes).copied().unwrap_or(0);
    let plane = |c: usize| p.channels.get(c).map(Vec::as_slice);
    let mut rgba = vec![0u8; n * 4];
    for i in 0..n {
        let (r, g, b) = match p.mode {
            1 | 8 => {
                let v = sample(plane(0)?, i);
                (v, v, v)
            }
            2 => {
                let k = usize::from(sample(plane(0)?, i));
                let pal = p.palette.as_deref()?;
                (pal.get(k).copied().unwrap_or(0), pal.get(256 + k).copied().unwrap_or(0), pal.get(512 + k).copied().unwrap_or(0))
            }
            3 => (sample(plane(0)?, i), sample(plane(1)?, i), sample(plane(2)?, i)),
            4 => {
                // Stored inverted: 255 is no ink.
                let k = f64::from(sample(plane(3)?, i)) / 255.0;
                let ch = |c: usize| plane(c).map_or(0.0, |pl| f64::from(sample(pl, i)) / 255.0);
                let f = |v: f64| (v * k * 255.0).round() as u8;
                (f(ch(0)), f(ch(1)), f(ch(2)))
            }
            _ => return None,
        };
        let a = p.alpha.as_deref().map_or(255, |al| sample(al, i));
        if let Some(px) = rgba.get_mut(i * 4..i * 4 + 4) {
            px.copy_from_slice(&[r, g, b, a]);
        }
    }
    Some(Tile { w: p.width, h: p.height, rgba })
}

/// The pattern as a REST tiled image paint (`None` when the file doesn't have it).
pub(crate) fn pattern_paint(sh: &mut Shared, fx: &FileCx, p: &Pattern) -> Option<Value> {
    let t = tile(sh, fx, &p.id)?;
    let key = format!("{}:pattern:{}", fx.id, p.id);
    let r = super::layers::cached_image(sh, key, || util::png(t.w, t.h, t.rgba.clone()))?;
    Some(json!({"type": "IMAGE", "scaleMode": "TILE", "scalingFactor": p.scale, "imageRef": r, "opacity": p.opacity}))
}

/// Paint `p` over pixels `data` (`w` wide, top left at `origin` in document pixels), keeping
/// their alpha; `layer` is the layer's bounds (where a pattern linked to the layer starts).
pub(crate) fn bake_pattern(t: &Tile, p: &Pattern, data: &mut [u8], w: u32, origin: (f64, f64), layer: Rect) -> bool {
    if t.w == 0 || t.h == 0 || w == 0 {
        return false;
    }
    let (ox, oy) = if p.align { (layer.x0 + p.phase.0, layer.y0 + p.phase.1) } else { (p.phase.0, p.phase.1) };
    let (tw, th) = (f64::from(t.w) * p.scale, f64::from(t.h) * p.scale);
    if tw < 1e-6 || th < 1e-6 {
        return false;
    }
    for (i, px) in data.chunks_exact_mut(4).enumerate() {
        let (x, y) = ((i % w as usize) as f64 + origin.0 + 0.5, (i / w as usize) as f64 + origin.1 + 0.5);
        let u = (((x - ox).rem_euclid(tw)) / p.scale) as usize;
        let v = (((y - oy).rem_euclid(th)) / p.scale) as usize;
        let at = (v.min(t.h as usize - 1) * t.w as usize + u.min(t.w as usize - 1)) * 4;
        let Some(src) = t.rgba.get(at..at + 4) else { continue };
        let k = f64::from(src[3]) / 255.0 * p.opacity;
        for c in 0..3 {
            px[c] = (f64::from(px[c]) * (1.0 - k) + f64::from(src[c]) * k).round().clamp(0.0, 255.0) as u8;
        }
    }
    true
}

/// Paint gradient `g` (over box `r`) onto pixels `data` (`w` wide, top left at `origin`).
pub(crate) fn bake_gradient(g: &Grad, r: Rect, data: &mut [u8], w: u32, origin: (f64, f64)) {
    for (i, px) in data.chunks_exact_mut(4).enumerate() {
        let p = Point::new((i % w as usize) as f64 + origin.0 + 0.5, (i / w as usize) as f64 + origin.1 + 0.5);
        let c = color_at(g, r, p);
        let k = c[3] * g.opacity;
        for ch in 0..3 {
            px[ch] = (f64::from(px[ch]) * (1.0 - k) + c[ch] * 255.0 * k).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// A layer's bounds in document pixels.
pub(crate) fn layer_box(rec: &figcraft_psd::LayerRecord) -> Rect {
    Rect::new(f64::from(rec.rect.left), f64::from(rec.rect.top), f64::from(rec.rect.right), f64::from(rec.rect.bottom))
}

/// The pattern and gradient overlays of a layer style as REST paints (bottom to top, as Photoshop
/// stacks them) for a node whose box is `node` (document pixels).
pub(crate) fn overlay_paints(sh: &mut Shared, fx: &FileCx, rec: &figcraft_psd::LayerRecord, fxs: &super::effects::Fx, node: Rect) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(p) = &fxs.pattern {
        match pattern_paint(sh, fx, p) {
            Some(v) => out.push(v),
            None => sh.note("pattern overlays whose pattern isn't in the file were left out"),
        }
    }
    if let Some(g) = &fxs.gradient {
        let r = reference(g, layer_box(rec), Rect::new(0.0, 0.0, fx.w, fx.h));
        out.push(paint(g, r, node));
    }
    out
}

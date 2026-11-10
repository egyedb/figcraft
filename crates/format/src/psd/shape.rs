//! Shape layers and vector masks: the path (`vsms` / `vmsk`), the live shape it was drawn as
//! (`vogk`), its fill (`SoCo`) and its stroke (`vstk`).

use figcraft_doc::geom::{Affine, BezPath, Point, Rect, Shape};
use figcraft_doc::{BoolOp, VectorNetwork, WindingRule};
use figcraft_psd::LayerRecord;
use figcraft_psd::descriptor::Value as D;
use figcraft_psd::path::{PsdKnot, VectorMaskBlock};
use serde_json::{Map as JsonMap, Value, json};

use super::Shared;
use super::effects::{self, Fx};
use super::layers::{Built, FileCx};
use super::util::{self, desc, enumv, flag, num};

/// Most knots read from one path.
const MAX_KNOTS: usize = 100_000;

/// The vector path of a layer in document pixels, its winding rule, and whether it is inverted
/// or switched off.
struct LayerPath {
    path: BezPath,
    winding: WindingRule,
    inverted: bool,
    disabled: bool,
}

fn layer_path(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord) -> Option<LayerPath> {
    let block = rec.block(b"vsms").or_else(|| rec.block(b"vmsk"))?;
    let vm = VectorMaskBlock::from_bytes(&block.data).ok()?;
    let subpaths = vm.path.subpaths();
    if subpaths.iter().map(|s| s.knots.len()).sum::<usize>() > MAX_KNOTS {
        sh.note("shapes with very long paths kept the pixels Photoshop rendered for them");
        return None;
    }
    let pt = |p: (f64, f64)| Point::new(p.0 * fx.w, p.1 * fx.h);
    let mut operands: Vec<(BezPath, BoolOp)> = Vec::new();
    for s in &subpaths {
        let Some(first) = s.knots.first() else { continue };
        let mut p = BezPath::new();
        p.move_to(pt(first.anchor));
        let seg = |p: &mut BezPath, a: &PsdKnot, b: &PsdKnot| {
            if a.post == a.anchor && b.pre == b.anchor {
                p.line_to(pt(b.anchor));
            } else {
                p.curve_to(pt(a.post), pt(b.pre), pt(b.anchor));
            }
        };
        for w in s.knots.windows(2) {
            if let [a, b] = w {
                seg(&mut p, a, b);
            }
        }
        if s.closed {
            if let Some(last) = s.knots.last()
                && s.knots.len() > 1
            {
                seg(&mut p, last, first);
            }
            p.close_path();
        }
        let op = match s.operation {
            2 => BoolOp::Subtract,
            3 => BoolOp::Intersect,
            0 => BoolOp::Exclude,
            _ => BoolOp::Union,
        };
        operands.push((p, op));
    }
    if operands.is_empty() {
        return None;
    }
    let all_union = operands.iter().skip(1).all(|(_, op)| *op == BoolOp::Union);
    let (path, winding) = if all_union {
        let mut p = BezPath::new();
        for (o, _) in &operands {
            p.extend(o.elements().iter().copied());
        }
        (p, WindingRule::NonZero)
    } else {
        match figcraft_pathops::boolean_ops(operands.iter().map(|(p, op)| (p, *op))) {
            Ok(p) => (p, WindingRule::NonZero),
            Err(_) => {
                sh.note("shapes whose path operations couldn't be combined were approximated");
                let mut p = BezPath::new();
                for (o, _) in &operands {
                    p.extend(o.elements().iter().copied());
                }
                (p, WindingRule::EvenOdd)
            }
        }
    };
    let bb = path.bounding_box();
    if !bb.is_finite() || bb.width() > util::MAX_SIZE || bb.height() > util::MAX_SIZE {
        return None;
    }
    Some(LayerPath {
        path,
        winding,
        inverted: vm.flags & VectorMaskBlock::FLAG_INVERT != 0,
        disabled: vm.flags & VectorMaskBlock::FLAG_DISABLED != 0,
    })
}

/// The live shape a path was drawn as, when the path still is that shape: a rectangle with its
/// corner radii (top left, top right, bottom right, bottom left), or an ellipse.
enum Live {
    Rect(Rect, [f64; 4]),
    Ellipse(Rect),
}

fn live_shape(rec: &LayerRecord, path_box: Rect) -> Option<Live> {
    let d = rec.block(b"vogk").and_then(|b| b.data.get(4..).and_then(util::versioned))?;
    let list = util::list(&d, "keyDescriptorList");
    let [D::Descriptor(k)] = list else { return None };
    let b = desc(k, "keyOriginShapeBBox")?;
    let r = Rect::new(num(b, "Left")?, num(b, "Top ")?, num(b, "Rght")?, num(b, "Btom")?);
    // Moved, rotated or edited since: the path says where it is.
    let close = |a: f64, b: f64| (a - b).abs() <= 1.0;
    if !(close(r.x0, path_box.x0) && close(r.y0, path_box.y0) && close(r.x1, path_box.x1) && close(r.y1, path_box.y1)) {
        return None;
    }
    match num(k, "keyOriginType").map(|t| t as i64) {
        Some(1) => Some(Live::Rect(r, [0.0; 4])),
        Some(2) => {
            let q = desc(k, "keyOriginRRectRadii");
            let rad = |key: &str| q.and_then(|q| num(q, key)).unwrap_or(0.0).clamp(0.0, r.width().min(r.height()) / 2.0);
            Some(Live::Rect(r, [rad("topLeft"), rad("topRight"), rad("bottomRight"), rad("bottomLeft")]))
        }
        Some(5) => Some(Live::Ellipse(r)),
        _ => None,
    }
}

/// A shape layer (`None` when it should keep Photoshop's pixels instead).
pub(crate) fn build(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, base: Affine, fxs: &Fx) -> Option<Built> {
    let lp = layer_path(sh, fx, rec)?;
    if lp.inverted {
        sh.note("inverted shapes kept the pixels Photoshop rendered for them");
        return None;
    }
    let bb = lp.path.bounding_box();
    let mut node = JsonMap::new();
    // A live shape is placed by its own exact box (the path's points are rounded to 1/2^24).
    let mut at = (bb.x0, bb.y0);
    let (ty, w, h) = match live_shape(rec, bb) {
        Some(Live::Rect(r, radii)) => {
            at = (r.x0, r.y0);
            if radii.iter().all(|x| (*x - radii[0]).abs() < 1e-6) {
                if radii[0] > 0.0 {
                    node.insert("cornerRadius".into(), json!(radii[0]));
                }
            } else {
                node.insert("rectangleCornerRadii".into(), json!(radii));
            }
            ("RECTANGLE", r.width(), r.height())
        }
        Some(Live::Ellipse(r)) => {
            at = (r.x0, r.y0);
            ("ELLIPSE", r.width(), r.height())
        }
        None => {
            let moved = Affine::translate((-bb.x0, -bb.y0)) * lp.path.clone();
            let mut net = VectorNetwork::from_path(&moved);
            for r in &mut net.regions {
                r.winding = lp.winding;
            }
            node.insert("vectorNetworkRef".into(), json!(sh.networks.len()));
            sh.networks.push(Some(net));
            ("VECTOR", bb.width(), bb.height())
        }
    };
    let mut b = Built::new(sh, ty, &rec.name(), base * Affine::translate(at), w, h);
    b.node.extend(node);
    let fill_opacity = f64::from(rec.fill_opacity()) / 255.0;
    let stroke = rec.block(b"vstk").and_then(|s| util::versioned(&s.data));
    let fill_on = stroke.as_ref().and_then(|s| flag(s, "fillEnabled")).unwrap_or(true);
    let node_box = Rect::new(at.0, at.1, at.0 + w, at.1 + h);
    let canvas = Rect::new(0.0, 0.0, fx.w, fx.h);
    let block = |key: &[u8; 4]| rec.block(key).and_then(|s| util::versioned(&s.data));
    let mut fills: Vec<Value> = Vec::new();
    if fill_on {
        if let Some(c) = block(b"SoCo").and_then(|d| desc(&d, "Clr ").and_then(util::color)) {
            fills.push(json!({"type": "SOLID", "color": util::rgba(c, 1.0), "opacity": fill_opacity}));
        } else if let Some(d) = block(b"GdFl") {
            let Some(g) = super::fills::gradient(&d, fill_opacity) else {
                sh.note("shapes with noise gradients kept the pixels Photoshop rendered for them");
                return None;
            };
            fills.push(super::fills::paint(&g, super::fills::reference(&g, node_box, canvas), node_box));
        } else if let Some(d) = block(b"PtFl") {
            let Some(p) = super::fills::pattern(&d, fill_opacity).and_then(|p| super::fills::pattern_paint(sh, fx, &p)) else {
                sh.note("shapes whose pattern isn't in the file kept the pixels Photoshop rendered for them");
                return None;
            };
            fills.push(p);
        }
        // The layer style's overlays, over the shape's own fill (pattern, gradient, colour on top).
        fills.extend(super::fills::overlay_paints(sh, fx, rec, fxs, node_box));
        if let Some((c, o)) = fxs.overlay {
            fills.push(json!({"type": "SOLID", "color": util::rgba(c, 1.0), "opacity": o * fill_opacity}));
        }
    }
    b.node.insert("fills".into(), Value::Array(fills));
    if let Some(s) = stroke.as_ref().filter(|s| flag(s, "strokeEnabled") == Some(true)) {
        stroke_fields(sh, s, fill_opacity, &mut b.node);
    } else if let Some(s) = &fxs.stroke {
        effects::apply_stroke(&mut b.node, s);
    }
    Some(b)
}

fn stroke_fields(sh: &mut Shared, s: &figcraft_psd::Descriptor, fill_opacity: f64, node: &mut JsonMap<String, Value>) {
    let Some(c) = desc(s, "strokeStyleContent").and_then(|c| desc(c, "Clr ")).and_then(util::color) else {
        sh.note("gradient and pattern shape strokes were left out");
        return;
    };
    let opacity = num(s, "strokeStyleOpacity").map_or(1.0, |o| (o / 100.0).clamp(0.0, 1.0)) * fill_opacity;
    let width = num(s, "strokeStyleLineWidth").unwrap_or(1.0).clamp(0.0, 1000.0);
    node.insert("strokes".into(), json!([{"type": "SOLID", "color": util::rgba(c, 1.0), "opacity": opacity}]));
    node.insert("strokeWeight".into(), json!(width));
    let align = match enumv(s, "strokeStyleLineAlignment").as_deref() {
        Some("strokeStyleAlignInside") => "INSIDE",
        Some("strokeStyleAlignOutside") => "OUTSIDE",
        _ => "CENTER",
    };
    node.insert("strokeAlign".into(), json!(align));
    let cap = match enumv(s, "strokeStyleLineCapType").as_deref() {
        Some("strokeStyleRoundCap") => "ROUND",
        Some("strokeStyleSquareCap") => "SQUARE",
        _ => "NONE",
    };
    node.insert("strokeCap".into(), json!(cap));
    let join = match enumv(s, "strokeStyleLineJoinType").as_deref() {
        Some("strokeStyleRoundJoin") => "ROUND",
        Some("strokeStyleBevelJoin") => "BEVEL",
        _ => "MITER",
    };
    node.insert("strokeJoin".into(), json!(join));
    let dashes: Vec<f64> =
        util::list(s, "strokeStyleLineDashSet").iter().filter_map(util::value_num).map(|d| (d * width).clamp(0.0, 10_000.0)).collect();
    if !dashes.is_empty() && dashes.iter().any(|d| *d > 0.0) {
        node.insert("strokeDashes".into(), json!(dashes));
    }
}

/// The vector mask of a layer that isn't a shape, as a mask layer.
pub(crate) fn vector_mask(sh: &mut Shared, fx: &FileCx, rec: &LayerRecord, base: Affine) -> Option<Built> {
    let lp = layer_path(sh, fx, rec)?;
    if lp.disabled {
        return None;
    }
    if lp.inverted {
        sh.note("inverted vector masks were left out");
        return None;
    }
    let bb = lp.path.bounding_box();
    let moved = Affine::translate((-bb.x0, -bb.y0)) * lp.path;
    let mut net = VectorNetwork::from_path(&moved);
    for r in &mut net.regions {
        r.winding = lp.winding;
    }
    let mut b = Built::new(sh, "VECTOR", "Vector mask", base * Affine::translate((bb.x0, bb.y0)), bb.width(), bb.height());
    b.node.insert("vectorNetworkRef".into(), json!(sh.networks.len()));
    sh.networks.push(Some(net));
    b.node.insert("fills".into(), json!([{"type": "SOLID", "color": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}]));
    b.node.insert("isMask".into(), json!(true));
    Some(b)
}

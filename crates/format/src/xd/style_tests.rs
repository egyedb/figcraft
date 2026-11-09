//! `.xd` styles: colours, strokes, effects, opacity, blend modes and gradients.

use figcraft_doc::color::PaintKind;
use figcraft_doc::{EffectKind, StrokeAlign};
use serde_json::json;

use super::fixture::*;
use super::*;

fn styled(style: serde_json::Value) -> figcraft_doc::Node {
    let mut r = rect("R", 0.0, 0.0, 100.0, 50.0);
    r["style"] = style;
    let (doc, _) = import(&xd_zip(&[board("A", (0.0, 0.0, 200.0, 200.0), vec![r])], vec![], vec![], &[]), "f").unwrap();
    doc.nodes().find(|n| n.name == "R").unwrap().clone()
}

#[test]
fn solid_fills_keep_colour_and_alpha() {
    let n = styled(json!({"fill": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 255, "g": 128, "b": 0}, "alpha": 0.5}}}));
    let PaintKind::Solid { color } = &n.fills[0].kind else { panic!("{:?}", n.fills) };
    assert!((color.r - 1.0).abs() < 1e-6 && (color.g - 128.0 / 255.0).abs() < 1e-6 && color.b == 0.0);
    assert!((color.a - 0.5).abs() < 1e-6 || (n.fills[0].opacity - 0.5).abs() < 1e-6);
}

#[test]
fn no_fill_is_no_paint() {
    assert!(styled(json!({"fill": {"type": "none"}})).fills.is_empty());
    assert!(styled(json!({})).fills.is_empty());
}

#[test]
fn strokes_keep_width_alignment_and_dashes() {
    let n = styled(
        json!({"stroke": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}, "width": 3, "align": "inside", "dash": [6]}}),
    );
    assert_eq!(n.strokes.len(), 1);
    assert_eq!(n.stroke.weight, 3.0);
    assert_eq!(n.stroke.align, StrokeAlign::Inside);
    assert_eq!(n.stroke.dashes, [6.0, 6.0]);
    let centred = styled(json!({"stroke": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}, "width": 1}}));
    assert_eq!(centred.stroke.align, StrokeAlign::Center);
    let none = styled(json!({"stroke": {"type": "none", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}, "width": 1}}));
    assert!(none.strokes.is_empty());
}

#[test]
fn shadows_and_blurs_become_effects_and_hidden_ones_are_left_out() {
    let n = styled(json!({"filters": [
        {"type": "dropShadow", "params": {"dropShadows": [{"dx": 0, "dy": 6, "r": 9, "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}, "alpha": 0.3}}]}},
        {"type": "uxdesign#innerShadow", "params": {"innerShadows": [{"dx": 1, "dy": 1, "r": 2, "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}}]}},
        {"type": "uxdesign#blur", "visible": false, "params": {"blurAmount": 10, "backgroundEffect": false}},
    ]}));
    assert_eq!(n.effects.len(), 2, "{:?}", n.effects);
    let EffectKind::DropShadow(s) = &n.effects[0].kind else { panic!() };
    assert_eq!((s.offset, s.radius), ((0.0, 6.0), 9.0));
    assert!(matches!(n.effects[1].kind, EffectKind::InnerShadow(_)));
}

#[test]
fn opacity_and_blend_mode() {
    let n = styled(json!({"fill": rgb(0, 0, 0), "opacity": 0.25, "blendMode": "multiply"}));
    assert!((n.opacity - 0.25).abs() < 1e-6);
    assert_eq!(n.blend, figcraft_doc::color::BlendMode::Multiply);
}

#[test]
fn linear_gradients_keep_their_stops() {
    let n = styled(json!({"fill": {"type": "gradient", "gradient": {"units": "objectBoundingBox", "x1": 0, "y1": 0.5, "x2": 1, "y2": 0.5,
        "meta": {"ux": {"gradientResources": {"type": "linear", "stops": [
            {"offset": 0, "color": {"mode": "RGB", "value": {"r": 255, "g": 0, "b": 0}}},
            {"offset": 1, "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 255}}}]}}}}}}));
    let PaintKind::Gradient(g) = &n.fills[0].kind else { panic!("{:?}", n.fills) };
    assert_eq!(g.stops.len(), 2);
}

#[test]
fn colours_as_argb_numbers() {
    let c = super::style::color(&json!({"value": 0x80ff0000u32})).unwrap();
    assert_eq!(c["r"], 1.0);
    assert!((c["a"].as_f64().unwrap() - 128.0 / 255.0).abs() < 1e-9);
    assert!(super::style::color(&json!({"value": "red"})).is_none());
    assert!(super::style::color(&json!({"value": u64::MAX})).is_none());
}

#[test]
fn moved_and_zoomed_images_keep_xds_crop() {
    // A 1600 × 1168 image covering a 417 px square, zoomed 1.0889 times and moved: XD puts its
    // top edge on the square's (centred, the zoom would hide 18.5 px above; the offset of
    // 0.0445 of its covering height brings it back down).
    let n = styled(json!({"fill": {"type": "pattern", "pattern": {"width": 1600, "height": 1168, "href": "",
        "meta": {"ux": {"scaleBehavior": "cover", "uid": "abc123", "scale": 1.0889, "offsetX": -0.0791, "offsetY": 0.0445}}}}}));
    let PaintKind::Image(img) = &n.fills[0].kind else { panic!("{:?}", n.fills) };
    let [a, _, _, d, e, f] = img.crop.unwrap_or_else(|| panic!("no crop: {img:?}"));
    let (w, h) = (100.0_f64, 50.0_f64);
    // The shape here is 100 × 50 (the fixture's): recompute what XD does for it.
    let cover = (w / 1600.0_f64).max(h / 1168.0);
    let (cw, ch) = (1600.0 * cover, 1168.0 * cover);
    let (dw, dh) = (cw * 1.0889, ch * 1.0889);
    let (x0, y0) = ((w - dw) / 2.0 - 0.0791 * cw, (h - dh) / 2.0 + 0.0445 * ch);
    assert!((a - w / dw).abs() < 1e-9 && (d - h / dh).abs() < 1e-9);
    assert!((e + x0 / dw).abs() < 1e-9 && (f + y0 / dh).abs() < 1e-9);
    // Without a scale or offsets the image just covers the shape.
    let plain = styled(
        json!({"fill": {"type": "pattern", "pattern": {"width": 16, "height": 9, "href": "", "meta": {"ux": {"scaleBehavior": "cover", "uid": "abc123"}}}}}),
    );
    let PaintKind::Image(img) = &plain.fills[0].kind else { panic!() };
    assert_eq!(img.scale_mode, figcraft_doc::color::ImageScaleMode::Fill);
    assert!(img.crop.is_none());
}

#[test]
fn radial_gradients_follow_their_transform() {
    // Mirrored left to right about x = 0.7: the centre at 0.2 lands at 1.2, the radius points left.
    let n =
        styled(json!({"fill": {"type": "gradient", "gradient": {"units": "objectBoundingBox", "cx": 0.2, "cy": 0.5, "fx": 0.2, "fy": 0.5, "r": 0.3,
        "transform": {"a": -1, "b": 0, "c": 0, "d": 1, "tx": 1.4, "ty": 0},
        "meta": {"ux": {"gradientResources": {"type": "radial", "stops": [
            {"offset": 0, "color": {"mode": "RGB", "value": {"r": 255, "g": 0, "b": 0}}},
            {"offset": 1, "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 255}}}]}}}}}}));
    let PaintKind::Gradient(g) = &n.fills[0].kind else { panic!("{:?}", n.fills) };
    let near = |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
    assert!(near(g.from, (1.2, 0.5)), "{:?}", g.from);
    assert!(near(g.to, (0.9, 0.5)), "{:?}", g.to);
}

#[test]
fn image_colour_adjustments_become_filters() {
    let fill = |params: serde_json::Value| {
        json!({"fill": {"type": "pattern", "pattern": {"width": 4, "height": 4, "href": "",
            "meta": {"ux": {"scaleBehavior": "cover", "uid": "abc123"}}, "uxdesign#colorTransform": {"params": params}}}})
    };
    let n = styled(fill(json!({"h": 0, "s": -0.5, "b": 0.1, "c": 0})));
    let PaintKind::Image(img) = &n.fills[0].kind else { panic!() };
    assert!((img.filters.saturation + 0.5).abs() < 1e-6);
    // Brighter: more exposure, contrast lowered so mid-grey rises by the brightness.
    assert!(img.filters.exposure > 0.0 && img.filters.contrast < 0.0, "{:?}", img.filters);
    let untouched = styled(fill(json!({"h": 0, "s": 0, "b": 0, "c": 0})));
    let PaintKind::Image(img) = &untouched.fills[0].kind else { panic!() };
    assert!(img.filters.is_identity());
}

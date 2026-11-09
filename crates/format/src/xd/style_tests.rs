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

//! `.xd` import: detection, the archive, artboards and budgets, on fixtures built in code.

use figcraft_doc::{NodeKind, geom::Point};
use serde_json::json;

use super::fixture::*;
use super::*;

fn find<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

/// Where a layer's top-left corner lands on the page.
pub(crate) fn origin_on_page(doc: &Document, name: &str) -> Point {
    let n = find(doc, name);
    doc.world_transform(n.id) * Point::ZERO
}

#[test]
fn detects_xd_and_not_others() {
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), vec![])], vec![], vec![], &[]);
    assert!(is_xd(&z));
    assert!(!is_xd(b"PK\x03\x04junk"));
    assert!(!is_xd(b"{}"));
    assert!(!is_xd(b""));
    let other = zip_of(&[("mimetype".into(), b"application/zip".to_vec())]);
    assert!(!is_xd(&other));
    assert!(try_import(&other, "x").is_none());
}

#[test]
fn artboards_become_frames_at_their_places() {
    let boards = [board("B1", (100.0, 50.0, 400.0, 300.0), vec![]), board("B2", (600.0, 50.0, 200.0, 100.0), vec![])];
    let (doc, _) = import(&xd_zip(&boards, vec![], vec![], &[]), "f").unwrap();
    let b1 = find(&doc, "B1");
    assert!(matches!(b1.kind, NodeKind::Frame));
    assert_eq!((b1.width, b1.height), (400.0, 300.0));
    assert_eq!(origin_on_page(&doc, "B1"), Point::new(100.0, 50.0));
    assert_eq!(origin_on_page(&doc, "B2"), Point::new(600.0, 50.0));
    assert!(b1.clip);
    // Manifest order is layer order on the page.
    let page = doc.first_page().unwrap();
    let names: Vec<&str> = doc.node(page).unwrap().children.iter().map(|c| doc.node(*c).unwrap().name.as_str()).collect();
    assert_eq!(names, ["B1", "B2"]);
}

#[test]
fn layers_in_an_artboard_are_placed_on_the_canvas() {
    // The artboard sits at 1000, 500; its rectangle's transform is in canvas coordinates.
    let boards = [board("A", (1000.0, 500.0, 300.0, 300.0), vec![rect("R", 1010.0, 520.0, 50.0, 40.0)])];
    let (doc, _) = import(&xd_zip(&boards, vec![], vec![], &[]), "f").unwrap();
    assert_eq!(origin_on_page(&doc, "R"), Point::new(1010.0, 520.0));
    let r = find(&doc, "R");
    assert_eq!(doc.node(r.parent.unwrap()).unwrap().name, "A");
    assert_eq!((r.width, r.height), (50.0, 40.0));
}

#[test]
fn pasteboard_layers_go_on_the_page() {
    let z = xd_zip(&[], vec![rect("Loose", -200.0, 30.0, 10.0, 10.0)], vec![], &[]);
    let (doc, _) = import(&z, "f").unwrap();
    assert_eq!(origin_on_page(&doc, "Loose"), Point::new(-200.0, 30.0));
}

#[test]
fn images_come_from_resources() {
    let png = {
        let img = image::RgbaImage::from_pixel(4, 3, image::Rgba([0, 128, 255, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    };
    let mut r = rect("Img", 0.0, 0.0, 40.0, 30.0);
    r["style"]["fill"] = json!({"type": "pattern", "pattern": {"width": 4, "height": 3, "href": "",
        "meta": {"ux": {"scaleBehavior": "cover", "uid": "0123abcd"}}}});
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), vec![r])], vec![], vec![], &[("resources/0123abcd", &png)]);
    let (doc, warnings) = import(&z, "f").unwrap();
    assert_eq!(doc.images.len(), 1, "{warnings:?}");
    let n = find(&doc, "Img");
    assert!(n.fills.iter().any(|p| matches!(p.kind, figcraft_doc::color::PaintKind::Image { .. })));
}

#[test]
fn a_missing_artboard_member_is_skipped_with_a_warning() {
    let boards = [board("A", (0.0, 0.0, 10.0, 10.0), vec![])];
    let z = without(&xd_zip(&boards, vec![], vec![], &[]), "artwork/artboard-A/graphics/graphicContent.agc");
    let (doc, warnings) = import(&z, "f").unwrap();
    assert!(warnings.iter().any(|w| w.contains("artboard was skipped")), "{warnings:?}");
    let _ = doc;
}

#[test]
fn layer_budget_is_kept() {
    let layers: Vec<_> = (0..50).map(|i| rect(&format!("R{i}"), f64::from(i), 0.0, 1.0, 1.0)).collect();
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), layers)], vec![], vec![], &[]);
    let (doc, warnings) = import_within_nodes(&z, "f", 10).unwrap();
    assert!(doc.nodes().filter(|n| !n.is_page()).count() <= 10);
    assert!(warnings.iter().any(|w| w.contains("more than 10 layers")), "{warnings:?}");
}

#[test]
fn junk_never_panics() {
    let mut t = point_text("T", 10.0, 30.0, "Hello");
    t["meta"]["ux"]["rangedStyles"][0]["length"] = json!(u64::MAX);
    let layers = vec![rect("R", 0.0, 0.0, 10.0, 10.0), t, group("G", None, vec![rect("R2", 5.0, 5.0, 1.0, 1.0)])];
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), layers)], vec![], vec![], &[]);
    for cut in (0..z.len()).step_by(11) {
        let _ = import(&z[..cut], "f");
    }
    // Wrong types everywhere.
    let junk = vec![
        json!({"type": "shape"}),
        json!({"type": "shape", "shape": {"type": "path", "path": "M 1e400 0 L"}}),
        json!({"type": "shape", "shape": {"type": "rect", "width": "x", "r": [1, "a", null]}, "transform": {"a": "1"}}),
        json!({"type": "text", "text": {"paragraphs": [{"lines": [[{"x": null}]]}]}, "meta": {"ux": {"rangedStyles": [{"length": -5}]}}}),
        json!({"type": "group", "group": {"children": "nope"}, "meta": {"ux": {"clipPathResources": {"children": [1, 2]}}}}),
        json!({"type": "syncRef", "syncSourceGuid": 5}),
        json!({"type": "shape", "shape": {"type": "polygon", "points": [{"x": 1}]}}),
        json!({"type": "shape", "shape": {"type": "line", "x1": f64::MAX, "y1": 0, "x2": -f64::MAX, "y2": 0}}),
        json!({"type": "shape", "style": {"fill": {"type": "gradient", "gradient": {"meta": {"ux": {"gradientResources": {"stops": [{"offset": "a"}]}}}}},
            "stroke": {"type": "solid", "width": -3, "dash": ["x"]}, "filters": [{"type": "dropShadow", "params": {"dropShadows": [{"color": 3}]}}]},
            "shape": {"type": "circle", "r": -4}}),
        json!(null),
        json!([1, 2]),
    ];
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), junk)], vec![], vec![], &[]);
    let _ = import(&z, "f");
}

#[test]
fn deep_nesting_is_cut_with_a_warning() {
    let mut l = rect("Deep", 0.0, 0.0, 1.0, 1.0);
    for i in 0..300 {
        l = group(&format!("G{i}"), None, vec![l]);
    }
    let z = xd_zip(&[board("A", (0.0, 0.0, 100.0, 100.0), vec![l])], vec![], vec![], &[]);
    // The JSON reader's own depth limit or the import's: either way a note, never a crash.
    let (_, warnings) = import(&z, "f").unwrap();
    assert!(warnings.iter().any(|w| w.contains("nested deeper") || w.contains("recursion limit")), "{warnings:?}");
}

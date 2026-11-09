//! `.xd` scrolling: artboards taller than their viewport scroll (the page's prototype screen is
//! their viewport), fixed layers stay put, scroll groups are frames of their viewport.

use figcraft_doc::NodeKind;
use figcraft_doc::geom::Point;
use figcraft_doc::proto::{Overflow, PrototypeSettings, ScrollPosition};
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

/// A 375 × 1200 artboard showing 375 × 812 at a time, holding `layers`.
fn tall(layers: Vec<Value>) -> (Document, Vec<String>) {
    let z = xd_zip(&[board("Phone", (0.0, 0.0, 375.0, 1200.0), layers)], vec![], vec![], &[]);
    // The fixture's resources don't say a viewport: add it.
    let res = json!({"version": "1.5.0", "children": [], "resources": {"meta": {"ux": {"symbols": []}}},
        "artboards": {"Phone": {"name": "Phone", "x": 0, "y": 0, "width": 375, "height": 1200, "viewportHeight": 812}}});
    let z = with_member(&z, "resources/graphics/graphicContent.agc", res.to_string().as_bytes());
    import(&z, "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

fn at(doc: &Document, name: &str) -> Point {
    doc.world_transform(node(doc, name).id) * Point::ZERO
}

#[test]
fn tall_artboards_scroll_in_a_screen_of_their_viewport() {
    let mut header = rect("Header", 0.0, 0.0, 375.0, 60.0);
    header["meta"] = json!({"ux": {"fixed": true}});
    let (doc, warnings) = tall(vec![header, rect("Body", 0.0, 60.0, 375.0, 1100.0)]);
    let phone = node(&doc, "Phone");
    assert_eq!(phone.overflow, Overflow::Vertical, "{warnings:?}");
    assert_eq!(node(&doc, "Header").scroll_position, ScrollPosition::Fixed);
    assert_eq!(node(&doc, "Body").scroll_position, ScrollPosition::Scrolls);
    let page = doc.node(doc.first_page().unwrap()).unwrap();
    let NodeKind::Page { prototype, .. } = &page.kind else { panic!() };
    assert_eq!(prototype.device, PrototypeSettings::CUSTOM_DEVICE);
    assert_eq!(prototype.custom_size, [375.0, 812.0]);
    assert!(!warnings.iter().any(|w| w.contains("scroll")), "{warnings:?}");
}

fn scroll_group(ux: Value, children: Vec<Value>) -> Value {
    let mut g = group("Scroller", None, children);
    g["meta"] = json!({"ux": ux});
    g
}

#[test]
fn horizontal_scroll_groups_are_frames_of_their_viewport() {
    // Cards from x 23 to 683; the viewport starts 23 before them and is 360 wide.
    let cards: Vec<Value> = (0..4).map(|i| rect(&format!("Card{i}"), 23.0 + f64::from(i) * 170.0, 100.0, 150.0, 80.0)).collect();
    let (doc, _) = tall(vec![scroll_group(json!({"scrollingType": "horizontal", "offsetX": -23, "viewportWidth": 360}), cards)]);
    let s = node(&doc, "Scroller");
    assert!(matches!(s.kind, NodeKind::Frame));
    assert_eq!(s.overflow, Overflow::Horizontal);
    assert!(s.clip);
    assert_eq!((s.width, s.height), (360.0, 80.0));
    assert_eq!(at(&doc, "Scroller"), Point::new(0.0, 100.0));
    // The cards stay where XD shows them.
    assert_eq!(at(&doc, "Card1"), Point::new(193.0, 100.0));
}

#[test]
fn vertical_scroll_groups_clip_to_their_viewport_height() {
    let rows: Vec<Value> = (0..10).map(|i| rect(&format!("Row{i}"), 0.0, 200.0 + f64::from(i) * 50.0, 375.0, 40.0)).collect();
    let (doc, _) = tall(vec![scroll_group(json!({"scrollingType": "vertical", "offsetY": 0, "viewportHeight": 300}), rows)]);
    let s = node(&doc, "Scroller");
    assert_eq!(s.overflow, Overflow::Vertical);
    assert_eq!((s.width, s.height), (375.0, 300.0));
    assert_eq!(at(&doc, "Scroller"), Point::new(0.0, 200.0));
    assert_eq!(at(&doc, "Row9"), Point::new(0.0, 650.0));
}

#[test]
fn artboards_that_fit_their_viewport_do_not_scroll() {
    let (doc, _) = import(&xd_zip(&[board("Flat", (0.0, 0.0, 375.0, 812.0), vec![])], vec![], vec![], &[]), "f").unwrap();
    assert_eq!(node(&doc, "Flat").overflow, Overflow::None);
}

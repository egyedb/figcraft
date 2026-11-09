//! `.xd` layers: groups and transforms, shapes, masks, component copies and the notes about
//! what isn't supported yet.

use figcraft_doc::geom::Point;
use figcraft_doc::{MaskType, NodeKind};
use serde_json::json;

use super::fixture::*;
use super::tests::origin_on_page;
use super::*;

fn one_board(layers: Vec<serde_json::Value>) -> (Document, Vec<String>) {
    import(&xd_zip(&[board("A", (0.0, 0.0, 500.0, 500.0), layers)], vec![], vec![], &[]), "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

fn close(a: Point, b: Point) -> bool {
    (a - b).hypot() < 1e-6
}

#[test]
fn group_transforms_compose() {
    let inner = rect("R", 30.0, 40.0, 10.0, 10.0);
    let (doc, _) = one_board(vec![group("G", Some(translate(100.0, 200.0)), vec![inner])]);
    assert!(close(origin_on_page(&doc, "R"), Point::new(130.0, 240.0)));
    let g = node(&doc, "G");
    assert!(matches!(g.kind, NodeKind::Group));
    // The group is sized to what it holds.
    assert!(close(origin_on_page(&doc, "G"), Point::new(130.0, 240.0)));
    assert_eq!((g.width.round(), g.height.round()), (10.0, 10.0));
}

#[test]
fn rotated_groups_keep_their_layers_where_xd_draws_them() {
    // A half turn about the origin, then moved: the rectangle's corner at 10, 0 lands at 90, 100.
    let t = json!({"a": -1, "b": 0, "c": 0, "d": -1, "tx": 100, "ty": 100});
    let (doc, _) = one_board(vec![group("G", Some(t), vec![rect("R", 10.0, 0.0, 20.0, 5.0)])]);
    let r = node(&doc, "R");
    let corner = doc.world_transform(r.id) * Point::ZERO;
    assert!(close(corner, Point::new(90.0, 100.0)), "{corner:?}");
    let far = doc.world_transform(r.id) * Point::new(20.0, 5.0);
    assert!(close(far, Point::new(70.0, 95.0)), "{far:?}");
}

#[test]
fn circles_ellipses_and_rounded_rectangles() {
    let circle = json!({"type": "shape", "id": "C", "name": "C", "transform": translate(10.0, 10.0),
        "style": {"fill": rgb(0, 0, 255)}, "shape": {"type": "circle", "cx": 20, "cy": 20, "r": 20}});
    let ellipse = json!({"type": "shape", "id": "E", "name": "E", "style": {"fill": rgb(0, 0, 255)},
        "shape": {"type": "ellipse", "cx": 100, "cy": 50, "rx": 30, "ry": 10}});
    let mut rounded = rect("RR", 0.0, 0.0, 50.0, 50.0);
    rounded["shape"]["r"] = json!([4, 4, 0, 0]);
    let (doc, _) = one_board(vec![circle, ellipse, rounded]);
    let c = node(&doc, "C");
    assert!(matches!(c.kind, NodeKind::Ellipse { .. }));
    assert_eq!((c.width, c.height), (40.0, 40.0));
    assert!(close(origin_on_page(&doc, "C"), Point::new(10.0, 10.0)));
    let e = node(&doc, "E");
    assert_eq!((e.width, e.height), (60.0, 20.0));
    assert!(close(origin_on_page(&doc, "E"), Point::new(70.0, 40.0)));
    assert_eq!(node(&doc, "RR").corners.radii, [4.0, 4.0, 0.0, 0.0]);
}

#[test]
fn lines_run_between_their_points() {
    let line = json!({"type": "shape", "id": "L", "name": "L", "transform": translate(0.5, 10.5),
        "style": {"stroke": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}, "width": 1}},
        "shape": {"type": "line", "x1": 0, "y1": 0, "x2": 0, "y2": 100}});
    let (doc, _) = one_board(vec![line]);
    let l = node(&doc, "L");
    assert!(matches!(l.kind, NodeKind::Line));
    assert!((l.width - 100.0).abs() < 1e-9);
    let end = doc.world_transform(l.id) * Point::new(l.width, 0.0);
    assert!(close(end, Point::new(0.5, 110.5)), "{end:?}");
}

#[test]
fn paths_keep_their_place_and_outline() {
    let path = json!({"type": "shape", "id": "P", "name": "P", "style": {"fill": rgb(0, 0, 0)},
        "shape": {"type": "path", "path": "M 100 200 L 140 200 L 120 230 Z"}});
    let (doc, _) = one_board(vec![path]);
    let p = node(&doc, "P");
    let NodeKind::Vector { network } = &p.kind else { panic!("not a vector: {:?}", p.kind) };
    assert_eq!(network.vertices.len(), 3);
    assert_eq!((p.width, p.height), (40.0, 30.0));
    assert!(close(origin_on_page(&doc, "P"), Point::new(100.0, 200.0)));
}

#[test]
fn polygons_and_compound_shapes_are_vectors() {
    let poly = json!({"type": "shape", "id": "Tri", "name": "Tri", "style": {"fill": rgb(0, 0, 0)},
        "shape": {"type": "polygon", "points": [{"x": 5, "y": 0}, {"x": 10, "y": 10}, {"x": 0, "y": 10}], "uxdesign#cornerCount": 3}});
    let compound = json!({"type": "shape", "id": "Cmp", "name": "Cmp", "style": {"fill": rgb(0, 0, 0)},
        "shape": {"type": "compound", "operation": "exclude", "path": "M 0 0 L 10 0 L 10 10 L 0 10 Z M 2 2 L 8 2 L 8 8 L 2 8 Z", "children": []}});
    let (doc, _) = one_board(vec![poly, compound]);
    assert!(matches!(node(&doc, "Tri").kind, NodeKind::Vector { .. }));
    let NodeKind::Vector { network } = &node(&doc, "Cmp").kind else { panic!() };
    assert!(network.regions.iter().all(|r| r.winding == figcraft_doc::WindingRule::EvenOdd));
}

#[test]
fn clip_paths_become_masks_in_the_groups_space() {
    let mut g = group("Masked", Some(translate(50.0, 50.0)), vec![rect("Pic", 0.0, 0.0, 200.0, 200.0)]);
    g["meta"] = json!({"ux": {"clipPathResources": {"type": "clipPath", "children": [
        {"type": "shape", "id": "crop", "name": "crop", "transform": translate(10.0, 20.0), "shape": {"type": "rect", "x": 0, "y": 0, "width": 30, "height": 40}}
    ]}}});
    let (doc, _) = one_board(vec![g]);
    let gnode = node(&doc, "Masked");
    let first = doc.node(gnode.children[0]).unwrap();
    assert_eq!(first.name, "crop");
    assert!(first.is_mask);
    assert_eq!(first.mask_type, MaskType::Vector);
    assert!(close(origin_on_page(&doc, "crop"), Point::new(60.0, 70.0)));
    assert_eq!((first.width, first.height), (30.0, 40.0));
    // What it clips comes after it.
    assert_eq!(doc.node(gnode.children[1]).unwrap().name, "Pic");
}

#[test]
fn hidden_layers_stay_hidden() {
    let mut r = rect("Off", 0.0, 0.0, 10.0, 10.0);
    r["visible"] = json!(false);
    let (doc, _) = one_board(vec![r]);
    assert!(!node(&doc, "Off").visible);
}

#[test]
fn component_references_are_copied_in_as_plain_layers() {
    let def = json!({"type": "group", "id": "MAIN", "name": "Button", "meta": {"ux": {"symbolId": "S1"}}, "group": {"children": [
        {"type": "shape", "id": "SRC", "name": "Face", "transform": translate(0.0, 0.0), "style": {"fill": rgb(0, 200, 0)},
            "shape": {"type": "rect", "x": 0, "y": 0, "width": 80, "height": 30}}
    ]}});
    let inst = |id: &str, x: f64, guid: &str| {
        json!({"type": "group", "id": id, "name": id, "transform": translate(x, 10.0),
            "meta": {"ux": {"symbolId": "S1", "syncMap": {"SRC": guid}}},
            "group": {"children": [{"type": "syncRef", "syncSourceGuid": "SRC", "guid": guid}]}})
    };
    let z = xd_zip(&[board("A", (0.0, 0.0, 500.0, 500.0), vec![inst("I1", 0.0, "g1"), inst("I2", 100.0, "g2")])], vec![], vec![def], &[]);
    let (doc, warnings) = import(&z, "f").unwrap();
    let faces: Vec<_> = doc.nodes().filter(|n| n.name == "Face").collect();
    assert_eq!(faces.len(), 2);
    let xs: Vec<f64> = faces.iter().map(|f| (doc.world_transform(f.id) * Point::ZERO).x).collect();
    assert!(xs.contains(&0.0) && xs.contains(&100.0), "{xs:?}");
    assert!(warnings.iter().any(|w| w.contains("components are not supported yet")), "{warnings:?}");
}

#[test]
fn a_reference_to_a_missing_component_is_noted() {
    let r = json!({"type": "syncRef", "syncSourceGuid": "NOPE", "guid": "g"});
    let (_, warnings) = one_board(vec![group("G", None, vec![r, rect("Keep", 0.0, 0.0, 1.0, 1.0)])]);
    assert!(warnings.iter().any(|w| w.contains("definition is missing")), "{warnings:?}");
}

#[test]
fn a_component_that_holds_itself_stops() {
    // A definition whose child refers back to it: the copies stop at the depth cap.
    let def = json!({"type": "group", "id": "LOOP", "name": "Loop", "group": {"children": [
        {"type": "syncRef", "syncSourceGuid": "LOOP", "guid": "again"}, {"type": "shape", "id": "Dot", "name": "Dot",
            "shape": {"type": "rect", "x": 0, "y": 0, "width": 1, "height": 1}, "style": {"fill": rgb(0, 0, 0)}}
    ]}});
    let r = json!({"type": "syncRef", "syncSourceGuid": "LOOP", "guid": "first"});
    let z = xd_zip(&[board("A", (0.0, 0.0, 10.0, 10.0), vec![r])], vec![], vec![def], &[]);
    let (_, warnings) = import(&z, "f").unwrap();
    assert!(warnings.iter().any(|w| w.contains("nested deeper")), "{warnings:?}");
}

#[test]
fn unsupported_features_import_statically_with_notes() {
    let mut grid = group("Grid", None, vec![rect("Cell1", 0.0, 0.0, 10.0, 10.0), rect("Cell2", 20.0, 0.0, 10.0, 10.0)]);
    grid["meta"] = json!({"ux": {"repeatGrid": {"columns": 2, "rows": 1, "cellWidth": 10, "cellHeight": 10, "paddingX": 10, "paddingY": 0}}});
    let mut stack = group("Stack", None, vec![rect("S1", 0.0, 50.0, 10.0, 10.0)]);
    stack["meta"] = json!({"ux": {"contentStackType": "vertical"}});
    let mut states = group("Btn", None, vec![rect("S2", 0.0, 80.0, 10.0, 10.0)]);
    states["meta"] = json!({"ux": {"states": [{"name": "Hover"}], "stateId": "x"}});
    let (doc, warnings) = one_board(vec![grid, stack, states]);
    for name in ["Cell1", "Cell2", "S1", "S2"] {
        node(&doc, name);
    }
    for note in ["repeat grids", "stacks and padding", "component states"] {
        assert!(warnings.iter().any(|w| w.contains(note)), "{note}: {warnings:?}");
    }
}

#[test]
fn prototype_interactions_are_noted() {
    let mut r = rect("Linked", 0.0, 0.0, 5.0, 5.0);
    r["meta"] = json!({"ux": {"interactions": [{"triggerEvent": "click"}]}});
    let z = xd_zip(&[board("A", (0.0, 0.0, 10.0, 10.0), vec![r])], vec![], vec![], &[]);
    let (_, warnings) = import(&z, "f").unwrap();
    assert!(warnings.iter().any(|w| w.contains("prototype interactions")), "{warnings:?}");
}

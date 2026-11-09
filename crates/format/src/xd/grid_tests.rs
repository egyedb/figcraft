//! `.xd` repeat grids: a grid auto layout of cell frames, the first cell a component the others
//! are instances of, every layer where XD draws it.

use figcraft_doc::geom::Point;
use figcraft_doc::{LayoutMode, NodeKind, Sizing};
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

/// A cell at `x`, `y`: a card rectangle and a label `text`.
fn cell(i: usize, x: f64, y: f64, text: &str) -> Value {
    json!({"type": "group", "id": format!("C{i}"), "name": "", "transform": translate(x, y), "meta": {"ux": {"disableNaming": true}}, "group": {"children": [
        {"type": "shape", "id": format!("R{i}"), "name": "Card", "style": {"fill": rgb(240, 240, 240)}, "shape": {"type": "rect", "x": 0, "y": 0, "width": 100, "height": 60}},
        point_text(&format!("T{i}"), 10.0, 30.0, text),
    ]}})
}

/// A repeat grid of `cols` × `rows` cards of 100 × 60, `px`, `py` apart, starting at 20, 40.
fn grid_of(cols: usize, rows: usize, px: f64, py: f64) -> Value {
    let cells: Vec<Value> = (0..cols * rows)
        .map(|i| cell(i, 20.0 + (i % cols) as f64 * (100.0 + px), 40.0 + (i / cols) as f64 * (60.0 + py), &format!("Item {i}")))
        .collect();
    json!({"type": "group", "id": "G", "name": "Cards", "meta": {"ux": {"repeatGrid": {
        "width": cols as f64 * 100.0 + (cols - 1) as f64 * px, "height": rows as f64 * 60.0 + (rows - 1) as f64 * py,
        "paddingX": px, "paddingY": py, "cellWidth": 100, "cellHeight": 60, "columns": cols, "rows": rows}}}, "group": {"children": cells}})
}

fn import_board(layers: Vec<Value>) -> (Document, Vec<String>) {
    import(&xd_zip(&[board("Board", (0.0, 0.0, 800.0, 800.0), layers)], vec![], vec![], &[]), "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

fn at(doc: &Document, id: figcraft_doc::NodeId) -> Point {
    doc.world_transform(id) * Point::ZERO
}

#[test]
fn repeat_grids_become_grid_auto_layout_of_cells() {
    let (doc, warnings) = import_board(vec![grid_of(3, 2, 20.0, 10.0)]);
    let g = node(&doc, "Cards");
    assert!(matches!(g.kind, NodeKind::Frame), "{:?} {warnings:?}", g.kind);
    let l = g.layout.as_ref().unwrap_or_else(|| panic!("{warnings:?}"));
    assert_eq!(l.mode, LayoutMode::Grid);
    assert_eq!((l.grid_columns, l.grid_rows, l.gap, l.cross_gap), (3, 2, 20.0, 10.0));
    assert!(l.column_tracks.iter().all(|t| t.sizing == Sizing::Fixed && t.value == 100.0), "{:?}", l.column_tracks);
    assert!(l.row_tracks.iter().all(|t| t.sizing == Sizing::Fixed && t.value == 60.0), "{:?}", l.row_tracks);
    assert_eq!(g.children.len(), 6);
    assert_eq!((g.width, g.height), (340.0, 130.0));
    assert_eq!(at(&doc, g.id), Point::new(20.0, 40.0));
    // Every card and label stays where XD draws it.
    for i in 0..6 {
        let card = doc.nodes().filter(|n| n.name == "Card").nth(i).unwrap();
        let p = at(&doc, card.id);
        let expected = Point::new(20.0 + (i % 3) as f64 * 120.0, 40.0 + (i / 3) as f64 * 70.0);
        assert!((p - expected).hypot() < 1e-6, "cell {i}: {p:?} vs {expected:?}");
    }
}

#[test]
fn the_first_cell_is_a_component_the_others_instances_with_their_own_text() {
    let (doc, _) = import_board(vec![grid_of(2, 2, 10.0, 10.0)]);
    let g = node(&doc, "Cards");
    let cells: Vec<&figcraft_doc::Node> = g.children.iter().map(|c| doc.node(*c).unwrap()).collect();
    assert!(matches!(cells[0].kind, NodeKind::Component { .. }), "{:?}", cells[0].kind);
    assert_eq!(cells[0].name, "Cards cell");
    for c in &cells[1..] {
        assert!(matches!(c.kind, NodeKind::Instance { component, .. } if component == cells[0].id), "{:?}", c.kind);
    }
    let texts: Vec<String> = doc
        .nodes()
        .filter_map(|n| match &n.kind {
            NodeKind::Text(t) => Some(t.characters.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, ["Item 0", "Item 1", "Item 2", "Item 3"]);
}

#[test]
fn overlapping_cells_step_by_less_than_a_cell() {
    // Cells 100 wide, 20 apart backwards: they step by 80 and overlap.
    let (doc, _) = import_board(vec![grid_of(3, 1, -20.0, 0.0)]);
    let g = node(&doc, "Cards");
    let l = g.layout.as_ref().unwrap();
    assert_eq!(l.gap, 0.0);
    assert!(l.column_tracks.iter().all(|t| t.value == 80.0));
    let xs: Vec<f64> = doc.nodes().filter(|n| n.name == "Card").map(|c| at(&doc, c.id).x).collect();
    assert_eq!(xs, [20.0, 100.0, 180.0]);
}

#[test]
fn cells_that_differ_stay_frames() {
    let mut g = grid_of(2, 1, 10.0, 0.0);
    // The second cell lost its label.
    g["group"]["children"][1]["group"]["children"] = json!([{"type": "shape", "id": "X", "name": "Card", "style": {"fill": rgb(1, 1, 1)},
        "shape": {"type": "rect", "x": 0, "y": 0, "width": 100, "height": 60}}]);
    let (doc, _) = import_board(vec![g]);
    let grid = node(&doc, "Cards");
    assert!(grid.children.iter().all(|c| matches!(doc.node(*c).unwrap().kind, NodeKind::Frame)));
    assert_eq!(grid.layout.as_ref().map(|l| l.mode), Some(LayoutMode::Grid));
}

#[test]
fn grids_inside_components_have_no_cell_components() {
    let def =
        json!({"type": "group", "id": "DEF", "name": "List", "meta": {"ux": {"symbolId": "S"}}, "group": {"children": [grid_of(2, 1, 10.0, 0.0)]}});
    let inst = json!({"type": "group", "id": "I", "name": "Inst", "transform": translate(0.0, 0.0), "meta": {"ux": {"symbolId": "S", "isMaster": false}},
        "group": {"children": [{"type": "syncRef", "syncSourceGuid": "G", "guid": "gG"}]}});
    let z = xd_zip(&[board("Board", (0.0, 0.0, 800.0, 800.0), vec![inst])], vec![], vec![def], &[]);
    let (doc, warnings) = import(&z, "f").unwrap();
    assert!(doc.nodes().filter(|n| matches!(n.kind, NodeKind::Component { .. })).all(|c| c.name == "List"), "{warnings:?}");
    assert!(doc.nodes().filter(|n| n.name == "Cards").all(|g| g.layout.as_ref().map(|l| l.mode) == Some(LayoutMode::Grid)));
}

#[test]
fn incomplete_grids_keep_fixed_places_with_a_note() {
    let mut g = grid_of(2, 2, 10.0, 10.0);
    // Three cells for a grid of four.
    g["group"]["children"].as_array_mut().unwrap().pop();
    let (doc, warnings) = import_board(vec![g]);
    let grid = node(&doc, "Cards");
    assert!(grid.layout.is_none());
    assert!(warnings.iter().any(|w| w.starts_with("1 of 1 repeat grids")), "{warnings:?}");
}

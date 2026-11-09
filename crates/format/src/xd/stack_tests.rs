//! `.xd` stacks and padding: auto layout when it keeps every layer in place, fixed places
//! otherwise.

use figcraft_doc::geom::Point;
use figcraft_doc::{CrossAlign, LayoutMode, NodeKind};
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

fn one_board(layers: Vec<Value>) -> (Document, Vec<String>) {
    import(&xd_zip(&[board("Board", (0.0, 0.0, 800.0, 800.0), layers)], vec![], vec![], &[]), "f").unwrap()
}

fn node<'d>(doc: &'d Document, name: &str) -> &'d figcraft_doc::Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer {name}"))
}

fn at(doc: &Document, name: &str) -> Point {
    doc.world_transform(node(doc, name).id) * Point::ZERO
}

/// A group whose `meta.ux` is `ux`.
fn stack(name: &str, ux: Value, children: Vec<Value>) -> Value {
    let mut g = group(name, None, children);
    g["meta"] = json!({"ux": ux});
    g
}

#[test]
fn an_evenly_spaced_column_becomes_auto_layout() {
    // Listed out of order: the flow follows where they are.
    let s = stack(
        "Col",
        json!({"contentStackType": "vertical"}),
        vec![rect("B", 10.0, 50.0, 40.0, 20.0), rect("A", 10.0, 10.0, 60.0, 30.0), rect("C", 10.0, 80.0, 30.0, 10.0)],
    );
    let (doc, warnings) = one_board(vec![s]);
    let col = node(&doc, "Col");
    assert!(matches!(col.kind, NodeKind::Frame), "{:?} {warnings:?}", col.kind);
    let l = col.layout.as_ref().unwrap_or_else(|| panic!("no layout: {warnings:?}"));
    assert_eq!(l.mode, LayoutMode::Vertical);
    assert_eq!(l.gap, 10.0);
    assert_eq!(l.cross_align, CrossAlign::Min);
    let names: Vec<&str> = col.children.iter().map(|c| doc.node(*c).unwrap().name.as_str()).collect();
    assert_eq!(names, ["A", "B", "C"]);
    // Every layer stays where XD put it.
    for (n, p) in [("A", (10.0, 10.0)), ("B", (10.0, 50.0)), ("C", (10.0, 80.0))] {
        assert_eq!(at(&doc, n), Point::new(p.0, p.1), "{n}");
    }
}

#[test]
fn a_row_centred_across_keeps_its_centre() {
    let s = stack("Row", json!({"contentStackType": "horizontal"}), vec![rect("A", 0.0, 0.0, 20.0, 40.0), rect("B", 30.0, 10.0, 20.0, 20.0)]);
    let (doc, _) = one_board(vec![s]);
    let l = node(&doc, "Row").layout.clone().unwrap();
    assert_eq!((l.mode, l.gap, l.cross_align), (LayoutMode::Horizontal, 10.0, CrossAlign::Center));
    assert_eq!(at(&doc, "B"), Point::new(30.0, 10.0));
}

#[test]
fn uneven_stacks_keep_fixed_places_with_a_note() {
    let s = stack(
        "Uneven",
        json!({"contentStackType": "vertical"}),
        vec![rect("A", 0.0, 0.0, 10.0, 10.0), rect("B", 0.0, 20.0, 10.0, 10.0), rect("C", 0.0, 50.0, 10.0, 10.0)],
    );
    let (doc, warnings) = one_board(vec![s]);
    let g = node(&doc, "Uneven");
    assert!(matches!(g.kind, NodeKind::Group) && g.layout.is_none());
    assert_eq!(at(&doc, "C"), Point::new(0.0, 50.0));
    assert!(warnings.iter().any(|w| w.starts_with("1 of 1 stacks")), "{warnings:?}");
}

#[test]
fn padded_backgrounds_become_padding() {
    // A button: its background 10 px larger than the label on the left and right, 5 above and below.
    let s = stack(
        "Button",
        json!({"contentStackType": "horizontal", "hasContentBackground": true,
        "contentPadding": {"left": 10, "right": 10, "top": 5, "bottom": 5}}),
        vec![rect("Bg", 100.0, 100.0, 70.0, 30.0), rect("Label", 110.0, 105.0, 50.0, 20.0)],
    );
    let (doc, warnings) = one_board(vec![s]);
    let b = node(&doc, "Button");
    let l = b.layout.as_ref().unwrap_or_else(|| panic!("{warnings:?}"));
    // `[top, right, bottom, left]`.
    assert_eq!(l.padding, [5.0, 10.0, 5.0, 10.0]);
    assert_eq!((b.width, b.height), (70.0, 30.0));
    let bg = node(&doc, "Bg");
    assert!(bg.child_layout.absolute);
    assert_eq!(doc.node(b.children[0]).unwrap().name, "Bg");
    assert_eq!(at(&doc, "Label"), Point::new(110.0, 105.0));
    assert_eq!(at(&doc, "Bg"), Point::new(100.0, 100.0));
}

#[test]
fn padding_without_a_stack_lays_out_its_one_layer() {
    let s = stack(
        "Chip",
        json!({"hasContentBackground": true, "contentPadding": {"left": 4, "right": 4, "top": 2, "bottom": 2}}),
        vec![rect("Bg", 0.0, 0.0, 28.0, 14.0), rect("Dot", 4.0, 2.0, 20.0, 10.0)],
    );
    let (doc, _) = one_board(vec![s]);
    let l = node(&doc, "Chip").layout.clone().unwrap();
    assert_eq!(l.padding, [2.0, 4.0, 2.0, 4.0]);
    assert_eq!(at(&doc, "Dot"), Point::new(4.0, 2.0));
}

#[test]
fn hidden_layers_take_no_room() {
    let mut hidden = rect("Hidden", 0.0, 300.0, 10.0, 10.0);
    hidden["visible"] = json!(false);
    let s = stack("Col", json!({"contentStackType": "vertical"}), vec![rect("A", 0.0, 0.0, 10.0, 10.0), hidden, rect("B", 0.0, 15.0, 10.0, 10.0)]);
    let (doc, _) = one_board(vec![s]);
    assert_eq!(node(&doc, "Col").layout.as_ref().map(|l| l.gap), Some(5.0));
    assert_eq!(at(&doc, "B"), Point::new(0.0, 15.0));
}

#[test]
fn xds_own_gaps_win_over_guessed_text_widths() {
    // A row of a label and an icon: the label's width is only known once FigCraft measures it,
    // so the boxes can't show the 8 px XD keeps after it; the stack's gap says so.
    let mut label = point_text("Label", 0.0, 20.0, "Log out");
    label["meta"]["ux"]["contentStackMarginAfter"] = json!(8);
    // The icon centred on the label's 24 px line (without the font's metrics, the import's own).
    let (a, d) = super::text::FALLBACK_METRICS;
    let top = 20.0 - ((24.0 - (a + d) * 20.0) / 2.0 + a * 20.0);
    let s = stack("Row", json!({"contentStackType": "horizontal"}), vec![label, rect("Icon", 70.0, top + 4.0, 16.0, 16.0)]);
    let (doc, warnings) = one_board(vec![s]);
    let l = node(&doc, "Row").layout.clone().unwrap_or_else(|| panic!("{warnings:?}"));
    assert_eq!(l.gap, 8.0);
}

//! `.xd` text: fonts, sizes, colours, alignment, line heights, styled runs and where the box goes
//! so the first baseline lands where XD draws it.

use figcraft_doc::geom::Point;
use figcraft_doc::{LineHeight, NodeKind, TextAlignH, TextAutoResize, TextData};
use serde_json::json;

use super::fixture::*;
use super::tests::origin_on_page;
use super::*;

/// Fonts that say every face has an ascent of 0.8 and a descent of 0.2 of its size.
fn fake_fonts() -> Fonts {
    Fonts { face: None, metrics: Some(Box::new(|_: &str, _: &str| Some((0.8, 0.2)))) }
}

fn import_one(layer: serde_json::Value) -> (Document, Vec<String>) {
    let z = xd_zip(&[board("A", (0.0, 0.0, 800.0, 600.0), vec![layer])], vec![], vec![], &[]);
    import_with_fonts(&z, "f", fake_fonts()).unwrap()
}

fn text<'d>(doc: &'d Document, name: &str) -> (&'d figcraft_doc::Node, &'d TextData) {
    let n = doc.nodes().find(|n| n.name == name && matches!(n.kind, NodeKind::Text(_))).unwrap();
    let NodeKind::Text(t) = &n.kind else { panic!("not text") };
    (n, t)
}

#[test]
fn point_text_puts_its_first_baseline_on_xds() {
    // 20 px, line height 24: FigCraft's first baseline is (24 - 20) / 2 + 16 = 18 below the top.
    let (doc, _) = import_one(point_text("Hello", 100.0, 200.0, "Hello"));
    let (n, t) = text(&doc, "Hello");
    assert_eq!(t.characters, "Hello");
    assert_eq!(t.style.font_family, "Arial");
    assert_eq!(t.style.font_size, 20.0);
    assert_eq!(t.style.line_height, LineHeight::Pixels(24.0));
    assert_eq!(t.auto_resize, TextAutoResize::WidthAndHeight);
    let top = origin_on_page(&doc, "Hello");
    assert!((top.x - 100.0).abs() < 1e-9 && (top.y - 182.0).abs() < 1e-9, "{top:?}");
    assert!(n.width > 0.0);
}

#[test]
fn area_text_keeps_its_box_width_and_alignment() {
    let mut t = point_text("Area", 10.0, 20.0, "Centred words");
    t["text"]["frame"] = json!({"type": "area", "width": 300, "height": 50});
    t["text"]["paragraphs"] = json!([{"lines": [[{"x": 90, "y": 18, "from": 0, "to": 13}]]}]);
    t["style"]["textAttributes"] = json!({"lineHeight": 24, "paragraphAlign": "center"});
    let (doc, _) = import_one(t);
    let (n, td) = text(&doc, "Area");
    assert_eq!(td.align_h, TextAlignH::Center);
    assert_eq!(td.auto_resize, TextAutoResize::None);
    assert_eq!(n.width, 300.0);
    // The top: the frame's top plus XD's baseline (18) less FigCraft's (18).
    let top = origin_on_page(&doc, "Area");
    assert!((top.y - 20.0).abs() < 1e-9, "{top:?}");
}

#[test]
fn centred_point_text_is_centred_on_its_point() {
    let mut t = point_text("Mid", 400.0, 100.0, "Save");
    t["style"]["textAttributes"] = json!({"lineHeight": 24, "paragraphAlign": "center"});
    t["text"]["paragraphs"] = json!([{"lines": [[{"x": -20, "y": 0, "from": 0, "to": 4}]]}]);
    let (doc, _) = import_one(t);
    let (n, td) = text(&doc, "Mid");
    assert_eq!(td.align_h, TextAlignH::Center);
    let left = origin_on_page(&doc, "Mid").x;
    assert!((left + n.width / 2.0 - 400.0).abs() < 1e-6, "left {left}, width {}", n.width);
}

#[test]
fn letter_spacing_is_in_thousandths_of_an_em() {
    let mut t = point_text("Spaced", 0.0, 50.0, "ABC");
    t["style"]["textAttributes"] = json!({"lineHeight": 24, "letterSpacing": 50});
    let (doc, _) = import_one(t);
    let (_, td) = text(&doc, "Spaced");
    assert_eq!(td.style.letter_spacing, figcraft_doc::LetterSpacing::Pixels(1.0));
}

#[test]
fn ranged_styles_become_runs() {
    let mut t = point_text("Runs", 0.0, 50.0, "plain bold");
    t["meta"]["ux"]["rangedStyles"] = json!([
        {"length": 6, "fontFamily": "Arial", "fontStyle": "Regular", "postscriptName": "ArialMT", "fontSize": 20, "charSpacing": 0,
            "underline": false, "strikethrough": false, "textTransform": "none", "textScript": "none", "fill": {"value": 4278190080u32}},
        {"length": 4, "fontFamily": "Arial", "fontStyle": "Bold", "postscriptName": "Arial-BoldMT", "fontSize": 20, "charSpacing": 0,
            "underline": true, "strikethrough": false, "textTransform": "uppercase", "textScript": "none", "fill": {"value": 4294901760u32}},
    ]);
    let (doc, _) = import_one(t);
    let (_, td) = text(&doc, "Runs");
    let bold = td.runs.iter().find(|r| r.style.font_style == "Bold").unwrap_or_else(|| panic!("{:?}", td.runs));
    assert_eq!((bold.start, bold.end), (6, 10));
    assert_eq!(bold.style.decoration, figcraft_doc::TextDecoration::Underline);
    assert_eq!(bold.style.case, figcraft_doc::TextCase::Upper);
    assert!(bold.style.fills.is_some());
}

#[test]
fn postscript_families_take_the_names_xd_resolved() {
    // XD often writes the PostScript name as the family; its laid-out lines say the real one.
    let mut t = point_text("Light", 0.0, 50.0, "Hi");
    t["style"]["font"] = json!({"postscriptName": "Montserrat-Light", "family": "Montserrat-Light", "size": 14});
    t["meta"]["ux"]["rangedStyles"] = json!([]);
    t["text"]["paragraphs"] = json!([{"lines": [[{"x": 0, "y": 0, "from": 0, "to": 2,
        "style": {"font": {"postscriptName": "Montserrat-Light", "family": "Montserrat", "style": "Light"}}}]]}]);
    let (doc, _) = import_one(t);
    let (_, td) = text(&doc, "Light");
    assert_eq!((td.style.font_family.as_str(), td.style.font_style.as_str()), ("Montserrat", "Light"));
}

#[test]
fn without_any_help_postscript_names_are_read() {
    let mut t = point_text("Ps", 0.0, 50.0, "Hi");
    t["style"]["font"] = json!({"postscriptName": "WorkSans-Bold", "family": "WorkSans-Bold", "size": 16});
    t["meta"]["ux"]["rangedStyles"] = json!([]);
    let (doc, _) = import_one(t);
    let (_, td) = text(&doc, "Ps");
    assert_eq!((td.style.font_family.as_str(), td.style.font_style.as_str()), ("Work Sans", "Bold"));
}

#[test]
fn installed_faces_win() {
    let mut fonts = fake_fonts();
    fonts.face = Some(Box::new(|ps: &str| (ps == "ArialMT").then(|| ("Arial Installed".to_string(), "Regular".to_string()))));
    let z = xd_zip(&[board("A", (0.0, 0.0, 800.0, 600.0), vec![point_text("I", 0.0, 50.0, "x")])], vec![], vec![], &[]);
    let (doc, _) = import_with_fonts(&z, "f", fonts).unwrap();
    assert_eq!(text(&doc, "I").1.style.font_family, "Arial Installed");
}

#[test]
fn line_height_falls_back_to_xds_line_spacing() {
    let mut t = point_text("Two", 0.0, 50.0, "a\nb");
    t["style"]["textAttributes"] = json!({});
    t["text"]["paragraphs"] = json!([{"lines": [[{"x": 0, "y": 0, "from": 0, "to": 1}], [{"x": 0, "y": 14, "from": 2, "to": 3}]]}]);
    let (doc, _) = import_one(t);
    assert_eq!(text(&doc, "Two").1.style.line_height, LineHeight::Pixels(14.0));
    let _ = Point::ZERO;
}

#[test]
fn control_characters_xd_keeps_are_invisible_and_keep_run_ranges() {
    let mut t = point_text("Code", 0.0, 50.0, "code:\u{3}GO");
    t["meta"]["ux"]["rangedStyles"] = json!([
        {"length": 6, "fontFamily": "Arial", "fontStyle": "Regular", "postscriptName": "ArialMT", "fontSize": 20},
        {"length": 2, "fontFamily": "Arial", "fontStyle": "Bold", "postscriptName": "Arial-BoldMT", "fontSize": 20},
    ]);
    let (doc, _) = import_one(t);
    let (_, td) = text(&doc, "Code");
    assert_eq!(td.characters, "code:\u{2060}GO");
    let bold = td.runs.iter().find(|r| r.style.font_style == "Bold").unwrap();
    assert_eq!((bold.start, bold.end), (6, 8));
}

#[test]
fn area_text_xd_laid_out_only_in_part_is_clipped_to_its_box() {
    let mut t = point_text("Long", 10.0, 20.0, "first line second line third line");
    t["text"]["frame"] = json!({"type": "area", "width": 120, "height": 50});
    // XD laid out the first two lines only: the third doesn't fit the box.
    t["text"]["paragraphs"] = json!([{"lines": [[{"x": 0, "y": 18, "from": 0, "to": 11}], [{"x": 0, "y": 42, "from": 11, "to": 23}]]}]);
    let (doc, warnings) = import_one(t);
    let (n, _) = text(&doc, "Long");
    let parent = doc.node(n.parent.unwrap()).unwrap();
    assert!(matches!(parent.kind, NodeKind::Frame) && parent.clip, "{:?}", parent.kind);
    // Down to the end of the second line: 42 - 18 + 24 (its baseline, less FigCraft's first, plus a line).
    assert_eq!((parent.width, parent.height), (120.0, 48.0));
    let corner = doc.world_transform(parent.id) * Point::ZERO;
    assert_eq!((corner.x, corner.y), (10.0, 20.0));
    assert!(warnings.iter().any(|w| w.contains("overflows its fixed box")), "{warnings:?}");
    // A box that holds all its text isn't wrapped.
    let mut fits = point_text("Fits", 10.0, 20.0, "short");
    fits["text"]["frame"] = json!({"type": "area", "width": 120, "height": 50});
    fits["text"]["paragraphs"] = json!([{"lines": [[{"x": 0, "y": 18, "from": 0, "to": 5}]]}]);
    let (doc, _) = import_one(fits);
    let (n, _) = text(&doc, "Fits");
    assert_eq!(doc.node(n.parent.unwrap()).unwrap().name, "A");
}

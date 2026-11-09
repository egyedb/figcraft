//! `.xd` document assets: colours, gradients and character styles become styles, and layers
//! that name them by `linkId` link to them.

use figcraft_doc::StyleKind;
use serde_json::{Value, json};

use super::fixture::*;
use super::*;

fn color(id: &str, name: Option<&str>, rgb: [u8; 3]) -> Value {
    json!({"id": id, "type": "application/vnd.adobe.element.color+dcx", "name": name.unwrap_or(""), "displayName": null,
        "representations": [{"type": "application/vnd.adobe.xdcolor+json", "content": {"mode": "RGB", "value": {"r": rgb[0], "g": rgb[1], "b": rgb[2]}}}]})
}

fn char_style(id: &str, name: &str, display: Option<&str>) -> Value {
    json!({"id": id, "type": "application/vnd.adobe.element.characterstyle+dcx", "name": name, "displayName": display,
        "representations": [{"type": "application/vnd.adobe.characterstyle+json", "content": {
            "fontFamily": "Inter", "fontStyle": "Bold", "postscriptName": "Inter-Bold", "fontSize": 24, "fontColor": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}},
            "charSpacing": 0, "lineSpacing": 30, "linkId": "", "underline": false, "strikethrough": false, "textTransform": "uppercase", "textScript": "none"}}]})
}

fn import_with(layers: Vec<Value>, ux: Value) -> (Document, Vec<String>) {
    let z = xd_zip_full(&[board("Board", (0.0, 0.0, 400.0, 400.0), layers)], vec![], vec![], ux, &[]);
    import(&z, "f").unwrap()
}

fn style<'d>(doc: &'d Document, id: &str) -> &'d figcraft_doc::Style {
    doc.styles.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no style {id}: {:?}", doc.styles.iter().map(|s| &s.id).collect::<Vec<_>>()))
}

#[test]
fn colours_become_paint_styles_named_or_by_their_hex() {
    let lib = json!({"documentLibrary": {"elements": [color("C1", Some("Brand"), [10, 20, 30]), color("C2", None, [255, 0, 128])]}});
    let (doc, _) = import_with(vec![], lib);
    assert_eq!(style(&doc, "C1").name, "Brand");
    assert_eq!(style(&doc, "C2").name, "#FF0080");
    assert!(matches!(style(&doc, "C1").kind, StyleKind::Paint { .. }));
}

#[test]
fn layers_link_to_the_colours_they_name() {
    let lib = json!({"documentLibrary": {"elements": [color("FILL", Some("Brand"), [10, 20, 30]), color("LINE", Some("Line"), [0, 0, 0])]}});
    let mut r = rect("R", 0.0, 0.0, 10.0, 10.0);
    r["style"] = json!({
        "fill": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 10, "g": 20, "b": 30}, "linkId": "FILL"}},
        "stroke": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}, "linkId": "LINE"}, "width": 1},
    });
    let mut plain = rect("Plain", 20.0, 0.0, 10.0, 10.0);
    plain["style"]["fill"]["color"]["linkId"] = json!("NOT-AN-ASSET");
    let (doc, _) = import_with(vec![r, plain], lib);
    let n = doc.nodes().find(|n| n.name == "R").unwrap();
    assert_eq!(n.styles.fill.as_deref(), Some("FILL"));
    assert_eq!(n.styles.stroke.as_deref(), Some("LINE"));
    assert!(doc.nodes().find(|n| n.name == "Plain").unwrap().styles.fill.is_none());
}

#[test]
fn character_styles_become_text_styles_and_texts_link_to_them() {
    let lib = json!({"documentLibrary": {"elements": [char_style("H1", "Heading", Some("Titles/Heading")), char_style("BODY", "", None)]}});
    let mut t = point_text("Title", 0.0, 50.0, "Hello");
    t["meta"]["ux"]["rangedStyles"][0]["linkId"] = json!("H1");
    let mut mixed = point_text("Mixed", 0.0, 100.0, "ab");
    mixed["meta"]["ux"]["rangedStyles"] = json!([{"length": 1, "linkId": "H1"}, {"length": 1, "linkId": "BODY"}]);
    let (doc, _) = import_with(vec![t, mixed], lib);
    let h1 = style(&doc, "H1");
    assert_eq!(h1.name, "Titles/Heading");
    let StyleKind::Text { style: ts, .. } = &h1.kind else { panic!() };
    assert_eq!((ts.font_family.as_str(), ts.font_style.as_str(), ts.font_size), ("Inter", "Bold", 24.0));
    assert_eq!(ts.line_height, figcraft_doc::LineHeight::Pixels(30.0));
    assert_eq!(ts.case, figcraft_doc::TextCase::Upper);
    // An unnamed one is named by its font and size.
    assert_eq!(style(&doc, "BODY").name, "Inter Bold 24");
    let text_of = |n: &str| doc.nodes().find(|x| x.name == n && matches!(x.kind, figcraft_doc::NodeKind::Text(_))).unwrap();
    assert_eq!(text_of("Title").styles.text.as_deref(), Some("H1"));
    // Runs naming different styles: the layer links to none.
    assert!(text_of("Mixed").styles.text.is_none());
}

#[test]
fn gradients_and_old_swatches_become_paint_styles() {
    let grad = json!({"id": "G", "type": "application/vnd.adobe.element.gradient+dcx", "name": "",
        "representations": [{"type": "application/vnd.adobe.xdlineargradient+json", "content": [
            {"stop": 0, "color": {"mode": "RGB", "value": {"r": 255, "g": 255, "b": 255}}}, {"stop": 1, "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}}]}]});
    let lib = json!({"documentLibrary": {"elements": [grad]}, "colorSwatches": [{"mode": "RGB", "value": {"r": 72, "g": 128, "b": 255}}]});
    let (doc, _) = import_with(vec![], lib);
    let StyleKind::Paint { paints } = &style(&doc, "G").kind else { panic!() };
    assert!(matches!(paints[0].kind, figcraft_doc::color::PaintKind::Gradient(_)));
    assert_eq!(style(&doc, "xd-swatch:0").name, "#4880FF");
}

#[test]
fn junk_assets_are_skipped() {
    let lib = json!({"documentLibrary": {"elements": [
        {"id": "X", "type": "application/vnd.adobe.element.color+dcx", "representations": "nope"},
        {"type": "application/vnd.adobe.element.color+dcx"},
        {"id": "Y", "type": "application/vnd.adobe.element.characterstyle+dcx", "representations": [{"content": {"fontSize": "big"}}]},
        {"id": "Z", "type": "application/vnd.adobe.element.audio+dcx"},
        7,
    ]}, "colorSwatches": [null, {"value": 3}]});
    let (doc, _) = import_with(vec![], lib);
    assert!(doc.styles.iter().all(|s| s.id != "X" && s.id != "Z"));
}

#[test]
fn runs_naming_different_character_styles_link_each_to_theirs() {
    let lib = json!({"documentLibrary": {"elements": [char_style("H1", "Heading", None), char_style("BODY", "Body", None)]}});
    let mut mixed = point_text("Mixed", 0.0, 100.0, "abcd");
    mixed["meta"]["ux"]["rangedStyles"] = json!([
        {"length": 2, "linkId": "H1", "fontFamily": "Arial", "fontStyle": "Regular", "postscriptName": "ArialMT", "fontSize": 20},
        {"length": 2, "linkId": "BODY", "fontFamily": "Arial", "fontStyle": "Bold", "postscriptName": "Arial-BoldMT", "fontSize": 20},
    ]);
    let (doc, _) = import_with(vec![mixed], lib);
    let n = doc.nodes().find(|x| x.name == "Mixed" && matches!(x.kind, figcraft_doc::NodeKind::Text(_))).unwrap();
    let figcraft_doc::NodeKind::Text(td) = &n.kind else { panic!() };
    assert!(n.styles.text.is_none());
    let linked: Vec<(usize, usize, Option<&str>)> = td.runs.iter().map(|r| (r.start, r.end, r.style.style_id.as_deref())).collect();
    assert_eq!(linked, [(0, 2, Some("H1")), (2, 4, Some("BODY"))]);
}

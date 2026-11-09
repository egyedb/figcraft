//! `.xd` files through the file commands. The importer's own tests (fixtures built in code) are
//! in `figcraft_format::xd`.

use std::io::Write;

use base64::Engine;
use serde_json::{Value, json};

use figcraft_doc::NodeKind;

use crate::Session;

/// A minimal `.xd` zip: one artboard `A` at 0, 0 of 400 × 300 holding `layers`.
fn xd(layers: &[Value]) -> Vec<u8> {
    xd_with(layers, &[])
}

/// [`xd`] with component definitions `symbols`.
fn xd_with(layers: &[Value], symbols: &[Value]) -> Vec<u8> {
    let resources = json!({"version": "1.5.0", "children": [], "resources": {"meta": {"ux": {"symbols": symbols}}},
        "artboards": {"A": {"name": "Home", "x": 0, "y": 0, "width": 400, "height": 300}}});
    let manifest = json!({"children": [{"path": "artwork", "children": [{"path": "artboard-A", "name": "Home"}]}]});
    let board = json!({"version": "1.5.0", "children": [{"type": "artboard", "id": "A", "artboard": {"children": layers}}]});
    let mut out = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let o = zip::write::SimpleFileOptions::default();
        for (name, body) in [
            ("mimetype", figcraft_format::xd::MIMETYPE.to_string()),
            ("manifest", manifest.to_string()),
            ("resources/graphics/graphicContent.agc", resources.to_string()),
            ("artwork/artboard-A/graphics/graphicContent.agc", board.to_string()),
        ] {
            z.start_file(name, o).unwrap();
            z.write_all(body.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }
    out
}

/// A centred point text on its point `x`, `y` (the first baseline).
fn centred_text(x: f64, y: f64, s: &str) -> Value {
    json!({"type": "text", "id": "T", "name": "Label", "transform": {"a": 1, "b": 0, "c": 0, "d": 1, "tx": x, "ty": y},
        "style": {"fill": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 0, "g": 0, "b": 0}}},
            "font": {"postscriptName": "Inter-Regular", "family": "Inter", "style": "Regular", "size": 16},
            "textAttributes": {"lineHeight": 20, "paragraphAlign": "center"}},
        "text": {"frame": {"type": "positioned"}, "paragraphs": [{"lines": [[{"x": -50, "y": 0, "from": 0, "to": s.len()}]]}], "rawText": s}})
}

#[test]
fn xd_data_opens_with_a_summary_and_no_path() {
    let data = base64::engine::general_purpose::STANDARD.encode(xd(&[]));
    let mut s = Session::new();
    let r = s.execute("file.open", &json!({"data": data, "name": "design.xd"})).unwrap();
    assert!(r.get("summary").is_some());
    let doc = s.doc().unwrap();
    let page = doc.first_page().unwrap();
    let board = doc.node(doc.node(page).unwrap().children[0]).unwrap();
    assert_eq!(board.name, "Home");
    assert!(matches!(board.kind, NodeKind::Frame));
    assert!(s.docs[s.active].path.is_none());
}

#[test]
fn centred_point_text_stays_centred_after_measuring() {
    let data = base64::engine::general_purpose::STANDARD.encode(xd(&[centred_text(200.0, 100.0, "Hello there")]));
    let mut s = Session::new();
    s.execute("file.open", &json!({"data": data, "name": "t.xd"})).unwrap();
    let doc = s.doc().unwrap();
    let t = doc.nodes().find(|n| n.name == "Label").unwrap();
    let left = (doc.world_transform(t.id) * figcraft_doc::geom::Point::ZERO).x;
    assert!((left + t.width / 2.0 - 200.0).abs() < 1e-6, "left {left}, width {}", t.width);
}

#[test]
fn damaged_xd_files_fail_cleanly() {
    let mut broken = xd(&[]);
    broken.truncate(broken.len() / 2);
    let data = base64::engine::general_purpose::STANDARD.encode(broken);
    let mut s = Session::new();
    // Not recognised as XD any more: another reader refuses it, without a panic.
    assert!(s.execute("file.open", &json!({"data": data, "name": "x.xd"})).is_err());
}

/// A rectangle layer named `name` with id `id`, filled `rgb`.
fn face(id: &str, rgb: [u8; 3]) -> Value {
    json!({"type": "shape", "id": id, "name": "Face", "style": {"fill": {"type": "solid", "color": {"mode": "RGB", "value": {"r": rgb[0], "g": rgb[1], "b": rgb[2]}}}},
        "shape": {"type": "rect", "x": 0, "y": 0, "width": 80, "height": 30}})
}

#[test]
fn xd_instances_link_to_their_main_with_overrides() {
    let def =
        json!({"type": "group", "id": "DEF", "name": "Button", "meta": {"ux": {"symbolId": "S"}}, "group": {"children": [face("SRC", [0, 200, 0])]}});
    let group = |id: &str, x: f64, master: bool, child: Value| {
        json!({"type": "group", "id": id, "name": id, "transform": {"a": 1, "b": 0, "c": 0, "d": 1, "tx": x, "ty": 10},
            "meta": {"ux": {"symbolId": "S", "isMaster": master}}, "group": {"children": [child]}})
    };
    let reference = |guid: &str| json!({"type": "syncRef", "syncSourceGuid": "SRC", "guid": guid});
    let layers = [
        group("Main", 0.0, true, reference("g0")),
        group("Same", 100.0, false, reference("g1")),
        // XD writes an overridden layer out in full, under the id the instance's syncMap gives it.
        group("Red", 200.0, false, face("g2", [255, 0, 0])),
    ];
    let data = base64::engine::general_purpose::STANDARD.encode(xd_with(&layers, &[def]));
    let mut s = Session::new();
    s.execute("file.open", &json!({"data": data, "name": "c.xd"})).unwrap();
    let doc = s.doc().unwrap();
    let by_name = |n: &str| doc.nodes().find(|x| x.name == n).unwrap();
    let main = by_name("Main");
    assert!(matches!(main.kind, NodeKind::Component { .. }));
    let main_face = main.children[0];
    for (name, overridden) in [("Same", false), ("Red", true)] {
        let inst = by_name(name);
        assert!(matches!(inst.kind, NodeKind::Instance { component, .. } if component == main.id), "{name}");
        let f = doc.node(inst.children[0]).unwrap();
        assert_eq!(f.source, Some(main_face), "{name}");
        assert_eq!(f.overrides.contains("fills"), overridden, "{name}: {:?}", f.overrides);
    }
}

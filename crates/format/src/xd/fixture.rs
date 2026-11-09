//! Synthetic `.xd` files for the tests, written from scratch: a `mimetype`, a `manifest` listing
//! the artboards, one `graphicContent.agc` per artboard, the resources (artboard places,
//! components) and any images. Layers are built with small helpers.

use std::io::Write;

use serde_json::{Value, json};

/// One artboard of a fixture: its id, name, place and size, and its layers.
pub(crate) struct Board {
    pub id: &'static str,
    pub name: &'static str,
    pub rect: (f64, f64, f64, f64),
    pub layers: Vec<Value>,
}

pub(crate) fn board(id: &'static str, rect: (f64, f64, f64, f64), layers: Vec<Value>) -> Board {
    Board { id, name: id, rect, layers }
}

/// An `.xd` zip of `boards`, loose `pasteboard` layers, component `symbols` and other `files`.
pub(crate) fn xd_zip(boards: &[Board], pasteboard: Vec<Value>, symbols: Vec<Value>, files: &[(&str, &[u8])]) -> Vec<u8> {
    xd_zip_full(boards, pasteboard, symbols, json!({}), files)
}

/// [`xd_zip`] with more fields in the resources' `meta.ux` (`documentLibrary`, `colorSwatches`).
pub(crate) fn xd_zip_full(boards: &[Board], pasteboard: Vec<Value>, symbols: Vec<Value>, ux: Value, files: &[(&str, &[u8])]) -> Vec<u8> {
    let artboards: serde_json::Map<String, Value> = boards
        .iter()
        .map(|b| (b.id.to_string(), json!({"name": b.name, "x": b.rect.0, "y": b.rect.1, "width": b.rect.2, "height": b.rect.3})))
        .collect();
    let resources = json!({
        "version": "1.5.0", "children": [],
        "resources": {"meta": {"ux": {"symbols": symbols}}, "gradients": {}, "clipPaths": {}},
        "artboards": artboards,
    });
    let mut resources = resources;
    for (k, v) in ux.as_object().into_iter().flatten() {
        resources["resources"]["meta"]["ux"][k] = v.clone();
    }
    let manifest = json!({
        "id": "M",
        "children": [{"path": "artwork", "name": "artwork", "children":
            std::iter::once(json!({"path": "pasteboard", "name": "pasteboard"}))
                .chain(boards.iter().map(|b| json!({"path": format!("artboard-{}", b.id), "name": b.name})))
                .collect::<Vec<_>>()
        }],
    });
    let mut members: Vec<(String, Vec<u8>)> = vec![
        ("mimetype".into(), super::MIMETYPE.as_bytes().to_vec()),
        ("manifest".into(), manifest.to_string().into_bytes()),
        ("resources/graphics/graphicContent.agc".into(), resources.to_string().into_bytes()),
        ("artwork/pasteboard/graphics/graphicContent.agc".into(), json!({"version": "1.5.0", "children": pasteboard}).to_string().into_bytes()),
        ("interactions/interactions.json".into(), br#"{"version":"1.0","interactions":{}}"#.to_vec()),
    ];
    for b in boards {
        let content = json!({"version": "1.5.0", "children": [{
            "type": "artboard", "id": b.id,
            "style": {"fill": {"type": "solid", "color": {"mode": "RGB", "value": {"r": 255, "g": 255, "b": 255}}}},
            "artboard": {"children": b.layers},
        }]});
        members.push((format!("artwork/artboard-{}/graphics/graphicContent.agc", b.id), content.to_string().into_bytes()));
    }
    for (name, bytes) in files {
        members.push(((*name).to_string(), bytes.to_vec()));
    }
    zip_of(&members)
}

/// `z` without its member `name`.
pub(crate) fn without(z: &[u8], name: &str) -> Vec<u8> {
    let mut a = zip::ZipArchive::new(std::io::Cursor::new(z)).unwrap();
    let mut members = Vec::new();
    for i in 0..a.len() {
        let mut f = a.by_index(i).unwrap();
        if f.name() == name {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut f, &mut bytes).unwrap();
        members.push((f.name().to_string(), bytes));
    }
    zip_of(&members)
}

/// A zip of `members`, in order.
pub(crate) fn zip_of(members: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut z = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, bytes) in members {
            z.start_file(name.as_str(), opts).unwrap();
            z.write_all(bytes).unwrap();
        }
        z.finish().unwrap();
    }
    out
}

pub(crate) fn rgb(r: u8, g: u8, b: u8) -> Value {
    json!({"type": "solid", "color": {"mode": "RGB", "value": {"r": r, "g": g, "b": b}}})
}

pub(crate) fn translate(x: f64, y: f64) -> Value {
    json!({"a": 1, "b": 0, "c": 0, "d": 1, "tx": x, "ty": y})
}

/// A filled rectangle at `x`, `y` (its transform).
pub(crate) fn rect(id: &str, x: f64, y: f64, w: f64, h: f64) -> Value {
    json!({"type": "shape", "id": id, "name": id, "transform": translate(x, y),
        "style": {"fill": rgb(255, 0, 0)},
        "shape": {"type": "rect", "x": 0, "y": 0, "width": w, "height": h}})
}

pub(crate) fn group(id: &str, transform: Option<Value>, children: Vec<Value>) -> Value {
    let mut g = json!({"type": "group", "id": id, "name": id, "group": {"children": children}});
    if let Some(t) = transform {
        g["transform"] = t;
    }
    g
}

/// A point text at `x`, `y` (its first baseline), left aligned, 20 px Arial, line height 24.
pub(crate) fn point_text(id: &str, x: f64, y: f64, s: &str) -> Value {
    json!({"type": "text", "id": id, "name": id, "transform": translate(x, y),
        "meta": {"ux": {"rangedStyles": [{"length": 0, "fontFamily": "Arial", "fontStyle": "Regular", "postscriptName": "ArialMT",
            "fontSize": 20, "charSpacing": 0, "underline": false, "strikethrough": false, "textTransform": "none", "textScript": "none",
            "fill": {"value": 4278190080u32}}]}},
        "style": {"fill": rgb(0, 0, 0), "font": {"postscriptName": "ArialMT", "family": "Arial", "style": "Regular", "size": 20},
            "textAttributes": {"lineHeight": 24}},
        "text": {"frame": {"type": "positioned"}, "paragraphs": [{"lines": [[{"x": 0, "y": 0, "from": 0, "to": s.len()}]]}], "rawText": s}})
}

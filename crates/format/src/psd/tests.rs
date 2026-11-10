//! Photoshop import tests on files built in memory.

use figcraft_doc::{Document, Node, NodeKind};
use figcraft_psd::descriptor::{Descriptor, Id, UnicodeString, Value as D, VersionedDescriptor};
use figcraft_psd::path::{PathData, PsdKnot, PsdSubpath, VectorMaskBlock};
use figcraft_psd::{GroupSpec, ImageResource, LayerSpec, PixelData, PsdBuilder, TaggedBlock};

use super::{Options, import_with, is_psd};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> PixelData {
    PixelData::Rgba8(rgba.iter().copied().cycle().take((w * h * 4) as usize).collect())
}

fn import(bytes: &[u8]) -> (Document, Vec<String>) {
    import_with(bytes, "Test", Options { preview: true, ..Options::default() }).unwrap()
}

fn named<'a>(doc: &'a Document, name: &str) -> &'a Node {
    doc.nodes().find(|n| n.name == name).unwrap_or_else(|| panic!("no layer named {name}"))
}

fn world_xy(doc: &Document, n: &Node) -> (f64, f64) {
    let t = doc.world_transform(n.id).as_coeffs();
    (t[4], t[5])
}

/// A versioned descriptor, as most layer blocks store them.
fn versioned(d: Descriptor) -> Vec<u8> {
    VersionedDescriptor::new(d).to_bytes()
}

fn px(v: f64) -> D {
    D::UnitFloat { unit: *b"#Pxl", value: v }
}

fn rgb(r: f64, g: f64, b: f64) -> D {
    D::Descriptor(Descriptor::new("RGBC").with("Rd  ", D::Double(r)).with("Grn ", D::Double(g)).with("Bl  ", D::Double(b)))
}

fn base(w: u32, h: u32) -> PsdBuilder {
    let mut b = PsdBuilder::new(w, h);
    b.composite(solid(w, h, [255, 255, 255, 255]));
    b
}

#[test]
fn only_photoshop_documents_are_taken() {
    assert!(!is_psd(b"PK\x03\x04 not a psd"));
    assert!(!is_psd(b"8BPS\x00\x03 version three is no psd"));
    let bytes = base(4, 4).to_bytes().unwrap();
    assert!(is_psd(&bytes));
}

#[test]
fn pixel_layers_and_groups_keep_their_place_order_and_visibility() {
    let mut b = base(100, 80);
    b.push_layer(LayerSpec::new("Back", 0, 0, 100, 80, solid(100, 80, [0, 0, 255, 255])));
    b.begin_group(GroupSpec::new("Card"));
    let mut red = LayerSpec::new("Red", 10, 12, 20, 16, solid(20, 16, [255, 0, 0, 255]));
    red.opacity = 128;
    b.push_layer(red);
    let mut hidden = LayerSpec::new("Hidden", 40, 40, 5, 5, solid(5, 5, [0, 255, 0, 255]));
    hidden.visible = false;
    b.push_layer(hidden);
    b.end_group().unwrap();
    let (doc, _) = import(&b.to_bytes().unwrap());
    let frame = named(&doc, "Test");
    assert!(matches!(frame.kind, NodeKind::Frame));
    assert_eq!((frame.width, frame.height), (100.0, 80.0));
    let red = named(&doc, "Red");
    assert!(matches!(red.kind, NodeKind::Rectangle));
    assert_eq!((red.width, red.height), (20.0, 16.0));
    assert_eq!(world_xy(&doc, red), (10.0, 12.0));
    assert!((red.opacity - 128.0 / 255.0).abs() < 1e-3);
    assert!(red.fills.iter().any(|p| format!("{p:?}").contains("Image")), "{:?}", red.fills);
    assert!(!named(&doc, "Hidden").visible);
    let card = named(&doc, "Card");
    assert!(matches!(card.kind, NodeKind::Group));
    // Bottom to top, as in Photoshop.
    let kids: Vec<&str> = frame.children.iter().filter_map(|c| doc.node(*c)).map(|n| n.name.as_str()).collect();
    assert_eq!(kids, ["Back", "Card"]);
    // Photoshop's own picture is put beside the import.
    assert!(doc.nodes().any(|n| n.name == "Test (Photoshop preview)"));
}

#[test]
fn ruler_guides_become_guides_on_the_frame() {
    // Version, grid cycles, count, then (position in 1/32 px, direction: 0 vertical) per guide.
    let mut data = Vec::new();
    for v in [1u32, 576, 576, 2] {
        data.extend_from_slice(&v.to_be_bytes());
    }
    data.extend_from_slice(&(32 * 32i32).to_be_bytes());
    data.push(0);
    data.extend_from_slice(&(40 * 32i32 + 16).to_be_bytes());
    data.push(1);
    let mut b = base(100, 80).resource(ImageResource::new(1032, data));
    b.push_layer(LayerSpec::new("Back", 0, 0, 100, 80, solid(100, 80, [0, 0, 0, 255])));
    let (doc, _) = import(&b.to_bytes().unwrap());
    let g = named(&doc, "Test").guides();
    assert_eq!(g.len(), 2, "{g:?}");
    assert!(g.iter().any(|g| g.vertical && g.offset == 32.0));
    assert!(g.iter().any(|g| !g.vertical && g.offset == 40.5));
}

/// A rounded rectangle shape layer: its path, live shape and solid fill.
fn rounded_rect_layer(name: &str, l: f64, t: f64, r: f64, b: f64, w: f64, h: f64) -> LayerSpec {
    let k = |x: f64, y: f64| PsdKnot { linked: false, pre: (x / w, y / h), anchor: (x / w, y / h), post: (x / w, y / h) };
    let path = PathData::from_subpaths(&[PsdSubpath { closed: true, operation: 1, knots: vec![k(l, t), k(r, t), k(r, b), k(l, b)] }], false);
    let vmsk = VectorMaskBlock { version: 3, flags: 0, path }.to_bytes();
    let radii = Descriptor::new("radii")
        .with("unitValueQuadVersion", D::Integer(1))
        .with("topRight", px(6.0))
        .with("topLeft", px(6.0))
        .with("bottomLeft", px(6.0))
        .with("bottomRight", px(6.0));
    let bbox = Descriptor::new("bbox")
        .with("unitValueQuadVersion", D::Integer(1))
        .with("Top ", px(t))
        .with("Left", px(l))
        .with("Btom", px(b))
        .with("Rght", px(r));
    let key = Descriptor::new("null")
        .with("keyOriginType", D::Integer(2))
        .with("keyOriginRRectRadii", D::Descriptor(radii))
        .with("keyOriginShapeBBox", D::Descriptor(bbox));
    let mut vogk = 1u32.to_be_bytes().to_vec();
    vogk.extend(versioned(Descriptor::new("null").with("keyDescriptorList", D::List(vec![D::Descriptor(key)]))));
    let soco = versioned(Descriptor::new("null").with("Clr ", rgb(62.0, 218.0, 183.0)));
    let (pw, ph) = ((r - l) as u32, (b - t) as u32);
    let mut spec = LayerSpec::new(name, l as i32, t as i32, pw, ph, solid(pw, ph, [62, 218, 183, 255]));
    spec.extra_blocks = vec![TaggedBlock::new(*b"SoCo", soco), TaggedBlock::new(*b"vmsk", vmsk), TaggedBlock::new(*b"vogk", vogk)];
    spec
}

#[test]
fn live_rounded_rectangles_become_rectangles_with_their_radius_and_colour() {
    let mut b = base(200, 100);
    b.push_layer(rounded_rect_layer("Button", 20.0, 30.0, 120.0, 70.0, 200.0, 100.0));
    let (doc, _) = import(&b.to_bytes().unwrap());
    let n = named(&doc, "Button");
    assert!(matches!(n.kind, NodeKind::Rectangle), "{:?}", n.kind);
    assert_eq!((n.width, n.height), (100.0, 40.0));
    assert_eq!(world_xy(&doc, n), (20.0, 30.0));
    assert!(n.corners.radii.iter().all(|r| (*r - 6.0).abs() < 1e-9), "{:?}", n.corners.radii);
    let fill = format!("{:?}", n.fills);
    assert!(fill.contains("Solid"), "{fill}");
}

#[test]
fn clipped_layers_are_masked_by_their_base() {
    let mut b = base(100, 100);
    b.push_layer(LayerSpec::new("Base", 10, 10, 50, 50, solid(50, 50, [0, 0, 0, 255])));
    let mut photo = LayerSpec::new("Photo", 0, 0, 100, 100, solid(100, 100, [200, 100, 0, 255]));
    photo.clipping = true;
    b.push_layer(photo);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let photo = named(&doc, "Photo");
    let group = photo.parent.and_then(|p| doc.node(p)).unwrap();
    assert!(matches!(group.kind, NodeKind::Group));
    let first = group.children.first().and_then(|c| doc.node(*c)).unwrap();
    assert!(first.is_mask, "the base's copy masks the clipped layers");
    assert!(doc.node(group.children[1]).is_some_and(|n| n.name == "Base" && !n.is_mask));
}

#[test]
fn drop_shadows_become_effects() {
    let shadow = Descriptor::new("DrSh")
        .with("enab", D::Boolean(true))
        .with("Md  ", D::Enumerated { type_id: Id::new("BlnM"), value: Id::new("Mltp") })
        .with("Clr ", rgb(0.0, 0.0, 0.0))
        .with("Opct", D::UnitFloat { unit: *b"#Prc", value: 50.0 })
        .with("uglg", D::Boolean(false))
        .with("lagl", D::UnitFloat { unit: *b"#Ang", value: 90.0 })
        .with("Dstn", px(4.0))
        .with("Ckmt", px(0.0))
        .with("blur", px(8.0));
    let fx = Descriptor::new("null")
        .with("Scl ", D::UnitFloat { unit: *b"#Prc", value: 100.0 })
        .with("masterFXSwitch", D::Boolean(true))
        .with("DrSh", D::Descriptor(shadow));
    let mut data = 0u32.to_be_bytes().to_vec();
    data.extend(versioned(fx));
    let mut card = LayerSpec::new("Card", 10, 10, 40, 40, solid(40, 40, [255, 255, 255, 255]));
    card.extra_blocks = vec![TaggedBlock::new(*b"lfx2", data)];
    let mut b = base(100, 100);
    b.push_layer(card);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let effects = format!("{:?}", named(&doc, "Card").effects);
    assert!(effects.contains("DropShadow"), "{effects}");
    assert!(effects.contains("radius: 8.0"), "{effects}");
}

/// An embedded PNG placed twice becomes one component with two instances.
#[test]
fn smart_objects_placing_the_same_picture_share_a_component() {
    let png = super::util::png(4, 2, vec![255; 4 * 2 * 4]).unwrap();
    let uuid = "7f0c1d2e-0000-4000-8000-000000000001";
    // One `liFD` item: kind, version, uuid, name, type and creator, length, no descriptor, bytes.
    let mut item = b"liFD".to_vec();
    item.extend_from_slice(&7u32.to_be_bytes());
    item.push(uuid.len() as u8);
    item.extend_from_slice(uuid.as_bytes());
    let name: Vec<u16> = "logo.png".encode_utf16().collect();
    item.extend_from_slice(&(name.len() as u32).to_be_bytes());
    item.extend(name.iter().flat_map(|u| u.to_be_bytes()));
    item.extend_from_slice(b"png \0\0\0\0");
    item.extend_from_slice(&(png.len() as u64).to_be_bytes());
    item.push(0);
    item.extend_from_slice(&png);
    let mut lnk2 = (item.len() as u64).to_be_bytes().to_vec();
    lnk2.extend_from_slice(&item);
    while !lnk2.len().is_multiple_of(4) {
        lnk2.push(0);
    }
    let sold = |x: f64, y: f64, s: f64| {
        let corners = [x, y, x + 4.0 * s, y, x + 4.0 * s, y + 2.0 * s, x, y + 2.0 * s];
        let d = Descriptor::new("null")
            .with("Idnt", D::Text(UnicodeString::new(uuid)))
            .with("Trnf", D::List(corners.iter().map(|v| D::Double(*v)).collect()))
            .with("Sz  ", D::Descriptor(Descriptor::new("Pnt ").with("Wdth", D::Double(4.0)).with("Hght", D::Double(2.0))));
        let mut data = b"soLD".to_vec();
        data.extend_from_slice(&4u32.to_be_bytes());
        data.extend(versioned(d));
        data
    };
    let mut b = base(100, 100);
    for (name, x, y, s) in [("Logo", 10.0, 10.0, 1.0), ("Big logo", 40.0, 40.0, 5.0)] {
        let (w, h) = ((4.0 * s) as u32, (2.0 * s) as u32);
        let mut spec = LayerSpec::new(name, x as i32, y as i32, w, h, solid(w, h, [255, 255, 255, 255]));
        spec.extra_blocks = vec![TaggedBlock::new(*b"SoLd", sold(x, y, s))];
        b.push_layer(spec);
    }
    let mut file = b.build().unwrap();
    file.global_blocks.push(TaggedBlock::new(*b"lnk2", lnk2));
    let (doc, notes) = import(&file.to_bytes().unwrap());
    let mains: Vec<&Node> = doc.nodes().filter(|n| matches!(n.kind, NodeKind::Component { .. })).collect();
    assert_eq!(mains.len(), 1, "{notes:?}");
    assert_eq!(mains[0].name, "logo.png");
    assert_eq!((mains[0].width, mains[0].height), (4.0, 2.0));
    let big = named(&doc, "Big logo");
    assert!(matches!(big.kind, NodeKind::Instance { component, .. } if component == mains[0].id), "{:?}", big.kind);
    assert_eq!((big.width, big.height), (20.0, 10.0));
    assert_eq!(world_xy(&doc, big), (40.0, 40.0));
}

#[test]
fn postscript_names_give_a_family_and_style() {
    assert_eq!(super::text::guess_face("OpenSans-SemiBoldItalic"), ("Open Sans".into(), "Semi Bold Italic".into()));
    assert_eq!(super::text::guess_face("ArialMT"), ("Arial".into(), "Regular".into()));
    assert_eq!(super::text::guess_face("SourceSansPro-Semibold"), ("Source Sans Pro".into(), "Semi Bold".into()));
}

/// A file cut short anywhere, or with bytes flipped, is refused or imported, never a crash.
#[test]
fn damaged_files_never_crash() {
    let mut b = base(60, 40);
    b.push_layer(rounded_rect_layer("Shape", 5.0, 5.0, 40.0, 30.0, 60.0, 40.0));
    b.push_layer(LayerSpec::new("Pixels", 2, 2, 10, 10, solid(10, 10, [9, 9, 9, 255])));
    let bytes = b.to_bytes().unwrap();
    for cut in (26..bytes.len()).step_by(7) {
        let _ = import_with(&bytes[..cut], "Cut", Options::default());
    }
    for i in (26..bytes.len()).step_by(11) {
        let mut flipped = bytes.clone();
        flipped[i] ^= 0xA5;
        let _ = import_with(&flipped, "Flipped", Options::default());
    }
}

/// A two-stop gradient descriptor (red to blue), as overlays and fill layers store it.
fn red_to_blue(class: &str, angle: f64) -> Descriptor {
    let stop = |c: D, at: i32| {
        D::Descriptor(
            Descriptor::new("Clrt")
                .with("Clr ", c)
                .with("Type", D::Enumerated { type_id: Id::new("Clry"), value: Id::new("UsrS") })
                .with("Lctn", D::Integer(at))
                .with("Mdpn", D::Integer(50)),
        )
    };
    let opaque = |at: i32| {
        D::Descriptor(
            Descriptor::new("TrnS")
                .with("Opct", D::UnitFloat { unit: *b"#Prc", value: 100.0 })
                .with("Lctn", D::Integer(at))
                .with("Mdpn", D::Integer(50)),
        )
    };
    let grad = Descriptor::new("Grdn")
        .with("GrdF", D::Enumerated { type_id: Id::new("GrdF"), value: Id::new("CstS") })
        .with("Clrs", D::List(vec![stop(rgb(255.0, 0.0, 0.0), 0), stop(rgb(0.0, 0.0, 255.0), 4096)]))
        .with("Trns", D::List(vec![opaque(0), opaque(4096)]));
    Descriptor::new(class)
        .with("enab", D::Boolean(true))
        .with("Opct", D::UnitFloat { unit: *b"#Prc", value: 100.0 })
        .with("Grad", D::Descriptor(grad))
        .with("Angl", D::UnitFloat { unit: *b"#Ang", value: angle })
        .with("Type", D::Enumerated { type_id: Id::new("GrdT"), value: Id::new("Lnr ") })
        .with("Algn", D::Boolean(true))
        .with("Scl ", D::UnitFloat { unit: *b"#Prc", value: 100.0 })
}

/// A layer style block holding `effects` (key, descriptor).
fn style(effects: Vec<(&str, Descriptor)>) -> TaggedBlock {
    let mut fx = Descriptor::new("null").with("Scl ", D::UnitFloat { unit: *b"#Prc", value: 100.0 }).with("masterFXSwitch", D::Boolean(true));
    for (k, d) in effects {
        fx = fx.with(k, D::Descriptor(d));
    }
    let mut data = 0u32.to_be_bytes().to_vec();
    data.extend(versioned(fx));
    TaggedBlock::new(*b"lfx2", data)
}

fn gradient_of(n: &Node) -> figcraft_doc::color::Gradient {
    n.fills
        .iter()
        .find_map(|p| match &p.kind {
            figcraft_doc::color::PaintKind::Gradient(g) => Some(g.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no gradient in {:?}", n.fills))
}

#[test]
fn gradient_overlays_on_shapes_become_gradient_fills_over_the_shape_colour() {
    let mut shape = rounded_rect_layer("Button", 20.0, 30.0, 120.0, 70.0, 200.0, 100.0);
    shape.extra_blocks.push(style(vec![("GrFl", red_to_blue("GrFl", 0.0))]));
    let mut b = base(200, 100);
    b.push_layer(shape);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let n = named(&doc, "Button");
    assert!(format!("{:?}", n.fills.first()).contains("Solid"), "the shape's own colour stays below: {:?}", n.fills);
    let g = gradient_of(n);
    // Angle 0: left to right across the layer's bounds.
    assert!(g.from.0 < 0.01 && g.to.0 > 0.99, "{g:?}");
    assert!((g.from.1 - 0.5).abs() < 0.01 && (g.to.1 - 0.5).abs() < 0.01, "{g:?}");
    assert_eq!(g.stops.len(), 3, "both stops and their midpoint: {:?}", g.stops);
}

#[test]
fn gradient_fill_layers_become_rectangles_with_a_gradient() {
    let mut spec = LayerSpec::new("Sky", 0, 0, 1, 1, solid(1, 1, [0, 0, 0, 0]));
    spec.extra_blocks = vec![TaggedBlock::new(*b"GdFl", versioned(red_to_blue("null", 90.0)))];
    let mut b = base(100, 60);
    b.push_layer(spec);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let n = named(&doc, "Sky");
    assert!(matches!(n.kind, NodeKind::Rectangle));
    assert_eq!((n.width, n.height), (100.0, 60.0));
    // Angle 90: bottom to top.
    let g = gradient_of(n);
    assert!(g.from.1 > 0.99 && g.to.1 < 0.01, "{g:?}");
}

#[test]
fn gradient_overlays_on_pixel_layers_are_painted_into_the_pixels() {
    let mut spec = LayerSpec::new("Bar", 0, 0, 40, 4, solid(40, 4, [255, 255, 255, 255]));
    spec.extra_blocks = vec![style(vec![("GrFl", red_to_blue("GrFl", 0.0))])];
    let mut b = base(40, 4);
    b.push_layer(spec);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let n = named(&doc, "Bar");
    let key = n
        .fills
        .iter()
        .find_map(|p| match &p.kind {
            figcraft_doc::color::PaintKind::Image(i) => Some(i.image.clone()),
            _ => None,
        })
        .unwrap();
    let img = image::load_from_memory(&doc.images[&key].bytes).unwrap().to_rgba8();
    let (left, right) = (img.get_pixel(0, 2), img.get_pixel(39, 2));
    assert!(left[0] > 200 && left[2] < 60, "red on the left: {left:?}");
    assert!(right[2] > 200 && right[0] < 60, "blue on the right: {right:?}");
}

#[test]
fn pattern_overlays_become_tiled_image_fills() {
    let tile = figcraft_psd::patterns::PsdPattern {
        mode: 3,
        width: 2,
        height: 2,
        name: "Checks".into(),
        id: "11111111-2222-3333-4444-555555555555".into(),
        palette: None,
        depth: 8,
        channels: vec![vec![255, 0, 0, 255], vec![255, 0, 0, 255], vec![255, 0, 0, 255]],
        alpha: None,
    };
    let fill = Descriptor::new("patternFill")
        .with("enab", D::Boolean(true))
        .with("Opct", D::UnitFloat { unit: *b"#Prc", value: 50.0 })
        .with(
            "Ptrn",
            D::Descriptor(
                Descriptor::new("Ptrn").with("Nm  ", D::Text(UnicodeString::new("Checks"))).with("Idnt", D::Text(UnicodeString::new(&tile.id))),
            ),
        )
        .with("Scl ", D::UnitFloat { unit: *b"#Prc", value: 200.0 })
        .with("Algn", D::Boolean(true));
    let mut shape = rounded_rect_layer("Card", 10.0, 10.0, 60.0, 50.0, 100.0, 80.0);
    shape.extra_blocks.push(style(vec![("patternFill", fill)]));
    let mut b = base(100, 80);
    b.push_layer(shape);
    let mut file = b.build().unwrap();
    file.global_blocks.push(TaggedBlock::new(*b"Patt", figcraft_psd::patterns::write_pattern_block(&[tile]).unwrap()));
    let (doc, notes) = import(&file.to_bytes().unwrap());
    let n = named(&doc, "Card");
    let img = n
        .fills
        .iter()
        .find_map(|p| match &p.kind {
            figcraft_doc::color::PaintKind::Image(i) => Some((i.clone(), p.opacity)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no pattern fill in {:?} ({notes:?})", n.fills));
    assert_eq!(img.0.scale_mode, figcraft_doc::color::ImageScaleMode::Tile);
    assert!((img.0.scale - 2.0).abs() < 1e-9);
    assert!((img.1 - 0.5).abs() < 1e-6);
    assert_eq!((doc.images[&img.0.image].width, doc.images[&img.0.image].height), (2, 2));
}

/// A pixel layer with a raster mask and a vector mask: the raster mask is baked into its pixels
/// and the vector mask masks it in a group.
#[test]
fn vector_masks_on_pixel_layers_mask_them_in_a_group() {
    let k = |x: f64, y: f64| PsdKnot { linked: false, pre: (x, y), anchor: (x, y), post: (x, y) };
    let path =
        PathData::from_subpaths(&[PsdSubpath { closed: true, operation: 1, knots: vec![k(0.1, 0.1), k(0.5, 0.1), k(0.5, 0.5), k(0.1, 0.5)] }], false);
    let mut spec = LayerSpec::new("Photo", 0, 0, 100, 100, solid(100, 100, [9, 99, 199, 255]));
    spec.extra_blocks = vec![TaggedBlock::new(*b"vmsk", VectorMaskBlock { version: 3, flags: 0, path }.to_bytes())];
    spec.mask =
        Some(figcraft_psd::MaskSpec { rect: figcraft_psd::Rect::from_xywh(0, 0, 50, 50), data: vec![255; 2500], default_color: 0, disabled: false });
    let mut b = base(100, 100);
    b.push_layer(spec);
    let (doc, _) = import(&b.to_bytes().unwrap());
    let photo = doc.nodes().find(|n| n.name == "Photo" && matches!(n.kind, NodeKind::Rectangle)).unwrap();
    // The pixels are baked with their raster mask; the vector mask masks them in a group.
    let g = photo.parent.and_then(|p| doc.node(p)).unwrap();
    let masks = g.children.iter().filter_map(|c| doc.node(*c)).filter(|n| n.is_mask).count();
    assert_eq!(masks, 1, "one mask per group");
}

/// A placed Photoshop file without layers (saved flattened) gives its component its picture.
#[test]
fn flattened_placed_files_give_their_component_its_picture() {
    let mut inner = PsdBuilder::new(4, 2);
    inner.composite(solid(4, 2, [0, 200, 0, 255]));
    let inner = inner.to_bytes().unwrap();
    let uuid = "7f0c1d2e-0000-4000-8000-000000000002";
    let mut item = b"liFD".to_vec();
    item.extend_from_slice(&7u32.to_be_bytes());
    item.push(uuid.len() as u8);
    item.extend_from_slice(uuid.as_bytes());
    let name: Vec<u16> = "flat.psb".encode_utf16().collect();
    item.extend_from_slice(&(name.len() as u32).to_be_bytes());
    item.extend(name.iter().flat_map(|u| u.to_be_bytes()));
    item.extend_from_slice(b"8BPB\0\0\0\0");
    item.extend_from_slice(&(inner.len() as u64).to_be_bytes());
    item.push(0);
    item.extend_from_slice(&inner);
    let mut lnk2 = (item.len() as u64).to_be_bytes().to_vec();
    lnk2.extend_from_slice(&item);
    while !lnk2.len().is_multiple_of(4) {
        lnk2.push(0);
    }
    let corners = [10.0, 10.0, 14.0, 10.0, 14.0, 12.0, 10.0, 12.0];
    let d = Descriptor::new("null")
        .with("Idnt", D::Text(UnicodeString::new(uuid)))
        .with("Trnf", D::List(corners.iter().map(|v| D::Double(*v)).collect()))
        .with("Sz  ", D::Descriptor(Descriptor::new("Pnt ").with("Wdth", D::Double(4.0)).with("Hght", D::Double(2.0))));
    let mut sold = b"soLD".to_vec();
    sold.extend_from_slice(&4u32.to_be_bytes());
    sold.extend(versioned(d));
    let mut spec = LayerSpec::new("Flat", 10, 10, 4, 2, solid(4, 2, [0, 200, 0, 255]));
    spec.extra_blocks = vec![TaggedBlock::new(*b"SoLd", sold)];
    let mut b = base(40, 40);
    b.push_layer(spec);
    let mut file = b.build().unwrap();
    file.global_blocks.push(TaggedBlock::new(*b"lnk2", lnk2));
    let (doc, _) = import(&file.to_bytes().unwrap());
    let main = doc.nodes().find(|n| matches!(n.kind, NodeKind::Component { .. })).unwrap();
    assert_eq!(main.name, "flat.psb");
    let child = main.children.first().and_then(|c| doc.node(*c)).expect("the component holds the file's picture");
    assert!(child.fills.iter().any(|p| format!("{p:?}").contains("Image")), "{:?}", child.fills);
}

/// A one-page PDF `w` × `h` points drawing `content`, with a correct cross-reference table.
fn tiny_pdf(w: u32, h: u32, content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 4 0 R /Resources << >> >>"),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len() + 1),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).bytes());
    out
}

/// A PSD placing the embedded file `bytes` (named `name`, `w` × `h`) at (10, 10), at its size,
/// with layer style blocks `extra`.
fn placing(name: &str, bytes: &[u8], w: u32, h: u32, extra: Vec<TaggedBlock>) -> Vec<u8> {
    let uuid = "7f0c1d2e-0000-4000-8000-0000000000aa";
    let mut item = b"liFD".to_vec();
    item.extend_from_slice(&7u32.to_be_bytes());
    item.push(uuid.len() as u8);
    item.extend_from_slice(uuid.as_bytes());
    let units: Vec<u16> = name.encode_utf16().collect();
    item.extend_from_slice(&(units.len() as u32).to_be_bytes());
    item.extend(units.iter().flat_map(|u| u.to_be_bytes()));
    item.extend_from_slice(b"PDF \0\0\0\0");
    item.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    item.push(0);
    item.extend_from_slice(bytes);
    let mut lnk2 = (item.len() as u64).to_be_bytes().to_vec();
    lnk2.extend_from_slice(&item);
    while !lnk2.len().is_multiple_of(4) {
        lnk2.push(0);
    }
    let (fw, fh) = (f64::from(w), f64::from(h));
    let corners = [10.0, 10.0, 10.0 + fw, 10.0, 10.0 + fw, 10.0 + fh, 10.0, 10.0 + fh];
    let d = Descriptor::new("null")
        .with("Idnt", D::Text(UnicodeString::new(uuid)))
        .with("Trnf", D::List(corners.iter().map(|v| D::Double(*v)).collect()))
        .with("Sz  ", D::Descriptor(Descriptor::new("Pnt ").with("Wdth", D::Double(fw)).with("Hght", D::Double(fh))));
    let mut sold = b"soLD".to_vec();
    sold.extend_from_slice(&4u32.to_be_bytes());
    sold.extend(versioned(d));
    let mut spec = LayerSpec::new("Icon", 10, 10, w, h, solid(w, h, [255, 0, 0, 255]));
    spec.extra_blocks = vec![TaggedBlock::new(*b"SoLd", sold)];
    spec.extra_blocks.extend(extra);
    let mut b = base(60, 60);
    b.push_layer(spec);
    let mut file = b.build().unwrap();
    file.global_blocks.push(TaggedBlock::new(*b"lnk2", lnk2));
    file.to_bytes().unwrap()
}

fn solid_fill_of(n: &Node) -> Option<figcraft_doc::color::Color> {
    n.fills.iter().find_map(|p| match &p.kind {
        figcraft_doc::color::PaintKind::Solid { color } => Some(*color),
        _ => None,
    })
}

/// A vector smart object becomes a component holding its paths, with Photoshop's pixels above.
#[test]
fn vector_smart_objects_become_vector_components_with_photoshops_picture_above() {
    let pdf = tiny_pdf(20, 10, "1 0 0 rg 0 0 20 10 re f");
    let (doc, notes) = import(&placing("Icon.ai", &pdf, 20, 10, Vec::new()));
    let main = doc.nodes().find(|n| matches!(n.kind, NodeKind::Component { .. })).unwrap_or_else(|| panic!("no component: {notes:?}"));
    assert_eq!(main.name, "Icon.ai");
    assert_eq!((main.width, main.height), (20.0, 10.0));
    let kids: Vec<&Node> = main.children.iter().filter_map(|c| doc.node(*c)).collect();
    assert_eq!(kids.iter().map(|k| k.name.as_str()).collect::<Vec<_>>(), ["Vector", "Photoshop render"], "{notes:?}");
    let path = kids[0].children.first().and_then(|c| doc.node(*c)).unwrap();
    assert!(matches!(path.kind, NodeKind::Vector { .. }));
    assert_eq!((path.width, path.height), (20.0, 10.0));
    let c = solid_fill_of(path).unwrap();
    assert!(c.r > 0.99 && c.g < 0.01 && c.b < 0.01, "{c:?}");
}

/// A solid colour overlay recolours the vectors.
#[test]
fn colour_overlays_recolour_vector_smart_objects() {
    let pdf = tiny_pdf(20, 10, "0 0 0 rg 0 0 20 10 re f");
    let overlay = Descriptor::new("SoFi")
        .with("enab", D::Boolean(true))
        .with("Md  ", D::Enumerated { type_id: Id::new("BlnM"), value: Id::new("Nrml") })
        .with("Clr ", rgb(0.0, 200.0, 100.0))
        .with("Opct", D::UnitFloat { unit: *b"#Prc", value: 100.0 });
    let (doc, _) = import(&placing("Check.ai", &pdf, 20, 10, vec![style(vec![("SoFi", overlay)])]));
    let path = doc.nodes().find(|n| matches!(n.kind, NodeKind::Vector { .. }) && n.name == "Path").unwrap();
    let c = solid_fill_of(path).unwrap();
    assert!(c.r < 0.01 && (c.g - 200.0 / 255.0).abs() < 0.01 && (c.b - 100.0 / 255.0).abs() < 0.01, "{c:?}");
}

/// Artwork with something vectors can't hold the same way keeps Photoshop's pixels only.
#[test]
fn vector_smart_objects_with_text_keep_their_pixels() {
    let pdf = tiny_pdf(20, 10, "BT /F1 8 Tf 0 0 Td (Hi) Tj ET");
    let (doc, _) = import(&placing("Text.ai", &pdf, 20, 10, Vec::new()));
    assert!(!doc.nodes().any(|n| n.name == "Vector"));
    let main = doc.nodes().find(|n| matches!(n.kind, NodeKind::Component { .. })).unwrap();
    let child = main.children.first().and_then(|c| doc.node(*c)).unwrap();
    assert!(child.fills.iter().any(|p| format!("{p:?}").contains("Image")));
}

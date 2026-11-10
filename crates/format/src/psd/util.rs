//! Reading Photoshop descriptors and making images.

use figcraft_doc::geom::{Affine, Rect};
use figcraft_psd::descriptor::{Descriptor, Value, VersionedDescriptor};
use serde_json::{Value as Json, json};

/// Largest image side made (wider layers are left out with a note).
pub const MAX_IMAGE_SIDE: u32 = 32_000;
/// Largest size and farthest position kept (the REST importer's own bounds).
pub const MAX_SIZE: f64 = 1e6;

/// The descriptor at `key`.
pub fn desc<'a>(d: &'a Descriptor, key: &str) -> Option<&'a Descriptor> {
    match d.get(key)? {
        Value::Descriptor(x) | Value::GlobalObject(x) => Some(x),
        _ => None,
    }
}

/// A number at `key` (plain, unit or integer), when finite.
pub fn num(d: &Descriptor, key: &str) -> Option<f64> {
    value_num(d.get(key)?)
}

/// A value as a finite number.
pub fn value_num(v: &Value) -> Option<f64> {
    let x = match v {
        Value::Double(x) => *x,
        Value::UnitFloat { value, .. } => *value,
        Value::Integer(i) => f64::from(*i),
        Value::LargeInteger(i) => *i as f64,
        _ => return None,
    };
    x.is_finite().then_some(x)
}

/// A flag at `key`.
pub fn flag(d: &Descriptor, key: &str) -> Option<bool> {
    match d.get(key)? {
        Value::Boolean(b) => Some(*b),
        _ => None,
    }
}

/// An enumerated value's id at `key` (`Nrml`, `strokeStyleAlignInside`).
pub fn enumv(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(String::from_utf8_lossy(value.as_bytes()).into_owned()),
        _ => None,
    }
}

/// A text at `key`, without its trailing NULs.
pub fn text(d: &Descriptor, key: &str) -> Option<String> {
    match d.get(key)? {
        Value::Text(t) => Some(t.to_string_lossy()),
        _ => None,
    }
}

/// The list at `key` (empty when there is none).
pub fn list<'a>(d: &'a Descriptor, key: &str) -> &'a [Value] {
    match d.get(key) {
        Some(Value::List(l)) => l,
        _ => &[],
    }
}

/// A colour descriptor (`Clr `) as RGB in 0..=1: RGB and grayscale colours; others give `None`.
pub fn color(d: &Descriptor) -> Option<[f64; 3]> {
    let c = |k: &str| num(d, k).map(|v| (v / 255.0).clamp(0.0, 1.0));
    if let (Some(r), Some(g), Some(b)) = (c("Rd  "), c("Grn "), c("Bl  ")) {
        return Some([r, g, b]);
    }
    if let (Some(r), Some(g), Some(b)) = (num(d, "redFloat"), num(d, "greenFloat"), num(d, "blueFloat")) {
        return Some([r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]);
    }
    // Grayscale: the share of black ink, in percent.
    if let Some(k) = num(d, "Gry ") {
        let v = (1.0 - k / 100.0).clamp(0.0, 1.0);
        return Some([v, v, v]);
    }
    None
}

/// A REST colour.
pub fn rgba(c: [f64; 3], a: f64) -> Json {
    json!({"r": c[0], "g": c[1], "b": c[2], "a": a.clamp(0.0, 1.0)})
}

/// A versioned descriptor at the start of `data` (a `u32` version, then the descriptor).
pub fn versioned(data: &[u8]) -> Option<Descriptor> {
    VersionedDescriptor::parse_prefix(data).ok().map(|(v, _)| v.descriptor)
}

/// RGBA pixels encoded as PNG.
pub fn png(width: u32, height: u32, rgba: Vec<u8>) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_IMAGE_SIDE || height > MAX_IMAGE_SIDE {
        return None;
    }
    let img = image::RgbaImage::from_raw(width, height, rgba)?;
    let mut out = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Fast,
        image::codecs::png::FilterType::Adaptive,
    );
    img.write_with_encoder(enc).ok()?;
    Some(out.into_inner())
}

/// A REST blend mode for a Photoshop one.
pub fn blend(mode: figcraft_psd::BlendMode) -> &'static str {
    use figcraft_psd::BlendMode as B;
    match mode {
        B::PassThrough => "PASS_THROUGH",
        B::Darken | B::DarkerColor => "DARKEN",
        B::Multiply => "MULTIPLY",
        B::ColorBurn => "COLOR_BURN",
        B::LinearBurn => "LINEAR_BURN",
        B::Lighten | B::LighterColor => "LIGHTEN",
        B::Screen => "SCREEN",
        B::ColorDodge => "COLOR_DODGE",
        B::LinearDodge => "LINEAR_DODGE",
        B::Overlay => "OVERLAY",
        B::SoftLight => "SOFT_LIGHT",
        B::HardLight | B::VividLight | B::LinearLight | B::PinLight | B::HardMix => "HARD_LIGHT",
        B::Difference | B::Subtract | B::Divide => "DIFFERENCE",
        B::Exclusion => "EXCLUSION",
        B::Hue => "HUE",
        B::Saturation => "SATURATION",
        B::Color => "COLOR",
        B::Luminosity => "LUMINOSITY",
        _ => "NORMAL",
    }
}

/// A blend mode named by an effect's `Md  ` key (`Nrml`, `Mltp`, `Scrn`, …).
pub fn blend_key(key: &str) -> &'static str {
    match key {
        "Mltp" => "MULTIPLY",
        "Scrn" => "SCREEN",
        "Ovrl" => "OVERLAY",
        "SftL" => "SOFT_LIGHT",
        "HrdL" | "vividLight" | "linearLight" | "pinLight" | "hardMix" => "HARD_LIGHT",
        "Drkn" | "darkerColor" => "DARKEN",
        "Lghn" | "lighterColor" => "LIGHTEN",
        "CDdg" => "COLOR_DODGE",
        "CBrn" => "COLOR_BURN",
        "linearDodge" => "LINEAR_DODGE",
        "linearBurn" => "LINEAR_BURN",
        "Dfrn" | "blendSubtraction" | "blendDivide" => "DIFFERENCE",
        "Xclu" => "EXCLUSION",
        "H   " => "HUE",
        "Strt" => "SATURATION",
        "Clr " => "COLOR",
        "Lmns" => "LUMINOSITY",
        _ => "NORMAL",
    }
}

/// A transform as REST `relativeTransform`.
pub fn rel(t: Affine) -> Json {
    let [a, b, c, d, e, f] = t.as_coeffs();
    json!([[a, c, e], [b, d, f]])
}

/// The box `w` × `h` placed by `t`, in its parent's space.
pub fn bbox(t: Affine, w: f64, h: f64) -> Rect {
    figcraft_doc::geom::transform_rect_bbox(t, Rect::new(0.0, 0.0, w, h))
}

/// Whether every coefficient of `t` is finite and within reach.
pub fn sane(t: Affine) -> bool {
    t.as_coeffs().iter().all(|v| v.is_finite() && v.abs() <= 1e7)
}

/// `t` split into a rotation (with any mirroring) and translation, and the scale along its own
/// axes: `t ≈ placed * scale(sx, sy)`. `None` when it is degenerate.
pub fn split_scale(t: Affine) -> Option<(Affine, f64, f64)> {
    let [a, b, c, d, e, f] = t.as_coeffs();
    let sx = a.hypot(b);
    let det = a * d - b * c;
    if !(sx > 1e-9 && det.abs() > 1e-12 && sane(t)) {
        return None;
    }
    let sy = det.abs() / sx;
    let flip = if det < 0.0 { -1.0 } else { 1.0 };
    let (ca, sa) = (a / sx, b / sx);
    // The rotation, mirrored across the x axis when the determinant is negative.
    let placed = Affine::new([ca, sa, -sa * flip, ca * flip, e, f]);
    Some((placed, sx, sy))
}

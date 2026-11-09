//! Import of Adobe XD `.xd` files (best effort), written from black-box study of files only: the
//! format is not publicly specified.
//!
//! An `.xd` file is a zip archive whose `mimetype` member says
//! `application/vnd.adobe.sparkler.project+dcxucf`. `manifest` lists the artboards in order (as
//! `artwork/artboard-<id>`), each artboard's layers are in
//! `artwork/artboard-<id>/graphics/graphicContent.agc` (JSON), layers outside every artboard in
//! `artwork/pasteboard/graphics/graphicContent.agc`, and
//! `resources/graphics/graphicContent.agc` holds the artboards' places and sizes and the
//! definitions of the components. Images are `resources/<uid>`. [`tree`] maps the layers onto the
//! REST API file JSON shape and the REST importer builds the document, so this import shares its
//! mapping, budgets and import report with the `.fig`, `.sketch` and file JSON imports.
//!
//! Artboards, groups, shapes, paths, text, images, transforms, masks, components and their states,
//! stacks, repeat grids, document styles and prototype links are mapped. Anything not understood is skipped with a warning;
//! malformed input never panics.

mod assets;
#[cfg(test)]
mod assets_tests;
#[cfg(test)]
mod fixture;
mod grid;
#[cfg(test)]
mod grid_tests;
mod proto;
#[cfg(test)]
mod proto_tests;
mod stack;
#[cfg(test)]
mod stack_tests;
#[cfg(test)]
mod states_tests;
mod style;
#[cfg(test)]
mod style_tests;
#[cfg(test)]
mod tests;
mod text;
#[cfg(test)]
mod text_tests;
mod tree;
#[cfg(test)]
mod tree_tests;

use figcraft_doc::Document;
use serde_json::Value;

use crate::FormatError;

/// What the `mimetype` member of an `.xd` file says.
pub const MIMETYPE: &str = "application/vnd.adobe.sparkler.project+dcxucf";
/// Largest `graphicContent.agc` read (a real artboard of a few thousand layers takes a few MB).
const MAX_JSON_MEMBER: u64 = 256 * 1024 * 1024;
/// Most bytes a member may unpack to, per compressed byte, with a floor for small members.
const MAX_RATIO: u64 = 100;
const MIN_ALLOWANCE: u64 = 16 * 1024 * 1024;
/// Most JSON read from one file in all.
#[cfg(not(target_pointer_width = "32"))]
const MAX_JSON_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(target_pointer_width = "32")]
const MAX_JSON_TOTAL: u64 = 512 * 1024 * 1024;
/// Largest image member read.
const MAX_IMAGE: u64 = figcraft_doc::ImageData::MAX_BYTES;
/// Most image bytes read from one file in all.
#[cfg(not(target_pointer_width = "32"))]
const MAX_IMAGE_TOTAL: u64 = 1024 * 1024 * 1024;
#[cfg(target_pointer_width = "32")]
const MAX_IMAGE_TOTAL: u64 = 256 * 1024 * 1024;
/// Most artboards read from one file.
const MAX_ARTBOARDS: usize = 5000;

/// Finds the installed face a PostScript name belongs to: its family and style.
pub type FontLookup = crate::sketch::FontLookup;
/// A face's ascent and descent as shares of its size (both positive), by family and style.
pub type MetricsLookup = Box<dyn Fn(&str, &str) -> Option<(f64, f64)>>;

/// What the import may ask about the installed fonts.
#[derive(Default)]
pub struct Fonts {
    /// The installed face of a PostScript name.
    pub face: Option<FontLookup>,
    /// A face's vertical metrics, to put text lines where XD draws them.
    pub metrics: Option<MetricsLookup>,
}

/// Whether `bytes` look like an `.xd` file: a zip whose `mimetype` member names XD's document type.
pub fn is_xd(bytes: &[u8]) -> bool {
    if !bytes.starts_with(b"PK") {
        return false;
    }
    let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(bytes)) else { return false };
    crate::read_member(&mut z, "mimetype", 256).is_ok_and(|m| m.trim_ascii() == MIMETYPE.as_bytes())
}

/// Import `bytes` when they are an `.xd` file; `None` when they are something else.
pub fn try_import(bytes: &[u8], name: &str) -> Option<Result<(Document, Vec<String>), FormatError>> {
    try_import_with_fonts(bytes, name, Fonts::default())
}

/// [`try_import`] asking `fonts` about the installed fonts.
pub fn try_import_with_fonts(bytes: &[u8], name: &str, fonts: Fonts) -> Option<Result<(Document, Vec<String>), FormatError>> {
    is_xd(bytes).then(|| import_with_fonts(bytes, name, fonts))
}

/// Build a document from an `.xd` file. Returns it with the warnings about what was skipped or
/// approximated. `name`: the document name.
pub fn import(bytes: &[u8], name: &str) -> Result<(Document, Vec<String>), FormatError> {
    import_with_fonts(bytes, name, Fonts::default())
}

/// [`import`] asking `fonts` about the installed fonts.
pub fn import_with_fonts(bytes: &[u8], name: &str, fonts: Fonts) -> Result<(Document, Vec<String>), FormatError> {
    import_parts(bytes, name, crate::fig::layer_budget(bytes.len()), fonts)
}

/// [`import`] with at most `max_nodes` layers.
#[cfg(test)]
pub(crate) fn import_within_nodes(bytes: &[u8], name: &str, max_nodes: usize) -> Result<(Document, Vec<String>), FormatError> {
    import_parts(bytes, name, max_nodes, Fonts::default())
}

fn import_parts(bytes: &[u8], name: &str, max_nodes: usize, fonts: Fonts) -> Result<(Document, Vec<String>), FormatError> {
    let archive = Archive::open(bytes)?;
    let map = tree::to_rest(&archive, name, max_nodes, fonts);
    let mut warnings = archive.warnings.clone();
    warnings.extend(map.warnings);
    let images = |r: &str| archive.image(r);
    let (doc, more) = crate::rest::import_with_networks_plain(&map.json, &images, map.networks, max_nodes)?;
    warnings.extend(more);
    Ok((doc, warnings))
}

fn corrupt(msg: impl Into<String>) -> FormatError {
    FormatError::Corrupt(format!("XD file: {}", msg.into()))
}

/// One artboard: its id, name, place and size on the canvas, and its layers.
pub(crate) struct Artboard {
    pub id: String,
    pub name: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// The artboard's `graphicContent.agc`.
    pub content: Value,
}

/// The parts of an `.xd` archive.
pub(crate) struct Archive<'b> {
    /// The artboards in the file's order.
    pub artboards: Vec<Artboard>,
    /// The layers outside every artboard (`artwork/pasteboard`), when there are any.
    pub pasteboard: Option<Value>,
    /// `resources/graphics/graphicContent.agc`: artboard places, component definitions.
    pub resources: Value,
    /// The prototype interactions by layer id (`interactions/interactions.json`).
    pub protos: Value,
    /// The artboard a prototype starts on (`homeArtboard`).
    pub home: Option<String>,
    /// Problems met while reading (artboards skipped).
    pub warnings: Vec<String>,
    zip: zip::ZipArchive<std::io::Cursor<&'b [u8]>>,
    /// Image bytes read so far.
    image_total: std::cell::Cell<u64>,
}

impl<'b> Archive<'b> {
    fn open(bytes: &'b [u8]) -> Result<Self, FormatError> {
        let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| corrupt(e.to_string()))?;
        let mut total = 0u64;
        let mut warnings = Vec::new();
        let resources = match json_member(&mut z, "resources/graphics/graphicContent.agc", &mut total) {
            Ok(r) if r.is_object() => r,
            other => {
                let why = other.err().map_or_else(|| "not an object".to_string(), |e| e.to_string());
                warnings.push(format!("the file's resources (artboard sizes, components) were skipped: {}", trim_err(&why)));
                Value::Null
            }
        };
        let manifest = json_member(&mut z, "manifest", &mut total).unwrap_or(Value::Null);
        let ids = artboard_ids(&manifest, &z);
        let places = resources.get("artboards");
        let mut artboards = Vec::new();
        for id in ids.iter().take(MAX_ARTBOARDS) {
            let member = format!("artwork/artboard-{id}/graphics/graphicContent.agc");
            let content = match json_member(&mut z, &member, &mut total) {
                Ok(c) if c.is_object() => c,
                Ok(_) => {
                    warnings.push("artboards that aren't JSON objects were skipped".into());
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("an artboard was skipped: {}", trim_err(&e.to_string())));
                    continue;
                }
            };
            let place = places.and_then(|p| p.get(id.as_str()));
            let n = |k: &str| place.and_then(|p| p.get(k)).and_then(Value::as_f64).filter(|v| v.is_finite());
            let name = place.and_then(|p| p.get("name")).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| manifest_name(&manifest, id));
            let (Some(width), Some(height)) = (n("width"), n("height")) else {
                warnings.push("artboards without a size were imported at their content's place".into());
                artboards.push(Artboard { id: id.clone(), name, x: 0.0, y: 0.0, width: 0.0, height: 0.0, content });
                continue;
            };
            artboards.push(Artboard { id: id.clone(), name, x: n("x").unwrap_or(0.0), y: n("y").unwrap_or(0.0), width, height, content });
        }
        if ids.len() > MAX_ARTBOARDS {
            warnings.push(format!("the file has more than {MAX_ARTBOARDS} artboards; the rest were skipped"));
        }
        let pasteboard = json_member(&mut z, "artwork/pasteboard/graphics/graphicContent.agc", &mut total)
            .ok()
            .filter(|p| p.get("children").and_then(Value::as_array).is_some_and(|c| !c.is_empty()));
        let interactions = json_member(&mut z, "interactions/interactions.json", &mut total).unwrap_or(Value::Null);
        let protos = interactions.get("interactions").filter(|i| i.is_object()).cloned().unwrap_or(Value::Null);
        let home = interactions.get("homeArtboard").and_then(Value::as_str).filter(|h| valid_id(h)).map(str::to_string);
        Ok(Archive { artboards, pasteboard, resources, protos, home, warnings, zip: z, image_total: std::cell::Cell::new(0) })
    }

    /// The bytes of image `uid` (`resources/<uid>`).
    pub(crate) fn image(&self, uid: &str) -> Option<Vec<u8>> {
        if !valid_id(uid) {
            return None;
        }
        // A clone shares the parsed directory and the bytes; the resolver only has shared access.
        let mut z = self.zip.clone();
        let mut total = self.image_total.get();
        let data = capped_member(&mut z, &format!("resources/{uid}"), MAX_IMAGE, &mut total, MAX_IMAGE_TOTAL).ok();
        self.image_total.set(total);
        data
    }
}

/// The artboard ids in the manifest's order, else in the archive's.
fn artboard_ids(manifest: &Value, z: &zip::ZipArchive<std::io::Cursor<&[u8]>>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let artwork = manifest
        .get("children")
        .and_then(Value::as_array)
        .and_then(|c| c.iter().find(|c| c.get("path").and_then(Value::as_str) == Some("artwork")))
        .and_then(|a| a.get("children"))
        .and_then(Value::as_array);
    let from_manifest: Vec<String> = artwork
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("path").and_then(Value::as_str)?.strip_prefix("artboard-"))
        .filter(|id| valid_id(id))
        .map(str::to_string)
        .filter(|id| seen.insert(id.clone()))
        .collect();
    // Artboards the manifest doesn't list come after, in archive order.
    let mut out = from_manifest;
    for name in z.file_names() {
        if let Some(id) = name.strip_prefix("artwork/artboard-").and_then(|r| r.strip_suffix("/graphics/graphicContent.agc"))
            && valid_id(id)
            && seen.insert(id.to_string())
        {
            out.push(id.to_string());
        }
    }
    out
}

/// The name the manifest gives artboard `id`, else "Artboard".
fn manifest_name(manifest: &Value, id: &str) -> String {
    let path = format!("artboard-{id}");
    manifest
        .get("children")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|c| c.get("children").and_then(Value::as_array))
        .flatten()
        .find(|c| c.get("path").and_then(Value::as_str) == Some(path.as_str()))
        .and_then(|c| c.get("name").and_then(Value::as_str))
        .map_or_else(|| "Artboard".to_string(), |n| n.chars().take(256).collect())
}

fn trim_err(e: &str) -> &str {
    e.trim_start_matches("not a valid FigCraft file: ")
}

/// Member `name`, refused before unpacking when it would be larger than `limit`, than
/// [`MAX_RATIO`] times what it takes in the archive (past a floor), or than what is left of
/// `max_total` (`total`: read so far, counted up).
fn capped_member(
    z: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    limit: u64,
    total: &mut u64,
    max_total: u64,
) -> Result<Vec<u8>, FormatError> {
    let (size, packed) = {
        let f = z.by_name(name).map_err(|e| corrupt(format!("{name}: {e}")))?;
        (f.size(), f.compressed_size())
    };
    let allowed = limit.min(packed.saturating_mul(MAX_RATIO).max(MIN_ALLOWANCE)).min(max_total.saturating_sub(*total));
    if size > allowed {
        return Err(corrupt(format!("{name} is too large")));
    }
    let data = crate::read_member(z, name, allowed);
    // A failed read unpacked all the same: charged in full.
    *total += data.as_ref().map_or(size, |d| d.len() as u64);
    data
}

/// JSON member `name`, read by [`capped_member`] within the file's [`MAX_JSON_TOTAL`].
fn json_member(z: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str, total: &mut u64) -> Result<Value, FormatError> {
    let data = capped_member(z, name, MAX_JSON_MEMBER, total, MAX_JSON_TOTAL)?;
    serde_json::from_slice(&data).map_err(|e| corrupt(format!("{name}: {e}")))
}

/// Ids name archive members: letters, digits, `-` and `_` only, never a path.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

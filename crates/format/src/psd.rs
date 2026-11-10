//! Import of Adobe Photoshop `.psd` and `.psb` files as editable design documents.
//!
//! The file is read with `figcraft-psd` (the PSD reader FigCraft shares with PhotoCraft) and its
//! layers are mapped onto the REST API file JSON shape, which the REST importer turns into a
//! document, as the `.xd` and `.sketch` imports do. What maps:
//!
//! * the canvas becomes a frame of the document's size, or each artboard a frame of its own;
//! * groups become groups, with their opacity, blend mode and visibility;
//! * pixel layers become rectangles filled with their pixels (a raster user mask is baked in);
//! * type layers become text with their fonts, sizes, colours, tracking, leading and alignment;
//! * shape layers become rectangles, ellipses (live shapes) or vectors, with fill and stroke;
//! * smart objects become instances of a component per placed file: an embedded or linked PSD
//!   becomes the component's own layers, an embedded image its picture, a vector smart object
//!   (an Illustrator file saved with PDF content, read with craft-pdf) its paths as vector layers
//!   under Photoshop's picture of it, recoloured by a colour overlay; anything else (a missing
//!   link, artwork vectors can't hold) the pixels Photoshop rendered for it;
//! * drop and inner shadows, outer and inner glows, colour overlays and strokes become effects,
//!   fills and strokes; gradient overlays and gradient fill layers become gradient fills, pattern
//!   overlays and pattern fill layers tiled image fills of the file's pattern (painted into the
//!   pixels of pixel layers); layer and vector masks become mask layers; clipping masks become
//!   masked groups; ruler guides become guides.
//!
//! Adjustment layers, warped type, 3D and video layers, noise gradients, bevels, satin, and
//! effects FigCraft can't draw are left out or approximated, each with a note. Malformed input
//! never panics.

mod effects;
mod fills;
mod layers;
mod shape;
mod smart;
#[cfg(test)]
mod tests;
mod text;
mod util;
mod vector;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

use figcraft_doc::{Document, VectorNetwork};
use figcraft_psd::PsdFile;

use crate::FormatError;

/// Most distinct notes kept.
const MAX_NOTES: usize = 200;
/// Deepest smart object nesting whose placed file is opened (deeper ones keep Photoshop's pixels).
const MAX_SMART_DEPTH: usize = 3;
/// Most bytes of encoded images made from one file.
#[cfg(not(target_pointer_width = "32"))]
const MAX_IMAGE_TOTAL: u64 = 2 * 1024 * 1024 * 1024;
#[cfg(target_pointer_width = "32")]
const MAX_IMAGE_TOTAL: u64 = 384 * 1024 * 1024;

/// A file a smart object links to, as the PSD names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkedRef {
    /// The file's name (`header_980.psd`).
    pub name: String,
    /// Its path relative to the PSD, as Photoshop saved it (`links/980/header_980.psd`).
    pub rel_path: Option<String>,
    /// Its full path or `file://` URL on the machine that saved the PSD.
    pub full_path: Option<String>,
}

/// Finds the bytes of a linked file (the import itself reads no files).
pub type LinkedLookup = Box<dyn Fn(&LinkedRef) -> Option<Vec<u8>>>;

/// How to import.
#[derive(Default)]
pub struct Options {
    /// What the import may ask about the installed fonts.
    pub fonts: crate::xd::Fonts,
    /// Where linked smart object files come from.
    pub linked: Option<LinkedLookup>,
    /// Put Photoshop's own flattened picture of the file beside the import, to compare with.
    pub preview: bool,
}

/// Whether `bytes` look like a Photoshop document (`8BPS`, version 1 or 2).
pub fn is_psd(bytes: &[u8]) -> bool {
    bytes.len() >= 26 && bytes.starts_with(b"8BPS") && matches!(bytes.get(4..6), Some([0, 1] | [0, 2]))
}

/// Import `bytes` when they are a Photoshop document; `None` when they are something else.
pub fn try_import_with(bytes: &[u8], name: &str, options: Options) -> Option<Result<(Document, Vec<String>), FormatError>> {
    is_psd(bytes).then(|| import_with(bytes, name, options))
}

/// Build a document from a `.psd` or `.psb` file. Returns it with notes about what was left out
/// or approximated. `name`: the document name.
pub fn import(bytes: &[u8], name: &str) -> Result<(Document, Vec<String>), FormatError> {
    import_with(bytes, name, Options { preview: true, ..Options::default() })
}

/// [`import`] with `options`.
pub fn import_with(bytes: &[u8], name: &str, options: Options) -> Result<(Document, Vec<String>), FormatError> {
    let max_nodes = crate::fig::layer_budget(bytes.len());
    let file = PsdFile::from_bytes(bytes).map_err(|e| FormatError::Corrupt(format!("Photoshop file: {e}")))?;
    let mut sh = Shared::new(max_nodes, options);
    let json = layers::to_rest(&mut sh, Rc::new(file), name);
    let images = std::mem::take(&mut sh.images);
    let resolve = |r: &str| images.get(r).cloned();
    let networks = std::mem::take(&mut sh.networks);
    let mut warnings = sh.finish_notes();
    let (doc, more) = crate::rest::import_with_networks_plain(&json, &resolve, networks, max_nodes)?;
    warnings.extend(more);
    Ok((doc, warnings))
}

/// The import's state across the file and the files its smart objects place.
pub(crate) struct Shared {
    warnings: Vec<String>,
    seen: BTreeSet<String>,
    /// Encoded images by the `imageRef` the JSON gives them.
    pub images: HashMap<String, Vec<u8>>,
    image_by_hash: HashMap<u64, String>,
    image_bytes: u64,
    /// Image refs already made, by what they were made from.
    pub image_cache: HashMap<String, Option<String>>,
    /// Vector networks the JSON references by `vectorNetworkRef`.
    pub networks: Vec<Option<VectorNetwork>>,
    next_id: usize,
    nodes: usize,
    max_nodes: usize,
    pub fonts: crate::xd::Fonts,
    pub face_cache: HashMap<String, Option<(String, String)>>,
    pub metrics_cache: HashMap<(String, String), Option<(f64, f64)>>,
    pub linked: Option<LinkedLookup>,
    pub preview: bool,
    /// Component mains made so far (REST nodes for the Components page).
    pub mains: Vec<serde_json::Value>,
    /// Each component's node id and size, by what it stands for.
    pub components: HashMap<String, smart::Main>,
    /// Where the next component main goes on the Components page.
    pub main_cursor: (f64, f64, f64),
    /// Placed files opened so far, by their uuid (`None`: not a PSD, or unreadable).
    pub opened: HashMap<String, Option<Rc<PsdFile>>>,
    /// How deep in placed files the import is.
    pub depth: usize,
    /// Things approximated or left out, counted, for one note each.
    pub counts: BTreeMap<&'static str, usize>,
    /// The next id given to a file being mapped (keys the image cache).
    pub next_file: usize,
    /// Decoded pattern tiles, by file id and pattern id.
    pub tiles: HashMap<(usize, String), Option<Rc<fills::Tile>>>,
    /// The files whose patterns were read.
    pub tiles_read: std::collections::HashSet<usize>,
    /// The first uuid of each embedded file's bytes, by file id, hash and length.
    pub same_file: HashMap<(usize, u64, usize), String>,
    /// Vector smart objects read so far, by the uuid of their file (`None`: not kept as vectors).
    pub vectors: HashMap<String, Option<Rc<vector::Art>>>,
    /// Each mapped file's parsed `Txt2`, by its id.
    pub txt2_cache: HashMap<usize, Rc<Option<figcraft_psd::engine_data::Value>>>,
}

impl Shared {
    fn new(max_nodes: usize, options: Options) -> Self {
        Shared {
            warnings: Vec::new(),
            seen: BTreeSet::new(),
            images: HashMap::new(),
            image_by_hash: HashMap::new(),
            image_bytes: 0,
            image_cache: HashMap::new(),
            networks: Vec::new(),
            next_id: 0,
            nodes: 0,
            max_nodes,
            fonts: options.fonts,
            face_cache: HashMap::new(),
            metrics_cache: HashMap::new(),
            linked: options.linked,
            preview: options.preview,
            mains: Vec::new(),
            components: HashMap::new(),
            main_cursor: (0.0, 0.0, 0.0),
            opened: HashMap::new(),
            depth: 0,
            counts: BTreeMap::new(),
            next_file: 0,
            txt2_cache: HashMap::new(),
            vectors: HashMap::new(),
            same_file: HashMap::new(),
            tiles: HashMap::new(),
            tiles_read: std::collections::HashSet::new(),
        }
    }

    pub fn warn(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.seen.len() > MAX_NOTES || !self.seen.insert(msg.clone()) {
            return;
        }
        if self.seen.len() > MAX_NOTES {
            self.warnings.push("more notes were left out".into());
        } else {
            self.warnings.push(msg);
        }
    }

    /// Count one more thing approximated or left out (one note per kind at the end).
    pub fn note(&mut self, what: &'static str) {
        *self.counts.entry(what).or_default() += 1;
    }

    /// A node id not given yet.
    pub fn id(&mut self) -> String {
        self.next_id += 1;
        format!("psd:{}", self.next_id)
    }

    /// Count one more layer; false (with a note) once the file's budget is spent.
    pub fn count(&mut self) -> bool {
        if self.nodes >= self.max_nodes {
            self.warn(format!("the file has more than {} layers; the rest were skipped", self.max_nodes));
            return false;
        }
        self.nodes += 1;
        true
    }

    /// Keep an encoded image; its `imageRef` (the same for the same bytes), or `None` past the
    /// image budget.
    pub fn add_image(&mut self, png: Vec<u8>) -> Option<String> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        png.hash(&mut h);
        let key = h.finish();
        if let Some(r) = self.image_by_hash.get(&key)
            && self.images.get(r).is_some_and(|b| *b == png)
        {
            return Some(r.clone());
        }
        let len = png.len() as u64;
        if self.image_bytes.saturating_add(len) > MAX_IMAGE_TOTAL {
            self.warn("the file's pictures are larger than the import keeps; some layers were left empty");
            return None;
        }
        self.image_bytes += len;
        let r = format!("psd-image:{}", self.images.len());
        self.images.insert(r.clone(), png);
        self.image_by_hash.insert(key, r.clone());
        Some(r)
    }

    fn finish_notes(&mut self) -> Vec<String> {
        let counts = std::mem::take(&mut self.counts);
        for (what, n) in counts {
            self.warn(format!("{n} {what}"));
        }
        std::mem::take(&mut self.warnings)
    }
}

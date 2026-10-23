//! Tool #5 — PDF Input: loads a PDF file and analyses each page's content.

pub mod analyze;
mod fonts;
pub mod geometry;
pub mod images;

use std::{borrow::Cow, fs, path::PathBuf, sync::Arc};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PdfPageInfo, PortSpec, Value,
};
use serde::{Deserialize, Serialize};

use crate::runtime::{FileStamp, RuntimeState};

pub use analyze::{Options, Stats, analyze_page};

/// Name of the output port carrying the loaded document.
pub const OUTPUT: &str = "pdf";

const OUTPUTS: &[PortSpec] = &[PortSpec::document(OUTPUT)];

const PT_PER_MM: f32 = 72.0 / 25.4;

/// Which box represents a page's artwork.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bounds {
    /// Tight bounds of everything visibly painted (vectors, images, text).
    #[default]
    Content,
    /// The page's visible area (CropBox).
    CropBox,
    /// The full page (MediaBox).
    MediaBox,
}

/// Loads a PDF and reports, for every page, the bounding box of its actual
/// content and the images painted on it.
///
/// The output carries the original file unchanged plus that analysis in
/// [`PdfDocument::page_info`], so layout tools can place exactly the artwork
/// rather than the whole page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfInput {
    /// Source file; relative paths resolve against the graph's base directory.
    pub path: PathBuf,
    pub bounds: Bounds,
    /// Ignore areas filled with plain white (e.g. page-sized backgrounds),
    /// since they are invisible on white paper.
    pub ignore_white_fills: bool,
    /// Extra space around the content box, in mm (kept within the page).
    pub padding_mm: f32,
    /// Trim every page to its content box, so the output holds just the
    /// artwork. When off, the file passes through unchanged and the box is
    /// only reported.
    pub crop_to_content: bool,
    /// Keys of embedded images to extract (see [`images::list`]). Each
    /// becomes an image output port of the same name, in this order.
    pub selected_images: Vec<String>,
    /// File state seen by the last load attempt.
    #[serde(skip)]
    observed: RuntimeState<Option<Option<FileStamp>>>,
}

impl Default for PdfInput {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            bounds: Bounds::Content,
            ignore_white_fills: true,
            padding_mm: 0.0,
            crop_to_content: true,
            selected_images: Vec::new(),
            observed: RuntimeState::default(),
        }
    }
}

impl PdfInput {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    /// Loads and analyses the file. Usable on its own, outside any graph.
    pub fn load(&self, ctx: &EvalContext) -> Result<PdfDocument, NodeError> {
        if self.path.as_os_str().is_empty() {
            return Err(NodeError::InvalidParam {
                name: "path",
                reason: "no file selected".into(),
            });
        }
        if !(self.padding_mm.is_finite() && self.padding_mm >= 0.0) {
            return Err(NodeError::InvalidParam {
                name: "padding_mm",
                reason: "must be zero or more".into(),
            });
        }
        let path = ctx.resolve_path(&self.path);
        self.observed.set(Some(FileStamp::read(&path)));
        let bytes = fs::read(&path).map_err(|source| NodeError::Io {
            path: path.clone(),
            source,
        })?;
        analyze_bytes(bytes, self)
            .map_err(|reason| NodeError::Other(format!("{}: {reason}", path.display())))
    }
}

/// Parses `bytes` and builds the document with per-page analysis.
fn analyze_bytes(bytes: Vec<u8>, settings: &PdfInput) -> Result<PdfDocument, String> {
    let mut doc =
        lopdf::Document::load_mem(&bytes).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.is_encrypted() && doc.get_pages().is_empty() {
        return Err("the PDF is password-protected".into());
    }
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err("the PDF has no pages".into());
    }

    let opts = Options {
        ignore_white_fills: settings.ignore_white_fills,
    };
    let mut page_info = Vec::with_capacity(pages.len());
    let mut stats = Vec::with_capacity(pages.len());
    for &page_id in pages.values() {
        let (mut info, page_stats) = analyze_page(&doc, page_id, opts);
        info.content_box = match settings.bounds {
            Bounds::Content => info.content_box.map(|[x0, y0, x1, y1]| {
                let pad = settings.padding_mm * PT_PER_MM;
                let [mx0, my0, mx1, my1] = info.media_box;
                [
                    (x0 - pad).max(mx0),
                    (y0 - pad).max(my0),
                    (x1 + pad).min(mx1),
                    (y1 + pad).min(my1),
                ]
            }),
            Bounds::CropBox => Some(info.crop_box),
            Bounds::MediaBox => Some(info.media_box),
        };
        page_info.push(info);
        stats.push(page_stats);
    }

    // Summarise in original page coordinates, before cropping moves the page.
    images::assign_keys(&mut page_info);
    let summary = summarize(
        &page_info,
        &stats,
        settings.bounds,
        settings.crop_to_content,
    );
    let mut bytes = bytes;
    if settings.crop_to_content && crop_pages(&mut doc, &pages, &mut page_info)? {
        bytes = save(&mut doc)?;
    }
    let pages_mm = page_sizes_mm(&page_info);
    Ok(PdfDocument {
        bytes,
        pages_mm,
        summary,
        page_info,
    })
}

/// Displayed page sizes (rotation applied), in mm.
fn page_sizes_mm(pages: &[PdfPageInfo]) -> Vec<[f32; 2]> {
    pages
        .iter()
        .map(|p| {
            let [x0, y0, x1, y1] = p.crop_box;
            let (w, h) = ((x1 - x0) / PT_PER_MM, (y1 - y0) / PT_PER_MM);
            if p.rotation % 180 == 90 {
                [h, w]
            } else {
                [w, h]
            }
        })
        .collect()
}

/// Sets each page's MediaBox and CropBox to its content box, as the
/// reference grid tool does. Content streams are untouched, so the art stays
/// vector and its coordinates stay valid. Unlike the reference, `/Rotate` is
/// kept so rotated pages still display upright. Returns whether any page
/// changed.
fn crop_pages(
    doc: &mut lopdf::Document,
    pages: &std::collections::BTreeMap<u32, lopdf::ObjectId>,
    info: &mut [PdfPageInfo],
) -> Result<bool, String> {
    let mut changed = false;
    for (&page_id, page) in pages.values().zip(info.iter_mut()) {
        let Some(b) = page.content_box else { continue };
        // A zero-width line or empty box cannot be a page.
        if b[2] - b[0] < 1.0 || b[3] - b[1] < 1.0 || b == page.media_box && b == page.crop_box {
            continue;
        }
        let rect = || lopdf::Object::Array(b.iter().map(|v| lopdf::Object::Real(*v)).collect());
        let dict = doc
            .get_dictionary_mut(page_id)
            .map_err(|e| format!("cannot edit page ({e})"))?;
        dict.set("MediaBox", rect());
        dict.set("CropBox", rect());
        // These must lie within the MediaBox; the content box replaces them.
        for key in [&b"BleedBox"[..], b"TrimBox", b"ArtBox"] {
            dict.remove(key);
        }
        page.media_box = b;
        page.crop_box = b;
        changed = true;
    }
    Ok(changed)
}

fn save(doc: &mut lopdf::Document) -> Result<Vec<u8>, String> {
    // lopdf decrypted the objects while loading; write a plain copy rather
    // than one whose /Encrypt dictionary no longer matches its contents.
    if doc.trailer.remove(b"Encrypt").is_some() {
        doc.encryption_state = None;
    }
    let mut out = Vec::new();
    doc.save_to(&mut out)
        .map_err(|e| format!("cannot write the cropped PDF ({e})"))?;
    Ok(out)
}

fn summarize(pages: &[PdfPageInfo], stats: &[Stats], bounds: Bounds, cropped: bool) -> Vec<String> {
    let first = &pages[0];
    let what = match bounds {
        Bounds::Content => "content",
        Bounds::CropBox => "crop box",
        Bounds::MediaBox => "page",
    };
    let size = match (first.content_size_mm(), first.content_box) {
        (Some([w, h]), Some([x0, y0, ..])) => {
            let (x, y) = (x0 / PT_PER_MM, y0 / PT_PER_MM);
            if cropped {
                format!(
                    "cropped to {what} {w:.1} × {h:.1} mm (was at {x:.1}, {y:.1} mm on the page)"
                )
            } else {
                format!("{what} {w:.1} × {h:.1} mm at ({x:.1}, {y:.1}) mm")
            }
        }
        _ => "blank".into(),
    };
    let s = &stats[0];
    let mut parts = Vec::new();
    for (n, one, many) in [
        (s.paths, "path", "paths"),
        (s.images, "image", "images"),
        (s.text_runs, "text run", "text runs"),
    ] {
        if n > 0 {
            parts.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    }
    let pages_label = if pages.len() == 1 {
        "1 page".to_owned()
    } else {
        format!("{} pages", pages.len())
    };
    let page_ref = if pages.len() == 1 { "" } else { "page 1 " };
    let mut summary = vec![format!(
        "PDF {pages_label} · {page_ref}{size}{}",
        if parts.is_empty() {
            String::new()
        } else {
            format!(" · {}", parts.join(", "))
        }
    )];

    let blank: Vec<String> = pages
        .iter()
        .enumerate()
        .filter(|(_, p)| p.content_box.is_none())
        .map(|(i, _)| (i + 1).to_string())
        .collect();
    if !blank.is_empty() && bounds == Bounds::Content {
        summary.push(format!("No visible content on page {}.", blank.join(", ")));
    }
    for (i, s) in stats.iter().enumerate() {
        for w in &s.warnings {
            summary.push(format!("Page {}: {w}.", i + 1));
        }
    }
    summary
}

impl Node for PdfInput {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        if self.selected_images.is_empty() {
            return Cow::Borrowed(OUTPUTS);
        }
        let mut ports = OUTPUTS.to_vec();
        ports.extend(
            self.selected_images
                .iter()
                .map(|key| PortSpec::new(key.clone(), docforge_core::PortType::Image)),
        );
        Cow::Owned(ports)
    }

    fn evaluate(&self, _: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let doc = self.load(ctx)?;
        let keys: Vec<&str> = self.selected_images.iter().map(String::as_str).collect();
        let mut outputs = Vec::with_capacity(1 + keys.len());
        let extracted = images::extract(&doc, &keys, None);
        outputs.push(Value::Document(Arc::new(doc)));
        for (key, image) in keys.iter().zip(extracted) {
            let image =
                image.map_err(|reason| NodeError::Other(format!("image `{key}`: {reason}")))?;
            outputs.push(Value::from(image));
        }
        Ok(outputs)
    }

    fn needs_refresh(&self, ctx: &EvalContext) -> bool {
        self.observed
            .get()
            .is_some_and(|seen| seen != FileStamp::read(&ctx.resolve_path(&self.path)))
    }
}

impl NodeType for PdfInput {
    const KIND: &'static str = "pdf_input";
    const DISPLAY_NAME: &'static str = "PDF Input";
    const CATEGORY: &'static str = "Input / Output";
}

#[cfg(test)]
mod tests;

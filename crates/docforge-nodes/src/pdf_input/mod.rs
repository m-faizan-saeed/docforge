//! Tool #5 — PDF Input: loads a PDF file and provides it, plus any of its
//! embedded images, to downstream nodes. It never changes the PDF; cropping
//! and recolouring belong to PDF Edit.

pub mod images;

use std::{borrow::Cow, path::PathBuf, sync::Arc};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PortSpec, Value, files,
};
use serde::{Deserialize, Serialize};

use crate::{
    pdf::{
        self,
        analyze::{Options, Stats, analyze_page},
    },
    runtime::{FileStamp, RuntimeState},
};

/// Name of the output port carrying the loaded document.
pub const OUTPUT: &str = "pdf";

const OUTPUTS: &[PortSpec] = &[PortSpec::document(OUTPUT)];

/// Loads a PDF unchanged and lists, for every page, its boxes, rotation and
/// the images painted on it. Each page's `content_box` is its whole visible
/// area (use PDF Edit to find and crop to the actual artwork).
///
/// Images whose keys are in `selected_images` are extracted and offered on
/// output ports of the same names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfInput {
    /// Source file; relative paths resolve against the graph's base directory.
    pub path: PathBuf,
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

    /// Loads and inspects the file. Usable on its own, outside any graph.
    pub fn load(&self, ctx: &EvalContext) -> Result<PdfDocument, NodeError> {
        if self.path.as_os_str().is_empty() {
            return Err(NodeError::InvalidParam {
                name: "path",
                reason: "no file selected".into(),
            });
        }
        let path = ctx.resolve_path(&self.path);
        self.observed.set(Some(FileStamp::read(&path)));
        let bytes = files::read(&path).map_err(|source| NodeError::Io {
            path: path.clone(),
            source,
        })?;
        inspect(bytes).map_err(|reason| NodeError::Other(format!("{}: {reason}", path.display())))
    }
}

/// Parses `bytes` and describes every page, leaving the file untouched.
fn inspect(bytes: Vec<u8>) -> Result<PdfDocument, String> {
    let doc = lopdf::Document::load_mem(&bytes).map_err(|e| format!("not a readable PDF ({e})"))?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err(if doc.is_encrypted() {
            "the PDF is password-protected".into()
        } else {
            "the PDF has no pages".into()
        });
    }
    let mut page_info = Vec::with_capacity(pages.len());
    let mut stats: Vec<Stats> = Vec::with_capacity(pages.len());
    for &page in pages.values() {
        // The walk lists the images; the measured content box is not used
        // here — every page counts as a whole.
        let (mut info, page_stats) = analyze_page(
            &doc,
            page,
            Options {
                ignore_white_fills: true,
            },
        );
        info.content_box = Some(info.crop_box);
        page_info.push(info);
        stats.push(page_stats);
    }
    images::assign_keys(&mut page_info);
    let pages_mm = pdf::page_sizes_mm(&page_info);

    let image_count = images::list_pages(&page_info).len();
    let [w, h] = pages_mm[0];
    let mut summary = vec![format!(
        "PDF {} · {}{w:.1} × {h:.1} mm{}",
        if pages_mm.len() == 1 {
            "1 page".to_owned()
        } else {
            format!("{} pages", pages_mm.len())
        },
        if pages_mm.len() == 1 { "" } else { "page 1 " },
        match image_count {
            0 => String::new(),
            1 => " · 1 embedded image".into(),
            n => format!(" · {n} embedded images"),
        }
    )];
    for (i, s) in stats.iter().enumerate() {
        for w in &s.warnings {
            summary.push(format!("Page {}: {w}.", i + 1));
        }
    }
    Ok(PdfDocument {
        bytes,
        pages_mm,
        summary,
        page_info,
    })
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

//! Tool #6 — PDF Edit: crops pages (to their detected content or a manual
//! box) and changes the colours of vector graphics, keeping the PDF vector.

mod migrate;
pub mod recolor;

use std::{borrow::Cow, sync::Arc};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PdfPageInfo, PortSpec, Value,
};
use lopdf::Object;
use serde::{Deserialize, Serialize};

pub use migrate::migrate_pdf_input_crop;
pub use recolor::{ColorRule, PaletteEntry, Recolor, RecolorMode, RecolorStats, palette};

use crate::{
    pdf::{
        self, PT_PER_MM, PageBox,
        analyze::{Options, Stats, analyze_page},
        displayed_to_page,
    },
    pdf_input::images,
};

pub const INPUT: &str = "pdf";
pub const OUTPUT: &str = "pdf";

const INPUTS: &[PortSpec] = &[PortSpec::document(INPUT)];
const OUTPUTS: &[PortSpec] = &[PortSpec::document(OUTPUT)];

/// How pages are cropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CropMode {
    /// To the tight bounds of everything visibly painted.
    #[default]
    Content,
    /// To `manual_box`.
    Manual,
    /// Pages keep their size.
    None,
}

/// Crops PDF pages and recolours their vector graphics.
///
/// Cropping only changes the page boxes (MediaBox/CropBox); the drawing is
/// untouched, so the art stays vector and its coordinates stay valid. The
/// content box is measured on the original colours, so recolouring cannot
/// change what counts as content.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfEdit {
    pub crop: CropMode,
    /// `content` crop: extra space around the content, mm (kept on the page).
    pub padding_mm: f32,
    /// `content` crop: plain-white filled shapes don't count as content.
    pub ignore_white_fills: bool,
    /// `manual` crop: `[x0, y0, x1, y1]` as fractions of each page as
    /// displayed (rotation applied; origin top-left, y down).
    pub manual_box: [f32; 4],
    pub recolor: Recolor,
}

impl Default for PdfEdit {
    fn default() -> Self {
        Self {
            crop: CropMode::Content,
            padding_mm: 0.0,
            ignore_white_fills: true,
            manual_box: [0.1, 0.1, 0.9, 0.9],
            recolor: Recolor::default(),
        }
    }
}

impl PdfEdit {
    fn validate(&self) -> Result<(), NodeError> {
        if !(self.padding_mm.is_finite() && self.padding_mm >= 0.0) {
            return Err(NodeError::InvalidParam {
                name: "padding_mm",
                reason: "must be zero or more".into(),
            });
        }
        let [x0, y0, x1, y1] = self.manual_box;
        let inside = self.manual_box.iter().all(|v| (0.0..=1.0).contains(v));
        if self.crop == CropMode::Manual && !(inside && x1 > x0 && y1 > y0) {
            return Err(NodeError::InvalidParam {
                name: "manual_box",
                reason: "must be a non-empty box within the page (fractions 0–1)".into(),
            });
        }
        Ok(())
    }

    /// The box each page would be cropped to (page points), from the
    /// analysis of the original page; `None` keeps the page as it is.
    fn target_box(&self, info: &PdfPageInfo) -> Option<[f32; 4]> {
        let [mx0, my0, mx1, my1] = info.media_box;
        let clamp =
            |[x0, y0, x1, y1]: [f32; 4]| [x0.max(mx0), y0.max(my0), x1.min(mx1), y1.min(my1)];
        match self.crop {
            CropMode::Content => info.content_box.map(|[x0, y0, x1, y1]| {
                let pad = self.padding_mm * PT_PER_MM;
                clamp([x0 - pad, y0 - pad, x1 + pad, y1 + pad])
            }),
            CropMode::Manual => Some(clamp(displayed_to_page(
                self.manual_box,
                PageBox {
                    crop: info.crop_box,
                    rotation: info.rotation,
                },
            ))),
            CropMode::None => None,
        }
    }

    /// Applies the edits to `src`. Usable on its own, outside any graph.
    pub fn apply(&self, src: &PdfDocument) -> Result<PdfDocument, NodeError> {
        self.validate()?;
        let fail = |reason: String| NodeError::Other(reason);
        let mut doc = lopdf::Document::load_mem(&src.bytes)
            .map_err(|e| fail(format!("the PDF cannot be read ({e})")))?;
        let pages: Vec<lopdf::ObjectId> = doc.get_pages().values().copied().collect();

        // 1. Measure on the original drawing.
        let opts = Options {
            ignore_white_fills: self.ignore_white_fills,
        };
        let (mut infos, stats): (Vec<PdfPageInfo>, Vec<Stats>) =
            pages.iter().map(|&p| analyze_page(&doc, p, opts)).unzip();
        let boxes: Vec<Option<[f32; 4]>> = infos.iter().map(|i| self.target_box(i)).collect();

        // 2. Recolour.
        let recolored = self
            .recolor
            .is_active()
            .then(|| recolor::recolor_document(&mut doc, &self.recolor));

        // 3. Crop: page boxes only.
        let mut cropped = 0;
        for ((&page, info), target) in pages.iter().zip(&mut infos).zip(&boxes) {
            match target {
                Some(b) if b[2] - b[0] >= 1.0 && b[3] - b[1] >= 1.0 => {
                    if *b != info.crop_box || *b != info.media_box {
                        set_page_box(&mut doc, page, *b).map_err(fail)?;
                        info.media_box = *b;
                        info.crop_box = *b;
                        cropped += 1;
                    }
                    info.content_box = Some(*b);
                }
                // Blank page in content mode: left as it is, nothing to print.
                _ if self.crop == CropMode::Content => info.content_box = None,
                _ => info.content_box = Some(info.crop_box),
            }
        }

        let changed = cropped > 0 || recolored.as_ref().is_some_and(|s| s.changed > 0);
        let bytes = if changed {
            pdf::save(&mut doc).map_err(fail)?
        } else {
            src.bytes.clone()
        };
        images::assign_keys(&mut infos);
        let pages_mm = pdf::page_sizes_mm(&infos);
        let summary = self.summarize(&infos, &stats, &pages_mm, recolored.as_ref());
        Ok(PdfDocument {
            bytes,
            pages_mm,
            summary,
            page_info: infos,
        })
    }

    fn summarize(
        &self,
        infos: &[PdfPageInfo],
        stats: &[Stats],
        pages_mm: &[[f32; 2]],
        recolored: Option<&RecolorStats>,
    ) -> Vec<String> {
        let n = infos.len();
        let mut first = format!(
            "PDF {}",
            if n == 1 {
                "1 page".to_owned()
            } else {
                format!("{n} pages")
            }
        );
        if let Some([w, h]) = pages_mm.first() {
            let what = match self.crop {
                CropMode::Content => "cropped to content",
                CropMode::Manual => "cropped",
                CropMode::None => "",
            };
            first += &format!(
                " · {}{}{w:.1} × {h:.1} mm",
                if n > 1 { "page 1 " } else { "" },
                if what.is_empty() {
                    String::new()
                } else {
                    format!("{what} ")
                }
            );
        }
        if let Some(r) = recolored {
            first += &format!(
                " · {} colour{} changed",
                r.changed,
                if r.changed == 1 { "" } else { "s" }
            );
        }
        let mut summary = vec![first];
        if self.crop == CropMode::Content {
            let blank: Vec<String> = infos
                .iter()
                .enumerate()
                .filter(|(_, p)| p.content_box.is_none())
                .map(|(i, _)| (i + 1).to_string())
                .collect();
            if !blank.is_empty() {
                summary.push(format!("No visible content on page {}.", blank.join(", ")));
            }
        }
        if let Some(r) = recolored {
            if r.untouched_shadings > 0 {
                summary.push(format!(
                    "{} gradient/pattern fill(s) were left in their colours.",
                    r.untouched_shadings
                ));
            }
            if r.unreadable_streams > 0 {
                summary.push(format!(
                    "{} drawing stream(s) could not be read and were left unchanged.",
                    r.unreadable_streams
                ));
            }
        }
        for (i, s) in stats.iter().enumerate() {
            for w in &s.warnings {
                summary.push(format!("Page {}: {w}.", i + 1));
            }
        }
        summary
    }
}

/// Makes `b` the page's MediaBox and CropBox (and drops the boxes that must
/// lie within them). `/Rotate` is kept so rotated pages still show upright.
fn set_page_box(
    doc: &mut lopdf::Document,
    page: lopdf::ObjectId,
    b: [f32; 4],
) -> Result<(), String> {
    let rect = || Object::Array(b.iter().map(|v| Object::Real(*v)).collect());
    let dict = doc
        .get_dictionary_mut(page)
        .map_err(|e| format!("cannot edit page ({e})"))?;
    dict.set("MediaBox", rect());
    dict.set("CropBox", rect());
    for key in [&b"BleedBox"[..], b"TrimBox", b"ArtBox"] {
        dict.remove(key);
    }
    Ok(())
}

impl Node for PdfEdit {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(INPUTS)
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(OUTPUTS)
    }

    fn evaluate(&self, inputs: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let src = inputs.document(INPUT)?;
        Ok(vec![Value::Document(Arc::new(self.apply(src)?))])
    }
}

impl NodeType for PdfEdit {
    const KIND: &'static str = "pdf_edit";
    const DISPLAY_NAME: &'static str = "PDF Edit";
    const CATEGORY: &'static str = "Processing";
}

#[cfg(test)]
mod tests;

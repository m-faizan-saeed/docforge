//! Tool #4 — Print Layout: packs images onto paper at their real size and
//! produces a printable PDF with cutting lines.

pub mod pack;
mod pdf;

use std::{
    borrow::Cow,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PortSpec, RasterImage, Value,
};
use serde::{Deserialize, Serialize};

pub use pack::{Orientation, Packing, Placement};
pub use pdf::CutMarks;

use crate::{output_image::write_atomic, runtime::RuntimeState};

/// Multi-input port: every connected image becomes one item, in connection order.
pub const INPUT: &str = "images";
pub const OUTPUT: &str = "pdf";

const INPUTS: &[PortSpec] = &[PortSpec::image(INPUT).multi()];
const OUTPUTS: &[PortSpec] = &[PortSpec::document(OUTPUT)];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Paper {
    #[default]
    A4,
    A3,
    A5,
    Letter,
    Legal,
    /// Uses `custom_paper_mm`.
    Custom,
}

impl Paper {
    pub const ALL: [Paper; 6] = [
        Self::A4,
        Self::A3,
        Self::A5,
        Self::Letter,
        Self::Legal,
        Self::Custom,
    ];

    /// Portrait `[width, height]` in mm, `None` for custom.
    pub fn size_mm(self) -> Option<[f32; 2]> {
        Some(match self {
            Self::A4 => [210.0, 297.0],
            Self::A3 => [297.0, 420.0],
            Self::A5 => [148.0, 210.0],
            Self::Letter => [215.9, 279.4],
            Self::Legal => [215.9, 355.6],
            Self::Custom => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::A4 => "A4",
            Self::A3 => "A3",
            Self::A5 => "A5",
            Self::Letter => "US Letter",
            Self::Legal => "US Legal",
            Self::Custom => "Custom",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageOrientation {
    /// Whichever needs fewer pages.
    #[default]
    Auto,
    Portrait,
    Landscape,
}

/// How large an item is printed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ItemSize {
    /// Real size from the image's DPI, or `fallback_dpi` if it has none.
    FromDpi { fallback_dpi: f32 },
    /// Given width; height follows the image's proportions.
    Width { width_mm: f32 },
    /// Exactly this size (the image is stretched if proportions differ),
    /// e.g. 85.6 × 54 mm for an ID card.
    Exact { width_mm: f32, height_mm: f32 },
}

impl Default for ItemSize {
    fn default() -> Self {
        Self::FromDpi {
            fallback_dpi: 300.0,
        }
    }
}

impl ItemSize {
    /// Common real-world sizes for quick selection.
    pub const PRESETS: [(&'static str, f32, f32); 7] = [
        ("ID card / credit card (85.6 × 54)", 85.6, 54.0),
        ("Passport photo (35 × 45)", 35.0, 45.0),
        ("Business card (85 × 55)", 85.0, 55.0),
        ("Photo 10 × 15 cm", 101.6, 152.4),
        ("A6 (105 × 148)", 105.0, 148.0),
        ("A5 (148 × 210)", 148.0, 210.0),
        ("A4 (210 × 297)", 210.0, 297.0),
    ];

    /// `[width, height]` in mm for `img`, plus a note if the DPI was assumed.
    pub fn resolve(&self, img: &RasterImage) -> ([f32; 2], Option<String>) {
        let (w, h) = (img.width().max(1) as f32, img.height().max(1) as f32);
        match *self {
            Self::FromDpi { fallback_dpi } => {
                let (dpi, note) = match img.dpi() {
                    Some(dpi) => (dpi, None),
                    None => (
                        fallback_dpi.max(1.0),
                        Some(format!("has no DPI; assumed {fallback_dpi:.0}")),
                    ),
                };
                ([w / dpi * 25.4, h / dpi * 25.4], note)
            }
            Self::Width { width_mm } => ([width_mm, width_mm * h / w], None),
            Self::Exact {
                width_mm,
                height_mm,
            } => ([width_mm, height_mm], None),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintItem {
    pub size: ItemSize,
    pub copies: u32,
}

impl Default for PrintItem {
    fn default() -> Self {
        Self {
            size: ItemSize::default(),
            copies: 1,
        }
    }
}

/// Lays out every connected image (with its copies) on as few sheets as
/// possible and renders a PDF with cutting lines.
///
/// `items[i]` configures the i-th connected image; images without an entry
/// use [`PrintItem::default`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrintLayout {
    pub items: Vec<PrintItem>,
    pub paper: Paper,
    /// Paper size for [`Paper::Custom`], `[width, height]` in mm.
    pub custom_paper_mm: [f32; 2],
    pub orientation: PageOrientation,
    /// Unprinted border on every side, mm.
    pub margin_mm: f32,
    /// Space between pieces, mm.
    pub gap_mm: f32,
    pub allow_rotation: bool,
    pub cut_marks: CutMarks,
    /// Maximum resolution of embedded images.
    pub image_dpi: f32,
    /// JPEG quality of embedded images; `None` embeds them losslessly.
    pub jpeg_quality: Option<u8>,
    /// Where to save the PDF; nothing is written while empty.
    pub path: PathBuf,
    pub auto_save: bool,
    #[serde(skip)]
    last_written: RuntimeState<Option<PathBuf>>,
}

impl Default for PrintLayout {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            paper: Paper::A4,
            custom_paper_mm: [210.0, 297.0],
            orientation: PageOrientation::Auto,
            margin_mm: 10.0,
            gap_mm: 4.0,
            allow_rotation: true,
            cut_marks: CutMarks::Outline,
            image_dpi: 300.0,
            jpeg_quality: Some(90),
            path: PathBuf::new(),
            auto_save: true,
            last_written: RuntimeState::default(),
        }
    }
}

impl PrintLayout {
    /// Settings for the i-th connected image.
    pub fn item(&self, index: usize) -> PrintItem {
        self.items.get(index).cloned().unwrap_or_default()
    }

    /// Settings for the i-th image, creating defaults up to it if needed.
    pub fn item_mut(&mut self, index: usize) -> &mut PrintItem {
        if self.items.len() <= index {
            self.items.resize_with(index + 1, PrintItem::default);
        }
        &mut self.items[index]
    }

    pub fn paper_mm(&self) -> [f32; 2] {
        self.paper.size_mm().unwrap_or(self.custom_paper_mm)
    }

    pub fn last_written_path(&self) -> Option<PathBuf> {
        self.last_written.get()
    }

    fn validate(&self) -> Result<(), NodeError> {
        let positive = |v: f32| v.is_finite() && v > 0.0;
        let invalid = |name, reason: &str| {
            Err(NodeError::InvalidParam {
                name,
                reason: reason.into(),
            })
        };
        if !self.paper_mm().into_iter().all(positive) {
            return invalid("custom_paper_mm", "paper size must be positive");
        }
        if !(self.margin_mm.is_finite() && self.margin_mm >= 0.0) {
            return invalid("margin_mm", "must be zero or more");
        }
        if !(self.gap_mm.is_finite() && self.gap_mm >= 0.0) {
            return invalid("gap_mm", "must be zero or more");
        }
        if !positive(self.image_dpi) {
            return invalid("image_dpi", "must be positive");
        }
        for (i, item) in self.items.iter().enumerate() {
            let ok = match item.size {
                ItemSize::FromDpi { fallback_dpi } => positive(fallback_dpi),
                ItemSize::Width { width_mm } => positive(width_mm),
                ItemSize::Exact {
                    width_mm,
                    height_mm,
                } => positive(width_mm) && positive(height_mm),
            };
            if !ok {
                return Err(NodeError::InvalidParam {
                    name: "items",
                    reason: format!("item {} needs a positive size", i + 1),
                });
            }
        }
        Ok(())
    }

    /// Lays out `images` and renders the PDF. Usable outside a graph.
    pub fn build(&self, images: &[&RasterImage]) -> Result<PdfDocument, NodeError> {
        self.validate()?;
        let mut notes = Vec::new();
        let (sizes, copies): (Vec<[f32; 2]>, Vec<u32>) = images
            .iter()
            .enumerate()
            .map(|(i, img)| {
                let item = self.item(i);
                let (size, note) = item.size.resolve(img);
                if let Some(note) = note {
                    notes.push(format!("Image {} {note}.", i + 1));
                }
                (size, item.copies)
            })
            .unzip();
        if copies.iter().all(|&c| c == 0) {
            return Err(NodeError::InvalidParam {
                name: "items",
                reason: "every item has 0 copies".into(),
            });
        }

        let packing = pack::pack(&pack::Request {
            sizes: &sizes,
            copies: &copies,
            paper: self.paper_mm(),
            orientation: match self.orientation {
                PageOrientation::Auto => Orientation::Auto,
                PageOrientation::Portrait => Orientation::Portrait,
                PageOrientation::Landscape => Orientation::Landscape,
            },
            margin: self.margin_mm,
            gap: self.gap_mm,
            allow_rotation: self.allow_rotation,
        })
        .ok_or_else(|| NodeError::InvalidParam {
            name: "margin_mm",
            reason: "the margins leave no printable area".into(),
        })?;

        let scaled: Vec<[f32; 2]> = sizes
            .iter()
            .zip(&packing.scale)
            .map(|(s, k)| s.map(|v| v * k))
            .collect();
        for (i, &k) in packing.scale.iter().enumerate() {
            if k < 0.999 && copies[i] > 0 {
                notes.push(format!(
                    "Image {} was shrunk to {:.0}% to fit on the paper.",
                    i + 1,
                    k * 100.0
                ));
            }
        }

        let bytes = pdf::write(
            &packing,
            images,
            &scaled,
            &pdf::Options {
                dpi: self.image_dpi,
                jpeg_quality: self.jpeg_quality,
                cut_marks: self.cut_marks,
                gap: self.gap_mm,
                title: "docforge print layout".into(),
            },
        )?;

        let pieces = packing.placements.len();
        let landscape = packing.page[0] > packing.page[1];
        let mut summary = vec![format!(
            "{} {} on {} {} {} · {:.0}% of the paper used",
            pieces,
            if pieces == 1 { "piece" } else { "pieces" },
            packing.pages,
            self.paper.name(),
            match (packing.pages, landscape) {
                (1, true) => "page (landscape)",
                (1, false) => "page (portrait)",
                (_, true) => "pages (landscape)",
                (_, false) => "pages (portrait)",
            },
            packing.utilisation() * 100.0,
        )];
        summary.extend(notes);
        Ok(PdfDocument {
            bytes,
            pages_mm: vec![packing.page; packing.pages],
            summary,
        })
    }

    /// Writes `doc` to the configured path (atomically) and returns it.
    pub fn save(&self, doc: &PdfDocument, ctx: &EvalContext) -> Result<PathBuf, NodeError> {
        if self.path.as_os_str().is_empty() {
            return Err(NodeError::InvalidParam {
                name: "path",
                reason: "no PDF file chosen".into(),
            });
        }
        let mut path = ctx.resolve_path(&self.path);
        if path.extension().is_none() {
            path.set_extension("pdf");
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| NodeError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }
        write_atomic(&path, |w| w.write_all(&doc.bytes).map_err(Into::into))?;
        self.last_written.set(Some(path.clone()));
        Ok(path)
    }
}

impl Node for PrintLayout {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(INPUTS)
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(OUTPUTS)
    }

    fn evaluate(&self, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let images = inputs.images(INPUT)?;
        let images: Vec<&RasterImage> = images.iter().map(|i| i.as_ref()).collect();
        let doc = self.build(&images)?;
        if self.auto_save && !self.path.as_os_str().is_empty() {
            self.save(&doc, ctx)?;
        }
        Ok(vec![Value::Document(Arc::new(doc))])
    }

    /// Re-save if the PDF was deleted behind our back.
    fn needs_refresh(&self, _: &EvalContext) -> bool {
        self.auto_save
            && self
                .last_written
                .get()
                .is_some_and(|p| !Path::new(&p).exists())
    }
}

impl NodeType for PrintLayout {
    const KIND: &'static str = "print_layout";
    const DISPLAY_NAME: &'static str = "Print Layout (PDF)";
    const CATEGORY: &'static str = "Print";
}

#[cfg(test)]
mod tests {
    use docforge_core::Graph;

    use super::*;
    use crate::{InputImage, input_image};

    /// An ID card scanned at 300 dpi: 85.6 × 54 mm → 1011 × 638 px.
    fn id_card() -> RasterImage {
        RasterImage::new(1011, 638, [40, 90, 160, 255]).with_dpi(Some(300.0))
    }

    fn page_count(pdf: &[u8]) -> usize {
        lopdf::Document::load_mem(pdf).unwrap().get_pages().len()
    }

    #[test]
    fn id_card_copies_plus_a_document_fit_one_page() {
        let layout = PrintLayout {
            items: vec![
                PrintItem {
                    size: ItemSize::default(),
                    copies: 2,
                },
                PrintItem {
                    size: ItemSize::Width { width_mm: 105.0 },
                    copies: 1,
                },
            ],
            ..PrintLayout::default()
        };
        let a6 = RasterImage::new(700, 990, [250, 250, 245, 255]);
        let doc = layout.build(&[&id_card(), &a6]).unwrap();
        assert_eq!(doc.pages_mm.len(), 1, "{:?}", doc.summary);
        assert_eq!(page_count(&doc.bytes), 1);
        assert!(
            doc.summary[0].starts_with("3 pieces on 1 A4 page"),
            "{:?}",
            doc.summary
        );
    }

    #[test]
    fn pdf_is_valid_and_shares_one_image_per_item() {
        let layout = PrintLayout {
            items: vec![PrintItem {
                size: ItemSize::Exact {
                    width_mm: 85.6,
                    height_mm: 54.0,
                },
                copies: 12,
            }],
            ..PrintLayout::default()
        };
        let doc = layout.build(&[&id_card()]).unwrap();
        let pdf = lopdf::Document::load_mem(&doc.bytes).unwrap();
        let images = pdf
            .objects
            .values()
            .filter(|o| {
                o.as_stream().is_ok_and(|s| {
                    s.dict
                        .get(b"Subtype")
                        .and_then(|v| v.as_name())
                        .is_ok_and(|n| n == b"Image")
                })
            })
            .count();
        assert_eq!(images, 1, "copies must reuse the same image object");
        // 12 cards need two A4 sheets at most.
        assert!(pdf.get_pages().len() <= 2);
    }

    #[test]
    fn missing_dpi_is_reported_and_large_images_downsampled() {
        let photo = RasterImage::new(4000, 3000, [128; 4]); // no DPI
        let layout = PrintLayout {
            image_dpi: 150.0,
            ..PrintLayout::default()
        };
        let doc = layout.build(&[&photo]).unwrap();
        assert!(
            doc.summary.iter().any(|s| s.contains("assumed 300")),
            "{:?}",
            doc.summary
        );
        // 4000 px at 300 dpi ≈ 339 mm: too wide, so it is shrunk to fit.
        assert!(
            doc.summary.iter().any(|s| s.contains("shrunk")),
            "{:?}",
            doc.summary
        );

        let pdf = lopdf::Document::load_mem(&doc.bytes).unwrap();
        let width = pdf
            .objects
            .values()
            .filter_map(|o| o.as_stream().ok())
            .find_map(|s| s.dict.get(b"Width").and_then(|w| w.as_i64()).ok())
            .unwrap();
        assert!(width < 2000, "embedded at {width} px, expected ≤ 150 dpi");
    }

    #[test]
    fn pieces_print_at_exact_physical_size() {
        let layout = PrintLayout {
            items: vec![PrintItem {
                size: ItemSize::Exact {
                    width_mm: 85.6,
                    height_mm: 54.0,
                },
                copies: 1,
            }],
            orientation: PageOrientation::Portrait,
            ..PrintLayout::default()
        };
        let doc = layout.build(&[&id_card()]).unwrap();
        let pdf = lopdf::Document::load_mem(&doc.bytes).unwrap();
        let page = *pdf.get_pages().values().next().unwrap();
        let content = lopdf::content::Content::decode(&pdf.get_page_content(page)).unwrap();
        let cm = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .expect("image placed");
        let m: Vec<f32> = cm.operands.iter().map(|o| o.as_float().unwrap()).collect();
        // The image's unit square is scaled to the piece size, in points.
        let (w_pt, h_pt) = if m[1] == 0.0 {
            (m[0], m[3])
        } else {
            (-m[2], m[1])
        };
        let mm = |pt: f32| pt * 25.4 / 72.0;
        let (w, h) = (mm(w_pt).max(mm(h_pt)), mm(w_pt).min(mm(h_pt)));
        assert!(
            (w - 85.6).abs() < 0.01 && (h - 54.0).abs() < 0.01,
            "{w} × {h} mm"
        );
        // Placed inside the 10 mm margin, measured from the top-left.
        let x = mm(m[4] + if m[1] == 0.0 { 0.0 } else { m[2] });
        assert!(x >= 10.0 - 0.01, "x = {x}");
    }

    #[test]
    fn lossless_and_crop_marks_also_produce_valid_pdfs() {
        for cut_marks in [CutMarks::None, CutMarks::Outline, CutMarks::CropMarks] {
            let layout = PrintLayout {
                jpeg_quality: None,
                cut_marks,
                ..PrintLayout::default()
            };
            let doc = layout.build(&[&id_card()]).unwrap();
            assert_eq!(page_count(&doc.bytes), 1);
        }
    }

    #[test]
    fn rejects_bad_settings() {
        let img = id_card();
        let bad = PrintLayout {
            margin_mm: 200.0,
            ..PrintLayout::default()
        };
        assert!(matches!(
            bad.build(&[&img]),
            Err(NodeError::InvalidParam {
                name: "margin_mm",
                ..
            })
        ));
        let none = PrintLayout {
            items: vec![PrintItem {
                copies: 0,
                ..PrintItem::default()
            }],
            ..PrintLayout::default()
        };
        assert!(none.build(&[&img]).is_err());
    }

    #[test]
    fn in_graph_saves_pdf_and_outputs_document() {
        let dir = tempfile::tempdir().unwrap();
        let card_path = dir.path().join("card.png");
        crate::OutputImage::new(&card_path)
            .save(&id_card(), &EvalContext::new())
            .unwrap();

        let mut g = Graph::new();
        let a = g.add(InputImage::new(&card_path));
        let b = g.add(InputImage::new(&card_path));
        let layout = PrintLayout {
            path: "print/sheet".into(),
            ..PrintLayout::default()
        };
        let print = g.add(layout);
        g.connect(a, input_image::OUTPUT, print, INPUT).unwrap();
        g.connect(b, input_image::OUTPUT, print, INPUT).unwrap();

        let ctx = EvalContext::new().with_base_dir(dir.path());
        let report = g.evaluate(&ctx);
        assert!(report.is_ok(), "{:?}", report.failed);
        let doc = g.output(print, OUTPUT).unwrap().as_document().unwrap();
        assert!(doc.summary[0].starts_with("2 pieces"));
        let saved = dir.path().join("print/sheet.pdf");
        assert_eq!(std::fs::read(&saved).unwrap(), doc.bytes);

        let json = g.to_json().unwrap();
        let back = Graph::from_json(&json, &crate::registry()).unwrap();
        assert_eq!(
            back.node_as::<PrintLayout>(print),
            g.node_as::<PrintLayout>(print)
        );
    }
}

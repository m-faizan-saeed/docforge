//! Tool #4 — Print Layout: packs images onto paper at their real size and
//! produces a printable PDF with cutting lines.

pub(crate) mod art;
pub mod pack;
mod pdf;

use std::{
    borrow::Cow,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PdfPageInfo, PortSpec,
    RasterImage, Value,
};
use serde::{Deserialize, Serialize};

pub use pack::{Orientation, Packing, Placement};
pub use pdf::CutMarks;

use art::PdfArt;
use pdf::Artwork;

use crate::{output_image::write_atomic, runtime::RuntimeState};

/// Multi-input port: every connected image becomes one item, in connection order.
pub const INPUT: &str = "images";
/// Multi-input port: every non-blank page of each connected PDF becomes a
/// piece, printed as vector art.
pub const PDF_INPUT: &str = "pdfs";
pub const OUTPUT: &str = "pdf";

// Both optional: a layout needs images, PDFs, or both (checked in `evaluate`).
const INPUTS: &[PortSpec] = &[
    PortSpec::image(INPUT).multi().optional(),
    PortSpec::document(PDF_INPUT).multi().optional(),
];
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
    /// Size of vector art whose own size is `natural` mm. `FromDpi` means
    /// "real size" — for a PDF that is simply its size.
    pub fn resolve_art(&self, natural: [f32; 2]) -> [f32; 2] {
        match *self {
            Self::FromDpi { .. } => natural,
            Self::Width { width_mm } => [
                width_mm,
                width_mm * natural[1] / natural[0].max(f32::EPSILON),
            ],
            Self::Exact {
                width_mm,
                height_mm,
            } => [width_mm, height_mm],
        }
    }

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
    /// Add as many further copies as fit without needing another sheet
    /// (at least `copies`, never fewer).
    pub fill: bool,
}

impl Default for PrintItem {
    fn default() -> Self {
        Self {
            size: ItemSize::default(),
            copies: 1,
            fill: false,
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
    /// `pdf_items[i]` configures the pages of the i-th connected PDF.
    pub pdf_items: Vec<PrintItem>,
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
            pdf_items: Vec::new(),
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
    /// Settings for the pages of the i-th connected PDF.
    pub fn pdf_item(&self, index: usize) -> PrintItem {
        self.pdf_items.get(index).cloned().unwrap_or_default()
    }

    pub fn pdf_item_mut(&mut self, index: usize) -> &mut PrintItem {
        if self.pdf_items.len() <= index {
            self.pdf_items.resize_with(index + 1, PrintItem::default);
        }
        &mut self.pdf_items[index]
    }

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
        let all = self
            .items
            .iter()
            .map(|i| ("item", i))
            .chain(self.pdf_items.iter().map(|i| ("PDF", i)));
        for (i, (what, item)) in all.enumerate() {
            let i = if what == "PDF" {
                i - self.items.len()
            } else {
                i
            };
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
                    reason: format!("{what} {} needs a positive size", i + 1),
                });
            }
        }
        Ok(())
    }

    /// Lays out `images` and renders the PDF. Usable outside a graph.
    pub fn build(&self, images: &[&RasterImage]) -> Result<PdfDocument, NodeError> {
        self.build_with_pdfs(images, &[])
    }

    /// Lays out `images` plus every non-blank page of `pdfs` (as vector art)
    /// and renders the PDF.
    pub fn build_with_pdfs(
        &self,
        images: &[&RasterImage],
        pdfs: &[&PdfDocument],
    ) -> Result<PdfDocument, NodeError> {
        self.validate()?;
        let loaded: Vec<lopdf::Document> = pdfs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                lopdf::Document::load_mem(&d.bytes)
                    .map_err(|e| NodeError::Other(format!("PDF {} cannot be read ({e})", i + 1)))
            })
            .collect::<Result<_, _>>()?;

        let mut notes = Vec::new();
        let mut pieces: Vec<Piece<'_>> = Vec::new();
        for (i, img) in images.iter().enumerate() {
            let item = self.item(i);
            let (size, note) = item.size.resolve(img);
            if let Some(note) = note {
                notes.push(format!("Image {} {note}.", i + 1));
            }
            pieces.push(Piece {
                art: Artwork::Image(img),
                size,
                item,
                label: format!("Image {}", i + 1),
            });
        }
        for (d, (doc, source)) in loaded.iter().zip(pdfs).enumerate() {
            let item = self.pdf_item(d);
            let pages = doc.get_pages();
            let many = pages.len() > 1;
            for (p, &page) in pages.values().enumerate() {
                let Some(art) = pdf_art(doc, page, source.page_info.get(p)) else {
                    continue; // blank page
                };
                let natural = art.display_size_pt().map(|pt| pt / PT_PER_MM);
                pieces.push(Piece {
                    size: item.size.resolve_art(natural),
                    art: Artwork::Pdf(art),
                    item: item.clone(),
                    label: if many {
                        format!("PDF {} page {}", d + 1, p + 1)
                    } else {
                        format!("PDF {}", d + 1)
                    },
                });
            }
        }
        if pieces.is_empty() {
            return Err(NodeError::InvalidParam {
                name: "pdfs",
                reason: "the connected PDFs have no visible content".into(),
            });
        }

        let sizes: Vec<[f32; 2]> = pieces.iter().map(|p| p.size).collect();
        let mut copies: Vec<u32> = pieces
            .iter()
            .map(|p| {
                if p.item.fill {
                    p.item.copies.max(1)
                } else {
                    p.item.copies
                }
            })
            .collect();
        if copies.iter().all(|&c| c == 0) {
            return Err(NodeError::InvalidParam {
                name: "items",
                reason: "every item has 0 copies".into(),
            });
        }
        let pack_with = |copies: &[u32]| self.pack(&sizes, copies);
        let base = pack_with(&copies).ok_or_else(no_printable_area)?;
        for i in (0..pieces.len()).filter(|&i| pieces[i].item.fill) {
            copies[i] = max_copies_without_new_sheet(i, &copies, base.pages, &pack_with);
        }
        let packing = pack_with(&copies).ok_or_else(no_printable_area)?;

        let scaled: Vec<[f32; 2]> = sizes
            .iter()
            .zip(&packing.scale)
            .map(|(s, k)| s.map(|v| v * k))
            .collect();
        for (i, &k) in packing.scale.iter().enumerate() {
            if k < 0.999 && copies[i] > 0 {
                notes.push(format!(
                    "{} was shrunk to {:.0}% to fit on the paper.",
                    pieces[i].label,
                    k * 100.0
                ));
            }
        }

        let art: Vec<Artwork<'_>> = pieces.into_iter().map(|p| p.art).collect();
        let bytes = pdf::write(
            &packing,
            &art,
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
            page_info: Vec::new(),
        })
    }

    fn pack(&self, sizes: &[[f32; 2]], copies: &[u32]) -> Option<Packing> {
        pack::pack(&pack::Request {
            sizes,
            copies,
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

/// One printable piece before packing.
struct Piece<'a> {
    art: Artwork<'a>,
    size: [f32; 2],
    item: PrintItem,
    label: String,
}

const PT_PER_MM: f32 = 72.0 / 25.4;

fn no_printable_area() -> NodeError {
    NodeError::InvalidParam {
        name: "margin_mm",
        reason: "the margins leave no printable area".into(),
    }
}

/// The artwork box of a source page: the analysed content box when the PDF
/// came from PDF Input, otherwise the page's visible area (e.g. the output
/// of another Print Layout). `None` for blank pages.
fn pdf_art<'a>(
    doc: &'a lopdf::Document,
    page: lopdf::ObjectId,
    info: Option<&PdfPageInfo>,
) -> Option<PdfArt<'a>> {
    use crate::pdf_input::analyze::{inherited, num, rect};
    let (bbox, rotation) = match info {
        Some(info) => (info.content_box?, info.rotation),
        None => {
            let media = inherited(doc, page, b"MediaBox").and_then(|o| rect(doc, o))?;
            let crop = inherited(doc, page, b"CropBox")
                .and_then(|o| rect(doc, o))
                .and_then(|c| c.intersect(&media))
                .unwrap_or(media);
            let rotation = inherited(doc, page, b"Rotate")
                .and_then(num)
                .map_or(0, |r| (r as i64).rem_euclid(360) as u16 / 90 * 90);
            (crop.to_array(), rotation)
        }
    };
    let [x0, y0, x1, y1] = bbox;
    (x1 - x0 >= 1.0 && y1 - y0 >= 1.0).then_some(PdfArt {
        doc,
        page,
        bbox,
        rotation,
    })
}

/// Largest copy count for piece `i` that keeps the layout on `pages` sheets.
/// Grows exponentially, then bisects: ~20 packings even for hundreds of copies.
fn max_copies_without_new_sheet(
    i: usize,
    copies: &[u32],
    pages: usize,
    pack: &impl Fn(&[u32]) -> Option<Packing>,
) -> u32 {
    const LIMIT: u32 = 2000;
    let fits = |n: u32| {
        let mut c = copies.to_vec();
        c[i] = n;
        pack(&c).is_some_and(|p| p.pages <= pages)
    };
    let mut good = copies[i];
    let mut step = good.max(1);
    while good < LIMIT && fits(good + step) {
        good += step;
        step *= 2;
    }
    // The answer lies in (good, good + step).
    let (mut lo, mut hi) = (good, (good + step).min(LIMIT + 1));
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) { lo = mid } else { hi = mid }
    }
    lo
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
        let pdfs = inputs
            .all(PDF_INPUT)
            .iter()
            .map(|v| {
                v.as_document()
                    .map(|d| d.as_ref())
                    .ok_or_else(|| NodeError::InputType {
                        port: PDF_INPUT.into(),
                        expected: docforge_core::PortType::Document,
                        actual: v.port_type(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if images.is_empty() && pdfs.is_empty() {
            return Err(NodeError::MissingInput(format!("{INPUT}` or `{PDF_INPUT}")));
        }
        let doc = self.build_with_pdfs(&images, &pdfs)?;
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
                    fill: false,
                },
                PrintItem {
                    size: ItemSize::Width { width_mm: 105.0 },
                    copies: 1,
                    fill: false,
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
                fill: false,
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
                fill: false,
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

    // ----- vector artwork from PDFs ------------------------------------------

    use lopdf::{Dictionary, Document, Object, Stream, dictionary};

    /// A 200 × 200 pt one-page PDF with `content`, optionally rotated.
    fn source_pdf(content: &str, rotate: i64) -> Vec<u8> {
        let mut doc = Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let content_id =
            doc.add_object(Stream::new(Dictionary::new(), content.as_bytes().to_vec()));
        let page = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id, "Rotate" => rotate,
            "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
            "Resources" => Dictionary::new(),
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages", "Count" => 1, "Kids" => vec![page.into()],
            }),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    /// Loads `bytes` through PDF Input (analysed and cropped to content).
    fn art(bytes: &[u8]) -> PdfDocument {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("art.pdf");
        std::fs::write(&path, bytes).unwrap();
        crate::PdfInput::new(&path)
            .load(&EvalContext::new())
            .unwrap()
    }

    /// Every `cm … Do` on the first page: (xobject name, art box placed, in mm).
    fn placed(doc: &PdfDocument) -> Vec<(String, [f32; 4])> {
        let pdf = Document::load_mem(&doc.bytes).unwrap();
        let page = *pdf.get_pages().values().next().unwrap();
        let ops = lopdf::content::Content::decode(&pdf.get_page_content(page))
            .unwrap()
            .operations;
        let (resources, _) = pdf.get_page_resources(page).unwrap();
        let xobjects = resources
            .unwrap()
            .get(b"XObject")
            .unwrap()
            .as_dict()
            .unwrap();
        let mut out = Vec::new();
        for pair in ops.windows(2) {
            if pair[0].operator != "cm" || pair[1].operator != "Do" {
                continue;
            }
            let m: Vec<f64> = pair[0]
                .operands
                .iter()
                .map(|o| o.as_float().unwrap() as f64)
                .collect();
            let m = crate::pdf_input::geometry::Matrix::new([m[0], m[1], m[2], m[3], m[4], m[5]]);
            let name = pair[1].operands[0].as_name().unwrap();
            let id = xobjects.get(name).unwrap().as_reference().unwrap();
            let dict = &pdf.get_object(id).unwrap().as_stream().unwrap().dict;
            // Images draw the unit square; forms draw their BBox.
            let src = match dict.get(b"BBox") {
                Ok(b) => {
                    let v: Vec<f64> = b
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|o| o.as_float().unwrap() as f64)
                        .collect();
                    crate::pdf_input::geometry::Bbox::from_rect([v[0], v[1], v[2], v[3]])
                }
                Err(_) => crate::pdf_input::geometry::Bbox::from_rect([0.0, 0.0, 1.0, 1.0]),
            };
            let r = crate::pdf_input::geometry::Bbox::transformed(&src, &m);
            let mm = |v: f64| (v * 25.4 / 72.0) as f32;
            out.push((
                String::from_utf8_lossy(name).into_owned(),
                [mm(r.x0), mm(r.y0), mm(r.x1), mm(r.y1)],
            ));
        }
        out
    }

    fn size([x0, y0, x1, y1]: [f32; 4]) -> [f32; 2] {
        [x1 - x0, y1 - y0]
    }

    #[test]
    fn pdf_art_prints_at_its_real_size_from_one_shared_form() {
        // 72 × 36 pt of art (25.4 × 12.7 mm) somewhere on a larger page.
        let src = art(&source_pdf("0 g 30 40 72 36 re f", 0));
        let layout = PrintLayout {
            pdf_items: vec![PrintItem {
                copies: 3,
                ..PrintItem::default()
            }],
            allow_rotation: false,
            ..PrintLayout::default()
        };
        let doc = layout.build_with_pdfs(&[], &[&src]).unwrap();
        let pieces = placed(&doc);
        assert_eq!(pieces.len(), 3);
        assert!(
            pieces.iter().all(|(name, _)| name == "Fm0"),
            "all copies use one form"
        );
        for (_, r) in &pieces {
            let [w, h] = size(*r);
            assert!(
                (w - 25.4).abs() < 0.01 && (h - 12.7).abs() < 0.01,
                "{w} × {h} mm"
            );
            // Inside the 10 mm margins.
            assert!(r[0] >= 10.0 - 0.01 && r[1] >= 10.0 - 0.01, "{r:?}");
        }
        let pdf = Document::load_mem(&doc.bytes).unwrap();
        let forms = pdf.objects.values().filter(|o| {
            o.as_stream().is_ok_and(|s| {
                s.dict
                    .get(b"Subtype")
                    .and_then(|v| v.as_name())
                    .is_ok_and(|n| n == b"Form")
            })
        });
        assert_eq!(forms.count(), 1);
    }

    #[test]
    fn pdf_art_can_be_resized_and_respects_page_rotation() {
        let src = art(&source_pdf("0 g 30 40 72 36 re f", 0));
        let layout = PrintLayout {
            pdf_items: vec![PrintItem {
                size: ItemSize::Width { width_mm: 50.8 },
                ..PrintItem::default()
            }],
            allow_rotation: false,
            ..PrintLayout::default()
        };
        let [w, h] = size(placed(&layout.build_with_pdfs(&[], &[&src]).unwrap())[0].1);
        assert!(
            (w - 50.8).abs() < 0.01 && (h - 25.4).abs() < 0.01,
            "{w} × {h}"
        );

        // The source page is displayed rotated 90°: its art is 36 × 72 pt as seen.
        let rotated = art(&source_pdf("0 g 30 40 72 36 re f", 90));
        let layout = PrintLayout {
            allow_rotation: false,
            ..PrintLayout::default()
        };
        let [w, h] = size(placed(&layout.build_with_pdfs(&[], &[&rotated]).unwrap())[0].1);
        assert!(
            (w - 12.7).abs() < 0.01 && (h - 25.4).abs() < 0.01,
            "{w} × {h}"
        );
    }

    #[test]
    fn fill_sheet_adds_copies_until_another_sheet_would_be_needed() {
        // 9 ID cards is the A4 maximum with 10 mm margins and 4 mm gaps.
        let layout = PrintLayout {
            items: vec![PrintItem {
                size: ItemSize::Exact {
                    width_mm: 85.6,
                    height_mm: 54.0,
                },
                fill: true,
                ..PrintItem::default()
            }],
            ..PrintLayout::default()
        };
        let doc = layout.build(&[&id_card()]).unwrap();
        assert_eq!(doc.pages_mm.len(), 1);
        assert!(
            doc.summary[0].starts_with("9 pieces on 1 A4 page"),
            "{:?}",
            doc.summary
        );

        // With other pieces present, filling uses only the space left over.
        let src = art(&source_pdf("0 g 0 0 150 150 re f", 0)); // 52.9 mm square
        let layout = PrintLayout {
            items: vec![PrintItem {
                copies: 2,
                ..PrintItem::default()
            }],
            pdf_items: vec![PrintItem {
                fill: true,
                ..PrintItem::default()
            }],
            ..PrintLayout::default()
        };
        let doc = layout.build_with_pdfs(&[&id_card()], &[&src]).unwrap();
        assert_eq!(doc.pages_mm.len(), 1);
        let squares = placed(&doc)
            .iter()
            .filter(|(n, _)| n.starts_with("Fm"))
            .count();
        assert!(
            squares >= 4,
            "{squares} squares beside 2 cards: {:?}",
            doc.summary
        );
    }

    #[test]
    fn images_and_pdfs_share_sheets_and_layouts_can_be_nested() {
        let src = art(&source_pdf("0 g 30 40 72 36 re f", 0));
        let first = PrintLayout {
            pdf_items: vec![PrintItem {
                copies: 4,
                ..PrintItem::default()
            }],
            ..PrintLayout::default()
        }
        .build_with_pdfs(&[&id_card()], &[&src])
        .unwrap();
        let names: Vec<String> = placed(&first).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.iter().filter(|n| n.starts_with("Im")).count(), 1);
        assert_eq!(names.iter().filter(|n| n.starts_with("Fm")).count(), 4);

        // A generated sheet (no page analysis) can itself be a piece: the
        // whole A4 page, shrunk to fit inside the margins of another A4.
        let nested = PrintLayout::default()
            .build_with_pdfs(&[], &[&first])
            .unwrap();
        let (_, r) = &placed(&nested)[0];
        let [w, h] = size(*r);
        assert!(
            (w / h - 210.0 / 297.0).abs() < 0.001,
            "keeps A4 proportions: {w} × {h}"
        );
        assert!(
            nested
                .summary
                .iter()
                .any(|s| s.contains("PDF 1 was shrunk")),
            "{:?}",
            nested.summary
        );
    }

    #[test]
    fn blank_pdfs_and_missing_inputs_are_reported() {
        let blank = art(&source_pdf("1 g 0 0 200 200 re f", 0));
        let err = PrintLayout::default()
            .build_with_pdfs(&[], &[&blank])
            .unwrap_err();
        assert!(err.to_string().contains("no visible content"), "{err}");

        let mut g = Graph::new();
        let layout = g.add(PrintLayout::default());
        g.evaluate(&EvalContext::new());
        let Some(docforge_core::NodeStatus::Failed(err)) = g.status(layout) else {
            panic!("expected failure");
        };
        assert_eq!(
            err.to_string(),
            "required input `images` or `pdfs` is not connected"
        );
    }

    #[test]
    fn pdf_input_feeds_print_layout_in_a_graph() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("art.pdf");
        std::fs::write(&path, source_pdf("0 g 30 40 72 36 re f", 0)).unwrap();
        let mut g = Graph::new();
        let input = g.add(crate::PdfInput::new(&path));
        let print = g.add(PrintLayout {
            pdf_items: vec![PrintItem {
                copies: 2,
                ..PrintItem::default()
            }],
            ..PrintLayout::default()
        });
        g.connect(input, crate::pdf_input::OUTPUT, print, PDF_INPUT)
            .unwrap();
        let report = g.evaluate(&EvalContext::new());
        assert!(report.is_ok(), "{:?}", report.failed);
        let doc = g.output(print, OUTPUT).unwrap().as_document().unwrap();
        assert!(doc.summary[0].starts_with("2 pieces"), "{:?}", doc.summary);

        // Settings survive saving, and old projects without `pdf_items`/`fill` still load.
        let back = Graph::from_json(&g.to_json().unwrap(), &crate::registry()).unwrap();
        assert_eq!(
            back.node_as::<PrintLayout>(print),
            g.node_as::<PrintLayout>(print)
        );
        let old: PrintLayout = serde_json::from_str(
            r#"{"items":[{"size":{"mode":"from_dpi","fallback_dpi":300.0},"copies":2}]}"#,
        )
        .unwrap();
        assert!(old.pdf_items.is_empty() && !old.items[0].fill);
    }
}

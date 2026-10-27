//! Tool #7 — PDF Overlay: stacks several PDFs onto one page, e.g. a gray
//! PCB bottom layer under a black top layer for an assembly guide.

use std::{borrow::Cow, sync::Arc};

use docforge_core::{
    EvalContext, Inputs, Node, NodeError, NodeType, PdfDocument, PdfPageInfo, PortSpec, Value,
};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};
use serde::{Deserialize, Serialize};

use crate::{
    pdf::{self, PT_PER_MM, PageBox, geometry::Matrix},
    print::art::{Imported, PdfArt, import_page_as_form},
};

/// Multi-input: the PDFs to stack, first connected = bottom layer.
pub const INPUT: &str = "pdfs";
pub const OUTPUT: &str = "pdf";

const INPUTS: &[PortSpec] = &[PortSpec::document(INPUT).multi()];
const OUTPUTS: &[PortSpec] = &[PortSpec::document(OUTPUT)];

/// How layers are positioned relative to each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    /// Where they are in their pages' own coordinates. Cropping with PDF
    /// Edit keeps those coordinates, so layers exported from one design stay
    /// in register even when each was cropped differently.
    #[default]
    Original,
    /// Centres on the bottom layer's centre.
    Center,
    /// Top-left corners on the bottom layer's.
    TopLeft,
}

/// The size of the resulting page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageSize {
    /// Large enough for every layer.
    #[default]
    Union,
    /// The bottom layer's page; other layers are clipped to it.
    Bottom,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Blend {
    #[default]
    Normal,
    /// Darkens what is below; white becomes see-through.
    Multiply,
    /// Keeps the darker of layer and background.
    Darken,
}

impl Blend {
    fn pdf_name(self) -> &'static str {
        match self {
            Blend::Normal => "Normal",
            Blend::Multiply => "Multiply",
            Blend::Darken => "Darken",
        }
    }
}

/// Settings of one layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Layer {
    pub visible: bool,
    /// 0–1.
    pub opacity: f32,
    pub blend: Blend,
    /// Extra shift in mm: x to the right, y downwards.
    pub offset_mm: [f32; 2],
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            visible: true,
            opacity: 1.0,
            blend: Blend::Normal,
            offset_mm: [0.0, 0.0],
        }
    }
}

/// Where one layer lands on an output page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Index of the layer (connection order).
    pub layer: usize,
    /// Its visible area on the output page, points, origin bottom-left.
    pub rect: [f32; 4],
    /// Maps the layer page's coordinates onto the output page.
    pub matrix: Matrix,
}

/// One output page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLayout {
    /// Width and height in points.
    pub size: [f32; 2],
    /// Bottom to top.
    pub layers: Vec<Placement>,
}

/// Stacks the connected PDFs page by page: output page *i* overlays page *i*
/// of every layer (layers with fewer pages simply end). Each layer stays
/// vector: it is embedded once as a Form XObject.
///
/// `layers[i]` configures the i-th connected PDF; missing entries use
/// [`Layer::default`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PdfOverlay {
    pub align: Align,
    pub page_size: PageSize,
    pub layers: Vec<Layer>,
}

/// Turns a page's coordinates the way the page is displayed (`/Rotate` is
/// clockwise), about the origin.
fn rotation_matrix(rotation: u16) -> Matrix {
    match rotation {
        90 => Matrix::new([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        180 => Matrix::new([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        270 => Matrix::new([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        _ => Matrix::IDENTITY,
    }
}

fn transform_box(m: &Matrix, [x0, y0, x1, y1]: [f32; 4]) -> [f64; 4] {
    let b = pdf::geometry::Bbox::transformed(
        &pdf::geometry::Bbox::from_rect([x0, y0, x1, y1].map(f64::from)),
        m,
    );
    [b.x0, b.y0, b.x1, b.y1]
}

impl PdfOverlay {
    pub fn layer(&self, index: usize) -> Layer {
        self.layers.get(index).copied().unwrap_or_default()
    }

    pub fn layer_mut(&mut self, index: usize) -> &mut Layer {
        if self.layers.len() <= index {
            self.layers.resize_with(index + 1, Layer::default);
        }
        &mut self.layers[index]
    }

    /// Works out every output page from the layers' page boxes (outer: layer,
    /// inner: page). Pure, so editors can draw where each layer lands.
    pub fn layout(&self, layers: &[Vec<PageBox>]) -> Vec<PageLayout> {
        let pages = layers.iter().map(Vec::len).max().unwrap_or(0);
        (0..pages)
            .map(|p| {
                // Each layer's visible area, turned as displayed.
                let shown: Vec<(usize, Matrix, [f64; 4])> = layers
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| self.layer(*i).visible)
                    .filter_map(|(i, boxes)| {
                        let page = boxes.get(p)?;
                        let rot = rotation_matrix(page.rotation);
                        Some((i, rot, transform_box(&rot, page.crop)))
                    })
                    .collect();
                let Some(&(_, _, base)) = shown.first() else {
                    return PageLayout {
                        size: [0.0, 0.0],
                        layers: Vec::new(),
                    };
                };
                let placed: Vec<(usize, Matrix, [f64; 4])> = shown
                    .iter()
                    .map(|&(i, rot, b)| {
                        let (ax, ay) = match self.align {
                            Align::Original => (0.0, 0.0),
                            Align::Center => (
                                (base[0] + base[2] - b[0] - b[2]) / 2.0,
                                (base[1] + base[3] - b[1] - b[3]) / 2.0,
                            ),
                            Align::TopLeft => (base[0] - b[0], base[3] - b[3]),
                        };
                        let [ox, oy] = self.layer(i).offset_mm.map(|v| (v * PT_PER_MM) as f64);
                        let (dx, dy) = (ax + ox, ay - oy); // offset y is downwards
                        (
                            i,
                            rot.then(&Matrix::translate(dx, dy)),
                            [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy],
                        )
                    })
                    .collect();
                let page = match self.page_size {
                    PageSize::Bottom => placed[0].2,
                    PageSize::Union => placed.iter().fold(placed[0].2, |u, (_, _, b)| {
                        [
                            u[0].min(b[0]),
                            u[1].min(b[1]),
                            u[2].max(b[2]),
                            u[3].max(b[3]),
                        ]
                    }),
                };
                let origin = Matrix::translate(-page[0], -page[1]);
                PageLayout {
                    size: [(page[2] - page[0]) as f32, (page[3] - page[1]) as f32],
                    layers: placed
                        .into_iter()
                        .map(|(layer, m, b)| Placement {
                            layer,
                            rect: [
                                b[0] - page[0],
                                b[1] - page[1],
                                b[2] - page[0],
                                b[3] - page[1],
                            ]
                            .map(|v| v as f32),
                            matrix: m.then(&origin),
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// Stacks `inputs` (bottom first). Usable on its own, outside any graph.
    pub fn apply(&self, inputs: &[&PdfDocument]) -> Result<PdfDocument, NodeError> {
        for (i, l) in self.layers.iter().enumerate() {
            if !(0.0..=1.0).contains(&l.opacity) || !l.offset_mm.iter().all(|v| v.is_finite()) {
                return Err(NodeError::InvalidParam {
                    name: "layers",
                    reason: format!(
                        "layer {} needs an opacity of 0–1 and a finite offset",
                        i + 1
                    ),
                });
            }
        }
        let sources: Vec<Document> = inputs
            .iter()
            .enumerate()
            .map(|(i, d)| {
                Document::load_mem(&d.bytes)
                    .map_err(|e| NodeError::Other(format!("layer {} cannot be read ({e})", i + 1)))
            })
            .collect::<Result<_, _>>()?;
        let boxes: Vec<Vec<PageBox>> = inputs.iter().map(|d| pdf::page_boxes(d)).collect();
        let layout = self.layout(&boxes);
        if layout.iter().all(|p| p.layers.is_empty()) {
            return Err(NodeError::InvalidParam {
                name: "layers",
                reason: "every layer is hidden or empty".into(),
            });
        }
        let page_ids: Vec<Vec<lopdf::ObjectId>> = sources
            .iter()
            .map(|d| d.get_pages().values().copied().collect())
            .collect();

        let mut out = Document::with_version("1.7");
        let pages_id = out.new_object_id();
        let mut imported = Imported::default();
        let mut kids = Vec::new();
        let mut page_info = Vec::new();
        for (p, page) in layout.iter().enumerate() {
            if page.layers.is_empty() {
                continue;
            }
            let mut content = String::new();
            let mut xobjects = Dictionary::new();
            let mut states = Dictionary::new();
            for placement in &page.layers {
                let i = placement.layer;
                let settings = self.layer(i);
                let art = PdfArt {
                    doc: &sources[i],
                    page: page_ids[i][p],
                    bbox: boxes[i][p].crop,
                    rotation: boxes[i][p].rotation,
                };
                let form = import_page_as_form(&mut out, &art, &mut imported);
                xobjects.set(format!("L{i}"), form);
                let m = placement.matrix;
                let mut gs_op = String::new();
                if settings.opacity < 1.0 || settings.blend != Blend::Normal {
                    states.set(
                        format!("G{i}"),
                        dictionary! {
                            "Type" => "ExtGState",
                            "CA" => Object::Real(settings.opacity),
                            "ca" => Object::Real(settings.opacity),
                            "BM" => settings.blend.pdf_name(),
                        },
                    );
                    gs_op = format!("/G{i} gs ");
                }
                content += &format!(
                    "q {gs_op}{:.6} {:.6} {:.6} {:.6} {:.4} {:.4} cm /L{i} Do Q\n",
                    m.a, m.b, m.c, m.d, m.e, m.f
                );
            }
            let [w, h] = page.size;
            let content_id = out.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
            let mut resources = dictionary! { "XObject" => xobjects };
            if !states.is_empty() {
                resources.set("ExtGState", states);
            }
            let page_id = out.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)],
                "Contents" => content_id,
                "Resources" => resources,
                // Blending happens in a transparency group of the page.
                "Group" => dictionary! { "Type" => "Group", "S" => "Transparency", "CS" => "DeviceRGB" },
            });
            kids.push(Object::Reference(page_id));
            let full = [0.0, 0.0, w, h];
            page_info.push(PdfPageInfo {
                media_box: full,
                crop_box: full,
                rotation: 0,
                content_box: Some(full),
                images: Vec::new(),
            });
        }
        out.objects.insert(
            pages_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Count" => kids.len() as i64, "Kids" => kids },
            ),
        );
        let catalog = out.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        let info = out.add_object(dictionary! {
            "Producer" => Object::string_literal("docforge"),
            "Title" => Object::string_literal("docforge overlay"),
        });
        out.trailer.set("Root", catalog);
        out.trailer.set("Info", info);
        out.compress();
        let bytes = pdf::save(&mut out).map_err(NodeError::Other)?;

        let pages_mm = pdf::page_sizes_mm(&page_info);
        let visible = (0..inputs.len()).filter(|&i| self.layer(i).visible).count();
        let [w, h] = pages_mm.first().copied().unwrap_or_default();
        let summary = vec![format!(
            "{visible} layer{} on {} page{} · {w:.1} × {h:.1} mm",
            if visible == 1 { "" } else { "s" },
            pages_mm.len(),
            if pages_mm.len() == 1 { "" } else { "s" },
        )];
        Ok(PdfDocument {
            bytes,
            pages_mm,
            summary,
            page_info,
        })
    }
}

impl Node for PdfOverlay {
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
        let docs: Vec<&PdfDocument> = inputs
            .all(INPUT)
            .iter()
            .filter_map(|v| v.as_document().map(|d| d.as_ref()))
            .collect();
        Ok(vec![Value::Document(Arc::new(self.apply(&docs)?))])
    }
}

impl NodeType for PdfOverlay {
    const KIND: &'static str = "pdf_overlay";
    const DISPLAY_NAME: &'static str = "PDF Overlay";
    const CATEGORY: &'static str = "Processing";
}

#[cfg(test)]
mod tests;

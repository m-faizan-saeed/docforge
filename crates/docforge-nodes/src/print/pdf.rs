//! Low-level PDF assembly with `lopdf`.

use std::fmt::Write as _;

use docforge_core::{NodeError, RasterImage};
use image::{ExtendedColorType, ImageEncoder, codecs::jpeg::JpegEncoder, imageops::FilterType};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, dictionary};

use super::{
    art::{Imported, PdfArt, import_page_as_form},
    pack::{Packing, Rect},
};

/// Points per millimetre.
const PT: f32 = 72.0 / 25.4;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutMarks {
    None,
    /// A thin dashed line around every piece.
    #[default]
    Outline,
    /// Short marks at each corner, outside the piece.
    CropMarks,
}

pub struct Options {
    /// Embedded images are downsampled to at most this resolution.
    pub dpi: f32,
    /// JPEG quality for embedded images, or `None` for lossless (Flate).
    pub jpeg_quality: Option<u8>,
    pub cut_marks: CutMarks,
    pub gap: f32,
    pub title: String,
}

/// What a piece shows.
pub enum Artwork<'a> {
    Image(&'a RasterImage),
    /// A page of a PDF, kept as vector graphics.
    Pdf(PdfArt<'a>),
}

/// Builds the PDF. `sizes` are the items' final (scaled) sizes in mm.
pub fn write(
    packing: &Packing,
    pieces: &[Artwork<'_>],
    sizes: &[[f32; 2]],
    opts: &Options,
) -> Result<Vec<u8>, NodeError> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();

    // One image/form object per item that is actually placed, shared by all copies.
    let mut xobjects: Vec<Option<ObjectId>> = vec![None; pieces.len()];
    let mut imported = Imported::default();
    for placement in &packing.placements {
        let slot = &mut xobjects[placement.item];
        if slot.is_none() {
            *slot = Some(match &pieces[placement.item] {
                Artwork::Image(img) => {
                    let stream = image_stream(img, sizes[placement.item], opts)?;
                    doc.add_object(stream)
                }
                Artwork::Pdf(art) => import_page_as_form(&mut doc, art, &mut imported),
            });
        }
    }

    let [page_w, page_h] = packing.page;
    let mut kids = Vec::with_capacity(packing.pages);
    for page in 0..packing.pages {
        let mut content = String::new();
        let mut resources = Dictionary::new();
        let on_page: Vec<_> = packing
            .placements
            .iter()
            .filter(|p| p.page == page)
            .collect();
        for p in &on_page {
            let r = to_pdf(&p.rect, page_h);
            let (name, m) = match &pieces[p.item] {
                // Map the image's unit square onto the piece; rotated pieces
                // are turned 90° counter-clockwise.
                Artwork::Image(_) => (
                    format!("Im{}", p.item),
                    if p.rotated {
                        [0.0, r.h, -r.w, 0.0, r.x + r.w, r.y]
                    } else {
                        [r.w, 0.0, 0.0, r.h, r.x, r.y]
                    },
                ),
                Artwork::Pdf(art) => {
                    let name = format!("Fm{}", p.item);
                    resources.set(name.clone(), xobjects[p.item].expect("created above"));
                    // Six decimals: the matrix carries scale factors, where
                    // rounding would visibly shift large art.
                    let t = art.placement(&r, p.rotated);
                    let _ = writeln!(
                        content,
                        "q {:.6} {:.6} {:.6} {:.6} {:.4} {:.4} cm /{name} Do Q",
                        t.a, t.b, t.c, t.d, t.e, t.f
                    );
                    continue;
                }
            };
            resources.set(name.clone(), xobjects[p.item].expect("created above"));
            let _ = writeln!(
                content,
                "q {} {} {} {} {} {} cm /{name} Do Q",
                n(m[0]),
                n(m[1]),
                n(m[2]),
                n(m[3]),
                n(m[4]),
                n(m[5])
            );
        }
        cut_marks(&mut content, on_page.iter().map(|p| p.rect), page_h, opts);

        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "MediaBox" => vec![0.into(), 0.into(), Object::Real(page_w * PT), Object::Real(page_h * PT)],
            "Contents" => content_id,
            "Resources" => dictionary! { "XObject" => resources },
        });
        kids.push(Object::Reference(page_id));
    }

    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Count" => kids.len() as i64,
            "Kids" => kids,
        }),
    );
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    let info_id = doc.add_object(dictionary! {
        "Producer" => Object::string_literal("docforge"),
        "Title" => Object::string_literal(opts.title.as_str()),
    });
    doc.trailer.set("Root", catalog_id);
    doc.trailer.set("Info", info_id);
    doc.compress();

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .map_err(|e| NodeError::Other(format!("writing PDF failed: {e}")))?;
    Ok(bytes)
}

/// Page rectangle in PDF points, origin bottom-left.
fn to_pdf(r: &Rect, page_h: f32) -> Rect {
    Rect {
        x: r.x * PT,
        y: (page_h - r.y - r.h) * PT,
        w: r.w * PT,
        h: r.h * PT,
    }
}

/// Compact number formatting for content streams.
fn n(v: f32) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn cut_marks(content: &mut String, rects: impl Iterator<Item = Rect>, page_h: f32, opts: &Options) {
    match opts.cut_marks {
        CutMarks::None => {}
        CutMarks::Outline => {
            content.push_str("q 0.45 G 0.4 w [3 2] 0 d\n");
            for r in rects {
                let r = to_pdf(&r, page_h);
                let _ = writeln!(content, "{} {} {} {} re S", n(r.x), n(r.y), n(r.w), n(r.h));
            }
            content.push_str("Q\n");
        }
        CutMarks::CropMarks => {
            // Marks stay inside half the gap so neighbours' marks never cross pieces.
            let offset = 0.6 * PT;
            let len = ((opts.gap / 2.0 - 0.6).clamp(1.5, 5.0)) * PT;
            content.push_str("q 0 G 0.3 w\n");
            for r in rects {
                let r = to_pdf(&r, page_h);
                for (cx, cy, dx, dy) in [
                    (r.x, r.y, -1.0, -1.0),
                    (r.x + r.w, r.y, 1.0, -1.0),
                    (r.x + r.w, r.y + r.h, 1.0, 1.0),
                    (r.x, r.y + r.h, -1.0, 1.0),
                ] {
                    // Horizontal mark along the edge's line, then vertical.
                    let _ = writeln!(
                        content,
                        "{} {} m {} {} l S {} {} m {} {} l S",
                        n(cx + dx * offset),
                        n(cy),
                        n(cx + dx * (offset + len)),
                        n(cy),
                        n(cx),
                        n(cy + dy * offset),
                        n(cx),
                        n(cy + dy * (offset + len)),
                    );
                }
            }
            content.push_str("Q\n");
        }
    }
}

fn image_stream(img: &RasterImage, size_mm: [f32; 2], opts: &Options) -> Result<Stream, NodeError> {
    // Never upsample: the PDF viewer scales; only shed resolution beyond `dpi`.
    let target = size_mm.map(|mm| ((mm / 25.4 * opts.dpi).round() as u32).max(1));
    let (w, h) = (img.width().min(target[0]), img.height().min(target[1]));
    let rgba = if (w, h) == img.dimensions() {
        img.to_rgba_image()
    } else {
        image::imageops::resize(&img.to_rgba_image(), w, h, FilterType::Triangle)
    };
    // Flatten transparency onto white paper.
    let rgb: Vec<u8> = rgba
        .pixels()
        .flat_map(|p| {
            let [r, g, b, a] = p.0;
            let a = a as u32;
            [r, g, b].map(|c| ((c as u32 * a + 255 * (255 - a) + 127) / 255) as u8)
        })
        .collect();

    let mut dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => w as i64,
        "Height" => h as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
    };
    Ok(match opts.jpeg_quality {
        Some(quality) => {
            let mut jpeg = Vec::new();
            JpegEncoder::new_with_quality(&mut jpeg, quality.clamp(1, 100))
                .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
                .map_err(|e| NodeError::Other(format!("JPEG encoding failed: {e}")))?;
            dict.set("Filter", "DCTDecode");
            // Already compressed; Flate on top would only cost time.
            Stream::new(dict, jpeg).with_compression(false)
        }
        None => Stream::new(dict, rgb),
    })
}

//! Builds small PDFs by hand for tests.

use std::path::{Path, PathBuf};

use lopdf::{Dictionary, Document, Object, Stream, dictionary};

/// Builds a one-page PDF (200 × 200 pt) with `content`. `setup` may add
/// resources or change the page/pages dictionaries.
pub fn pdf_with(
    content: &[u8],
    setup: impl FnOnce(&mut Document, &mut Dictionary, &mut Dictionary, &mut Dictionary),
) -> Vec<u8> {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let content_id = doc.add_object(Stream::new(Dictionary::new(), content.to_vec()));
    let mut resources = Dictionary::new();
    let mut page = dictionary! {
        "Type" => "Page",
        "Parent" => pages_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 200.into()],
        "Contents" => content_id,
    };
    let mut pages = dictionary! { "Type" => "Pages", "Count" => 1 };
    setup(&mut doc, &mut resources, &mut page, &mut pages);
    page.set("Resources", resources);
    let page_id = doc.add_object(page);
    pages.set("Kids", vec![page_id.into()]);
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

pub fn pdf(content: &str) -> Vec<u8> {
    pdf_with(content.as_bytes(), |_, _, _, _| {})
}

pub fn write_pdf(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The first page of a PDF, loaded back.
pub fn page_of(bytes: &[u8]) -> (Document, lopdf::ObjectId) {
    let doc = Document::load_mem(bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    (doc, id)
}

pub fn rect_of(doc: &Document, page: lopdf::ObjectId, key: &[u8]) -> Option<[f32; 4]> {
    let arr = doc
        .get_dictionary(page)
        .ok()?
        .get(key)
        .ok()?
        .as_array()
        .ok()?;
    let v: Vec<f32> = arr.iter().filter_map(|o| o.as_float().ok()).collect();
    Some([v[0], v[1], v[2], v[3]])
}

#[track_caller]
pub fn assert_box(actual: Option<[f32; 4]>, expected: [f32; 4]) {
    let a = actual.expect("expected content");
    for (x, y) in a.iter().zip(&expected) {
        assert!((x - y).abs() < 0.01, "got {a:?}, expected {expected:?}");
    }
}

/// Renders page 1 at `scale` pixels per point on white; returns a pixel
/// lookup in page points (origin bottom-left, like PDF coordinates).
pub fn render(bytes: &[u8], scale: f32) -> impl Fn(f32, f32) -> [u8; 3] + use<> {
    use hayro::{
        PixmapSettings, RenderCache, RenderSettings, hayro_interpret::InterpreterSettings,
        hayro_syntax::Pdf, vello_cpu::color::palette::css::WHITE,
    };
    let pdf = Pdf::new(bytes.to_vec()).unwrap();
    let page = &pdf.pages()[0];
    let pixmap = hayro::render(
        page,
        &RenderCache::new(),
        &InterpreterSettings::default(),
        &RenderSettings::default(),
        &PixmapSettings {
            x_scale: scale,
            y_scale: scale,
            bg_color: WHITE,
        },
    );
    let (w, h) = (pixmap.width() as usize, pixmap.height() as usize);
    let data = pixmap.data_as_u8_slice().to_vec();
    let crop = {
        let (doc, id) = page_of(bytes);
        rect_of(&doc, id, b"CropBox")
            .or_else(|| rect_of(&doc, id, b"MediaBox"))
            .unwrap()
    };
    move |x: f32, y: f32| {
        let px = (((x - crop[0]) * scale) as usize).min(w - 1);
        let py = (((crop[3] - y) * scale) as usize).min(h - 1);
        let i = (py * w + px) * 4;
        [data[i], data[i + 1], data[i + 2]]
    }
}

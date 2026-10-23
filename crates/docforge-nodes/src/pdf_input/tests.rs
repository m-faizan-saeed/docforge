//! Each test builds a tiny PDF by hand with a known answer.

use docforge_core::{EvalContext, Graph, GraphError, PdfPageInfo};
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

use super::*;
use crate::{InputImage, PrintLayout, print};

/// Builds a one-page PDF (200 × 200 pt) with `content`. `setup` may add
/// resources or change the page/pages dictionaries.
fn pdf_with(
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

fn pdf(content: &str) -> Vec<u8> {
    pdf_with(content.as_bytes(), |_, _, _, _| {})
}

fn analyze_with(bytes: &[u8], ignore_white_fills: bool) -> (PdfPageInfo, Stats) {
    let doc = Document::load_mem(bytes).unwrap();
    let page = *doc.get_pages().values().next().unwrap();
    analyze_page(&doc, page, Options { ignore_white_fills })
}

fn content_box(bytes: &[u8]) -> Option<[f32; 4]> {
    analyze_with(bytes, true).0.content_box
}

#[track_caller]
fn assert_box(actual: Option<[f32; 4]>, expected: [f32; 4]) {
    let a = actual.expect("expected content");
    for (x, y) in a.iter().zip(&expected) {
        assert!((x - y).abs() < 0.01, "got {a:?}, expected {expected:?}");
    }
}

#[test]
fn reference_cases_transforms_and_painting() {
    // Translation inside q/Q, then a rectangle outside it.
    assert_box(
        content_box(&pdf("q 1 0 0 1 50 60 cm 0 0 10 20 re f Q 5 5 2 2 re f")),
        [5.0, 5.0, 60.0, 80.0],
    );
    // Nested scale and 90° rotation: (x, y) → (−y, x), then ×2 and +10.
    assert_box(
        content_box(&pdf(
            "q 2 0 0 2 10 10 cm q 0 1 -1 0 0 0 cm 0 0 10 5 re f Q Q",
        )),
        [0.0, 10.0, 10.0, 30.0],
    );
    // `n` discards the path: nothing visible.
    assert_eq!(content_box(&pdf("0 0 50 50 re n")), None);
}

#[test]
fn curves_include_their_bulge_and_v_y_operators() {
    // Arch 10→110 with control points 100 above: true top at 10 + 75.
    assert_box(
        content_box(&pdf("0 w 10 10 m 10 110 110 110 110 10 c S")),
        [10.0, 10.0, 110.0, 85.0],
    );
    // `v` (first control point = current point) and `y` (second = end point).
    let v = content_box(&pdf("0 w 0 0 m 100 100 100 0 v S")).unwrap();
    assert!(v[2] > 70.0 && v[3] > 40.0, "{v:?}");
    let y = content_box(&pdf("0 w 0 0 m 0 100 100 0 y S")).unwrap();
    assert!(y[3] > 40.0, "{y:?}");
}

#[test]
fn strokes_are_widened_by_half_the_line_width() {
    assert_box(
        content_box(&pdf("8 w 10 10 m 50 10 l S")),
        [6.0, 6.0, 54.0, 14.0],
    );
    // Line width is scaled by the CTM.
    assert_box(
        content_box(&pdf("2 0 0 2 0 0 cm 4 w 10 10 m 20 10 l S")),
        [16.0, 16.0, 44.0, 24.0],
    );
}

#[test]
fn clipping_limits_what_counts() {
    // A page-sized fill clipped to a 50 × 50 square.
    assert_box(
        content_box(&pdf("q 20 20 50 50 re W n 0 g 0 0 200 200 re f Q")),
        [20.0, 20.0, 70.0, 70.0],
    );
    // Disjoint clips leave nothing visible.
    assert_eq!(
        content_box(&pdf(
            "q 0 0 10 10 re W n 50 50 10 10 re W n 0 0 200 200 re f Q"
        )),
        None
    );
    // The clip ends with Q.
    assert_box(
        content_box(&pdf("q 0 0 10 10 re W n Q 100 100 20 20 re f")),
        [100.0, 100.0, 120.0, 120.0],
    );
}

#[test]
fn white_fills_can_be_ignored() {
    let page = "1 g 0 0 200 200 re f 1 1 1 rg 0 0 100 100 re f 0 0 0 0 k 0 0 50 50 re f 0 g 50 50 10 10 re f";
    let bytes = pdf(page);
    let (ignored, stats) = analyze_with(&bytes, true);
    assert_box(ignored.content_box, [50.0, 50.0, 60.0, 60.0]);
    assert_eq!(stats.white_fills_ignored, 3);
    assert_box(
        analyze_with(&bytes, false).0.content_box,
        [0.0, 0.0, 200.0, 200.0],
    );
    // A white fill that is also stroked still counts (the stroke is visible).
    assert_box(
        content_box(&pdf("1 g 0 G 0 w 10 10 20 20 re B")),
        [10.0, 10.0, 30.0, 30.0],
    );
}

#[test]
fn form_xobjects_are_entered_with_matrix_and_bbox_clip() {
    let bytes = pdf_with(b"q 0.5 0 0 0.5 10 10 cm /Fm0 Do Q", |doc, res, _, _| {
        let form = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 40.into(), 40.into()],
                "Matrix" => vec![1.into(), 0.into(), 0.into(), 1.into(), 30.into(), 30.into()],
            },
            b"0 0 100 100 re f".to_vec(),
        );
        let id = doc.add_object(form);
        res.set("XObject", dictionary! { "Fm0" => id });
    });
    // (0,0) → form matrix (30,30) → ×0.5 + 10 = 25; the 100 pt square is
    // cut to the form's 40 pt BBox → 25…45.
    let (info, stats) = analyze_with(&bytes, true);
    assert_box(info.content_box, [25.0, 25.0, 45.0, 45.0]);
    assert_eq!(stats.forms, 1);
}

#[test]
fn self_referencing_forms_do_not_hang() {
    let bytes = pdf_with(b"/Fm0 Do", |doc, res, _, _| {
        let id = doc.new_object_id();
        let form = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Form",
                "BBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
                "Resources" => dictionary! { "XObject" => dictionary! { "Fm0" => id } },
            },
            b"0 0 10 10 re f /Fm0 Do".to_vec(),
        );
        doc.objects.insert(id, Object::Stream(form));
        res.set("XObject", dictionary! { "Fm0" => id });
    });
    let (info, stats) = analyze_with(&bytes, true);
    assert_box(info.content_box, [0.0, 0.0, 10.0, 10.0]);
    assert!(
        stats
            .warnings
            .iter()
            .any(|w| w.contains("refers to itself"))
    );
}

#[test]
fn images_are_measured_and_listed() {
    let bytes = pdf_with(b"q 80 0 0 40 20 30 cm /Im0 Do Q", |doc, res, _, _| {
        let image = Stream::new(
            dictionary! {
                "Type" => "XObject", "Subtype" => "Image", "Width" => 4, "Height" => 2,
                "ColorSpace" => "DeviceGray", "BitsPerComponent" => 8, "Filter" => "FlateDecode",
            },
            vec![0; 8],
        )
        .with_compression(false);
        let id = doc.add_object(image);
        res.set("XObject", dictionary! { "Im0" => id });
    });
    let (info, stats) = analyze_with(&bytes, true);
    assert_box(info.content_box, [20.0, 30.0, 100.0, 70.0]);
    assert_eq!(stats.images, 1);
    let img = &info.images[0];
    assert_eq!(
        (img.name.as_str(), img.width_px, img.height_px),
        ("Im0", 4, 2)
    );
    assert!(img.object.is_some());
    assert_eq!(img.filter.as_deref(), Some("FlateDecode"));
    assert_eq!(img.placed, [20.0, 30.0, 100.0, 70.0]);
}

#[test]
fn inline_images_are_measured() {
    let mut content = b"q 20 0 0 10 5 5 cm BI /W 2 /H 2 /CS /G /BPC 8 ID ".to_vec();
    content.extend([0u8, 255, 0, 255]);
    content.extend(b" EI Q");
    let bytes = pdf_with(&content, |_, _, _, _| {});
    let (info, _) = analyze_with(&bytes, true);
    assert_box(info.content_box, [5.0, 5.0, 25.0, 15.0]);
    assert_eq!(
        (info.images[0].name.as_str(), info.images[0].width_px),
        ("inline", 2)
    );
}

fn with_font(
    font: Dictionary,
) -> impl FnOnce(&mut Document, &mut Dictionary, &mut Dictionary, &mut Dictionary) {
    move |doc, res, _, _| {
        let id = doc.add_object(font);
        res.set("Font", dictionary! { "F1" => id });
    }
}

#[test]
fn text_is_measured_from_font_metrics() {
    // Standard font without widths: ~0.55 em per glyph, ascent .8, descent −.2.
    let helvetica =
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" };
    let bytes = pdf_with(b"BT /F1 10 Tf 20 50 Td (Hello) Tj ET", with_font(helvetica));
    assert_box(content_box(&bytes), [20.0, 48.0, 47.5, 58.0]);

    // Explicit widths, kerning in TJ and a font descriptor.
    let font = || {
        dictionary! {
            "Type" => "Font", "Subtype" => "TrueType", "BaseFont" => "Test",
            "FirstChar" => 65, "Widths" => vec![1000.into(), 500.into()],
            "FontDescriptor" => dictionary! { "Ascent" => 900, "Descent" => -100 },
        }
    };
    // A = 1 em, kern −1000 = +1 em, B = 0.5 em → 2.5 em at 10 pt.
    let bytes = pdf_with(
        b"BT /F1 10 Tf 0 0 Td [(A) -1000 (B)] TJ ET",
        with_font(font()),
    );
    assert_box(content_box(&bytes), [0.0, -0.0, 25.0, 9.0]);

    // Text matrix scaling and invisible text (Tr 3).
    let bytes = pdf_with(
        b"BT /F1 10 Tf 2 0 0 2 10 10 Tm (A) Tj ET",
        with_font(font()),
    );
    assert_box(content_box(&bytes), [10.0, 8.0, 30.0, 28.0]);
    let bytes = pdf_with(b"BT 3 Tr /F1 10 Tf (A) Tj ET", with_font(font()));
    assert_eq!(content_box(&bytes), None);
}

#[test]
fn two_byte_cid_fonts_use_w_widths() {
    let font = dictionary! {
        "Type" => "Font", "Subtype" => "Type0", "BaseFont" => "CID", "Encoding" => "Identity-H",
        "DescendantFonts" => vec![Object::Dictionary(dictionary! {
            "Type" => "Font", "Subtype" => "CIDFontType2", "DW" => 1000,
            "W" => vec![3.into(), vec![Object::Integer(250)].into(), 10.into(), 12.into(), 600.into()],
        })],
    };
    // Codes 3 (250) and 11 (600) → 0.85 em at 20 pt = 17.
    let bytes = pdf_with(b"BT /F1 20 Tf 0 100 Td <0003000B> Tj ET", with_font(font));
    let b = content_box(&bytes).unwrap();
    assert!((b[2] - 17.0).abs() < 0.01, "{b:?}");
}

#[test]
fn crop_box_inheritance_and_rotation() {
    let bytes = pdf_with(b"50 50 100 100 re f", |_, _, page, pages| {
        page.remove(b"MediaBox");
        pages.set("MediaBox", vec![0.into(), 0.into(), 200.into(), 200.into()]);
        pages.set("Rotate", 90);
        page.set("CropBox", vec![0.into(), 0.into(), 100.into(), 100.into()]);
    });
    let (info, _) = analyze_with(&bytes, true);
    assert_eq!(info.media_box, [0.0, 0.0, 200.0, 200.0]);
    assert_eq!(info.crop_box, [0.0, 0.0, 100.0, 100.0]);
    assert_eq!(info.rotation, 90);
    assert_box(info.content_box, [50.0, 50.0, 100.0, 100.0]);
}

#[test]
fn shadings_fill_their_clip() {
    assert_box(
        content_box(&pdf("q 10 10 30 30 re W n /Sh0 sh Q")),
        [10.0, 10.0, 40.0, 40.0],
    );
}

// ----- the node --------------------------------------------------------------

fn write_pdf(dir: &std::path::Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn node_reports_bounds_modes_and_padding() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "art.pdf", &pdf("0 g 72 72 72 36 re f"));
    let ctx = EvalContext::new();

    let doc = PdfInput::new(&path).load(&ctx).unwrap();
    let page = &doc.page_info[0];
    assert_box(page.content_box, [72.0, 72.0, 144.0, 108.0]);
    let [w, h] = page.content_size_mm().unwrap();
    assert!((w - 25.4).abs() < 0.01 && (h - 12.7).abs() < 0.01);
    assert!(
        doc.summary[0].contains("content 25.4 × 12.7 mm"),
        "{:?}",
        doc.summary
    );
    // Cropped: the page now is the content box.
    assert_eq!(page.media_box, [72.0, 72.0, 144.0, 108.0]);
    assert!((doc.pages_mm[0][0] - 25.4).abs() < 0.01);

    let untouched = PdfInput {
        crop_to_content: false,
        ..PdfInput::new(&path)
    }
    .load(&ctx)
    .unwrap();
    assert_eq!(
        untouched.bytes,
        std::fs::read(&path).unwrap(),
        "passes through unchanged"
    );
    assert_eq!(untouched.page_info[0].media_box, [0.0, 0.0, 200.0, 200.0]);
    assert_box(
        untouched.page_info[0].content_box,
        [72.0, 72.0, 144.0, 108.0],
    );

    let padded = PdfInput {
        padding_mm: 25.4,
        ..PdfInput::new(&path)
    };
    // 72 pt of padding, but never beyond the 200 pt page.
    assert_box(
        padded.load(&ctx).unwrap().page_info[0].content_box,
        [0.0, 0.0, 200.0, 180.0],
    );

    for (bounds, expected) in [
        (Bounds::CropBox, [0.0, 0.0, 200.0, 200.0]),
        (Bounds::MediaBox, [0.0, 0.0, 200.0, 200.0]),
    ] {
        let node = PdfInput {
            bounds,
            ..PdfInput::new(&path)
        };
        assert_box(node.load(&ctx).unwrap().page_info[0].content_box, expected);
    }
}

#[test]
fn node_rejects_bad_input() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = EvalContext::new();
    assert!(matches!(
        PdfInput::default().load(&ctx),
        Err(NodeError::InvalidParam { name: "path", .. })
    ));
    assert!(matches!(
        PdfInput::new(dir.path().join("none.pdf")).load(&ctx),
        Err(NodeError::Io { .. })
    ));
    let junk = write_pdf(dir.path(), "junk.pdf", b"definitely not a pdf");
    let err = PdfInput::new(&junk).load(&ctx).unwrap_err();
    assert!(err.to_string().contains("not a readable PDF"), "{err}");
}

#[test]
fn in_graph_output_is_a_document_and_tool_4_is_unaffected() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "art.pdf", &pdf("0 g 10 10 50 50 re f"));
    let mut g = Graph::new();
    let input = g.add(PdfInput::new(&path));
    let layout = g.add(PrintLayout::default());

    // The PDF's document output cannot be wired into Print Layout's image
    // input: existing pipelines keep their meaning.
    assert!(matches!(
        g.connect(input, OUTPUT, layout, print::INPUT),
        Err(GraphError::TypeMismatch { .. })
    ));

    let ctx = EvalContext::new();
    g.evaluate(&ctx);
    let doc = g.output(input, OUTPUT).unwrap().as_document().unwrap();
    assert_eq!(doc.page_info.len(), 1);
    assert!(
        g.evaluate(&ctx).recomputed.is_empty(),
        "cached until the file changes"
    );

    // Rewriting the file triggers a reload.
    std::fs::write(&path, pdf("0 g 10 10 80 80 re f")).unwrap();
    g.evaluate(&ctx);
    let doc = g.output(input, OUTPUT).unwrap().as_document().unwrap();
    assert_box(doc.page_info[0].content_box, [10.0, 10.0, 90.0, 90.0]);

    // Parameters survive a save/load cycle.
    let json = g.to_json().unwrap();
    let back = Graph::from_json(&json, &crate::registry()).unwrap();
    assert_eq!(
        back.node_as::<PdfInput>(input),
        g.node_as::<PdfInput>(input)
    );
    let _ = InputImage::default(); // other input kinds still registered alongside
    assert!(crate::registry().get(InputImage::KIND).is_some());
}

/// Reads a page's boxes and content back from output bytes.
fn page_of(bytes: &[u8]) -> (Document, lopdf::ObjectId) {
    let doc = Document::load_mem(bytes).unwrap();
    let id = *doc.get_pages().values().next().unwrap();
    (doc, id)
}

fn rect_of(doc: &Document, page: lopdf::ObjectId, key: &[u8]) -> Option<[f32; 4]> {
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

#[test]
fn output_is_cropped_to_content_without_touching_the_artwork() {
    let dir = tempfile::tempdir().unwrap();
    let original = pdf_with(b"0 g 40 50 30 20 re f", |_, _, page, _| {
        page.set("TrimBox", vec![0.into(), 0.into(), 200.into(), 200.into()]);
        page.set("Rotate", 90);
    });
    let path = write_pdf(dir.path(), "art.pdf", &original);
    let doc = PdfInput::new(&path).load(&EvalContext::new()).unwrap();

    let (out, page) = page_of(&doc.bytes);
    assert_eq!(
        rect_of(&out, page, b"MediaBox"),
        Some([40.0, 50.0, 70.0, 70.0])
    );
    assert_eq!(
        rect_of(&out, page, b"CropBox"),
        Some([40.0, 50.0, 70.0, 70.0])
    );
    assert_eq!(
        rect_of(&out, page, b"TrimBox"),
        None,
        "boxes outside the new page are dropped"
    );
    assert_eq!(
        out.get_dictionary(page)
            .unwrap()
            .get(b"Rotate")
            .unwrap()
            .as_i64()
            .unwrap(),
        90
    );
    // The drawing itself is byte-for-byte the original.
    let (orig, orig_page) = page_of(&original);
    assert_eq!(out.get_page_content(page), orig.get_page_content(orig_page));
    // Rotation swaps the displayed size: 30 × 20 pt shown as 20 × 30.
    let [w, h] = doc.pages_mm[0];
    assert!((w - 20.0 / PT_PER_MM).abs() < 0.01 && (h - 30.0 / PT_PER_MM).abs() < 0.01);
    assert!(
        doc.summary[0].contains("cropped to content"),
        "{:?}",
        doc.summary
    );

    // Analysing the cropped output again finds the same box: it is stable.
    let again = write_pdf(dir.path(), "again.pdf", &doc.bytes);
    let doc2 = PdfInput::new(&again).load(&EvalContext::new()).unwrap();
    assert_box(doc2.page_info[0].content_box, [40.0, 50.0, 70.0, 70.0]);
}

#[test]
fn blank_pages_are_left_as_they_are() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "blank.pdf", &pdf("1 g 0 0 200 200 re f"));
    let doc = PdfInput::new(&path).load(&EvalContext::new()).unwrap();
    assert_eq!(doc.page_info[0].content_box, None);
    assert_eq!(doc.bytes, std::fs::read(&path).unwrap());
    assert!(
        doc.summary
            .iter()
            .any(|s| s.contains("No visible content on page 1"))
    );
}

#[test]
fn encrypted_pdfs_with_an_empty_password_are_cropped_into_a_readable_file() {
    use lopdf::{EncryptionState, EncryptionVersion, Permissions};
    let plain = pdf("0 g 10 20 30 40 re f");
    let mut doc = Document::load_mem(&plain).unwrap();
    doc.trailer.set(
        "ID",
        vec![
            Object::string_literal("0123456789abcdef"),
            Object::string_literal("0123456789abcdef"),
        ],
    );
    let state = EncryptionState::try_from(EncryptionVersion::V2 {
        document: &doc,
        owner_password: "owner",
        user_password: "",
        key_length: 128,
        permissions: Permissions::PRINTABLE,
    })
    .unwrap();
    doc.encrypt(&state).unwrap();
    let mut encrypted = Vec::new();
    doc.save_to(&mut encrypted).unwrap();
    let has_encrypt = |bytes: &[u8]| bytes.windows(8).any(|w| w == b"/Encrypt");
    assert!(has_encrypt(&encrypted), "fixture really is encrypted");

    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "locked.pdf", &encrypted);
    let out = PdfInput::new(&path).load(&EvalContext::new()).unwrap();
    assert_box(out.page_info[0].content_box, [10.0, 20.0, 40.0, 60.0]);

    let (reloaded, page) = page_of(&out.bytes);
    assert!(!has_encrypt(&out.bytes), "written decrypted");
    assert_eq!(
        rect_of(&reloaded, page, b"MediaBox"),
        Some([10.0, 20.0, 40.0, 60.0])
    );
    let content = String::from_utf8_lossy(&reloaded.get_page_content(page)).into_owned();
    assert!(content.contains("re"), "content is readable: {content:?}");
}

// ----- embedded image extraction ---------------------------------------------

/// Pixel data of a 3 × 2 RGB test image, row by row.
const RGB_3X2: [[u8; 3]; 6] = [
    [255, 0, 0],
    [0, 255, 0],
    [0, 0, 255],
    [255, 255, 0],
    [0, 255, 255],
    [255, 0, 255],
];

fn image_stream(dict: Dictionary, data: Vec<u8>) -> Stream {
    let mut d = dictionary! { "Type" => "XObject", "Subtype" => "Image", "BitsPerComponent" => 8 };
    for (k, v) in dict.iter() {
        d.set(k.clone(), v.clone());
    }
    Stream::new(d, data).with_compression(false)
}

/// A page drawing `image` as /Im0 with `content`; also returns the loaded node output.
fn load_with_image(
    content: &str,
    image: Stream,
    rotate: Option<i64>,
    selected: &[&str],
) -> PdfDocument {
    let bytes = pdf_with(content.as_bytes(), |doc, res, page, _| {
        let id = doc.add_object(image);
        res.set("XObject", dictionary! { "Im0" => id });
        if let Some(r) = rotate {
            page.set("Rotate", r);
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "img.pdf", &bytes);
    let node = PdfInput {
        selected_images: selected.iter().map(|s| s.to_string()).collect(),
        ..PdfInput::new(&path)
    };
    node.load(&EvalContext::new()).unwrap()
}

fn extract_one(doc: &PdfDocument, key: &str) -> docforge_core::RasterImage {
    images::extract(doc, &[key], None).remove(0).unwrap()
}

fn rgb_image() -> Stream {
    image_stream(
        dictionary! { "Width" => 3, "Height" => 2, "ColorSpace" => "DeviceRGB" },
        RGB_3X2.concat(),
    )
}

#[test]
fn extracted_pixels_match_the_stored_image_exactly() {
    let doc = load_with_image("q 216 0 0 144 10 10 cm /Im0 Do Q", rgb_image(), None, &[]);
    let list = images::list(&doc);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].key, "p1 Im0");
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.dimensions(), (3, 2));
    for (i, rgb) in RGB_3X2.iter().enumerate() {
        assert_eq!(img.pixels()[i], [rgb[0], rgb[1], rgb[2], 255], "pixel {i}");
    }
    // 3 px across 216 pt (3 inches) → 1 dpi.
    assert!((img.dpi().unwrap() - 1.0).abs() < 1e-4);
}

#[test]
fn decode_arrays_and_palettes_are_applied() {
    // Gray with /Decode [1 0]: stored 0 shows white, 255 shows black.
    let gray = image_stream(
        dictionary! { "Width" => 2, "Height" => 1, "ColorSpace" => "DeviceGray",
        "Decode" => vec![1.into(), 0.into()] },
        vec![0, 255],
    );
    let doc = load_with_image("q 20 0 0 10 0 0 cm /Im0 Do Q", gray, None, &[]);
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.pixels(), [[255, 255, 255, 255], [0, 0, 0, 255]]);

    // Indexed: index 0 = red, 1 = green.
    let palette = Object::Array(vec![
        "Indexed".into(),
        "DeviceRGB".into(),
        1.into(),
        Object::String(vec![255, 0, 0, 0, 255, 0], lopdf::StringFormat::Hexadecimal),
    ]);
    let indexed = image_stream(
        dictionary! { "Width" => 2, "Height" => 1, "ColorSpace" => palette },
        vec![1, 0],
    );
    let doc = load_with_image("q 20 0 0 10 0 0 cm /Im0 Do Q", indexed, None, &[]);
    assert_eq!(
        extract_one(&doc, "p1 Im0").pixels(),
        [[0, 255, 0, 255], [255, 0, 0, 255]]
    );
}

#[test]
fn jpeg_images_decode_close_to_the_original() {
    use image::{ExtendedColorType, ImageEncoder, codecs::jpeg::JpegEncoder};
    let (w, h) = (32u32, 16u32);
    let rgb: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i % w * 8) as u8, (i / w * 16) as u8, 128])
        .collect();
    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut jpeg, 95)
        .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
        .unwrap();
    let stream = image_stream(
        dictionary! { "Width" => w as i64, "Height" => h as i64, "ColorSpace" => "DeviceRGB", "Filter" => "DCTDecode" },
        jpeg,
    );
    let doc = load_with_image("q 64 0 0 32 0 0 cm /Im0 Do Q", stream, None, &[]);
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.dimensions(), (w, h));
    let worst = img
        .pixels()
        .iter()
        .zip(rgb.as_chunks::<3>().0)
        .flat_map(|(a, b)| (0..3).map(move |c| (a[c] as i32 - b[c] as i32).abs()))
        .max()
        .unwrap();
    assert!(worst <= 24, "JPEG decoded {worst} levels off");
}

#[test]
fn soft_masks_become_transparency() {
    let bytes = pdf_with(b"q 30 0 0 10 0 0 cm /Im0 Do Q", |doc, res, _, _| {
        let mask = doc.add_object(image_stream(
            dictionary! { "Width" => 3, "Height" => 1, "ColorSpace" => "DeviceGray" },
            vec![0, 128, 255],
        ));
        let mut img = image_stream(
            dictionary! { "Width" => 3, "Height" => 1, "ColorSpace" => "DeviceRGB" },
            [[0u8, 0, 255]; 3].concat(),
        );
        img.dict.set("SMask", mask);
        let id = doc.add_object(img);
        res.set("XObject", dictionary! { "Im0" => id });
    });
    let dir = tempfile::tempdir().unwrap();
    let doc = PdfInput::new(write_pdf(dir.path(), "m.pdf", &bytes))
        .load(&EvalContext::new())
        .unwrap();
    let alpha: Vec<u8> = extract_one(&doc, "p1 Im0")
        .pixels()
        .iter()
        .map(|p| p[3])
        .collect();
    assert_eq!(alpha[0], 0);
    assert!((alpha[1] as i32 - 128).abs() <= 2, "{alpha:?}");
    assert_eq!(alpha[2], 255);
    assert_eq!(extract_one(&doc, "p1 Im0").pixels()[2], [0, 0, 255, 255]);
}

#[test]
fn images_come_out_as_they_appear_on_the_page() {
    let src = |u: usize, v: usize| {
        let p = RGB_3X2[v * 3 + u];
        [p[0], p[1], p[2], 255]
    };
    // Placed turned 90° counter-clockwise: 2 wide × 3 tall, top-left → bottom-left.
    let doc = load_with_image("q 0 216 -144 0 200 10 cm /Im0 Do Q", rgb_image(), None, &[]);
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.dimensions(), (2, 3));
    assert_eq!(img.get_pixel(0, 2), Some(src(0, 0)));
    assert_eq!(img.get_pixel(1, 2), Some(src(0, 1)));
    assert_eq!(img.get_pixel(0, 0), Some(src(2, 0)));
    // The displayed width is 144 pt (2 in) for 2 px → 1 dpi.
    assert!((img.dpi().unwrap() - 1.0).abs() < 1e-4);

    // Page shown rotated 90° clockwise: top-left → top-right.
    let doc = load_with_image("q 216 0 0 144 0 0 cm /Im0 Do Q", rgb_image(), Some(90), &[]);
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.dimensions(), (2, 3));
    assert_eq!(img.get_pixel(1, 0), Some(src(0, 0)));
    assert_eq!(img.get_pixel(0, 0), Some(src(0, 1)));

    // Mirrored horizontally (negative x scale).
    let doc = load_with_image("q -216 0 0 144 300 0 cm /Im0 Do Q", rgb_image(), None, &[]);
    let img = extract_one(&doc, "p1 Im0");
    assert_eq!(img.get_pixel(0, 0), Some(src(2, 0)));
}

#[test]
fn keys_identify_each_image_object_once() {
    use docforge_core::{PdfImageInfo, PdfPageInfo};
    let img = |name: &str, object: Option<(u32, u16)>| PdfImageInfo {
        key: None,
        name: name.into(),
        object,
        width_px: 1,
        height_px: 1,
        placed: [0.0; 4],
        filter: None,
        matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    };
    let mut pages = vec![
        PdfPageInfo {
            images: vec![
                img("Im0", Some((5, 0))),
                img("Im0", Some((5, 0))),
                img("inline", None),
            ],
            ..Default::default()
        },
        // Same object again (one key), and a different object also named Im0.
        PdfPageInfo {
            images: vec![img("Im0", Some((5, 0))), img("Im0", Some((9, 0)))],
            ..Default::default()
        },
    ];
    images::assign_keys(&mut pages);
    let keys: Vec<Option<&str>> = pages
        .iter()
        .flat_map(|p| p.images.iter().map(|i| i.key.as_deref()))
        .collect();
    assert_eq!(
        keys,
        [
            Some("p1 Im0"),
            Some("p1 Im0"),
            None,
            Some("p1 Im0"),
            Some("p2 Im0")
        ]
    );
}

#[test]
fn selected_images_become_output_ports_and_feed_enhance_and_layout() {
    let bytes = pdf_with(b"q 216 0 0 144 10 10 cm /Im0 Do Q", |doc, res, _, _| {
        let id = doc.add_object(rgb_image());
        res.set("XObject", dictionary! { "Im0" => id });
    });
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "img.pdf", &bytes);

    let mut g = Graph::new();
    let input = g.add(PdfInput {
        selected_images: vec!["p1 Im0".into()],
        ..PdfInput::new(&path)
    });
    let ports: Vec<String> = g
        .node(input)
        .unwrap()
        .outputs()
        .iter()
        .map(|p| p.name.to_string())
        .collect();
    assert_eq!(ports, ["pdf", "p1 Im0"]);

    let enhance = g.add(crate::EnhanceImage::new([crate::Effect::Grayscale]));
    let layout = g.add(PrintLayout::default());
    g.connect(input, "p1 Im0", enhance, crate::enhance::INPUT)
        .unwrap();
    g.connect(enhance, crate::enhance::OUTPUT, layout, print::INPUT)
        .unwrap();
    let report = g.evaluate(&EvalContext::new());
    assert!(report.is_ok(), "{:?}", report.failed);

    let img = g.output(input, "p1 Im0").unwrap().as_image().unwrap();
    assert_eq!(img.dimensions(), (3, 2));
    let grey = g
        .output(enhance, crate::enhance::OUTPUT)
        .unwrap()
        .as_image()
        .unwrap();
    assert!(grey.pixels().iter().all(|p| p[0] == p[1] && p[1] == p[2]));
    let pdf = g
        .output(layout, print::OUTPUT)
        .unwrap()
        .as_document()
        .unwrap();
    // 3 px at 1 dpi = 76.2 mm wide: printed at the size it had in the PDF.
    assert!(pdf.summary[0].starts_with("1 piece"), "{:?}", pdf.summary);

    // Deselecting removes the port and its connection.
    g.edit_node::<PdfInput, _>(input, |n| n.selected_images.clear())
        .unwrap();
    assert!(g.edges().iter().all(|e| e.from.node != input));

    // A key that is not in the PDF is reported.
    g.edit_node::<PdfInput, _>(input, |n| n.selected_images = vec!["p9 Nope".into()])
        .unwrap();
    g.evaluate(&EvalContext::new());
    let Some(docforge_core::NodeStatus::Failed(err)) = g.status(input) else {
        panic!()
    };
    assert!(err.to_string().contains("not in this PDF"), "{err}");
}

//! PDF Input: loading, and extraction of embedded images.

use docforge_core::{EvalContext, Graph, GraphError};
use lopdf::{Dictionary, Object, Stream, dictionary};

use super::*;
use crate::{InputImage, PrintLayout, pdf::test_support::*, print};

#[test]
fn loads_the_file_unchanged_with_every_page_as_its_content() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_pdf(dir.path(), "art.pdf", &pdf("0 g 72 72 72 36 re f"));
    let doc = PdfInput::new(&path).load(&EvalContext::new()).unwrap();
    assert_eq!(doc.bytes, std::fs::read(&path).unwrap(), "no cropping here");
    let page = &doc.page_info[0];
    assert_eq!(page.media_box, [0.0, 0.0, 200.0, 200.0]);
    assert_eq!(
        page.content_box,
        Some(page.crop_box),
        "the whole visible page"
    );
    assert!((doc.pages_mm[0][0] - 200.0 / crate::pdf::PT_PER_MM).abs() < 0.01);
    assert!(
        doc.summary[0].starts_with("PDF 1 page"),
        "{:?}",
        doc.summary
    );
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
    let first = g
        .output(input, OUTPUT)
        .unwrap()
        .as_document()
        .unwrap()
        .clone();
    assert!(
        g.evaluate(&ctx).recomputed.is_empty(),
        "cached until the file changes"
    );

    // Rewriting the file triggers a reload.
    std::fs::write(&path, pdf("0 g 10 10 80 80 re f")).unwrap();
    g.evaluate(&ctx);
    let doc = g.output(input, OUTPUT).unwrap().as_document().unwrap();
    assert_ne!(doc.bytes, first.bytes);

    // Parameters survive a save/load cycle.
    let json = g.to_json().unwrap();
    let back = Graph::from_json(&json, &crate::registry()).unwrap();
    assert_eq!(
        back.node_as::<PdfInput>(input),
        g.node_as::<PdfInput>(input)
    );
    assert!(crate::registry().get(InputImage::KIND).is_some());
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

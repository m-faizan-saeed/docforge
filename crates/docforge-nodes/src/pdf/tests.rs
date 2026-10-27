//! Content-box analysis: each test builds a tiny PDF by hand with a known
//! answer.

use docforge_core::PdfPageInfo;
use lopdf::{Dictionary, Document, Object, Stream, dictionary};

use super::{
    PageBox,
    analyze::{Options, Stats, analyze_page},
    displayed_to_page, page_to_displayed,
    test_support::*,
};

fn analyze_with(bytes: &[u8], ignore_white_fills: bool) -> (PdfPageInfo, Stats) {
    let doc = Document::load_mem(bytes).unwrap();
    let page = *doc.get_pages().values().next().unwrap();
    analyze_page(&doc, page, Options { ignore_white_fills })
}

fn content_box(bytes: &[u8]) -> Option<[f32; 4]> {
    analyze_with(bytes, true).0.content_box
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

#[test]
fn displayed_and_page_coordinates_round_trip_at_every_rotation() {
    // A 200 × 100 pt page whose visible area starts at (50, 20).
    let crop = [50.0, 20.0, 250.0, 120.0];
    // The top-left quarter of the page as displayed.
    let quarter = [0.0, 0.0, 0.5, 0.5];
    let expected = [
        (0, [50.0, 70.0, 150.0, 120.0]),    // upright: top-left of the page
        (90, [50.0, 20.0, 150.0, 70.0]),    // shown turned clockwise: bottom-left
        (180, [150.0, 20.0, 250.0, 70.0]),  // upside down: bottom-right
        (270, [150.0, 70.0, 250.0, 120.0]), // turned anticlockwise: top-right
    ];
    for (rotation, page_rect) in expected {
        let page = PageBox { crop, rotation };
        let got = displayed_to_page(quarter, page);
        assert_box(Some(got), page_rect);
        let back = page_to_displayed(got, page);
        assert_box(Some(back), quarter);
    }
}

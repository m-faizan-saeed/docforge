//! Prints what PDF Edit's "crop to content" finds on each page of a PDF —
//! the content box — plus the images painted on it. With a second argument,
//! also saves the PDF cropped to its content.
//!
//!     cargo run --release -p docforge-nodes --example pdf_bounds -- file.pdf [cropped.pdf]

use docforge_core::EvalContext;
use docforge_nodes::{PdfEdit, PdfInput};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .expect("usage: pdf_bounds <file.pdf> [cropped.pdf]");
    let save_to = args.next();
    let doc = match PdfInput::new(path)
        .load(&EvalContext::new())
        .and_then(|loaded| PdfEdit::default().apply(&loaded))
    {
        Ok(doc) => doc,
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
    for line in &doc.summary {
        println!("{line}");
    }
    let mm = |pt: f32| pt * 25.4 / 72.0;
    for (i, page) in doc.page_info.iter().enumerate() {
        println!(
            "\npage {}: crop box {:?} pt, rotation {}°",
            i + 1,
            page.crop_box,
            page.rotation
        );
        match (page.content_box, page.content_size_mm()) {
            (Some(b), Some([w, h])) => println!(
                "  content: [{:.2}, {:.2}, {:.2}, {:.2}] pt = {w:.2} × {h:.2} mm at ({:.2}, {:.2}) mm",
                b[0],
                b[1],
                b[2],
                b[3],
                mm(b[0]),
                mm(b[1])
            ),
            _ => println!("  content: none (blank page)"),
        }
        for img in &page.images {
            println!(
                "  image {:<8} {} × {} px, {}, placed {:?}",
                img.name,
                img.width_px,
                img.height_px,
                img.filter.as_deref().unwrap_or("raw"),
                img.placed.map(|v| (v * 10.0).round() / 10.0)
            );
        }
    }
    if let Some(out) = save_to {
        std::fs::write(&out, &doc.bytes).expect("write cropped PDF");
        println!("\nsaved cropped PDF to {}", out.to_string_lossy());
    }
}

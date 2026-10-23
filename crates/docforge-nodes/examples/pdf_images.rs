//! Lists the images embedded in a PDF and, with an output folder, saves every
//! one as PNG — oriented as on the page and tagged with its print DPI.
//!
//!     cargo run --release -p docforge-nodes --example pdf_images -- file.pdf [out-dir]

use std::time::Instant;

use docforge_core::EvalContext;
use docforge_nodes::{OutputImage, PdfInput, pdf_input::images};

fn main() {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().expect("usage: pdf_images <file.pdf> [out-dir]");
    let out_dir = args.next().map(std::path::PathBuf::from);
    let doc = PdfInput::new(path)
        .load(&EvalContext::new())
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1)
        });
    let entries = images::list(&doc);
    println!(
        "{} image(s), {} inline (not extractable)",
        entries.len(),
        images::inline_count(&doc)
    );
    for e in &entries {
        println!(
            "  {:<12} page {:<3} {} × {} px  {}",
            e.key,
            e.page,
            e.info.width_px,
            e.info.height_px,
            e.info.filter.as_deref().unwrap_or("raw")
        );
    }
    let Some(dir) = out_dir else { return };
    std::fs::create_dir_all(&dir).unwrap();
    for e in &entries {
        let start = Instant::now();
        match images::extract(&doc, &[e.key.as_str()], None).remove(0) {
            Ok(img) => {
                let file = dir.join(format!("{}.png", e.key.replace(' ', "_")));
                OutputImage::new(&file)
                    .save(&img, &EvalContext::new())
                    .unwrap();
                println!(
                    "  saved {} ({} × {} px, {:.0} dpi) in {:.0?}",
                    file.display(),
                    img.width(),
                    img.height(),
                    img.dpi().unwrap_or(0.0),
                    start.elapsed()
                );
            }
            Err(err) => println!("  {}: {err}", e.key),
        }
    }
}

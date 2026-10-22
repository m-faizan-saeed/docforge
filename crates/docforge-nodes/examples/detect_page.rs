//! Prints the page corners Crop-to-document finds in a photo.
//!
//!     cargo run --release -p docforge-nodes --example detect_page -- photo.jpg

use docforge_core::EvalContext;
use docforge_nodes::{InputImage, enhance::detect_page};

fn main() {
    let path = std::env::args_os()
        .nth(1)
        .expect("usage: detect_page <image>");
    let img = InputImage::new(path)
        .load(&EvalContext::new())
        .expect("load image");
    match detect_page(&img) {
        Some(quad) => {
            for (name, [x, y]) in ["top-left", "top-right", "bottom-right", "bottom-left"]
                .iter()
                .zip(quad)
            {
                println!("{name:>12}: ({x:.0}, {y:.0})");
            }
        }
        None => println!("no page found"),
    }
}

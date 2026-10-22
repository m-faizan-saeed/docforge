//! Times every Enhance Image effect on a 12-megapixel test page.
//!
//!     cargo run --release -p docforge-nodes --example bench_effects [image]

use std::time::Instant;

use docforge_core::{EvalContext, RasterImage};
use docforge_nodes::{Effect, InputImage};

fn main() {
    let img = match std::env::args_os().nth(1) {
        Some(path) => InputImage::new(path)
            .load(&EvalContext::new())
            .expect("load image"),
        None => RasterImage::from_rgba8(
            4000,
            3000,
            (0..4000u32 * 3000)
                .flat_map(|i| {
                    let (x, y) = (i % 4000, i / 4000);
                    let page = (600..3400).contains(&x) && (300..2700).contains(&y);
                    let ink = page && y % 60 < 6 && (800..3200).contains(&x);
                    let v = if ink {
                        40
                    } else if page {
                        225 - (x / 60) as u8
                    } else {
                        70
                    };
                    [v, v, v.saturating_sub(8), 255]
                })
                .collect(),
        )
        .unwrap(),
    };
    println!("{} x {} px", img.width(), img.height());
    for effect in Effect::catalogue() {
        let start = Instant::now();
        let out = effect.apply(&img).expect("effect runs");
        println!(
            "  {:<22} {:>7.1?}  -> {} x {}",
            effect.name(),
            start.elapsed(),
            out.width(),
            out.height()
        );
    }
}

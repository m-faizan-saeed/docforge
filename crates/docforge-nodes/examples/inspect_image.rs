//! Loads an image through a one-node graph, saves the graph, reloads it and
//! shows that the second evaluation is served from cache.
//!
//!     cargo run -p docforge-nodes --example inspect_image -- path/to/scan.jpg [dpi]

use std::{error::Error, path::PathBuf, time::Instant};

use docforge_core::{EvalContext, Graph};
use docforge_nodes::{InputImage, input_image};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(args.next().ok_or("usage: inspect_image <image> [dpi]")?);
    let dpi = args
        .next()
        .map(|d| d.to_string_lossy().parse::<f32>())
        .transpose()?;

    let mut node = InputImage::new(&path);
    node.dpi = dpi;
    let mut graph = Graph::new();
    let input = graph.add(node);
    let ctx = EvalContext::new();

    let start = Instant::now();
    let report = graph.evaluate(&ctx);
    if let Some((id, err)) = report.failed.first() {
        return Err(format!("node {id} failed: {err}").into());
    }
    let image = graph
        .output(input, input_image::OUTPUT)
        .and_then(|v| v.as_image())
        .ok_or("no image produced")?;
    println!("loaded {} in {:.1?}", path.display(), start.elapsed());
    println!("  pixels : {} x {}", image.width(), image.height());
    match (image.dpi(), image.size_mm()) {
        (Some(dpi), Some((w, h))) => println!("  dpi    : {dpi:.0}  ->  {w:.1} x {h:.1} mm"),
        _ => println!("  dpi    : unknown (pass one as the 2nd argument)"),
    }

    let saved = std::env::temp_dir().join("docforge-inspect.json");
    graph.save(&saved)?;
    println!(
        "\ngraph saved to {}:\n{}",
        saved.display(),
        graph.to_json()?
    );

    let mut reloaded = Graph::load(&saved, &docforge_nodes::registry())?;
    reloaded.evaluate(&ctx);
    let start = Instant::now();
    let report = reloaded.evaluate(&ctx);
    println!(
        "\nreloaded graph re-evaluated in {:.1?}; nodes recomputed: {}",
        start.elapsed(),
        report.recomputed.len()
    );
    Ok(())
}

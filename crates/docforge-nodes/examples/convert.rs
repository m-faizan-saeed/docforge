//! Converts an image through an Input → Output graph and saves that graph as
//! a reusable project file.
//!
//!     cargo run -p docforge-nodes --example convert -- in.jpg out.png [dpi] [project.json]
//!
//! The project can then be re-run with the `run_graph` example.

use std::{error::Error, path::PathBuf};

use docforge_core::{EvalContext, Graph};
use docforge_nodes::{InputImage, OutputImage, input_image, output_image};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [input, output, rest @ ..] = args.as_slice() else {
        return Err("usage: convert <input> <output> [dpi] [project.json]".into());
    };
    let dpi = rest
        .first()
        .map(|d| d.to_string_lossy().parse::<f32>())
        .transpose()?;

    let mut graph = Graph::new();
    let mut source = InputImage::new(input);
    source.dpi = dpi;
    let src = graph.add(source);
    let dst = graph.add(OutputImage::new(output));
    graph.connect(src, input_image::OUTPUT, dst, output_image::INPUT)?;

    let report = graph.evaluate(&EvalContext::new());
    if let Some((id, err)) = report.failed.first() {
        return Err(format!("node {id} failed: {err}").into());
    }
    let written = graph
        .node_as::<OutputImage>(dst)
        .and_then(OutputImage::last_written_path)
        .ok_or("nothing written")?;
    println!("wrote {}", written.display());

    if let Some(project) = rest.get(1).map(PathBuf::from) {
        graph.save(&project)?;
        println!("saved project {}", project.display());
    }
    Ok(())
}

//! Headless runner: loads a saved project and evaluates every node. Relative
//! paths inside the project resolve against the project's own directory.
//!
//!     cargo run -p docforge-nodes --example run_graph -- project.json

use std::{error::Error, path::PathBuf, process::ExitCode, time::Instant};

use docforge_core::{EvalContext, Graph, NodeStatus};

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let path = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("usage: run_graph <project.json>")?,
    );
    let mut graph = Graph::load(&path, &docforge_nodes::registry())?;
    let base = path.parent().map(PathBuf::from).unwrap_or_default();
    let ctx = EvalContext::new().with_base_dir(base);

    let start = Instant::now();
    let report = graph.evaluate(&ctx);
    println!("evaluated {} nodes in {:.1?}", graph.len(), start.elapsed());

    for (id, node) in graph.nodes() {
        let status = match graph.status(id) {
            Some(NodeStatus::Ready(_)) => "ok".to_owned(),
            Some(NodeStatus::Failed(err)) => format!("FAILED: {err}"),
            Some(NodeStatus::Blocked) => "blocked by an upstream failure".to_owned(),
            Some(NodeStatus::Pending) | None => "not evaluated".to_owned(),
        };
        println!("  {id} {:<14} {status}", node.kind());
    }
    Ok(if report.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

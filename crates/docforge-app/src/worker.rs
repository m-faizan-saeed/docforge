//! Background evaluation.
//!
//! The UI owns the document graph and never runs nodes itself. After each
//! edit it sends a snapshot ([`GraphDoc`]) here; the worker keeps its own
//! [`Graph`], syncs it to the snapshot (keeping the caches of unchanged
//! nodes), evaluates, and posts the results back. A newer snapshot cancels the
//! one in progress, so dragging a slider never queues up stale work.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use web_time::Instant;

use docforge_core::{
    EvalContext, Graph, GraphDoc, NodeError, NodeId, NodeRegistry, NodeStatus, Value,
};
use docforge_nodes::{OutputImage, PrintLayout};

use crate::background::Background;

pub struct Job {
    pub rev: u64,
    pub doc: GraphDoc,
    pub base_dir: Option<PathBuf>,
    /// Auxiliary values to collect after evaluating (see `Node::probe`),
    /// e.g. the image an Enhance step receives, for the bounds editor.
    pub probes: Vec<(NodeId, String)>,
}

#[derive(Clone, Debug)]
pub enum NodeResult {
    Pending,
    Ready(Vec<Value>),
    Failed(Arc<NodeError>),
    Blocked,
}

pub struct EvalResult {
    /// Revision of the document this result belongs to.
    pub rev: u64,
    pub nodes: HashMap<NodeId, NodeResult>,
    /// Files written by Output Image nodes.
    pub written: HashMap<NodeId, PathBuf>,
    pub recomputed: usize,
    pub elapsed: Duration,
    pub cancelled: bool,
    pub sync_error: Option<String>,
    pub probes: HashMap<(NodeId, String), Value>,
}

pub struct Evaluator {
    jobs: Background<Job>,
    results: mpsc::Receiver<EvalResult>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl Evaluator {
    /// Starts the worker (a thread on desktop). `wake_ui` is called whenever
    /// a result is ready (pass something that requests a repaint).
    pub fn spawn(registry: NodeRegistry, wake_ui: impl Fn() + Send + Sync + 'static) -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let busy = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            registry,
            results: result_tx,
            cancel: Arc::clone(&cancel),
            busy: Arc::clone(&busy),
        };
        let wake_ui = Arc::new(wake_ui);
        let wake = Arc::clone(&wake_ui);
        let mut graph = Graph::new();
        let jobs = Background::spawn(
            "docforge-eval",
            move || wake(),
            move |job| worker.handle(&mut graph, job, &*wake_ui),
        );
        Self {
            jobs,
            results: result_rx,
            cancel,
            busy,
        }
    }

    /// Queues a snapshot, cancelling whatever is being evaluated.
    pub fn submit(&self, job: Job) {
        self.cancel.store(true, Ordering::SeqCst);
        self.busy.store(true, Ordering::SeqCst);
        self.jobs.send(job);
    }

    /// The newest finished result, if any arrived since the last call. On
    /// the web this is where the evaluation itself runs.
    pub fn poll(&self) -> Option<EvalResult> {
        self.jobs.poll();
        self.results.try_iter().last()
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }
}

struct Worker {
    registry: NodeRegistry,
    results: mpsc::Sender<EvalResult>,
    cancel: Arc<AtomicBool>,
    busy: Arc<AtomicBool>,
}

impl Worker {
    /// Evaluates the newest snapshot (older queued ones are skipped).
    fn handle(&self, graph: &mut Graph, job: Job, wake_ui: &dyn Fn()) {
        self.cancel.store(false, Ordering::SeqCst);
        let result = self.evaluate(graph, job);
        if self.jobs_pending() {
            return; // a newer job is waiting; its result supersedes this one
        }
        self.busy.store(false, Ordering::SeqCst);
        if self.results.send(result).is_ok() {
            wake_ui();
        }
    }

    fn jobs_pending(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn evaluate(&self, graph: &mut Graph, job: Job) -> EvalResult {
        let start = Instant::now();
        let mut result = EvalResult {
            rev: job.rev,
            nodes: HashMap::new(),
            written: HashMap::new(),
            recomputed: 0,
            elapsed: Duration::ZERO,
            cancelled: false,
            sync_error: None,
            probes: HashMap::new(),
        };
        if let Err(err) = graph.sync_from_doc(&job.doc, &self.registry) {
            result.sync_error = Some(err.to_string());
            // Start over from scratch next time rather than from a half-synced graph.
            *graph = Graph::new();
            return result;
        }

        let mut ctx = EvalContext::new().with_cancel_flag(Arc::clone(&self.cancel));
        if let Some(dir) = job.base_dir {
            ctx = ctx.with_base_dir(dir);
        }
        let report = graph.evaluate(&ctx);
        result.cancelled = report.cancelled;
        result.recomputed = report.recomputed.len();
        result.elapsed = start.elapsed();

        for (id, key) in job.probes {
            if let Some(value) = graph.probe(id, &key) {
                result.probes.insert((id, key), value);
            }
        }
        for (id, node) in graph.nodes() {
            let status = match graph.status(id) {
                Some(NodeStatus::Ready(values)) => NodeResult::Ready(values.to_vec()),
                Some(NodeStatus::Failed(err)) => NodeResult::Failed(Arc::clone(err)),
                Some(NodeStatus::Blocked) => NodeResult::Blocked,
                Some(NodeStatus::Pending) | None => NodeResult::Pending,
            };
            result.nodes.insert(id, status);
            let written = node
                .downcast_ref::<OutputImage>()
                .and_then(OutputImage::last_written_path)
                .or_else(|| {
                    node.downcast_ref::<PrintLayout>()
                        .and_then(PrintLayout::last_written_path)
                });
            if let Some(path) = written {
                result.written.insert(id, path);
            }
        }
        result
    }
}

use std::{
    any::{Any, type_name},
    collections::{BTreeMap, HashSet, VecDeque},
    fmt, fs,
    panic::{self, AssertUnwindSafe},
    path::Path,
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use crate::{EvalContext, GraphError, Inputs, Node, NodeError, NodeRegistry, Value};

/// Version written to saved graphs. Bump on incompatible format changes.
pub const GRAPH_FORMAT_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// One end of an edge: a named port on a node.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PortRef {
    pub node: NodeId,
    pub port: String,
}

impl PortRef {
    pub fn new(node: NodeId, port: impl Into<String>) -> Self {
        Self {
            node,
            port: port.into(),
        }
    }
}

/// A connection from an output port to an input port.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Edge {
    pub from: PortRef,
    pub to: PortRef,
}

/// Editor-only data stored with each node. Changing it never triggers
/// re-evaluation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub position: [f32; 2],
}

/// Result state of a node after the last evaluation that reached it.
#[derive(Debug)]
pub enum NodeStatus<'a> {
    /// Never evaluated, or edited since.
    Pending,
    Ready(&'a [Value]),
    Failed(&'a Arc<NodeError>),
    /// An upstream node failed, so this one could not run.
    Blocked,
}

/// Summary of one evaluation pass.
#[derive(Clone, Debug, Default)]
pub struct EvalReport {
    /// Nodes whose `evaluate` actually ran; all others were served from cache.
    pub recomputed: Vec<NodeId>,
    /// Visited nodes that are currently failed (freshly or from cache).
    pub failed: Vec<(NodeId, Arc<NodeError>)>,
    pub blocked: Vec<NodeId>,
    pub cancelled: bool,
}

impl EvalReport {
    pub fn is_ok(&self) -> bool {
        self.failed.is_empty() && self.blocked.is_empty() && !self.cancelled
    }
}

enum State {
    Pending,
    Ready(Vec<Value>),
    Failed(Arc<NodeError>),
    Blocked,
}

struct Entry {
    node: Box<dyn Node>,
    meta: NodeMeta,
    /// Parameters changed since the last evaluation.
    dirty: bool,
    state: State,
    /// Generations of the upstream outputs the cached state was computed
    /// from, in edge order. Any difference means the inputs changed.
    input_gens: Vec<u64>,
    /// Bumped every time this node is recomputed.
    generation: u64,
}

impl Entry {
    fn new(node: Box<dyn Node>, meta: NodeMeta) -> Self {
        Self {
            node,
            meta,
            dirty: true,
            state: State::Pending,
            input_gens: Vec::new(),
            generation: 0,
        }
    }
}

enum Gathered {
    Ready(Inputs, Vec<u64>),
    Blocked,
}

/// A directed acyclic graph of nodes with cached, incremental evaluation.
///
/// Acyclicity is enforced by [`Graph::connect`], so evaluation can never loop.
#[derive(Default)]
pub struct Graph {
    nodes: BTreeMap<NodeId, Entry>,
    edges: Vec<Edge>,
    next_id: u64,
    next_gen: u64,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    // ----- structure ---------------------------------------------------------

    pub fn add<T: Node>(&mut self, node: T) -> NodeId {
        self.add_node(Box::new(node))
    }

    pub fn add_node(&mut self, node: Box<dyn Node>) -> NodeId {
        self.add_node_with_meta(node, NodeMeta::default())
    }

    pub fn add_node_with_meta(&mut self, node: Box<dyn Node>, meta: NodeMeta) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        self.nodes.insert(id, Entry::new(node, meta));
        id
    }

    /// Removes a node and every edge touching it.
    pub fn remove_node(&mut self, id: NodeId) -> Option<Box<dyn Node>> {
        let entry = self.nodes.remove(&id)?;
        self.edges.retain(|e| e.from.node != id && e.to.node != id);
        Some(entry.node)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    pub fn node(&self, id: NodeId) -> Option<&dyn Node> {
        self.nodes.get(&id).map(|e| e.node.as_ref())
    }

    pub fn node_as<T: Node>(&self, id: NodeId) -> Option<&T> {
        self.node(id)?.downcast_ref()
    }

    /// All nodes in id order.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &dyn Node)> {
        self.nodes.iter().map(|(&id, e)| (id, e.node.as_ref()))
    }

    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    pub fn meta(&self, id: NodeId) -> Option<&NodeMeta> {
        self.nodes.get(&id).map(|e| &e.meta)
    }

    pub fn meta_mut(&mut self, id: NodeId) -> Option<&mut NodeMeta> {
        self.nodes.get_mut(&id).map(|e| &mut e.meta)
    }

    /// Edits a node's parameters. The node is re-evaluated on the next pass,
    /// and edges to ports that no longer exist (or changed type) are dropped.
    pub fn edit_node<T: Node, R>(
        &mut self,
        id: NodeId,
        f: impl FnOnce(&mut T) -> R,
    ) -> Result<R, GraphError> {
        let entry = self.nodes.get_mut(&id).ok_or(GraphError::UnknownNode(id))?;
        let node = entry
            .node
            .as_mut()
            .downcast_mut::<T>()
            .ok_or(GraphError::WrongNodeType {
                node: id,
                expected: type_name::<T>(),
            })?;
        let result = f(node);
        entry.dirty = true;
        self.prune_edges(id);
        Ok(result)
    }

    /// Type-erased variant of [`Graph::edit_node`].
    pub fn edit_node_dyn<R>(
        &mut self,
        id: NodeId,
        f: impl FnOnce(&mut dyn Node) -> R,
    ) -> Result<R, GraphError> {
        let entry = self.nodes.get_mut(&id).ok_or(GraphError::UnknownNode(id))?;
        let result = f(entry.node.as_mut());
        entry.dirty = true;
        self.prune_edges(id);
        Ok(result)
    }

    /// Forces a node (and therefore its dependents) to recompute.
    pub fn invalidate(&mut self, id: NodeId) {
        if let Some(entry) = self.nodes.get_mut(&id) {
            entry.dirty = true;
        }
    }

    pub fn invalidate_all(&mut self) {
        self.nodes.values_mut().for_each(|e| e.dirty = true);
    }

    /// Connects `from.from_port` to `to.to_port`.
    ///
    /// A single-input port loses its previous connection; a multi-input port
    /// gains another one. Rejects unknown ports, type mismatches and cycles.
    pub fn connect(
        &mut self,
        from: NodeId,
        from_port: &str,
        to: NodeId,
        to_port: &str,
    ) -> Result<(), GraphError> {
        let src = self.entry(from)?;
        let dst = self.entry(to)?;
        let out_ty = src
            .node
            .outputs()
            .iter()
            .find(|p| p.name == from_port)
            .map(|p| p.ty)
            .ok_or_else(|| GraphError::UnknownPort {
                node: from,
                port: from_port.to_owned(),
                dir: "output",
            })?;
        let (in_ty, multi) = dst
            .node
            .inputs()
            .iter()
            .find(|p| p.name == to_port)
            .map(|p| (p.ty, p.multi))
            .ok_or_else(|| GraphError::UnknownPort {
                node: to,
                port: to_port.to_owned(),
                dir: "input",
            })?;
        if out_ty != in_ty {
            return Err(GraphError::TypeMismatch {
                from: out_ty,
                to: in_ty,
            });
        }
        if from == to || self.reaches(to, from) {
            return Err(GraphError::WouldCycle);
        }

        let edge = Edge {
            from: PortRef::new(from, from_port),
            to: PortRef::new(to, to_port),
        };
        if self.edges.contains(&edge) {
            return Ok(());
        }
        if !multi {
            self.edges.retain(|e| e.to != edge.to);
        }
        self.edges.push(edge);
        self.invalidate(to);
        Ok(())
    }

    /// Removes every connection into `to.to_port`; returns how many.
    pub fn disconnect(&mut self, to: NodeId, to_port: &str) -> usize {
        let before = self.edges.len();
        self.edges
            .retain(|e| !(e.to.node == to && e.to.port == to_port));
        let removed = before - self.edges.len();
        if removed > 0 {
            self.invalidate(to);
        }
        removed
    }

    pub fn remove_edge(&mut self, edge: &Edge) -> bool {
        let Some(pos) = self.edges.iter().position(|e| e == edge) else {
            return false;
        };
        self.edges.remove(pos);
        self.invalidate(edge.to.node);
        true
    }

    fn entry(&self, id: NodeId) -> Result<&Entry, GraphError> {
        self.nodes.get(&id).ok_or(GraphError::UnknownNode(id))
    }

    /// Is `goal` downstream of (or equal to) `start`?
    fn reaches(&self, start: NodeId, goal: NodeId) -> bool {
        let mut seen = HashSet::new();
        let mut stack = vec![start];
        while let Some(id) = stack.pop() {
            if id == goal {
                return true;
            }
            if seen.insert(id) {
                stack.extend(
                    self.edges
                        .iter()
                        .filter(|e| e.from.node == id)
                        .map(|e| e.to.node),
                );
            }
        }
        false
    }

    /// Drops edges of `id` whose ports disappeared or changed type.
    fn prune_edges(&mut self, id: NodeId) {
        let nodes = &self.nodes;
        let port_ty = |r: &PortRef, output: bool| {
            let node = &nodes.get(&r.node)?.node;
            let ports = if output {
                node.outputs()
            } else {
                node.inputs()
            };
            ports.iter().find(|p| p.name == r.port).map(|p| p.ty)
        };
        let mut orphaned = Vec::new();
        self.edges.retain(|e| {
            if e.from.node != id && e.to.node != id {
                return true;
            }
            let valid = matches!(
                (port_ty(&e.from, true), port_ty(&e.to, false)),
                (Some(a), Some(b)) if a == b
            );
            if !valid {
                orphaned.push(e.to.node);
            }
            valid
        });
        for node in orphaned {
            self.invalidate(node);
        }
    }

    // ----- evaluation --------------------------------------------------------

    /// Brings every node up to date.
    pub fn evaluate(&mut self, ctx: &EvalContext) -> EvalReport {
        self.run(ctx, None)
    }

    /// Brings only `targets` and their ancestors up to date — e.g. just what a
    /// preview viewport is showing.
    pub fn evaluate_for(&mut self, ctx: &EvalContext, targets: &[NodeId]) -> EvalReport {
        self.run(ctx, Some(targets))
    }

    fn run(&mut self, ctx: &EvalContext, targets: Option<&[NodeId]>) -> EvalReport {
        let needed = targets.map(|t| self.ancestors(t));
        let mut report = EvalReport::default();
        for id in self.topo_order() {
            if needed.as_ref().is_some_and(|n| !n.contains(&id)) {
                continue;
            }
            if ctx.is_cancelled() {
                report.cancelled = true;
                break;
            }
            self.eval_node(id, ctx, &mut report);
        }
        report
    }

    fn eval_node(&mut self, id: NodeId, ctx: &EvalContext, report: &mut EvalReport) {
        let entry = &self.nodes[&id];
        let (inputs, gens) = match self.gather_inputs(id, entry) {
            Gathered::Ready(inputs, gens) => (inputs, gens),
            Gathered::Blocked => {
                let entry = self.nodes.get_mut(&id).expect("node exists");
                entry.state = State::Blocked;
                entry.input_gens.clear();
                report.blocked.push(id);
                return;
            }
        };

        let up_to_date = !entry.dirty
            && entry.input_gens == gens
            && matches!(entry.state, State::Ready(_) | State::Failed(_))
            && !entry.node.needs_refresh(ctx);
        if !up_to_date {
            let result = run_node(entry.node.as_ref(), &inputs, ctx);
            self.next_gen += 1;
            let entry = self.nodes.get_mut(&id).expect("node exists");
            entry.dirty = false;
            entry.input_gens = gens;
            entry.generation = self.next_gen;
            entry.state = match result {
                Ok(values) => State::Ready(values),
                Err(err) => State::Failed(Arc::new(err)),
            };
            report.recomputed.push(id);
        }
        if let State::Failed(err) = &self.nodes[&id].state {
            report.failed.push((id, Arc::clone(err)));
        }
    }

    fn gather_inputs(&self, id: NodeId, entry: &Entry) -> Gathered {
        let mut inputs = Inputs::new();
        let mut gens = Vec::new();
        for spec in entry.node.inputs().iter() {
            let mut values = Vec::new();
            for edge in self
                .edges
                .iter()
                .filter(|e| e.to.node == id && e.to.port == spec.name)
            {
                let Some(src) = self.nodes.get(&edge.from.node) else {
                    return Gathered::Blocked;
                };
                let State::Ready(outputs) = &src.state else {
                    return Gathered::Blocked;
                };
                let value = src
                    .node
                    .outputs()
                    .iter()
                    .position(|p| p.name == edge.from.port)
                    .and_then(|i| outputs.get(i));
                let Some(value) = value else {
                    return Gathered::Blocked;
                };
                values.push(value.clone());
                gens.push(src.generation);
            }
            inputs.insert_port(spec.name.to_string(), values);
        }
        Gathered::Ready(inputs, gens)
    }

    /// Kahn's algorithm; ties broken by node id so evaluation is deterministic.
    fn topo_order(&self) -> Vec<NodeId> {
        let mut indegree: BTreeMap<NodeId, usize> = self.nodes.keys().map(|&id| (id, 0)).collect();
        for edge in &self.edges {
            *indegree
                .get_mut(&edge.to.node)
                .expect("edge endpoints exist") += 1;
        }
        let mut queue: VecDeque<NodeId> = indegree
            .iter()
            .filter(|&(_, &d)| d == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(id) = queue.pop_front() {
            order.push(id);
            for edge in self.edges.iter().filter(|e| e.from.node == id) {
                let d = indegree
                    .get_mut(&edge.to.node)
                    .expect("edge endpoints exist");
                *d -= 1;
                if *d == 0 {
                    queue.push_back(edge.to.node);
                }
            }
        }
        debug_assert_eq!(order.len(), self.nodes.len(), "graph contains a cycle");
        order
    }

    fn ancestors(&self, targets: &[NodeId]) -> HashSet<NodeId> {
        let mut seen = HashSet::new();
        let mut stack = targets.to_vec();
        while let Some(id) = stack.pop() {
            if seen.insert(id) {
                stack.extend(
                    self.edges
                        .iter()
                        .filter(|e| e.to.node == id)
                        .map(|e| e.from.node),
                );
            }
        }
        seen
    }

    // ----- results -----------------------------------------------------------

    pub fn status(&self, id: NodeId) -> Option<NodeStatus<'_>> {
        Some(match &self.nodes.get(&id)?.state {
            State::Pending => NodeStatus::Pending,
            State::Ready(values) => NodeStatus::Ready(values),
            State::Failed(err) => NodeStatus::Failed(err),
            State::Blocked => NodeStatus::Blocked,
        })
    }

    /// All outputs of a node, in port order, if it evaluated successfully.
    pub fn outputs(&self, id: NodeId) -> Option<&[Value]> {
        match &self.nodes.get(&id)?.state {
            State::Ready(values) => Some(values),
            _ => None,
        }
    }

    pub fn output(&self, id: NodeId, port: &str) -> Option<&Value> {
        let entry = self.nodes.get(&id)?;
        let index = entry.node.outputs().iter().position(|p| p.name == port)?;
        match &entry.state {
            State::Ready(values) => values.get(index),
            _ => None,
        }
    }

    pub fn failures(&self) -> impl Iterator<Item = (NodeId, &Arc<NodeError>)> {
        self.nodes.iter().filter_map(|(&id, e)| match &e.state {
            State::Failed(err) => Some((id, err)),
            _ => None,
        })
    }

    // ----- persistence -------------------------------------------------------

    pub fn to_doc(&self) -> Result<GraphDoc, GraphError> {
        let nodes = self
            .nodes
            .iter()
            .map(|(&id, e)| {
                let kind = e.node.kind();
                let params = e.node.save_params().map_err(|source| GraphError::Params {
                    kind: kind.to_owned(),
                    source,
                })?;
                Ok(NodeDoc {
                    id,
                    kind: kind.to_owned(),
                    params,
                    meta: e.meta.clone(),
                })
            })
            .collect::<Result<_, GraphError>>()?;
        Ok(GraphDoc {
            version: GRAPH_FORMAT_VERSION,
            nodes,
            edges: self.edges.clone(),
        })
    }

    /// Rebuilds a graph, validating every edge exactly as [`Graph::connect`]
    /// would, so a hand-edited file cannot smuggle in a cycle or bad types.
    pub fn from_doc(doc: GraphDoc, registry: &NodeRegistry) -> Result<Self, GraphError> {
        if doc.version > GRAPH_FORMAT_VERSION {
            return Err(GraphError::UnsupportedVersion {
                found: doc.version,
                supported: GRAPH_FORMAT_VERSION,
            });
        }
        let mut graph = Self::new();
        for nd in doc.nodes {
            if graph.nodes.contains_key(&nd.id) {
                return Err(GraphError::DuplicateId(nd.id));
            }
            let node = registry.deserialize(&nd.kind, nd.params)?;
            graph.nodes.insert(nd.id, Entry::new(node, nd.meta));
            graph.next_id = graph.next_id.max(nd.id.0.saturating_add(1));
        }
        for e in doc.edges {
            graph.connect(e.from.node, &e.from.port, e.to.node, &e.to.port)?;
        }
        Ok(graph)
    }

    pub fn to_json(&self) -> Result<String, GraphError> {
        Ok(serde_json::to_string_pretty(&self.to_doc()?)?)
    }

    pub fn from_json(json: &str, registry: &NodeRegistry) -> Result<Self, GraphError> {
        Self::from_doc(serde_json::from_str(json)?, registry)
    }

    /// Saves atomically (write to a temporary sibling, then rename) so a crash
    /// mid-save never corrupts an existing project file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), GraphError> {
        let path = path.as_ref();
        let json = self.to_json()?;
        let file_name = path.file_name().unwrap_or_default().to_string_lossy();
        let tmp = path.with_file_name(format!(".{file_name}.tmp"));
        let io_err = |source| GraphError::Io {
            path: path.to_owned(),
            source,
        };
        fs::write(&tmp, json).map_err(io_err)?;
        fs::rename(&tmp, path).map_err(io_err)
    }

    pub fn load(path: impl AsRef<Path>, registry: &NodeRegistry) -> Result<Self, GraphError> {
        let path = path.as_ref();
        let json = fs::read_to_string(path).map_err(|source| GraphError::Io {
            path: path.to_owned(),
            source,
        })?;
        Self::from_json(&json, registry)
    }
}

/// Evaluates one node behind the safety net: checks required inputs, turns
/// panics into errors and verifies the outputs match the declared ports.
fn run_node(node: &dyn Node, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
    for spec in node.inputs().iter() {
        if spec.required && inputs.all(&spec.name).is_empty() {
            return Err(NodeError::MissingInput(spec.name.to_string()));
        }
    }
    let outputs = panic::catch_unwind(AssertUnwindSafe(|| node.evaluate(inputs, ctx)))
        .unwrap_or_else(|payload| Err(NodeError::Panicked(panic_message(payload.as_ref()))))?;

    let specs = node.outputs();
    if outputs.len() != specs.len() {
        return Err(NodeError::BadOutput(format!(
            "declared {} outputs, returned {}",
            specs.len(),
            outputs.len()
        )));
    }
    for (value, spec) in outputs.iter().zip(specs.iter()) {
        if value.port_type() != spec.ty {
            return Err(NodeError::BadOutput(format!(
                "output `{}` should be {}, got {}",
                spec.name,
                spec.ty,
                value.port_type()
            )));
        }
    }
    Ok(outputs)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".to_owned())
}

impl fmt::Debug for Graph {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Graph")
            .field(
                "nodes",
                &self
                    .nodes
                    .iter()
                    .map(|(id, e)| (id, e.node.kind()))
                    .collect::<Vec<_>>(),
            )
            .field("edges", &self.edges)
            .finish()
    }
}

/// Serialisable form of a [`Graph`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphDoc {
    #[serde(default = "current_version")]
    pub version: u32,
    #[serde(default)]
    pub nodes: Vec<NodeDoc>,
    #[serde(default)]
    pub edges: Vec<Edge>,
}

fn current_version() -> u32 {
    GRAPH_FORMAT_VERSION
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeDoc {
    pub id: NodeId,
    pub kind: String,
    #[serde(default)]
    pub params: serde_json::Value,
    #[serde(default)]
    pub meta: NodeMeta,
}

//! Nesting: a whole graph packaged as a single node.

use std::{
    borrow::Cow,
    collections::HashMap,
    fmt,
    sync::{Mutex, MutexGuard, PoisonError},
};

use serde::{Deserialize, Serialize, Serializer, ser::Error as _};

use crate::{
    EvalContext, Graph, GraphDoc, GraphError, Inputs, Node, NodeDescriptor, NodeError, NodeId,
    NodeRegistry, NodeStatus, NodeType, PortSpec, PortType, Value,
};

/// Port name used by both group proxy nodes.
pub const GROUP_VALUE_PORT: &str = "value";

/// Inside a group: emits the value bound to the group's input port `name`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupInput {
    pub name: String,
    pub ty: PortType,
}

impl Default for GroupInput {
    fn default() -> Self {
        Self {
            name: "input".into(),
            ty: PortType::Image,
        }
    }
}

impl GroupInput {
    pub fn new(name: impl Into<String>, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

impl Node for GroupInput {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Owned(vec![PortSpec::new(GROUP_VALUE_PORT, self.ty)])
    }

    fn evaluate(&self, _: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let value = ctx
            .group_input(&self.name)
            .ok_or_else(|| NodeError::MissingInput(self.name.clone()))?;
        Ok(vec![value.clone()])
    }
}

impl NodeType for GroupInput {
    const KIND: &'static str = "group_input";
    const DISPLAY_NAME: &'static str = "Group Input";
    const CATEGORY: &'static str = "Group";
}

/// Inside a group: whatever reaches this node becomes the group's output
/// port `name`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupOutput {
    pub name: String,
    pub ty: PortType,
}

impl Default for GroupOutput {
    fn default() -> Self {
        Self {
            name: "output".into(),
            ty: PortType::Image,
        }
    }
}

impl GroupOutput {
    pub fn new(name: impl Into<String>, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
        }
    }
}

impl Node for GroupOutput {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Owned(vec![PortSpec::new(GROUP_VALUE_PORT, self.ty)])
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        self.inputs()
    }

    fn evaluate(&self, inputs: &Inputs, _: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let value = inputs
            .get(GROUP_VALUE_PORT)
            .ok_or_else(|| NodeError::MissingInput(GROUP_VALUE_PORT.into()))?;
        Ok(vec![value.clone()])
    }
}

impl NodeType for GroupOutput {
    const KIND: &'static str = "group_output";
    const DISPLAY_NAME: &'static str = "Group Output";
    const CATEGORY: &'static str = "Group";
}

/// A node that runs an inner graph. Its ports mirror the inner graph's
/// [`GroupInput`] and [`GroupOutput`] nodes (first one wins on duplicate
/// names). The inner graph keeps its own cache, so re-running a group after
/// one input changed only recomputes the inner nodes that depend on it.
pub struct GroupNode {
    inner: Mutex<Graph>,
    inputs: Vec<PortSpec>,
    outputs: Vec<PortSpec>,
    input_ids: Vec<NodeId>,
    output_ids: Vec<NodeId>,
}

impl GroupNode {
    pub const KIND: &'static str = "group";

    pub fn new(graph: Graph) -> Self {
        let mut group = Self {
            inner: Mutex::new(graph),
            inputs: Vec::new(),
            outputs: Vec::new(),
            input_ids: Vec::new(),
            output_ids: Vec::new(),
        };
        group.refresh_ports();
        group
    }

    /// Edits the inner graph and re-derives the group's ports. Call through
    /// [`Graph::edit_node`] on the outer graph so the group is re-evaluated.
    pub fn edit_inner<R>(&mut self, f: impl FnOnce(&mut Graph) -> R) -> R {
        let result = f(self.inner.get_mut().unwrap_or_else(PoisonError::into_inner));
        self.refresh_ports();
        result
    }

    pub fn with_inner<R>(&self, f: impl FnOnce(&Graph) -> R) -> R {
        f(&self.lock())
    }

    pub fn into_inner(self) -> Graph {
        self.inner
            .into_inner()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self) -> MutexGuard<'_, Graph> {
        // A poisoned lock only means a node panicked mid-evaluation; the graph
        // structure itself is still consistent.
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn refresh_ports(&mut self) {
        let graph = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner);
        self.inputs.clear();
        self.outputs.clear();
        self.input_ids.clear();
        self.output_ids.clear();
        for (id, node) in graph.nodes() {
            if let Some(proxy) = node.downcast_ref::<GroupInput>() {
                if !self.inputs.iter().any(|p| p.name == proxy.name) {
                    self.inputs
                        .push(PortSpec::new(proxy.name.clone(), proxy.ty));
                    self.input_ids.push(id);
                }
            } else if let Some(proxy) = node.downcast_ref::<GroupOutput>()
                && !self.outputs.iter().any(|p| p.name == proxy.name)
            {
                self.outputs
                    .push(PortSpec::new(proxy.name.clone(), proxy.ty));
                self.output_ids.push(id);
            }
        }
    }

    pub(crate) fn descriptor() -> NodeDescriptor {
        NodeDescriptor {
            kind: Self::KIND,
            display_name: "Group",
            category: "Group",
            create: || Box::new(GroupNode::default()),
            deserialize: deserialize_group,
        }
    }
}

fn deserialize_group(
    params: serde_json::Value,
    registry: &NodeRegistry,
) -> Result<Box<dyn Node>, GraphError> {
    let doc: GraphDoc = serde_json::from_value(params).map_err(|source| GraphError::Params {
        kind: GroupNode::KIND.to_owned(),
        source,
    })?;
    Ok(Box::new(GroupNode::new(Graph::from_doc(doc, registry)?)))
}

impl Default for GroupNode {
    fn default() -> Self {
        Self::new(Graph::new())
    }
}

impl Serialize for GroupNode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.lock()
            .to_doc()
            .map_err(S::Error::custom)?
            .serialize(serializer)
    }
}

impl fmt::Debug for GroupNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GroupNode")
            .field("inputs", &self.inputs)
            .field("outputs", &self.outputs)
            .field("inner", &*self.lock())
            .finish()
    }
}

impl Node for GroupNode {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(&self.inputs)
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(&self.outputs)
    }

    fn evaluate(&self, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let bound: HashMap<String, Value> = self
            .inputs
            .iter()
            .filter_map(|p| Some((p.name.to_string(), inputs.get(&p.name)?.clone())))
            .collect();

        let mut graph = self.lock();
        // The outer graph only calls us when our inputs or the inner graph
        // changed; re-run the proxies and let inner caching handle the rest.
        for &id in &self.input_ids {
            graph.invalidate(id);
        }
        let report = graph.evaluate_for(&ctx.for_group(bound), &self.output_ids);
        if report.cancelled {
            return Err(NodeError::Cancelled);
        }

        self.output_ids
            .iter()
            .map(|&id| match graph.status(id) {
                Some(NodeStatus::Ready(values)) => values
                    .first()
                    .cloned()
                    .ok_or_else(|| NodeError::BadOutput("group output proxy is empty".into())),
                _ => Err(match report.failed.first() {
                    Some((node, err)) => NodeError::Subgraph {
                        node: *node,
                        source: err.clone(),
                    },
                    None => NodeError::BadOutput(format!("group output {id} was not produced")),
                }),
            })
            .collect()
    }

    fn needs_refresh(&self, ctx: &EvalContext) -> bool {
        self.lock().nodes().any(|(_, node)| node.needs_refresh(ctx))
    }
}

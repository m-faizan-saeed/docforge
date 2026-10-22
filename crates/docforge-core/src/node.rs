use std::{
    any::Any,
    borrow::Cow,
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use serde::{Serialize, de::DeserializeOwned};

use crate::{NodeError, PortSpec, RasterImage, Value};

/// Serialises a node's parameters for saving. Implemented automatically for
/// every `Serialize` type, so nodes normally just `#[derive(Serialize)]`.
pub trait SaveParams {
    fn save_params(&self) -> Result<serde_json::Value, serde_json::Error>;
}

impl<T: Serialize> SaveParams for T {
    fn save_params(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }
}

/// A tool in the graph.
///
/// The node itself holds only its *parameters*; the graph owns connections
/// and cached results. `evaluate` takes `&self`, so a node is a pure function
/// of its parameters and inputs (plus whatever it reads from the filesystem),
/// which is what makes caching and future parallel evaluation sound.
pub trait Node: Any + Send + Sync + SaveParams {
    /// Stable identifier written to saved graphs, e.g. `"input_image"`.
    fn kind(&self) -> &'static str;

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(&[])
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(&[])
    }

    /// Produces one value per entry of [`Node::outputs`], in the same order.
    fn evaluate(&self, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError>;

    /// Lets a node invalidate its own cached result when something outside
    /// the graph changed, e.g. its source file was modified on disk.
    fn needs_refresh(&self, _ctx: &EvalContext) -> bool {
        false
    }
}

impl dyn Node {
    pub fn downcast_ref<T: Node>(&self) -> Option<&T> {
        (self as &dyn Any).downcast_ref()
    }

    pub fn downcast_mut<T: Node>(&mut self) -> Option<&mut T> {
        (self as &mut dyn Any).downcast_mut()
    }
}

/// A node type that can be listed in a [`crate::NodeRegistry`]: created with
/// default parameters from an "add node" menu and restored from a saved graph.
pub trait NodeType: Node + Default + Serialize + DeserializeOwned {
    const KIND: &'static str;
    const DISPLAY_NAME: &'static str;
    const CATEGORY: &'static str = "General";
}

/// Values delivered to a node's input ports for one evaluation.
#[derive(Clone, Debug, Default)]
pub struct Inputs {
    ports: Vec<(String, Vec<Value>)>,
}

impl Inputs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder for calling nodes directly, e.g. in tests or batch tools.
    pub fn with(mut self, port: impl Into<String>, value: impl Into<Value>) -> Self {
        self.push(port, value.into());
        self
    }

    /// Appends a value to `port` (multi-input ports receive several).
    pub fn push(&mut self, port: impl Into<String>, value: Value) {
        let port = port.into();
        match self.ports.iter_mut().find(|(name, _)| *name == port) {
            Some((_, values)) => values.push(value),
            None => self.ports.push((port, vec![value])),
        }
    }

    pub(crate) fn insert_port(&mut self, port: String, values: Vec<Value>) {
        self.ports.push((port, values));
    }

    /// Every value connected to `port`, in connection order.
    pub fn all(&self, port: &str) -> &[Value] {
        self.ports
            .iter()
            .find(|(name, _)| name == port)
            .map_or(&[], |(_, values)| values.as_slice())
    }

    pub fn get(&self, port: &str) -> Option<&Value> {
        self.all(port).first()
    }

    pub fn image(&self, port: &str) -> Result<&Arc<RasterImage>, NodeError> {
        let value = self
            .get(port)
            .ok_or_else(|| NodeError::MissingInput(port.to_owned()))?;
        expect_image(port, value)
    }

    pub fn opt_image(&self, port: &str) -> Result<Option<&Arc<RasterImage>>, NodeError> {
        self.get(port).map(|v| expect_image(port, v)).transpose()
    }

    pub fn images(&self, port: &str) -> Result<Vec<&Arc<RasterImage>>, NodeError> {
        self.all(port)
            .iter()
            .map(|v| expect_image(port, v))
            .collect()
    }
}

fn expect_image<'a>(port: &str, value: &'a Value) -> Result<&'a Arc<RasterImage>, NodeError> {
    value.as_image().ok_or_else(|| NodeError::InputType {
        port: port.to_owned(),
        expected: crate::PortType::Image,
        actual: value.port_type(),
    })
}

/// Environment shared by all nodes during one evaluation pass.
#[derive(Clone, Debug, Default)]
pub struct EvalContext {
    base_dir: Option<PathBuf>,
    group_inputs: HashMap<String, Value>,
    cancel: Option<Arc<AtomicBool>>,
}

impl EvalContext {
    pub fn new() -> Self {
        Self::default()
    }

    /// Directory relative file paths are resolved against — normally the
    /// directory of the saved graph, so projects can be moved as a folder.
    pub fn with_base_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.base_dir = Some(dir.into());
        self
    }

    /// Flag checked between nodes; set it from another thread to abort a pass.
    pub fn with_cancel_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel = Some(flag);
        self
    }

    pub fn base_dir(&self) -> Option<&Path> {
        self.base_dir.as_deref()
    }

    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        match &self.base_dir {
            Some(base) if path.is_relative() => base.join(path),
            _ => path.to_path_buf(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
    }

    /// Value bound to a [`crate::GroupInput`] of the enclosing group.
    pub fn group_input(&self, name: &str) -> Option<&Value> {
        self.group_inputs.get(name)
    }

    /// Context for evaluating a nested graph with the given bound inputs.
    pub fn for_group(&self, inputs: HashMap<String, Value>) -> Self {
        Self {
            base_dir: self.base_dir.clone(),
            group_inputs: inputs,
            cancel: self.cancel.clone(),
        }
    }
}

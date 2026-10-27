use std::collections::BTreeMap;

use crate::{GraphDoc, GraphError, GroupInput, GroupNode, GroupOutput, Node, NodeType};

pub type CreateFn = fn() -> Box<dyn Node>;
pub type DeserializeFn = fn(serde_json::Value, &NodeRegistry) -> Result<Box<dyn Node>, GraphError>;

/// Everything the editor needs to know about one node kind.
#[derive(Clone, Debug)]
pub struct NodeDescriptor {
    pub kind: &'static str,
    pub display_name: &'static str,
    pub category: &'static str,
    /// Builds the node with default parameters.
    pub create: CreateFn,
    /// Restores the node from saved parameters. Receives the registry so
    /// container nodes can restore their children.
    pub deserialize: DeserializeFn,
}

/// Maps node kinds to constructors. Needed to load saved graphs and to
/// populate the editor's "add node" menu.
#[derive(Clone, Debug, Default)]
pub struct NodeRegistry {
    types: BTreeMap<&'static str, NodeDescriptor>,
    migrations: Vec<Migration>,
}

/// Rewrites a saved graph from an older layout into the current one, e.g.
/// when a tool's job is split across two nodes. Must be idempotent: it runs
/// on every load, including graphs that are already up to date.
pub type Migration = fn(&mut GraphDoc);

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry holding the core node kinds (grouping).
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        registry
            .register::<GroupInput>()
            .register::<GroupOutput>()
            .register_descriptor(GroupNode::descriptor());
        registry
    }

    pub fn register<T: NodeType>(&mut self) -> &mut Self {
        self.register_descriptor(NodeDescriptor {
            kind: T::KIND,
            display_name: T::DISPLAY_NAME,
            category: T::CATEGORY,
            create: create_typed::<T>,
            deserialize: deserialize_typed::<T>,
        })
    }

    /// Registers a hand-written descriptor, replacing any with the same kind.
    pub fn register_descriptor(&mut self, descriptor: NodeDescriptor) -> &mut Self {
        self.types.insert(descriptor.kind, descriptor);
        self
    }

    pub fn get(&self, kind: &str) -> Option<&NodeDescriptor> {
        self.types.get(kind)
    }

    /// All descriptors, sorted by kind.
    pub fn iter(&self) -> impl Iterator<Item = &NodeDescriptor> {
        self.types.values()
    }

    pub fn create(&self, kind: &str) -> Result<Box<dyn Node>, GraphError> {
        Ok((self.descriptor(kind)?.create)())
    }

    /// Adds a migration applied by [`crate::Graph::from_doc`] (and so by
    /// `load`/`from_json`) and [`crate::Graph::paste`].
    pub fn register_migration(&mut self, migration: Migration) -> &mut Self {
        self.migrations.push(migration);
        self
    }

    /// Brings `doc` up to date with every registered migration.
    pub fn migrate(&self, doc: &mut GraphDoc) {
        for migration in &self.migrations {
            migration(doc);
        }
    }

    pub fn deserialize(
        &self,
        kind: &str,
        params: serde_json::Value,
    ) -> Result<Box<dyn Node>, GraphError> {
        let descriptor = self.descriptor(kind)?;
        if !params.is_null() {
            return (descriptor.deserialize)(params, self);
        }
        // `null` is what parameterless (unit) nodes save, and also what a
        // hand-edited file that omits params yields; the latter means "all
        // defaults", which struct nodes accept as an empty object.
        (descriptor.deserialize)(serde_json::Value::Null, self).or_else(|_| {
            (descriptor.deserialize)(serde_json::Value::Object(Default::default()), self)
        })
    }

    fn descriptor(&self, kind: &str) -> Result<&NodeDescriptor, GraphError> {
        self.get(kind)
            .ok_or_else(|| GraphError::UnknownKind(kind.to_owned()))
    }
}

fn create_typed<T: NodeType>() -> Box<dyn Node> {
    Box::new(T::default())
}

fn deserialize_typed<T: NodeType>(
    params: serde_json::Value,
    _: &NodeRegistry,
) -> Result<Box<dyn Node>, GraphError> {
    serde_json::from_value::<T>(params)
        .map(|node| Box::new(node) as Box<dyn Node>)
        .map_err(|source| GraphError::Params {
            kind: T::KIND.to_owned(),
            source,
        })
}

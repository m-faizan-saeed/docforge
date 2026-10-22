use std::{path::PathBuf, sync::Arc};

use crate::{graph::NodeId, value::PortType};

/// Failure raised while a single node evaluates.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NodeError {
    #[error("required input `{0}` is not connected")]
    MissingInput(String),

    #[error("input `{port}` carries {actual}, expected {expected}")]
    InputType {
        port: String,
        expected: PortType,
        actual: PortType,
    },

    #[error("invalid parameter `{name}`: {reason}")]
    InvalidParam { name: &'static str, reason: String },

    #[error("invalid pixel buffer: {0}")]
    InvalidBuffer(String),

    #[error("cannot access `{}`: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("cannot decode `{}`: {source}", .path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },

    #[error("cannot encode `{}`: {source}", .path.display())]
    Encode {
        path: PathBuf,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("node produced invalid outputs: {0}")]
    BadOutput(String),

    #[error("inner node {node} failed: {source}")]
    Subgraph {
        node: NodeId,
        #[source]
        source: Arc<NodeError>,
    },

    #[error("node panicked: {0}")]
    Panicked(String),

    #[error("evaluation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

/// Failure while editing, loading or saving a graph.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GraphError {
    #[error("node {0} does not exist")]
    UnknownNode(NodeId),

    #[error("node {node} has no {dir} port named `{port}`")]
    UnknownPort {
        node: NodeId,
        port: String,
        dir: &'static str,
    },

    #[error("cannot connect a {from} output to a {to} input")]
    TypeMismatch { from: PortType, to: PortType },

    #[error("connection would create a cycle")]
    WouldCycle,

    #[error("node {node} is not a `{expected}`")]
    WrongNodeType {
        node: NodeId,
        expected: &'static str,
    },

    #[error("unknown node kind `{0}` (is the crate providing it registered?)")]
    UnknownKind(String),

    #[error("duplicate node id {0}")]
    DuplicateId(NodeId),

    #[error("unsupported graph format version {found} (this build reads up to {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },

    #[error("invalid parameters for `{kind}`: {source}")]
    Params {
        kind: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("malformed graph document: {0}")]
    Json(#[from] serde_json::Error),

    #[error("cannot access `{}`: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

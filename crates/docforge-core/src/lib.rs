//! Core of the docforge node editor.
//!
//! Everything in docforge is a [`Node`]: a small tool with typed input and
//! output ports. Nodes are wired together in a [`Graph`], which
//!
//! * evaluates them in dependency order, caching every output so that editing
//!   one parameter only recomputes the affected downstream nodes (this is what
//!   makes real-time previews cheap);
//! * isolates failures: an erroring or panicking node is reported, and its
//!   dependents are marked blocked instead of bringing down the editor;
//! * serialises to a versioned JSON document ([`GraphDoc`]) and loads back
//!   through a [`NodeRegistry`] that maps node kinds to constructors.
//!
//! Graphs nest: a [`GroupNode`] wraps a whole inner graph and exposes its
//! [`GroupInput`]/[`GroupOutput`] proxy nodes as its own ports.
//!
//! Images travel between nodes as [`RasterImage`] — a tightly packed RGBA8
//! buffer with optional print resolution — wrapped in an `Arc` so fan-out to
//! several consumers never copies pixels.

mod error;
pub mod files;
mod graph;
mod group;
mod node;
mod raster;
mod registry;
mod value;

pub use error::{GraphError, NodeError};
pub use graph::{
    Edge, EvalReport, GRAPH_FORMAT_VERSION, Graph, GraphDoc, NodeDoc, NodeId, NodeMeta, NodeStatus,
    PortRef,
};
pub use group::{GROUP_VALUE_PORT, GroupInput, GroupNode, GroupOutput};
pub use node::{EvalContext, Inputs, Node, NodeType, SaveParams};
pub use raster::RasterImage;
pub use registry::{CreateFn, DeserializeFn, Migration, NodeDescriptor, NodeRegistry};
pub use value::{PdfDocument, PdfImageInfo, PdfPageInfo, PortSpec, PortType, Value};

/// Re-exported so node crates convert to and from the exact `image` version
/// that [`RasterImage`] was built against.
pub use image;

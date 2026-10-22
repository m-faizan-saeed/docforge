use std::{borrow::Cow, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::RasterImage;

/// The kind of data a port carries. Connections are only allowed between
/// ports of the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Image,
}

impl fmt::Display for PortType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Image => "image",
        })
    }
}

/// A value flowing along an edge. Payloads are reference counted, so passing a
/// value to several consumers (or caching it) never copies the data.
#[derive(Clone, Debug)]
pub enum Value {
    Image(Arc<RasterImage>),
}

impl Value {
    pub fn port_type(&self) -> PortType {
        match self {
            Self::Image(_) => PortType::Image,
        }
    }

    pub fn as_image(&self) -> Option<&Arc<RasterImage>> {
        match self {
            Self::Image(img) => Some(img),
        }
    }

    /// True when both values share the same payload allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Image(a), Self::Image(b)) => Arc::ptr_eq(a, b),
        }
    }
}

impl From<RasterImage> for Value {
    fn from(img: RasterImage) -> Self {
        Self::Image(Arc::new(img))
    }
}

impl From<Arc<RasterImage>> for Value {
    fn from(img: Arc<RasterImage>) -> Self {
        Self::Image(img)
    }
}

/// Declaration of one input or output port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortSpec {
    pub name: Cow<'static, str>,
    pub ty: PortType,
    /// Inputs only: evaluation fails with [`crate::NodeError::MissingInput`]
    /// when a required port has no connection.
    pub required: bool,
    /// Inputs only: accept any number of connections, delivered in the order
    /// they were made. Connecting to a non-multi port replaces its edge.
    pub multi: bool,
}

impl PortSpec {
    pub fn new(name: impl Into<Cow<'static, str>>, ty: PortType) -> Self {
        Self {
            name: name.into(),
            ty,
            required: true,
            multi: false,
        }
    }

    /// Required single image port; usable in `const` port tables.
    pub const fn image(name: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty: PortType::Image,
            required: true,
            multi: false,
        }
    }

    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub const fn multi(mut self) -> Self {
        self.multi = true;
        self
    }
}

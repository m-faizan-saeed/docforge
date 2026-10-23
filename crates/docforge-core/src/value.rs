use std::{borrow::Cow, fmt, sync::Arc};

use serde::{Deserialize, Serialize};

use crate::RasterImage;

/// The kind of data a port carries. Connections are only allowed between
/// ports of the same type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    Image,
    Document,
}

impl fmt::Display for PortType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Image => "image",
            Self::Document => "document",
        })
    }
}

/// A value flowing along an edge. Payloads are reference counted, so passing a
/// value to several consumers (or caching it) never copies the data.
#[derive(Clone, Debug)]
pub enum Value {
    Image(Arc<RasterImage>),
    Document(Arc<PdfDocument>),
}

impl Value {
    pub fn port_type(&self) -> PortType {
        match self {
            Self::Image(_) => PortType::Image,
            Self::Document(_) => PortType::Document,
        }
    }

    pub fn as_image(&self) -> Option<&Arc<RasterImage>> {
        match self {
            Self::Image(img) => Some(img),
            _ => None,
        }
    }

    pub fn as_document(&self) -> Option<&Arc<PdfDocument>> {
        match self {
            Self::Document(doc) => Some(doc),
            _ => None,
        }
    }

    /// True when both values share the same payload allocation.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Image(a), Self::Image(b)) => Arc::ptr_eq(a, b),
            (Self::Document(a), Self::Document(b)) => Arc::ptr_eq(a, b),
            _ => false,
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

impl From<PdfDocument> for Value {
    fn from(doc: PdfDocument) -> Self {
        Self::Document(Arc::new(doc))
    }
}

/// A PDF flowing through the graph — generated or loaded — together with
/// facts about it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfDocument {
    pub bytes: Vec<u8>,
    /// Width and height of each page in millimetres.
    pub pages_mm: Vec<[f32; 2]>,
    /// Human-readable notes about how it was produced, e.g. paper usage.
    pub summary: Vec<String>,
    /// Per-page analysis of a loaded PDF; empty for generated documents.
    pub page_info: Vec<PdfPageInfo>,
}

/// What one page of a loaded PDF contains.
///
/// Rectangles are `[x0, y0, x1, y1]` in PDF points (1/72 inch) in the page's
/// own coordinate system: origin bottom-left, before `/Rotate` is applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PdfPageInfo {
    pub media_box: [f32; 4],
    /// The visible region (CropBox, clipped to the MediaBox).
    pub crop_box: [f32; 4],
    /// Clockwise display rotation in degrees: 0, 90, 180 or 270.
    pub rotation: u16,
    /// The box to treat as the page's artwork: by default the tight bounds of
    /// everything visibly painted. `None` for a blank page.
    pub content_box: Option<[f32; 4]>,
    pub images: Vec<PdfImageInfo>,
}

impl PdfPageInfo {
    /// Width and height of the content box in millimetres.
    pub fn content_size_mm(&self) -> Option<[f32; 2]> {
        self.content_box
            .map(|[x0, y0, x1, y1]| [(x1 - x0) * 25.4 / 72.0, (y1 - y0) * 25.4 / 72.0])
    }
}

/// An image painted on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfImageInfo {
    /// Stable identifier for selecting the image, e.g. `p1 Im0` (page and
    /// resource name where it first appears). The same image object drawn
    /// several times has one key. `None` for inline images.
    pub key: Option<String>,
    /// Resource name such as `Im0`, or `inline` for inline images.
    pub name: String,
    /// Object number and generation; `None` for inline images.
    pub object: Option<(u32, u16)>,
    pub width_px: u32,
    pub height_px: u32,
    /// Bounding box of where it is painted, in page points.
    pub placed: [f32; 4],
    /// Compression filter, e.g. `DCTDecode` (JPEG) or `FlateDecode`.
    pub filter: Option<String>,
    /// How the image's unit square maps onto the page (`[a b c d e f]`),
    /// i.e. its placement, scale, rotation and flips.
    pub matrix: [f32; 6],
}

impl AsRef<[u8]> for PdfDocument {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
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

    /// Required single document port; usable in `const` port tables.
    pub const fn document(name: &'static str) -> Self {
        Self {
            name: Cow::Borrowed(name),
            ty: PortType::Document,
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

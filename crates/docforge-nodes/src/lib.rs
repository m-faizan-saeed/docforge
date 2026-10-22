//! The built-in docforge tools.
//!
//! | Kind           | Tool                                         |
//! |----------------|----------------------------------------------|
//! | `input_image`  | [`InputImage`] — load a file into the graph  |
//! | `output_image` | [`OutputImage`] — save the image to a file   |
//! | `enhance_image`| [`EnhanceImage`] — document effects & overlays |
//! | `print_layout` | [`PrintLayout`] — pack images onto paper as a PDF |

mod dpi;
pub mod enhance;
pub mod input_image;
pub mod output_image;
pub mod print;
mod runtime;

use docforge_core::NodeRegistry;

pub use enhance::{Effect, EnhanceImage, Step};
pub use input_image::InputImage;
pub use output_image::{IfExists, OutputImage, PngCompression, SaveFormat};
pub use print::PrintLayout;

/// Adds every tool from this crate to `registry`.
pub fn register_all(registry: &mut NodeRegistry) {
    registry
        .register::<InputImage>()
        .register::<OutputImage>()
        .register::<EnhanceImage>()
        .register::<PrintLayout>();
}

/// A registry with the core node kinds plus every tool from this crate.
pub fn registry() -> NodeRegistry {
    let mut registry = NodeRegistry::with_builtins();
    register_all(&mut registry);
    registry
}

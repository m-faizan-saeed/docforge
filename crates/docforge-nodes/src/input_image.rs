//! Tool #1 — Input Image: the entry point that loads a file into the graph.

use std::{borrow::Cow, fs, io::Cursor, path::PathBuf};

use docforge_core::{EvalContext, Inputs, Node, NodeError, NodeType, PortSpec, RasterImage, Value};
use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, metadata::Orientation};
use serde::{Deserialize, Serialize};

use crate::{
    dpi::sniff_dpi,
    runtime::{FileStamp, RuntimeState},
};

/// Name of the single output port.
pub const OUTPUT: &str = "image";

const OUTPUTS: &[PortSpec] = &[PortSpec::image(OUTPUT)];

/// Loads an image file (PNG, JPEG, WebP, TIFF, BMP, GIF) as a [`RasterImage`].
///
/// * Phone photos are rotated upright using their EXIF orientation.
/// * The print resolution is read from the file header (PNG `pHYs`, JPEG
///   JFIF) unless overridden with `dpi`.
/// * If the file changes on disk the node reloads on the next evaluation, so
///   an image edited in another program updates the preview automatically.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputImage {
    /// Source file; relative paths resolve against the graph's base directory.
    pub path: PathBuf,
    /// Rotate/flip according to the EXIF orientation tag.
    pub apply_orientation: bool,
    /// Print resolution to assume, overriding whatever the file declares.
    pub dpi: Option<f32>,
    /// File state seen by the last load attempt: `None` before the first
    /// attempt, `Some(None)` if the file did not exist.
    #[serde(skip)]
    observed: RuntimeState<Option<Option<FileStamp>>>,
}

impl Default for InputImage {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            apply_orientation: true,
            dpi: None,
            observed: RuntimeState::default(),
        }
    }
}

impl InputImage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    pub fn with_dpi(mut self, dpi: f32) -> Self {
        self.dpi = Some(dpi);
        self
    }

    /// Loads the image. Usable on its own, outside any graph.
    pub fn load(&self, ctx: &EvalContext) -> Result<RasterImage, NodeError> {
        if self.path.as_os_str().is_empty() {
            return Err(NodeError::InvalidParam {
                name: "path",
                reason: "no file selected".into(),
            });
        }
        if let Some(dpi) = self.dpi
            && !(dpi.is_finite() && dpi > 0.0)
        {
            return Err(NodeError::InvalidParam {
                name: "dpi",
                reason: format!("must be a positive number, got {dpi}"),
            });
        }

        let path = ctx.resolve_path(&self.path);
        // Record what we are about to read *before* reading it, whether or not
        // loading succeeds: a later change (including the file appearing or
        // being fixed) then triggers a reload, but a persistently broken file
        // is not re-decoded on every pass.
        self.observed.set(Some(FileStamp::read(&path)));

        let bytes = fs::read(&path).map_err(|source| NodeError::Io {
            path: path.clone(),
            source,
        })?;
        let mut image = decode(&bytes, self.apply_orientation)
            .map_err(|source| NodeError::Decode { path, source })?;
        image.set_dpi(self.dpi.or_else(|| sniff_dpi(&bytes)));
        Ok(image)
    }
}

/// Decodes from memory so the file is read once for both pixels and headers.
fn decode(bytes: &[u8], apply_orientation: bool) -> Result<RasterImage, ImageError> {
    let mut decoder = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()?
        .into_decoder()?;
    // A malformed EXIF block should not make an otherwise fine image unusable.
    let orientation = if apply_orientation {
        decoder.orientation().unwrap_or(Orientation::NoTransforms)
    } else {
        Orientation::NoTransforms
    };
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);
    Ok(RasterImage::from(image))
}

impl Node for InputImage {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(OUTPUTS)
    }

    fn evaluate(&self, _: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        Ok(vec![self.load(ctx)?.into()])
    }

    fn needs_refresh(&self, ctx: &EvalContext) -> bool {
        self.observed
            .get()
            .is_some_and(|seen| seen != FileStamp::read(&ctx.resolve_path(&self.path)))
    }
}

impl NodeType for InputImage {
    const KIND: &'static str = "input_image";
    const DISPLAY_NAME: &'static str = "Input Image";
    const CATEGORY: &'static str = "Input / Output";
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use docforge_core::{Graph, NodeStatus};
    use image::{Rgba, RgbaImage};

    use super::*;

    fn write_png(path: &Path, w: u32, h: u32) -> RgbaImage {
        let img = RgbaImage::from_fn(w, h, |x, y| Rgba([x as u8, y as u8, 7, 255]));
        img.save(path).unwrap();
        img
    }

    #[test]
    fn loads_pixels_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.png");
        let expected = write_png(&path, 5, 3);

        let img = InputImage::new(&path).load(&EvalContext::new()).unwrap();
        assert_eq!(img.dimensions(), (5, 3));
        assert_eq!(img.as_raw(), expected.as_raw().as_slice());
    }

    #[test]
    fn resolves_relative_paths_and_overrides_dpi() {
        let dir = tempfile::tempdir().unwrap();
        write_png(&dir.path().join("card.png"), 2, 2);
        let ctx = EvalContext::new().with_base_dir(dir.path());

        let img = InputImage::new("card.png")
            .with_dpi(300.0)
            .load(&ctx)
            .unwrap();
        assert_eq!(img.dpi(), Some(300.0));
    }

    #[test]
    fn reads_jpeg_through_format_sniffing() {
        let dir = tempfile::tempdir().unwrap();
        // Wrong extension on purpose: the format is detected from content.
        let path = dir.path().join("photo.png");
        DynamicImage::ImageRgb8(image::RgbImage::new(8, 4))
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .unwrap();

        let img = InputImage::new(&path).load(&EvalContext::new()).unwrap();
        assert_eq!(img.dimensions(), (8, 4));
    }

    #[test]
    fn reports_bad_input() {
        let ctx = EvalContext::new();
        assert!(matches!(
            InputImage::default().load(&ctx),
            Err(NodeError::InvalidParam { name: "path", .. })
        ));
        assert!(matches!(
            InputImage::new("/definitely/not/here.png").load(&ctx),
            Err(NodeError::Io { .. })
        ));
        assert!(matches!(
            InputImage::new("x.png").with_dpi(0.0).load(&ctx),
            Err(NodeError::InvalidParam { name: "dpi", .. })
        ));

        let dir = tempfile::tempdir().unwrap();
        let junk = dir.path().join("junk.png");
        fs::write(&junk, b"not an image").unwrap();
        assert!(matches!(
            InputImage::new(&junk).load(&ctx),
            Err(NodeError::Decode { .. })
        ));
    }

    #[test]
    fn reloads_when_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("live.png");
        write_png(&path, 4, 4);

        let mut graph = Graph::new();
        let id = graph.add(InputImage::new(&path));
        let ctx = EvalContext::new();
        assert_eq!(graph.evaluate(&ctx).recomputed, [id]);
        assert!(graph.evaluate(&ctx).recomputed.is_empty());

        write_png(&path, 6, 2);
        assert_eq!(graph.evaluate(&ctx).recomputed, [id]);
        let img = graph.output(id, OUTPUT).unwrap().as_image().unwrap();
        assert_eq!(img.dimensions(), (6, 2));
        assert!(graph.evaluate(&ctx).recomputed.is_empty());

        // Deleting the file surfaces an error; restoring it recovers.
        fs::remove_file(&path).unwrap();
        graph.evaluate(&ctx);
        assert!(matches!(graph.status(id), Some(NodeStatus::Failed(_))));
        write_png(&path, 1, 1);
        assert!(graph.evaluate(&ctx).is_ok());
    }

    #[test]
    fn params_roundtrip_through_saved_graph() {
        let mut graph = Graph::new();
        let id = graph.add(InputImage::new("docs/id-card.jpg").with_dpi(600.0));
        let json = graph.to_json().unwrap();
        assert!(json.contains("\"kind\": \"input_image\""));

        let back = Graph::from_json(&json, &crate::registry()).unwrap();
        assert_eq!(
            back.node_as::<InputImage>(id),
            graph.node_as::<InputImage>(id)
        );

        // Missing fields fall back to defaults.
        let node: InputImage = serde_json::from_str(r#"{"path":"a.png"}"#).unwrap();
        assert!(node.apply_orientation);
    }
}

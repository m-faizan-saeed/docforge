//! Tool #2 — Output Image: a graph endpoint that writes an image to disk.

use std::{
    borrow::Cow,
    error::Error,
    fs::{self, File},
    io::{self, BufWriter, ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

use docforge_core::{EvalContext, Inputs, Node, NodeError, NodeType, PortSpec, RasterImage, Value};
use image::{
    ExtendedColorType, ImageEncoder,
    codecs::{
        bmp::BmpEncoder,
        jpeg::{JpegEncoder, PixelDensity},
        tiff::TiffEncoder,
        webp::WebPEncoder,
    },
};
use serde::{Deserialize, Serialize};

use crate::runtime::{FileStamp, RuntimeState};

/// Name of the input port.
pub const INPUT: &str = "image";
/// Name of the pass-through output port.
pub const OUTPUT: &str = "image";

const INPUTS: &[PortSpec] = &[PortSpec::image(INPUT)];
const OUTPUTS: &[PortSpec] = &[PortSpec::image(OUTPUT)];

/// File format to write.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SaveFormat {
    /// Pick from the file extension.
    #[default]
    Auto,
    Png,
    Jpeg,
    /// Lossless WebP.
    Webp,
    Tiff,
    Bmp,
}

impl SaveFormat {
    pub fn from_extension(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "png" => Self::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "webp" => Self::Webp,
            "tif" | "tiff" => Self::Tiff,
            "bmp" => Self::Bmp,
            _ => return None,
        })
    }

    /// Replaces [`SaveFormat::Auto`] with the format implied by `path`.
    pub fn resolve(self, path: &Path) -> Result<Self, NodeError> {
        match self {
            Self::Auto => Self::from_extension(path).ok_or_else(|| NodeError::InvalidParam {
                name: "format",
                reason: format!(
                    "cannot infer a format from `{}`; use a .png/.jpg/.webp/.tif/.bmp \
                     extension or choose the format explicitly",
                    path.display()
                ),
            }),
            format => Ok(format),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Auto | Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Tiff => "tif",
            Self::Bmp => "bmp",
        }
    }

    pub fn supports_alpha(self) -> bool {
        !matches!(self, Self::Jpeg)
    }

    /// Whether the print resolution is written into the file.
    pub fn stores_dpi(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg)
    }
}

/// PNG speed/size trade-off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PngCompression {
    Fast,
    #[default]
    Balanced,
    /// Smallest files, slowest encode.
    High,
}

/// What to do when the target file already exists.
///
/// The node's own previous output never counts as a conflict, so re-saving
/// after every edit replaces that same file instead of failing or piling up
/// numbered copies. Something else rewriting it in the meantime makes it a
/// conflict again.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IfExists {
    #[default]
    Overwrite,
    /// Fail rather than replace someone else's file.
    Error,
    /// Write `name-1.ext`, `name-2.ext`, … instead.
    Rename,
}

/// Saves the incoming image and passes it through unchanged, so it can also
/// sit mid-chain as a checkpoint.
///
/// * Writes are atomic: data goes to a temporary sibling that is renamed over
///   the target, so a crash or full disk never leaves a truncated image.
/// * The print resolution (the `dpi` override, or the image's own) is written
///   to PNG and JPEG files, so physical size survives the round trip.
/// * Fully opaque images are stored without an alpha channel (25% less raw
///   data); formats without alpha get transparency flattened onto
///   `background`.
/// * With `auto_save` off the node only passes the image through; the editor
///   calls [`OutputImage::save`] on demand instead, e.g. from an Export button.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputImage {
    /// Target file; relative paths resolve against the graph's base directory.
    pub path: PathBuf,
    pub format: SaveFormat,
    /// JPEG quality, 1–100.
    pub jpeg_quality: u8,
    pub png_compression: PngCompression,
    /// Colour transparent pixels are composited onto when the format has no alpha.
    pub background: [u8; 3],
    /// Print resolution to record, overriding the image's own.
    pub dpi: Option<f32>,
    pub if_exists: IfExists,
    /// Create missing parent directories.
    pub create_dirs: bool,
    /// Write on every evaluation whose input changed.
    pub auto_save: bool,
    #[serde(skip)]
    last_written: RuntimeState<Option<Written>>,
}

/// What the node wrote last: the path it was asked for, and the file it
/// actually produced (they differ under [`IfExists::Rename`]).
#[derive(Clone, Debug)]
struct Written {
    requested: PathBuf,
    file: FileStamp,
}

impl Default for OutputImage {
    fn default() -> Self {
        Self {
            path: PathBuf::new(),
            format: SaveFormat::Auto,
            jpeg_quality: 92,
            png_compression: PngCompression::Balanced,
            background: [255, 255, 255],
            dpi: None,
            if_exists: IfExists::Overwrite,
            create_dirs: true,
            auto_save: true,
            last_written: RuntimeState::default(),
        }
    }
}

impl OutputImage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            ..Self::default()
        }
    }

    pub fn with_format(mut self, format: SaveFormat) -> Self {
        self.format = format;
        self
    }

    pub fn with_if_exists(mut self, policy: IfExists) -> Self {
        self.if_exists = policy;
        self
    }

    /// The file most recently written by this node, if any.
    pub fn last_written_path(&self) -> Option<PathBuf> {
        self.last_written.get().map(|w| w.file.path)
    }

    /// Writes `image` according to the node's settings and returns the path
    /// actually written. Usable on its own, outside any graph.
    pub fn save(&self, image: &RasterImage, ctx: &EvalContext) -> Result<PathBuf, NodeError> {
        self.validate()?;
        let requested = ctx.resolve_path(&self.path);
        let format = self.format.resolve(&requested)?;
        let target = self.choose_target(&requested)?;

        if self.create_dirs
            && let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|source| NodeError::Io {
                path: parent.to_owned(),
                source,
            })?;
        }

        let dpi = self.dpi.or(image.dpi());
        write_atomic(&target, |w| self.encode(image, format, dpi, w))?;

        if let Some(file) = FileStamp::read(&target) {
            self.last_written.set(Some(Written { requested, file }));
        }
        Ok(target)
    }

    fn validate(&self) -> Result<(), NodeError> {
        if self.path.as_os_str().is_empty() {
            return Err(NodeError::InvalidParam {
                name: "path",
                reason: "no output file chosen".into(),
            });
        }
        if !(1..=100).contains(&self.jpeg_quality) {
            return Err(NodeError::InvalidParam {
                name: "jpeg_quality",
                reason: format!("must be 1–100, got {}", self.jpeg_quality),
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
        Ok(())
    }

    fn choose_target(&self, requested: &Path) -> Result<PathBuf, NodeError> {
        // Our previous output for this same request, still untouched on disk.
        let own = self
            .last_written
            .get()
            .filter(|w| w.requested == requested && w.file.is_current())
            .map(|w| w.file.path);

        match self.if_exists {
            IfExists::Overwrite => Ok(requested.to_owned()),
            IfExists::Error => {
                if !requested.exists() || own.as_deref() == Some(requested) {
                    Ok(requested.to_owned())
                } else {
                    Err(NodeError::Io {
                        path: requested.to_owned(),
                        source: io::Error::new(
                            ErrorKind::AlreadyExists,
                            "file exists and `if_exists` is set to `error`",
                        ),
                    })
                }
            }
            IfExists::Rename => {
                if let Some(own) = own {
                    return Ok(own);
                }
                if !requested.exists() {
                    return Ok(requested.to_owned());
                }
                (1..=9999)
                    .map(|n| numbered(requested, n))
                    .find(|candidate| !candidate.exists())
                    .ok_or_else(|| NodeError::Io {
                        path: requested.to_owned(),
                        source: io::Error::new(ErrorKind::AlreadyExists, "no free numbered name"),
                    })
            }
        }
    }

    fn encode(
        &self,
        image: &RasterImage,
        format: SaveFormat,
        dpi: Option<f32>,
        w: &mut BufWriter<File>,
    ) -> Result<(), Box<dyn Error + Send + Sync>> {
        let (width, height) = image.dimensions();
        let (data, color) = encodable_pixels(image, format.supports_alpha(), self.background);
        match format {
            SaveFormat::Png => {
                write_png(w, &data, width, height, color, dpi, self.png_compression)?;
            }
            SaveFormat::Jpeg => {
                let mut encoder = JpegEncoder::new_with_quality(w, self.jpeg_quality);
                if let Some(dpi) = dpi {
                    encoder.set_pixel_density(PixelDensity::dpi(
                        dpi.round().clamp(1.0, u16::MAX as f32) as u16,
                    ));
                }
                encoder.write_image(&data, width, height, color)?;
            }
            SaveFormat::Webp => {
                WebPEncoder::new_lossless(w).write_image(&data, width, height, color)?
            }
            SaveFormat::Tiff => TiffEncoder::new(w).write_image(&data, width, height, color)?,
            SaveFormat::Bmp => BmpEncoder::new(w).write_image(&data, width, height, color)?,
            SaveFormat::Auto => unreachable!("format resolved before encoding"),
        }
        Ok(())
    }
}

/// RGB when the image is opaque or the format cannot store alpha (flattening
/// onto `background`), otherwise the RGBA buffer as is, without copying.
fn encodable_pixels(
    image: &RasterImage,
    keep_alpha: bool,
    background: [u8; 3],
) -> (Cow<'_, [u8]>, ExtendedColorType) {
    let opaque = image.pixels().iter().all(|px| px[3] == u8::MAX);
    if keep_alpha && !opaque {
        return (Cow::Borrowed(image.as_raw()), ExtendedColorType::Rgba8);
    }
    let mut rgb = Vec::with_capacity(image.pixels().len() * 3);
    if opaque {
        rgb.extend(image.pixels().iter().flat_map(|px| [px[0], px[1], px[2]]));
    } else {
        rgb.extend(image.pixels().iter().flat_map(|&[r, g, b, a]| {
            let blend = |c: u8, bg: u8| {
                ((c as u32 * a as u32 + bg as u32 * (255 - a as u32) + 127) / 255) as u8
            };
            [
                blend(r, background[0]),
                blend(g, background[1]),
                blend(b, background[2]),
            ]
        }));
    }
    (Cow::Owned(rgb), ExtendedColorType::Rgb8)
}

/// PNG goes through the `png` crate directly because `image`'s encoder cannot
/// write the `pHYs` (resolution) chunk.
fn write_png(
    w: &mut BufWriter<File>,
    data: &[u8],
    width: u32,
    height: u32,
    color: ExtendedColorType,
    dpi: Option<f32>,
    compression: PngCompression,
) -> Result<(), png::EncodingError> {
    let mut encoder = png::Encoder::new(w, width, height);
    encoder.set_color(match color {
        ExtendedColorType::Rgb8 => png::ColorType::Rgb,
        _ => png::ColorType::Rgba,
    });
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(match compression {
        PngCompression::Fast => png::Compression::Fast,
        PngCompression::Balanced => png::Compression::Balanced,
        PngCompression::High => png::Compression::High,
    });
    if let Some(dpi) = dpi {
        let per_metre = (dpi / 0.0254).round() as u32;
        encoder.set_pixel_dims(Some(png::PixelDimensions {
            xppu: per_metre,
            yppu: per_metre,
            unit: png::Unit::Meter,
        }));
    }
    let mut writer = encoder.write_header()?;
    writer.write_image_data(data)?;
    writer.finish()
}

/// Writes through a temporary sibling and renames it over `target`, so the
/// target is either the old file or the complete new one, never partial.
pub(crate) fn write_atomic(
    target: &Path,
    encode: impl FnOnce(&mut BufWriter<File>) -> Result<(), Box<dyn Error + Send + Sync>>,
) -> Result<(), NodeError> {
    let file_name = target.file_name().unwrap_or_default().to_string_lossy();
    let tmp = target.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let io_err = |source| NodeError::Io {
        path: target.to_owned(),
        source,
    };

    let result = File::create(&tmp).map_err(io_err).and_then(|file| {
        let mut writer = BufWriter::new(file);
        encode(&mut writer).map_err(|source| NodeError::Encode {
            path: target.to_owned(),
            source,
        })?;
        writer.flush().map_err(io_err)?;
        let file = writer.into_inner().map_err(|e| io_err(e.into_error()))?;
        file.sync_data().map_err(io_err)?;
        fs::rename(&tmp, target).map_err(io_err)
    });
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// `dir/scan.png` → `dir/scan-3.png`.
fn numbered(path: &Path, n: u32) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

impl Node for OutputImage {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn inputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(INPUTS)
    }

    fn outputs(&self) -> Cow<'_, [PortSpec]> {
        Cow::Borrowed(OUTPUTS)
    }

    fn evaluate(&self, inputs: &Inputs, ctx: &EvalContext) -> Result<Vec<Value>, NodeError> {
        let image = inputs.image(INPUT)?;
        if self.auto_save {
            self.save(image, ctx)?;
        }
        Ok(vec![Value::Image(Arc::clone(image))])
    }

    /// Re-save if our output file was deleted behind our back.
    fn needs_refresh(&self, _: &EvalContext) -> bool {
        self.auto_save
            && self
                .last_written
                .get()
                .is_some_and(|w| !w.file.path.exists())
    }
}

impl NodeType for OutputImage {
    const KIND: &'static str = "output_image";
    const DISPLAY_NAME: &'static str = "Output Image";
    const CATEGORY: &'static str = "Input / Output";
}

#[cfg(test)]
mod tests {
    use docforge_core::{Graph, NodeStatus};
    use image::{ColorType, DynamicImage};

    use super::*;
    use crate::InputImage;

    fn gradient(w: u32, h: u32, alpha: u8) -> RasterImage {
        let data = (0..w * h)
            .flat_map(|i| [(i % 251) as u8, (i * 7 % 256) as u8, (i / w) as u8, alpha])
            .collect();
        RasterImage::from_rgba8(w, h, data).unwrap()
    }

    fn reload(path: &Path) -> RasterImage {
        InputImage::new(path).load(&EvalContext::new()).unwrap()
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn lossless_formats_roundtrip_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = EvalContext::new();
        let translucent = gradient(17, 9, 128);
        for ext in ["png", "webp", "tif", "bmp"] {
            let path = dir.path().join(format!("out.{ext}"));
            OutputImage::new(&path).save(&translucent, &ctx).unwrap();
            assert_eq!(reload(&path).as_raw(), translucent.as_raw(), "{ext}");
        }
    }

    #[test]
    fn opaque_images_drop_the_alpha_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opaque.png");
        OutputImage::new(&path)
            .save(&gradient(4, 4, 255), &EvalContext::new())
            .unwrap();
        assert_eq!(image::open(&path).unwrap().color(), ColorType::Rgb8);
    }

    #[test]
    fn dpi_survives_png_and_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = EvalContext::new();
        let img = gradient(8, 8, 255).with_dpi(Some(300.0));

        let png = dir.path().join("a.png");
        OutputImage::new(&png).save(&img, &ctx).unwrap();
        assert!((reload(&png).dpi().unwrap() - 300.0).abs() < 0.01);

        let jpg = dir.path().join("a.jpg");
        let mut node = OutputImage::new(&jpg);
        node.dpi = Some(600.0); // override wins over the image's own
        node.save(&img, &ctx).unwrap();
        assert_eq!(reload(&jpg).dpi(), Some(600.0));
    }

    #[test]
    fn jpeg_flattens_transparency_onto_background() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("flat.jpg");
        let clear = RasterImage::new(16, 16, [0, 0, 0, 0]);
        let mut node = OutputImage::new(&path);
        node.background = [255, 0, 0];
        node.save(&clear, &EvalContext::new()).unwrap();

        let [r, g, b, a] = reload(&path).pixels()[0];
        assert!(
            r > 240 && g < 15 && b < 15 && a == 255,
            "{:?}",
            [r, g, b, a]
        );
    }

    #[test]
    fn format_selection() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = EvalContext::new();
        let img = gradient(4, 4, 255);

        // Explicit format beats the extension.
        let path = dir.path().join("actually-jpeg.png");
        OutputImage::new(&path)
            .with_format(SaveFormat::Jpeg)
            .save(&img, &ctx)
            .unwrap();
        assert_eq!(
            image::ImageFormat::from_path(&path).unwrap(),
            image::ImageFormat::Png
        );
        let sniffed = image::ImageReader::open(&path)
            .unwrap()
            .with_guessed_format()
            .unwrap();
        assert_eq!(sniffed.format(), Some(image::ImageFormat::Jpeg));

        assert_eq!(
            SaveFormat::from_extension(Path::new("X.JPEG")),
            Some(SaveFormat::Jpeg)
        );
        assert!(matches!(
            OutputImage::new(dir.path().join("out.xyz")).save(&img, &ctx),
            Err(NodeError::InvalidParam { name: "format", .. })
        ));
    }

    #[test]
    fn rejects_bad_params() {
        let ctx = EvalContext::new();
        let img = gradient(2, 2, 255);
        assert!(matches!(
            OutputImage::default().save(&img, &ctx),
            Err(NodeError::InvalidParam { name: "path", .. })
        ));
        let mut node = OutputImage::new("x.jpg");
        node.jpeg_quality = 0;
        assert!(matches!(
            node.save(&img, &ctx),
            Err(NodeError::InvalidParam {
                name: "jpeg_quality",
                ..
            })
        ));
    }

    #[test]
    fn existing_file_policies() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = EvalContext::new();
        let img = gradient(4, 4, 255);
        let path = dir.path().join("scan.png");
        fs::write(&path, b"someone else's file").unwrap();

        let strict = OutputImage::new(&path).with_if_exists(IfExists::Error);
        assert!(matches!(strict.save(&img, &ctx), Err(NodeError::Io { .. })));
        assert_eq!(fs::read(&path).unwrap(), b"someone else's file");

        let renaming = OutputImage::new(&path).with_if_exists(IfExists::Rename);
        let first = renaming.save(&img, &ctx).unwrap();
        assert_eq!(first, dir.path().join("scan-1.png"));
        // Re-saving reuses our own file rather than creating scan-2.png ...
        assert_eq!(renaming.save(&gradient(5, 5, 255), &ctx).unwrap(), first);
        assert_eq!(reload(&first).dimensions(), (5, 5));
        // ... unless someone else has replaced it since.
        fs::write(&first, b"edited elsewhere").unwrap();
        assert_eq!(
            renaming.save(&img, &ctx).unwrap(),
            dir.path().join("scan-2.png")
        );

        let new_path = dir.path().join("fresh.png");
        let strict = OutputImage::new(&new_path).with_if_exists(IfExists::Error);
        strict.save(&img, &ctx).unwrap();
        // Our own previous output is not a conflict.
        strict.save(&img, &ctx).unwrap();
    }

    #[test]
    fn writes_are_atomic_and_create_directories() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = EvalContext::new().with_base_dir(dir.path());
        OutputImage::new("nested/deeper/out.png")
            .save(&gradient(3, 3, 255), &ctx)
            .unwrap();
        assert_eq!(files_in(&dir.path().join("nested/deeper")), ["out.png"]);

        // A failed encode leaves neither a partial target nor a temp file.
        let empty = RasterImage::new(0, 0, [0; 4]);
        assert!(OutputImage::new("empty.png").save(&empty, &ctx).is_err());
        assert_eq!(files_in(dir.path()), ["nested"]);
    }

    #[test]
    fn in_graph_saves_once_per_change_and_passes_through() {
        let dir = tempfile::tempdir().unwrap();
        let src_path = dir.path().join("in.png");
        let out_path = dir.path().join("out.jpg");
        DynamicImage::ImageRgba8(gradient(6, 6, 255).into_rgba_image())
            .save(&src_path)
            .unwrap();

        let mut g = Graph::new();
        let input = g.add(InputImage::new(&src_path));
        let output = g.add(OutputImage::new(&out_path));
        g.connect(input, crate::input_image::OUTPUT, output, INPUT)
            .unwrap();
        let ctx = EvalContext::new();

        assert!(g.evaluate(&ctx).is_ok());
        assert!(
            g.output(input, "image")
                .unwrap()
                .ptr_eq(g.output(output, OUTPUT).unwrap())
        );
        let first = FileStamp::read(&out_path).unwrap();

        // Unchanged input: served from cache, file untouched.
        assert!(g.evaluate(&ctx).recomputed.is_empty());
        assert!(first.is_current());

        // Deleted output is re-created.
        fs::remove_file(&out_path).unwrap();
        assert_eq!(g.evaluate(&ctx).recomputed, [output]);
        assert!(out_path.exists());

        // Manual mode: pass-through only.
        let manual = dir.path().join("manual.png");
        g.edit_node::<OutputImage, _>(output, |o| {
            o.path = manual.clone();
            o.auto_save = false;
        })
        .unwrap();
        g.evaluate(&ctx);
        assert!(matches!(g.status(output), Some(NodeStatus::Ready(_))));
        assert!(!manual.exists());
    }

    #[test]
    fn params_roundtrip_through_saved_graph() {
        let mut g = Graph::new();
        let mut node = OutputImage::new("print/id-card.jpg").with_if_exists(IfExists::Rename);
        node.jpeg_quality = 80;
        node.dpi = Some(300.0);
        let id = g.add(node);

        let json = g.to_json().unwrap();
        assert!(json.contains("\"if_exists\": \"rename\""));
        let back = Graph::from_json(&json, &crate::registry()).unwrap();
        assert_eq!(
            back.node_as::<OutputImage>(id),
            g.node_as::<OutputImage>(id)
        );

        let defaults: OutputImage = serde_json::from_str(r#"{"path":"a.png"}"#).unwrap();
        assert_eq!(defaults.jpeg_quality, 92);
        assert!(defaults.auto_save);
    }
}

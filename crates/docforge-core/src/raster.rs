use std::fmt;

use image::{DynamicImage, RgbaImage};

use crate::NodeError;

/// The common in-memory image every node consumes and produces.
///
/// Pixels are tightly packed, row-major, non-premultiplied RGBA8 (sRGB). One
/// fixed layout keeps every tool simple and lets them share buffers without
/// conversion; 8 bits per channel is plenty for scanned documents and is a
/// quarter the size of a float buffer.
///
/// `dpi` is the print resolution, when known. The layout engine uses it to
/// place images at their true physical size.
#[derive(Clone, PartialEq)]
pub struct RasterImage {
    width: u32,
    height: u32,
    dpi: Option<f32>,
    data: Vec<u8>,
}

impl RasterImage {
    pub const CHANNELS: usize = 4;

    /// Creates an image filled with one colour.
    ///
    /// # Panics
    /// If `width * height * 4` overflows `usize`.
    pub fn new(width: u32, height: u32, fill: [u8; 4]) -> Self {
        let len = buffer_len(width, height).expect("image dimensions overflow usize");
        Self {
            width,
            height,
            dpi: None,
            data: fill.repeat(len / Self::CHANNELS),
        }
    }

    /// Wraps an existing RGBA8 buffer without copying it.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> Result<Self, NodeError> {
        let expected = buffer_len(width, height)
            .ok_or_else(|| NodeError::InvalidBuffer(format!("{width}x{height} overflows usize")))?;
        if data.len() != expected {
            return Err(NodeError::InvalidBuffer(format!(
                "{width}x{height} RGBA8 needs {expected} bytes, got {}",
                data.len()
            )));
        }
        Ok(Self {
            width,
            height,
            dpi: None,
            data,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn dpi(&self) -> Option<f32> {
        self.dpi
    }

    pub fn set_dpi(&mut self, dpi: Option<f32>) {
        self.dpi = dpi.filter(|d| d.is_finite() && *d > 0.0);
    }

    pub fn with_dpi(mut self, dpi: Option<f32>) -> Self {
        self.set_dpi(dpi);
        self
    }

    /// Physical size in millimetres, if the resolution is known.
    pub fn size_mm(&self) -> Option<(f32, f32)> {
        let dpi = self.dpi?;
        let mm = |px: u32| px as f32 / dpi * 25.4;
        Some((mm(self.width), mm(self.height)))
    }

    pub fn as_raw(&self) -> &[u8] {
        &self.data
    }

    pub fn as_raw_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn into_raw(self) -> Vec<u8> {
        self.data
    }

    pub fn pixels(&self) -> &[[u8; 4]] {
        self.data.as_chunks().0
    }

    pub fn pixels_mut(&mut self) -> &mut [[u8; 4]] {
        self.data.as_chunks_mut().0
    }

    /// # Panics
    /// If `y >= height`.
    pub fn row(&self, y: u32) -> &[[u8; 4]] {
        let w = self.width as usize;
        &self.pixels()[y as usize * w..][..w]
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[[u8; 4]]> {
        self.pixels().chunks_exact(self.width.max(1) as usize)
    }

    pub fn rows_mut(&mut self) -> impl ExactSizeIterator<Item = &mut [[u8; 4]]> {
        let w = self.width.max(1) as usize;
        self.pixels_mut().chunks_exact_mut(w)
    }

    pub fn get_pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        self.index(x, y).map(|i| self.pixels()[i])
    }

    pub fn pixel_mut(&mut self, x: u32, y: u32) -> Option<&mut [u8; 4]> {
        let i = self.index(x, y)?;
        Some(&mut self.pixels_mut()[i])
    }

    fn index(&self, x: u32, y: u32) -> Option<usize> {
        (x < self.width && y < self.height).then(|| y as usize * self.width as usize + x as usize)
    }

    /// Copies into an `image` crate buffer, e.g. to reuse its filters.
    pub fn to_rgba_image(&self) -> RgbaImage {
        RgbaImage::from_raw(self.width, self.height, self.data.clone())
            .expect("RasterImage buffer length invariant")
    }

    /// Converts into an `image` crate buffer without copying.
    pub fn into_rgba_image(self) -> RgbaImage {
        RgbaImage::from_raw(self.width, self.height, self.data)
            .expect("RasterImage buffer length invariant")
    }
}

fn buffer_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(RasterImage::CHANNELS)
}

impl From<RgbaImage> for RasterImage {
    fn from(img: RgbaImage) -> Self {
        let (width, height) = img.dimensions();
        let mut data = img.into_raw();
        // `ImageBuffer` tolerates oversized backing storage; we do not.
        data.truncate(width as usize * height as usize * Self::CHANNELS);
        Self {
            width,
            height,
            dpi: None,
            data,
        }
    }
}

impl From<DynamicImage> for RasterImage {
    /// Free when the image is already RGBA8, otherwise one conversion pass.
    fn from(img: DynamicImage) -> Self {
        img.into_rgba8().into()
    }
}

impl fmt::Debug for RasterImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RasterImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("dpi", &self.dpi)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_wrong_buffer_length() {
        assert!(RasterImage::from_rgba8(2, 2, vec![0; 15]).is_err());
        assert!(RasterImage::from_rgba8(2, 2, vec![0; 16]).is_ok());
    }

    #[test]
    fn pixel_access_is_row_major() {
        let mut img = RasterImage::new(3, 2, [0, 0, 0, 255]);
        *img.pixel_mut(2, 1).unwrap() = [1, 2, 3, 4];
        assert_eq!(img.row(1)[2], [1, 2, 3, 4]);
        assert_eq!(img.pixels()[5], [1, 2, 3, 4]);
        assert_eq!(img.get_pixel(3, 0), None);
        assert_eq!(img.rows().len(), 2);
    }

    #[test]
    fn roundtrips_through_image_crate() {
        let img = RasterImage::new(4, 3, [10, 20, 30, 40]);
        let back = RasterImage::from(img.to_rgba_image());
        assert_eq!(img, back);
    }

    #[test]
    fn physical_size_from_dpi() {
        let img = RasterImage::new(300, 600, [0; 4]).with_dpi(Some(300.0));
        let (w, h) = img.size_mm().unwrap();
        assert!((w - 25.4).abs() < 1e-4 && (h - 50.8).abs() < 1e-4);
        assert_eq!(img.with_dpi(Some(-1.0)).dpi(), None);
    }
}

//! Reads the print resolution stored in image headers. The `image` crate does
//! not expose it, but placing a scan at its true physical size depends on it.

/// Horizontal resolution in dots per inch from a PNG `pHYs` chunk or a JPEG
/// JFIF header. Returns `None` when absent or stored as an aspect ratio only.
pub(crate) fn sniff_dpi(bytes: &[u8]) -> Option<f32> {
    png_dpi(bytes).or_else(|| jfif_dpi(bytes))
}

fn png_dpi(bytes: &[u8]) -> Option<f32> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    const METRE: u8 = 1;
    let mut rest = bytes.strip_prefix(SIGNATURE)?;
    // Chunk: length (4), type (4), data (length), crc (4). pHYs must precede IDAT.
    while rest.len() >= 8 {
        let len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
        let kind = &rest[4..8];
        let data = rest.get(8..8usize.checked_add(len)?)?;
        match kind {
            b"pHYs" if len == 9 => {
                let per_metre = u32::from_be_bytes(data[..4].try_into().ok()?);
                return (data[8] == METRE && per_metre > 0).then_some(per_metre as f32 * 0.0254);
            }
            b"IDAT" | b"IEND" => return None,
            _ => rest = rest.get(12 + len..)?,
        }
    }
    None
}

fn jfif_dpi(bytes: &[u8]) -> Option<f32> {
    const PER_INCH: u8 = 1;
    const PER_CM: u8 = 2;
    let mut rest = bytes.strip_prefix(&[0xFF, 0xD8])?;
    loop {
        // Skip fill bytes before a marker.
        while rest.first() == Some(&0xFF) && rest.get(1) == Some(&0xFF) {
            rest = &rest[1..];
        }
        let [0xFF, marker, ..] = *rest else {
            return None;
        };
        match marker {
            // Standalone markers carry no length.
            0x01 | 0xD0..=0xD7 => rest = &rest[2..],
            // Start of scan or end of image: headers are over.
            0xDA | 0xD9 => return None,
            _ => {
                let len = u16::from_be_bytes([*rest.get(2)?, *rest.get(3)?]) as usize;
                let segment = rest.get(4..2 + len)?;
                // APP0: "JFIF\0", version (2), units (1), x density (2), y density (2).
                if marker == 0xE0 && segment.len() >= 12 && segment.starts_with(b"JFIF\0") {
                    let density = u16::from_be_bytes([segment[8], segment[9]]) as f32;
                    return match segment[7] {
                        _ if density == 0.0 => None,
                        PER_INCH => Some(density),
                        PER_CM => Some(density * 2.54),
                        _ => None,
                    };
                }
                rest = rest.get(2 + len..)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_with_phys(per_metre: u32, unit: u8) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            png.extend((data.len() as u32).to_be_bytes());
            png.extend(kind);
            png.extend(data);
            png.extend([0; 4]); // CRC is not checked here
        };
        chunk(b"IHDR", &[0; 13]);
        let mut phys = per_metre.to_be_bytes().to_vec();
        phys.extend(per_metre.to_be_bytes());
        phys.push(unit);
        chunk(b"pHYs", &phys);
        chunk(b"IDAT", &[]);
        png
    }

    fn jfif(units: u8, density: u16) -> Vec<u8> {
        let mut jpg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0, 16];
        jpg.extend(b"JFIF\0\x01\x02");
        jpg.push(units);
        jpg.extend(density.to_be_bytes());
        jpg.extend(density.to_be_bytes());
        jpg.extend([0, 0]); // no thumbnail
        jpg.extend([0xFF, 0xDA]);
        jpg
    }

    #[test]
    fn reads_png_phys() {
        let dpi = sniff_dpi(&png_with_phys(11811, 1)).unwrap();
        assert!((dpi - 300.0).abs() < 0.01, "{dpi}");
        assert_eq!(sniff_dpi(&png_with_phys(11811, 0)), None);
    }

    #[test]
    fn reads_jfif_density() {
        assert_eq!(sniff_dpi(&jfif(1, 600)), Some(600.0));
        assert!((sniff_dpi(&jfif(2, 118)).unwrap() - 299.72).abs() < 0.01);
        assert_eq!(sniff_dpi(&jfif(0, 1)), None);
    }

    #[test]
    fn tolerates_garbage() {
        assert_eq!(sniff_dpi(b""), None);
        assert_eq!(sniff_dpi(&[0xFF, 0xD8, 0xFF]), None);
        assert_eq!(sniff_dpi(&[0xFF, 0xD8, 0xFF, 0xE1, 0xFF, 0xFF]), None);
        let mut truncated = png_with_phys(11811, 1);
        truncated.truncate(30);
        assert_eq!(sniff_dpi(&truncated), None);
    }
}

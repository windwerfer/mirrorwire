//! Camera frames for QR scanning (the desktop viewer's "Scan" button).
//!
//! Two parts: [`extract_luma`] turns one raw camera frame into a tightly packed 8-bit grey
//! image (pure, unit-tested, no feature needed), and `CameraScanner` (Linux, cargo feature
//! `portal`) asks the Camera portal for access, reads frames from the camera's PipeWire node
//! and runs a caller-supplied decoder (the app passes `pairing::scan_qr_luma`) on a worker
//! thread. Raw formats only (YUY2, UYVY, NV12, I420, GRAY8, BGRx, RGBx); an MJPEG-only camera
//! reports that in the scan status. No `unsafe`.

/// Raw pixel layouts the scanner can turn into luma.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LumaFormat {
    /// Packed Y0 U Y1 V.
    Yuy2,
    /// Packed U Y0 V Y1.
    Uyvy,
    /// Planar Y plane first (NV12 and I420 both start with the full-size Y plane).
    Planar420,
    Gray8,
    /// Bytes B G R x.
    Bgrx,
    /// Bytes R G B x.
    Rgbx,
}

/// Largest frame side accepted (pixels); bigger input is dropped unread.
pub const MAX_SIDE: usize = 8192;

/// Extracts the luma plane of one frame into a `width * height` buffer (no row padding).
/// `stride` is the byte distance between rows (0 = tightly packed). Returns `None` for bad
/// sizes or when `data` is too short.
pub fn extract_luma(
    format: LumaFormat,
    width: usize,
    height: usize,
    stride: usize,
    data: &[u8],
) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return None;
    }
    let (bpp, y_offset, y_step) = match format {
        LumaFormat::Yuy2 => (2, 0, 2),
        LumaFormat::Uyvy => (2, 1, 2),
        LumaFormat::Planar420 | LumaFormat::Gray8 => (1, 0, 1),
        LumaFormat::Bgrx | LumaFormat::Rgbx => (4, 0, 0),
    };
    let row_bytes = width.checked_mul(bpp)?;
    let stride = if stride == 0 { row_bytes } else { stride };
    if stride < row_bytes {
        return None;
    }
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        let start = y.checked_mul(stride)?;
        let row = data.get(start..start.checked_add(row_bytes)?)?;
        match format {
            LumaFormat::Bgrx | LumaFormat::Rgbx => {
                let rgb = format == LumaFormat::Rgbx;
                out.extend(row.as_chunks::<4>().0.iter().map(|px| {
                    let (r, g, b) = match *px {
                        [r, g, b, _] if rgb => (r, g, b),
                        [b, g, r, _] => (r, g, b),
                    };
                    ((77 * r as u32 + 150 * g as u32 + 29 * b as u32) >> 8) as u8
                }));
            }
            _ if y_step == 1 => out.extend_from_slice(row),
            _ => out.extend(row.iter().skip(y_offset).step_by(y_step).copied()),
        }
    }
    Some(out)
}

#[cfg(all(target_os = "linux", feature = "portal"))]
pub use scanner::{CameraScanner, ScanEvent};

#[cfg(all(target_os = "linux", feature = "portal"))]
mod scanner;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yuy2_and_uyvy_pick_the_y_bytes() {
        // 4x2, stride 10 (2 bytes padding per row).
        let mut yuy2 = vec![0u8; 20];
        let mut uyvy = vec![0u8; 20];
        for y in 0..2 {
            for x in 0..4 {
                let v = (y * 4 + x + 1) as u8 * 10;
                yuy2[y * 10 + x * 2] = v;
                yuy2[y * 10 + x * 2 + 1] = 99;
                uyvy[y * 10 + x * 2 + 1] = v;
                uyvy[y * 10 + x * 2] = 99;
            }
        }
        let want = vec![10, 20, 30, 40, 50, 60, 70, 80];
        assert_eq!(
            extract_luma(LumaFormat::Yuy2, 4, 2, 10, &yuy2),
            Some(want.clone())
        );
        assert_eq!(extract_luma(LumaFormat::Uyvy, 4, 2, 10, &uyvy), Some(want));
    }

    #[test]
    fn planar_and_gray_copy_rows_and_ignore_chroma() {
        let mut nv12 = [0u8; 6 * 3]; // 4x2 Y with stride 6, then chroma
        nv12[0..4].copy_from_slice(&[1, 2, 3, 4]);
        nv12[6..10].copy_from_slice(&[5, 6, 7, 8]);
        // Chroma starts after height*stride; data is only checked to cover the Y rows.
        let got = extract_luma(LumaFormat::Planar420, 4, 2, 6, &nv12[..12]).unwrap();
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            extract_luma(LumaFormat::Gray8, 2, 2, 0, &[9, 8, 7, 6]),
            Some(vec![9, 8, 7, 6])
        );
    }

    #[test]
    fn rgb_formats_weigh_green_most() {
        // One pixel: white, black, pure green, pure red/blue by layout.
        let bgrx = [255, 255, 255, 0, 0, 0, 0, 0];
        assert_eq!(
            extract_luma(LumaFormat::Bgrx, 2, 1, 0, &bgrx),
            Some(vec![255, 0])
        );
        let green = extract_luma(LumaFormat::Bgrx, 1, 1, 0, &[0, 255, 0, 0]).unwrap()[0];
        let red_bgrx = extract_luma(LumaFormat::Bgrx, 1, 1, 0, &[0, 0, 255, 0]).unwrap()[0];
        let red_rgbx = extract_luma(LumaFormat::Rgbx, 1, 1, 0, &[255, 0, 0, 0]).unwrap()[0];
        assert!(green > red_bgrx);
        assert_eq!(red_bgrx, red_rgbx);
    }

    #[test]
    fn hostile_sizes_are_rejected() {
        assert_eq!(extract_luma(LumaFormat::Gray8, 0, 4, 0, &[]), None);
        assert_eq!(extract_luma(LumaFormat::Gray8, 4, 4, 0, &[0; 15]), None);
        assert_eq!(extract_luma(LumaFormat::Gray8, 4, 4, 3, &[0; 64]), None);
        assert_eq!(extract_luma(LumaFormat::Yuy2, usize::MAX, 2, 0, &[]), None);
        assert_eq!(
            extract_luma(LumaFormat::Gray8, MAX_SIDE + 1, 1, 0, &[]),
            None
        );
        assert_eq!(
            extract_luma(LumaFormat::Gray8, 2, 2, usize::MAX, &[0; 8]),
            None
        );
    }
}

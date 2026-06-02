use std::io;

use yuv::{
    BufferStoreMut, YuvError, YuvPlanarImage, YuvPlanarImageMut, rdp_abgr_to_yuv444, rdp_argb_to_yuv444,
    rdp_bgra_to_yuv444, rdp_rgba_to_yuv444, rdp_yuv444_to_argb, rdp_yuv444_to_rgba,
};

use crate::image_processing::PixelFormat;

// FIXME: used for the test suite, we may want to drop it
pub fn ycbcr_to_argb(input: YCbCrBuffer<'_>, output: &mut [u8]) -> io::Result<()> {
    let len = u32::try_from(output.len()).map_err(io::Error::other)?;
    let width = len / 4;
    let planar = YuvPlanarImage {
        y_plane: input.y,
        y_stride: width,
        u_plane: input.cb,
        u_stride: width,
        v_plane: input.cr,
        v_stride: width,
        width,
        height: 1,
    };
    rdp_yuv444_to_argb(&planar, output, len).map_err(io::Error::other)
}

pub fn ycbcr_to_rgba(input: YCbCrBuffer<'_>, output: &mut [u8]) -> io::Result<()> {
    let len = u32::try_from(output.len()).map_err(io::Error::other)?;
    let width = len / 4;
    let planar = YuvPlanarImage {
        y_plane: input.y,
        y_stride: width,
        u_plane: input.cb,
        u_stride: width,
        v_plane: input.cr,
        v_stride: width,
        width,
        height: 1,
    };
    rdp_yuv444_to_rgba(&planar, output, len).map_err(io::Error::other)
}

/// # Panics
///
/// - Panics if `width` > 64.
/// - Panics if `height` > 64.
#[expect(clippy::too_many_arguments)]
pub fn to_64x64_ycbcr_tile(
    input: &[u8],
    width: u32,
    height: u32,
    stride: u32,
    format: PixelFormat,
    y: &mut [i16; 64 * 64],
    cb: &mut [i16; 64 * 64],
    cr: &mut [i16; 64 * 64],
) -> Result<(), YuvError> {
    assert!(width <= 64);
    assert!(height <= 64);

    let y_plane = BufferStoreMut::Borrowed(y);
    let u_plane = BufferStoreMut::Borrowed(cb);
    let v_plane = BufferStoreMut::Borrowed(cr);
    let mut plane = YuvPlanarImageMut {
        y_plane,
        y_stride: 64,
        u_plane,
        u_stride: 64,
        v_plane,
        v_stride: 64,
        width,
        height,
    };

    match format {
        PixelFormat::RgbA32 | PixelFormat::RgbX32 => rdp_rgba_to_yuv444(&mut plane, input, stride),
        PixelFormat::ARgb32 | PixelFormat::XRgb32 => rdp_argb_to_yuv444(&mut plane, input, stride),
        PixelFormat::BgrA32 | PixelFormat::BgrX32 => rdp_bgra_to_yuv444(&mut plane, input, stride),
        PixelFormat::ABgr32 | PixelFormat::XBgr32 => rdp_abgr_to_yuv444(&mut plane, input, stride),
    }
}

/// Convert a 16-bit RDP color (RGB565) to RGB representation. Input value should be represented in
/// little-endian format.
///
/// RGB565 format: RRRRR GGGGGG BBBBB (5 bits red, 6 bits green, 5 bits blue)
pub fn rdp_16bit_to_rgb(color: u16) -> [u8; 3] {
    #[expect(clippy::missing_panics_doc, reason = "unreachable panic (checked integer underflow)")]
    let out = {
        let r = u8::try_from(((((color >> 11) & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        let g = u8::try_from(((((color >> 5) & 0x3f) * 259) + 33) >> 6).expect("max possible value is 255");
        let b = u8::try_from((((color & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        [r, g, b]
    };

    out
}

/// Converts a 15-bit RDP color (RGB555) to 8-bit RGB representation.
///
/// This function handles the RGB555 pixel format used by legacy RDP servers
/// operating at 15 bits-per-pixel color depth.
///
/// # Format
///
/// RGB555 uses 16 bits per pixel with the following layout:
/// ```text
/// Bit:  15 | 14-10 | 9-5  | 4-0
///       X  | RRRRR | GGGGG | BBBBB
/// ```
/// - Bit 15: Unused (ignored)
/// - Bits 14-10: Red component (5 bits, 0-31)
/// - Bits 9-5: Green component (5 bits, 0-31)
/// - Bits 4-0: Blue component (5 bits, 0-31)
///
/// # Arguments
///
/// * `color` - A 16-bit value containing the RGB555 color in little-endian format
///
/// # Returns
///
/// An array `[R, G, B]` containing the 8-bit color components (0-255 each).
///
/// # Example
///
/// ```
/// use ironrdp_graphics::color_conversion::rdp_15bit_to_rgb;
///
/// // Pure red (0x7C00 = 0b0_11111_00000_00000)
/// assert_eq!(rdp_15bit_to_rgb(0x7C00), [255, 0, 0]);
///
/// // Pure green (0x03E0 = 0b0_00000_11111_00000)
/// assert_eq!(rdp_15bit_to_rgb(0x03E0), [0, 255, 0]);
///
/// // Pure blue (0x001F = 0b0_00000_00000_11111)
/// assert_eq!(rdp_15bit_to_rgb(0x001F), [0, 0, 255]);
/// ```
pub fn rdp_15bit_to_rgb(color: u16) -> [u8; 3] {
    #[expect(clippy::missing_panics_doc, reason = "unreachable panic (checked integer underflow)")]
    let out = {
        // Extract 5-bit components and scale to 8-bit
        // Using the same scaling formula as 16-bit: ((value * 527) + 23) >> 6
        // This formula provides accurate rounding when scaling 5-bit to 8-bit
        let r = u8::try_from(((((color >> 10) & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        let g = u8::try_from(((((color >> 5) & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        let b = u8::try_from((((color & 0x1f) * 527) + 23) >> 6).expect("max possible value is 255");
        [r, g, b]
    };

    out
}

#[derive(Debug)]
pub struct YCbCrBuffer<'a> {
    pub y: &'a [i16],
    pub cb: &'a [i16],
    pub cr: &'a [i16],
}

impl Iterator for YCbCrBuffer<'_> {
    type Item = YCbCr;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.y.is_empty() && !self.cb.is_empty() && !self.cr.is_empty() {
            let y = self.y[0];
            let cb = self.cb[0];
            let cr = self.cr[0];

            self.y = &self.y[1..];
            self.cb = &self.cb[1..];
            self.cr = &self.cr[1..];

            Some(YCbCr { y, cb, cr })
        } else {
            None
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct YCbCr {
    pub y: i16,
    pub cb: i16,
    pub cr: i16,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rdp_16bit_to_rgb_black() {
        // All zeros = black
        let rgb = rdp_16bit_to_rgb(0x0000);
        assert_eq!(rgb, [0, 0, 0]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_white() {
        // All ones = white (0xFFFF in RGB565)
        let rgb = rdp_16bit_to_rgb(0xFFFF);
        assert_eq!(rgb, [255, 255, 255]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_red() {
        // Red: 11111 000000 00000 = 0xF800
        let rgb = rdp_16bit_to_rgb(0xF800);
        assert_eq!(rgb, [255, 0, 0]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_green() {
        // Green: 00000 111111 00000 = 0x07E0
        let rgb = rdp_16bit_to_rgb(0x07E0);
        assert_eq!(rgb, [0, 255, 0]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_blue() {
        // Blue: 00000 000000 11111 = 0x001F
        let rgb = rdp_16bit_to_rgb(0x001F);
        assert_eq!(rgb, [0, 0, 255]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_black() {
        // All zeros = black
        let rgb = rdp_15bit_to_rgb(0x0000);
        assert_eq!(rgb, [0, 0, 0]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_white() {
        // All ones in RGB555 = 0x7FFF (bit 15 is ignored)
        let rgb = rdp_15bit_to_rgb(0x7FFF);
        assert_eq!(rgb, [255, 255, 255]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_red() {
        // Red: 0 11111 00000 00000 = 0x7C00
        let rgb = rdp_15bit_to_rgb(0x7C00);
        assert_eq!(rgb, [255, 0, 0]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_green() {
        // Green: 0 00000 11111 00000 = 0x03E0
        let rgb = rdp_15bit_to_rgb(0x03E0);
        assert_eq!(rgb, [0, 255, 0]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_blue() {
        // Blue: 0 00000 00000 11111 = 0x001F
        let rgb = rdp_15bit_to_rgb(0x001F);
        assert_eq!(rgb, [0, 0, 255]);
    }

    #[test]
    fn test_rdp_15bit_ignores_high_bit() {
        // Setting bit 15 should have no effect
        let rgb_without_high = rdp_15bit_to_rgb(0x7FFF);
        let rgb_with_high = rdp_15bit_to_rgb(0xFFFF);
        assert_eq!(rgb_without_high, rgb_with_high);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_mid_values() {
        // Test mid-range values to verify scaling accuracy
        // Half intensity for each channel: 0 10000 10000 10000 = 0x4210
        let rgb = rdp_15bit_to_rgb(0x4210);
        // 16/31 scaled to 255 should be approximately 131-132
        assert!(rgb[0] >= 130 && rgb[0] <= 135);
        assert!(rgb[1] >= 130 && rgb[1] <= 135);
        assert!(rgb[2] >= 130 && rgb[2] <= 135);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_mid_values() {
        // Test mid-range values
        // Half intensity: 10000 100000 10000 = 0x8410
        let rgb = rdp_16bit_to_rgb(0x8410);
        // 16/31 for R and B, 32/63 for G, scaled to 255
        assert!(rgb[0] >= 130 && rgb[0] <= 135);
        assert!(rgb[1] >= 130 && rgb[1] <= 135);
        assert!(rgb[2] >= 130 && rgb[2] <= 135);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_yellow() {
        // Yellow = Red + Green: 0 11111 11111 00000 = 0x7FE0
        let rgb = rdp_15bit_to_rgb(0x7FE0);
        assert_eq!(rgb, [255, 255, 0]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_cyan() {
        // Cyan = Green + Blue: 0 00000 11111 11111 = 0x03FF
        let rgb = rdp_15bit_to_rgb(0x03FF);
        assert_eq!(rgb, [0, 255, 255]);
    }

    #[test]
    fn test_rdp_15bit_to_rgb_magenta() {
        // Magenta = Red + Blue: 0 11111 00000 11111 = 0x7C1F
        let rgb = rdp_15bit_to_rgb(0x7C1F);
        assert_eq!(rgb, [255, 0, 255]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_yellow() {
        // Yellow = Red + Green: 11111 111111 00000 = 0xFFE0
        let rgb = rdp_16bit_to_rgb(0xFFE0);
        assert_eq!(rgb, [255, 255, 0]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_cyan() {
        // Cyan = Green + Blue: 00000 111111 11111 = 0x07FF
        let rgb = rdp_16bit_to_rgb(0x07FF);
        assert_eq!(rgb, [0, 255, 255]);
    }

    #[test]
    fn test_rdp_16bit_to_rgb_magenta() {
        // Magenta = Red + Blue: 11111 000000 11111 = 0xF81F
        let rgb = rdp_16bit_to_rgb(0xF81F);
        assert_eq!(rgb, [255, 0, 255]);
    }

    #[test]
    fn test_rdp_15bit_single_bit_values() {
        // Test minimum non-zero value for each channel
        // Just red bit 10 set: 0 00001 00000 00000 = 0x0400
        let rgb = rdp_15bit_to_rgb(0x0400);
        assert_eq!(rgb[0], 8); // 1/31 * 255 ≈ 8
        assert_eq!(rgb[1], 0);
        assert_eq!(rgb[2], 0);

        // Just green bit 5 set: 0 00000 00001 00000 = 0x0020
        let rgb = rdp_15bit_to_rgb(0x0020);
        assert_eq!(rgb[0], 0);
        assert_eq!(rgb[1], 8);
        assert_eq!(rgb[2], 0);

        // Just blue bit 0 set: 0 00000 00000 00001 = 0x0001
        let rgb = rdp_15bit_to_rgb(0x0001);
        assert_eq!(rgb[0], 0);
        assert_eq!(rgb[1], 0);
        assert_eq!(rgb[2], 8);
    }
}

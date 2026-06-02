//! Graphics Update Processing
//!
//! This module handles both fast-path and slow-path graphics updates from the RDP server.
//! It processes bitmap data, drawing orders, and palette updates to render the remote
//! desktop image.
//!
//! # Fast-Path vs Slow-Path
//!
//! RDP defines two mechanisms for sending graphics updates:
//!
//! - **Fast-Path**: An optimized binary format with minimal headers, used by modern servers
//!   for better performance. Updates are sent via [`FastPathUpdate`] PDUs.
//!
//! - **Slow-Path**: The original X224/T.125 data path with full headers. Used by older
//!   servers or when fast-path is not negotiated. Updates arrive as [`ShareDataPdu::Update`]
//!   containing [`ServerGraphicsUpdate`] data.
//!
//! Both paths ultimately use the same rendering primitives (bitmaps, drawing orders),
//! just with different wire encodings.
//!
//! # Drawing Orders
//!
//! Drawing orders are GDI-style commands that instruct the client to draw primitives
//! directly on the frame buffer. This is more bandwidth-efficient than sending bitmap
//! data for simple operations like filling rectangles or drawing lines.
//!
//! Supported primary orders:
//! - `DstBlt` - Destination block transfer (fill with ROP)
//! - `PatBlt` - Pattern block transfer (fill with brush)
//! - `ScrBlt` - Screen block transfer (copy within screen)
//! - `OpaqueRect` - Fill rectangle with solid color
//! - `LineTo` - Draw a line
//! - `Polyline` - Draw connected line segments
//! - `Multi*` variants - Batch versions of the above
//!
//! # 8bpp Palette Mode
//!
//! When the connection uses 8 bits-per-pixel color depth, pixel values are indices
//! into a 256-entry color palette. The server sends palette updates via
//! [`PaletteUpdate`] PDUs. See [`ColorPalette`] for details.
//!
//! [`FastPathUpdate`]: ironrdp_pdu::fast_path::FastPathUpdate
//! [`ShareDataPdu::Update`]: ironrdp_pdu::rdp::headers::ShareDataPdu::Update
//! [`ServerGraphicsUpdate`]: ironrdp_pdu::rdp::server_graphics_update::ServerGraphicsUpdate
//! [`PaletteUpdate`]: ironrdp_pdu::rdp::server_graphics_update::PaletteUpdate

use std::sync::Arc;

use bytes::Bytes;
use ironrdp_bulk::BulkCompressor;
use ironrdp_core::{DecodeErrorKind, ReadCursor, WriteBuf, decode_cursor};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::pointer::{DecodedPointer, PointerBitmapTarget, PointerPalette};
use ironrdp_graphics::rdp6::BitmapStreamDecoder;
use ironrdp_graphics::rle::RlePixelFormat;
use ironrdp_pdu::bitmap::BitmapUpdateData;
use ironrdp_pdu::codecs::rfx::FrameAcknowledgePdu;
use ironrdp_pdu::fast_path::{FastPathHeader, FastPathUpdate, FastPathUpdatePdu, Fragmentation};
use ironrdp_pdu::geometry::{InclusiveRectangle, Rectangle as _};
use ironrdp_pdu::pointer::PointerUpdateData;
use ironrdp_pdu::rdp::capability_sets::{CodecId, CODEC_ID_NONE, CODEC_ID_REMOTEFX};
use ironrdp_pdu::rdp::drawing_orders::{DrawingOrder, DrawingOrderState, PrimaryOrderData};
use ironrdp_pdu::rdp::headers::{CompressionFlags, ShareDataPdu};
use ironrdp_pdu::rdp::server_graphics_update::{PaletteUpdate, ServerGraphicsUpdate};
use ironrdp_pdu::surface_commands::{FrameAction, FrameMarkerPdu, SurfaceCommand};
use tracing::{debug, trace, warn};

/// Color palette for 8bpp indexed color mode.
///
/// In 8 bits-per-pixel color mode, each pixel value is an index into a 256-entry
/// color palette. This structure stores the RGB color values for each palette index.
///
/// The palette is initialized with a grayscale gradient by default, where index 0
/// is black and index 255 is white. The server can update the palette at any time
/// by sending a Palette Update PDU.
///
/// # Example
///
/// ```
/// use ironrdp_session::fast_path::ColorPalette;
///
/// let mut palette = ColorPalette::new();
///
/// // Default is grayscale
/// assert_eq!(palette.get(0), [0, 0, 0]);       // Black
/// assert_eq!(palette.get(255), [255, 255, 255]); // White
/// ```
#[derive(Debug, Clone)]
#[must_use]
pub struct ColorPalette {
    /// RGB entries indexed by palette index (0-255)
    entries: [[u8; 3]; 256],
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl ColorPalette {
    /// Creates a new palette initialized with grayscale colors.
    ///
    /// Index 0 will be black `[0, 0, 0]` and index 255 will be white `[255, 255, 255]`,
    /// with a linear gradient in between.
    pub fn new() -> Self {
        let mut entries = [[0u8; 3]; 256];
        // Initialize with grayscale by default
        for gray in 0u8..=255 {
            entries[usize::from(gray)] = [gray, gray, gray];
        }
        Self { entries }
    }

    /// Updates palette entries from a [`PaletteUpdate`] PDU.
    ///
    /// This replaces palette entries starting from index 0 with the colors
    /// from the update. Entries beyond index 255 are ignored.
    ///
    /// [`PaletteUpdate`]: ironrdp_pdu::rdp::server_graphics_update::PaletteUpdate
    pub fn update(&mut self, palette: &PaletteUpdate) {
        for (i, entry) in palette.entries.iter().enumerate() {
            if i < 256 {
                self.entries[i] = [entry.red, entry.green, entry.blue];
            }
        }
    }

    /// Returns the RGB color for the given palette index.
    ///
    /// # Arguments
    ///
    /// * `index` - Palette index (0-255)
    ///
    /// # Returns
    ///
    /// An array `[R, G, B]` containing the color components (0-255 each).
    #[inline]
    pub fn get(&self, index: u8) -> [u8; 3] {
        self.entries[usize::from(index)]
    }

    /// Converts this palette to a [`PointerPalette`] for use in pointer decoding.
    ///
    /// [`PointerPalette`]: ironrdp_graphics::pointer::PointerPalette
    pub fn to_pointer_palette(&self) -> PointerPalette {
        PointerPalette::from_entries(self.entries)
    }
}

/// Cached bitmap entry
#[derive(Debug)]
pub(crate) struct CachedBitmap {
    /// Bitmap width in pixels
    pub(crate) width: u16,
    /// Bitmap height in pixels
    pub(crate) height: u16,
    /// Bitmap data in RGBA format (4 bytes per pixel)
    /// Uses Bytes for zero-copy sharing via Arc
    pub(crate) data: Bytes,
}

/// Bitmap cache for secondary drawing orders
///
/// RDP uses bitmap caching to reduce bandwidth. The server sends bitmaps once
/// via CacheBitmapV2 orders, then references them in MemBlt orders to draw.
///
/// The cache has 3 separate caches (cache IDs 0-2), each with configurable size.
/// Per MS-RDPEGDI, typical sizes are:
/// - Cache 0: 200 entries (small bitmaps, e.g. 8x8)
/// - Cache 1: 600 entries (medium bitmaps, e.g. 32x32)
/// - Cache 2: 2048 entries (large bitmaps, e.g. 64x64)
///
/// Uses Bytes for zero-copy bitmap storage (Arc-based reference counting).
#[derive(Debug)]
pub struct BitmapCache {
    /// Three separate caches indexed by cache ID (0-2)
    caches: [Vec<Option<CachedBitmap>>; 3],
}

impl Default for BitmapCache {
    fn default() -> Self {
        Self::new()
    }
}

impl BitmapCache {
    /// Creates a new bitmap cache with default sizes
    pub fn new() -> Self {
        // Initialize caches without requiring Clone on CachedBitmap
        let mut cache0 = Vec::with_capacity(200);
        cache0.resize_with(200, || None);

        let mut cache1 = Vec::with_capacity(600);
        cache1.resize_with(600, || None);

        let mut cache2 = Vec::with_capacity(2048);
        cache2.resize_with(2048, || None);

        Self {
            caches: [cache0, cache1, cache2],
        }
    }

    /// Stores a bitmap in the cache
    ///
    /// # Arguments
    ///
    /// * `cache_id` - Cache ID (0-2)
    /// * `cache_index` - Index within the cache
    /// * `width` - Bitmap width in pixels
    /// * `height` - Bitmap height in pixels
    /// * `data` - Bitmap data in RGBA format (zero-copy Bytes)
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, `Err` if cache_id or cache_index is invalid
    pub fn store(&mut self, cache_id: u8, cache_index: u16, width: u16, height: u16, data: Bytes) -> SessionResult<()> {
        let data_len = data.len();
        let cache = self
            .caches
            .get_mut(usize::from(cache_id))
            .ok_or_else(|| reason_err!("BitmapCache", "Invalid cache_id: {}", cache_id))?;

        let entry = cache
            .get_mut(usize::from(cache_index))
            .ok_or_else(|| reason_err!("BitmapCache", "Invalid cache_index: {}", cache_index))?;

        *entry = Some(CachedBitmap { width, height, data });

        trace!(
            "Stored bitmap in cache[{}][{}]: {}x{}, {} bytes",
            cache_id,
            cache_index,
            width,
            height,
            data_len
        );

        Ok(())
    }

    /// Retrieves a bitmap from the cache
    ///
    /// # Arguments
    ///
    /// * `cache_id` - Cache ID (0-2)
    /// * `cache_index` - Index within the cache
    ///
    /// # Returns
    ///
    /// Reference to the cached bitmap, or `Err` if not found
    pub fn get(&self, cache_id: u8, cache_index: u16) -> SessionResult<&CachedBitmap> {
        let cache = self
            .caches
            .get(usize::from(cache_id))
            .ok_or_else(|| reason_err!("BitmapCache", "Invalid cache_id: {}", cache_id))?;

        let entry = cache
            .get(usize::from(cache_index))
            .ok_or_else(|| reason_err!("BitmapCache", "Invalid cache_index: {}", cache_index))?;

        entry
            .as_ref()
            .ok_or_else(|| reason_err!("BitmapCache", "Bitmap not cached at [{}][{}]", cache_id, cache_index))
    }
}

use crate::image::DecodedImage;
use crate::pointer::PointerCache;
use crate::{SessionError, SessionErrorExt as _, SessionResult, custom_err, reason_err, rfx};

/// Creates an `InclusiveRectangle` from signed coordinates, clamping negative values to 0.
///
/// This handles the case where RDP servers may send negative coordinates for off-screen
/// rendering or clipping. Per MS-RDPEGDI, coordinates are signed 16-bit integers, but
/// our rectangle type uses unsigned values for screen positions.
///
/// The function also handles potential overflow when calculating `right` and `bottom`
/// by performing arithmetic in `i32` space before clamping.
#[inline]
#[expect(
    clippy::as_conversions,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    reason = "Coordinates are clamped to valid u16 range before casting"
)]
fn make_inclusive_rect(x: i16, y: i16, width: i16, height: i16) -> InclusiveRectangle {
    // Clamp coordinates to non-negative values
    let left = x.max(0) as u16;
    let top = y.max(0) as u16;

    // Calculate right/bottom in i32 to avoid overflow, then clamp
    let right = (i32::from(x) + i32::from(width) - 1).max(0).min(i32::from(u16::MAX)) as u16;
    let bottom = (i32::from(y) + i32::from(height) - 1).max(0).min(i32::from(u16::MAX)) as u16;

    InclusiveRectangle {
        left,
        top,
        right,
        bottom,
    }
}

/// Combines multiple rectangles into a single bounding rectangle.
///
/// Returns `None` if the iterator is empty.
fn combine_rectangles(rects: impl Iterator<Item = InclusiveRectangle>) -> Option<InclusiveRectangle> {
    rects.fold(None, |acc, rect| {
        Some(match acc {
            None => rect,
            Some(prev) => InclusiveRectangle {
                left: prev.left.min(rect.left),
                top: prev.top.min(rect.top),
                right: prev.right.max(rect.right),
                bottom: prev.bottom.max(rect.bottom),
            },
        })
    })
}

/// Extracts RGB components from a 24-bit color stored as `0x00RRGGBB` in a `u32`.
///
/// Returns `[R, G, B, 255]` as an RGBA color with full opacity.
#[inline]
const fn rgb_from_u32(color: u32) -> [u8; 4] {
    let [b, g, r, _] = color.to_le_bytes();
    [r, g, b, 255]
}

#[derive(Debug)]
pub enum UpdateKind {
    None,
    Region(InclusiveRectangle),
    PointerDefault,
    PointerHidden,
    PointerPosition { x: u16, y: u16 },
    PointerBitmap(Arc<DecodedPointer>),
}

pub struct Processor {
    complete_data: CompleteData,
    rfx_handler: rfx::DecodingContext,
    marker_processor: FrameMarkerProcessor,
    bitmap_stream_decoder: BitmapStreamDecoder,
    pointer_cache: PointerCache,
    use_system_pointer: bool,
    mouse_pos_update: Option<(u16, u16)>,
    enable_server_pointer: bool,
    pointer_software_rendering: bool,
    /// Bulk decompressor for server-to-client compressed PDUs.
    /// `None` when compression was not negotiated.
    bulk_decompressor: Option<BulkCompressor>,
    /// Color palette for 8bpp indexed color mode
    color_palette: ColorPalette,
    /// Drawing order state for delta encoding
    drawing_order_state: DrawingOrderState,
    /// Bitmap cache for secondary drawing orders
    bitmap_cache: BitmapCache,
    #[cfg(feature = "qoiz")]
    zdctx: zstd_safe::DCtx<'static>,
}

impl Processor {
    pub fn update_mouse_pos(&mut self, x: u16, y: u16) {
        self.mouse_pos_update = Some((x, y));
    }

    /// Update the color palette from a PaletteUpdate PDU.
    /// This is used for 8bpp indexed color mode.
    pub fn update_palette(&mut self, palette: &PaletteUpdate) {
        debug!("Updating color palette with {} entries", palette.entries.len());
        self.color_palette.update(palette);
    }

    /// Get a reference to the current color palette.
    pub fn palette(&self) -> &ColorPalette {
        &self.color_palette
    }

    /// Process a single drawing order and apply it to the image.
    ///
    /// Returns the affected rectangle if the order modified the image.
    fn process_drawing_order(
        &mut self,
        image: &mut DecodedImage,
        order: &DrawingOrder<'_>,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        match order {
            DrawingOrder::Primary(primary) => {
                let rect = self.process_primary_order(image, primary)?;
                Ok(rect)
            }
            DrawingOrder::Secondary(secondary) => {
                // Secondary orders are caching operations - we don't render them directly
                self.process_secondary_order(image, secondary)?;
                Ok(None)
            }
            DrawingOrder::AlternateSecondary(alt_secondary) => {
                // Alternate secondary orders include frame markers
                trace!("Alternate secondary order: {:?}", alt_secondary.order_type);
                Ok(None)
            }
        }
    }

    /// Process a secondary drawing order (caching operations).
    fn process_secondary_order(
        &mut self,
        image: &mut DecodedImage,
        secondary: &ironrdp_pdu::rdp::drawing_orders::SecondaryOrder<'_>,
    ) -> SessionResult<()> {
        use ironrdp_pdu::rdp::drawing_orders::SecondaryOrderType;

        debug!("Processing secondary order: {:?}", secondary.order_type);

        match secondary.order_type {
            SecondaryOrderType::CacheBitmapV2 => {
                self.process_cache_bitmap_v2(image, secondary.data)?;
            }
            _ => {
                debug!("Unhandled secondary order: {:?}", secondary.order_type);
            }
        }

        Ok(())
    }

    /// Process CacheBitmapV2 secondary order.
    ///
    /// This order tells the client to cache a bitmap for later use by MemBlt orders.
    /// The bitmap is stored in one of three caches (cache_id 0-2) at a specific index.
    fn process_cache_bitmap_v2(&mut self, _image: &mut DecodedImage, data: &[u8]) -> SessionResult<()> {
        // CacheBitmapV2 format (MS-RDPEGDI 2.2.2.2.1.2.3):
        // - cache_id (u8, bits 0-1)
        // - flags (u8, bits 2-7)
        // - cache_index (u16)
        // - key1 (u32, optional if PERSISTENT flag set)
        // - key2 (u32, optional if PERSISTENT flag set)
        // - bitmap_data (variable)

        if data.len() < 3 {
            return Err(reason_err!("CacheBitmapV2", "Data too short: {} bytes", data.len()));
        }

        let mut cursor = ReadCursor::new(data);

        // Read cache_id and flags
        let cache_id_and_flags = cursor.read_u8();
        let cache_id = cache_id_and_flags & 0x03; // Bits 0-1
        let flags = (cache_id_and_flags >> 2) & 0x3F; // Bits 2-7

        // Read cache_index
        let cache_index = cursor.read_u16();

        // Skip persistent keys if present (PERSISTENT flag = 0x01)
        if (flags & 0x01) != 0 {
            let _ = cursor.read_u32(); // Skip key1
            let _ = cursor.read_u32(); // Skip key2
        }

        // Remaining data is the compressed bitmap
        let bitmap_data = cursor.remaining();

        // Parse bitmap header from the compressed data
        // Format: width (u16), height (u16), bpp (u16), flags (u16), length (u16), data
        if bitmap_data.len() < 10 {
            return Err(reason_err!("CacheBitmapV2", "Bitmap data too short"));
        }

        let mut bmp_cursor = ReadCursor::new(bitmap_data);
        let width = bmp_cursor.read_u16();
        let height = bmp_cursor.read_u16();
        let bpp = bmp_cursor.read_u16();
        let bmp_flags = bmp_cursor.read_u16();
        let bmp_length = bmp_cursor.read_u16();

        let compressed_data = bmp_cursor.read_slice(usize::from(bmp_length));

        // Decompress the bitmap data
        let rgba_data = self.decompress_bitmap(compressed_data, width, height, bpp, bmp_flags)?;

        // Store in cache
        self.bitmap_cache
            .store(cache_id, cache_index, width, height, rgba_data)?;

        debug!(
            "Cached bitmap: cache[{}][{}] = {}x{} @ {}bpp",
            cache_id, cache_index, width, height, bpp
        );

        Ok(())
    }

    /// Decompress bitmap data to RGBA format.
    /// Returns Bytes for zero-copy sharing.
    fn decompress_bitmap(
        &mut self,
        data: &[u8],
        width: u16,
        height: u16,
        bpp: u16,
        flags: u16,
    ) -> SessionResult<Bytes> {
        // Check if compressed (NO_BITMAP_COMPRESSION_HDR flag = 0x0400)
        let is_compressed = (flags & 0x0400) == 0;

        let pixel_count = usize::from(width) * usize::from(height);
        let mut rgba_data = vec![0u8; pixel_count * 4];

        if is_compressed {
            // Use RLE decompression
            let bpp_usize = usize::from(bpp);

            // Decompress to temporary buffer
            let mut temp_buffer = Vec::new();

            ironrdp_graphics::rle::decompress(data, &mut temp_buffer, usize::from(width), usize::from(height), bpp_usize)
                .map_err(|e| reason_err!("RLE", "Decompression failed: {:?}", e))?;

            // Convert to RGBA
            self.convert_to_rgba(&temp_buffer, &mut rgba_data, width, height, bpp)?;
        } else {
            // Uncompressed - convert directly to RGBA
            self.convert_to_rgba(data, &mut rgba_data, width, height, bpp)?;
        }

        // Wrap in Bytes for zero-copy Arc-based sharing
        Ok(Bytes::from(rgba_data))
    }

    /// Convert bitmap data to RGBA format.
    fn convert_to_rgba(&self, src: &[u8], dst: &mut [u8], width: u16, height: u16, bpp: u16) -> SessionResult<()> {
        let pixel_count = usize::from(width) * usize::from(height);

        match bpp {
            8 => {
                // 8bpp indexed color - use palette
                for i in 0..pixel_count {
                    let palette_index = src[i];
                    let rgb = self.color_palette.get(palette_index);
                    dst[i * 4] = rgb[0]; // R
                    dst[i * 4 + 1] = rgb[1]; // G
                    dst[i * 4 + 2] = rgb[2]; // B
                    dst[i * 4 + 3] = 255; // A
                }
            }
            15 | 16 => {
                // 15/16bpp RGB - convert from BGR565/555 to RGBA
                for i in 0..pixel_count {
                    let pixel = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]);

                    #[expect(clippy::cast_possible_truncation, clippy::as_conversions)]
                    let (r, g, b) = if bpp == 15 {
                        // RGB555: 0RRRRRGGGGGBBBBB (masked to 5 bits, safe to cast to u8)
                        let r = ((pixel >> 10) & 0x1F) as u8;
                        let g = ((pixel >> 5) & 0x1F) as u8;
                        let b = (pixel & 0x1F) as u8;
                        ((r << 3) | (r >> 2), (g << 3) | (g >> 2), (b << 3) | (b >> 2))
                    } else {
                        // RGB565: RRRRRGGGGGGBBBBB (masked to 5/6 bits, safe to cast to u8)
                        let r = ((pixel >> 11) & 0x1F) as u8;
                        let g = ((pixel >> 5) & 0x3F) as u8;
                        let b = (pixel & 0x1F) as u8;
                        ((r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2))
                    };

                    dst[i * 4] = r;
                    dst[i * 4 + 1] = g;
                    dst[i * 4 + 2] = b;
                    dst[i * 4 + 3] = 255;
                }
            }
            24 => {
                // 24bpp BGR to RGBA
                for i in 0..pixel_count {
                    dst[i * 4] = src[i * 3 + 2]; // R (from B)
                    dst[i * 4 + 1] = src[i * 3 + 1]; // G
                    dst[i * 4 + 2] = src[i * 3]; // B (from R)
                    dst[i * 4 + 3] = 255; // A
                }
            }
            32 => {
                // 32bpp BGRA to RGBA
                for i in 0..pixel_count {
                    dst[i * 4] = src[i * 4 + 2]; // R (from B)
                    dst[i * 4 + 1] = src[i * 4 + 1]; // G
                    dst[i * 4 + 2] = src[i * 4]; // B (from R)
                    dst[i * 4 + 3] = src[i * 4 + 3]; // A
                }
            }
            _ => {
                return Err(reason_err!("Bitmap", "Unsupported bpp: {}", bpp));
            }
        }

        Ok(())
    }

    /// Try to process a MemBlt order (draws cached bitmap).
    ///
    /// MemBlt (Memory Block Transfer) is the most common drawing order from Windows servers.
    /// It copies a bitmap from the cache to the screen.
    fn try_process_memblt(&self, image: &mut DecodedImage, data: &[u8]) -> SessionResult<Option<InclusiveRectangle>> {
        debug!("Attempting to process MemBlt order ({} bytes)", data.len());
        // MemBlt format (MS-RDPEGDI 2.2.2.2.1.1.2.9):
        // - cache_id (u16, bits 0-7 = cache_id, bits 8-15 = color_table)
        // - x (i16)
        // - y (i16)
        // - width (i16)
        // - height (i16)
        // - rop (u8)
        // - src_x (i16)
        // - src_y (i16)
        // - cache_index (u16)

        if data.len() < 18 {
            return Ok(None); // Not a MemBlt order
        }

        let mut cursor = ReadCursor::new(data);

        let cache_id_and_color = cursor.read_u16();
        #[expect(clippy::cast_possible_truncation, clippy::as_conversions)]
        let cache_id = (cache_id_and_color & 0xFF) as u8; // Masked to 8 bits, safe to cast
        let x = cursor.read_i16();
        let y = cursor.read_i16();
        let width = cursor.read_i16();
        let height = cursor.read_i16();
        let rop = cursor.read_u8();
        let src_x = cursor.read_i16();
        let src_y = cursor.read_i16();
        let cache_index = cursor.read_u16();

        trace!(
            "MemBlt: cache[{}][{}] -> ({},{}) {}x{}, rop=0x{:02X}, src=({},{})",
            cache_id,
            cache_index,
            x,
            y,
            width,
            height,
            rop,
            src_x,
            src_y
        );

        // Get cached bitmap
        let _cached = match self.bitmap_cache.get(cache_id, cache_index) {
            Ok(bmp) => bmp,
            Err(e) => {
                warn!("MemBlt: Bitmap not in cache[{}][{}]: {}", cache_id, cache_index, e);
                return Ok(None);
            }
        };

        // Create destination rectangle
        let dst_rect = make_inclusive_rect(x, y, width, height);

        // For now, only support SRCCOPY (0xCC) ROP
        if rop != 0xCC {
            trace!("MemBlt: Unsupported ROP 0x{:02X}, skipping", rop);
            return Ok(None);
        }

        // Copy bitmap to image
        // TODO: Implement proper bitmap copying with direct memory access
        // For now, draw a colored rectangle to show the cached bitmap was used
        image.fill_rectangle(&dst_rect, [128, 128, 255, 255])?;

        debug!(
            "MemBlt: Drew cached bitmap[{}][{}] at ({},{}) {}x{}",
            cache_id, cache_index, x, y, width, height
        );

        Ok(Some(dst_rect))
    }

    /// Process a primary drawing order.
    fn process_primary_order(
        &mut self,
        image: &mut DecodedImage,
        order: &ironrdp_pdu::rdp::drawing_orders::PrimaryOrder,
    ) -> SessionResult<Option<InclusiveRectangle>> {
        match &order.data {
            PrimaryOrderData::DstBlt(dstblt) => {
                // DstBlt performs a ROP on the destination rectangle
                // Common ROPs:
                // - 0x00 (BLACKNESS): Fill with black
                // - 0x55 (DSTINVERT): Invert destination
                // - 0xFF (WHITENESS): Fill with white
                trace!(
                    "DstBlt: x={}, y={}, w={}, h={}, rop=0x{:02X}",
                    dstblt.x,
                    dstblt.y,
                    dstblt.width,
                    dstblt.height,
                    dstblt.rop
                );

                let rect = make_inclusive_rect(dstblt.x, dstblt.y, dstblt.width, dstblt.height);

                // Apply the ROP operation
                match dstblt.rop {
                    0x00 => {
                        // BLACKNESS - fill with black
                        image.fill_rectangle(&rect, [0, 0, 0, 255])?;
                    }
                    0x55 => {
                        // DSTINVERT - invert destination
                        image.invert_rectangle(&rect)?;
                    }
                    0xFF => {
                        // WHITENESS - fill with white
                        image.fill_rectangle(&rect, [255, 255, 255, 255])?;
                    }
                    _ => {
                        debug!("DstBlt ROP 0x{:02X} not implemented", dstblt.rop);
                    }
                }

                Ok(Some(rect))
            }
            PrimaryOrderData::OpaqueRect(opaque) => {
                // OpaqueRect fills a rectangle with a solid color
                trace!(
                    "OpaqueRect: x={}, y={}, w={}, h={}, color=0x{:06X}",
                    opaque.x,
                    opaque.y,
                    opaque.width,
                    opaque.height,
                    opaque.color
                );

                let rect = make_inclusive_rect(opaque.x, opaque.y, opaque.width, opaque.height);

                image.fill_rectangle(&rect, rgb_from_u32(opaque.color))?;
                Ok(Some(rect))
            }
            PrimaryOrderData::ScrBlt(scrblt) => {
                // ScrBlt copies a rectangle from source to destination
                trace!(
                    "ScrBlt: dst=({},{}) size={}x{} src=({},{}) rop=0x{:02X}",
                    scrblt.x,
                    scrblt.y,
                    scrblt.width,
                    scrblt.height,
                    scrblt.src_x,
                    scrblt.src_y,
                    scrblt.rop
                );

                let rect = make_inclusive_rect(scrblt.x, scrblt.y, scrblt.width, scrblt.height);

                // ROP 0xCC is SRCCOPY - direct copy
                match scrblt.rop {
                    0xCC => {
                        // SRCCOPY - copy source to destination
                        image.copy_rectangle(scrblt.src_x, scrblt.src_y, &rect)?;
                    }
                    _ => {
                        debug!("ScrBlt ROP 0x{:02X} not implemented, using SRCCOPY", scrblt.rop);
                        image.copy_rectangle(scrblt.src_x, scrblt.src_y, &rect)?;
                    }
                }

                Ok(Some(rect))
            }
            PrimaryOrderData::PatBlt(patblt) => {
                // PatBlt fills a rectangle with a pattern
                trace!(
                    "PatBlt: x={}, y={}, w={}, h={}, rop=0x{:02X}",
                    patblt.x,
                    patblt.y,
                    patblt.width,
                    patblt.height,
                    patblt.rop
                );

                let rect = make_inclusive_rect(patblt.x, patblt.y, patblt.width, patblt.height);

                // Handle different ROPs
                match patblt.rop {
                    0x00 => {
                        // BLACKNESS
                        image.fill_rectangle(&rect, [0, 0, 0, 255])?;
                    }
                    0x55 => {
                        // DSTINVERT
                        image.invert_rectangle(&rect)?;
                    }
                    0xF0 => {
                        // PATCOPY - copy pattern to destination
                        if patblt.brush.style == 0 {
                            // BS_SOLID - solid brush
                            image.fill_rectangle(&rect, rgb_from_u32(patblt.fore_color))?;
                        } else {
                            debug!("PatBlt PATCOPY with brush style {} not implemented", patblt.brush.style);
                        }
                    }
                    0xFF => {
                        // WHITENESS
                        image.fill_rectangle(&rect, [255, 255, 255, 255])?;
                    }
                    _ => {
                        // For other ROPs with solid brush, use foreground color
                        if patblt.brush.style == 0 {
                            image.fill_rectangle(&rect, rgb_from_u32(patblt.fore_color))?;
                        } else {
                            debug!(
                                "PatBlt ROP 0x{:02X} with brush style {} not implemented",
                                patblt.rop, patblt.brush.style
                            );
                        }
                    }
                }

                Ok(Some(rect))
            }
            PrimaryOrderData::LineTo(line) => {
                trace!(
                    "LineTo: ({},{}) to ({},{}) color=0x{:06X}",
                    line.start_x,
                    line.start_y,
                    line.end_x,
                    line.end_y,
                    line.pen_color
                );

                let rect = image.draw_line(
                    line.start_x,
                    line.start_y,
                    line.end_x,
                    line.end_y,
                    rgb_from_u32(line.pen_color),
                )?;

                Ok(Some(rect))
            }
            PrimaryOrderData::MultiOpaqueRect(multi) => {
                trace!(
                    "MultiOpaqueRect: {} rectangles, color=0x{:06X}",
                    multi.rectangles.len(),
                    multi.color
                );

                let color = rgb_from_u32(multi.color);
                let mut rects = Vec::with_capacity(multi.rectangles.len());

                for delta in &multi.rectangles {
                    let rect = make_inclusive_rect(delta.left, delta.top, delta.width, delta.height);
                    image.fill_rectangle(&rect, color)?;
                    rects.push(rect);
                }

                Ok(combine_rectangles(rects.into_iter()))
            }
            PrimaryOrderData::MultiDstBlt(multi) => {
                trace!(
                    "MultiDstBlt: {} rectangles, rop=0x{:02X}",
                    multi.rectangles.len(),
                    multi.rop
                );

                let mut rects = Vec::with_capacity(multi.rectangles.len());

                for delta in &multi.rectangles {
                    let rect = make_inclusive_rect(delta.left, delta.top, delta.width, delta.height);

                    match multi.rop {
                        0x00 => image.fill_rectangle(&rect, [0, 0, 0, 255])?,
                        0x55 => image.invert_rectangle(&rect)?,
                        0xFF => image.fill_rectangle(&rect, [255, 255, 255, 255])?,
                        _ => {
                            debug!("MultiDstBlt ROP 0x{:02X} not implemented", multi.rop);
                            rect.clone()
                        }
                    };

                    rects.push(rect);
                }

                Ok(combine_rectangles(rects.into_iter()))
            }
            PrimaryOrderData::MultiPatBlt(multi) => {
                trace!(
                    "MultiPatBlt: {} rectangles, rop=0x{:02X}",
                    multi.rectangles.len(),
                    multi.rop
                );

                let color = rgb_from_u32(multi.fore_color);
                let mut rects = Vec::with_capacity(multi.rectangles.len());

                for delta in &multi.rectangles {
                    let rect = make_inclusive_rect(delta.left, delta.top, delta.width, delta.height);

                    match multi.rop {
                        0x00 => image.fill_rectangle(&rect, [0, 0, 0, 255])?,
                        0x55 => image.invert_rectangle(&rect)?,
                        0xF0 if multi.brush.style == 0 => image.fill_rectangle(&rect, color)?,
                        0xFF => image.fill_rectangle(&rect, [255, 255, 255, 255])?,
                        _ => {
                            if multi.brush.style == 0 {
                                image.fill_rectangle(&rect, color)?
                            } else {
                                debug!(
                                    "MultiPatBlt ROP 0x{:02X} with brush style {} not implemented",
                                    multi.rop, multi.brush.style
                                );
                                rect.clone()
                            }
                        }
                    };

                    rects.push(rect);
                }

                Ok(combine_rectangles(rects.into_iter()))
            }
            PrimaryOrderData::MultiScrBlt(multi) => {
                trace!(
                    "MultiScrBlt: {} rectangles, src=({},{}) rop=0x{:02X}",
                    multi.rectangles.len(),
                    multi.src_x,
                    multi.src_y,
                    multi.rop
                );

                let mut rects = Vec::with_capacity(multi.rectangles.len());
                let mut src_x = multi.src_x;
                let mut src_y = multi.src_y;

                for (i, delta) in multi.rectangles.iter().enumerate() {
                    let rect = make_inclusive_rect(delta.left, delta.top, delta.width, delta.height);

                    // For first rectangle, use the base src coordinates
                    // For subsequent ones, calculate offset from the delta
                    if i > 0 {
                        src_x = src_x.wrapping_add(delta.left - multi.rectangles[i - 1].left);
                        src_y = src_y.wrapping_add(delta.top - multi.rectangles[i - 1].top);
                    }

                    image.copy_rectangle(src_x, src_y, &rect)?;
                    rects.push(rect);
                }

                Ok(combine_rectangles(rects.into_iter()))
            }
            PrimaryOrderData::Polyline(poly) => {
                trace!(
                    "Polyline: start=({},{}) {} points, color=0x{:06X}",
                    poly.x,
                    poly.y,
                    poly.points.len(),
                    poly.pen_color
                );

                let color = rgb_from_u32(poly.pen_color);

                let mut rects = Vec::with_capacity(poly.points.len());
                let mut current_x = poly.x;
                let mut current_y = poly.y;

                for point in &poly.points {
                    let next_x = current_x.wrapping_add(point.x);
                    let next_y = current_y.wrapping_add(point.y);

                    let rect = image.draw_line(current_x, current_y, next_x, next_y, color)?;
                    rects.push(rect);

                    current_x = next_x;
                    current_y = next_y;
                }

                Ok(combine_rectangles(rects.into_iter()))
            }
            PrimaryOrderData::Other(data) => {
                // Try to parse as MemBlt (order type 0x0D)
                // MemBlt is the most common order from Windows servers
                if let Some(rect) = self.try_process_memblt(image, data)? {
                    Ok(Some(rect))
                } else {
                    trace!("Unsupported primary order type: {:?}", order.order_type);
                    Ok(None)
                }
            }
        }
    }

    /// Process input fast path frame and return list of updates.
    pub fn process(
        &mut self,
        image: &mut DecodedImage,
        input: &[u8],
        output: &mut WriteBuf,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        if let Some((x, y)) = self.mouse_pos_update.take() {
            if let Some(rect) = image.move_pointer(x, y)? {
                processor_updates.push(UpdateKind::Region(rect));
            }
        }

        let mut input = ReadCursor::new(input);

        let header = decode_cursor::<FastPathHeader>(&mut input).map_err(SessionError::decode)?;
        trace!(fast_path_header = ?header, "Received Fast-Path packet");

        // A single FastPath output PDU can contain multiple updates.
        // Loop over all updates within the PDU payload.
        while !input.is_empty() {
            let update_result = self.process_single_update(&mut input, image, output)?;
            processor_updates.extend(update_result);
        }

        Ok(processor_updates)
    }

    /// Process a single FastPath update from the cursor, advancing past it.
    fn process_single_update(
        &mut self,
        input: &mut ReadCursor<'_>,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        let update_pdu = decode_cursor::<FastPathUpdatePdu<'_>>(input).map_err(SessionError::decode)?;
        trace!(fast_path_update_fragmentation = ?update_pdu.fragmentation);

        // Decompress the payload if the server sent it compressed.
        let decompressed_data;
        let payload = if let Some(flags) = update_pdu.compression_flags {
            if flags.contains(CompressionFlags::COMPRESSED) || flags.contains(CompressionFlags::FLUSHED) {
                let bulk_flags =
                    u32::from(flags.bits()) | u32::from(update_pdu.compression_type.map_or(0, |ct| ct.as_u8()));

                if let Some(ref mut decompressor) = self.bulk_decompressor {
                    let decompressed = decompressor
                        .decompress(update_pdu.data, bulk_flags)
                        .map_err(|e| reason_err!("FastPath", "bulk decompression failed: {}", e))?;
                    // Copy decompressed data before accessing metrics (releases the mutable borrow).
                    decompressed_data = decompressed.to_vec();
                    debug!(
                        compressed_size = update_pdu.data.len(),
                        decompressed_size = decompressed_data.len(),
                        compression_type = ?update_pdu.compression_type,
                        compression_ratio = format_args!("{:.2}x", decompressor.compression_ratio()),
                        total_compressed = decompressor.total_compressed_bytes(),
                        total_uncompressed = decompressor.total_uncompressed_bytes(),
                        "Decompressed FastPath update"
                    );
                    decompressed_data.as_slice()
                } else {
                    warn!("Received compressed FastPath data but no decompressor is configured");
                    update_pdu.data
                }
            } else {
                // Compression flags present but COMPRESSED bit not set — pass data through.
                // Still need to inform the decompressor of FLUSHED/AT_FRONT flags even
                // without compressed payload.
                update_pdu.data
            }
        } else {
            update_pdu.data
        };

        let processed_complete_data = self.complete_data.process_data(payload, update_pdu.fragmentation);

        let update_code = update_pdu.update_code;

        let Some(data) = processed_complete_data else {
            return Ok(processor_updates);
        };

        let update = FastPathUpdate::decode_with_code(data.as_slice(), update_code);

        match update {
            Ok(FastPathUpdate::SurfaceCommands(surface_commands)) => {
                trace!("Received Surface Commands: {} pieces", surface_commands.len());
                let update_region =
                    self.process_surface_commands(image, output, surface_commands, &mut processor_updates)?;
                processor_updates.push(UpdateKind::Region(update_region));
            }
            Ok(FastPathUpdate::Bitmap(bitmap_update)) => {
                trace!("Received bitmap update");
                let updates = self.process_bitmap_update(image, bitmap_update)?;
                processor_updates.extend(updates);
            }
            Ok(FastPathUpdate::Pointer(update)) => {
                let updates = self.process_pointer_update(image, update)?;
                processor_updates.extend(updates);
            }
            Ok(FastPathUpdate::Palette(palette)) => {
                debug!("Received fast-path palette update: {} entries", palette.entries.len());
                let palette_update = PaletteUpdate {
                    entries: palette
                        .entries
                        .into_iter()
                        .map(|e| ironrdp_pdu::rdp::server_graphics_update::PaletteEntry {
                            red: e.red,
                            green: e.green,
                            blue: e.blue,
                        })
                        .collect(),
                };
                self.color_palette.update(&palette_update);
            }
            Ok(FastPathUpdate::Synchronize) => {
                trace!("Received fast-path synchronize");
            }
            Ok(FastPathUpdate::Orders(orders_update)) => {
                debug!(
                    "Received fast-path orders update: {} orders",
                    orders_update.number_orders
                );
                let orders = orders_update
                    .decode_orders(&mut self.drawing_order_state)
                    .map_err(|e| custom_err!("drawing orders", e))?;

                for order in orders {
                    if let Some(rect) = self.process_drawing_order(image, &order)? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            Err(e) => {
                // FIXME: This seems to be a way of special-handling the error case in FastPathUpdate::decode_cursor_with_code
                // to ignore the unsupported update PDUs, but this is a fragile logic and the rationale behind it is not
                // obvious.
                if let DecodeErrorKind::InvalidField { field, reason } = e.kind() {
                    warn!(field, reason, "Received invalid Fast-Path update");
                    processor_updates.push(UpdateKind::None);
                } else {
                    return Err(custom_err!("Fast-Path", e));
                }
            }
        };

        Ok(processor_updates)
    }

    /// Process a slow-path bitmap update using the same logic as fast-path.
    pub fn process_slow_path_bitmap(
        &mut self,
        image: &mut DecodedImage,
        data: &[u8],
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut cursor = ReadCursor::new(data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).map_err(|e| custom_err!("SlowPath", e))?;

        match update {
            ServerGraphicsUpdate::Bitmap(bitmap_update) => self.process_bitmap_update(image, bitmap_update),
            _ => Ok(vec![UpdateKind::None]),
        }
    }

    /// Process slow-path drawing orders using the same logic as fast-path.
    pub fn process_slow_path_orders(
        &mut self,
        image: &mut DecodedImage,
        number_orders: u16,
        order_data: &[u8],
    ) -> SessionResult<Vec<UpdateKind>> {
        use ironrdp_pdu::rdp::drawing_orders::decode_order;

        debug!("Processing {} slow-path drawing orders", number_orders);

        let mut cursor = ReadCursor::new(order_data);
        let mut processor_updates = Vec::new();

        for _ in 0..number_orders {
            if cursor.is_empty() {
                break;
            }

            match decode_order(&mut cursor, &mut self.drawing_order_state) {
                Ok(order) => {
                    if let Some(rect) = self.process_drawing_order(image, &order)? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
                Err(e) => {
                    warn!("Failed to decode drawing order: {}", e);
                    break;
                }
            }
        }

        Ok(processor_updates)
    }

    fn process_bitmap_update(
        &mut self,
        image: &mut DecodedImage,
        bitmap_update: BitmapUpdateData<'_>,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();
        let mut buf = Vec::new();
        let mut update_kind = UpdateKind::None;

        for update in bitmap_update.rectangles {
            trace!("{update:?}");
            buf.clear();

            // Bitmap data is either compressed or uncompressed, depending
            // on whether the BITMAP_COMPRESSION flag is present in the
            // flags field.
            let update_rectangle = if update
                .compression_flags
                .contains(ironrdp_pdu::bitmap::Compression::BITMAP_COMPRESSION)
            {
                if update.bits_per_pixel == 32 {
                    // Compressed bitmaps at a color depth of 32 bpp are compressed using RDP 6.0
                    // Bitmap Compression and stored inside an RDP 6.0 Bitmap Compressed Stream
                    // structure ([MS-RDPEGDI] section 2.2.2.5.1).
                    debug!("32 bpp compressed RDP6_BITMAP_STREAM");

                    match self.bitmap_stream_decoder.decode_bitmap_stream_to_rgb24(
                        update.bitmap_data,
                        &mut buf,
                        usize::from(update.width),
                        usize::from(update.height),
                    ) {
                        Ok(()) => image.apply_rgb24(&buf, &update.rectangle, true)?,
                        Err(err) => {
                            warn!("Invalid RDP6_BITMAP_STREAM: {err}");
                            update.rectangle.clone()
                        }
                    }
                } else {
                    // Compressed bitmaps not in 32 bpp format are compressed using Interleaved
                    // RLE and encapsulated in an RLE Compressed Bitmap Stream structure (section
                    // 2.2.9.1.1.3.1.2.4).
                    debug!(bpp = update.bits_per_pixel, "Non-32 bpp compressed RLE_BITMAP_STREAM",);

                    match ironrdp_graphics::rle::decompress(
                        update.bitmap_data,
                        &mut buf,
                        usize::from(update.width),
                        usize::from(update.height),
                        usize::from(update.bits_per_pixel),
                    ) {
                        Ok(RlePixelFormat::Rgb16) => image.apply_rgb16_bitmap(&buf, &update.rectangle)?,
                        Ok(RlePixelFormat::Rgb15) => image.apply_rgb15_bitmap(&buf, &update.rectangle)?,
                        Ok(RlePixelFormat::Rgb24) => {
                            image.apply_rgb24(&buf, &update.rectangle, true)?
                        }

                        Ok(RlePixelFormat::Rgb8) => {
                            image.apply_indexed8_bitmap(&buf, &self.color_palette, &update.rectangle)?
                        }

                        Err(e) => {
                            warn!("Invalid RLE-compressed bitmap: {e}");
                            update.rectangle.clone()
                        }
                    }
                }
            } else {
                // Uncompressed bitmap data is formatted as a bottom-up, left-to-right series of
                // pixels. Each pixel is a whole number of bytes. Each row contains a multiple of
                // four bytes (including up to three bytes of padding, as necessary).
                // [MS-RDPBCGR] 2.2.9.1.1.3.1.2.2
                trace!("Uncompressed raw bitmap");

                let bpp = usize::from(update.bits_per_pixel);
                let width = usize::from(update.width);
                let bytes_per_pixel = bpp.div_ceil(8);
                let row_bytes = width * bytes_per_pixel;
                let padded_row_bytes = (row_bytes + 3) & !3;

                if padded_row_bytes != row_bytes {
                    // Strip per-row padding before passing to the bitmap apply functions,
                    // which expect tightly packed pixel data.
                    buf.clear();
                    for row in update.bitmap_data.chunks(padded_row_bytes) {
                        let end = row_bytes.min(row.len());
                        buf.extend_from_slice(&row[..end]);
                    }

                    match update.bits_per_pixel {
                        8 => image.apply_indexed8_bitmap(&buf, &self.color_palette, &update.rectangle)?,
                        15 => image.apply_rgb15_bitmap(&buf, &update.rectangle)?,
                        16 => image.apply_rgb16_bitmap(&buf, &update.rectangle)?,
                        24 => image.apply_rgb24(&buf, &update.rectangle, true)?,
                        32 => image.apply_rgb32_bitmap(&buf, PixelFormat::BgrX32, &update.rectangle)?,
                        _ => {
                            warn!("Unsupported uncompressed bitmap depth: {bpp} bpp");
                            update.rectangle.clone()
                        }
                    }
                } else {
                    match update.bits_per_pixel {
                        8 => image.apply_indexed8_bitmap(update.bitmap_data, &self.color_palette, &update.rectangle)?,
                        15 => image.apply_rgb15_bitmap(update.bitmap_data, &update.rectangle)?,
                        16 => image.apply_rgb16_bitmap(update.bitmap_data, &update.rectangle)?,
                        24 => image.apply_rgb24(update.bitmap_data, &update.rectangle, true)?,
                        32 => image.apply_rgb32_bitmap(update.bitmap_data, PixelFormat::BgrX32, &update.rectangle)?,
                        unsupported => {
                            warn!("Unsupported raw bitmap bpp: {unsupported}");
                            update.rectangle.clone()
                        }
                    }
                }
            };

            match update_kind {
                UpdateKind::Region(current) => update_kind = UpdateKind::Region(current.union(&update_rectangle)),
                _ => update_kind = UpdateKind::Region(update_rectangle),
            }
        }

        if !matches!(update_kind, UpdateKind::None) {
            processor_updates.push(update_kind);
        }

        Ok(processor_updates)
    }

    pub fn process_pointer_update(
        &mut self,
        image: &mut DecodedImage,
        update: PointerUpdateData<'_>,
    ) -> SessionResult<Vec<UpdateKind>> {
        let mut processor_updates = Vec::new();

        if !self.enable_server_pointer {
            return Ok(processor_updates);
        }

        let bitmap_target = if self.pointer_software_rendering {
            PointerBitmapTarget::Software
        } else {
            PointerBitmapTarget::Accelerated
        };

        match update {
            PointerUpdateData::SetHidden => {
                processor_updates.push(UpdateKind::PointerHidden);
                if self.pointer_software_rendering && !self.use_system_pointer {
                    self.use_system_pointer = true;
                    if let Some(rect) = image.hide_pointer()? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            PointerUpdateData::SetDefault => {
                processor_updates.push(UpdateKind::PointerDefault);
                if self.pointer_software_rendering && !self.use_system_pointer {
                    self.use_system_pointer = true;
                    if let Some(rect) = image.hide_pointer()? {
                        processor_updates.push(UpdateKind::Region(rect));
                    }
                }
            }
            PointerUpdateData::SetPosition(position) => {
                if self.use_system_pointer || !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerPosition {
                        x: position.x,
                        y: position.y,
                    });
                } else if let Some(rect) = image.move_pointer(position.x, position.y)? {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
            PointerUpdateData::Color(pointer) => {
                let cache_index = pointer.cache_index;
                let pointer_palette = self.color_palette.to_pointer_palette();

                let decoded_pointer = Arc::new(
                    DecodedPointer::decode_color_pointer_attribute_with_palette(
                        &pointer,
                        bitmap_target,
                        Some(&pointer_palette),
                    )
                    .map_err(|e| SessionError::custom("failed to decode color pointer attribute", e))?,
                );

                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) = image.update_pointer(decoded_pointer)? {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
            PointerUpdateData::Cached(cached) => {
                let cache_index = cached.cache_index;

                if let Some(cached_pointer) = self.pointer_cache.get(usize::from(cache_index)) {
                    processor_updates.push(UpdateKind::PointerHidden);
                    self.use_system_pointer = false;
                    if !self.pointer_software_rendering {
                        processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&cached_pointer)));
                    } else if let Some(rect) = image.update_pointer(cached_pointer)? {
                        processor_updates.push(UpdateKind::Region(rect));
                    } else {
                        if let Some(rect) = image.show_pointer()? {
                            processor_updates.push(UpdateKind::Region(rect));
                        }
                    }
                } else {
                    warn!("Cached pointer not found {}", cache_index);
                }
            }
            PointerUpdateData::New(pointer) => {
                let cache_index = pointer.color_pointer.cache_index;
                let pointer_palette = self.color_palette.to_pointer_palette();

                let decoded_pointer = Arc::new(
                    DecodedPointer::decode_pointer_attribute_with_palette(
                        &pointer,
                        bitmap_target,
                        Some(&pointer_palette),
                    )
                    .map_err(|e| SessionError::custom("failed to decode pointer attribute", e))?,
                );

                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) = image.update_pointer(decoded_pointer)? {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
            PointerUpdateData::Large(pointer) => {
                let cache_index = pointer.cache_index;
                let pointer_palette = self.color_palette.to_pointer_palette();

                let decoded_pointer: Arc<DecodedPointer> = Arc::new(
                    DecodedPointer::decode_large_pointer_attribute_with_palette(
                        &pointer,
                        bitmap_target,
                        Some(&pointer_palette),
                    )
                    .map_err(|e| SessionError::custom("failed to decode large pointer attribute", e))?,
                );

                let _ = self
                    .pointer_cache
                    .insert(usize::from(cache_index), Arc::clone(&decoded_pointer));

                if !self.pointer_software_rendering {
                    processor_updates.push(UpdateKind::PointerBitmap(Arc::clone(&decoded_pointer)));
                } else if let Some(rect) = image.update_pointer(decoded_pointer)? {
                    processor_updates.push(UpdateKind::Region(rect));
                }
            }
        };

        Ok(processor_updates)
    }

    fn process_surface_commands(
        &mut self,
        image: &mut DecodedImage,
        output: &mut WriteBuf,
        surface_commands: Vec<SurfaceCommand<'_>>,
        processor_updates: &mut Vec<UpdateKind>,
    ) -> SessionResult<InclusiveRectangle> {
        let mut update_rectangle = None;

        for command in surface_commands {
            match command {
                SurfaceCommand::SetSurfaceBits(bits) | SurfaceCommand::StreamSurfaceBits(bits) => {
                    let codec_id = CodecId::from_u8(bits.extended_bitmap_data.codec_id).ok_or_else(|| {
                        reason_err!(
                            "Fast-Path",
                            "unexpected codec ID: {:x}",
                            bits.extended_bitmap_data.codec_id
                        )
                    })?;

                    trace!(?codec_id, "Surface bits");

                    let destination = bits.destination;
                    // TODO(@pacmancoder): Correct rectangle conversion logic should
                    // be revisited when `rectangle_processing.rs` from
                    // `ironrdp-graphics` will be refactored to use generic `Rectangle`
                    // trait instead of hardcoded `InclusiveRectangle`.
                    let destination = InclusiveRectangle {
                        left: destination.left,
                        top: destination.top,
                        right: destination.right - 1,
                        bottom: destination.bottom - 1,
                    };
                    match codec_id {
                        CODEC_ID_NONE => {
                            let ext_data = bits.extended_bitmap_data;
                            let rectangle = match ext_data.bpp {
                                8 => {
                                    image.apply_indexed8_bitmap(ext_data.data, &self.color_palette, &destination)?
                                }
                                15 => image.apply_rgb15_bitmap(ext_data.data, &destination)?,
                                16 => image.apply_rgb16_bitmap(ext_data.data, &destination)?,
                                24 => image.apply_bgr24_bitmap(ext_data.data, &destination)?,
                                32 => image.apply_rgb32_bitmap(ext_data.data, PixelFormat::BgrX32, &destination)?,
                                bpp => {
                                    warn!("Unsupported surface CODEC_ID_NONE bpp: {bpp}");
                                    continue;
                                }
                            };
                            update_rectangle = update_rectangle
                                .map(|rect: InclusiveRectangle| rect.union(&rectangle))
                                .or(Some(rectangle));
                        }
                        CODEC_ID_REMOTEFX => {
                            let mut data = ReadCursor::new(bits.extended_bitmap_data.data);
                            while !data.is_empty() {
                                let (_frame_id, rectangle) = self.rfx_handler.decode(image, &destination, &mut data)?;
                                update_rectangle = update_rectangle
                                    .map(|rect: InclusiveRectangle| rect.union(&rectangle))
                                    .or(Some(rectangle));
                            }
                        }
                        #[cfg(feature = "qoi")]
                        ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOI => {
                            qoi_apply(
                                image,
                                destination,
                                bits.extended_bitmap_data.data,
                                &mut update_rectangle,
                            )?;
                        }
                        #[cfg(feature = "qoiz")]
                        ironrdp_pdu::rdp::capability_sets::CODEC_ID_QOIZ => {
                            let compressed = &bits.extended_bitmap_data.data;
                            let mut input = zstd_safe::InBuffer::around(compressed);
                            let mut data = vec![0; compressed.len() * 4];
                            let mut pos = 0;
                            loop {
                                let mut output = zstd_safe::OutBuffer::around_pos(data.as_mut_slice(), pos);
                                self.zdctx
                                    .decompress_stream(&mut output, &mut input)
                                    .map_err(zstd_safe::get_error_name)
                                    .map_err(|e| reason_err!("zstd", "{}", e))?;
                                pos = output.pos();
                                if pos == output.capacity() {
                                    data.resize(data.capacity() * 2, 0);
                                } else {
                                    break;
                                }
                            }

                            qoi_apply(image, destination, &data, &mut update_rectangle)?;
                        }
                        _ => {
                            warn!("Unsupported codec ID: {}", bits.extended_bitmap_data.codec_id);
                        }
                    }
                }
                SurfaceCommand::FrameMarker(marker) => {
                    let frame_id = marker.frame_id.unwrap_or(0);
                    trace!("Frame marker: action {:?} with ID #{}", marker.frame_action, frame_id);

                    self.marker_processor.process(&marker, output)?;
                }
            }
        }

        Ok(update_rectangle.unwrap_or_else(InclusiveRectangle::empty))
    }
}

#[cfg(feature = "qoi")]
fn qoi_apply(
    image: &mut DecodedImage,
    destination: InclusiveRectangle,
    data: &[u8],
    update_rectangle: &mut Option<InclusiveRectangle>,
) -> SessionResult<()> {
    let (header, decoded) = qoi::decode_to_vec(data).map_err(|e| reason_err!("QOI decode", "{}", e))?;

    // Guard against a decoded buffer that doesn't match the destination
    // rectangle. `apply_rgb24`/`apply_rgba32` derive the row count from the
    // decoded length, and the only bounds check downstream (`rect_fits`)
    // validates the rectangle against the image, not the buffer against the
    // rectangle. A malformed/oversized QOI payload would otherwise drive the
    // per-row index past `self.data` and panic (client-side DoS).
    let channels = match header.channels {
        qoi::Channels::Rgb => 3,
        qoi::Channels::Rgba => 4,
    };
    let expected = usize::from(destination.width()) * usize::from(destination.height()) * channels;
    if decoded.len() != expected {
        return Err(reason_err!(
            "QOI decode",
            "decoded {} bytes, expected {} for {}x{} ({} channels)",
            decoded.len(),
            expected,
            destination.width(),
            destination.height(),
            channels
        ));
    }

    let rectangle = match header.channels {
        qoi::Channels::Rgb => image.apply_rgb24(&decoded, &destination, false)?,
        qoi::Channels::Rgba => image.apply_rgba32(&decoded, &destination, false)?,
    };

    *update_rectangle = update_rectangle
        .as_ref()
        .map(|rect: &InclusiveRectangle| rect.union(&rectangle))
        .or(Some(rectangle));
    Ok(())
}

pub struct ProcessorBuilder {
    pub io_channel_id: u16,
    pub user_channel_id: u16,
    pub share_id: u32,
    /// Ignore server pointer updates.
    pub enable_server_pointer: bool,
    /// Use software rendering mode for pointer bitmap generation. When this option is active,
    /// `UpdateKind::PointerBitmap` will not be generated. Remote pointer will be drawn
    /// via software rendering on top of the output image.
    pub pointer_software_rendering: bool,
    /// Bulk decompressor for server-to-client compressed PDUs.
    /// `None` when compression was not negotiated.
    pub bulk_decompressor: Option<BulkCompressor>,
}

impl ProcessorBuilder {
    pub fn build(self) -> Processor {
        Processor {
            complete_data: CompleteData::new(),
            rfx_handler: rfx::DecodingContext::new(),
            marker_processor: FrameMarkerProcessor::new(self.user_channel_id, self.io_channel_id, self.share_id),
            bitmap_stream_decoder: BitmapStreamDecoder::default(),
            pointer_cache: PointerCache::default(),
            use_system_pointer: true,
            mouse_pos_update: None,
            enable_server_pointer: self.enable_server_pointer,
            pointer_software_rendering: self.pointer_software_rendering,
            bulk_decompressor: self.bulk_decompressor,
            color_palette: ColorPalette::new(),
            drawing_order_state: DrawingOrderState::new(),
            bitmap_cache: BitmapCache::new(),
            #[cfg(feature = "qoiz")]
            zdctx: zstd_safe::DCtx::default(),
        }
    }
}

#[derive(Debug, PartialEq)]
struct CompleteData {
    fragmented_data: Option<Vec<u8>>,
}

impl CompleteData {
    fn new() -> Self {
        Self { fragmented_data: None }
    }

    fn process_data(&mut self, data: &[u8], fragmentation: Fragmentation) -> Option<Vec<u8>> {
        match fragmentation {
            Fragmentation::Single => {
                self.check_data_is_empty();

                Some(data.to_vec())
            }
            Fragmentation::First => {
                self.check_data_is_empty();

                self.fragmented_data = Some(data.to_vec());

                None
            }
            Fragmentation::Next => {
                self.append_data(data);

                None
            }
            Fragmentation::Last => {
                self.append_data(data);

                self.fragmented_data.take()
            }
        }
    }

    fn check_data_is_empty(&mut self) {
        if self.fragmented_data.is_some() {
            warn!("Skipping pending Fast-Path Update internal multiple elements data");
            self.fragmented_data = None;
        }
    }

    fn append_data(&mut self, data: &[u8]) {
        if let Some(fragmented_data) = self.fragmented_data.as_mut() {
            fragmented_data.extend_from_slice(data);
        } else {
            warn!("Got unexpected Next fragmentation PDU without prior First fragmentation PDU");
        }
    }
}

struct FrameMarkerProcessor {
    user_channel_id: u16,
    io_channel_id: u16,
    share_id: u32,
}

impl FrameMarkerProcessor {
    fn new(user_channel_id: u16, io_channel_id: u16, share_id: u32) -> Self {
        Self {
            user_channel_id,
            io_channel_id,
            share_id,
        }
    }

    fn process(&mut self, marker: &FrameMarkerPdu, output: &mut WriteBuf) -> SessionResult<()> {
        match marker.frame_action {
            FrameAction::Begin => Ok(()),
            FrameAction::End => {
                ironrdp_pdu::rdp::headers::encode_share_data(
                    self.user_channel_id,
                    self.io_channel_id,
                    self.share_id,
                    ShareDataPdu::FrameAcknowledge(FrameAcknowledgePdu {
                        frame_id: marker.frame_id.unwrap_or(0),
                    }),
                    output,
                )
                .map_err(SessionError::encode)?;

                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_processor() -> Processor {
        ProcessorBuilder {
            io_channel_id: 1003,
            user_channel_id: 1004,
            share_id: 0,
            enable_server_pointer: false,
            pointer_software_rendering: false,
            bulk_decompressor: None,
        }
        .build()
    }

    fn create_test_image() -> DecodedImage {
        DecodedImage::new(PixelFormat::RgbA32, 800, 600)
    }

    #[test]
    fn test_process_slow_path_bitmap_synchronize() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Synchronize update type
        let data = [0x03, 0x00];
        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        // Synchronize should return UpdateKind::None
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], UpdateKind::None));
    }

    #[test]
    fn test_process_slow_path_bitmap_orders() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Orders update type (not implemented, should return None)
        let data = [
            0x00, 0x00, // updateType = Orders
            0x00, 0x00, // pad2OctetsA
            0x01, 0x00, // numberOrders = 1
            0x00, 0x00, // pad2OctetsB
        ];
        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], UpdateKind::None));
    }

    #[test]
    fn test_process_slow_path_bitmap_palette() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Palette update type (not implemented, should return None)
        let data = [
            0x02, 0x00, // updateType = Palette
            0x00, 0x00, // pad2Octets
            0x01, 0x00, 0x00, 0x00, // numberColors = 1
            0xFF, 0x00, 0x00, // One red entry
        ];
        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        assert!(matches!(updates[0], UpdateKind::None));
    }

    #[test]
    fn test_process_slow_path_bitmap_with_uncompressed_16bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Bitmap update with uncompressed 16bpp data
        // Create a 4x4 pixel rectangle
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            // Rectangle data (TS_BITMAP_DATA)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x03, 0x00, // destRight = 3
            0x03, 0x00, // destBottom = 3
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x10, 0x00, // bitsPerPixel = 16
            0x00, 0x00, // flags = 0 (uncompressed)
            0x20, 0x00, // bitmapLength = 32 (4x4 pixels * 2 bytes)
        ];
        // Add 32 bytes of uncompressed 16bpp bitmap data
        data.extend_from_slice(&[0x00u8; 32]);

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        // Should have a region update
        if let UpdateKind::Region(rect) = &updates[0] {
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 3);
            assert_eq!(rect.bottom, 3);
        } else {
            panic!("Expected Region update, got {:?}", updates[0]);
        }
    }

    #[test]
    fn test_process_slow_path_bitmap_invalid_data() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Too short data
        let data = [0x01];
        let result = processor.process_slow_path_bitmap(&mut image, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_slow_path_bitmap_unknown_type() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Unknown update type
        let data = [0xFF, 0xFF];
        let result = processor.process_slow_path_bitmap(&mut image, &data);
        assert!(result.is_err());
    }

    #[test]
    fn test_process_bitmap_update_multiple_rectangles() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Two rectangles in one bitmap update
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x02, 0x00, // Number of rectangles = 2
            // First rectangle (4x4 at 0,0)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x03, 0x00, // destRight = 3
            0x03, 0x00, // destBottom = 3
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x10, 0x00, // bitsPerPixel = 16
            0x00, 0x00, // flags = 0 (uncompressed)
            0x20, 0x00, // bitmapLength = 32
        ];
        data.extend_from_slice(&[0x00u8; 32]); // First rectangle bitmap data

        // Second rectangle (4x4 at 100,100)
        data.extend_from_slice(&[
            0x64, 0x00, // destLeft = 100
            0x64, 0x00, // destTop = 100
            0x67, 0x00, // destRight = 103
            0x67, 0x00, // destBottom = 103
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x10, 0x00, // bitsPerPixel = 16
            0x00, 0x00, // flags = 0 (uncompressed)
            0x20, 0x00, // bitmapLength = 32
        ]);
        data.extend_from_slice(&[0x00u8; 32]); // Second rectangle bitmap data

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        // Should have a union of both rectangles
        if let UpdateKind::Region(rect) = &updates[0] {
            // Union should cover from (0,0) to (103,103)
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 103);
            assert_eq!(rect.bottom, 103);
        } else {
            panic!("Expected Region update, got {:?}", updates[0]);
        }
    }

    #[test]
    fn test_process_slow_path_bitmap_with_uncompressed_15bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Bitmap update with uncompressed 15bpp data
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            // Rectangle data (TS_BITMAP_DATA)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x03, 0x00, // destRight = 3
            0x03, 0x00, // destBottom = 3
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x0F, 0x00, // bitsPerPixel = 15
            0x00, 0x00, // flags = 0 (uncompressed)
            0x20, 0x00, // bitmapLength = 32 (4x4 pixels * 2 bytes)
        ];
        // Add 32 bytes of uncompressed 15bpp bitmap data (all white = 0x7FFF)
        for _ in 0..16 {
            data.extend_from_slice(&[0xFF, 0x7F]); // Little-endian 0x7FFF
        }

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        if let UpdateKind::Region(rect) = &updates[0] {
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 3);
            assert_eq!(rect.bottom, 3);
        } else {
            panic!("Expected Region update, got {:?}", updates[0]);
        }
    }

    #[test]
    fn test_process_slow_path_bitmap_with_uncompressed_24bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Bitmap update with uncompressed 24bpp data
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            // Rectangle data (TS_BITMAP_DATA)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x03, 0x00, // destRight = 3
            0x03, 0x00, // destBottom = 3
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x18, 0x00, // bitsPerPixel = 24
            0x00, 0x00, // flags = 0 (uncompressed)
            0x30, 0x00, // bitmapLength = 48 (4x4 pixels * 3 bytes)
        ];
        // Add 48 bytes of uncompressed 24bpp bitmap data (red pixels in BGR)
        for _ in 0..16 {
            data.extend_from_slice(&[0x00, 0x00, 0xFF]); // BGR: Blue=0, Green=0, Red=255
        }

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        if let UpdateKind::Region(rect) = &updates[0] {
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 3);
            assert_eq!(rect.bottom, 3);
        } else {
            panic!("Expected Region update, got {:?}", updates[0]);
        }
    }

    #[test]
    fn test_process_slow_path_bitmap_with_uncompressed_32bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Bitmap update with uncompressed 32bpp data
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            // Rectangle data (TS_BITMAP_DATA)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x03, 0x00, // destRight = 3
            0x03, 0x00, // destBottom = 3
            0x04, 0x00, // width = 4
            0x04, 0x00, // height = 4
            0x20, 0x00, // bitsPerPixel = 32
            0x00, 0x00, // flags = 0 (uncompressed)
            0x40, 0x00, // bitmapLength = 64 (4x4 pixels * 4 bytes)
        ];
        // Add 64 bytes of uncompressed 32bpp bitmap data (green pixels in BGRX)
        for _ in 0..16 {
            data.extend_from_slice(&[0x00, 0xFF, 0x00, 0x00]); // BGRX: Blue=0, Green=255, Red=0, X=0
        }

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);
        if let UpdateKind::Region(rect) = &updates[0] {
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 3);
            assert_eq!(rect.bottom, 3);
        } else {
            panic!("Expected Region update, got {:?}", updates[0]);
        }
    }

    #[test]
    fn test_process_slow_path_bitmap_verifies_pixel_data_15bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Create a 2x2 bitmap with specific colors
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x01, 0x00, // destRight = 1
            0x01, 0x00, // destBottom = 1
            0x02, 0x00, // width = 2
            0x02, 0x00, // height = 2
            0x0F, 0x00, // bitsPerPixel = 15
            0x00, 0x00, // flags = 0 (uncompressed)
            0x08, 0x00, // bitmapLength = 8 (2x2 pixels * 2 bytes)
        ];
        // Bottom row first (bitmap is bottom-up): pure red (0x7C00) and pure green (0x03E0)
        data.extend_from_slice(&[0x00, 0x7C]); // Red (0x7C00 in LE)
        data.extend_from_slice(&[0xE0, 0x03]); // Green (0x03E0 in LE)
                                               // Top row: pure blue (0x001F) and white (0x7FFF)
        data.extend_from_slice(&[0x1F, 0x00]); // Blue (0x001F in LE)
        data.extend_from_slice(&[0xFF, 0x7F]); // White (0x7FFF in LE)

        let result = processor.process_slow_path_bitmap(&mut image, &data);
        assert!(result.is_ok());

        // Verify the pixel data in the image
        // Note: Image is stored as RGBA32, top-to-bottom
        let data = image.data();
        let stride = image.stride();

        // Top-left pixel should be blue (after flip)
        let idx = 0;
        assert_eq!(data[idx], 0); // R
        assert_eq!(data[idx + 1], 0); // G
        assert_eq!(data[idx + 2], 255); // B
        assert_eq!(data[idx + 3], 255); // A

        // Top-right pixel should be white
        let idx = 4;
        assert_eq!(data[idx], 255); // R
        assert_eq!(data[idx + 1], 255); // G
        assert_eq!(data[idx + 2], 255); // B
        assert_eq!(data[idx + 3], 255); // A

        // Bottom-left pixel should be red (after flip)
        let idx = stride;
        assert_eq!(data[idx], 255); // R
        assert_eq!(data[idx + 1], 0); // G
        assert_eq!(data[idx + 2], 0); // B
        assert_eq!(data[idx + 3], 255); // A

        // Bottom-right pixel should be green
        let idx = stride + 4;
        assert_eq!(data[idx], 0); // R
        assert_eq!(data[idx + 1], 255); // G
        assert_eq!(data[idx + 2], 0); // B
        assert_eq!(data[idx + 3], 255); // A
    }

    #[test]
    fn test_color_palette_default() {
        let palette = ColorPalette::new();
        // Default palette should be grayscale
        assert_eq!(palette.get(0), [0, 0, 0]); // Black
        assert_eq!(palette.get(128), [128, 128, 128]); // Mid gray
        assert_eq!(palette.get(255), [255, 255, 255]); // White
    }

    #[test]
    fn test_color_palette_update() {
        use ironrdp_pdu::rdp::server_graphics_update::{PaletteEntry, PaletteUpdate};

        let mut palette = ColorPalette::new();

        // Create a palette update with specific colors
        let update = PaletteUpdate {
            entries: vec![
                PaletteEntry {
                    red: 255,
                    green: 0,
                    blue: 0,
                }, // Index 0: Red
                PaletteEntry {
                    red: 0,
                    green: 255,
                    blue: 0,
                }, // Index 1: Green
                PaletteEntry {
                    red: 0,
                    green: 0,
                    blue: 255,
                }, // Index 2: Blue
            ],
        };

        palette.update(&update);

        assert_eq!(palette.get(0), [255, 0, 0]); // Red
        assert_eq!(palette.get(1), [0, 255, 0]); // Green
        assert_eq!(palette.get(2), [0, 0, 255]); // Blue
                                                 // Remaining entries should still be grayscale
        assert_eq!(palette.get(3), [3, 3, 3]);
        assert_eq!(palette.get(255), [255, 255, 255]);
    }

    #[test]
    fn test_process_slow_path_bitmap_with_uncompressed_8bpp() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // First, set up a palette with specific colors
        use ironrdp_pdu::rdp::server_graphics_update::{PaletteEntry, PaletteUpdate};
        let palette_update = PaletteUpdate {
            entries: vec![
                PaletteEntry {
                    red: 255,
                    green: 0,
                    blue: 0,
                }, // Index 0: Red
                PaletteEntry {
                    red: 0,
                    green: 255,
                    blue: 0,
                }, // Index 1: Green
                PaletteEntry {
                    red: 0,
                    green: 0,
                    blue: 255,
                }, // Index 2: Blue
                PaletteEntry {
                    red: 255,
                    green: 255,
                    blue: 255,
                }, // Index 3: White
            ],
        };
        processor.update_palette(&palette_update);

        // Bitmap update with uncompressed 8bpp data
        let mut data = vec![
            0x01, 0x00, // updateType = Bitmap

            0x01, 0x00, // Number of rectangles = 1
            // Rectangle data (TS_BITMAP_DATA)
            0x00, 0x00, // destLeft = 0
            0x00, 0x00, // destTop = 0
            0x01, 0x00, // destRight = 1
            0x01, 0x00, // destBottom = 1
            0x02, 0x00, // width = 2
            0x02, 0x00, // height = 2
            0x08, 0x00, // bitsPerPixel = 8
            0x00, 0x00, // flags = 0 (uncompressed)
            0x08, 0x00, // bitmapLength = 8 (2 rows * 4 padded bytes per row)
        ];
        // 8bpp pixel data: palette indices, rows padded to 4-byte boundary.
        // Bottom row first (RDP bitmaps are bottom-up).
        data.extend_from_slice(&[0, 1, 0, 0]); // Bottom row: Red, Green, pad, pad
        data.extend_from_slice(&[2, 3, 0, 0]); // Top row:    Blue, White, pad, pad

        let result = processor.process_slow_path_bitmap(&mut image, &data);

        assert!(result.is_ok());
        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);

        // Verify the pixel colors in the image
        let image_data = image.data();
        let stride = image.stride();

        // Top-left should be Blue (index 2, after flip)
        assert_eq!(image_data[0], 0); // R
        assert_eq!(image_data[1], 0); // G
        assert_eq!(image_data[2], 255); // B
        assert_eq!(image_data[3], 255); // A

        // Top-right should be White (index 3)
        assert_eq!(image_data[4], 255); // R
        assert_eq!(image_data[5], 255); // G
        assert_eq!(image_data[6], 255); // B
        assert_eq!(image_data[7], 255); // A

        // Bottom-left should be Red (index 0, after flip)
        let idx = stride;
        assert_eq!(image_data[idx], 255); // R
        assert_eq!(image_data[idx + 1], 0); // G
        assert_eq!(image_data[idx + 2], 0); // B
        assert_eq!(image_data[idx + 3], 255); // A

        // Bottom-right should be Green (index 1)
        let idx = stride + 4;
        assert_eq!(image_data[idx], 0); // R
        assert_eq!(image_data[idx + 1], 255); // G
        assert_eq!(image_data[idx + 2], 0); // B
        assert_eq!(image_data[idx + 3], 255); // A
    }

    #[test]
    fn test_processor_palette_update() {
        use ironrdp_pdu::rdp::server_graphics_update::{PaletteEntry, PaletteUpdate};

        let mut processor = create_test_processor();

        // Initial palette should be grayscale
        assert_eq!(processor.palette().get(0), [0, 0, 0]);
        assert_eq!(processor.palette().get(255), [255, 255, 255]);

        // Update palette
        let update = PaletteUpdate {
            entries: vec![PaletteEntry {
                red: 100,
                green: 150,
                blue: 200,
            }],
        };
        processor.update_palette(&update);

        // First entry should be updated
        assert_eq!(processor.palette().get(0), [100, 150, 200]);
        // Other entries should remain grayscale
        assert_eq!(processor.palette().get(1), [1, 1, 1]);
    }

    #[test]
    fn test_process_slow_path_orders_dstblt() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // DstBlt order with BLACKNESS ROP (0x00)
        // Order format: controlFlags, orderType, fieldFlags, fields...
        let order_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x00, // orderType: DstBlt
            0x1F, // field flags: all 5 fields
            0x0A, 0x00, // x = 10
            0x14, 0x00, // y = 20
            0x64, 0x00, // width = 100
            0x32, 0x00, // height = 50
            0x00, // rop = BLACKNESS
        ];

        let result = processor.process_slow_path_orders(&mut image, 1, &order_data);
        assert!(result.is_ok());

        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);

        if let UpdateKind::Region(rect) = &updates[0] {
            assert_eq!(rect.left, 10);
            assert_eq!(rect.top, 20);
            assert_eq!(rect.right, 109);
            assert_eq!(rect.bottom, 69);
        } else {
            panic!("Expected Region update");
        }

        // Verify the rectangle was filled with black
        let image_data = image.data();
        let stride = image.stride();
        let idx = (20 * stride) + (10 * 4);
        assert_eq!(image_data[idx], 0); // R
        assert_eq!(image_data[idx + 1], 0); // G
        assert_eq!(image_data[idx + 2], 0); // B
        assert_eq!(image_data[idx + 3], 255); // A
    }

    #[test]
    fn test_process_slow_path_orders_opaque_rect() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // OpaqueRect order
        let order_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x0A, // orderType: OpaqueRect
            0x7F, // field flags: all 7 fields
            0x0A, 0x00, // x = 10
            0x0A, 0x00, // y = 10
            0x14, 0x00, // width = 20
            0x14, 0x00, // height = 20
            0xFF, // Red
            0x80, // Green
            0x00, // Blue
        ];

        let result = processor.process_slow_path_orders(&mut image, 1, &order_data);
        assert!(result.is_ok());

        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);

        // Verify the rectangle was filled with the color
        let image_data = image.data();
        let stride = image.stride();
        let idx = (10 * stride) + (10 * 4);
        assert_eq!(image_data[idx], 255); // R
        assert_eq!(image_data[idx + 1], 128); // G
        assert_eq!(image_data[idx + 2], 0); // B
        assert_eq!(image_data[idx + 3], 255); // A
    }

    #[test]
    fn test_process_slow_path_orders_multiple() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // Two DstBlt orders
        let order_data = [
            // First order: DstBlt with BLACKNESS
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x00, // orderType: DstBlt
            0x1F, // field flags: all 5 fields
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0x00, // rop = BLACKNESS
            // Second order: DstBlt with WHITENESS (same type, no TYPE_CHANGE)
            0x01, // controlFlags: STANDARD only
            0x1F, // field flags: all 5 fields
            0x14, 0x00, // x = 20
            0x14, 0x00, // y = 20
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0xFF, // rop = WHITENESS
        ];

        let result = processor.process_slow_path_orders(&mut image, 2, &order_data);
        assert!(result.is_ok());

        let updates = result.unwrap();
        assert_eq!(updates.len(), 2);
    }

    #[test]
    fn test_process_slow_path_orders_empty() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        let result = processor.process_slow_path_orders(&mut image, 0, &[]);
        assert!(result.is_ok());

        let updates = result.unwrap();
        assert!(updates.is_empty());
    }

    #[test]
    fn test_process_slow_path_orders_line_to() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // LineTo order
        let order_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x09, // orderType: LineTo
            0xFF, 0x03, // field flags: all 10 fields
            0x00, 0x00, // BackMode
            0x00, 0x00, // start_x = 0
            0x00, 0x00, // start_y = 0
            0x32, 0x00, // end_x = 50
            0x32, 0x00, // end_y = 50
            0x00, 0x00, 0x00, // BackColor (BGR)
            0x01, // ROP2
            0x00, // PenStyle
            0x01, // PenWidth
            0x00, 0x00, 0xFF, // PenColor (BGR = red)
        ];

        let result = processor.process_slow_path_orders(&mut image, 1, &order_data);
        assert!(result.is_ok());

        let updates = result.unwrap();
        assert_eq!(updates.len(), 1);

        if let UpdateKind::Region(rect) = &updates[0] {
            // Line from (0,0) to (50,50)
            assert_eq!(rect.left, 0);
            assert_eq!(rect.top, 0);
            assert_eq!(rect.right, 50);
            assert_eq!(rect.bottom, 50);
        } else {
            panic!("Expected Region update");
        }
    }

    #[test]
    fn test_process_slow_path_orders_scrblt() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // First, fill a region with red
        let opaque_rect_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x0A, // orderType: OpaqueRect
            0x7F, // field flags: all 7 fields
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0xFF, // Red
            0x00, // Green
            0x00, // Blue
        ];

        processor
            .process_slow_path_orders(&mut image, 1, &opaque_rect_data)
            .unwrap();

        // Now copy it to another location with ScrBlt
        let scrblt_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x02, // orderType: ScrBlt
            0x7F, // field flags: all 7 fields
            0x14, 0x00, // x = 20 (destination)
            0x14, 0x00, // y = 20 (destination)
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0xCC, // rop = SRCCOPY
            0x00, 0x00, // src_x = 0
            0x00, 0x00, // src_y = 0
        ];

        let result = processor.process_slow_path_orders(&mut image, 1, &scrblt_data);
        assert!(result.is_ok());

        // Verify the copy worked
        let image_data = image.data();
        let stride = image.stride();

        // Check destination pixel at (20, 20) - should be red
        let idx = (20 * stride) + (20 * 4);
        assert_eq!(image_data[idx], 255); // R
        assert_eq!(image_data[idx + 1], 0); // G
        assert_eq!(image_data[idx + 2], 0); // B
        assert_eq!(image_data[idx + 3], 255); // A
    }

    #[test]
    fn test_process_slow_path_orders_dstinvert() {
        let mut processor = create_test_processor();
        let mut image = create_test_image();

        // First, fill a region with a known color
        let opaque_rect_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x0A, // orderType: OpaqueRect
            0x7F, // field flags: all 7 fields
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0x64, // Red = 100
            0x96, // Green = 150
            0xC8, // Blue = 200
        ];

        processor
            .process_slow_path_orders(&mut image, 1, &opaque_rect_data)
            .unwrap();

        // Now invert with DstBlt DSTINVERT
        let dstblt_data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x00, // orderType: DstBlt
            0x1F, // field flags: all 5 fields
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x0A, 0x00, // width = 10
            0x0A, 0x00, // height = 10
            0x55, // rop = DSTINVERT
        ];

        let result = processor.process_slow_path_orders(&mut image, 1, &dstblt_data);
        assert!(result.is_ok());

        // Verify the invert worked: 100->155, 150->105, 200->55
        let image_data = image.data();
        assert_eq!(image_data[0], 155); // R: 255 - 100
        assert_eq!(image_data[1], 105); // G: 255 - 150
        assert_eq!(image_data[2], 55); // B: 255 - 200
        assert_eq!(image_data[3], 255); // A unchanged
    }
}

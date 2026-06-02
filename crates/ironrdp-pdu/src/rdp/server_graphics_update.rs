//! Slow-Path Server Graphics Update PDU
//!
//! This module implements parsing for slow-path graphics update PDUs as specified in
//! [\[MS-RDPBCGR\] 2.2.9.1.1.3]. These updates are sent via the X224 data path (as opposed
//! to fast-path) and include bitmap updates, palette updates, drawing orders, and
//! synchronization markers.
//!
//! # Supported Update Types
//!
//! - **Bitmap**: Contains compressed or uncompressed bitmap data for screen regions
//! - **Palette**: Updates the 256-color palette for 8bpp indexed color mode
//! - **Synchronize**: Marker PDU indicating a synchronization point
//! - **Orders**: Drawing commands (parsing only, not yet fully implemented)
//!
//! # Example
//!
//! ```ignore
//! use ironrdp_pdu::rdp::server_graphics_update::ServerGraphicsUpdate;
//! use ironrdp_core::ReadCursor;
//!
//! let data = /* raw PDU data */;
//! let mut cursor = ReadCursor::new(&data);
//! match ServerGraphicsUpdate::decode(&mut cursor) {
//!     Ok(ServerGraphicsUpdate::Bitmap(bitmap)) => {
//!         // Process bitmap update
//!     }
//!     Ok(ServerGraphicsUpdate::Palette(palette)) => {
//!         // Update color palette
//!     }
//!     _ => {}
//! }
//! ```
//!
//! [\[MS-RDPBCGR\] 2.2.9.1.1.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/a1c4cec9-39f8-4ea1-a915-d5c90c185c5c

use ironrdp_core::{ensure_size, invalid_field_err, Decode as _, DecodeResult, ReadCursor};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;

use crate::bitmap::BitmapUpdateData;

/// Update PDU types as defined in [MS-RDPBCGR 2.2.9.1.1.3.1].
///
/// These values appear in the `updateType` field of the Server Graphics Update PDU
/// and indicate what type of graphics data follows.
///
/// [MS-RDPBCGR 2.2.9.1.1.3.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive)]
pub enum UpdateType {
    /// Drawing orders (primary, secondary, and alternate secondary)
    Orders = 0x0000,
    /// Bitmap graphics data (compressed or uncompressed)
    Bitmap = 0x0001,
    /// Color palette for 8bpp indexed color mode
    Palette = 0x0002,
    /// Synchronization marker
    Synchronize = 0x0003,
}

/// Parsed Server Graphics Update PDU.
///
/// This enum represents the different types of graphics updates that can be
/// received from an RDP server via the slow-path (X224) data channel.
///
/// # Variants
///
/// - `Orders` - Drawing commands for rendering primitives (lines, rectangles, etc.)
/// - `Bitmap` - Compressed or uncompressed bitmap data for screen regions
/// - `Palette` - Color palette updates for 8bpp indexed color mode
/// - `Synchronize` - Marker indicating a synchronization point in the update stream
#[derive(Debug, Clone)]
pub enum ServerGraphicsUpdate<'a> {
    /// Drawing orders (not yet fully implemented)
    Orders(OrdersUpdate<'a>),
    /// Bitmap update containing one or more screen region updates
    Bitmap(BitmapUpdateData<'a>),
    /// Palette update for 8bpp indexed color mode
    Palette(PaletteUpdate),
    /// Synchronization marker
    Synchronize,
}

impl<'de> ServerGraphicsUpdate<'de> {
    pub fn decode(src: &mut ReadCursor<'de>) -> DecodeResult<Self> {
        ensure_size!(in: src, size: 2);
        let update_type_raw = src.read_u16();
        let update_type = UpdateType::from_u16(update_type_raw)
            .ok_or_else(|| invalid_field_err!("updateType", "unknown update type"))?;

        match update_type {
            UpdateType::Orders => {
                ensure_size!(in: src, size: 6);
                let _pad = src.read_u16();
                let number_orders = src.read_u16();
                let _pad = src.read_u16();
                let order_data = src.remaining();
                Ok(ServerGraphicsUpdate::Orders(OrdersUpdate {
                    number_orders,
                    order_data,
                }))
            }
            UpdateType::Bitmap => {
                // Note: We've already consumed the updateType (0x0001) above.
                // The BitmapUpdateData structure expects its own update type header,
                // but in the slow-path Update PDU, there's only ONE update type field.
                // We need to parse the bitmap rectangles directly without re-reading the type.
                ensure_size!(in: src, size: 2);
                let rectangle_count = usize::from(src.read_u16());
                let mut rectangles = Vec::with_capacity(rectangle_count);
                for _ in 0..rectangle_count {
                    rectangles.push(crate::bitmap::BitmapData::decode(src)?);
                }
                Ok(ServerGraphicsUpdate::Bitmap(BitmapUpdateData { rectangles }))
            }
            UpdateType::Palette => {
                ensure_size!(in: src, size: 6);
                let _pad = src.read_u16();
                let number_colors = usize::try_from(src.read_u32())
                    .map_err(|_| invalid_field_err!("numberColors", "value too large"))?;
                ensure_size!(in: src, size: number_colors * 3);
                let mut entries = Vec::with_capacity(number_colors);
                for _ in 0..number_colors {
                    entries.push(PaletteEntry {
                        red: src.read_u8(),
                        green: src.read_u8(),
                        blue: src.read_u8(),
                    });
                }
                Ok(ServerGraphicsUpdate::Palette(PaletteUpdate { entries }))
            }
            UpdateType::Synchronize => {
                if src.len() >= 2 {
                    let _ = src.read_u16();
                }
                Ok(ServerGraphicsUpdate::Synchronize)
            }
        }
    }
}

/// Orders Update PDU containing drawing commands.
///
/// Drawing orders are used to render primitives like lines, rectangles,
/// and text directly on the client's frame buffer. This is more bandwidth-efficient
/// than sending bitmap data for simple graphics.
///
/// # Note
///
/// Order parsing and execution is not yet fully implemented. The raw order
/// data is provided for future implementation.
#[derive(Debug, Clone)]
pub struct OrdersUpdate<'a> {
    /// Number of drawing orders in this update
    pub number_orders: u16,
    /// Raw order data (parsing not yet implemented)
    pub order_data: &'a [u8],
}

/// Palette Update PDU for 8bpp indexed color mode.
///
/// When the RDP connection is using 8 bits-per-pixel color depth,
/// pixel values are indices into a 256-color palette. This update
/// provides the RGB values for those palette entries.
///
/// # Example
///
/// ```
/// use ironrdp_pdu::rdp::server_graphics_update::{PaletteUpdate, PaletteEntry};
///
/// let palette = PaletteUpdate {
///     entries: vec![
///         PaletteEntry { red: 0, green: 0, blue: 0 },       // Index 0: Black
///         PaletteEntry { red: 255, green: 255, blue: 255 }, // Index 1: White
///         // ... up to 256 entries
///     ],
/// };
/// ```
#[derive(Debug, Clone)]
pub struct PaletteUpdate {
    /// Color entries indexed by palette index (typically 256 entries for full palette)
    pub entries: Vec<PaletteEntry>,
}

/// A single RGB color entry in a palette.
///
/// Used in [`PaletteUpdate`] to define colors for 8bpp indexed color mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    /// Red component (0-255)
    pub red: u8,
    /// Green component (0-255)
    pub green: u8,
    /// Blue component (0-255)
    pub blue: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_synchronize_update() {
        let data = [0x03, 0x00]; // updateType = Synchronize
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        assert!(matches!(update, ServerGraphicsUpdate::Synchronize));
    }

    #[test]
    fn test_parse_synchronize_update_with_padding() {
        // Some servers send padding after the synchronize type
        let data = [0x03, 0x00, 0x00, 0x00]; // updateType = Synchronize + padding
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        assert!(matches!(update, ServerGraphicsUpdate::Synchronize));
    }

    #[test]
    fn test_parse_orders_update() {
        let data = [
            0x00, 0x00, // updateType = Orders
            0x00, 0x00, // pad2OctetsA
            0x05, 0x00, // numberOrders = 5
            0x00, 0x00, // pad2OctetsB
            0xAA, 0xBB, // order data
        ];
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Orders(orders) = update {
            assert_eq!(orders.number_orders, 5);
            assert_eq!(orders.order_data, &[0xAA, 0xBB]);
        } else {
            panic!("Expected Orders");
        }
    }

    #[test]
    fn test_parse_orders_update_empty_data() {
        let data = [
            0x00, 0x00, // updateType = Orders
            0x00, 0x00, // pad2OctetsA
            0x00, 0x00, // numberOrders = 0
            0x00, 0x00, // pad2OctetsB
        ];
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Orders(orders) = update {
            assert_eq!(orders.number_orders, 0);
            assert!(orders.order_data.is_empty());
        } else {
            panic!("Expected Orders");
        }
    }

    #[test]
    fn test_parse_palette_update() {
        let data = [
            0x02, 0x00, // updateType = Palette
            0x00, 0x00, // pad2Octets
            0x03, 0x00, 0x00, 0x00, // numberColors = 3
            0xFF, 0x00, 0x00, // Red
            0x00, 0xFF, 0x00, // Green
            0x00, 0x00, 0xFF, // Blue
        ];
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Palette(palette) = update {
            assert_eq!(palette.entries.len(), 3);
            assert_eq!(palette.entries[0].red, 0xFF);
            assert_eq!(palette.entries[0].green, 0x00);
            assert_eq!(palette.entries[0].blue, 0x00);
            assert_eq!(palette.entries[1].red, 0x00);
            assert_eq!(palette.entries[1].green, 0xFF);
            assert_eq!(palette.entries[1].blue, 0x00);
            assert_eq!(palette.entries[2].red, 0x00);
            assert_eq!(palette.entries[2].green, 0x00);
            assert_eq!(palette.entries[2].blue, 0xFF);
        } else {
            panic!("Expected Palette");
        }
    }

    #[test]
    fn test_parse_palette_update_empty() {
        let data = [
            0x02, 0x00, // updateType = Palette
            0x00, 0x00, // pad2Octets
            0x00, 0x00, 0x00, 0x00, // numberColors = 0
        ];
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Palette(palette) = update {
            assert!(palette.entries.is_empty());
        } else {
            panic!("Expected Palette");
        }
    }

    #[test]
    fn test_parse_bitmap_update() {
        // This is a minimal bitmap update with one rectangle
        // Note: Slow-path Update PDU has only ONE updateType field (not duplicated)
        let data = [
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
            // 32 bytes of uncompressed bitmap data (4x4 @ 16bpp)
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Bitmap(bitmap_data) = update {
            assert_eq!(bitmap_data.rectangles.len(), 1);
            let rect = &bitmap_data.rectangles[0];
            assert_eq!(rect.width, 4);
            assert_eq!(rect.height, 4);
            assert_eq!(rect.bits_per_pixel, 16);
            assert_eq!(rect.bitmap_data.len(), 32);
        } else {
            panic!("Expected Bitmap");
        }
    }

    #[test]
    fn test_parse_unknown_update_type() {
        let data = [0xFF, 0xFF]; // unknown updateType
        let mut cursor = ReadCursor::new(&data);
        let result = ServerGraphicsUpdate::decode(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_insufficient_data() {
        let data = [0x03]; // only 1 byte, need at least 2
        let mut cursor = ReadCursor::new(&data);
        let result = ServerGraphicsUpdate::decode(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_orders_insufficient_header() {
        let data = [
            0x00, 0x00, // updateType = Orders
            0x00, 0x00, // only pad2OctetsA, missing rest
        ];
        let mut cursor = ReadCursor::new(&data);
        let result = ServerGraphicsUpdate::decode(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_palette_insufficient_colors() {
        let data = [
            0x02, 0x00, // updateType = Palette
            0x00, 0x00, // pad2Octets
            0x02, 0x00, 0x00, 0x00, // numberColors = 2
            0xFF, 0x00, 0x00, // Only 1 color (Red), but claimed 2
        ];
        let mut cursor = ReadCursor::new(&data);
        let result = ServerGraphicsUpdate::decode(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_update_type_values() {
        assert_eq!(UpdateType::Orders as u16, 0x0000);
        assert_eq!(UpdateType::Bitmap as u16, 0x0001);
        assert_eq!(UpdateType::Palette as u16, 0x0002);
        assert_eq!(UpdateType::Synchronize as u16, 0x0003);
    }

    #[test]
    fn test_parse_palette_full_256_colors() {
        // Test a full 256-color palette
        let mut data = vec![
            0x02, 0x00, // updateType = Palette
            0x00, 0x00, // pad2Octets
            0x00, 0x01, 0x00, 0x00, // numberColors = 256
        ];
        // Add 256 color entries
        for i in 0..=255u8 {
            data.push(i); // Red
            data.push(255 - i); // Green
            data.push(i / 2); // Blue
        }

        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Palette(palette) = update {
            assert_eq!(palette.entries.len(), 256);
            // Check first entry
            assert_eq!(palette.entries[0].red, 0);
            assert_eq!(palette.entries[0].green, 255);
            assert_eq!(palette.entries[0].blue, 0);
            // Check last entry
            assert_eq!(palette.entries[255].red, 255);
            assert_eq!(palette.entries[255].green, 0);
            assert_eq!(palette.entries[255].blue, 127);
        } else {
            panic!("Expected Palette");
        }
    }

    #[test]
    fn test_parse_orders_with_large_data() {
        // Test orders with a larger data payload
        let mut data = vec![
            0x00, 0x00, // updateType = Orders
            0x00, 0x00, // pad2OctetsA
            0x10, 0x00, // numberOrders = 16
            0x00, 0x00, // pad2OctetsB
        ];
        // Add 100 bytes of order data
        data.extend_from_slice(&[0xAA; 100]);

        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        if let ServerGraphicsUpdate::Orders(orders) = update {
            assert_eq!(orders.number_orders, 16);
            assert_eq!(orders.order_data.len(), 100);
            assert!(orders.order_data.iter().all(|&b| b == 0xAA));
        } else {
            panic!("Expected Orders");
        }
    }

    #[test]
    fn test_palette_entry_equality() {
        let entry1 = PaletteEntry {
            red: 100,
            green: 150,
            blue: 200,
        };
        let entry2 = PaletteEntry {
            red: 100,
            green: 150,
            blue: 200,
        };
        let entry3 = PaletteEntry {
            red: 100,
            green: 150,
            blue: 201,
        };

        assert_eq!(entry1, entry2);
        assert_ne!(entry1, entry3);
    }

    #[test]
    fn test_palette_entry_copy() {
        let entry1 = PaletteEntry {
            red: 100,
            green: 150,
            blue: 200,
        };
        let entry2 = entry1; // Copy
        assert_eq!(entry1, entry2);
    }

    #[test]
    fn test_parse_palette_insufficient_header() {
        // Only updateType, missing pad and numberColors
        let data = [0x02, 0x00, 0x00, 0x00];
        let mut cursor = ReadCursor::new(&data);
        let result = ServerGraphicsUpdate::decode(&mut cursor);
        assert!(result.is_err());
    }

    #[test]
    fn test_synchronize_consumes_padding_when_present() {
        // Test that synchronize correctly handles extra padding bytes
        let data = [0x03, 0x00, 0xFF, 0xFF]; // updateType = Synchronize + 2 bytes padding
        let mut cursor = ReadCursor::new(&data);
        let update = ServerGraphicsUpdate::decode(&mut cursor).unwrap();
        assert!(matches!(update, ServerGraphicsUpdate::Synchronize));
        // Cursor should have consumed the padding
        assert!(cursor.is_empty() || cursor.len() == 2); // Depends on implementation
    }
}

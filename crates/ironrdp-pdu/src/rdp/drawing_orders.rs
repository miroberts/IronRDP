//! Drawing Orders PDU
//!
//! This module implements parsing for RDP drawing orders as specified in
//! [\[MS-RDPEGDI\]]. Drawing orders are used to render primitives like lines,
//! rectangles, and text directly on the client's frame buffer.
//!
//! # Order Types
//!
//! RDP defines three classes of orders:
//!
//! - **Primary Orders**: Basic drawing operations (DstBlt, PatBlt, ScrBlt, LineTo, etc.)
//! - **Secondary Orders**: Caching operations for bitmaps, brushes, glyphs, etc.
//! - **Alternate Secondary Orders**: Extended caching and frame markers
//!
//! # Implementation Status
//!
//! ## Primary Orders (Fully Parsed)
//!
//! | Order | Parsing | Rendering |
//! |-------|---------|-----------|
//! | DstBlt | ✅ | ✅ Partial (common ROPs) |
//! | PatBlt | ✅ | ✅ Partial (solid brush) |
//! | ScrBlt | ✅ | ✅ Partial (SRCCOPY) |
//! | OpaqueRect | ✅ | ✅ Complete |
//! | LineTo | ✅ | ✅ Complete |
//! | Polyline | ✅ | ✅ Complete |
//! | Multi* variants | ✅ | ✅ Partial |
//!
//! ## Secondary Orders (Parsed Only)
//!
//! Secondary orders are parsed but caching is not yet implemented.
//! The order data is available in [`SecondaryOrder::data`] for future use.
//!
//! ## Alternate Secondary Orders (Parsed Only)
//!
//! Alternate secondary orders (frame markers, etc.) are parsed but not acted upon.
//!
//! # Wire Format
//!
//! Orders are encoded with delta compression - each order contains only the fields
//! that differ from the previous order of the same type. The `controlFlags` byte
//! indicates which fields are present. The [`DrawingOrderState`] structure tracks
//! the current state for delta decoding.
//!
//! # Example
//!
//! ```ignore
//! use ironrdp_pdu::rdp::drawing_orders::{decode_order, DrawingOrder, DrawingOrderState};
//! use ironrdp_core::ReadCursor;
//!
//! let mut state = DrawingOrderState::new();
//! let mut cursor = ReadCursor::new(&order_data);
//!
//! while cursor.len() > 0 {
//!     match decode_order(&mut cursor, &mut state)? {
//!         DrawingOrder::Primary(primary) => {
//!             // Render the primary order
//!         }
//!         DrawingOrder::Secondary(secondary) => {
//!             // Cache the resource for later use
//!         }
//!         DrawingOrder::AlternateSecondary(alt) => {
//!             // Handle frame markers, etc.
//!         }
//!     }
//! }
//! ```
//!
//! [\[MS-RDPEGDI\]]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegdi/745f2eee-d110-464c-8aca-06fc1dc57b16

use bitflags::bitflags;
use ironrdp_core::{ensure_size, invalid_field_err, DecodeResult, ReadCursor};
use num_derive::FromPrimitive;
use num_traits::FromPrimitive as _;

bitflags! {
    /// Control flags for order encoding (MS-RDPEGDI 2.2.2.2.1.1.2)
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct ControlFlags: u8 {
        /// Order is a standard (primary) order
        const STANDARD = 0x01;
        /// Order is a secondary order
        const SECONDARY = 0x02;
        /// Order type is present in the order data
        const TYPE_CHANGE = 0x08;
        /// Bounding rectangle is present
        const BOUNDS = 0x20;
        /// Bounding rectangle uses delta encoding
        const ZERO_BOUNDS_DELTAS = 0x40;
        /// Coordinates use delta encoding from previous order
        const ZERO_FIELD_BYTE_BIT0 = 0x80;
    }
}

/// Primary drawing order types (MS-RDPEGDI 2.2.2.2.1.1.2)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive)]
pub enum PrimaryOrderType {
    /// Destination block transfer
    DstBlt = 0x00,
    /// Pattern block transfer
    PatBlt = 0x01,
    /// Screen block transfer
    ScrBlt = 0x02,
    /// Draw nine grid
    DrawNineGrid = 0x07,
    /// Multi draw nine grid
    MultiDrawNineGrid = 0x08,
    /// Line to
    LineTo = 0x09,
    /// Opaque rectangle
    OpaqueRect = 0x0A,
    /// Save bitmap
    SaveBitmap = 0x0B,
    /// Memory block transfer
    MemBlt = 0x0D,
    /// Memory 3-way block transfer
    Mem3Blt = 0x0E,
    /// Multi destination block transfer
    MultiDstBlt = 0x0F,
    /// Multi pattern block transfer
    MultiPatBlt = 0x10,
    /// Multi screen block transfer
    MultiScrBlt = 0x11,
    /// Multi opaque rectangle
    MultiOpaqueRect = 0x12,
    /// Fast index
    FastIndex = 0x13,
    /// Polygon solid color
    PolygonSC = 0x14,
    /// Polygon color brush
    PolygonCB = 0x15,
    /// Polyline
    Polyline = 0x16,
    /// Fast glyph
    FastGlyph = 0x18,
    /// Ellipse solid color
    EllipseSC = 0x19,
    /// Ellipse color brush
    EllipseCB = 0x1A,
    /// Glyph index
    GlyphIndex = 0x1B,
}

/// Secondary drawing order types (MS-RDPEGDI 2.2.2.2.1.2.1)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive)]
pub enum SecondaryOrderType {
    /// Cache bitmap (revision 1)
    CacheBitmap = 0x00,
    /// Cache color table
    CacheColorTable = 0x01,
    /// Cache bitmap (revision 2)
    CacheBitmapV2 = 0x02,
    /// Cache glyph
    CacheGlyph = 0x03,
    /// Cache bitmap (revision 3)
    CacheBitmapV3 = 0x04,
    /// Cache brush
    CacheBrush = 0x07,
}

/// Alternate secondary order types (MS-RDPEGDI 2.2.2.2.1.3)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive)]
pub enum AlternateSecondaryOrderType {
    /// Switch surface
    SwitchSurface = 0x00,
    /// Create offscreen bitmap
    CreateOffscreenBitmap = 0x01,
    /// Stream bitmap first
    StreamBitmapFirst = 0x02,
    /// Stream bitmap next
    StreamBitmapNext = 0x03,
    /// Create nine grid bitmap
    CreateNineGridBitmap = 0x04,
    /// GDIP first
    GdipFirst = 0x05,
    /// GDIP next
    GdipNext = 0x06,
    /// GDIP end
    GdipEnd = 0x07,
    /// GDIP cache first
    GdipCacheFirst = 0x08,
    /// GDIP cache next
    GdipCacheNext = 0x09,
    /// GDIP cache end
    GdipCacheEnd = 0x0A,
    /// Window
    Window = 0x0B,
    /// Desktop composition
    CompDesk = 0x0C,
    /// Frame marker
    FrameMarker = 0x0D,
}

/// A parsed drawing order
#[derive(Debug, Clone)]
pub enum DrawingOrder<'a> {
    /// Primary drawing order
    Primary(PrimaryOrder),
    /// Secondary (caching) order
    Secondary(SecondaryOrder<'a>),
    /// Alternate secondary order (zero-copy reference to input buffer)
    AlternateSecondary(AlternateSecondaryOrder<'a>),
}

/// Primary drawing order
#[derive(Debug, Clone)]
pub struct PrimaryOrder {
    /// The order type
    pub order_type: PrimaryOrderType,
    /// Bounding rectangle (if present)
    pub bounds: Option<BoundingRectangle>,
    /// Order-specific data (not yet parsed)
    pub data: PrimaryOrderData,
}

/// Bounding rectangle for drawing orders
#[derive(Debug, Clone, Copy, Default)]
pub struct BoundingRectangle {
    pub left: i16,
    pub top: i16,
    pub right: i16,
    pub bottom: i16,
}

/// Primary order data
#[derive(Debug, Clone)]
pub enum PrimaryOrderData {
    /// DstBlt order data
    DstBlt(DstBltOrder),
    /// PatBlt order data  
    PatBlt(PatBltOrder),
    /// ScrBlt order data
    ScrBlt(ScrBltOrder),
    /// OpaqueRect order data
    OpaqueRect(OpaqueRectOrder),
    /// LineTo order data
    LineTo(LineToOrder),
    /// MultiOpaqueRect order data
    MultiOpaqueRect(MultiOpaqueRectOrder),
    /// MultiDstBlt order data
    MultiDstBlt(MultiDstBltOrder),
    /// MultiPatBlt order data
    MultiPatBlt(MultiPatBltOrder),
    /// MultiScrBlt order data
    MultiScrBlt(MultiScrBltOrder),
    /// Polyline order data
    Polyline(PolylineOrder),
    /// Other order types (raw data, not yet parsed)
    Other(Vec<u8>),
}

/// DstBlt (Destination Block Transfer) order
/// MS-RDPEGDI 2.2.2.2.1.1.2.1
#[derive(Debug, Clone, Default)]
pub struct DstBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
}

/// PatBlt (Pattern Block Transfer) order
/// MS-RDPEGDI 2.2.2.2.1.1.2.2
#[derive(Debug, Clone, Default)]
pub struct PatBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
    pub back_color: u32,
    pub fore_color: u32,
    pub brush: BrushData,
}

/// ScrBlt (Screen Block Transfer) order
/// MS-RDPEGDI 2.2.2.2.1.1.2.3
#[derive(Debug, Clone, Default)]
pub struct ScrBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
    pub src_x: i16,
    pub src_y: i16,
}

/// OpaqueRect order
/// MS-RDPEGDI 2.2.2.2.1.1.2.10
#[derive(Debug, Clone, Default)]
pub struct OpaqueRectOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub color: u32,
}

/// LineTo order
/// MS-RDPEGDI 2.2.2.2.1.1.2.9
#[derive(Debug, Clone, Default)]
pub struct LineToOrder {
    pub back_mode: u16,
    pub start_x: i16,
    pub start_y: i16,
    pub end_x: i16,
    pub end_y: i16,
    pub back_color: u32,
    pub rop2: u8,
    pub pen_style: u8,
    pub pen_width: u8,
    pub pen_color: u32,
}

/// MultiOpaqueRect order
/// MS-RDPEGDI 2.2.2.2.1.1.2.12
#[derive(Debug, Clone, Default)]
pub struct MultiOpaqueRectOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub color: u32,
    pub rectangles: Vec<DeltaRect>,
}

/// MultiDstBlt order
/// MS-RDPEGDI 2.2.2.2.1.1.2.5
#[derive(Debug, Clone, Default)]
pub struct MultiDstBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
    pub rectangles: Vec<DeltaRect>,
}

/// MultiPatBlt order
/// MS-RDPEGDI 2.2.2.2.1.1.2.6
#[derive(Debug, Clone, Default)]
pub struct MultiPatBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
    pub back_color: u32,
    pub fore_color: u32,
    pub brush: BrushData,
    pub rectangles: Vec<DeltaRect>,
}

/// MultiScrBlt order
/// MS-RDPEGDI 2.2.2.2.1.1.2.7
#[derive(Debug, Clone, Default)]
pub struct MultiScrBltOrder {
    pub x: i16,
    pub y: i16,
    pub width: i16,
    pub height: i16,
    pub rop: u8,
    pub src_x: i16,
    pub src_y: i16,
    pub rectangles: Vec<DeltaRect>,
}

/// Polyline order
/// MS-RDPEGDI 2.2.2.2.1.1.2.16
#[derive(Debug, Clone, Default)]
pub struct PolylineOrder {
    pub x: i16,
    pub y: i16,
    pub rop2: u8,
    pub brush_cache_entry: u16,
    pub pen_color: u32,
    pub points: Vec<DeltaPoint>,
}

/// Delta-encoded rectangle for Multi* orders
#[derive(Debug, Clone, Copy, Default)]
pub struct DeltaRect {
    pub left: i16,
    pub top: i16,
    pub width: i16,
    pub height: i16,
}

/// Delta-encoded point for Polyline
#[derive(Debug, Clone, Copy, Default)]
pub struct DeltaPoint {
    pub x: i16,
    pub y: i16,
}

/// Brush data for pattern orders
#[derive(Debug, Clone, Default)]
pub struct BrushData {
    pub org_x: i8,
    pub org_y: i8,
    pub style: u8,
    pub hatch: u8,
    pub extra: [u8; 7],
}

/// Secondary order (caching)
#[derive(Debug, Clone)]
pub struct SecondaryOrder<'a> {
    pub order_type: SecondaryOrderType,
    pub data: &'a [u8],
}

/// Alternate secondary order
#[derive(Debug, Clone)]
pub struct AlternateSecondaryOrder<'a> {
    pub order_type: AlternateSecondaryOrderType,
    /// Raw order data (zero-copy reference to input buffer)
    pub data: &'a [u8],
}

/// Drawing order state - tracks the current state for delta decoding
#[derive(Debug, Clone, Default)]
#[must_use]
pub struct DrawingOrderState {
    /// Current primary order type
    pub primary_order_type: Option<PrimaryOrderType>,
    /// Current bounds
    pub bounds: BoundingRectangle,
    /// Current DstBlt state
    pub dstblt: DstBltOrder,
    /// Current PatBlt state
    pub patblt: PatBltOrder,
    /// Current ScrBlt state
    pub scrblt: ScrBltOrder,
    /// Current OpaqueRect state
    pub opaque_rect: OpaqueRectOrder,
    /// Current LineTo state
    pub line_to: LineToOrder,
    /// Current MultiOpaqueRect state
    pub multi_opaque_rect: MultiOpaqueRectOrder,
    /// Current MultiDstBlt state
    pub multi_dstblt: MultiDstBltOrder,
    /// Current MultiPatBlt state
    pub multi_patblt: MultiPatBltOrder,
    /// Current MultiScrBlt state
    pub multi_scrblt: MultiScrBltOrder,
    /// Current Polyline state
    pub polyline: PolylineOrder,
}

impl DrawingOrderState {
    /// Create a new drawing order state
    pub fn new() -> Self {
        Self::default()
    }
}

/// Decode a single drawing order from the stream
pub fn decode_order<'a>(cursor: &mut ReadCursor<'a>, state: &mut DrawingOrderState) -> DecodeResult<DrawingOrder<'a>> {
    ensure_size!(in: cursor, size: 1);
    let control_flags = ControlFlags::from_bits_truncate(cursor.read_u8());

    if control_flags.contains(ControlFlags::SECONDARY) {
        decode_secondary_order(cursor)
    } else if control_flags.intersects(ControlFlags::STANDARD) {
        decode_primary_order(cursor, state, control_flags)
    } else {
        // Alternate secondary order
        decode_alternate_secondary_order(cursor)
    }
}

/// Number of field flag bytes for each primary order type
fn field_flag_bytes(order_type: PrimaryOrderType) -> usize {
    match order_type {
        PrimaryOrderType::DstBlt => 1,          // 5 fields
        PrimaryOrderType::PatBlt => 2,          // 12 fields
        PrimaryOrderType::ScrBlt => 1,          // 7 fields
        PrimaryOrderType::OpaqueRect => 1,      // 7 fields
        PrimaryOrderType::LineTo => 2,          // 10 fields
        PrimaryOrderType::MemBlt => 2,          // 9 fields
        PrimaryOrderType::Mem3Blt => 3,         // 16 fields
        PrimaryOrderType::MultiDstBlt => 1,     // 7 fields
        PrimaryOrderType::MultiPatBlt => 2,     // 14 fields
        PrimaryOrderType::MultiScrBlt => 2,     // 9 fields
        PrimaryOrderType::MultiOpaqueRect => 2, // 9 fields
        PrimaryOrderType::Polyline => 1,        // 7 fields
        _ => 3,                                 // Default to 3 bytes for safety
    }
}

fn decode_primary_order<'a>(
    cursor: &mut ReadCursor<'a>,
    state: &mut DrawingOrderState,
    control_flags: ControlFlags,
) -> DecodeResult<DrawingOrder<'a>> {
    // Read order type if TYPE_CHANGE flag is set
    let order_type = if control_flags.contains(ControlFlags::TYPE_CHANGE) {
        ensure_size!(in: cursor, size: 1);
        let type_byte = cursor.read_u8();
        let order_type = PrimaryOrderType::from_u8(type_byte)
            .ok_or_else(|| invalid_field_err!("orderType", "unknown primary order type"))?;
        state.primary_order_type = Some(order_type);
        order_type
    } else {
        state
            .primary_order_type
            .ok_or_else(|| invalid_field_err!("orderType", "no previous order type"))?
    };

    // Read field flags
    let flag_bytes = field_flag_bytes(order_type);
    ensure_size!(in: cursor, size: flag_bytes);
    let mut field_flags: u32 = 0;
    for i in 0..flag_bytes {
        field_flags |= u32::from(cursor.read_u8()) << (i * 8);
    }

    // Read bounds if present
    let bounds = if control_flags.contains(ControlFlags::BOUNDS) {
        Some(decode_bounds(cursor, &state.bounds, control_flags)?)
    } else {
        None
    };

    if let Some(ref b) = bounds {
        state.bounds = *b;
    }

    // Parse order-specific fields based on type
    let data = match order_type {
        PrimaryOrderType::DstBlt => {
            decode_dstblt_order(cursor, &mut state.dstblt, field_flags)?;
            PrimaryOrderData::DstBlt(state.dstblt.clone())
        }
        PrimaryOrderType::PatBlt => {
            decode_patblt_order(cursor, &mut state.patblt, field_flags)?;
            PrimaryOrderData::PatBlt(state.patblt.clone())
        }
        PrimaryOrderType::ScrBlt => {
            decode_scrblt_order(cursor, &mut state.scrblt, field_flags)?;
            PrimaryOrderData::ScrBlt(state.scrblt.clone())
        }
        PrimaryOrderType::OpaqueRect => {
            decode_opaque_rect_order(cursor, &mut state.opaque_rect, field_flags)?;
            PrimaryOrderData::OpaqueRect(state.opaque_rect.clone())
        }
        PrimaryOrderType::LineTo => {
            decode_line_to_order(cursor, &mut state.line_to, field_flags)?;
            PrimaryOrderData::LineTo(state.line_to.clone())
        }
        PrimaryOrderType::MultiOpaqueRect => {
            decode_multi_opaque_rect_order(cursor, &mut state.multi_opaque_rect, field_flags)?;
            PrimaryOrderData::MultiOpaqueRect(state.multi_opaque_rect.clone())
        }
        PrimaryOrderType::MultiDstBlt => {
            decode_multi_dstblt_order(cursor, &mut state.multi_dstblt, field_flags)?;
            PrimaryOrderData::MultiDstBlt(state.multi_dstblt.clone())
        }
        PrimaryOrderType::MultiPatBlt => {
            decode_multi_patblt_order(cursor, &mut state.multi_patblt, field_flags)?;
            PrimaryOrderData::MultiPatBlt(state.multi_patblt.clone())
        }
        PrimaryOrderType::MultiScrBlt => {
            decode_multi_scrblt_order(cursor, &mut state.multi_scrblt, field_flags)?;
            PrimaryOrderData::MultiScrBlt(state.multi_scrblt.clone())
        }
        PrimaryOrderType::Polyline => {
            decode_polyline_order(cursor, &mut state.polyline, field_flags)?;
            PrimaryOrderData::Polyline(state.polyline.clone())
        }
        _ => {
            // For unsupported order types, we can't safely skip them without knowing
            // the exact field layout, so just return empty data
            PrimaryOrderData::Other(Vec::new())
        }
    };

    Ok(DrawingOrder::Primary(PrimaryOrder {
        order_type,
        bounds,
        data,
    }))
}

/// Decode DstBlt order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.1
/// Fields: nLeftRect, nTopRect, nWidth, nHeight, bRop
fn decode_dstblt_order(cursor: &mut ReadCursor<'_>, order: &mut DstBltOrder, field_flags: u32) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x01 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x02 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x04 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x08 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x10 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    Ok(())
}

/// Decode PatBlt order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.2
/// Fields: nLeftRect, nTopRect, nWidth, nHeight, bRop, BackColor, ForeColor, BrushOrgX, BrushOrgY, BrushStyle, BrushHatch, BrushExtra
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    reason = "BrushOrg fields are signed bytes encoded as unsigned per RDP spec"
)]
fn decode_patblt_order(cursor: &mut ReadCursor<'_>, order: &mut PatBltOrder, field_flags: u32) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x0001 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x0002 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x0004 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x0008 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x0010 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    // Field 6: BackColor (3 bytes RGB)
    if field_flags & 0x0020 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.back_color = (r << 16) | (g << 8) | b;
    }
    // Field 7: ForeColor (3 bytes RGB)
    if field_flags & 0x0040 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.fore_color = (r << 16) | (g << 8) | b;
    }
    // Field 8: BrushOrgX (i8)
    if field_flags & 0x0080 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.org_x = cursor.read_u8() as i8;
    }
    // Field 9: BrushOrgY (i8)
    if field_flags & 0x0100 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.org_y = cursor.read_u8() as i8;
    }
    // Field 10: BrushStyle (u8)
    if field_flags & 0x0200 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.style = cursor.read_u8();
    }
    // Field 11: BrushHatch (u8)
    if field_flags & 0x0400 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.hatch = cursor.read_u8();
    }
    // Field 12: BrushExtra (7 bytes)
    if field_flags & 0x0800 != 0 {
        ensure_size!(in: cursor, size: 7);
        cursor.read_slice(7).iter().enumerate().for_each(|(i, &b)| {
            order.brush.extra[i] = b;
        });
    }
    Ok(())
}

/// Decode ScrBlt order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.3
/// Fields: nLeftRect, nTopRect, nWidth, nHeight, bRop, nXSrc, nYSrc
fn decode_scrblt_order(cursor: &mut ReadCursor<'_>, order: &mut ScrBltOrder, field_flags: u32) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x01 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x02 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x04 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x08 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x10 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    // Field 6: nXSrc (i16)
    if field_flags & 0x20 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.src_x = cursor.read_i16();
    }
    // Field 7: nYSrc (i16)
    if field_flags & 0x40 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.src_y = cursor.read_i16();
    }
    Ok(())
}

/// Decode OpaqueRect order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.10
/// Fields: nLeftRect, nTopRect, nWidth, nHeight, Color (split into R, G, B)
fn decode_opaque_rect_order(
    cursor: &mut ReadCursor<'_>,
    order: &mut OpaqueRectOrder,
    field_flags: u32,
) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x01 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x02 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x04 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x08 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: Color Red (u8)
    if field_flags & 0x10 != 0 {
        ensure_size!(in: cursor, size: 1);
        let r = u32::from(cursor.read_u8());
        order.color = (order.color & 0x00FFFF) | (r << 16);
    }
    // Field 6: Color Green (u8)
    if field_flags & 0x20 != 0 {
        ensure_size!(in: cursor, size: 1);
        let g = u32::from(cursor.read_u8());
        order.color = (order.color & 0xFF00FF) | (g << 8);
    }
    // Field 7: Color Blue (u8)
    if field_flags & 0x40 != 0 {
        ensure_size!(in: cursor, size: 1);
        let b = u32::from(cursor.read_u8());
        order.color = (order.color & 0xFFFF00) | b;
    }
    Ok(())
}

/// Decode LineTo order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.9
fn decode_line_to_order(cursor: &mut ReadCursor<'_>, order: &mut LineToOrder, field_flags: u32) -> DecodeResult<()> {
    // Field 1: BackMode (u16)
    if field_flags & 0x0001 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.back_mode = cursor.read_u16();
    }
    // Field 2: nXStart (i16)
    if field_flags & 0x0002 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.start_x = cursor.read_i16();
    }
    // Field 3: nYStart (i16)
    if field_flags & 0x0004 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.start_y = cursor.read_i16();
    }
    // Field 4: nXEnd (i16)
    if field_flags & 0x0008 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.end_x = cursor.read_i16();
    }
    // Field 5: nYEnd (i16)
    if field_flags & 0x0010 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.end_y = cursor.read_i16();
    }
    // Field 6: BackColor (3 bytes BGR)
    if field_flags & 0x0020 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.back_color = (r << 16) | (g << 8) | b;
    }
    // Field 7: ROP2 (u8)
    if field_flags & 0x0040 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop2 = cursor.read_u8();
    }
    // Field 8: PenStyle (u8)
    if field_flags & 0x0080 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.pen_style = cursor.read_u8();
    }
    // Field 9: PenWidth (u8)
    if field_flags & 0x0100 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.pen_width = cursor.read_u8();
    }
    // Field 10: PenColor (3 bytes BGR)
    if field_flags & 0x0200 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.pen_color = (r << 16) | (g << 8) | b;
    }
    Ok(())
}

/// Decode MultiOpaqueRect order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.12
fn decode_multi_opaque_rect_order(
    cursor: &mut ReadCursor<'_>,
    order: &mut MultiOpaqueRectOrder,
    field_flags: u32,
) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x0001 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x0002 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x0004 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x0008 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: Color Red (u8)
    if field_flags & 0x0010 != 0 {
        ensure_size!(in: cursor, size: 1);
        let r = u32::from(cursor.read_u8());
        order.color = (order.color & 0x00FFFF) | (r << 16);
    }
    // Field 6: Color Green (u8)
    if field_flags & 0x0020 != 0 {
        ensure_size!(in: cursor, size: 1);
        let g = u32::from(cursor.read_u8());
        order.color = (order.color & 0xFF00FF) | (g << 8);
    }
    // Field 7: Color Blue (u8)
    if field_flags & 0x0040 != 0 {
        ensure_size!(in: cursor, size: 1);
        let b = u32::from(cursor.read_u8());
        order.color = (order.color & 0xFFFF00) | b;
    }
    // Field 8: nDeltaEntries (u8)
    // Field 9: CodedDeltaList (variable)
    if field_flags & 0x0180 != 0 {
        ensure_size!(in: cursor, size: 1);
        let num_entries = usize::from(cursor.read_u8());
        order.rectangles = decode_delta_rects(cursor, num_entries)?;
    }
    Ok(())
}

/// Decode MultiDstBlt order fields
fn decode_multi_dstblt_order(
    cursor: &mut ReadCursor<'_>,
    order: &mut MultiDstBltOrder,
    field_flags: u32,
) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x01 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x02 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x04 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x08 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x10 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    // Field 6: nDeltaEntries (u8)
    // Field 7: CodedDeltaList (variable)
    if field_flags & 0x60 != 0 {
        ensure_size!(in: cursor, size: 1);
        let num_entries = usize::from(cursor.read_u8());
        order.rectangles = decode_delta_rects(cursor, num_entries)?;
    }
    Ok(())
}

/// Decode MultiPatBlt order fields
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    reason = "BrushOrg fields are signed bytes encoded as unsigned per RDP spec"
)]
fn decode_multi_patblt_order(
    cursor: &mut ReadCursor<'_>,
    order: &mut MultiPatBltOrder,
    field_flags: u32,
) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x0001 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x0002 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x0004 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x0008 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x0010 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    // Field 6: BackColor (3 bytes BGR)
    if field_flags & 0x0020 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.back_color = (r << 16) | (g << 8) | b;
    }
    // Field 7: ForeColor (3 bytes BGR)
    if field_flags & 0x0040 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.fore_color = (r << 16) | (g << 8) | b;
    }
    // Fields 8-12: Brush (same as PatBlt)
    if field_flags & 0x0080 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.org_x = cursor.read_u8() as i8;
    }
    if field_flags & 0x0100 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.org_y = cursor.read_u8() as i8;
    }
    if field_flags & 0x0200 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.style = cursor.read_u8();
    }
    if field_flags & 0x0400 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.brush.hatch = cursor.read_u8();
    }
    if field_flags & 0x0800 != 0 {
        ensure_size!(in: cursor, size: 7);
        cursor.read_slice(7).iter().enumerate().for_each(|(i, &b)| {
            order.brush.extra[i] = b;
        });
    }
    // Field 13: nDeltaEntries (u8)
    // Field 14: CodedDeltaList (variable)
    if field_flags & 0x3000 != 0 {
        ensure_size!(in: cursor, size: 1);
        let num_entries = usize::from(cursor.read_u8());
        order.rectangles = decode_delta_rects(cursor, num_entries)?;
    }
    Ok(())
}

/// Decode MultiScrBlt order fields
fn decode_multi_scrblt_order(
    cursor: &mut ReadCursor<'_>,
    order: &mut MultiScrBltOrder,
    field_flags: u32,
) -> DecodeResult<()> {
    // Field 1: nLeftRect (i16)
    if field_flags & 0x0001 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: nTopRect (i16)
    if field_flags & 0x0002 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: nWidth (i16)
    if field_flags & 0x0004 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.width = cursor.read_i16();
    }
    // Field 4: nHeight (i16)
    if field_flags & 0x0008 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.height = cursor.read_i16();
    }
    // Field 5: bRop (u8)
    if field_flags & 0x0010 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop = cursor.read_u8();
    }
    // Field 6: nXSrc (i16)
    if field_flags & 0x0020 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.src_x = cursor.read_i16();
    }
    // Field 7: nYSrc (i16)
    if field_flags & 0x0040 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.src_y = cursor.read_i16();
    }
    // Field 8: nDeltaEntries (u8)
    // Field 9: CodedDeltaList (variable)
    if field_flags & 0x0180 != 0 {
        ensure_size!(in: cursor, size: 1);
        let num_entries = usize::from(cursor.read_u8());
        order.rectangles = decode_delta_rects(cursor, num_entries)?;
    }
    Ok(())
}

/// Decode Polyline order fields
/// MS-RDPEGDI 2.2.2.2.1.1.2.16
fn decode_polyline_order(cursor: &mut ReadCursor<'_>, order: &mut PolylineOrder, field_flags: u32) -> DecodeResult<()> {
    // Field 1: xStart (i16)
    if field_flags & 0x01 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.x = cursor.read_i16();
    }
    // Field 2: yStart (i16)
    if field_flags & 0x02 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.y = cursor.read_i16();
    }
    // Field 3: ROP2 (u8)
    if field_flags & 0x04 != 0 {
        ensure_size!(in: cursor, size: 1);
        order.rop2 = cursor.read_u8();
    }
    // Field 4: BrushCacheEntry (u16) - not commonly used
    if field_flags & 0x08 != 0 {
        ensure_size!(in: cursor, size: 2);
        order.brush_cache_entry = cursor.read_u16();
    }
    // Field 5: PenColor (3 bytes BGR)
    if field_flags & 0x10 != 0 {
        ensure_size!(in: cursor, size: 3);
        let b = u32::from(cursor.read_u8());
        let g = u32::from(cursor.read_u8());
        let r = u32::from(cursor.read_u8());
        order.pen_color = (r << 16) | (g << 8) | b;
    }
    // Field 6: NumDeltaEntries (u8)
    // Field 7: CodedDeltaList (variable)
    if field_flags & 0x60 != 0 {
        ensure_size!(in: cursor, size: 1);
        let num_entries = usize::from(cursor.read_u8());
        order.points = decode_delta_points(cursor, num_entries)?;
    }
    Ok(())
}

/// Decode delta-encoded rectangles for Multi* orders.
///
/// Per MS-RDPEGDI 2.2.2.2.1.1.2, delta rectangles use a zeroBits field
/// to indicate which coordinates use 1-byte vs 2-byte encoding.
///
/// Note: We must copy the zeroBits into a Vec because we continue reading
/// from the cursor after it, which would invalidate any slice reference.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    reason = "Delta values are signed bytes encoded as unsigned per RDP spec"
)]
fn decode_delta_rects(cursor: &mut ReadCursor<'_>, count: usize) -> DecodeResult<Vec<DeltaRect>> {
    // zeroBits field: 4 bits per rectangle, packed 2 per byte
    let zero_bits_len = count.div_ceil(2);
    ensure_size!(in: cursor, size: zero_bits_len);

    // Copy is necessary: we read zeroBits first, then interleaved delta values
    let mut zero_bits = [0u8; 128]; // Max 256 rectangles = 128 bytes
    let actual_len = zero_bits_len.min(zero_bits.len());
    zero_bits[..actual_len].copy_from_slice(cursor.read_slice(actual_len));

    let mut rects = Vec::with_capacity(count);
    let mut left: i16 = 0;
    let mut top: i16 = 0;

    for i in 0..count {
        let zero_byte = zero_bits[i / 2];
        let flags = if i % 2 == 0 { zero_byte & 0x0F } else { zero_byte >> 4 };

        // Read delta values based on flags (0 = 1-byte signed, 1 = 2-byte signed)
        // Left and top are cumulative deltas from previous rectangle
        left = left.wrapping_add(if flags & 0x01 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        });

        top = top.wrapping_add(if flags & 0x02 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        });

        // Width and height are absolute values per rectangle
        let width = if flags & 0x04 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        };

        let height = if flags & 0x08 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        };

        rects.push(DeltaRect {
            left,
            top,
            width,
            height,
        });
    }

    Ok(rects)
}

/// Decode delta-encoded points for Polyline.
///
/// Per MS-RDPEGDI 2.2.2.2.1.1.2.16, polyline points use a zeroBits field
/// to indicate which coordinates use 1-byte vs 2-byte encoding.
///
/// Note: We must copy the zeroBits into a fixed array because we continue
/// reading from the cursor after it, which would invalidate any slice reference.
#[expect(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    reason = "Delta values are signed bytes encoded as unsigned per RDP spec"
)]
fn decode_delta_points(cursor: &mut ReadCursor<'_>, count: usize) -> DecodeResult<Vec<DeltaPoint>> {
    // zeroBits field: 2 bits per point, packed 4 per byte
    let zero_bits_len = count.div_ceil(4);
    ensure_size!(in: cursor, size: zero_bits_len);

    // Copy is necessary: we read zeroBits first, then interleaved delta values
    // Max 256 points = 64 bytes for zeroBits
    let mut zero_bits = [0u8; 64];
    let actual_len = zero_bits_len.min(zero_bits.len());
    zero_bits[..actual_len].copy_from_slice(cursor.read_slice(actual_len));

    let mut points = Vec::with_capacity(count);

    for i in 0..count {
        let zero_byte = zero_bits[i / 4];
        let bit_offset = (i % 4) * 2;
        let flags = (zero_byte >> bit_offset) & 0x03;

        let x = if flags & 0x01 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        };

        let y = if flags & 0x02 == 0 {
            ensure_size!(in: cursor, size: 1);
            i16::from(cursor.read_u8() as i8)
        } else {
            ensure_size!(in: cursor, size: 2);
            cursor.read_i16()
        };

        points.push(DeltaPoint { x, y });
    }

    Ok(points)
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_wrap,
    clippy::cast_lossless,
    reason = "Bounds delta values are signed bytes encoded as unsigned per RDP spec"
)]
fn decode_bounds(
    cursor: &mut ReadCursor<'_>,
    prev_bounds: &BoundingRectangle,
    control_flags: ControlFlags,
) -> DecodeResult<BoundingRectangle> {
    let mut bounds = *prev_bounds;

    if control_flags.contains(ControlFlags::ZERO_BOUNDS_DELTAS) {
        // Bounds use delta encoding - read flags indicating which fields are present
        ensure_size!(in: cursor, size: 1);
        let flags = cursor.read_u8();

        if flags & 0x01 != 0 {
            ensure_size!(in: cursor, size: 1);
            bounds.left = bounds.left.wrapping_add(cursor.read_u8() as i8 as i16);
        }
        if flags & 0x02 != 0 {
            ensure_size!(in: cursor, size: 1);
            bounds.top = bounds.top.wrapping_add(cursor.read_u8() as i8 as i16);
        }
        if flags & 0x04 != 0 {
            ensure_size!(in: cursor, size: 1);
            bounds.right = bounds.right.wrapping_add(cursor.read_u8() as i8 as i16);
        }
        if flags & 0x08 != 0 {
            ensure_size!(in: cursor, size: 1);
            bounds.bottom = bounds.bottom.wrapping_add(cursor.read_u8() as i8 as i16);
        }
    } else {
        // Read full coordinates
        ensure_size!(in: cursor, size: 1);
        let flags = cursor.read_u8();

        if flags & 0x01 != 0 {
            ensure_size!(in: cursor, size: 2);
            bounds.left = cursor.read_i16();
        }
        if flags & 0x02 != 0 {
            ensure_size!(in: cursor, size: 2);
            bounds.top = cursor.read_i16();
        }
        if flags & 0x04 != 0 {
            ensure_size!(in: cursor, size: 2);
            bounds.right = cursor.read_i16();
        }
        if flags & 0x08 != 0 {
            ensure_size!(in: cursor, size: 2);
            bounds.bottom = cursor.read_i16();
        }
    }

    Ok(bounds)
}

fn decode_secondary_order<'a>(cursor: &mut ReadCursor<'a>) -> DecodeResult<DrawingOrder<'a>> {
    // Secondary order format (MS-RDPEGDI 2.2.2.2.1.2.1):
    //
    // Header (6 bytes total):
    // - orderLength (2 bytes, i16) - see quirk note below
    // - extraFlags  (2 bytes, u16)
    // - orderType   (1 byte, u8)
    //
    // Per MS-RDPEGDI: "The orderLength field MUST contain the number of bytes
    // that remain in the secondary drawing order structure after the orderLength
    // field, PLUS 7."
    //
    // This means: orderLength = (extraFlags + orderType + data).len() + 7
    //           = 2 + 1 + data.len() + 7
    //           = data.len() + 10
    //
    // Therefore: data.len() = orderLength - 10
    //
    // But wait - we've already read 6 bytes (orderLength + extraFlags + orderType),
    // and we only care about the remaining data. Since:
    //   orderLength = bytes_after_orderLength_field + 7
    //   bytes_after_orderLength_field = extraFlags(2) + orderType(1) + data
    //                                 = 3 + data.len()
    //
    // So: orderLength = 3 + data.len() + 7 = data.len() + 10
    // And: data.len() = orderLength - 10
    //
    // After reading extraFlags and orderType (3 bytes after orderLength),
    // remaining bytes in cursor should be data.len() = orderLength - 10
    ensure_size!(in: cursor, size: 5);
    let order_length = cursor.read_u16();
    let _extra_flags = cursor.read_u16();
    let type_byte = cursor.read_u8();

    let order_type = SecondaryOrderType::from_u8(type_byte)
        .ok_or_else(|| invalid_field_err!("orderType", "unknown secondary order type"))?;

    // Calculate data length: orderLength - 10 (see header comment for derivation)
    // Use saturating_sub to handle malformed packets gracefully
    let data_len = usize::from(order_length).saturating_sub(10);
    ensure_size!(in: cursor, size: data_len);
    let data = cursor.read_slice(data_len);

    Ok(DrawingOrder::Secondary(SecondaryOrder { order_type, data }))
}

fn decode_alternate_secondary_order<'a>(cursor: &mut ReadCursor<'a>) -> DecodeResult<DrawingOrder<'a>> {
    // Alternate secondary order format (MS-RDPEGDI 2.2.2.2.1.3.1):
    // - orderType (1 byte, bits 0-3 = type, bits 4-7 = class = 0)
    // - orderLength (2 bytes) - length of order data following this field
    // - data (variable)
    ensure_size!(in: cursor, size: 3);
    let type_byte = cursor.read_u8();
    let order_length = cursor.read_u16();

    let order_type = AlternateSecondaryOrderType::from_u8(type_byte & 0x0F)
        .ok_or_else(|| invalid_field_err!("orderType", "unknown alternate secondary order type"))?;

    // Read the order data (zero-copy slice reference)
    let data_len = usize::from(order_length);
    ensure_size!(in: cursor, size: data_len);
    let data = cursor.read_slice(data_len);

    Ok(DrawingOrder::AlternateSecondary(AlternateSecondaryOrder {
        order_type,
        data,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_control_flags() {
        let flags = ControlFlags::STANDARD | ControlFlags::TYPE_CHANGE;
        assert!(flags.contains(ControlFlags::STANDARD));
        assert!(flags.contains(ControlFlags::TYPE_CHANGE));
        assert!(!flags.contains(ControlFlags::BOUNDS));
    }

    #[test]
    fn test_primary_order_type_values() {
        assert_eq!(PrimaryOrderType::DstBlt as u8, 0x00);
        assert_eq!(PrimaryOrderType::PatBlt as u8, 0x01);
        assert_eq!(PrimaryOrderType::ScrBlt as u8, 0x02);
        assert_eq!(PrimaryOrderType::LineTo as u8, 0x09);
        assert_eq!(PrimaryOrderType::OpaqueRect as u8, 0x0A);
    }

    #[test]
    fn test_drawing_order_state_default() {
        let state = DrawingOrderState::new();
        assert!(state.primary_order_type.is_none());
        assert_eq!(state.bounds.left, 0);
        assert_eq!(state.bounds.top, 0);
    }

    #[test]
    fn test_secondary_order_type_values() {
        assert_eq!(SecondaryOrderType::CacheBitmap as u8, 0x00);
        assert_eq!(SecondaryOrderType::CacheBrush as u8, 0x07);
    }

    #[test]
    fn test_alternate_secondary_order_type_values() {
        assert_eq!(AlternateSecondaryOrderType::FrameMarker as u8, 0x0D);
    }

    #[test]
    fn test_decode_dstblt_all_fields() {
        let mut order = DstBltOrder::default();
        // All 5 fields present: x, y, width, height, rop
        let data = [
            0x0A, 0x00, // x = 10
            0x14, 0x00, // y = 20
            0x64, 0x00, // width = 100
            0x32, 0x00, // height = 50
            0xCC, // rop = 0xCC (SRCCOPY)
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_dstblt_order(&mut cursor, &mut order, 0x1F).unwrap();

        assert_eq!(order.x, 10);
        assert_eq!(order.y, 20);
        assert_eq!(order.width, 100);
        assert_eq!(order.height, 50);
        assert_eq!(order.rop, 0xCC);
    }

    #[test]
    fn test_decode_dstblt_partial_fields() {
        let mut order = DstBltOrder {
            x: 5,
            y: 10,
            width: 50,
            height: 25,
            rop: 0x00,
        };
        // Only x and rop present (flags 0x11)
        let data = [
            0x14, 0x00, // x = 20
            0xAA, // rop = 0xAA
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_dstblt_order(&mut cursor, &mut order, 0x11).unwrap();

        assert_eq!(order.x, 20); // Updated
        assert_eq!(order.y, 10); // Unchanged
        assert_eq!(order.width, 50); // Unchanged
        assert_eq!(order.height, 25); // Unchanged
        assert_eq!(order.rop, 0xAA); // Updated
    }

    #[test]
    fn test_decode_scrblt_all_fields() {
        let mut order = ScrBltOrder::default();
        // All 7 fields: x, y, width, height, rop, src_x, src_y
        let data = [
            0x10, 0x00, // x = 16
            0x20, 0x00, // y = 32
            0x40, 0x00, // width = 64
            0x30, 0x00, // height = 48
            0xCC, // rop
            0x00, 0x01, // src_x = 256
            0x80, 0x00, // src_y = 128
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_scrblt_order(&mut cursor, &mut order, 0x7F).unwrap();

        assert_eq!(order.x, 16);
        assert_eq!(order.y, 32);
        assert_eq!(order.width, 64);
        assert_eq!(order.height, 48);
        assert_eq!(order.rop, 0xCC);
        assert_eq!(order.src_x, 256);
        assert_eq!(order.src_y, 128);
    }

    #[test]
    fn test_decode_opaque_rect_with_color() {
        let mut order = OpaqueRectOrder::default();
        // All 7 fields: x, y, width, height, R, G, B
        let data = [
            0x0A, 0x00, // x = 10
            0x14, 0x00, // y = 20
            0x64, 0x00, // width = 100
            0x32, 0x00, // height = 50
            0xFF, // Red
            0x80, // Green
            0x00, // Blue
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_opaque_rect_order(&mut cursor, &mut order, 0x7F).unwrap();

        assert_eq!(order.x, 10);
        assert_eq!(order.y, 20);
        assert_eq!(order.width, 100);
        assert_eq!(order.height, 50);
        // Color is stored as 0xRRGGBB
        assert_eq!(order.color, 0xFF8000);
    }

    #[test]
    fn test_decode_opaque_rect_partial_color() {
        let mut order = OpaqueRectOrder {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            color: 0x112233, // Initial color
        };
        // Only update green (flag 0x20)
        let data = [0xAA]; // Green = 0xAA
        let mut cursor = ReadCursor::new(&data);
        decode_opaque_rect_order(&mut cursor, &mut order, 0x20).unwrap();

        // Red and Blue should be preserved, only Green updated
        assert_eq!(order.color, 0x11AA33);
    }

    #[test]
    fn test_decode_patblt_with_brush() {
        let mut order = PatBltOrder::default();
        // Fields: x, y, width, height, rop, back_color, fore_color, brush fields
        let data = [
            0x10, 0x00, // x = 16
            0x20, 0x00, // y = 32
            0x40, 0x00, // width = 64
            0x30, 0x00, // height = 48
            0xF0, // rop (PATCOPY)
            0x00, 0x00, 0xFF, // back_color (red in BGR)
            0xFF, 0x00, 0x00, // fore_color (blue in BGR)
            0x05, // brush org_x
            0x03, // brush org_y
            0x02, // brush style (BS_HATCHED)
            0x01, // brush hatch (HS_HORIZONTAL)
        ];
        let mut cursor = ReadCursor::new(&data);
        // Flags for all fields up to hatch (0x07FF)
        decode_patblt_order(&mut cursor, &mut order, 0x07FF).unwrap();

        assert_eq!(order.x, 16);
        assert_eq!(order.y, 32);
        assert_eq!(order.width, 64);
        assert_eq!(order.height, 48);
        assert_eq!(order.rop, 0xF0);
        assert_eq!(order.back_color, 0xFF0000); // Red
        assert_eq!(order.fore_color, 0x0000FF); // Blue
        assert_eq!(order.brush.org_x, 5);
        assert_eq!(order.brush.org_y, 3);
        assert_eq!(order.brush.style, 2);
        assert_eq!(order.brush.hatch, 1);
    }

    #[test]
    fn test_decode_primary_order_dstblt() {
        let mut state = DrawingOrderState::new();
        // STANDARD | TYPE_CHANGE flags, DstBlt type, all fields
        let data = [
            0x09, // controlFlags: STANDARD | TYPE_CHANGE
            0x00, // orderType: DstBlt
            0x1F, // field flags: all 5 fields
            0x0A, 0x00, // x = 10
            0x14, 0x00, // y = 20
            0x64, 0x00, // width = 100
            0x32, 0x00, // height = 50
            0xCC, // rop
        ];
        let mut cursor = ReadCursor::new(&data);
        let order = decode_order(&mut cursor, &mut state).unwrap();

        if let DrawingOrder::Primary(primary) = order {
            assert_eq!(primary.order_type, PrimaryOrderType::DstBlt);
            if let PrimaryOrderData::DstBlt(dstblt) = primary.data {
                assert_eq!(dstblt.x, 10);
                assert_eq!(dstblt.y, 20);
                assert_eq!(dstblt.width, 100);
                assert_eq!(dstblt.height, 50);
                assert_eq!(dstblt.rop, 0xCC);
            } else {
                panic!("Expected DstBlt data");
            }
        } else {
            panic!("Expected Primary order");
        }

        // State should be updated
        assert_eq!(state.primary_order_type, Some(PrimaryOrderType::DstBlt));
    }

    #[test]
    fn test_decode_primary_order_uses_previous_type() {
        let mut state = DrawingOrderState::new();
        state.primary_order_type = Some(PrimaryOrderType::DstBlt);
        state.dstblt = DstBltOrder {
            x: 10,
            y: 20,
            width: 100,
            height: 50,
            rop: 0xCC,
        };

        // STANDARD flag only (no TYPE_CHANGE), update only x and y
        let data = [
            0x01, // controlFlags: STANDARD only
            0x03, // field flags: x and y
            0x14, 0x00, // x = 20
            0x28, 0x00, // y = 40
        ];
        let mut cursor = ReadCursor::new(&data);
        let order = decode_order(&mut cursor, &mut state).unwrap();

        if let DrawingOrder::Primary(primary) = order {
            assert_eq!(primary.order_type, PrimaryOrderType::DstBlt);
            if let PrimaryOrderData::DstBlt(dstblt) = primary.data {
                assert_eq!(dstblt.x, 20); // Updated
                assert_eq!(dstblt.y, 40); // Updated
                assert_eq!(dstblt.width, 100); // From previous state
                assert_eq!(dstblt.height, 50); // From previous state
                assert_eq!(dstblt.rop, 0xCC); // From previous state
            } else {
                panic!("Expected DstBlt data");
            }
        } else {
            panic!("Expected Primary order");
        }
    }

    #[test]
    fn test_field_flag_bytes() {
        assert_eq!(field_flag_bytes(PrimaryOrderType::DstBlt), 1);
        assert_eq!(field_flag_bytes(PrimaryOrderType::PatBlt), 2);
        assert_eq!(field_flag_bytes(PrimaryOrderType::ScrBlt), 1);
        assert_eq!(field_flag_bytes(PrimaryOrderType::OpaqueRect), 1);
        assert_eq!(field_flag_bytes(PrimaryOrderType::LineTo), 2);
        assert_eq!(field_flag_bytes(PrimaryOrderType::MultiDstBlt), 1);
        assert_eq!(field_flag_bytes(PrimaryOrderType::MultiOpaqueRect), 2);
        assert_eq!(field_flag_bytes(PrimaryOrderType::Polyline), 1);
    }

    #[test]
    fn test_decode_line_to_order() {
        let mut order = LineToOrder::default();
        // All 10 fields: BackMode, start_x, start_y, end_x, end_y, BackColor, ROP2, PenStyle, PenWidth, PenColor
        let data = [
            0x01, 0x00, // BackMode = 1 (TRANSPARENT)
            0x0A, 0x00, // start_x = 10
            0x14, 0x00, // start_y = 20
            0x64, 0x00, // end_x = 100
            0x32, 0x00, // end_y = 50
            0x00, 0x00, 0xFF, // BackColor (BGR = red)
            0x0D, // ROP2
            0x00, // PenStyle
            0x01, // PenWidth
            0x00, 0xFF, 0x00, // PenColor (BGR = green)
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_line_to_order(&mut cursor, &mut order, 0x03FF).unwrap();

        assert_eq!(order.back_mode, 1);
        assert_eq!(order.start_x, 10);
        assert_eq!(order.start_y, 20);
        assert_eq!(order.end_x, 100);
        assert_eq!(order.end_y, 50);
        assert_eq!(order.back_color, 0xFF0000); // Red in RGB
        assert_eq!(order.rop2, 0x0D);
        assert_eq!(order.pen_style, 0);
        assert_eq!(order.pen_width, 1);
        assert_eq!(order.pen_color, 0x00FF00); // Green in RGB
    }

    #[test]
    fn test_decode_line_to_partial() {
        let mut order = LineToOrder {
            back_mode: 1,
            start_x: 10,
            start_y: 20,
            end_x: 100,
            end_y: 50,
            back_color: 0,
            rop2: 0x0D,
            pen_style: 0,
            pen_width: 1,
            pen_color: 0xFF0000,
        };
        // Update only end coordinates (fields 4 and 5)
        let data = [
            0xC8, 0x00, // end_x = 200
            0x96, 0x00, // end_y = 150
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_line_to_order(&mut cursor, &mut order, 0x0018).unwrap(); // Only bits 3,4 set

        assert_eq!(order.start_x, 10); // Unchanged
        assert_eq!(order.start_y, 20); // Unchanged
        assert_eq!(order.end_x, 200); // Updated
        assert_eq!(order.end_y, 150); // Updated
        assert_eq!(order.pen_color, 0xFF0000); // Unchanged
    }

    #[test]
    fn test_decode_multi_opaque_rect_order() {
        let mut order = MultiOpaqueRectOrder::default();
        // Basic fields plus 2 delta rectangles
        let data = [
            0x00, 0x00, // x = 0
            0x00, 0x00, // y = 0
            0x64, 0x00, // width = 100
            0x64, 0x00, // height = 100
            0xFF, // Red
            0x00, // Green
            0x00, // Blue
            0x02, // nDeltaEntries = 2
            // zeroBits for 2 rectangles (1 byte)
            0x00, // All deltas are 1-byte
            // Rectangle 1
            10, 20, 30, 40, // left, top, width, height
            // Rectangle 2 (delta from prev)
            10, 10, 30, 40, // left, top, width, height
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_multi_opaque_rect_order(&mut cursor, &mut order, 0x01FF).unwrap();

        assert_eq!(order.x, 0);
        assert_eq!(order.y, 0);
        assert_eq!(order.width, 100);
        assert_eq!(order.height, 100);
        assert_eq!(order.color, 0xFF0000); // Red in RGB
        assert_eq!(order.rectangles.len(), 2);
        assert_eq!(order.rectangles[0].left, 10);
        assert_eq!(order.rectangles[0].top, 20);
        assert_eq!(order.rectangles[0].width, 30);
        assert_eq!(order.rectangles[0].height, 40);
        // Second rectangle: left = 10 + 10 = 20, top = 20 + 10 = 30
        assert_eq!(order.rectangles[1].left, 20);
        assert_eq!(order.rectangles[1].top, 30);
    }

    #[test]
    fn test_decode_polyline_order() {
        let mut order = PolylineOrder::default();
        // Basic fields plus 3 points
        let data = [
            0x32, 0x00, // x = 50 (start)
            0x32, 0x00, // y = 50 (start)
            0x0D, // ROP2
            0x00, 0x00, // BrushCacheEntry
            0x00, 0x00, 0xFF, // PenColor (BGR = red)
            0x03, // nDeltaEntries = 3
            // zeroBits for 3 points (1 byte = 4 points capacity)
            0x00, // All deltas are 1-byte
            // Point 1: delta (10, 0)
            10, 0, // Point 2: delta (0, 10)
            0, 10, // Point 3: delta (-10, -10)
            246, 246, // -10 as unsigned byte
        ];
        let mut cursor = ReadCursor::new(&data);
        decode_polyline_order(&mut cursor, &mut order, 0x7F).unwrap();

        assert_eq!(order.x, 50);
        assert_eq!(order.y, 50);
        assert_eq!(order.rop2, 0x0D);
        assert_eq!(order.pen_color, 0xFF0000); // Red in RGB
        assert_eq!(order.points.len(), 3);
        assert_eq!(order.points[0].x, 10);
        assert_eq!(order.points[0].y, 0);
        assert_eq!(order.points[1].x, 0);
        assert_eq!(order.points[1].y, 10);
        assert_eq!(order.points[2].x, -10); // Sign extended
        assert_eq!(order.points[2].y, -10);
    }

    #[test]
    fn test_delta_point_decoding() {
        // Test the delta point decoding directly
        let data = [
            0x00, // zeroBits: all 1-byte deltas
            10, 20, // Point 0: (10, 20)
            246, 236, // Point 1: (-10 as u8, -20 as u8)
        ];
        let mut cursor = ReadCursor::new(&data);
        let points = decode_delta_points(&mut cursor, 2).unwrap();

        assert_eq!(points.len(), 2);
        assert_eq!(points[0].x, 10);
        assert_eq!(points[0].y, 20);
        assert_eq!(points[1].x, -10);
        assert_eq!(points[1].y, -20);
    }

    #[test]
    fn test_delta_rect_decoding() {
        // Test the delta rect decoding directly
        let data = [
            0x00, // zeroBits: all 1-byte deltas
            10, 20, 30, 40, // Rect 0
        ];
        let mut cursor = ReadCursor::new(&data);
        let rects = decode_delta_rects(&mut cursor, 1).unwrap();

        assert_eq!(rects.len(), 1);
        assert_eq!(rects[0].left, 10);
        assert_eq!(rects[0].top, 20);
        assert_eq!(rects[0].width, 30);
        assert_eq!(rects[0].height, 40);
    }

    #[test]
    fn test_new_order_types_in_state() {
        // Ensure all new order types are present in DrawingOrderState
        let state = DrawingOrderState::default();

        // These should all exist and be default
        assert_eq!(state.line_to.start_x, 0);
        assert_eq!(state.multi_opaque_rect.rectangles.len(), 0);
        assert_eq!(state.multi_dstblt.rectangles.len(), 0);
        assert_eq!(state.multi_patblt.rectangles.len(), 0);
        assert_eq!(state.multi_scrblt.rectangles.len(), 0);
        assert_eq!(state.polyline.points.len(), 0);
    }
}

# Future Work: Drawing Orders & Graphics Updates

This document outlines remaining work for full drawing order support in IronRDP.

## Currently Implemented

### Primary Drawing Orders
| Order | Status | Notes |
|-------|--------|-------|
| DstBlt | ✅ Partial | ROPs: BLACKNESS (0x00), DSTINVERT (0x55), WHITENESS (0xFF) |
| PatBlt | ✅ Partial | ROPs: BLACKNESS, DSTINVERT, PATCOPY (0xF0), WHITENESS. Solid brush only. |
| ScrBlt | ✅ Partial | SRCCOPY (0xCC) only |
| OpaqueRect | ✅ Complete | |
| LineTo | ✅ Complete | Bresenham's algorithm |
| Polyline | ✅ Complete | Delta-encoded points |
| MultiOpaqueRect | ✅ Complete | |
| MultiDstBlt | ✅ Partial | Same ROP limitations as DstBlt |
| MultiPatBlt | ✅ Partial | Same limitations as PatBlt |
| MultiScrBlt | ✅ Partial | Same limitations as ScrBlt |

### Other Features
- ✅ Slow-path (X224) graphics update routing
- ✅ 8bpp palette support
- ✅ Delta encoding for coordinates
- ✅ Bounds rectangle handling

## Not Yet Implemented

### 1. Secondary Drawing Orders (Caching)

Secondary orders cache resources on the client for later use. Currently parsed but not acted upon.

**Orders to implement:**

| Order | Purpose | Priority |
|-------|---------|----------|
| CacheBitmap | Cache bitmap tiles | High |
| CacheBitmapV2 | Improved bitmap caching | High |
| CacheBitmapV3 | Extended bitmap caching | Medium |
| CacheColorTable | Cache color palettes | Medium |
| CacheGlyph | Cache font glyphs | High |
| CacheGlyphV2 | Improved glyph caching | High |
| CacheBrush | Cache brush patterns | Medium |

**Implementation approach:**
```rust
// In fast_path.rs, add cache structures:
pub struct BitmapCache {
    // Cache ID -> Vec of cached bitmaps
    caches: [Vec<Option<CachedBitmap>>; 3],
}

pub struct GlyphCache {
    // Fragment cache + glyph caches
    fragments: Vec<Option<GlyphFragment>>,
    glyphs: [Vec<Option<CachedGlyph>>; 10],
}

// Then handle in process_drawing_order:
DrawingOrder::Secondary(secondary) => {
    match secondary.order_type {
        SecondaryOrderType::CacheBitmapV2 => {
            let cached = CacheBitmapV2::decode(secondary.data)?;
            self.bitmap_cache.store(cached.cache_id, cached.cache_index, bitmap);
        }
        // ...
    }
}
```

### 2. Additional Primary Orders

| Order | Purpose | Complexity |
|-------|---------|------------|
| MemBlt | Memory block transfer (uses cached bitmap) | Medium |
| Mem3Blt | 3-way MemBlt with ROP | Medium |
| SaveBitmap | Save/restore screen regions | Low |
| GlyphIndex | Render cached glyphs (text) | High |
| FastIndex | Optimized glyph rendering | High |
| FastGlyph | Single glyph rendering | Medium |
| EllipseSC | Ellipse stroke | Low |
| EllipseCB | Ellipse fill | Low |

**Text rendering (GlyphIndex/FastIndex):**
```rust
// Requires glyph cache to be populated first
PrimaryOrderData::GlyphIndex(glyph) => {
    let cache = &self.glyph_cache.glyphs[glyph.cache_id];
    for glyph_data in decode_glyph_fragments(&glyph.data) {
        let cached = cache[glyph_data.index]?;
        // Render glyph bitmap at position with foreground/background colors
        image.draw_glyph(x, y, &cached, glyph.fore_color, glyph.back_color)?;
    }
}
```

### 3. Full ROP (Raster Operation) Support

Currently only a few ROPs are implemented. Full support requires:

```rust
/// Apply a ternary raster operation
fn apply_rop3(dst: u8, src: u8, pattern: u8, rop: u8) -> u8 {
    // ROP code encodes truth table for combining dst, src, pattern
    let mut result = 0u8;
    for bit in 0..8 {
        let d = (dst >> bit) & 1;
        let s = (src >> bit) & 1;
        let p = (pattern >> bit) & 1;
        let index = (d << 2) | (s << 1) | p;
        result |= ((rop >> index) & 1) << bit;
    }
    result
}

// Common ROPs:
// 0x00 - BLACKNESS (0)
// 0x55 - DSTINVERT (~D)
// 0xAA - NOP (D)
// 0xCC - SRCCOPY (S)
// 0xF0 - PATCOPY (P)
// 0xFF - WHITENESS (1)
// 0x5A - PATINVERT (D ^ P)
// 0x66 - SRCINVERT (D ^ S)
// ... 256 total combinations
```

### 4. Brush Pattern Support

Currently only solid brushes (style 0) are supported.

```rust
pub struct Brush {
    pub style: u8,      // 0=solid, 1=null, 2=hatched, 3=pattern
    pub hatch: u8,      // Hatch pattern index (for style 2)
    pub extra: [u8; 7], // 8x8 monochrome pattern (for style 3)
    pub org_x: i8,      // Pattern origin
    pub org_y: i8,
}

// Pattern rendering:
fn get_pattern_pixel(brush: &Brush, x: i32, y: i32) -> bool {
    let px = ((x - brush.org_x as i32) % 8) as usize;
    let py = ((y - brush.org_y as i32) % 8) as usize;
    
    match brush.style {
        0 => true, // Solid - always foreground
        1 => false, // Null - always background
        2 => HATCH_PATTERNS[brush.hatch as usize][py] & (1 << px) != 0,
        3 => brush.extra[py] & (1 << px) != 0,
        _ => true,
    }
}
```

### 5. Alternate Secondary Orders

Frame markers and other control orders:

| Order | Purpose |
|-------|---------|
| FrameMarker | Begin/end frame for synchronization |
| CreateOffscreenBitmap | Off-screen rendering surface |
| SwitchSurface | Switch between surfaces |
| CreateNineGridBitmap | Nine-grid scaling bitmap |
| StreamBitmapFirst/Next | Streaming bitmap transfer |
| DrawGdiPlusFirst/Next/End | GDI+ rendering commands |

### 6. Off-Screen Bitmap Support

Some servers use off-screen surfaces for compositing:

```rust
pub struct OffscreenSurface {
    pub id: u16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,
}

pub struct SurfaceManager {
    pub primary: DecodedImage,  // The visible screen
    pub offscreen: HashMap<u16, OffscreenSurface>,
    pub current_surface: Option<u16>,  // None = primary
}
```

## Implementation Priority

### Phase 1: Core Caching (High Impact)
1. Bitmap caching (CacheBitmapV2)
2. MemBlt order (uses cached bitmaps)
3. This enables most basic RDP scenarios efficiently

### Phase 2: Text Rendering (Medium Impact)
1. Glyph caching (CacheGlyph, CacheGlyphV2)
2. GlyphIndex/FastIndex orders
3. Enables readable text in legacy mode

### Phase 3: Full ROP Support (Low-Medium Impact)
1. Implement full ROP3 engine
2. Update all Blt orders to use it
3. Mostly needed for complex GDI applications

### Phase 4: Advanced Features (Low Impact)
1. Brush patterns
2. Ellipse orders
3. Off-screen surfaces
4. GDI+ rendering

## Testing Strategy

### Unit Tests
- Each order type needs decode/encode round-trip tests
- Cache eviction and lookup tests
- ROP truth table verification

### Integration Tests
- Capture real RDP traffic with `rdp-rs` or Wireshark
- Replay against implementation
- Visual comparison of rendered output

### Fuzz Testing
- Already have fuzz targets in `/fuzz`
- Add targets for drawing order parsing
- Test malformed delta encodings

## References

- [MS-RDPEGDI]: Primary, Secondary, Alternate Secondary Drawing Orders
- [MS-RDPBCGR]: Basic Connectivity and Graphics Remoting
- FreeRDP source: `libfreerdp/core/orders.c` - reference implementation
- rdesktop source: `orders.c` - simpler reference

[MS-RDPEGDI]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpegdi/
[MS-RDPBCGR]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/

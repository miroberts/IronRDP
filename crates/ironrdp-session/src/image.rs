use std::sync::Arc;

use ironrdp_core::assert_impl;
use ironrdp_graphics::color_conversion::{rdp_15bit_to_rgb, rdp_16bit_to_rgb};
use ironrdp_graphics::image_processing::{ImageRegion, ImageRegionMut, PixelFormat};
use ironrdp_graphics::pointer::DecodedPointer;
use ironrdp_graphics::rectangle_processing::Region;
use ironrdp_pdu::geometry::{InclusiveRectangle, Rectangle as _};
use tracing::{debug, trace};

use crate::{SessionResult, custom_err};

const TILE_SIZE: u16 = 64;

pub struct DecodedImage {
    pixel_format: PixelFormat,
    data: Vec<u8>,

    /// Part of the pointer image which should be drawn
    pointer_src_rect: InclusiveRectangle,
    /// X position of the pointer sprite on the screen
    pointer_draw_x: u16,
    /// Y position of the pointer sprite on the screen
    pointer_draw_y: u16,

    pointer_x: u16,
    pointer_y: u16,

    pointer: Option<Arc<DecodedPointer>>,
    /// Image data, overridden by pointer. Used to restore image after pointer was hidden or moved
    pointer_backbuffer: Vec<u8>,
    /// Whether to show pointer or not
    show_pointer: bool,
    /// Whether pointer is visible on the screen or its sprite is currently out of bounds
    pointer_visible_on_screen: bool,

    width: u16,
    height: u16,
}

assert_impl!(DecodedImage: Send);

impl core::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("pixel_format", &self.pixel_format)
            .field("data_len", &self.data.len())
            .field("pointer_src_rect", &self.pointer_src_rect)
            .field("pointer_draw_x", &self.pointer_draw_x)
            .field("pointer_draw_y", &self.pointer_draw_y)
            .field("pointer_x", &self.pointer_x)
            .field("pointer_y", &self.pointer_y)
            .field("pointer", &self.pointer)
            .field("pointer_backbuffer", &self.pointer_backbuffer)
            .field("show_pointer", &self.show_pointer)
            .field("pointer_visible_on_screen", &self.pointer_visible_on_screen)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

#[derive(PartialEq, Eq)]
enum PointerLayer {
    Background,
    Pointer,
}

struct PointerRenderingState {
    redraw: bool,
    update_rectangle: InclusiveRectangle,
}

#[expect(clippy::too_many_arguments)]
fn copy_cursor_data(
    from: &[u8],
    from_pos: (usize, usize),
    from_stride: usize,
    to: &mut [u8],
    to_stride: usize,
    to_pos: (usize, usize),
    size: (usize, usize),
    dst_size: (usize, usize),
    composite: bool,
) {
    const PIXEL_SIZE: usize = 4;

    if to_pos.0 + size.0 > dst_size.0 || to_pos.1 + size.1 > dst_size.1 {
        // Perform clipping
        return;
    }

    let (from_x, from_y) = from_pos;
    let (to_x, to_y) = to_pos;
    let (width, height) = size;

    for y in 0..height {
        let from_start = (from_y + y) * from_stride + from_x * PIXEL_SIZE;
        let to_start = (to_y + y) * to_stride + to_x * PIXEL_SIZE;

        if composite {
            for pixel in 0..width {
                let dest_r = to[to_start + pixel * PIXEL_SIZE];
                let dest_g = to[to_start + pixel * PIXEL_SIZE + 1];
                let dest_b = to[to_start + pixel * PIXEL_SIZE + 2];

                let src_r = from[from_start + pixel * PIXEL_SIZE];
                let src_g = from[from_start + pixel * PIXEL_SIZE + 1];
                let src_b = from[from_start + pixel * PIXEL_SIZE + 2];
                let src_a = from[from_start + pixel * PIXEL_SIZE + 3];

                // Inverted pixel, this color has a special meaning when encoded by ironrdp-graphics
                if src_a == 0 && src_r == 255 && src_g == 255 && src_b == 255 {
                    to[to_start + pixel * PIXEL_SIZE] = 255 - dest_r;
                    to[to_start + pixel * PIXEL_SIZE + 1] = 255 - dest_g;
                    to[to_start + pixel * PIXEL_SIZE + 2] = 255 - dest_b;
                    to[to_start + pixel * PIXEL_SIZE + 3] = 255;
                    continue;
                }

                // Skip 100% transparent pixels
                if src_a == 0 {
                    continue;
                }

                #[expect(clippy::as_conversions, reason = "(u16 >> 8) fits into u8 + hot loop")]
                {
                    // Integer alpha blending, source represented as premultiplied alpha color, calculation in floating point
                    to[to_start + pixel * PIXEL_SIZE] =
                        src_r + ((u16::from(dest_r) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 1] =
                        src_g + ((u16::from(dest_g) * u16::from(255 - src_a)) >> 8) as u8;
                    to[to_start + pixel * PIXEL_SIZE + 2] =
                        src_b + ((u16::from(dest_b) * u16::from(255 - src_a)) >> 8) as u8;
                    // Framebuffer is always opaque, so we can skip alpha channel change
                }
            }
        } else {
            to[to_start..to_start + width * PIXEL_SIZE]
                .copy_from_slice(&from[from_start..from_start + width * PIXEL_SIZE]);
        }
    }
}

impl DecodedImage {
    pub fn new(pixel_format: PixelFormat, width: u16, height: u16) -> Self {
        let len = usize::from(width) * usize::from(height) * usize::from(pixel_format.bytes_per_pixel());

        Self {
            pixel_format,
            data: vec![0; len],
            width,
            height,

            pointer_src_rect: InclusiveRectangle {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            pointer_x: 0,
            pointer_y: 0,
            pointer_draw_x: 0,
            pointer_draw_y: 0,
            pointer_backbuffer: Vec::new(),
            pointer: None,
            show_pointer: false,
            pointer_visible_on_screen: true,
        }
    }

    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.pixel_format.bytes_per_pixel())
    }

    pub fn stride(&self) -> usize {
        usize::from(self.width) * self.bytes_per_pixel()
    }

    pub fn data_for_rect(&self, rect: &InclusiveRectangle) -> &[u8] {
        let start = usize::from(rect.left) * self.bytes_per_pixel() + usize::from(rect.top) * self.stride();
        let end =
            start + usize::from(rect.height() - 1) * self.stride() + usize::from(rect.width()) * self.bytes_per_pixel();
        &self.data[start..end]
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Returns `true` if the rectangle fits entirely within the image bounds.
    fn rect_fits(&self, rect: &InclusiveRectangle) -> bool {
        rect.right < self.width && rect.bottom < self.height
    }

    fn apply_pointer_layer(&mut self, layer: PointerLayer) -> SessionResult<Option<InclusiveRectangle>> {
        // Pointer is not hidden, but its texture is not visible on the screen, so we don't
        // need to render it
        if layer == PointerLayer::Pointer && !self.pointer_visible_on_screen {
            return Ok(None);
        }

        if self.data.is_empty() {
            return Ok(None);
        }

        let pointer = if let Some(pointer) = &self.pointer {
            pointer
        } else {
            return Ok(None);
        };

        if self.pointer_src_rect.width() == 0 || self.pointer_src_rect.height() == 0 {
            return Ok(None);
        }

        let dest_rect = InclusiveRectangle {
            left: self.pointer_draw_x,
            top: self.pointer_draw_y,
            right: self.pointer_draw_x + self.pointer_src_rect.width() - 1,
            bottom: self.pointer_draw_y + self.pointer_src_rect.height() - 1,
        };

        if dest_rect.width() == 0 || dest_rect.height() == 0 {
            return Ok(None);
        }

        let pointer_src_rect_width = usize::from(self.pointer_src_rect.width());
        let pointer_src_rect_height = usize::from(self.pointer_src_rect.height());
        let pointer_draw_x = usize::from(self.pointer_draw_x);
        let pointer_draw_y = usize::from(self.pointer_draw_y);
        let width = usize::from(self.width);
        let height = usize::from(self.height);

        match &layer {
            PointerLayer::Background => {
                if self.pointer_backbuffer.is_empty() {
                    // Backbuffer were previously empty
                    return Ok(None);
                }

                copy_cursor_data(
                    &self.pointer_backbuffer,
                    (0, 0),
                    pointer_src_rect_width * 4,
                    &mut self.data,
                    width * 4,
                    (pointer_draw_x, pointer_draw_y),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    false,
                );
            }
            PointerLayer::Pointer => {
                // Copy current background to backbuffer
                let buffer_size = self
                    .pointer_backbuffer
                    .len()
                    .max(pointer_src_rect_width * pointer_src_rect_height * 4);
                self.pointer_backbuffer.resize(buffer_size, 0);

                copy_cursor_data(
                    &self.data,
                    (pointer_draw_x, pointer_draw_y),
                    width * 4,
                    &mut self.pointer_backbuffer,
                    pointer_src_rect_width * 4,
                    (0, 0),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    false,
                );

                // Draw pointer (with compositing)
                copy_cursor_data(
                    pointer.bitmap_data.as_slice(),
                    (
                        usize::from(self.pointer_src_rect.left),
                        usize::from(self.pointer_src_rect.top),
                    ),
                    usize::from(pointer.width) * 4,
                    &mut self.data,
                    width * 4,
                    (pointer_draw_x, pointer_draw_y),
                    (pointer_src_rect_width, pointer_src_rect_height),
                    (width, height),
                    true,
                );
            }
        }

        // Request redraw of the changed area
        Ok(Some(dest_rect))
    }

    pub(crate) fn show_pointer(&mut self) -> SessionResult<Option<InclusiveRectangle>> {
        if !self.show_pointer {
            self.show_pointer = true;
            self.apply_pointer_layer(PointerLayer::Pointer)
        } else {
            Ok(None)
        }
    }

    pub(crate) fn hide_pointer(&mut self) -> SessionResult<Option<InclusiveRectangle>> {
        if self.show_pointer {
            self.show_pointer = false;
            self.apply_pointer_layer(PointerLayer::Background)
        } else {
            Ok(None)
        }
    }

    fn recalculate_pointer_geometry(&mut self) {
        let x = self.pointer_x;
        let y = self.pointer_y;

        let pointer = match &self.pointer {
            Some(pointer) if self.show_pointer => pointer,
            _ => return,
        };

        let left_virtual = i32::from(x) - i32::from(pointer.hotspot_x);
        let top_virtual = i32::from(y) - i32::from(pointer.hotspot_y);
        let right_virtual = left_virtual + i32::from(pointer.width) - 1;
        let bottom_virtual = top_virtual + i32::from(pointer.height) - 1;

        let (left, draw_x) = if left_virtual < 0 {
            // Cut left side if required
            (pointer.hotspot_x - x, 0)
        } else {
            (0, x - pointer.hotspot_x)
        };

        let (top, draw_y) = if top_virtual < 0 {
            // Cut top side if required
            (pointer.hotspot_y - y, 0)
        } else {
            (0, y - pointer.hotspot_y)
        };

        // Cut right side if required
        let right = if right_virtual >= i32::from(self.width - 1) {
            if draw_x + 1 >= self.width {
                // Pointer is completely out of bounds horizontally
                self.pointer_visible_on_screen = false;
                return;
            } else {
                self.width - (draw_x + 1)
            }
        } else {
            pointer.width - 1
        };

        // Cut bottom side if required
        let bottom = if bottom_virtual >= i32::from(self.height - 1) {
            if (draw_y + 1) >= self.height {
                // Pointer is completely out of bounds vertically
                self.pointer_visible_on_screen = false;
                return;
            } else {
                self.height - (draw_y + 1)
            }
        } else {
            pointer.height - 1
        };

        self.pointer_visible_on_screen = true;

        let pointer_src_rect = InclusiveRectangle {
            left,
            top,
            right,
            bottom,
        };

        self.pointer_src_rect = pointer_src_rect;
        self.pointer_draw_x = draw_x;
        self.pointer_draw_y = draw_y;
    }

    pub(crate) fn move_pointer(&mut self, x: u16, y: u16) -> SessionResult<Option<InclusiveRectangle>> {
        self.pointer_x = x;
        self.pointer_y = y;

        if self.pointer.is_some() && self.show_pointer {
            let old_rect = self.apply_pointer_layer(PointerLayer::Background)?;
            self.recalculate_pointer_geometry();
            let new_rect = self.apply_pointer_layer(PointerLayer::Pointer)?;

            match (old_rect, new_rect) {
                (None, None) => Ok(None),
                (None, Some(rect)) => Ok(Some(rect)),
                (Some(rect), None) => Ok(Some(rect)),
                (Some(a), Some(b)) => Ok(Some(a.union(&b))),
            }
        } else {
            Ok(None)
        }
    }

    pub(crate) fn update_pointer(&mut self, pointer: Arc<DecodedPointer>) -> SessionResult<Option<InclusiveRectangle>> {
        self.show_pointer = true;

        // Remove old pointer from frame buffer
        let old_rect = if self.pointer.is_some() {
            self.apply_pointer_layer(PointerLayer::Background)?
        } else {
            None
        };

        self.pointer = Some(pointer);
        self.recalculate_pointer_geometry();

        // Draw new pointer
        let new_rect = self.apply_pointer_layer(PointerLayer::Pointer)?;

        match (old_rect, new_rect) {
            (None, None) => Ok(None),
            (None, Some(rect)) => Ok(Some(rect)),
            (Some(rect), None) => Ok(Some(rect)),
            (Some(a), Some(b)) => Ok(Some(a.union(&b))),
        }
    }

    fn is_pointer_redraw_required(&self, update_rectangle: &InclusiveRectangle) -> bool {
        let pointer_dest_rect = InclusiveRectangle {
            left: self.pointer_draw_x,
            top: self.pointer_draw_y,
            right: self.pointer_draw_x + self.pointer_src_rect.width() - 1,
            bottom: self.pointer_draw_y + self.pointer_src_rect.height() - 1,
        };

        update_rectangle.intersect(&pointer_dest_rect).is_some() && self.show_pointer
    }

    /// This method should be called BEFORE and framebuffer updates, with the update rectangle,
    /// to determine if the pointer needs to be redrawn (overlapping with the update rectangle).
    fn pointer_rendering_begin(
        &mut self,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<PointerRenderingState> {
        if !self.is_pointer_redraw_required(update_rectangle) || self.pointer.is_none() {
            return Ok(PointerRenderingState {
                redraw: false,
                update_rectangle: update_rectangle.clone(),
            });
        }

        let state = self
            .apply_pointer_layer(PointerLayer::Background)?
            .map(|cursor_erase_rect| PointerRenderingState {
                redraw: true,
                update_rectangle: cursor_erase_rect.union(update_rectangle),
            })
            .unwrap_or_else(|| PointerRenderingState {
                redraw: false,
                update_rectangle: update_rectangle.clone(),
            });

        Ok(state)
    }

    fn pointer_rendering_end(
        &mut self,
        pointer_rendering_state: PointerRenderingState,
    ) -> SessionResult<InclusiveRectangle> {
        if !pointer_rendering_state.redraw {
            return Ok(pointer_rendering_state.update_rectangle);
        }

        let update_rectangle = self
            .apply_pointer_layer(PointerLayer::Pointer)?
            .map(|pointer_draw_rectangle| pointer_draw_rectangle.union(&pointer_rendering_state.update_rectangle))
            .unwrap_or_else(|| pointer_rendering_state.update_rectangle);

        Ok(update_rectangle)
    }

    // To apply the buffer, we need to un-apply previously drawn cursor, and then apply it again
    // in other position.

    pub(crate) fn apply_tile(
        &mut self,
        tile_output: &[u8],
        pixel_format: PixelFormat,
        clipping_rectangles: &Region,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        trace!("Tile: {:?}", update_rectangle);

        if !self.rect_fits(&clipping_rectangles.extents) {
            debug!(
                "Skipping tile update {:?} outside image bounds {}x{}",
                clipping_rectangles.extents, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        let pointer_rendering_state = self.pointer_rendering_begin(&clipping_rectangles.extents)?;

        let update_region = clipping_rectangles.intersect_rectangle(update_rectangle);
        for region_rectangle in &update_region.rectangles {
            let source_x = region_rectangle.left - update_rectangle.left;
            let source_y = region_rectangle.top - update_rectangle.top;
            let stride = u16::from(pixel_format.bytes_per_pixel()) * TILE_SIZE;
            let source_image_region = ImageRegion {
                region: InclusiveRectangle {
                    left: source_x,
                    top: source_y,
                    right: source_x + region_rectangle.width() - 1,
                    bottom: source_y + region_rectangle.height() - 1,
                },
                data: tile_output,
                step: stride,
                pixel_format,
            };

            let mut destination_image_region = ImageRegionMut {
                region: region_rectangle.clone(),
                step: self.width() * u16::from(self.pixel_format.bytes_per_pixel()),
                pixel_format: self.pixel_format,
                data: &mut self.data,
            };

            trace!("Source image region: {:?}", source_image_region.region);
            trace!("Destination image region: {:?}", destination_image_region.region);

            source_image_region
                .copy_to(&mut destination_image_region)
                .map_err(|e| custom_err!("copy_to", e))?;
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    pub(crate) fn apply_rgb16_bitmap(
        &mut self,
        rgb16: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb16 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 2;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb16
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let rgb16_value = u16::from_le_bytes(
                            src_pixel
                                .try_into()
                                .expect("src_pixel contains exactly two u8 elements"),
                        );
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        let [r, g, b] = rdp_16bit_to_rgb(rgb16_value);
                        self.data[dst_idx + ri] = r;
                        self.data[dst_idx + gi] = g;
                        self.data[dst_idx + bi] = b;
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply a 15-bit (RGB555) bitmap. Bottom-up row order, 2 bytes per pixel.
    pub(crate) fn apply_rgb15_bitmap(
        &mut self,
        rgb15: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb15 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }


        const SRC_COLOR_DEPTH: usize = 2;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb15
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let rgb15_value = u16::from_le_bytes(
                            src_pixel
                                .try_into()
                                .expect("src_pixel contains exactly two u8 elements"),
                        );
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        let [r, g, b] = rdp_15bit_to_rgb(rgb15_value);
                        self.data[dst_idx + ri] = r;
                        self.data[dst_idx + gi] = g;
                        self.data[dst_idx + bi] = b;
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply a 24-bit BGR bitmap. RLE 24bpp decompresses to BGR byte order,
    /// and uncompressed 24bpp bitmaps are also BGR per MS-RDPBCGR.
    /// Bottom-up row order, 3 bytes per pixel.
    pub(crate) fn apply_bgr24_bitmap(
        &mut self,
        bgr24: &[u8],
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping bgr24 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 3;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        bgr24
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.chunks_exact(SRC_COLOR_DEPTH)
                    .enumerate()
                    .for_each(|(col_idx, src_pixel)| {
                        let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                        // BGR -> RGB channel swap
                        self.data[dst_idx + ri] = src_pixel[2];
                        self.data[dst_idx + gi] = src_pixel[1];
                        self.data[dst_idx + bi] = src_pixel[0];
                        self.data[dst_idx + ai] = 0xff;
                    })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Apply an 8bpp indexed color bitmap using the provided palette.
    pub(crate) fn apply_indexed8_bitmap(
        &mut self,
        indexed8: &[u8],
        palette: &crate::fast_path::ColorPalette,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        const SRC_COLOR_DEPTH: usize = 1;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        indexed8
            .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
            .rev()
            .enumerate()
            .for_each(|(row_idx, row)| {
                row.iter().enumerate().for_each(|(col_idx, &palette_index)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;
                    let [r, g, b] = palette.get(palette_index);
                    self.data[dst_idx + ri] = r;
                    self.data[dst_idx + gi] = g;
                    self.data[dst_idx + bi] = b;
                    self.data[dst_idx + ai] = 0xff;
                })
            });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    fn apply_rgb24_iter<'a, I>(
        &mut self,
        rgb24: I,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle>
    where
        I: Iterator<Item = &'a [u8]>,
    {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb24 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 3;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgb24.enumerate().for_each(|(row_idx, row)| {
            row.chunks_exact(SRC_COLOR_DEPTH)
                .enumerate()
                .for_each(|(col_idx, src_pixel)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                    self.data[dst_idx + ri] = src_pixel[0];
                    self.data[dst_idx + gi] = src_pixel[1];
                    self.data[dst_idx + bi] = src_pixel[2];
                    self.data[dst_idx + ai] = 0xFF;
                })
        });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    pub(crate) fn apply_rgb24(
        &mut self,
        rgb24: &[u8],
        update_rectangle: &InclusiveRectangle,
        flip: bool,
    ) -> SessionResult<InclusiveRectangle> {
        const SRC_COLOR_DEPTH: usize = 3;
        let rectangle_width = usize::from(update_rectangle.width());
        let lines = rgb24.chunks_exact(rectangle_width * SRC_COLOR_DEPTH);
        if flip {
            self.apply_rgb24_iter(lines.rev(), update_rectangle)
        } else {
            self.apply_rgb24_iter(lines, update_rectangle)
        }
    }

    #[cfg(feature = "qoi")]
    fn apply_rgba32_iter<'a, I>(
        &mut self,
        rgba32: I,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle>
    where
        I: Iterator<Item = &'a [u8]>,
    {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgba32 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 4;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);
        let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        rgba32.enumerate().for_each(|(row_idx, row)| {
            row.chunks_exact(SRC_COLOR_DEPTH)
                .enumerate()
                .for_each(|(col_idx, src_pixel)| {
                    let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                    self.data[dst_idx + ri] = src_pixel[0];
                    self.data[dst_idx + gi] = src_pixel[1];
                    self.data[dst_idx + bi] = src_pixel[2];
                    self.data[dst_idx + ai] = src_pixel[3];
                })
        });

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    #[cfg(feature = "qoi")]
    pub(crate) fn apply_rgba32(
        &mut self,
        rgba32: &[u8],
        update_rectangle: &InclusiveRectangle,
        flip: bool,
    ) -> SessionResult<InclusiveRectangle> {
        const SRC_COLOR_DEPTH: usize = 4;
        let rectangle_width = usize::from(update_rectangle.width());
        let lines = rgba32.chunks_exact(rectangle_width * SRC_COLOR_DEPTH);
        if flip {
            self.apply_rgba32_iter(lines.rev(), update_rectangle)
        } else {
            self.apply_rgba32_iter(lines, update_rectangle)
        }
    }

    pub(crate) fn apply_rgb32_bitmap(
        &mut self,
        rgb32: &[u8],
        format: PixelFormat,
        update_rectangle: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        if !self.rect_fits(update_rectangle) {
            debug!(
                "Skipping rgb32 update {:?} outside image bounds {}x{}",
                update_rectangle, self.width, self.height,
            );
            return Ok(InclusiveRectangle::empty());
        }

        const SRC_COLOR_DEPTH: usize = 4;
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rectangle_width = usize::from(update_rectangle.width());
        let top = usize::from(update_rectangle.top);
        let left = usize::from(update_rectangle.left);

        let pointer_rendering_state = self.pointer_rendering_begin(update_rectangle)?;

        if format == self.pixel_format {
            rgb32
                .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
                .rev()
                .enumerate()
                .for_each(|(row_idx, row)| {
                    row.chunks_exact(SRC_COLOR_DEPTH)
                        .enumerate()
                        .for_each(|(col_idx, src_pixel)| {
                            let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                            self.data[dst_idx..dst_idx + SRC_COLOR_DEPTH].copy_from_slice(src_pixel);
                        })
                });
        } else {
            let [ri, gi, bi, ai] = self.pixel_format.channel_offsets();
            rgb32
                .chunks_exact(rectangle_width * SRC_COLOR_DEPTH)
                .rev()
                .enumerate()
                .try_for_each(|(row_idx, row)| {
                    row.chunks_exact(SRC_COLOR_DEPTH)
                        .enumerate()
                        .try_for_each(|(col_idx, src_pixel)| {
                            let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;

                            let c = format
                                .read_color(src_pixel)
                                .map_err(|err| custom_err!("read color", err))?;

                            self.data[dst_idx + ri] = c.r;
                            self.data[dst_idx + gi] = c.g;
                            self.data[dst_idx + bi] = c.b;
                            self.data[dst_idx + ai] = c.a;

                            Ok(())
                        })?;

                    Ok(())
                })?;
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;

        Ok(update_rectangle)
    }

    /// Fill a rectangle with a solid RGBA color.
    ///
    /// This is used for drawing order operations like DstBlt, OpaqueRect, and PatBlt.
    // FIXME: this assumes PixelFormat::RgbA32
    pub(crate) fn fill_rectangle(
        &mut self,
        rect: &InclusiveRectangle,
        color: [u8; 4],
    ) -> SessionResult<InclusiveRectangle> {
        const DST_COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rect_width = usize::from(rect.width());
        let rect_height = usize::from(rect.height());
        let top = usize::from(rect.top);
        let left = usize::from(rect.left);

        let pointer_rendering_state = self.pointer_rendering_begin(rect)?;

        for row_idx in 0..rect_height {
            for col_idx in 0..rect_width {
                let dst_idx = ((top + row_idx) * image_width + left + col_idx) * DST_COLOR_DEPTH;
                if dst_idx + DST_COLOR_DEPTH <= self.data.len() {
                    self.data[dst_idx] = color[0]; // R
                    self.data[dst_idx + 1] = color[1]; // G
                    self.data[dst_idx + 2] = color[2]; // B
                    self.data[dst_idx + 3] = color[3]; // A
                }
            }
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;
        Ok(update_rectangle)
    }

    /// Copy a rectangle from one location to another within the image.
    ///
    /// This is used for ScrBlt (screen block transfer) operations.
    /// Handles overlapping regions correctly by using an intermediate buffer.
    // FIXME: this assumes PixelFormat::RgbA32
    #[expect(
        clippy::as_conversions,
        clippy::cast_sign_loss,
        reason = "Coordinates are clamped to non-negative before casting to usize"
    )]
    pub(crate) fn copy_rectangle(
        &mut self,
        src_x: i16,
        src_y: i16,
        dst_rect: &InclusiveRectangle,
    ) -> SessionResult<InclusiveRectangle> {
        const COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let image_height = usize::from(self.height);
        let rect_width = usize::from(dst_rect.width());
        let rect_height = usize::from(dst_rect.height());

        // Convert coordinates, clamping to valid range
        let src_x = src_x.max(0) as usize;
        let src_y = src_y.max(0) as usize;
        let dst_x = usize::from(dst_rect.left);
        let dst_y = usize::from(dst_rect.top);

        // Calculate actual copy dimensions (clamp to image bounds)
        let copy_width = rect_width
            .min(image_width.saturating_sub(src_x))
            .min(image_width.saturating_sub(dst_x));
        let copy_height = rect_height
            .min(image_height.saturating_sub(src_y))
            .min(image_height.saturating_sub(dst_y));

        if copy_width == 0 || copy_height == 0 {
            return Ok(dst_rect.clone());
        }

        let pointer_rendering_state = self.pointer_rendering_begin(dst_rect)?;

        // Copy to intermediate buffer to handle overlapping regions
        let mut buffer = vec![0u8; copy_width * copy_height * COLOR_DEPTH];

        // Copy source to buffer
        for row in 0..copy_height {
            let src_row_start = ((src_y + row) * image_width + src_x) * COLOR_DEPTH;
            let buf_row_start = row * copy_width * COLOR_DEPTH;
            let row_bytes = copy_width * COLOR_DEPTH;

            if src_row_start + row_bytes <= self.data.len() {
                buffer[buf_row_start..buf_row_start + row_bytes]
                    .copy_from_slice(&self.data[src_row_start..src_row_start + row_bytes]);
            }
        }

        // Copy buffer to destination
        for row in 0..copy_height {
            let dst_row_start = ((dst_y + row) * image_width + dst_x) * COLOR_DEPTH;
            let buf_row_start = row * copy_width * COLOR_DEPTH;
            let row_bytes = copy_width * COLOR_DEPTH;

            if dst_row_start + row_bytes <= self.data.len() {
                self.data[dst_row_start..dst_row_start + row_bytes]
                    .copy_from_slice(&buffer[buf_row_start..buf_row_start + row_bytes]);
            }
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;
        Ok(update_rectangle)
    }

    /// Invert all pixels in a rectangle.
    ///
    /// This is used for DSTINVERT ROP operations.
    // FIXME: this assumes PixelFormat::RgbA32
    pub(crate) fn invert_rectangle(&mut self, rect: &InclusiveRectangle) -> SessionResult<InclusiveRectangle> {
        const COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let rect_width = usize::from(rect.width());
        let rect_height = usize::from(rect.height());
        let top = usize::from(rect.top);
        let left = usize::from(rect.left);

        let pointer_rendering_state = self.pointer_rendering_begin(rect)?;

        for row_idx in 0..rect_height {
            for col_idx in 0..rect_width {
                let idx = ((top + row_idx) * image_width + left + col_idx) * COLOR_DEPTH;
                if idx + COLOR_DEPTH <= self.data.len() {
                    self.data[idx] = 255 - self.data[idx]; // R
                    self.data[idx + 1] = 255 - self.data[idx + 1]; // G
                    self.data[idx + 2] = 255 - self.data[idx + 2]; // B
                                                                   // Alpha stays the same
                }
            }
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;
        Ok(update_rectangle)
    }

    /// Draw a line using Bresenham's algorithm.
    ///
    /// This is used for LineTo and Polyline operations.
    // FIXME: this assumes PixelFormat::RgbA32
    #[expect(
        clippy::as_conversions,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "Line coordinates are clamped to screen bounds before casting"
    )]
    pub(crate) fn draw_line(
        &mut self,
        x0: i16,
        y0: i16,
        x1: i16,
        y1: i16,
        color: [u8; 4],
    ) -> SessionResult<InclusiveRectangle> {
        const COLOR_DEPTH: usize = 4;

        let image_width = usize::from(self.width);
        let image_height = usize::from(self.height);

        // Calculate bounding rectangle for the line
        let rect = InclusiveRectangle {
            left: x0.min(x1).max(0) as u16,
            top: y0.min(y1).max(0) as u16,
            right: x0.max(x1).min(self.width as i16 - 1).max(0) as u16,
            bottom: y0.max(y1).min(self.height as i16 - 1).max(0) as u16,
        };

        let pointer_rendering_state = self.pointer_rendering_begin(&rect)?;

        // Bresenham's line algorithm
        let mut x = i32::from(x0);
        let mut y = i32::from(y0);
        let x1 = i32::from(x1);
        let y1 = i32::from(y1);

        let dx = (x1 - x).abs();
        let dy = -(y1 - y).abs();
        let sx = if x < x1 { 1 } else { -1 };
        let sy = if y < y1 { 1 } else { -1 };
        let mut err = dx + dy;

        loop {
            // Plot pixel if within bounds
            if x >= 0 && (x as usize) < image_width && y >= 0 && (y as usize) < image_height {
                let idx = (y as usize * image_width + x as usize) * COLOR_DEPTH;
                if idx + COLOR_DEPTH <= self.data.len() {
                    self.data[idx] = color[0];
                    self.data[idx + 1] = color[1];
                    self.data[idx + 2] = color[2];
                    self.data[idx + 3] = color[3];
                }
            }

            if x == x1 && y == y1 {
                break;
            }

            let e2 = 2 * err;
            if e2 >= dy {
                if x == x1 {
                    break;
                }
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                if y == y1 {
                    break;
                }
                err += dx;
                y += sy;
            }
        }

        let update_rectangle = self.pointer_rendering_end(pointer_rendering_state)?;
        Ok(update_rectangle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_image(width: u16, height: u16) -> DecodedImage {
        DecodedImage::new(PixelFormat::RgbA32, width, height)
    }

    #[test]
    fn test_fill_rectangle() {
        let mut image = create_test_image(100, 100);

        let rect = InclusiveRectangle {
            left: 10,
            top: 10,
            right: 19,
            bottom: 19,
        };

        let result = image.fill_rectangle(&rect, [255, 128, 64, 255]);
        assert!(result.is_ok());

        // Check a pixel inside the rectangle
        let idx = (10 * 100 + 10) * 4;
        assert_eq!(image.data[idx], 255); // R
        assert_eq!(image.data[idx + 1], 128); // G
        assert_eq!(image.data[idx + 2], 64); // B
        assert_eq!(image.data[idx + 3], 255); // A

        // Check a pixel outside the rectangle is still black (default)
        let idx = (0 * 100 + 0) * 4;
        assert_eq!(image.data[idx], 0); // R
        assert_eq!(image.data[idx + 1], 0); // G
        assert_eq!(image.data[idx + 2], 0); // B
        assert_eq!(image.data[idx + 3], 0); // A
    }

    #[test]
    fn test_copy_rectangle() {
        let mut image = create_test_image(100, 100);

        // First fill a source rectangle with a color
        let src_rect = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 9,
            bottom: 9,
        };
        image.fill_rectangle(&src_rect, [255, 0, 0, 255]).unwrap();

        // Copy to a different location
        let dst_rect = InclusiveRectangle {
            left: 50,
            top: 50,
            right: 59,
            bottom: 59,
        };

        let result = image.copy_rectangle(0, 0, &dst_rect);
        assert!(result.is_ok());

        // Check destination has the copied color
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(image.data[idx], 255); // R
        assert_eq!(image.data[idx + 1], 0); // G
        assert_eq!(image.data[idx + 2], 0); // B
        assert_eq!(image.data[idx + 3], 255); // A
    }

    #[test]
    fn test_copy_rectangle_overlapping() {
        let mut image = create_test_image(100, 100);

        // Fill first 10 columns with different colors per column
        for x in 0..10u16 {
            let rect = InclusiveRectangle {
                left: x,
                top: 0,
                right: x,
                bottom: 9,
            };
            let color = (x * 25) as u8;
            image.fill_rectangle(&rect, [color, color, color, 255]).unwrap();
        }

        // Copy overlapping: shift right by 5 pixels
        let dst_rect = InclusiveRectangle {
            left: 5,
            top: 0,
            right: 14,
            bottom: 9,
        };

        let result = image.copy_rectangle(0, 0, &dst_rect);
        assert!(result.is_ok());

        // The copy should work correctly even with overlap
        // At x=5, we should see what was at x=0 (color 0)
        let idx = (0 * 100 + 5) * 4;
        assert_eq!(image.data[idx], 0); // Was at x=0

        // At x=10, we should see what was at x=5 (color 125)
        let idx = (0 * 100 + 10) * 4;
        assert_eq!(image.data[idx], 125); // Was at x=5
    }

    #[test]
    fn test_invert_rectangle() {
        let mut image = create_test_image(100, 100);

        // Fill with a known color
        let rect = InclusiveRectangle {
            left: 0,
            top: 0,
            right: 9,
            bottom: 9,
        };
        image.fill_rectangle(&rect, [100, 150, 200, 255]).unwrap();

        // Invert
        let result = image.invert_rectangle(&rect);
        assert!(result.is_ok());

        // Check inverted values: 255 - original
        let idx = (0 * 100 + 0) * 4;
        assert_eq!(image.data[idx], 155); // 255 - 100
        assert_eq!(image.data[idx + 1], 105); // 255 - 150
        assert_eq!(image.data[idx + 2], 55); // 255 - 200
        assert_eq!(image.data[idx + 3], 255); // Alpha unchanged
    }

    #[test]
    fn test_draw_line_horizontal() {
        let mut image = create_test_image(100, 100);

        let result = image.draw_line(10, 50, 90, 50, [255, 0, 0, 255]);
        assert!(result.is_ok());

        let rect = result.unwrap();
        assert_eq!(rect.left, 10);
        assert_eq!(rect.right, 90);
        assert_eq!(rect.top, 50);
        assert_eq!(rect.bottom, 50);

        // Check some pixels on the line
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(image.data[idx], 255);
        assert_eq!(image.data[idx + 1], 0);
        assert_eq!(image.data[idx + 2], 0);
    }

    #[test]
    fn test_draw_line_vertical() {
        let mut image = create_test_image(100, 100);

        let result = image.draw_line(50, 10, 50, 90, [0, 255, 0, 255]);
        assert!(result.is_ok());

        let rect = result.unwrap();
        assert_eq!(rect.left, 50);
        assert_eq!(rect.right, 50);
        assert_eq!(rect.top, 10);
        assert_eq!(rect.bottom, 90);
    }

    #[test]
    fn test_draw_line_diagonal() {
        let mut image = create_test_image(100, 100);

        let result = image.draw_line(0, 0, 50, 50, [0, 0, 255, 255]);
        assert!(result.is_ok());

        let rect = result.unwrap();
        assert_eq!(rect.left, 0);
        assert_eq!(rect.right, 50);
        assert_eq!(rect.top, 0);
        assert_eq!(rect.bottom, 50);

        // Check that (25, 25) is on the line
        let idx = (25 * 100 + 25) * 4;
        assert_eq!(image.data[idx], 0);
        assert_eq!(image.data[idx + 1], 0);
        assert_eq!(image.data[idx + 2], 255);
    }

    #[test]
    fn test_draw_line_reverse() {
        let mut image = create_test_image(100, 100);

        // Draw from bottom-right to top-left
        let result = image.draw_line(90, 90, 10, 10, [255, 255, 0, 255]);
        assert!(result.is_ok());

        // Check that (50, 50) is on the line
        let idx = (50 * 100 + 50) * 4;
        assert_eq!(image.data[idx], 255);
        assert_eq!(image.data[idx + 1], 255);
        assert_eq!(image.data[idx + 2], 0);
    }

    #[test]
    fn test_draw_line_clipping() {
        let mut image = create_test_image(100, 100);

        // Line that goes partially outside image bounds
        let result = image.draw_line(-10, 50, 110, 50, [255, 0, 0, 255]);
        assert!(result.is_ok());

        // Should clip to image bounds
        let rect = result.unwrap();
        assert!(rect.left <= 0);
        assert!(rect.right <= 99);
    }
}

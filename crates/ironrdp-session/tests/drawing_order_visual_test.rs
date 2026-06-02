//! Visual verification test for drawing orders
//!
//! This test verifies that drawing orders actually modify the image data
//! and produce visible output (not just black screens).

use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_session::fast_path::{Processor, ProcessorBuilder};
use ironrdp_session::image::DecodedImage;

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

/// Verify that OpaqueRect actually draws visible pixels
#[test]
fn test_opaque_rect_produces_visible_output() {
    let mut processor = create_test_processor();
    let mut image = create_test_image();

    // Verify image starts as all black (zeros)
    let initial_data = image.data();
    assert!(initial_data.iter().all(|&b| b == 0), "Image should start as all black");

    // OpaqueRect order: draw a white 100x100 rectangle at (50, 50)
    // Order format: controlFlags, orderType, fieldFlags, fields...
    let order_data = vec![
        0x09, // controlFlags: STANDARD | TYPE_CHANGE
        0x0A, // orderType: OpaqueRect (TS_ENC_OPAQUERECT_ORDER)
        0x7F, // fieldFlags: all fields present
        // Fields (in order):
        50, 0, // left (u16 LE)
        50, 0, // top (u16 LE)
        150, 0, // right (u16 LE) - 50 + 100
        150, 0,    // bottom (u16 LE) - 50 + 100
        0xFF, // red (255)
        0xFF, // green (255)
        0xFF, // blue (255)
    ];

    // Process the drawing order
    let updates = processor
        .process_slow_path_orders(&mut image, 1, &order_data)
        .expect("Failed to process drawing order");

    // Verify we got an update
    assert!(!updates.is_empty(), "Should have received graphics update");

    // Verify the image data changed
    let updated_data = image.data();
    let has_non_zero = updated_data.iter().any(|&b| b != 0);
    assert!(
        has_non_zero,
        "Image should have non-zero pixels after drawing white rectangle"
    );

    // Verify specific pixels in the rectangle are white
    // RGBA format: R, G, B, A at position (100, 100) which is inside the rectangle
    let x = 100;
    let y = 100;
    let offset = ((y * 800 + x) * 4) as usize;

    assert_eq!(updated_data[offset], 255, "Red channel should be 255");
    assert_eq!(updated_data[offset + 1], 255, "Green channel should be 255");
    assert_eq!(updated_data[offset + 2], 255, "Blue channel should be 255");
    assert_eq!(updated_data[offset + 3], 255, "Alpha channel should be 255");

    // Verify pixels outside the rectangle are still black
    let outside_offset = ((10 * 800 + 10) * 4) as usize;
    assert_eq!(
        updated_data[outside_offset], 0,
        "Pixels outside rectangle should be black"
    );
}

/// Verify that DstBlt with WHITENESS fills the screen
#[test]
fn test_dstblt_whiteness_fills_screen() {
    let mut processor = create_test_processor();
    let mut image = create_test_image();

    // DstBlt order with WHITENESS ROP (0xFF) - fills entire screen with white
    let order_data = vec![
        0x09, // controlFlags: STANDARD | TYPE_CHANGE
        0x00, // orderType: DstBlt
        0x1F, // fieldFlags: all fields present
        // Fields:
        0, 0, // left (u16 LE)
        0, 0, // top (u16 LE)
        0x20, 0x03, // width (800 in LE)
        0x58, 0x02, // height (600 in LE)
        0xFF, // rop: WHITENESS
    ];

    let updates = processor
        .process_slow_path_orders(&mut image, 1, &order_data)
        .expect("Failed to process DstBlt");

    assert!(!updates.is_empty(), "Should have received graphics update");

    // Verify the screen is now white
    let data = image.data();

    // Check several random pixels to ensure they're white
    for y in [0, 100, 300, 599] {
        for x in [0, 100, 400, 799] {
            let offset = ((y * 800 + x) * 4) as usize;
            assert_eq!(data[offset], 255, "R should be 255 at ({}, {})", x, y);
            assert_eq!(data[offset + 1], 255, "G should be 255 at ({}, {})", x, y);
            assert_eq!(data[offset + 2], 255, "B should be 255 at ({}, {})", x, y);
        }
    }
}

/// Verify that LineTo draws a visible line
#[test]
fn test_line_to_draws_visible_line() {
    let mut processor = create_test_processor();
    let mut image = create_test_image();

    // LineTo order: draw a red line from (100, 100) to (200, 200).
    // Wire format: controlFlags, orderType, fieldFlags(2 bytes LE), then fields.
    // Colors are BGR on the wire. 0xFF,0x03 = all 10 fields present.
    let order_data = vec![
        0x09, // controlFlags: STANDARD | TYPE_CHANGE
        0x09, // orderType: LineTo
        0xFF, 0x03, // fieldFlags: all 10 fields present (2 bytes LE)
        0x00, 0x00, // backMode
        100, 0, // nXStart (i16 LE)
        100, 0, // nYStart (i16 LE)
        200, 0, // nXEnd (i16 LE)
        200, 0, // nYEnd (i16 LE)
        0x00, 0x00, 0x00, // backColor (BGR)
        0xFF, // rop2: R2_COPYPEN
        0x00, // penStyle: PS_SOLID
        0x01, // penWidth
        0x00, 0x00, 0xFF, // penColor (BGR: R=0xFF)
    ];

    let updates = processor
        .process_slow_path_orders(&mut image, 1, &order_data)
        .expect("Failed to process LineTo");

    assert!(!updates.is_empty(), "Should have received graphics update");

    // Verify some pixels along the line are red
    let data = image.data();

    // Check pixel at (150, 150) which should be on the line.
    // The line is drawn with penColor BGR=0x00,0x00,0xFF (red).
    // DecodedImage stores RGBA so any of the first 3 channels should be non-zero.
    let offset = ((150 * 800 + 150) * 4) as usize;
    let has_color = data[offset] > 0 || data[offset + 1] > 0 || data[offset + 2] > 0;

    assert!(has_color, "Line should have drawn colored pixels at (150, 150)");
}

/// Test that multiple drawing orders accumulate correctly
#[test]
fn test_multiple_orders_accumulate() {
    let mut processor = create_test_processor();
    let mut image = create_test_image();

    // First order: Draw white rectangle at (0, 0, 100, 100)
    let order1 = vec![0x09, 0x0A, 0x7F, 0, 0, 0, 0, 100, 0, 100, 0, 0xFF, 0xFF, 0xFF];

    processor
        .process_slow_path_orders(&mut image, 1, &order1)
        .expect("Failed to process first order");

    // Second order: Draw red rectangle at (200, 200, 300, 300)
    let order2 = vec![
        0x01, // controlFlags: STANDARD (no TYPE_CHANGE)
        0x7F, // fieldFlags: all fields present
        200, 0, 200, 0, 0x2C, 0x01, 0x2C, 0x01, // 300 = 0x012C
        0xFF, 0x00, 0x00, // red
    ];

    processor
        .process_slow_path_orders(&mut image, 1, &order2)
        .expect("Failed to process second order");

    let data = image.data();

    // Verify first rectangle is white
    let offset1 = ((50 * 800 + 50) * 4) as usize;
    assert_eq!(data[offset1], 255, "First rect should be white");

    // Verify second rectangle is red
    let offset2 = ((250 * 800 + 250) * 4) as usize;
    assert_eq!(data[offset2], 255, "Second rect should be red");
    assert_eq!(data[offset2 + 1], 0, "Second rect green should be 0");
    assert_eq!(data[offset2 + 2], 0, "Second rect blue should be 0");
}

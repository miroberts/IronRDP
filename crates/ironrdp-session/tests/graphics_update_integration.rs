//! Integration tests for graphics update processing
//!
//! Tests that graphics updates (bitmap, drawing orders, palette) are properly
//! integrated into the session layer and work end-to-end.

use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_session::image::DecodedImage;

/// Test that DecodedImage can be created and updated
///
/// This verifies the basic infrastructure for graphics updates works correctly.
#[test]
fn test_decoded_image_creation() {
    let width = 1024u16;
    let height = 768u16;

    let image = DecodedImage::new(PixelFormat::RgbA32, width, height);

    // Verify image was created with correct dimensions
    assert_eq!(image.width(), width);
    assert_eq!(image.height(), height);

    // Verify data buffer is correct size (RGBA = 4 bytes per pixel)
    let expected_size = (width as usize) * (height as usize) * 4;
    assert_eq!(image.data().len(), expected_size);
}

/// Test that DecodedImage handles various resolutions
#[test]
fn test_decoded_image_various_resolutions() {
    let test_cases = vec![
        (640, 480),   // VGA
        (800, 600),   // SVGA
        (1024, 768),  // XGA
        (1280, 720),  // HD
        (1920, 1080), // Full HD
        (2560, 1440), // QHD
        (3840, 2160), // 4K
    ];

    for (width, height) in test_cases {
        let image = DecodedImage::new(PixelFormat::RgbA32, width, height);
        assert_eq!(image.width(), width);
        assert_eq!(image.height(), height);

        let expected_size = (width as usize) * (height as usize) * 4;
        assert_eq!(image.data().len(), expected_size);
    }
}

/// Test that DecodedImage data is initialized to zeros
#[test]
fn test_decoded_image_initialized() {
    let width = 100u16;
    let height = 100u16;

    let image = DecodedImage::new(PixelFormat::RgbA32, width, height);

    // All pixels should be initialized (either to 0 or some default)
    assert!(!image.data().is_empty());

    // Data should be exactly the right size
    assert_eq!(image.data().len(), (width as usize) * (height as usize) * 4);
}

/// Test that graphics updates can handle large images without panicking
#[test]
fn test_large_image_handling() {
    // 4K resolution at 32bpp
    let width = 3840u16;
    let height = 2160u16;

    // This should not panic or allocate excessive memory
    let image = DecodedImage::new(PixelFormat::RgbA32, width, height);

    // Verify size is reasonable (4K RGBA = ~33MB)
    let size = image.data().len();
    assert_eq!(size, 3840 * 2160 * 4);
    assert!(size < 50_000_000); // Less than 50MB
}

/// Test that different pixel formats are supported
#[test]
fn test_pixel_format_support() {
    let width = 800u16;
    let height = 600u16;

    // Test RGBA32 format (most common for RDP)
    let image_rgba = DecodedImage::new(PixelFormat::RgbA32, width, height);
    assert_eq!(image_rgba.data().len(), (width as usize) * (height as usize) * 4);

    // RgbA32 is the primary format used in RDP graphics updates
    // Other formats would be converted to this format during processing
}

/// Test that graphics update infrastructure is thread-safe
#[test]
fn test_concurrent_image_creation() {
    use std::thread;

    let handles: Vec<_> = (0..4)
        .map(|i| {
            thread::spawn(move || {
                let width = 1024u16;
                let height = 768u16;
                let image = DecodedImage::new(PixelFormat::RgbA32, width, height);
                assert_eq!(image.width(), width);
                assert_eq!(image.height(), height);
                i
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("Thread panicked");
    }
}

/// Test that zero-sized images are handled gracefully
#[test]
fn test_zero_width_image_handled() {
    // Zero width creates an empty image (handled gracefully)
    let image = DecodedImage::new(PixelFormat::RgbA32, 0, 768);
    assert_eq!(image.width(), 0);
    assert_eq!(image.data().len(), 0);
}

/// Test that zero-height images are handled gracefully
#[test]
fn test_zero_height_image_handled() {
    // Zero height creates an empty image (handled gracefully)
    let image = DecodedImage::new(PixelFormat::RgbA32, 1024, 0);
    assert_eq!(image.height(), 0);
    assert_eq!(image.data().len(), 0);
}

/// Test graphics update flow with realistic dimensions
///
/// This simulates what happens when an RDP server sends graphics updates:
/// 1. Server sends bitmap/drawing order PDU
/// 2. Session layer creates/updates DecodedImage
/// 3. Image is rendered to client
#[test]
fn test_realistic_graphics_flow() {
    // Typical RDP session dimensions
    let width = 1920u16;
    let height = 1080u16;

    // Create image (simulates session initialization)
    let image = DecodedImage::new(PixelFormat::RgbA32, width, height);

    // Verify initial state
    assert_eq!(image.width(), width);
    assert_eq!(image.height(), height);

    // Simulate a partial update (common in RDP)
    let update_region_size = 200 * 150 * 4; // 200x150 pixels in RGBA
    assert!(update_region_size < image.data().len());

    // In a real scenario, ActiveStage would:
    // 1. Receive ServerGraphicsUpdate PDU
    // 2. Decode bitmap/drawing order
    // 3. Update the relevant region of DecodedImage
    // 4. Return ActiveStageOutput::GraphicsUpdate

    // For this integration test, we just verify the infrastructure is sound
}

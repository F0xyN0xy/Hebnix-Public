use std::io::Cursor;

use image::{DynamicImage, ImageBuffer, ImageFormat, metadata::Orientation};
use jpeg_decoder::{Decoder, PixelFormat};

/// Keep JPEG banners away from egui's zune-jpeg 0.5.15 decoder, which silently
/// produces green stripes for separate component scans (zune-image issue #419).
/// Run on the fetch worker for both cached files and new downloads. Other
/// formats pass through unchanged, including animated GIFs and transparent PNGs.
pub fn prepare(bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    if image::guess_format(&bytes).ok() != Some(ImageFormat::Jpeg) {
        return Ok(bytes);
    }
    let mut decoder = Decoder::new(Cursor::new(&bytes));
    decoder.set_max_decoding_buffer_size(128 * 1024 * 1024);
    let pixels = decoder.decode().map_err(|e| e.to_string())?;
    let info = decoder.info().ok_or("JPEG has no image information")?;
    let (width, height) = (u32::from(info.width), u32::from(info.height));
    let mut image = match info.pixel_format {
        PixelFormat::RGB24 => {
            ImageBuffer::from_raw(width, height, pixels).map(DynamicImage::ImageRgb8)
        }
        PixelFormat::L8 => {
            ImageBuffer::from_raw(width, height, pixels).map(DynamicImage::ImageLuma8)
        }
        PixelFormat::L16 => {
            let samples = pixels
                .chunks_exact(2)
                .map(|p| u16::from_ne_bytes([p[0], p[1]]))
                .collect();
            ImageBuffer::from_raw(width, height, samples).map(DynamicImage::ImageLuma16)
        }
        PixelFormat::CMYK32 => {
            let rgb = pixels
                .chunks_exact(4)
                .flat_map(|p| {
                    let black = 255 - u16::from(p[3]);
                    [0, 1, 2].map(|c| ((255 - u16::from(p[c])) * black / 255) as u8)
                })
                .collect();
            ImageBuffer::from_raw(width, height, rgb).map(DynamicImage::ImageRgb8)
        }
    }
    .ok_or("Invalid JPEG pixel buffer")?;
    if let Some(orientation) = decoder.exif_data().and_then(Orientation::from_exif_chunk) {
        image.apply_orientation(orientation);
    }
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(png.into_inner())
}

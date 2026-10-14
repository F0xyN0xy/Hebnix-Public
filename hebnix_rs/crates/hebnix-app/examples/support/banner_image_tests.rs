//! Synthetic baseline JPEGs: no external images or network required.
use super::banner_image;

fn segment(jpeg: &mut Vec<u8>, marker: u8, payload: &[u8]) {
    jpeg.extend_from_slice(&[0xff, marker]);
    jpeg.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(payload);
}

fn neutral_jpeg(width: u16, height: u16, h: usize, v: usize, separate: bool) -> Vec<u8> {
    let mut jpeg = vec![0xff, 0xd8];
    let mut quant = vec![0];
    quant.extend_from_slice(&[1; 64]);
    segment(&mut jpeg, 0xdb, &quant);
    let mut frame = vec![8];
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.extend_from_slice(&[3, 1, ((h << 4) | v) as u8, 0, 2, 0x11, 0, 3, 0x11, 0]);
    segment(&mut jpeg, 0xc0, &frame);

    // DC difference 0 and AC end-of-block both use the one-bit code 0.
    for class in [0, 0x10] {
        let mut table = vec![class, 1];
        table.extend_from_slice(&[0; 15]);
        table.push(0);
        segment(&mut jpeg, 0xc4, &table);
    }
    let scans: Vec<Vec<u8>> = if separate {
        vec![vec![1], vec![2], vec![3]]
    } else {
        vec![vec![1, 2, 3]]
    };
    for components in scans {
        let mut scan = vec![components.len() as u8];
        for &component in &components {
            scan.extend_from_slice(&[component, 0]);
        }
        scan.extend_from_slice(&[0, 63, 0]);
        segment(&mut jpeg, 0xda, &scan);
        let blocks = if separate {
            let (ch, cv) = if components[0] == 1 { (h, v) } else { (1, 1) };
            (usize::from(width) * ch).div_ceil(h * 8) * (usize::from(height) * cv).div_ceil(v * 8)
        } else {
            usize::from(width).div_ceil(h * 8) * usize::from(height).div_ceil(v * 8) * (h * v + 2)
        };
        let bits = blocks * 2;
        jpeg.extend(std::iter::repeat(0).take(bits / 8));
        if bits % 8 != 0 {
            jpeg.push((1 << (8 - bits % 8)) - 1);
        }
    }
    jpeg.extend_from_slice(&[0xff, 0xd9]);
    jpeg
}

#[test]
fn separate_component_scans_match_interleaved_pixels() {
    for (width, height) in [(32, 32), (17, 19), (200, 200)] {
        for (h, v) in [(1, 1), (2, 1), (1, 2), (2, 2)] {
            let decode = |separate| {
                let bytes = neutral_jpeg(width, height, h, v, separate);
                image::load_from_memory(&banner_image::prepare(bytes).unwrap())
                    .unwrap()
                    .into_rgb8()
                    .into_raw()
            };
            let expected = decode(false);
            assert!(expected.iter().all(|&c| c == 128));
            let actual = decode(true);
            assert_eq!(actual.len(), expected.len());
            assert!(actual == expected, "{width}x{height}, sampling {h}x{v}");
        }
    }
}

#[test]
fn png_alpha_and_gif_animation_bytes_are_unchanged() {
    let mut png = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 60, 80]))
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png = png.into_inner();
    assert_eq!(banner_image::prepare(png.clone()).unwrap(), png);

    let mut gif = Vec::new();
    {
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut gif);
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            encoder
                .encode_frame(image::Frame::new(image::RgbaImage::from_pixel(
                    2,
                    2,
                    image::Rgba(color),
                )))
                .unwrap();
        }
    }
    assert_eq!(banner_image::prepare(gif.clone()).unwrap(), gif);
}

#[test]
fn jpeg_exif_orientation_is_preserved() {
    let original = neutral_jpeg(17, 19, 2, 2, true);
    let mut rotated = original[..2].to_vec();
    // Little-endian TIFF with orientation 6 (90 degrees clockwise).
    segment(
        &mut rotated,
        0xe1,
        &[
            b'E', b'x', b'i', b'f', 0, 0, b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 1, 3, 0, 1, 0,
            0, 0, 6, 0, 0, 0, 0, 0, 0, 0,
        ],
    );
    rotated.extend_from_slice(&original[2..]);
    let image = image::load_from_memory(&banner_image::prepare(rotated).unwrap()).unwrap();
    assert_eq!((image.width(), image.height()), (19, 17));
}

#[test]
fn malformed_jpeg_is_reported_as_failed() {
    assert!(banner_image::prepare(vec![0xff, 0xd8, 0xff, 0xd9]).is_err());
}

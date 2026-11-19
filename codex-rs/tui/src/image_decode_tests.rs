#![cfg(test)]

use super::decode_oriented;
use image::DynamicImage;
use image::ImageEncoder;
use image::ImageFormat;
use image::ImageReader;
use image::Limits;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use pretty_assertions::assert_eq;
use std::io::Cursor;

fn write_fixture(mut encoder: impl ImageEncoder, orientation: Option<u8>) {
    if let Some(orientation) = orientation {
        // A little-endian EXIF directory with a single orientation entry.
        let mut exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
        exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        encoder.set_exif_metadata(exif).unwrap();
    }
    encoder
        .write_image(
            &[0, 40, 80, 120, 160, 200],
            /*width*/ 3,
            /*height*/ 2,
            image::ExtendedColorType::L8,
        )
        .unwrap();
}

fn check_orientation(orientation: Option<u8>, display: fn(&DynamicImage) -> DynamicImage) {
    let mut png = Vec::new();
    write_fixture(PngEncoder::new(&mut png), orientation);
    let mut jpeg = Vec::new();
    write_fixture(JpegEncoder::new(&mut jpeg), orientation);

    for (data, format) in [(png, ImageFormat::Png), (jpeg, ImageFormat::Jpeg)] {
        let raw = image::load_from_memory_with_format(&data, format).unwrap();
        let actual = decode_oriented(
            ImageReader::with_format(Cursor::new(data), format),
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            actual.to_rgb8(),
            display(&raw).to_rgb8(),
            "{format:?} {orientation:?}"
        );
    }
}

#[test]
fn decodes_images_in_display_orientation() {
    check_orientation(/*orientation*/ None, DynamicImage::clone);
    check_orientation(Some(1), DynamicImage::clone);
    check_orientation(Some(2), DynamicImage::fliph);
    check_orientation(Some(3), DynamicImage::rotate180);
    check_orientation(Some(4), DynamicImage::flipv);
    check_orientation(Some(5), |image| image.rotate90().fliph());
    check_orientation(Some(6), DynamicImage::rotate90);
    check_orientation(Some(7), |image| image.rotate270().fliph());
    check_orientation(Some(8), DynamicImage::rotate270);
}

#[test]
fn rejects_images_larger_than_the_pixel_budget() {
    let mut jpeg = Vec::new();
    write_fixture(JpegEncoder::new(&mut jpeg), /*orientation*/ None);
    let reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    let mut limits = Limits::default();
    limits.max_alloc = Some(5); // The fixture has six grayscale pixels.
    assert!(matches!(
        decode_oriented(reader, limits),
        Err(image::ImageError::Limits(_))
    ));
}

#[test]
fn native_png_reuses_only_decoded_pixel_chunks() {
    let raster =
        image::RgbaImage::from_raw(2, 1, vec![213, 45, 87, 255, 240, 11, 155, 13]).unwrap();
    let bytes = super::encode_png(&raster).unwrap();
    let (actual, native) =
        super::sidebar_pixels(&bytes, ImageFormat::Png, Limits::default()).unwrap();
    assert_eq!(actual, raster);
    assert_eq!(native.as_deref(), Some(bytes.as_slice()));

    let mut tagged = bytes.clone();
    // A known ancillary chunk with its CRC. Metadata must stay out of the terminal payload.
    let phys = b"\0\0\0\tpHYs\0\0\x0b\x13\0\0\x0b\x13\x01\0\x9a\x9c\x18";
    tagged.splice(33..33, phys.iter().copied());
    let (actual, native) =
        super::sidebar_pixels(&tagged, ImageFormat::Png, Limits::default()).unwrap();
    assert_eq!(actual, raster);
    assert_eq!(native.as_deref(), Some(bytes.as_slice()));

    let mut corrupt = bytes.clone();
    corrupt[41] ^= 1; // First IDAT data byte.
    assert!(super::sidebar_pixels(&corrupt, ImageFormat::Png, Limits::default()).is_err());
    let mut appended = bytes;
    appended.extend_from_slice(b"after IEND");
    let (_, native) =
        super::sidebar_pixels(&appended, ImageFormat::Png, Limits::default()).unwrap();
    assert!(native.is_none());
}

#[test]
fn sidebar_checks_final_rgba_size_and_orients_before_upload() {
    let mut bytes = Vec::new();
    write_fixture(PngEncoder::new(&mut bytes), Some(6));
    let raw = image::load_from_memory(&bytes).unwrap();
    let (actual, native) =
        super::sidebar_pixels(&bytes, ImageFormat::Png, Limits::default()).unwrap();
    assert_eq!(actual, raw.rotate90().into_rgba8());
    assert!(native.is_none());
    let mut limits = Limits::default();
    limits.max_alloc = Some(23); // Six grayscale pixels fit, six RGBA pixels don't.
    assert!(super::sidebar_pixels(&bytes, ImageFormat::Png, limits).is_err());
}

#[test]
fn sidebar_jpeg_decodes_camera_pixels_in_exif_orientation() {
    let mut bytes = Vec::new();
    let mut encoder = JpegEncoder::new(&mut bytes);
    let exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0";
    encoder.set_exif_metadata(exif.to_vec()).unwrap();
    encoder
        .write_image(
            &[
                255, 25, 0, 12, 239, 29, 3, 112, 195, 12, 34, 56, 89, 224, 17, 0, 0, 0,
            ],
            3,
            2,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    let expected = image::load_from_memory(&bytes)
        .unwrap()
        .rotate90()
        .into_rgba8();
    let (actual, native) =
        super::sidebar_pixels(&bytes, ImageFormat::Jpeg, Limits::default()).unwrap();
    assert_eq!(actual, expected);
    assert!(native.is_none());
    let mut limits = Limits::default();
    limits.max_alloc = Some(23);
    assert!(super::sidebar_pixels(&bytes, ImageFormat::Jpeg, limits).is_err());
}

#[test]
fn fallback_crops_exact_pixels_and_resamples_transparency() {
    let raster =
        image::RgbaImage::from_raw(3, 1, vec![0, 255, 0, 255, 0, 0, 255, 255, 255, 0, 0, 0])
            .unwrap();
    let crop = image::math::Rect {
        x: 1,
        y: 0,
        width: 2,
        height: 1,
    };
    let exact = super::resize_crop(&raster, crop, 2, 1).unwrap();
    assert_eq!(exact.as_raw(), &raster.as_raw()[4..]);
    let crop = image::math::Rect {
        x: 0,
        y: 0,
        width: 2,
        height: 1,
    };
    let blended = super::resize_crop(&exact, crop, 1, 1).unwrap();
    let [r, g, b, a] = blended.get_pixel(0, 0).0;
    assert_eq!([r, g, b], [0, 0, 255]);
    assert!((127..=128).contains(&a));
    assert!(super::resize_crop(&raster, crop, 65_536, 65_536).is_err());
}

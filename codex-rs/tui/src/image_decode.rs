//! Decode image pixels in their display orientation within the caller's allocation limits.

use image::ColorType;
use image::DynamicImage;
use image::ImageDecoder;
use image::ImageFormat;
use image::ImageReader;
use image::ImageResult;
use image::Limits;
use image::RgbaImage;
use std::borrow::Cow;
use std::io::BufRead;
use std::io::Cursor;
use std::io::Seek;

pub(crate) fn decode_oriented(
    reader: ImageReader<impl BufRead + Seek>,
    limits: Limits,
) -> ImageResult<DynamicImage> {
    decode_pixels(reader, limits, None)
}

fn decode_pixels(
    mut reader: ImageReader<impl BufRead + Seek>,
    mut limits: Limits,
    output: Option<ColorType>,
) -> ImageResult<DynamicImage> {
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder()?;
    if let Some(color) = output {
        let (width, height) = decoder.dimensions();
        limits.clone().reserve_buffer(width, height, color)?;
    }
    // Match ImageReader::decode's allocation check before allocating the pixel buffer.
    limits.reserve(decoder.total_bytes())?;
    decoder.set_limits(limits)?;

    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(image)
}

/// Prepare RGBA for painting. For 8-bit truecolor PNG, the checked pixel chunks can also
/// travel directly to Kitty. EXIF, transparency keys, animation and other image types use the
/// general decoder so their display semantics match the rest of the TUI.
pub(crate) fn sidebar_pixels(
    bytes: &[u8],
    format: ImageFormat,
    limits: Limits,
) -> color_eyre::Result<(RgbaImage, Option<Cow<'_, [u8]>>)> {
    if format == ImageFormat::Jpeg
        && let Some(image) = jpeg_rgba(bytes, &limits)?
    {
        return Ok((image, None));
    }
    if format == ImageFormat::Png
        && let Some((width, height, png)) = static_rgb_png(bytes)
    {
        limits
            .clone()
            .reserve_buffer(width, height, ColorType::Rgba8)?;
        let mut options = png::DecodeOptions::default();
        options.set_ignore_checksums(false);
        let mut decoder = png::Decoder::new_with_options(Cursor::new(png.as_ref()), options);
        decoder.set_limits(png::Limits {
            bytes: usize::try_from(limits.max_alloc.unwrap_or(u64::MAX)).unwrap_or(usize::MAX),
        });
        let mut reader = decoder.read_info()?;
        let mut image = RgbaImage::new(width, height);
        if reader.info().color_type == png::ColorType::Rgba {
            reader.next_frame(&mut image)?;
        } else {
            // Expand RGB one scanline at a time to avoid a second full-size raster.
            for row in image
                .as_chunks_mut::<4>()
                .0
                .chunks_exact_mut(width as usize)
            {
                let source = reader
                    .next_row()?
                    .ok_or_else(|| color_eyre::eyre::eyre!("Incomplete PNG raster"))?;
                for (dst, &[r, g, b]) in row.iter_mut().zip(source.data().as_chunks::<3>().0) {
                    *dst = [r, g, b, 255];
                }
            }
            // Let the reader flush and verify the end of the compressed frame.
            color_eyre::eyre::ensure!(reader.next_row()?.is_none(), "Unexpected extra PNG row");
        }
        reader.finish()?;
        return Ok((image, Some(png)));
    }
    let reader = ImageReader::with_format(Cursor::new(bytes), format);
    let image = decode_pixels(reader, limits, Some(ColorType::Rgba8))?;
    Ok((image.into_rgba8(), None))
}

/// Decode conventional YCbCr camera images straight into the final RGBA allocation.
/// Other JPEG color spaces go through image's existing conversion path.
fn jpeg_rgba(bytes: &[u8], limits: &Limits) -> color_eyre::Result<Option<RgbaImage>> {
    use image::metadata::Orientation;
    use zune_jpeg::JpegDecoder;
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::new_fast()
        .set_strict_mode(false)
        .set_max_width(usize::from(u16::MAX))
        .set_max_height(usize::from(u16::MAX))
        .jpeg_set_out_colorspace(ColorSpace::RGBA);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(bytes), options);
    decoder.decode_headers()?;
    if decoder.input_colorspace() != Some(ColorSpace::YCbCr) {
        return Ok(None);
    }
    let (width, height) = decoder
        .dimensions()
        .ok_or_else(|| color_eyre::eyre::eyre!("Missing JPEG dimensions"))?;
    let (width, height) = (u32::try_from(width)?, u32::try_from(height)?);
    limits
        .clone()
        .reserve_buffer(width, height, ColorType::Rgba8)?;
    let orientation = decoder
        .exif()
        .and_then(|exif| Orientation::from_exif_chunk(exif));
    let mut pixels = RgbaImage::new(width, height);
    decoder.decode_into(&mut pixels)?;
    let mut image = DynamicImage::ImageRgba8(pixels);
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    Ok(Some(image.into_rgba8()))
}

/// Keep only the PNG signature, IHDR, IDATs and the final IEND. Ignore common ancillary
/// metadata with no bearing on the RGBA samples. The decoder subsequently validates these
/// chunks' CRCs, deflate checksum and scanlines before the bytes are eligible for upload.
fn static_rgb_png(bytes: &[u8]) -> Option<(u32, u32, Cow<'_, [u8]>)> {
    let mut chunks = bytes.strip_prefix(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR")?;
    let width = u32::from_be_bytes(chunks.get(..4)?.try_into().ok()?);
    let height = u32::from_be_bytes(chunks.get(4..8)?.try_into().ok()?);
    if !matches!(chunks.get(8..13)?, [8, 2 | 6, 0, 0, 0]) {
        return None;
    }
    chunks = chunks.get(17..)?; // Remaining IHDR fields and CRC.
    let mut pixels: Option<Vec<u8>> = None;
    let mut has_data = false;
    while !chunks.is_empty() {
        let len = u32::from_be_bytes(chunks.get(..4)?.try_into().ok()?) as usize;
        let (chunk, rest) = chunks.split_at_checked(len.checked_add(12)?)?;
        match chunk.get(4..8)? {
            b"IDAT" => has_data = true,
            b"IEND" if len == 0 && has_data && rest.is_empty() => {
                return Some((
                    width,
                    height,
                    match pixels {
                        Some(mut kept) => {
                            kept.extend_from_slice(chunk);
                            Cow::Owned(kept)
                        }
                        None => Cow::Borrowed(bytes),
                    },
                ));
            }
            b"pHYs" | b"tIME" | b"tEXt" | b"zTXt" | b"iTXt" | b"gAMA" | b"cHRM" | b"sRGB"
            | b"iCCP" | b"bKGD" | b"sBIT" => {
                pixels.get_or_insert_with(|| bytes[..bytes.len() - chunks.len()].to_vec());
                chunks = rest;
                continue;
            }
            _ => return None,
        }
        if let Some(kept) = &mut pixels {
            kept.extend_from_slice(chunk);
        }
        chunks = rest;
    }
    None
}

pub(crate) fn resize_crop(
    raster: &RgbaImage,
    crop: image::math::Rect,
    width: u32,
    height: u32,
) -> color_eyre::Result<RgbaImage> {
    use fast_image_resize as fir;
    // Physical dimensions come from the terminal, which may report a stale size.
    color_eyre::eyre::ensure!(
        u64::from(width) * u64::from(height) <= 16 * 1024 * 1024,
        "Image viewport is too large"
    );
    let source = fir::images::ImageRef::new(
        raster.width(),
        raster.height(),
        raster,
        fir::PixelType::U8x4,
    )?;
    // Cropping the view first keeps alpha premultiplication and filter sampling inside it.
    let visible = fir::images::CroppedImage::new(&source, crop.x, crop.y, crop.width, crop.height)?;
    let mut resized = fir::images::Image::new(width, height, fir::PixelType::U8x4);
    let options = fir::ResizeOptions::new()
        .resize_alg(fir::ResizeAlg::Convolution(fir::FilterType::Bilinear));
    fir::Resizer::new().resize(&visible, &mut resized, &options)?;
    RgbaImage::from_raw(width, height, resized.into_vec())
        .ok_or_else(|| color_eyre::eyre::eyre!("Incomplete resized raster"))
}

pub(crate) fn encode_png(raster: &RgbaImage) -> ImageResult<Vec<u8>> {
    use image::ImageEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    let mut png = Vec::new();
    PngEncoder::new_with_quality(&mut png, CompressionType::Fast, FilterType::Sub).write_image(
        raster,
        raster.width(),
        raster.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(png)
}

#[cfg(test)]
#[path = "image_decode_tests.rs"]
mod tests;

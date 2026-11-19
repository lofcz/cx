//! Sixel encoder for image previews and pet sprites. Frames use an RGB332
//! palette and omit transparent pixels from each six-row band.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;

const ST: &[u8] = b"\x1b\\";
const SIXEL_BAND_HEIGHT: u32 = 6;
const PALETTE_COLOR_COUNT: usize = 256;
const TRANSPARENT_ALPHA_THRESHOLD: u8 = 128;
const TRANSPARENT_BACKGROUND_DCS: &[u8] = b"\x1bP9;1;0q";

pub(crate) fn encode_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    if width == 0 || height == 0 {
        bail!("sixel image dimensions must be non-zero");
    }

    let expected_len = pixel_count(width, height)?
        .checked_mul(4)
        .context("sixel RGBA buffer length overflow")?;
    if rgba.len() != expected_len {
        bail!(
            "sixel RGBA buffer has {} bytes, expected {expected_len}",
            rgba.len()
        );
    }

    let palette = Palette::from_rgba(rgba);
    let mut output = Vec::new();
    output.extend_from_slice(TRANSPARENT_BACKGROUND_DCS);
    output.extend_from_slice(format!("\"1;1;{width};{height}").as_bytes());
    palette.write_definitions(&mut output);
    write_pixels(&mut output, rgba, width as usize);
    output.extend_from_slice(ST);
    Ok(output)
}

/// Build sparse color planes in a single pass over each six-row band. A
/// photograph can use every palette entry in a single band; scanning the
/// original pixels again for every entry makes viewport-sized frames too slow.
fn write_pixels(output: &mut Vec<u8>, rgba: &[u8], width: usize) {
    use std::io::Write as _;
    let pixels = rgba.as_chunks::<4>().0;
    let mut planes: [Vec<(usize, u8)>; PALETTE_COLOR_COUNT] = std::array::from_fn(|_| Vec::new());
    for (band_index, band) in pixels
        .chunks(SIXEL_BAND_HEIGHT as usize * width)
        .enumerate()
    {
        if band_index > 0 {
            output.push(b'-');
        }
        for x in 0..width {
            for (bit, row) in band.chunks_exact(width).enumerate() {
                let [r, g, b, a] = row[x];
                if a < TRANSPARENT_ALPHA_THRESHOLD {
                    continue;
                }
                let plane = &mut planes[usize::from(rgb332_index(r, g, b))];
                if let Some((last, mask)) = plane.last_mut()
                    && *last == x
                {
                    *mask |= 1 << bit;
                } else {
                    plane.push((x, 1 << bit));
                }
            }
        }
        let mut first = true;
        for (color, plane) in planes.iter_mut().enumerate().filter(|(_, p)| !p.is_empty()) {
            if !first {
                output.push(b'$');
            }
            first = false;
            let _ = write!(output, "#{color}");
            let mut cursor = 0;
            let mut iter = plane.iter().copied().peekable();
            while let Some((x, mask)) = iter.next() {
                write_run(output, b'?', x - cursor);
                let mut end = x + 1;
                while iter
                    .next_if(|&(next, m)| next == end && m == mask)
                    .is_some()
                {
                    end += 1;
                }
                write_run(output, b'?' + mask, end - x);
                cursor = end;
            }
            plane.clear();
        }
        if !first && (band_index + 1) * SIXEL_BAND_HEIGHT as usize * width < pixels.len() {
            output.push(b'$');
        }
    }
}

fn write_run(out: &mut Vec<u8>, byte: u8, len: usize) {
    use std::io::Write as _;
    if len > 3 {
        let _ = write!(out, "!{len}");
        out.push(byte);
    } else {
        out.extend(std::iter::repeat_n(byte, len));
    }
}

fn pixel_count(width: u32, height: u32) -> Result<usize> {
    let count = u64::from(width)
        .checked_mul(u64::from(height))
        .context("sixel pixel count overflow")?;
    usize::try_from(count).context("sixel pixel count does not fit usize")
}

fn rgb332_index(red: u8, green: u8, blue: u8) -> u8 {
    let red = red >> 5;
    let green = green >> 5;
    let blue = blue >> 6;
    (red << 5) | (green << 2) | blue
}

fn rgb332_color(index: u8) -> (u8, u8, u8) {
    let red = index >> 5;
    let green = (index >> 2) & 0b111;
    let blue = index & 0b11;
    (
        scale_bucket_to_byte(red, /*max*/ 7),
        scale_bucket_to_byte(green, /*max*/ 7),
        scale_bucket_to_byte(blue, /*max*/ 3),
    )
}

fn scale_bucket_to_byte(bucket: u8, max: u8) -> u8 {
    let value = (u16::from(bucket) * 255) / u16::from(max);
    u8::try_from(value).unwrap_or(u8::MAX)
}

fn byte_to_sixel_percent(value: u8) -> u8 {
    let value = (u16::from(value) * 100) / 255;
    u8::try_from(value).unwrap_or(100)
}

struct Palette {
    used: [bool; PALETTE_COLOR_COUNT],
}

impl Palette {
    fn from_rgba(rgba: &[u8]) -> Self {
        let mut used = [false; PALETTE_COLOR_COUNT];
        const RGBA_CHANNELS: usize = 4;
        for pixel in rgba.as_chunks::<RGBA_CHANNELS>().0 {
            if pixel[3] < TRANSPARENT_ALPHA_THRESHOLD {
                continue;
            }

            used[usize::from(rgb332_index(pixel[0], pixel[1], pixel[2]))] = true;
        }

        Self { used }
    }

    fn indices(&self) -> impl Iterator<Item = u8> + '_ {
        (0..=u8::MAX).filter(|index| self.used[usize::from(*index)])
    }

    fn write_definitions(&self, output: &mut Vec<u8>) {
        for color_index in self.indices() {
            let (red, green, blue) = rgb332_color(color_index);
            output.extend_from_slice(
                format!(
                    "#{color_index};2;{};{};{}",
                    byte_to_sixel_percent(red),
                    byte_to_sixel_percent(green),
                    byte_to_sixel_percent(blue)
                )
                .as_bytes(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED_TRANSPARENT_BACKGROUND_DCS: &str = "\x1bP9;1;0q";

    #[test]
    fn encodes_red_pixel_with_palette_and_pixel_data() {
        let sixel = encode_rgba(&[255, 0, 0, 255], /*width*/ 1, /*height*/ 1).unwrap();
        let sixel = String::from_utf8(sixel).unwrap();

        assert_eq!(
            sixel,
            format!("{EXPECTED_TRANSPARENT_BACKGROUND_DCS}\"1;1;1;1#224;2;100;0;0#224@\x1b\\")
        );
    }

    #[test]
    fn transparent_pixels_do_not_emit_palette_or_pixel_data() {
        let sixel = encode_rgba(&[255, 0, 0, 0], /*width*/ 1, /*height*/ 1).unwrap();
        let sixel = String::from_utf8(sixel).unwrap();

        assert_eq!(
            sixel,
            format!("{EXPECTED_TRANSPARENT_BACKGROUND_DCS}\"1;1;1;1\x1b\\")
        );
    }

    #[test]
    fn multi_band_images_advance_to_next_sixel_band() {
        let mut rgba = Vec::new();
        for _ in 0..7 {
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }

        let sixel = encode_rgba(&rgba, /*width*/ 1, /*height*/ 7).unwrap();
        let sixel = String::from_utf8(sixel).unwrap();

        assert_eq!(
            sixel,
            format!(
                "{EXPECTED_TRANSPARENT_BACKGROUND_DCS}\"1;1;1;7#224;2;100;0;0#224~$-#224@\x1b\\"
            )
        );
    }

    #[test]
    fn repeated_cells_use_sixel_run_length_encoding() {
        let mut rgba = Vec::new();
        for _ in 0..4 {
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }

        let sixel = encode_rgba(&rgba, /*width*/ 4, /*height*/ 1).unwrap();
        let sixel = String::from_utf8(sixel).unwrap();

        assert!(sixel.contains("#224!4@"));
    }

    #[test]
    fn rejects_mismatched_rgba_buffer_length() {
        let err = encode_rgba(&[255, 0, 0], /*width*/ 1, /*height*/ 1).unwrap_err();

        assert_eq!(err.to_string(), "sixel RGBA buffer has 3 bytes, expected 4");
    }
}

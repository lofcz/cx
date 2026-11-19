//! Bounded local image previews for the transcript and composer.
//!
//! Only decoded local image pixels reach the terminal encoders. Previews are never source text;
//! their reserved rows belong to layout. The terminal retains visible placements and any local
//! transport files until those placements change.

pub(crate) mod composer;

use std::io::Write;
use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use crossterm::cursor::RestorePosition;
use crossterm::cursor::SavePosition;
use crossterm::queue;
use image::DynamicImage;
use image::RgbaImage;
use ratatui::layout::Rect;

use crate::pets::ImageProtocol;
use crate::pets::KittyPlacement;

// Separate from the two image ids reserved for pets (0xC0DE and 0xC0DF).
pub(crate) const FIRST_IMAGE_ID: u32 = 0xC100;

pub(crate) struct ImagePlacement {
    pub(crate) image: Arc<TranscriptImage>,
    pub(crate) area: Rect,
    pub(crate) first_row: usize,
    /// Native viewers can place a source rectangle from an already uploaded image.
    pub(crate) crop: Option<image::math::Rect>,
    pub(crate) file: Option<tempfile::NamedTempFile>,
    pub(crate) terminal_id: Option<u32>,
}

impl ImagePlacement {
    /// Register the same pixels for another frame. The terminal owns the transport file.
    pub(crate) fn registration(&self) -> Self {
        Self {
            image: Arc::clone(&self.image),
            area: self.area,
            first_row: self.first_row,
            crop: self.crop,
            file: None,
            terminal_id: None,
        }
    }
}

impl PartialEq for ImagePlacement {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.image, &other.image)
            && self.area == other.area
            && self.first_row == other.first_row
            && self.crop == other.crop
    }
}

pub(crate) struct TranscriptImage {
    pub(crate) protocol: ImageProtocol,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    pub(crate) pixels: Arc<RgbaImage>,
    /// Full-frame PNG or Sixel prepared before the terminal paints the placement.
    pub(crate) encoded: Option<Vec<u8>>,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
}

pub(crate) fn terminal_cell_size() -> Option<(u32, u32)> {
    crossterm::terminal::window_size()
        .ok()
        .filter(|size| size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0)
        .map(|size| {
            (
                u32::from(size.width / size.columns).max(1),
                u32::from(size.height / size.rows).max(1),
            )
        })
}

/// Generated output is larger than an image shown as a tool detail.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreviewKind {
    Inspected,
    Generated,
}

/// A history entry keeps one bounded source thumbnail across text reflows.
#[derive(Clone)]
pub(crate) struct HistoryImage {
    source: Arc<TranscriptImage>,
    pub(crate) display: Arc<TranscriptImage>,
    pub(crate) columns: u16,
    pub(crate) rows: u16,
    native_crop: bool,
}

impl HistoryImage {
    pub(crate) fn fit(
        path: &Path,
        width: u16,
        protocol: ImageProtocol,
        kind: PreviewKind,
        previous: Option<&Self>,
    ) -> Option<Self> {
        let columns = width.checked_sub(2).filter(|n| *n > 0)?;
        let source = match previous {
            Some(image) => Arc::clone(&image.source),
            None => Arc::new(TranscriptImage::load(path, u16::MAX, protocol, kind)?),
        };
        let columns = columns.min(source.columns);
        if let Some(previous) = previous.filter(|image| image.columns == columns) {
            return Some(previous.clone());
        }
        let native_crop =
            protocol == ImageProtocol::Kitty && crate::tui::tmux_fullscreen_image_base().is_none();
        let pixels = &source.pixels;
        let w = pixels.width().min(u32::from(columns) * source.cell_width);
        let h = ((u64::from(pixels.height()) * u64::from(w) + u64::from(pixels.width() / 2))
            / u64::from(pixels.width()))
        .max(1) as u32;
        let rows = h.div_ceil(source.cell_height) as u16;
        // Native Kitty scales the uploaded thumbnail. Sixel and virtual placements
        // need a fitted raster and can resample the bounded source in memory.
        let display = if native_crop || w == pixels.width() {
            Arc::clone(&source)
        } else {
            Arc::new(TranscriptImage {
                protocol,
                pixels: Arc::new(image::imageops::thumbnail(pixels.as_ref(), w, h)),
                encoded: None,
                columns,
                rows,
                cell_width: source.cell_width,
                cell_height: source.cell_height,
            })
        };
        Some(Self {
            source,
            display,
            columns,
            rows,
            native_crop,
        })
    }

    pub(crate) fn placement(&self, area: Rect, first_row: usize) -> ImagePlacement {
        let crop = self.native_crop.then(|| {
            let pixels = &self.source.pixels;
            let y = |row: usize| (row as u32 * pixels.height()) / u32::from(self.rows);
            let top = y(first_row);
            image::math::Rect {
                x: 0,
                y: top,
                width: pixels.width(),
                height: y(first_row + usize::from(area.height)) - top,
            }
        });
        ImagePlacement {
            image: Arc::clone(&self.display),
            area,
            // A native crop names its source rows directly.
            first_row: if self.native_crop { 0 } else { first_row },
            crop,
            file: None,
            terminal_id: None,
        }
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.source.pixels.len()
            + if Arc::ptr_eq(&self.source, &self.display) {
                0
            } else {
                self.display.pixels.len()
            }
    }
}

impl TranscriptImage {
    fn load_pixels(path: &Path, max_width: u32, max_height: u32) -> Option<RgbaImage> {
        if max_width == 0 || max_height == 0 {
            return None;
        }
        let image = Self::decode(path)?;
        Some(
            image
                .thumbnail(max_width.min(image.width()), max_height.min(image.height()))
                .to_rgba8(),
        )
    }

    fn decode(path: &Path) -> Option<DynamicImage> {
        let metadata = path.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > 32 * 1024 * 1024 {
            return None;
        }
        let reader = image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?;
        // The reader checks the decoded size before allocating the raster. Keep normal
        // screenshots usable, but never allocate hundreds of MB for an optional thumbnail.
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(32 * 1024 * 1024);
        crate::image_decode::decode_oriented(reader, limits).ok()
    }

    pub(crate) fn load(
        path: &Path,
        width: u16,
        protocol: ImageProtocol,
        kind: PreviewKind,
    ) -> Option<Self> {
        let columns = width.saturating_sub(2);
        if columns == 0 {
            return None;
        }
        // Sixel paints physical pixels. Guessing could paint outside the reserved rows.
        let (cell_width, cell_height) = match protocol {
            ImageProtocol::Sixel => terminal_cell_size()?,
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => match kind {
                PreviewKind::Inspected => (8, 16),
                PreviewKind::Generated => terminal_cell_size().unwrap_or((8, 16)),
            },
        };
        let image = Self::decode(path)?;
        // Keep the current Sixel encoding and tmux placeholder budgets.
        let (columns, max_width, max_height) = match (protocol, kind) {
            (ImageProtocol::Sixel, _) => (columns.min(48), 160, 80.min(6 * cell_height)),
            (_, PreviewKind::Inspected) => (columns.min(48), 384, 6 * cell_height),
            (_, PreviewKind::Generated) => {
                let ratio = f64::from(image.width()) / f64::from(image.height());
                let max_width = if ratio >= 2.0 {
                    768
                } else if ratio >= 1.01 {
                    480
                } else {
                    400
                };
                let max_height = if crate::tui::tmux_fullscreen_image_base().is_some() {
                    6 * cell_height
                } else {
                    400
                };
                (columns, max_width, max_height)
            }
        };
        let max_width = (u32::from(columns) * cell_width).min(max_width);
        let pixels = image
            .thumbnail(max_width.min(image.width()), max_height.min(image.height()))
            .to_rgba8();
        Some(Self {
            protocol,
            columns: pixels.width().div_ceil(cell_width) as u16,
            rows: pixels.height().div_ceil(cell_height) as u16,
            pixels: Arc::new(pixels),
            encoded: None,
            cell_width,
            cell_height,
        })
    }

    /// Draw only the visible vertical slice, never covering the prompt header or composer.
    pub(crate) fn draw(
        &self,
        writer: &mut impl Write,
        area: Rect,
        first_row: usize,
        crop: Option<image::math::Rect>,
        placement: KittyPlacement,
    ) -> Result<Option<tempfile::NamedTempFile>> {
        let encoded;
        let bytes = if let Some(bytes) = &self.encoded {
            bytes.as_slice()
        } else if let Ok(Some(slice)) = self.encode_rows(
            if crop.is_some() {
                Rect {
                    height: self.rows,
                    ..area
                }
            } else {
                area
            },
            first_row,
        ) {
            encoded = slice;
            &encoded
        } else {
            return Ok(None);
        };
        let mut file = None;
        let command;
        let payload = match (self.protocol, placement, crop) {
            (ImageProtocol::Kitty, KittyPlacement::Cursor(id), Some(source)) => {
                command =
                    crate::pets::kitty_png_command(bytes, &format!("a=t,t=d,f=100,q=2,i={id}"))
                        + &crate::pets::kitty_place_image(id, area, Some(source), false);
                command.as_bytes()
            }
            (ImageProtocol::Kitty, _, _) => {
                command = crate::pets::kitty_display_png(bytes, area.width, area.height, placement);
                command.as_bytes()
            }
            (ImageProtocol::KittyLocalFile, _, _) => {
                // Transport-file failures leave the text UI available.
                let Ok(mut png) = tempfile::Builder::new().suffix(".png").tempfile() else {
                    return Ok(None);
                };
                if png.write_all(bytes).is_err() {
                    return Ok(None);
                }
                let Ok(cmd) = crate::pets::kitty_transmit_png_file_with_id(
                    png.path(),
                    area.width,
                    area.height,
                    placement,
                ) else {
                    return Ok(None);
                };
                command = cmd;
                file = Some(png);
                command.as_bytes()
            }
            (ImageProtocol::Sixel, _, _) => bytes,
        };
        queue!(writer, SavePosition)?;
        placement.draw(writer, area, payload)?;
        queue!(writer, RestorePosition)?;
        Ok(file)
    }

    fn encode_rows(&self, area: Rect, first_row: usize) -> Result<Option<Vec<u8>>> {
        let (top, bottom) = match self.protocol {
            ImageProtocol::Sixel => (
                first_row as u32 * self.cell_height,
                (first_row as u32 + u32::from(area.height)) * self.cell_height,
            ),
            // Kitty scales the entire raster to r=rows; clip at those scaled boundaries.
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => (
                first_row as u32 * self.pixels.height() / u32::from(self.rows),
                (first_row as u32 + u32::from(area.height)) * self.pixels.height()
                    / u32::from(self.rows),
            ),
        };
        let top = top.min(self.pixels.height());
        let height = bottom.min(self.pixels.height()) - top;
        if height == 0 {
            return Ok(None);
        }
        let slice;
        let cropped = if top == 0 && height == self.pixels.height() {
            self.pixels.as_ref()
        } else {
            slice = image::imageops::crop_imm(
                self.pixels.as_ref(),
                0,
                top,
                self.pixels.width(),
                height,
            )
            .to_image();
            &slice
        };
        let bytes = match self.protocol {
            ImageProtocol::Kitty | ImageProtocol::KittyLocalFile => {
                crate::image_decode::encode_png(cropped)?
            }
            ImageProtocol::Sixel => {
                crate::pets::encode_rgba(cropped.as_raw(), cropped.width(), cropped.height())?
            }
        };
        Ok(Some(bytes))
    }
}

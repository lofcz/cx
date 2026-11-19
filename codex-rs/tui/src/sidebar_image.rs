//! Decode a workspace image once and reuse the transcript's terminal image lifecycle.

mod viewport;

use crate::motion::MotionMode;
use crate::tui::FrameRequester;
use crossterm::event::{KeyEvent, MouseEvent};
use image::RgbaImage;
use image::math::Rect as PixelRect;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;
use viewport::Viewport;

use color_eyre::eyre::OptionExt;
use color_eyre::eyre::Result;
use image::ImageFormat;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::widgets::Widget;

use crate::pets::ImageProtocol;
use crate::transcript_image::ImagePlacement;
use crate::transcript_image::TranscriptImage;
use crate::transcript_image::terminal_cell_size;

#[derive(Debug)]
pub(crate) enum DocumentContent {
    Text(String),
    Image(Box<SidebarImage>),
}

pub(crate) struct SidebarImage {
    source: Arc<ImageSource>,
    view: Viewport,
    // Only one rasterization may be in flight. Input updates the desired view
    // immediately and the next paint samples its latest position.
    pending: Option<oneshot::Receiver<Rendered>>,
    ready: Option<Rendered>,
}

struct ImageSource {
    label: String,
    raster: Arc<RgbaImage>,
    native: OnceLock<Result<Arc<TranscriptImage>, String>>,
    last_crop: Mutex<Option<Rendered>>,
}

impl std::fmt::Debug for SidebarImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidebarImage")
            .field("label", &self.source.label)
            .finish_non_exhaustive()
    }
}

pub(crate) fn supports_path(path: &str) -> bool {
    path.rsplit_once('.')
        .and_then(|(_, extension)| ImageFormat::from_extension(extension))
        .is_some_and(supported_format)
}

fn supported_format(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::WebP
    )
}

impl SidebarImage {
    pub(crate) fn decode_for_terminal(
        bytes: &[u8],
        protocol: Option<ImageProtocol>,
    ) -> Result<Self> {
        const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;
        color_eyre::eyre::ensure!(
            bytes.len() as u64 <= MAX_IMAGE_BYTES,
            "Image file is too large"
        );
        let format = image::guess_format(bytes)
            .ok()
            .filter(|format| supported_format(*format))
            .ok_or_eyre("Unsupported image format")?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(MAX_IMAGE_BYTES);
        let (image, png) = crate::image_decode::sidebar_pixels(bytes, format, limits)?;
        let (width, height) = image.dimensions();
        let source = Arc::new(ImageSource {
            label: format!(
                "{} · {width} × {height}",
                format.extensions_str()[0].to_ascii_uppercase()
            ),
            raster: Arc::new(image),
            native: OnceLock::new(),
            last_crop: Mutex::default(),
        });
        if protocol.map(ImageProtocol::for_interactive_image) == Some(ImageProtocol::Kitty)
            && crate::tui::tmux_fullscreen_image_base().is_none()
        {
            let _ = source.native(png.map(|bytes| bytes.into_owned()));
        }
        Ok(Self::new_view(source))
    }

    fn new_view(source: Arc<ImageSource>) -> Self {
        let (width, height) = source.raster.dimensions();
        Self {
            source,
            view: Viewport::new(width, height),
            pending: None,
            ready: None,
        }
    }

    pub(crate) fn fork(&self) -> Self {
        Self::new_view(Arc::clone(&self.source))
    }

    pub(crate) fn mouse(&mut self, event: MouseEvent) -> bool {
        self.view.mouse(event, Instant::now())
    }

    pub(crate) fn key(&mut self, event: KeyEvent) {
        self.view.key(event, Instant::now());
    }

    pub(crate) fn paint(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        protocol: Option<ImageProtocol>,
        motion: MotionMode,
        frames: &FrameRequester,
    ) -> Option<ImagePlacement> {
        Line::from(self.source.label.as_str())
            .dim()
            .render(area, buf);
        let available_rows = area.height.saturating_sub(1 + u16::from(area.height > 2));
        let virtual_placement = crate::tui::tmux_fullscreen_image_base().is_some();
        // tmux uses Kitty's virtual placements. The shared placeholder renderer
        // currently emits row marks for up to six rows.
        let rows = if virtual_placement {
            available_rows.min(6)
        } else {
            available_rows
        };
        let body = Rect::new(area.x, area.y.saturating_add(1), area.width, rows);
        if body.is_empty() {
            return None;
        }
        let display = protocol
            .map(ImageProtocol::for_interactive_image)
            .and_then(|p| {
                Some((
                    p,
                    terminal_cell_size()
                        .or_else(|| (p != ImageProtocol::Sixel).then_some((8, 16)))?,
                ))
            });
        let Some((protocol, (cw, ch))) = display else {
            Line::from("Image preview unavailable in this terminal").render(body, buf);
            return None;
        };
        let native = protocol == ImageProtocol::Kitty && !virtual_placement;
        self.view
            .layout(body, [cw, ch], native && motion == MotionMode::Animated);
        if self.view.advance(Instant::now()) {
            frames.schedule_frame_in(Duration::from_millis(12));
        }
        let fit = if self.view.fitting() { " · Fit" } else { "" };
        Line::from(format!(
            "{} · {:.0}%{fit}",
            self.source.label,
            self.view.scale() * 100.0
        ))
        .dim()
        .render(area, buf);
        let (crop, mut placement, pixels) = self.view.geometry();
        let plan = if native {
            RenderPlan::Native
        } else {
            RenderPlan::Raster {
                protocol,
                crop,
                pixels,
                cell: [cw, ch],
            }
        };
        if let Some(rx) = &mut self.pending {
            match rx.try_recv() {
                Ok(done) => {
                    self.ready = Some(done);
                    self.pending = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => self.pending = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        if self.ready.as_ref().is_none_or(|r| r.plan != plan)
            && let Some(prepared) = self.source.prepared(plan)
        {
            self.ready = Some(prepared);
        }
        if self.pending.is_none() && self.ready.as_ref().is_none_or(|r| r.plan != plan) {
            let (tx, rx) = oneshot::channel();
            self.pending = Some(rx);
            let source = Arc::clone(&self.source);
            let frames = frames.clone();
            tokio::task::spawn_blocking(move || {
                let result = match plan {
                    RenderPlan::Native => source.native(None).clone(),
                    RenderPlan::Raster {
                        protocol,
                        crop,
                        pixels,
                        cell,
                    } => source
                        .render_crop(protocol, crop, pixels, cell)
                        .map(Arc::new)
                        .map_err(|err| err.to_string()),
                };
                let done = Rendered { plan, result };
                if plan != RenderPlan::Native {
                    *source
                        .last_crop
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner) = Some(done.clone());
                }
                if tx.send(done).is_ok() {
                    frames.schedule_frame();
                }
            });
        }
        let Some(ready) = &self.ready else {
            Line::from("Rendering image…").dim().render(body, buf);
            return None;
        };
        let image = match &ready.result {
            Ok(image) => image,
            Err(error) => {
                Line::from(format!("Image preview failed · {error}")).render(body, buf);
                return None;
            }
        };
        // Continue displaying the last finished fallback frame while encoding
        // the next crop, provided it still fits inside this panel.
        if ready.plan != plan {
            if native
                || image.protocol != protocol
                || ready.plan == RenderPlan::Native
                || image.cell_width != cw
                || image.cell_height != ch
                || image.columns > body.width
                || image.rows > body.height
            {
                return None;
            }
            placement = Rect::new(
                body.x + (body.width - image.columns) / 2,
                body.y + (body.height - image.rows) / 2,
                image.columns,
                image.rows,
            );
        }
        Some(ImagePlacement {
            image: Arc::clone(image),
            area: placement,
            first_row: 0,
            crop: native.then_some(crop),
            file: None,
            terminal_id: None,
        })
    }
}

#[derive(Clone)]
struct Rendered {
    plan: RenderPlan,
    result: Result<Arc<TranscriptImage>, String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderPlan {
    // The entire original is uploaded once. Destination and source rectangles
    // then change independently without decoding or transmitting more pixels.
    Native,
    Raster {
        protocol: ImageProtocol,
        crop: PixelRect,
        pixels: [u32; 2],
        cell: [u32; 2],
    },
}

impl ImageSource {
    fn prepared(&self, plan: RenderPlan) -> Option<Rendered> {
        match plan {
            RenderPlan::Native => self.native.get().map(|result| Rendered {
                plan,
                result: result.clone(),
            }),
            RenderPlan::Raster { .. } => self
                .last_crop
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .filter(|cached| cached.plan == plan)
                .cloned(),
        }
    }

    fn native(&self, png: Option<Vec<u8>>) -> &Result<Arc<TranscriptImage>, String> {
        self.native.get_or_init(|| {
            let encoded = match png {
                Some(bytes) => bytes,
                None => crate::image_decode::encode_png(&self.raster)
                    .map_err(|error| error.to_string())?,
            };
            Ok(Arc::new(TranscriptImage {
                protocol: ImageProtocol::Kitty,
                columns: 1,
                rows: 1,
                pixels: Arc::clone(&self.raster),
                encoded: Some(encoded),
                cell_width: self.raster.width(),
                cell_height: self.raster.height(),
            }))
        })
    }

    fn render_crop(
        &self,
        protocol: ImageProtocol,
        crop: PixelRect,
        [width, height]: [u32; 2],
        cell: [u32; 2],
    ) -> Result<TranscriptImage> {
        let raster = crate::image_decode::resize_crop(&self.raster, crop, width, height)?;
        let encoded = if protocol == ImageProtocol::Sixel {
            crate::pets::encode_rgba(&raster, raster.width(), raster.height())
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
        } else {
            crate::image_decode::encode_png(&raster)?
        };
        Ok(TranscriptImage {
            protocol,
            columns: raster.width().div_ceil(cell[0]) as u16,
            rows: raster.height().div_ceil(cell[1]) as u16,
            pixels: Arc::new(raster),
            encoded: Some(encoded),
            cell_width: cell[0],
            cell_height: cell[1],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn fork_shares_prepared_native_pixels_with_a_fresh_view() {
        let raster = RgbaImage::from_pixel(16, 8, image::Rgba([1, 20, 39, 128]));
        let bytes = crate::image_decode::encode_png(&raster).unwrap();
        let mut original =
            SidebarImage::decode_for_terminal(&bytes, Some(ImageProtocol::Kitty)).unwrap();
        let prepared = Arc::clone(original.source.native(None).as_ref().unwrap());
        original.view.layout(Rect::new(0, 0, 8, 4), [8, 16], false);
        original.key(KeyCode::Char('1').into());
        let peek = original.fork();
        assert!(peek.view.fitting());
        assert!(!original.view.fitting());
        assert!(Arc::ptr_eq(&original.source, &peek.source));
        let warm = peek
            .source
            .prepared(RenderPlan::Native)
            .unwrap()
            .result
            .unwrap();
        assert!(Arc::ptr_eq(&prepared, &warm));
    }
}

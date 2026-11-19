//! Decode a workspace image once and reuse the transcript's terminal image lifecycle.

mod viewport;

use crate::motion::MotionMode;
use crate::tui::FrameRequester;
use crossterm::event::{KeyEvent, MouseEvent};
use image::imageops::{self, FilterType};
use image::math::Rect as PixelRect;
use image::{ImageEncoder, RgbaImage};
use std::io::Cursor;
use std::sync::Arc;
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
    label: String,
    raster: Arc<RgbaImage>,
    view: Viewport,
    // Only one rasterization may be in flight. Input updates the desired view
    // immediately and the next paint samples its latest position.
    pending: Option<oneshot::Receiver<Rendered>>,
    ready: Option<Rendered>,
}

impl std::fmt::Debug for SidebarImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SidebarImage")
            .field("label", &self.label)
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
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
        let format = reader
            .format()
            .filter(|format| supported_format(*format))
            .ok_or_eyre("Unsupported image format")?;
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(32 * 1024 * 1024);
        let image = crate::image_decode::decode_oriented(reader, limits)?;
        let (width, height) = (image.width(), image.height());
        Ok(Self {
            label: format!(
                "{} · {width} × {height}",
                format.extensions_str()[0].to_ascii_uppercase()
            ),
            raster: Arc::new(image.into_rgba8()),
            view: Viewport::new(width, height),
            pending: None,
            ready: None,
        })
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
        Line::from(self.label.as_str()).dim().render(area, buf);
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
        let display = protocol.map(ImageProtocol::for_sidebar).and_then(|p| {
            Some((
                p,
                terminal_cell_size().or_else(|| (p != ImageProtocol::Sixel).then_some((8, 16)))?,
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
            self.label,
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
        if self.pending.is_none() && self.ready.as_ref().is_none_or(|r| r.plan != plan) {
            let (tx, rx) = oneshot::channel();
            self.pending = Some(rx);
            let source = self.raster.clone();
            let frames = frames.clone();
            tokio::task::spawn_blocking(move || {
                let result = plan
                    .render(source)
                    .map(Arc::new)
                    .map_err(|err| err.to_string());
                if tx.send(Rendered { plan, result }).is_ok() {
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

impl RenderPlan {
    fn render(self, source: Arc<RgbaImage>) -> Result<TranscriptImage> {
        let (protocol, raster, cell) = match self {
            Self::Native => {
                let cell = [source.width(), source.height()];
                (ImageProtocol::Kitty, source, cell)
            }
            Self::Raster {
                protocol,
                crop,
                pixels: [width, height],
                cell,
            } => {
                // The terminal reports physical dimensions. Protect the allocation
                // if a stale window-size report describes an implausibly large panel.
                color_eyre::eyre::ensure!(
                    u64::from(width) * u64::from(height) <= 16 * 1024 * 1024,
                    "Image viewport is too large"
                );
                let visible =
                    imageops::crop_imm(source.as_ref(), crop.x, crop.y, crop.width, crop.height);
                (
                    protocol,
                    Arc::new(imageops::resize(
                        &*visible,
                        width,
                        height,
                        FilterType::Triangle,
                    )),
                    cell,
                )
            }
        };
        let encoded = if protocol == ImageProtocol::Sixel {
            crate::pets::encode_rgba(&raster, raster.width(), raster.height())
                .map_err(|e| color_eyre::eyre::eyre!("{e}"))?
        } else {
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(
                &raster,
                raster.width(),
                raster.height(),
                image::ExtendedColorType::Rgba8,
            )?;
            png
        };
        Ok(TranscriptImage {
            protocol,
            columns: raster.width().div_ceil(cell[0]) as u16,
            rows: raster.height().div_ceil(cell[1]) as u16,
            pixels: raster,
            encoded: Some(encoded),
            cell_width: cell[0],
            cell_height: cell[1],
        })
    }
}

//! The draft thumbnail strip prepares visible tiles off-thread and places them in textarea
//! reference order. The cache can retain one full strip across divider drags.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use image::Rgba;
use image::RgbaImage;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;
use tokio::sync::oneshot;

use super::ImagePlacement;
use super::TranscriptImage;
use super::terminal_cell_size;
use crate::bottom_pane::LocalImageAttachment;
use crate::bottom_pane::TextArea;
use crate::pets::ImageProtocol;
use crate::tui::FrameRequester;

// Six columns by three rows is square with typical 8 × 16 pixel cells.
const WIDTH: u16 = 6;
const HEIGHT: u16 = 3;
const FILE_RECHECK_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone)]
struct CachedTile {
    version: Option<(u64, Option<SystemTime>)>,
    checked: Instant,
    image: Option<Arc<TranscriptImage>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct TileStyle {
    protocol: ImageProtocol,
    cell_size: (u32, u32),
    background: (u8, u8, u8),
}

#[derive(Default)]
struct TileCache {
    ready: HashMap<PathBuf, CachedTile>,
    pending: Option<oneshot::Receiver<(PathBuf, CachedTile)>>,
    widest_strip: usize,
    last_check: Option<Instant>,
}

pub(crate) struct ComposerImages {
    style: TileStyle,
    frames: FrameRequester,
    cache: RefCell<TileCache>,
    pub(crate) placements: RefCell<Vec<ImagePlacement>>,
}

impl ComposerImages {
    pub(crate) fn for_terminal(
        protocol: ImageProtocol,
        frames: &FrameRequester,
        mut previous: Option<Self>,
    ) -> Option<Self> {
        // Direct Konsole supports keyed Kitty placements. Its Sixel graphics can survive
        // overwriting the text underneath a removed thumbnail.
        let protocol = protocol.for_interactive_image();
        // Kitty can scale a nominal raster into its cell placement. Sixel needs physical pixels.
        let cell_size = terminal_cell_size()
            .or_else(|| (protocol != ImageProtocol::Sixel).then_some((8, 16)))?;
        let background = crate::terminal_palette::default_bg()
            .map(crate::style::user_message_bg_rgb)
            .unwrap_or((48, 48, 48));
        let style = TileStyle {
            protocol,
            cell_size,
            background,
        };
        if let Some(old) = previous.as_mut()
            && old.style == style
        {
            // A backdrop can render without presenting its registrations. Each frame
            // must register only the thumbnails actually painted by its composer.
            old.placements.get_mut().clear();
            return previous;
        }
        Some(Self {
            style,
            frames: frames.clone(),
            cache: RefCell::default(),
            placements: RefCell::default(),
        })
    }

    /// Reserve a blank row between any visible strip and the draft.
    pub(crate) fn height(&self, width: u16, count: usize) -> u16 {
        let (visible, hidden) = visible_thumbnails(width, count);
        if visible > 0 {
            HEIGHT + 1
        } else if hidden > 0 && u32::from(width) >= 2 + hidden.ilog10() {
            2
        } else {
            0
        }
    }

    pub(crate) fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        attachments: &[LocalImageAttachment],
        textarea: &TextArea,
    ) {
        let (visible, hidden) = visible_thumbnails(area.width, attachments.len());
        let mut cache = self.cache.borrow_mut();
        if let Some(pending) = &mut cache.pending {
            match pending.try_recv() {
                Ok((path, tile)) => {
                    cache.last_check = Some(Instant::now());
                    cache.ready.insert(path, tile);
                    cache.pending = None;
                }
                Err(oneshot::error::TryRecvError::Closed) => cache.pending = None,
                Err(oneshot::error::TryRecvError::Empty) => {}
            }
        }
        let attached_paths: HashSet<_> = attachments
            .iter()
            .map(|image| image.path.as_path())
            .collect();
        cache
            .ready
            .retain(|path, _| attached_paths.contains(path.as_path()));
        let mut placements = self.placements.borrow_mut();
        placements.clear();
        let style = crate::style::secondary_text_style();
        let mut x = area.x;
        if area.is_empty() {
            return;
        }
        let mut ordered: Vec<_> = attachments.iter().collect();
        if ordered.len() > 1 {
            let positions: HashMap<_, _> = textarea
                .text_element_ranges()
                .map(|range| (&textarea.text()[range.clone()], range.start))
                .collect();
            ordered.sort_by_key(|image| {
                positions
                    .get(image.placeholder.as_str())
                    .copied()
                    .unwrap_or(usize::MAX)
            });
        }
        cache.widest_strip = cache.widest_strip.max(visible);
        let shown = &ordered[..visible];
        let mut excess = cache.ready.len().saturating_sub(cache.widest_strip);
        if excess > 0 {
            let shown: HashSet<_> = shown.iter().map(|image| image.path.as_path()).collect();
            cache.ready.retain(|path, _| {
                if excess == 0 || shown.contains(path.as_path()) {
                    return true;
                }
                excess -= 1;
                false
            });
        }
        cache.prepare(shown, self.style, &self.frames);
        for attachment in shown {
            let path = &attachment.path;
            let tile = Rect::new(x, area.y, WIDTH, HEIGHT);
            if let Some(image) = cache.ready.get(path).and_then(|tile| tile.image.as_ref()) {
                placements.push(ImagePlacement {
                    image: Arc::clone(image),
                    area: tile,
                    first_row: 0,
                    crop: None,
                    file: None,
                    terminal_id: None,
                });
            } else {
                Paragraph::new(attachment.placeholder.as_str())
                    .style(style)
                    .wrap(Wrap { trim: false })
                    .render(tile, buf);
            }
            x = x.saturating_add(WIDTH + 1);
        }
        if hidden > 0 {
            buf.set_string(x, area.y, format!("+{hidden}"), style);
        }
    }
}

impl TileCache {
    fn prepare(
        &mut self,
        shown: &[&LocalImageAttachment],
        style: TileStyle,
        frames: &FrameRequester,
    ) {
        if self.pending.is_some() {
            return;
        }
        let paths = shown.iter().map(|attachment| &attachment.path);
        let mut next = paths.clone().find(|path| !self.ready.contains_key(*path));
        if next.is_none()
            && self
                .last_check
                .is_some_and(|last| last.elapsed() >= FILE_RECHECK_INTERVAL)
        {
            // On a subsequent UI frame, revalidate at most one cached file. Prioritize
            // the oldest check so repeated drags cannot hammer a shared filesystem.
            next = paths.min_by_key(|path| self.ready[*path].checked);
        }
        let Some(path) = next.cloned() else { return };
        let previous = self.ready.get(&path).cloned();
        let frames = frames.clone();
        let (tx, rx) = oneshot::channel();
        self.pending = Some(rx);
        tokio::task::spawn_blocking(move || {
            if tx.is_closed() {
                return;
            }
            let version = path.metadata().ok().map(|m| (m.len(), m.modified().ok()));
            let image = match previous {
                Some(tile) if tile.version == version => tile.image,
                _ => version.and_then(|_| style.load_tile(&path).map(Arc::new)),
            };
            let tile = CachedTile {
                version,
                image,
                checked: Instant::now(),
            };
            if tx.send((path, tile)).is_ok() {
                frames.schedule_frame();
            }
        });
    }
}

impl TileStyle {
    fn load_tile(self, path: &Path) -> Option<TranscriptImage> {
        let (cell_width, cell_height) = self.cell_size;
        let width = u32::from(WIDTH) * cell_width;
        let height = u32::from(HEIGHT) * cell_height;
        let scale = width.div_ceil(384).max(height.div_ceil(160));
        // Kitty scales the raster to the tile. Sixel must paint at the physical cell size.
        if scale > 1 && self.protocol == ImageProtocol::Sixel {
            return None;
        }
        let width = (width / scale).max(3);
        let height = (height / scale).max(3);
        let pixels = TranscriptImage::load_pixels(path, width - 2, height - 2)?;
        let (r, g, b) = self.background;
        let contrast = if crate::color::is_light(self.background) {
            0
        } else {
            255
        };
        let (red, green, blue) = crate::color::blend(
            (contrast, contrast, contrast),
            self.background,
            /*alpha*/ 0.45,
        );
        let edge = Rgba([red, green, blue, 255]);
        let mut tile = RgbaImage::from_fn(width, height, |x, y| {
            if x == 0 || y == 0 || x == width - 1 || y == height - 1 {
                edge
            } else {
                Rgba([r, g, b, 255])
            }
        });
        image::imageops::overlay(
            &mut tile,
            &pixels,
            i64::from((width - pixels.width()) / 2),
            i64::from((height - pixels.height()) / 2),
        );
        let mut image = TranscriptImage {
            protocol: self.protocol,
            columns: WIDTH,
            rows: HEIGHT,
            pixels: Arc::new(tile),
            encoded: None,
            cell_width,
            cell_height,
        };
        image.encoded = Some(
            image
                .encode_rows(Rect::new(0, 0, WIDTH, HEIGHT), 0)
                .ok()??,
        );
        Some(image)
    }
}

fn visible_thumbnails(width: u16, count: usize) -> (usize, usize) {
    let mut visible = ((usize::from(width) + 1) / usize::from(WIDTH + 1)).min(count);
    while visible > 0 && visible < count {
        let badge = 2 + (count - visible).ilog10() as usize;
        if visible * usize::from(WIDTH + 1) + badge <= usize::from(width) {
            break;
        }
        visible -= 1;
    }
    (visible, count - visible)
}

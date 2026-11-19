use std::time::Duration;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use image::math::Rect as PixelRect;
use ratatui::layout::{Position, Rect};

const ZOOM_STEP: f64 = 1.20;
const ZOOM_TIME: Duration = Duration::from_millis(95);

struct Zoom {
    start: Instant,
    from: f64,
    to: f64,
    anchor: [f64; 2],
}

pub(super) struct Viewport {
    source: [f64; 2],
    center: [f64; 2],
    scale: Option<f64>,
    body: Rect,
    cell: [u32; 2],
    drag: Option<Position>,
    last_click: Option<(Position, Instant)>,
    animated: bool,
    zoom: Option<Zoom>,
}

impl Viewport {
    pub(super) fn new(width: u32, height: u32) -> Self {
        let source = [f64::from(width), f64::from(height)];
        Self {
            source,
            center: source.map(|n| n * 0.5),
            scale: None,
            body: Rect::default(),
            cell: [1, 1],
            drag: None,
            last_click: None,
            animated: false,
            zoom: None,
        }
    }

    pub(super) fn layout(&mut self, body: Rect, cell: [u32; 2], animated: bool) {
        self.body = body;
        self.cell = cell;
        self.animated = animated;
        self.clamp_center();
    }

    fn extent(&self) -> [f64; 2] {
        [
            f64::from(self.body.width) * f64::from(self.cell[0]),
            f64::from(self.body.height) * f64::from(self.cell[1]),
        ]
    }

    fn fit(&self) -> f64 {
        let [w, h] = self.extent();
        (w / self.source[0]).min(h / self.source[1])
    }

    pub(super) fn scale(&self) -> f64 {
        self.scale.unwrap_or_else(|| self.fit())
    }

    pub(super) fn fitting(&self) -> bool {
        self.scale.is_none()
    }

    fn clamp_center(&mut self) {
        let scale = self.scale();
        for (i, extent) in self.extent().into_iter().enumerate() {
            let half = (extent / scale).min(self.source[i]) * 0.5;
            self.center[i] = self.center[i].clamp(half, self.source[i] - half);
        }
    }

    /// Screen-pixel offsets from the canvas center, clamped to the displayed image
    /// so that zooming over a letterbox anchors the nearest image edge.
    fn anchor(&self, pos: Position) -> [f64; 2] {
        let extent = self.extent();
        let pos = [
            i32::from(pos.x) - i32::from(self.body.x),
            i32::from(pos.y) - i32::from(self.body.y),
        ];
        std::array::from_fn(|i| {
            let half = (self.source[i] * self.scale()).min(extent[i]) * 0.5;
            ((f64::from(pos[i]) + 0.5) * f64::from(self.cell[i]) - extent[i] * 0.5)
                .clamp(-half, half)
        })
    }

    fn set_scale(&mut self, scale: f64, anchor: [f64; 2]) {
        let delta = self.scale().recip() - scale.recip();
        for (i, offset) in anchor.into_iter().enumerate() {
            self.center[i] += offset * delta;
        }
        self.scale = Some(scale);
        self.clamp_center();
    }

    pub(super) fn advance(&mut self, now: Instant) -> bool {
        let Some(zoom) = self.zoom.as_ref() else {
            return false;
        };
        let t = (now.duration_since(zoom.start).as_secs_f64() / ZOOM_TIME.as_secs_f64()).min(1.0);
        let scale = if t == 1.0 {
            zoom.to
        } else {
            zoom.from * (zoom.to / zoom.from).powf(1.0 - (1.0 - t).powi(3))
        };
        self.set_scale(scale, zoom.anchor);
        if t == 1.0 {
            self.zoom = None;
        }
        self.zoom.is_some()
    }

    fn step(&mut self, factor: f64, anchor: [f64; 2], now: Instant) {
        self.advance(now);
        let current = self.scale();
        let target = self.zoom.as_ref().map_or(current, |z| z.to) * factor;
        let fit = self.fit();
        let target = target.clamp(fit.min(1.0) / 4.0, fit.max(32.0));
        if self.animated {
            self.zoom = Some(Zoom {
                start: now,
                from: current,
                to: target,
                anchor,
            });
        } else {
            self.set_scale(target, anchor);
        }
    }

    fn reset(&mut self) {
        self.scale = None;
        self.center = self.source.map(|n| n * 0.5);
        self.zoom = None;
    }

    fn pan(&mut self, cols: i32, rows: i32) {
        self.zoom = None;
        let scale = self.scale();
        self.center[0] += f64::from(cols) * f64::from(self.cell[0]) / scale;
        self.center[1] += f64::from(rows) * f64::from(self.cell[1]) / scale;
        self.clamp_center();
    }

    pub(super) fn geometry(&self) -> (PixelRect, Rect, [u32; 2]) {
        let extent = self.extent();
        let scale = self.scale();
        let pixels: [u32; 2] = std::array::from_fn(|i| {
            (self.source[i] * scale).min(extent[i]).round().max(1.0) as u32
        });
        let visible = [0, 1].map(|i| (extent[i] / scale).min(self.source[i]));
        let length = visible.map(|n| n.round().max(1.0) as u32);
        let origin = [0, 1].map(|i| {
            ((self.center[i] - visible[i] * 0.5).round() as u32)
                .min(self.source[i] as u32 - length[i])
        });
        let cols = pixels[0].div_ceil(self.cell[0]) as u16;
        let rows = pixels[1].div_ceil(self.cell[1]) as u16;
        (
            PixelRect {
                x: origin[0],
                y: origin[1],
                width: length[0],
                height: length[1],
            },
            Rect::new(
                self.body.x + (self.body.width - cols) / 2,
                self.body.y + (self.body.height - rows) / 2,
                cols,
                rows,
            ),
            pixels,
        )
    }

    pub(super) fn mouse(&mut self, event: MouseEvent, now: Instant) -> bool {
        let pos = Position::new(event.column, event.row);
        match event.kind {
            MouseEventKind::Drag(MouseButton::Left) if let Some(old) = self.drag => {
                self.drag = Some(pos);
                if old != pos {
                    self.last_click = None;
                    self.pan(
                        i32::from(old.x) - i32::from(pos.x),
                        i32::from(old.y) - i32::from(pos.y),
                    );
                }
                return true;
            }
            MouseEventKind::Up(MouseButton::Left) => return self.drag.take().is_some(),
            _ if !self.body.contains(pos) => return false,
            MouseEventKind::Down(MouseButton::Left) if event.modifiers.is_empty() => {
                self.drag = Some(pos);
                if self.last_click.take().is_some_and(|(old, t)| {
                    old == pos && now.duration_since(t) < Duration::from_millis(400)
                }) {
                    self.zoom = None;
                    if self.fitting() {
                        self.set_scale(1.0, self.anchor(pos));
                    } else {
                        self.reset();
                    }
                } else {
                    self.last_click = Some((pos, now));
                }
            }
            MouseEventKind::ScrollLeft => self.pan(-3, 0),
            MouseEventKind::ScrollRight => self.pan(3, 0),
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = event.kind == MouseEventKind::ScrollDown;
                let stride = if down { 3 } else { -3 };
                if event.modifiers.contains(KeyModifiers::SHIFT) {
                    self.pan(stride, 0);
                } else if event.modifiers.contains(KeyModifiers::ALT) {
                    self.pan(0, stride);
                } else {
                    self.step(
                        if down { ZOOM_STEP.recip() } else { ZOOM_STEP },
                        self.anchor(pos),
                        now,
                    );
                }
            }
            _ => return false,
        }
        true
    }

    pub(super) fn key(&mut self, event: KeyEvent, now: Instant) -> bool {
        if self.body.is_empty() || !(event.modifiers - KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        match event.code {
            KeyCode::Char('+' | '=') => self.step(ZOOM_STEP, [0.0; 2], now),
            KeyCode::Char('-') => self.step(ZOOM_STEP.recip(), [0.0; 2], now),
            KeyCode::Char('0') => self.reset(),
            KeyCode::Char('1') => {
                self.zoom = None;
                self.set_scale(1.0, [0.0; 2]);
            }
            KeyCode::Char('a') => self.pan(-3, 0),
            KeyCode::Char('d') => self.pan(3, 0),
            KeyCode::Char('w') => self.pan(0, -3),
            KeyCode::Char('s') => self.pan(0, 3),
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn burst_zoom_keeps_pointer_on_source_pixel_and_fit_tracks_resize() {
        let mut view = Viewport::new(1920, 1200);
        let body = Rect::new(40, 2, 60, 20);
        view.layout(body, [10, 20], true);
        let (crop, _, pixels) = view.geometry();
        assert_eq!((crop.width, crop.height, pixels), (1920, 1200, [600, 375]));
        let point = Position::new(85, 12);
        let offset = view.anchor(point);
        let source_at = |v: &Viewport| [0, 1].map(|i| v.center[i] + offset[i] / v.scale());
        let start = source_at(&view);
        let now = Instant::now();
        for _ in 0..5 {
            assert!(view.mouse(mouse(MouseEventKind::ScrollUp, point.x, point.y), now));
        }
        assert!(view.advance(now + ZOOM_TIME / 2));
        assert!(!view.advance(now + ZOOM_TIME));
        assert!((view.scale() - view.fit() * ZOOM_STEP.powi(5)).abs() < 1e-10);
        assert!((source_at(&view)[0] - start[0]).abs() < 1e-8);
        view.key(KeyCode::Char('0').into(), now);
        view.layout(Rect::new(40, 2, 30, 20), [10, 20], true);
        assert_eq!(view.geometry().0, crop);
        assert_eq!(view.geometry().2[0], 300);
    }

    #[test]
    fn drag_capture_wheel_pan_and_native_scale_stay_inside_source() {
        let mut view = Viewport::new(1920, 1200);
        view.layout(Rect::new(40, 2, 60, 20), [10, 20], false);
        let now = Instant::now();
        view.key(KeyCode::Char('1').into(), now);
        let start = view.geometry().0;
        view.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 52, 15), now);
        view.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 54, 16), now);
        let dragged = view.geometry().0;
        assert_eq!((dragged.x, dragged.y), (start.x - 20, start.y - 20));
        view.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 0, 0), now);
        assert!(view.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 0, 0), now));
        let end = view.geometry().0;
        assert!(!view.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 1, 0), now));
        view.mouse(mouse(MouseEventKind::ScrollLeft, 80, 10), now);
        assert_eq!(view.geometry().0.x + 30, end.x);
        let mut vertical = mouse(MouseEventKind::ScrollDown, 80, 10);
        vertical.modifiers = KeyModifiers::ALT;
        for _ in 0..100 {
            view.mouse(vertical, now);
        }
        let bottom = view.geometry().0;
        assert_eq!(bottom.y + bottom.height, 1200);
        assert_eq!(view.scale(), 1.0);
    }
}

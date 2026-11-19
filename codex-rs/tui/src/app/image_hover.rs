//! Image peeks share the document loader and its backend checks. Hovering never
//! creates a tab, changes keyboard focus, or replaces the pinned view's camera.

use super::sidebar::SidebarPane;
use super::sidebar_document::Document;
use super::*;
use crate::custom_terminal::Frame;
use crate::motion::MotionMode;
use crate::pets::ImageProtocol;
use crate::sidebar_image::SidebarImage;
use crate::tui::LinkTooltip;
use codex_app_server_client::AppServerPath;
use codex_utils_path_uri::PathUri;
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use std::collections::VecDeque;
use uuid::Uuid;

const RECENT_IMAGES: usize = 3;
const REUSE_FOR: std::time::Duration = std::time::Duration::from_secs(30);

struct Entry {
    last_seen: Instant,
    id: Uuid,
    document: Document,
    job: tokio::task::JoinHandle<()>,
}

#[derive(Default)]
pub(super) struct ImageHover {
    cached: VecDeque<Entry>,
    active: Option<Uuid>,
    view: Option<SidebarImage>,
    painted: Option<Rect>,
}

impl ImageHover {
    fn recent(
        &mut self,
        backend: crate::backend_id::BackendId,
        source: ThreadId,
        path: &PathUri,
    ) -> Option<Entry> {
        while self
            .cached
            .front()
            .is_some_and(|entry| entry.last_seen.elapsed() >= REUSE_FOR)
        {
            if let Some(entry) = self.cached.pop_front() {
                entry.job.abort();
            }
        }
        let index = self.cached.iter().position(|entry| {
            let doc = &entry.document;
            doc.path == *path && doc.backend == backend && doc.source == source
        })?;
        self.cached.remove(index)
    }

    pub(super) fn take(
        &mut self,
        backend: crate::backend_id::BackendId,
        source: ThreadId,
        path: &PathUri,
    ) -> Option<(Uuid, Document)> {
        let entry = self.recent(backend, source, path)?;
        if entry.document.failed() {
            return None;
        }
        // Dropping the handle detaches the load. The same ID completes in the pinned tab.
        Some((entry.id, entry.document))
    }

    pub(super) fn document_mut(&mut self, id: Uuid) -> Option<&mut Document> {
        self.cached
            .iter_mut()
            .find(|e| e.id == id)
            .map(|e| &mut e.document)
    }

    fn select(&mut self, id: Option<Uuid>) -> bool {
        if self.active == id {
            return false;
        }
        self.active = id;
        self.view = None;
        self.painted = None;
        true
    }
}

impl Drop for ImageHover {
    fn drop(&mut self) {
        for entry in &self.cached {
            entry.job.abort();
        }
    }
}

impl App {
    pub(super) fn update_image_hover(
        &mut self,
        tui: &tui::Tui,
        server: &AppServerSession,
        event: &TuiEvent,
    ) {
        if matches!(event, TuiEvent::Draw) {
            return;
        }
        let target = match event {
            TuiEvent::Mouse(mouse)
                if mouse.kind == MouseEventKind::Moved
                    && mouse.modifiers.is_empty()
                    && self.preview_image_protocol(tui).is_some()
                    && self.document_preview_available(tui, server)
                    && !self
                        .sidebar()
                        .and_then(|p| p.image_hover.painted)
                        .is_some_and(|area| area.contains((mouse.column, mouse.row).into())) =>
            {
                self.image_at_pointer(tui, *mouse)
            }
            _ => None,
        };
        let Some((source, path)) = target else {
            let parent = self.sidebar_parent();
            if self
                .sidebars
                .get_mut(&parent)
                .is_some_and(|p| p.image_hover.select(None))
            {
                tui.frame_requester().schedule_frame();
            }
            return;
        };
        let Some(parent) = self.sidebar_parent() else {
            return;
        };
        let backend = server.backend_id();
        let pane = self.sidebar_mut();
        let pinned = pane.documents.iter().find_map(|(&id, doc)| {
            (doc.path == path && doc.backend == backend && doc.source == source).then_some(id)
        });
        let id = if let Some(id) = pinned {
            id
        } else if let Some(mut entry) = pane.image_hover.recent(backend, source, &path) {
            entry.last_seen = Instant::now();
            let id = entry.id;
            pane.image_hover.cached.push_back(entry);
            id
        } else {
            let id = Uuid::new_v4();
            let original = AppServerPath::from_app_server(path.inferred_native_path_string());
            let doc = Document::loading(source, path, backend, None);
            let protocol = self.preview_image_protocol(tui);
            let job = self.load_document(server, parent, id, &doc, original, protocol);
            let cached = &mut self.sidebar_mut().image_hover.cached;
            if cached.len() == RECENT_IMAGES
                && let Some(old) = cached.pop_front()
            {
                old.job.abort();
            }
            cached.push_back(Entry {
                id,
                document: doc,
                job,
                last_seen: Instant::now(),
            });
            id
        };
        if self.sidebar_mut().image_hover.select(Some(id)) {
            tui.frame_requester().schedule_frame();
        }
    }
}

impl SidebarPane {
    pub(super) fn paint_image_hover(
        &mut self,
        frame: &mut Frame<'_>,
        tooltip: &LinkTooltip,
        panel: Option<Rect>,
        protocol: Option<ImageProtocol>,
        motion: MotionMode,
        frames: &tui::FrameRequester,
    ) -> bool {
        let hover = &mut self.image_hover;
        let Some(id) = hover.active else { return false };
        let Some(doc) = self.documents.get(&id).or_else(|| {
            hover
                .cached
                .iter()
                .find(|e| e.id == id)
                .map(|e| &e.document)
        }) else {
            return false;
        };
        if doc.path.inferred_native_path_string() != tooltip.destination {
            return false;
        }
        if hover.view.is_none() {
            hover.view = doc.peek_image();
        }
        if hover.view.is_none() && !doc.loading_image() {
            return false;
        }
        let Some(area) = preview_area(frame.area(), panel, tooltip.point) else {
            return false;
        };
        hover.painted = Some(area);
        frame
            .image_placements
            .retain(|img| !area.intersects(img.area));
        Clear.render(area, frame.buffer);
        let path = Paragraph::new(tooltip.destination.as_str()).wrap(Wrap { trim: false });
        let path_rows = (path.line_count(area.width) as u16).clamp(1, (area.height / 3).max(1));
        let canvas = Rect {
            height: area.height - path_rows - 1,
            ..area
        };
        if let Some(image) = &mut hover.view {
            frame.image_placements.extend(image.paint(
                canvas,
                frame.buffer,
                protocol,
                None,
                motion,
                frames,
            ));
            // Peeks are passive. The pinned viewer owns gesture and keyboard hints.
            Clear.render(
                Rect::new(area.x, canvas.bottom() - 1, area.width, 1),
                frame.buffer,
            );
        } else {
            Line::from("Opening image…")
                .dim()
                .render(canvas, frame.buffer);
        }
        path.render(
            Rect::new(area.x, canvas.bottom(), area.width, path_rows),
            frame.buffer,
        );
        Line::from("Right-click to copy").dim().render(
            Rect::new(area.x, area.bottom() - 1, area.width, 1),
            frame.buffer,
        );
        true
    }
}

fn preview_area(
    screen: Rect,
    panel: Option<Rect>,
    pointer: ratatui::layout::Position,
) -> Option<Rect> {
    // Keep the hovered hit target uncovered so moving along a list of images
    // doesn't make the layout jump or turn the preview into its own trigger.
    let area = if let Some(panel) = panel {
        if pointer.y == panel.y || !panel.contains(pointer) {
            super::side_pane::tab_and_body(panel)[1]
        } else {
            Rect::new(
                screen.x,
                screen.y + 1,
                panel.x.saturating_sub(screen.x + 1),
                screen.height.saturating_sub(2),
            )
        }
    } else {
        let right = screen.right().saturating_sub(pointer.x.saturating_add(1));
        let left = pointer.x.saturating_sub(screen.x);
        let width = left.max(right).min((screen.width / 2).max(1));
        let x = if right >= left {
            screen.right() - width
        } else {
            screen.x
        };
        Rect::new(x, screen.y + 1, width, screen.height.saturating_sub(2))
    };
    (area.width >= 8 && area.height >= 6 && !area.contains(pointer)).then_some(area)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Position;

    #[tokio::test]
    async fn pinning_reuses_only_the_matching_workspaces_inflight_load() {
        let source = ThreadId::new();
        let backend = crate::backend_id::BackendId::new();
        let path = PathUri::parse("file:///preview.png").unwrap();
        let id = Uuid::new_v4();
        let (send, receive) = tokio::sync::oneshot::channel();
        let (done, completed) = tokio::sync::oneshot::channel();
        let mut hover = ImageHover::default();
        hover.cached.push_back(Entry {
            id,
            last_seen: Instant::now(),
            document: Document::loading(source, path.clone(), backend, None),
            job: tokio::spawn(async move {
                if receive.await.is_ok() {
                    let _ = done.send(());
                }
            }),
        });
        assert!(hover.take(backend, ThreadId::new(), &path).is_none());
        assert!(
            hover
                .take(crate::backend_id::BackendId::new(), source, &path)
                .is_none()
        );
        let (promoted, doc) = hover.take(backend, source, &path).unwrap();
        assert_eq!(promoted, id);
        assert!(doc.loading_image());
        drop(hover);
        send.send(()).unwrap();
        completed.await.unwrap();
    }

    #[test]
    fn previews_leave_links_tabs_and_file_rows_under_the_pointer() {
        let screen = Rect::new(0, 0, 120, 40);
        let panel = Rect::new(75, 0, 45, 40);
        for point in [Position::new(83, 0), Position::new(2, 8)] {
            assert_eq!(
                preview_area(screen, Some(panel), point),
                Some(Rect::new(75, 1, 45, 39))
            );
        }
        let list = Position::new(85, 12);
        let preview = preview_area(screen, Some(panel), list).unwrap();
        assert!(!preview.intersects(panel));
        for x in 0..screen.width {
            let point = Position::new(x, 10);
            assert!(!preview_area(screen, None, point).unwrap().contains(point));
        }
        assert_eq!(preview_area(Rect::new(0, 0, 5, 2), None, list), None);
    }
}

//! Match pointer and foreground feedback to the last painted click targets.
//! Recheck on repaint so scrolling and live output cannot leave hover on stale content.
//! Keep passive link drags with their original view.

use codex_protocol::ThreadId;
use codex_utils_path_uri::LegacyAppPathString;
use codex_utils_path_uri::PathUri;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton::Left;
use crossterm::event::MouseButton::Right;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind::Down;
use crossterm::event::MouseEventKind::Drag;
use crossterm::event::MouseEventKind::Moved;
use crossterm::event::MouseEventKind::ScrollDown;
use crossterm::event::MouseEventKind::ScrollUp;
use crossterm::event::MouseEventKind::Up;
use ratatui::buffer::Buffer;
use ratatui::buffer::CellDiffOption;
use ratatui::buffer::CellWidth;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::App;
use super::side_companion::SideCompanion;
use super::side_focus::SidebarTarget;
use super::sidebar::SidebarContent;
use crate::app_event::AppEvent;
use crate::custom_terminal::osc8_hyperlink_parts;
use crate::pager_overlay::Overlay;
use crate::sidebar_tab_strip::SidebarTab;
use crate::sidebar_tab_strip::TabTarget;
use crate::terminal_hyperlinks::ReaderLink;
use crate::transcript_view::ViewAction;
use crate::tui::LinkTooltip;
use crate::tui::PointerShape;
use crate::tui::Tui;
use crate::tui::TuiEvent;

enum Destination {
    Web(String),
    File(PathUri),
}

impl Destination {
    fn resolve(link: ReaderLink, base: &PathUri) -> Option<Self> {
        match link {
            ReaderLink::Document(link) => {
                let path = LegacyAppPathString::from_string(link.path)
                    .resolve_against(base, None)
                    .ok()?;
                (!path.to_url().has_host()).then_some(Self::File(path))
            }
            ReaderLink::External(url) if url.starts_with("file:") => {
                PathUri::parse(&url).ok().map(Self::File)
            }
            ReaderLink::External(url) => Some(Self::Web(url)),
            ReaderLink::Agent(_) => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Web(url) => url.clone(),
            Self::File(path) => path.inferred_native_path_string(),
        }
    }
}

impl App {
    pub(super) fn observe_pointer(&self, tui: &mut Tui, event: &TuiEvent) {
        let previous = tui.link_hover.mouse;
        let previous_position = tui.link_hover.position();
        tui.link_hover.observe(event);
        let current = tui.link_hover.mouse;
        if previous == current {
            return;
        }
        let current_position = tui.link_hover.position();
        let buffer = tui.terminal.previous_buffer();
        if self.hovered_sidebar_target(previous_position)
            != self.hovered_sidebar_target(current_position)
            || [previous_position, current_position]
                .into_iter()
                .flatten()
                .any(|point| self.transcript_view.has_document_link_at(point))
            || self.hover_target_area(self.hover_pointer(tui, previous), buffer)
                != self.hover_target_area(self.hover_pointer(tui, current), buffer)
        {
            tui.frame_requester().schedule_frame();
        }
    }

    pub(super) fn hover_pointer(&self, tui: &Tui, mouse: Option<MouseEvent>) -> Option<MouseEvent> {
        mouse.filter(|mouse| {
            tui.is_owned_screen()
                && self.overlay.is_none()
                && !tui.shortcut_hud_contains(*mouse)
                && self.pane_resize.is_none()
                && self.sidebar().is_none_or(|pane| pane.close.is_none())
        })
    }

    pub(super) fn hover_target_area(
        &self,
        mouse: Option<MouseEvent>,
        buf: &Buffer,
    ) -> Option<Rect> {
        let mouse = mouse?;
        let point = Position::new(mouse.column, mouse.row);
        if !buf.area.contains(point) {
            return None;
        }
        let passive = self
            .conversation_frame
            .side_pane()
            .is_some_and(|panes| panes.passive.contains(point));
        let ordinary_input = self.chat_widget.no_modal_or_popup_active();
        if !passive {
            // Inline approvals allow disclosure in the transcript above the prompt.
            if mouse.modifiers.is_empty()
                && (ordinary_input
                    || (self.chat_widget.has_active_modal()
                        && self.chat_widget.centered_dialog().is_none()))
                && let Some(area) = self.transcript_view.disclosure_hover_area(point)
            {
                return Some(area);
            }
            if let Some(area) = self.chat_widget.warning_notice_at(point) {
                return Some(area);
            }
        }
        if !ordinary_input {
            return None;
        }
        if self.chat_widget.status_line_area().contains(point)
            || ((passive || !self.transcript_view.is_select_mode())
                && self.transcript_link_at(mouse).is_some())
        {
            painted_link_area(buf, point)
        } else {
            None
        }
    }

    pub(super) fn transcript_link_at(&self, mouse: MouseEvent) -> Option<ReaderLink> {
        if !mouse.modifiers.is_empty()
            && !mouse
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        {
            return None;
        }
        if let Some(area) = self.conversation_frame.panel_area()
            && area.contains((mouse.column, mouse.row).into())
            && let Some(pane) = self.sidebar()
        {
            return pane.visible_link_at(mouse.column, mouse.row);
        }
        let view = if let Some(panes) = self.conversation_frame.side_pane()
            && panes.passive.contains((mouse.column, mouse.row).into())
        {
            let passive = &self.side_companions.get(&panes.passive_thread)?.view;
            if passive.is_select_mode() {
                return None;
            }
            passive
        } else {
            &self.transcript_view
        };
        view.reader_link_at(mouse.column, mouse.row)
    }

    fn link_input_available(&self, tui: &Tui, mouse: MouseEvent) -> bool {
        if tui.shortcut_hud_contains(mouse) {
            return false;
        }
        match &self.overlay {
            Some(Overlay::Transcript(_)) => true,
            Some(_) => false,
            None => {
                tui.is_owned_screen()
                    && (self.chat_widget.no_modal_or_popup_active()
                        || self.focused_sidebar_chooser(tui))
                    && self.sidebar().is_none_or(|pane| pane.close.is_none())
                    && self.divider_drag.is_none()
                    && self.pane_resize.is_none()
            }
        }
    }

    fn pointer_destination(&self, mouse: MouseEvent) -> Option<Destination> {
        if let Some(Overlay::Transcript(overlay)) = &self.overlay {
            return Destination::resolve(
                overlay.pointer_destination(mouse)?,
                &PathUri::from_abs_path(&self.chat_widget.config_ref().cwd),
            );
        }
        let point = (mouse.column, mouse.row).into();
        let frame = &self.conversation_frame;
        if let Some(target) = self.hovered_sidebar_target(Some(point)) {
            let pane = self.sidebar()?;
            if frame.sidebar_input != Some(pane.input_state()) {
                return None;
            }
            return match target {
                SidebarTarget::Row(index) => {
                    let file = pane.chat_tabs().file(index)?.clone();
                    Destination::resolve(
                        ReaderLink::Document(file),
                        pane.files.as_ref()?.cwd.as_ref()?,
                    )
                }
                SidebarTarget::Tab(
                    TabTarget::Tab(SidebarTab::Document(id))
                    | TabTarget::Overflow {
                        tab: SidebarTab::Document(id),
                        ..
                    },
                ) => Some(Destination::File(pane.documents.get(&id)?.path.clone())),
                _ => None,
            };
        }
        if frame.panel_area().is_some_and(|area| area.contains(point)) {
            let pane = self.sidebar()?;
            let SidebarContent::Document(id) = pane.visible_content()? else {
                return None;
            };
            let doc = pane.documents.get(&id)?;
            return Destination::resolve(
                doc.link_at(mouse.column, mouse.row)?,
                &doc.path.parent()?,
            );
        }
        let (view, cwd) = if let Some(panes) = frame.side_pane()
            && panes.passive.contains(point)
        {
            let companion = self.side_companions.get(&panes.passive_thread)?;
            (
                &companion.view,
                PathUri::from_host_native_path(&companion.display.cwd).ok()?,
            )
        } else {
            (
                &self.transcript_view,
                PathUri::from_abs_path(&self.chat_widget.config_ref().cwd),
            )
        };
        if view.is_select_mode() {
            return None;
        }
        Destination::resolve(view.pointer_destination(mouse.column, mouse.row)?, &cwd)
    }

    pub(super) fn handle_link_gesture(&mut self, tui: &mut Tui, mouse: MouseEvent) -> bool {
        let copy = mouse.kind == Down(Right) && mouse.modifiers.is_empty();
        let open = mouse.kind == Down(Left)
            && mouse
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        if !(copy || open) || !self.link_input_available(tui, mouse) {
            return false;
        }
        let Some(destination) = self.pointer_destination(mouse) else {
            return false;
        };
        let tx = &self.app_event_tx;
        if copy {
            tx.send(AppEvent::CopySelection {
                text: destination.text().into(),
                label: "link destination".into(),
                format: crate::clipboard_copy::CopyFormat::PlainText,
            });
        } else {
            match destination {
                Destination::Web(url) => self.open_url_in_browser(url),
                Destination::File(path) => {
                    if crate::uses_remote_workspace_or_environment(
                        &self.app_server_target,
                        &self.environment_manager,
                    ) {
                        self.chat_widget.add_error_message(
                            "This file belongs to a remote workspace. Right-click to copy its path.".into(),
                        );
                    } else {
                        tx.send(AppEvent::OpenExternalFile(path));
                    }
                }
            }
        }
        true
    }

    pub(super) fn handle_passive_transcript_event(
        &mut self,
        tui: &mut Tui,
        id: ThreadId,
        event: &TuiEvent,
    ) -> bool {
        let Some(companion) = self.side_companions.get_mut(&id) else {
            return false;
        };
        let view = &mut companion.view;
        view.agent_links = self.transcript_view.agent_links;
        view.mouse_scroll_speed = self.transcript_view.mouse_scroll_speed;
        view.copy_on_select = self.transcript_view.copy_on_select;
        view.copy_on_select_clear_selection = self.transcript_view.copy_on_select_clear_selection;
        view.primary_selection = self.transcript_view.primary_selection;
        let action = match event {
            TuiEvent::Mouse(mouse) => view.handle_mouse(*mouse, &companion.cells),
            TuiEvent::Key(key) if crate::text_selection::is_copy_key(*key) => view
                .handle_selection_key(*key, &companion.cells)
                .filter(|action| matches!(action, ViewAction::Copy(_))),
            _ => None,
        };
        let Some(action) = action else {
            return false;
        };
        if let Some((count, result)) = view.copy_action(
            tui,
            &companion.cells,
            &action,
            crate::clipboard_copy::CopyFormat::Markdown,
        ) {
            self.transcript_view.show_copy_feedback(&result, count);
        } else if let ViewAction::OpenLink(url) = action {
            self.open_url_in_browser(url);
        } else if let ViewAction::OpenAgent(agent) = action
            && let Some(backend) = view.agent_links
        {
            self.app_event_tx.send(AppEvent::OpenSidebarAgent {
                backend,
                source: id,
                agent,
            });
        }
        tui.frame_requester().schedule_frame();
        true
    }

    pub(super) fn continue_passive_link_drag(
        &mut self,
        tui: &mut Tui,
        event: &TuiEvent,
    ) -> color_eyre::eyre::Result<bool> {
        let Some((id, area)) = self.passive_link_drag else {
            return Ok(false);
        };
        if self.conversation_focus_available(tui) && !crate::tui::mouse_capture_disabled() {
            match event {
                TuiEvent::Draw => {
                    if let Some(companion) = self.side_companions.get_mut(&id)
                        && companion.view.tick_selection(&companion.cells)
                    {
                        tui.frame_requester()
                            .schedule_frame_in(crate::tui::TARGET_FRAME_INTERVAL);
                    }
                    return Ok(false);
                }
                TuiEvent::Mouse(mouse) if mouse.kind == Moved => return Ok(false),
                TuiEvent::Mouse(mouse)
                    if matches!(mouse.kind, Drag(Left) | Up(Left) | ScrollUp | ScrollDown) =>
                {
                    if !matches!(mouse.kind, ScrollUp | ScrollDown)
                        || !self
                            .side_companions
                            .get(&id)
                            .is_some_and(SideCompanion::can_scroll_without_refresh)
                    {
                        self.refresh_owned_transcript(tui)?;
                    }
                    if self
                        .conversation_frame
                        .side_pane()
                        .is_some_and(|panes| panes.passive_thread == id && panes.passive == area)
                    {
                        self.handle_passive_transcript_event(tui, id, event);
                        if mouse.kind == Up(Left) {
                            self.passive_link_drag = None;
                        }
                        return Ok(true);
                    }
                }
                _ => {}
            }
        }
        self.passive_link_drag = None;
        if let Some(companion) = self.side_companions.get_mut(&id) {
            companion.view.end_drag();
        } else if self.current_displayed_thread_id() == Some(id) {
            self.transcript_view.end_drag();
        }
        Ok(false)
    }

    pub(super) fn refresh_link_hover(&self, tui: &mut Tui) -> std::io::Result<()> {
        let viewport = tui.terminal.last_known_screen_size.into();
        let mut sidebar_divider = false;
        let tooltip = tui
            .link_hover
            .mouse
            .filter(|mouse| {
                mouse.kind == Moved
                    && self.link_input_available(tui, *mouse)
                    && (mouse.modifiers.is_empty()
                        || mouse
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER))
            })
            .and_then(|mouse| {
                Some(LinkTooltip {
                    point: Position::new(mouse.column, mouse.row),
                    destination: self.pointer_destination(mouse)?.text(),
                })
            });
        if tooltip != tui.link_hover.tooltip {
            tui.link_hover.tooltip = tooltip;
            tui.frame_requester().schedule_frame();
        }
        let shape = if tui
            .link_hover
            .mouse
            .is_some_and(|mouse| tui.shortcut_hud_contains(mouse))
            || !tui.is_owned_screen()
            || self.overlay.is_some()
        {
            PointerShape::Default
        } else if let Some(drag) = self.divider_drag {
            sidebar_divider = !drag.is_center();
            PointerShape::Resize
        } else if self.conversation_focus_available(tui)
            && tui.link_hover.mouse.is_some_and(|mouse| {
                self.conversation_frame
                    .divider_area()
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
                    && self.divider_is_settled(tui, viewport)
            })
        {
            sidebar_divider = true;
            PointerShape::Resize
        } else if self
            .hover_pointer(tui, tui.link_hover.mouse)
            .is_some_and(|mouse| {
                self.center_divider_area(viewport)
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
            })
        {
            PointerShape::Resize
        } else if tui.link_hover.tooltip.is_some() {
            PointerShape::Link
        } else {
            PointerShape::Default
        };
        if sidebar_divider != tui.link_hover.sidebar_divider
            || (shape == PointerShape::Resize) != (tui.link_hover.shape == PointerShape::Resize)
        {
            tui.link_hover.sidebar_divider = sidebar_divider;
            tui.frame_requester().schedule_frame();
        }
        tui.link_hover.shape = shape;
        tui.set_pointer_shape(shape)
    }
}

/// Read a single visible link fragment from the OSC 8 markers attached by the renderer.
fn painted_link_area(buf: &Buffer, point: Position) -> Option<Rect> {
    let mut column = buf.area.left();
    let mut start = column;
    let mut destination = None;
    let right = buf.area.right();
    while column < right {
        let cell = &buf[(column, point.y)];
        let link = if cell.diff_option == CellDiffOption::Skip {
            destination
        } else {
            osc8_hyperlink_parts(cell.symbol()).map(|(url, _)| url)
        };
        if link != destination {
            if column > point.x {
                break;
            }
            start = column;
            destination = link;
        }
        // A wide glyph carries the link on its first column only.
        column = column.saturating_add(cell.cell_width().max(1)).min(right);
    }
    destination.map(|_| Rect::new(start, point.y, column - start, /*height*/ 1))
}

pub(super) fn paint_hover_foreground(area: Rect, buf: &mut Buffer) {
    for point in area.intersection(buf.area).positions() {
        let cell = &mut buf[point];
        if !cell.modifier.contains(Modifier::REVERSED)
            && let Some(foreground) = crate::style::hover_foreground(cell.fg, cell.bg)
        {
            cell.set_fg(foreground);
        }
    }
}

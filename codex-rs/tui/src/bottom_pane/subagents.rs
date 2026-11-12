//! Live subagent roster and manager. Selection is anchored to thread identity across updates.
//! Mouse hit areas come from the last paint; hidden surfaces never receive input.

use super::BottomPaneView;
use super::CancellationEvent;
use super::bottom_pane_view::ViewCompletion;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::keymap::KeymapContext;
use crate::keymap::KeymapContextSet;
use crate::keymap::ListAction;
use crate::keymap::ListKeymap;
use crate::render::renderable::Renderable;
use crate::style::StatusTone;
use crate::style::status_style;
use crate::text_formatting::truncate_text;
use codex_protocol::ThreadId;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use std::cell::RefCell;
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

pub(crate) const VIEW_ID: &str = "agent-picker";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SubagentRow {
    pub id: ThreadId,
    pub name: String,
    pub activity: String,
    pub status: &'static str,
    pub running: bool,
    pub primary: bool,
    pub current: bool,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub tokens: Option<i64>,
}

impl SubagentRow {
    fn can_stop(&self) -> bool {
        self.running && !self.primary && self.status != "stopping"
    }
    fn metrics(&self) -> String {
        let elapsed = self.started_at.map(|start| {
            let end = self
                .finished_at
                .unwrap_or_else(|| chrono::Utc::now().timestamp());
            let seconds = end.saturating_sub(start).max(0);
            if seconds >= 3600 {
                format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60)
            } else {
                format!("{}m {:02}s", seconds / 60, seconds % 60)
            }
        });
        let tokens = self.tokens.map(|tokens| {
            if tokens >= 1000 {
                format!("{:.1}k tokens", tokens as f64 / 1000.0)
            } else {
                format!("{tokens} tokens")
            }
        });
        [elapsed, tokens]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
    }

    fn render(&self, area: Rect, buf: &mut Buffer, selected: bool) {
        let style = if selected {
            super::picker_style::selection_style()
        } else {
            Style::default()
        };
        buf.set_style(area, style);
        let dot = if self.current {
            "●".cyan()
        } else if self.running {
            "●".green()
        } else {
            "○".dim()
        };
        let name_width = (area.width as usize / 3).clamp(8, 32);
        let name = truncate_text(&self.name, name_width);
        let name_padding = " ".repeat(name_width.saturating_sub(name.width()));
        let metrics = self.metrics();
        let status = format!("  {}", self.status);
        let status = match self.status {
            "needs approval" | "interrupted" | "stopping" => {
                Span::styled(status, status_style(StatusTone::Attention))
            }
            "failed" => Span::styled(status, status_style(StatusTone::Failure)),
            _ => status.dim(),
        };
        let mut spans = vec![
            if selected { "›".cyan() } else { " ".into() },
            dot,
            " ".into(),
            name.bold(),
            name_padding.into(),
            status,
        ];
        let used: usize = spans.iter().map(Span::width).sum();
        let remaining = (area.width as usize).saturating_sub(used + 2);
        if remaining > metrics.width() + 12 {
            let activity = truncate_text(
                &self.activity,
                remaining.saturating_sub(metrics.width() + 2),
            );
            spans.push(format!("  {activity}").dim());
            let used: usize = spans.iter().map(Span::width).sum();
            spans.push(
                format!(
                    "{}{metrics}",
                    " ".repeat((area.width as usize).saturating_sub(used + metrics.width()))
                )
                .dim(),
            );
        } else if remaining > 4 {
            spans.push(format!("  {}", truncate_text(&self.activity, remaining)).dim());
        }
        Paragraph::new(Line::from(spans))
            .style(style)
            .render(area, buf);
    }
}

#[derive(Clone, Copy)]
enum Hit {
    Row(ThreadId),
    Open,
    Stop,
    Close,
    Toggle,
    Search,
}

#[derive(Default)]
pub(crate) struct SubagentRoster {
    pub rows: Vec<SubagentRow>,
    collapsed: bool,
    hits: RefCell<Vec<(Rect, Hit)>>,
}

impl SubagentRoster {
    pub fn toggle(&mut self) {
        self.collapsed = !self.collapsed;
        self.hits.borrow_mut().clear();
    }
    pub fn set_rows(&mut self, rows: Vec<SubagentRow>) -> bool {
        if self.rows == rows {
            return false;
        }
        if self
            .rows
            .iter()
            .map(|row| row.id)
            .ne(rows.iter().map(|row| row.id))
        {
            self.hits.borrow_mut().clear();
        }
        self.rows = rows;
        true
    }
    pub fn visible(&self) -> bool {
        self.rows.iter().any(|row| !row.primary)
    }

    pub fn running(&self) -> bool {
        self.rows.iter().any(|row| row.running)
    }

    pub fn mouse(&mut self, event: MouseEvent, tx: &AppEventSender) -> bool {
        if event.kind != MouseEventKind::Down(MouseButton::Left) || !event.modifiers.is_empty() {
            return false;
        }
        let hit = self
            .hits
            .borrow()
            .iter()
            .find(|(rect, _)| rect.contains((event.column, event.row).into()))
            .map(|(_, hit)| *hit);
        match hit {
            Some(Hit::Toggle) => self.toggle(),
            Some(Hit::Row(id)) => tx.send(AppEvent::SelectAgentThread(id)),
            Some(_) => tx.send(AppEvent::OpenAgentPicker),
            None => return false,
        }
        true
    }
}

impl Renderable for SubagentRoster {
    fn desired_height(&self, _width: u16) -> u16 {
        if !self.visible() {
            0
        } else if self.collapsed {
            1
        } else {
            1 + self.rows.len().min(5) as u16
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let mut hits = self.hits.borrow_mut();
        hits.clear();
        if !self.visible() || area.is_empty() {
            return;
        }
        let count = self.rows.iter().filter(|row| !row.primary).count();
        let running = self
            .rows
            .iter()
            .filter(|row| !row.primary && row.running)
            .count();
        let header = Rect::new(area.x, area.y, area.width, 1);
        let toggle_width = area.width.min(12);
        let toggle = Rect::new(area.right() - toggle_width, area.y, toggle_width, 1);
        let title = Rect::new(area.x, area.y, area.width.saturating_sub(toggle_width), 1);
        let noun = if count == 1 { "subagent" } else { "subagents" };
        Paragraph::new(format!(" {count} {noun} · {running} running · F6 manage").cyan())
            .render(title, buf);
        Paragraph::new(if self.collapsed {
            "[⇧F6 show]"
        } else {
            "[⇧F6 hide]"
        })
        .render(toggle, buf);
        hits.push((toggle, Hit::Toggle));
        hits.push((header, Hit::Open));
        if !self.collapsed {
            let mut rows: Vec<_> = self.rows.iter().collect();
            rows.sort_by_key(|row| {
                (
                    !row.primary,
                    row.status != "needs approval",
                    !row.running,
                    !row.current,
                )
            });
            let limit = if rows.len() > 5 { 4 } else { 5 };
            for (offset, row) in rows.iter().take(limit).enumerate() {
                let y = area.y + 1 + offset as u16;
                if y >= area.bottom() {
                    break;
                }
                let rect = Rect::new(area.x, y, area.width, 1);
                row.render(rect, buf, false);
                hits.push((rect, Hit::Row(row.id)));
            }
            if rows.len() > 5 && area.height > 5 {
                let rect = Rect::new(area.x, area.y + 5, area.width, 1);
                Paragraph::new(format!("   +{} more · open manager", rows.len() - limit).dim())
                    .render(rect, buf);
                hits.push((rect, Hit::Open));
            }
        }
    }
}

pub(crate) struct SubagentManager {
    rows: Vec<SubagentRow>,
    selected: Option<ThreadId>,
    query: String,
    searching: bool,
    completion: Option<ViewCompletion>,
    tx: AppEventSender,
    keymap: ListKeymap,
    hits: RefCell<Vec<(Rect, Hit)>>,
    area: std::cell::Cell<Rect>,
}

impl SubagentManager {
    pub fn new(rows: Vec<SubagentRow>, tx: AppEventSender, keymap: ListKeymap) -> Self {
        let selected = rows
            .iter()
            .find(|r| r.status == "needs approval")
            .or_else(|| rows.iter().find(|r| r.current))
            .or_else(|| rows.first())
            .map(|r| r.id);
        Self {
            rows,
            selected,
            query: String::new(),
            searching: false,
            completion: None,
            tx,
            keymap,
            hits: RefCell::default(),
            area: std::cell::Cell::default(),
        }
    }

    fn filtered(&self) -> Vec<&SubagentRow> {
        let query = self.query.to_lowercase();
        self.rows
            .iter()
            .filter(|r| {
                format!("{} {} {} {}", r.name, r.id, r.status, r.activity)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect()
    }

    fn reconcile(&mut self) {
        let previous = self.selected;
        let rows = self.filtered();
        if !rows.iter().any(|row| Some(row.id) == self.selected) {
            self.selected = rows.first().map(|row| row.id);
        }
        if previous != self.selected || !self.query.is_empty() {
            self.hits.borrow_mut().clear();
        }
    }

    fn step(&mut self, delta: isize) {
        let rows = self.filtered();
        let index = rows
            .iter()
            .position(|row| Some(row.id) == self.selected)
            .unwrap_or(0);
        let next = index
            .saturating_add_signed(delta)
            .min(rows.len().saturating_sub(1));
        self.selected = rows.get(next).map(|row| row.id);
        self.hits.borrow_mut().clear();
    }

    fn page_size(&self) -> isize {
        let area = self.area.get();
        area.height
            .saturating_sub(if area.width < 22 { 8 } else { 6 })
            .clamp(1, 10) as isize
    }

    fn act(&mut self, hit: Hit) {
        match hit {
            Hit::Row(id) => {
                self.selected = Some(id);
                self.hits.borrow_mut().clear();
            }
            Hit::Open => {
                if let Some(id) = self.selected {
                    self.tx.send(AppEvent::SelectAgentThread(id));
                    self.completion = Some(ViewCompletion::Accepted);
                }
            }
            Hit::Stop => {
                if let Some(row) = self
                    .rows
                    .iter()
                    .find(|r| Some(r.id) == self.selected && r.can_stop())
                {
                    self.tx.send(AppEvent::StopSubagent(row.id));
                }
            }
            Hit::Close => self.completion = Some(ViewCompletion::Cancelled),
            Hit::Search => self.searching = true,
            Hit::Toggle => {}
        }
    }
}

impl BottomPaneView for SubagentManager {
    fn keymap_contexts(&self) -> KeymapContextSet {
        KeymapContextSet::new(KeymapContext::List)
    }
    fn view_id(&self) -> Option<&'static str> {
        Some(VIEW_ID)
    }
    fn selected_index(&self) -> Option<usize> {
        self.rows
            .iter()
            .position(|row| Some(row.id) == self.selected)
    }
    fn update_subagents(&mut self, rows: &[SubagentRow]) -> bool {
        if self
            .rows
            .iter()
            .map(|row| row.id)
            .ne(rows.iter().map(|row| row.id))
        {
            self.hits.borrow_mut().clear();
        }
        self.rows = rows.to_vec();
        self.reconcile();
        true
    }
    fn handle_key_event(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            self.act(Hit::Close);
            return;
        }
        if (!self.searching || !crate::key_hint::is_plain_text_key_event(key))
            && let Some(action) = self.keymap.action_for(key)
        {
            match action {
                ListAction::MoveDown => self.step(1),
                ListAction::MoveUp => self.step(-1),
                ListAction::PageDown => self.step(self.page_size()),
                ListAction::PageUp => self.step(-self.page_size()),
                ListAction::JumpTop => self.step(isize::MIN),
                ListAction::JumpBottom => self.step(isize::MAX),
                ListAction::Accept => self.act(Hit::Open),
                ListAction::Cancel if self.searching || !self.query.is_empty() => {
                    self.searching = false;
                    self.query.clear();
                    self.reconcile();
                }
                ListAction::Cancel => self.act(Hit::Close),
                ListAction::MoveLeft | ListAction::MoveRight => {}
            }
            return;
        }
        if !key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return;
        }
        match key.code {
            KeyCode::Esc if self.searching || !self.query.is_empty() => {
                self.searching = false;
                self.query.clear();
                self.reconcile();
            }
            KeyCode::Esc => self.act(Hit::Close),
            KeyCode::Char('/') if !self.searching => self.searching = true,
            KeyCode::Char('x') if !self.searching => self.act(Hit::Stop),
            KeyCode::Char('f') if !self.searching => self.act(Hit::Open),
            KeyCode::Char(c) => {
                self.searching = true;
                self.query.push(c);
                self.reconcile();
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.reconcile();
            }
            _ => {}
        }
    }
    fn handle_mouse(&mut self, event: MouseEvent) -> bool {
        if !self.area.get().contains((event.column, event.row).into()) {
            return false;
        }
        match event.kind {
            MouseEventKind::ScrollDown => self.step(1),
            MouseEventKind::ScrollUp => self.step(-1),
            MouseEventKind::Down(MouseButton::Left) if event.modifiers.is_empty() => {
                let hit = self
                    .hits
                    .borrow()
                    .iter()
                    .find(|(rect, _)| rect.contains((event.column, event.row).into()))
                    .map(|(_, hit)| *hit);
                if let Some(hit) = hit {
                    self.act(hit);
                }
            }
            _ => {}
        }
        true
    }
    fn handle_paste(&mut self, pasted: String) -> bool {
        self.searching = true;
        self.query
            .extend(pasted.chars().filter(|c| !c.is_control()));
        self.reconcile();
        true
    }
    fn on_ctrl_c(&mut self) -> CancellationEvent {
        self.act(Hit::Close);
        CancellationEvent::Handled
    }
    fn prefer_esc_to_handle_key_event(&self) -> bool {
        true
    }
    fn is_complete(&self) -> bool {
        self.completion.is_some()
    }
    fn completion(&self) -> Option<ViewCompletion> {
        self.completion
    }
    fn next_frame_delay(&self) -> Option<Duration> {
        self.rows
            .iter()
            .any(|row| row.running)
            .then_some(Duration::from_secs(1))
    }
}

impl Renderable for SubagentManager {
    fn desired_height(&self, width: u16) -> u16 {
        self.filtered().len().clamp(1, 10) as u16 + if width < 22 { 8 } else { 6 }
    }
    fn render(&self, area: Rect, buf: &mut Buffer) {
        self.area.set(area);
        let mut hits = self.hits.borrow_mut();
        hits.clear();
        if area.is_empty() {
            return;
        }
        let rows = self.filtered();
        let index = rows
            .iter()
            .position(|row| Some(row.id) == self.selected)
            .unwrap_or(0);
        let footer_height = if area.width < 22 { 3 } else { 1 };
        let count = area.height.saturating_sub(5 + footer_height).max(1) as usize;
        let start = index.saturating_sub(count - 1);
        let title = format!(
            " Subagents · {} running · {}–{} of {}",
            self.rows.iter().filter(|r| !r.primary && r.running).count(),
            if rows.is_empty() { 0 } else { start + 1 },
            (start + count).min(rows.len()),
            rows.len()
        );
        Paragraph::new(title.bold()).render(Rect::new(area.x, area.y, area.width, 1), buf);
        if area.height > 1 {
            let rect = Rect::new(area.x, area.y + 1, area.width, 1);
            Paragraph::new(
                format!(
                    " / Search: {}{}",
                    self.query,
                    if self.searching { "▏" } else { "" }
                )
                .dim(),
            )
            .render(rect, buf);
            hits.push((rect, Hit::Search));
        }
        for (offset, row) in rows.iter().skip(start).take(count).enumerate() {
            let y = area.y + 2 + offset as u16;
            if y >= area.bottom() {
                break;
            }
            let rect = Rect::new(area.x, y, area.width, 1);
            row.render(rect, buf, Some(row.id) == self.selected);
            hits.push((rect, Hit::Row(row.id)));
        }
        if rows.is_empty() && area.height > 2 {
            Paragraph::new(" No matching agents.".dim())
                .render(Rect::new(area.x, area.y + 2, area.width, 1), buf);
        }
        if area.height >= 5 + footer_height
            && let Some(row) = rows.iter().find(|row| Some(row.id) == self.selected)
        {
            let mut detail = format!(" {} · {}", row.name, row.id);
            let metrics = row.metrics();
            if !metrics.is_empty() {
                detail.push_str(" · ");
                detail.push_str(&metrics);
            }
            Paragraph::new(detail.dim()).render(
                Rect::new(area.x, area.bottom() - footer_height - 2, area.width, 1),
                buf,
            );
            Paragraph::new(format!(" {}", row.activity).dim()).render(
                Rect::new(area.x, area.bottom() - footer_height - 1, area.width, 1),
                buf,
            );
        }
        if area.height >= 3 + footer_height {
            let mut x = area.x;
            let mut y = area.bottom() - footer_height;
            let stoppable = rows
                .iter()
                .any(|r| Some(r.id) == self.selected && r.can_stop());
            let hint = |action| {
                self.keymap
                    .primary_hint(action)
                    .map(|hint| ratatui::text::Span::from(hint).content.into_owned())
                    .unwrap_or_default()
            };
            let view = if area.width < 40 {
                "[View] ".to_string()
            } else {
                format!("[{} View] ", hint(ListAction::Accept))
            };
            let close = if area.width < 40 {
                "[Close]".to_string()
            } else {
                format!("[{} Close]", hint(ListAction::Cancel))
            };
            for (label, hit) in [
                (view.as_str(), Hit::Open),
                (
                    if stoppable {
                        if area.width < 40 {
                            "[Stop] "
                        } else {
                            "[x Stop] "
                        }
                    } else {
                        ""
                    },
                    Hit::Stop,
                ),
                (close.as_str(), Hit::Close),
            ] {
                let width = (label.width() as u16).min(area.right().saturating_sub(x));
                let rect = Rect::new(x, y, width, 1);
                Paragraph::new(label.cyan()).render(rect, buf);
                hits.push((rect, hit));
                if footer_height > 1 {
                    y += 1;
                } else {
                    x += width;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keymap::RuntimeKeymap;
    use pretty_assertions::assert_eq;
    use tokio::sync::mpsc;

    fn rows(count: usize) -> Vec<SubagentRow> {
        (0..count)
            .map(|i| SubagentRow {
                id: ThreadId::from_u128(i as u128 + 1),
                name: if i == 0 {
                    "Main [default]".into()
                } else {
                    format!("/root/worker_{i}")
                },
                activity: "Checking keyboard and mouse interactions".into(),
                status: if i == 0 { "idle" } else { "running" },
                running: i != 0,
                primary: i == 0,
                current: i == 0,
                started_at: Some(100),
                finished_at: Some(190),
                tokens: Some(12345),
            })
            .collect()
    }

    fn manager(count: usize) -> (SubagentManager, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            SubagentManager::new(
                rows(count),
                AppEventSender::new(tx),
                RuntimeKeymap::defaults().list,
            ),
            rx,
        )
    }

    fn paint(view: &impl Renderable, width: u16, height: Option<u16>) -> String {
        let rect = Rect::new(
            3,
            2,
            width,
            height.unwrap_or_else(|| view.desired_height(width)),
        );
        let mut buf = Buffer::empty(rect);
        view.render(rect, &mut buf);
        (rect.y..rect.bottom())
            .map(|y| {
                (rect.x..rect.right())
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(view: &mut SubagentManager, target: fn(Hit) -> bool) {
        let area = view
            .hits
            .borrow()
            .iter()
            .find(|(_, hit)| target(*hit))
            .expect("painted control")
            .0;
        assert!(view.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y
        )));
    }

    #[test]
    fn keyboard_navigation_search_and_stop_keep_the_parent_owned_model() {
        let (mut view, mut rx) = manager(16);
        view.handle_key_event(KeyCode::Char('x').into());
        assert!(
            rx.try_recv().is_err(),
            "main is never stopped from a child control"
        );
        view.handle_key_event(KeyCode::End.into());
        let child = rows(16)[15].id;
        assert_eq!(view.selected, Some(child));
        view.handle_key_event(KeyCode::Char('x').into());
        assert!(matches!(rx.try_recv(), Ok(AppEvent::StopSubagent(id)) if id == child));
        assert!(!view.is_complete());
        view.handle_key_event(KeyCode::Char('/').into());
        view.handle_paste("worker_2".into());
        assert_eq!(view.filtered().len(), 1);
        let mut refreshed = rows(16);
        refreshed.reverse();
        refreshed
            .iter_mut()
            .for_each(|row| row.activity = "New activity".into());
        view.update_subagents(&refreshed);
        assert_eq!(view.selected, Some(rows(3)[2].id));
        assert_eq!(view.query, "worker_2");
        view.handle_key_event(KeyCode::Enter.into());
        assert!(
            matches!(rx.try_recv(), Ok(AppEvent::SelectAgentThread(id)) if id == rows(3)[2].id)
        );
        assert!(view.is_complete());
    }

    #[test]
    fn mouse_select_scroll_stop_view_and_close_work_at_all_widths() {
        for width in [100, 40, 22, 16] {
            let (mut view, mut rx) = manager(24);
            paint(&view, width, None);
            click(
                &mut view,
                |hit| matches!(hit, Hit::Row(id) if id == ThreadId::from_u128(2)),
            );
            paint(&view, width, None);
            click(&mut view, |hit| matches!(hit, Hit::Stop));
            assert!(
                matches!(rx.try_recv(), Ok(AppEvent::StopSubagent(id)) if id == ThreadId::from_u128(2))
            );
            for _ in 0..30 {
                assert!(view.handle_mouse(mouse(MouseEventKind::ScrollDown, 4, 4)));
            }
            paint(&view, width, None);
            assert_eq!(view.selected, Some(ThreadId::from_u128(24)));
            assert!(
                view.hits
                    .borrow()
                    .iter()
                    .any(|(_, hit)| matches!(hit, Hit::Row(id) if *id == ThreadId::from_u128(24)))
            );
            click(&mut view, |hit| matches!(hit, Hit::Open));
            assert!(
                matches!(rx.try_recv(), Ok(AppEvent::SelectAgentThread(id)) if id == ThreadId::from_u128(24))
            );
            let (mut view, _) = manager(3);
            paint(&view, width, None);
            click(&mut view, |hit| matches!(hit, Hit::Close));
            assert_eq!(view.completion(), Some(ViewCompletion::Cancelled));
        }
    }

    #[test]
    fn live_refresh_preserves_selection_and_rejects_removed_rows() {
        let (mut view, mut rx) = manager(4);
        view.handle_key_event(KeyCode::Down.into());
        paint(&view, 80, None);
        let old_hit = view
            .hits
            .borrow()
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::Row(id) if *id == ThreadId::from_u128(2)))
            .unwrap()
            .0;
        let mut updated = rows(4);
        updated.rotate_left(2);
        view.update_subagents(&updated);
        assert_eq!(view.selected, Some(ThreadId::from_u128(2)));
        updated.retain(|row| row.id != ThreadId::from_u128(2));
        view.update_subagents(&updated);
        view.handle_mouse(mouse(
            MouseEventKind::Down(MouseButton::Left),
            old_hit.x,
            old_hit.y,
        ));
        assert_eq!(view.selected, Some(ThreadId::from_u128(3)));
        assert!(rx.try_recv().is_err());
        view.handle_key_event(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn roster_mouse_can_collapse_manage_and_open_a_thread() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tx = AppEventSender::new(tx);
        let mut roster = SubagentRoster {
            rows: rows(4),
            ..Default::default()
        };
        paint(&roster, 80, None);
        assert!(roster.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 4), &tx));
        assert!(
            matches!(rx.try_recv(), Ok(AppEvent::SelectAgentThread(id)) if id == ThreadId::from_u128(2))
        );
        assert!(roster.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 79, 2), &tx));
        assert_eq!(roster.desired_height(80), 1);
        paint(&roster, 80, None);
        assert!(!roster.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 4), &tx));
        assert!(roster.mouse(mouse(MouseEventKind::Down(MouseButton::Left), 4, 2), &tx));
        assert!(matches!(rx.try_recv(), Ok(AppEvent::OpenAgentPicker)));
    }

    #[test]
    fn manager_and_roster_layouts() {
        let (mut view, _) = manager(4);
        view.handle_key_event(KeyCode::Down.into());
        insta::assert_snapshot!("subagent_manager_wide", paint(&view, 120, None));
        insta::assert_snapshot!("subagent_manager_narrow", paint(&view, 40, None));
        insta::assert_snapshot!("subagent_manager_tiny", paint(&view, 16, None));
        let roster = SubagentRoster {
            rows: rows(4),
            ..Default::default()
        };
        insta::assert_snapshot!("subagent_roster", paint(&roster, 120, None));
    }
}

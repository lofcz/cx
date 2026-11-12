//! Presentation telemetry for the live subagent roster, independent of transcript eviction.
//! The navigation cache remains authoritative for visibility, ownership and liveness.

use super::*;
use crate::bottom_pane::subagents::SubagentRow;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::Thread;
use codex_app_server_protocol::Turn;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;

#[derive(Clone, Debug, Default)]
pub(super) struct AgentLiveDetails {
    activity: String,
    started_at: Option<i64>,
    finished_at: Option<i64>,
    tokens: Option<i64>,
    turn_id: Option<String>,
    terminal_status: Option<&'static str>,
    stop_request: Option<Uuid>,
    streaming_item: Option<String>,
}

impl AgentLiveDetails {
    pub(super) fn observe_liveness(&mut self, running: bool) {
        if running {
            if self.finished_at.is_some() {
                self.started_at = None;
                self.turn_id = None;
                self.streaming_item = None;
            }
            self.started_at
                .get_or_insert_with(|| chrono::Utc::now().timestamp());
            self.finished_at = None;
            self.terminal_status = None;
        } else {
            self.stop_request = None;
            if self.started_at.is_some() {
                self.finished_at
                    .get_or_insert_with(|| chrono::Utc::now().timestamp());
            }
        }
    }

    fn observe_delta(&mut self, item_id: String, delta: &str) {
        if self.streaming_item.as_ref() != Some(&item_id) {
            self.streaming_item = Some(item_id);
            self.activity.clear();
        }
        // Status rows need a bounded preview, not an additional copy of streamed output.
        let remaining = 240_usize.saturating_sub(self.activity.chars().count());
        self.activity.extend(
            delta
                .chars()
                .map(|c| if c.is_whitespace() { ' ' } else { c })
                .filter(|c| !c.is_control())
                .take(remaining),
        );
    }

    fn observe_turn(&mut self, turn: &Turn) {
        if turn.status == TurnStatus::InProgress {
            let same_turn = self.turn_id.as_ref() == Some(&turn.id);
            self.turn_id = Some(turn.id.clone());
            self.started_at = turn
                .started_at
                .or(same_turn.then_some(self.started_at).flatten())
                .or_else(|| Some(chrono::Utc::now().timestamp()));
            self.finished_at = None;
            self.terminal_status = None;
            self.stop_request = None;
            if !same_turn {
                self.activity.clear();
                self.streaming_item = None;
            }
        } else if self.turn_id.as_ref().is_none_or(|id| *id == turn.id) {
            self.turn_id = Some(turn.id.clone());
            self.started_at = turn.started_at.or(self.started_at);
            self.finished_at = turn
                .completed_at
                .or_else(|| Some(chrono::Utc::now().timestamp()));
            self.terminal_status = Some(match turn.status {
                TurnStatus::Completed => "completed",
                TurnStatus::Interrupted => "interrupted",
                TurnStatus::Failed => "failed",
                TurnStatus::InProgress => unreachable!(),
            });
            self.stop_request = None;
        }
    }
}

impl AgentNavigationState {
    pub(super) fn observe_agent_snapshot(&mut self, id: ThreadId, thread: &Thread) {
        let details = self.activity.entry(id).or_default();
        if details.turn_id.is_none()
            && let Some(turn) = thread.turns.last()
        {
            details.observe_turn(turn);
            if let Some(summary) = turn
                .items
                .iter()
                .rev()
                .find_map(|item| super::agent_status_feed::activity_summary(&item.payload))
            {
                details.activity = summary;
            }
        }
        if details.activity.is_empty() {
            details.activity =
                super::agent_status_feed::bounded_summary(&thread.preview).unwrap_or_default();
        }
    }

    pub(super) fn observe_agent_notification(
        &mut self,
        id: ThreadId,
        notification: &ServerNotification,
    ) -> bool {
        match notification {
            ServerNotification::AgentMessageDelta(event) => self
                .activity
                .entry(id)
                .or_default()
                .observe_delta(event.item_id.clone(), &event.delta),
            ServerNotification::ReasoningSummaryTextDelta(event) => {
                self.activity.entry(id).or_default().observe_delta(
                    format!("{}:{}", event.item_id, event.summary_index),
                    &event.delta,
                )
            }
            ServerNotification::TurnStarted(event) => self
                .activity
                .entry(id)
                .or_default()
                .observe_turn(&event.turn),
            ServerNotification::TurnCompleted(event) => {
                let running = self.get(&id).is_some_and(|entry| entry.is_running);
                let details = self.activity.entry(id).or_default();
                if running && details.turn_id.as_deref() != Some(event.turn.id.as_str()) {
                    return false;
                }
                details.observe_turn(&event.turn);
            }
            ServerNotification::ThreadTokenUsageUpdated(event) => {
                self.activity.entry(id).or_default().tokens =
                    Some(event.token_usage.total.total_tokens)
            }
            ServerNotification::ItemStarted(event) => {
                if let Some(summary) =
                    super::agent_status_feed::activity_summary(&event.item.payload)
                {
                    let details = self.activity.entry(id).or_default();
                    details.activity = summary;
                    details.streaming_item = None;
                } else {
                    return false;
                }
            }
            ServerNotification::ItemCompleted(event) => {
                if let Some(summary) =
                    super::agent_status_feed::activity_summary(&event.item.payload)
                {
                    let details = self.activity.entry(id).or_default();
                    details.activity = summary;
                    details.streaming_item = None;
                } else {
                    return false;
                }
            }
            ServerNotification::ThreadStarted(event) => {
                self.observe_agent_snapshot(id, &event.thread)
            }
            _ => return false,
        }
        true
    }
}

impl App {
    pub(super) fn sync_subagents(&mut self) -> bool {
        let current = self.current_displayed_thread_id();
        let mut rows: Vec<_> = self
            .agent_navigation
            .visible_threads()
            .into_iter()
            .filter(|(id, _)| !self.side_threads.contains_key(id))
            .map(|(id, entry)| {
                let details = self.agent_navigation.activity.get(&id);
                let pending = self.agent_navigation.pending_approvals.contains(&id);
                let status = if details.is_some_and(|details| details.stop_request.is_some()) {
                    "stopping"
                } else if pending {
                    "needs approval"
                } else if entry.is_closed {
                    "closed"
                } else if entry.is_running {
                    "running"
                } else {
                    details.and_then(|d| d.terminal_status).unwrap_or("idle")
                };
                SubagentRow {
                    id,
                    name: self.thread_label(id),
                    activity: details.map(|d| d.activity.clone()).unwrap_or_default(),
                    status,
                    running: entry.is_running,
                    primary: self.primary_thread_id == Some(id),
                    current: current == Some(id),
                    started_at: details.and_then(|d| d.started_at),
                    finished_at: details.and_then(|d| d.finished_at),
                    tokens: details.and_then(|d| d.tokens),
                }
            })
            .collect();
        rows.sort_by_key(|row| !self.agent_navigation.pending_approvals.contains(&row.id));
        self.chat_widget.set_subagents(rows)
    }

    pub(super) async fn stop_subagent(
        &mut self,
        app_server: &AppServerSession,
        thread_id: ThreadId,
    ) {
        let Some(root) = self.primary_thread_id else {
            return;
        };
        if root == thread_id
            || !self
                .agent_navigation
                .visible_threads()
                .iter()
                .any(|(id, entry)| *id == thread_id && entry.is_running && !entry.is_closed)
        {
            return;
        }
        let turn_id = self
            .active_turn_id_for_thread(thread_id)
            .await
            .or_else(|| {
                self.agent_navigation
                    .activity
                    .get(&thread_id)
                    .and_then(|details| details.turn_id.clone())
            })
            .unwrap_or_default();
        let details = self.agent_navigation.activity.entry(thread_id).or_default();
        if details.stop_request.is_some() {
            return;
        }
        let request_id = Uuid::new_v4();
        details.stop_request = Some(request_id);
        self.sync_subagents();
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            let result = handle
                .request_typed::<TurnInterruptResponse>(ClientRequest::TurnInterrupt {
                    request_id: RequestId::String(request_id.to_string()),
                    params: TurnInterruptParams {
                        thread_id: thread_id.to_string(),
                        turn_id,
                    },
                })
                .await
                .map(|_| ())
                .map_err(|err| err.to_string());
            tx.send(AppEvent::SubagentStopFinished {
                root,
                thread_id,
                request_id,
                result,
            });
        });
    }

    pub(super) fn finish_subagent_stop(
        &mut self,
        root: ThreadId,
        thread_id: ThreadId,
        request_id: Uuid,
        result: Result<(), String>,
    ) {
        if self.primary_thread_id != Some(root) {
            return;
        }
        let Some(details) = self.agent_navigation.activity.get_mut(&thread_id) else {
            return;
        };
        if details.stop_request != Some(request_id) {
            return;
        }
        details.stop_request = None;
        if let Err(err) = result {
            details.activity = format!(
                "Stop failed: {}",
                crate::text_formatting::truncate_text(&err, 200)
            );
            self.chat_widget.add_error_message(format!(
                "Could not stop {}: {err}",
                self.thread_label(thread_id)
            ));
        }
        self.sync_subagents();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn turn(id: &str, status: TurnStatus, start: i64, end: Option<i64>) -> Turn {
        serde_json::from_value(serde_json::json!({
            "id": id, "status": status, "items": [], "startedAt": start, "completedAt": end,
        }))
        .expect("turn")
    }

    #[test]
    fn subagent_telemetry_tracks_turns_and_ignores_late_completions() {
        let mut details = AgentLiveDetails::default();
        details.observe_turn(&turn("one", TurnStatus::InProgress, 100, None));
        details.observe_delta("message-one".into(), "Checking files");
        details.observe_turn(&turn("one", TurnStatus::Completed, 100, Some(120)));
        assert_eq!(details.finished_at, Some(120));
        assert_eq!(details.terminal_status, Some("completed"));
        details.stop_request = Some(Uuid::new_v4());
        details.observe_turn(&turn("two", TurnStatus::InProgress, 130, None));
        assert_eq!(details.stop_request, None);
        details.observe_turn(&turn("one", TurnStatus::Completed, 100, Some(120)));
        assert_eq!(details.finished_at, None);
        assert_eq!(details.started_at, Some(130));
        assert_eq!(details.terminal_status, None);
        assert!(details.activity.is_empty());
        details.observe_turn(&turn("two", TurnStatus::Interrupted, 130, Some(150)));
        assert_eq!(details.terminal_status, Some("interrupted"));
        assert_eq!(details.finished_at, Some(150));
        details.observe_liveness(true);
        assert_eq!(details.turn_id, None);
        assert_eq!(details.finished_at, None);
    }

    #[test]
    fn subagent_streaming_preview_is_bounded_and_resets_for_new_items() {
        let mut details = AgentLiveDetails::default();
        details.observe_delta("one".into(), "Checking\n");
        details.observe_delta("one".into(), "files");
        assert_eq!(details.activity, "Checking files");
        details.observe_delta("one".into(), &"é".repeat(10000));
        assert_eq!(details.activity.chars().count(), 240);
        details.observe_delta("two".into(), "New activity");
        assert_eq!(details.activity, "New activity");
    }
}

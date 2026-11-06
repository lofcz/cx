//! Default-enabled, bounded recovery from provider overload errors.

use super::*;

const RETRY_SECONDS: [u64; 15] = [10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 20, 40, 80, 160, 240];

#[derive(Default)]
pub(super) struct OverloadRecovery {
    attempts: usize,
    token: Option<uuid::Uuid>,
    ready: bool,
}

impl OverloadRecovery {
    pub(super) fn pending(&self) -> bool {
        self.token.is_some()
    }

    pub(super) fn waiting(&self) -> bool {
        self.token.is_some() && !self.ready
    }
}

impl ChatWidget {
    pub(super) fn reset_overload_recovery(&mut self) {
        self.overload_recovery = OverloadRecovery::default();
    }

    pub(super) fn schedule_overload_recovery(&mut self) {
        if !self.local_settings.tui.server_overloaded_resume {
            return;
        }
        let Some(seconds) = RETRY_SECONDS.get(self.overload_recovery.attempts).copied() else {
            self.overload_recovery.token = None;
            return;
        };
        let token = uuid::Uuid::new_v4();
        self.overload_recovery.attempts += 1;
        self.overload_recovery.token = Some(token);
        self.overload_recovery.ready = false;
        self.add_to_history(history_cell::new_warning_event(format!(
            "Server overloaded. Resuming in {seconds}s. Press Ctrl+C to cancel."
        )));
        let tx = self.app_event_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(seconds)).await;
            tx.send(AppEvent::ServerOverloadedRetry { token });
        });
    }

    pub(crate) fn on_server_overloaded_retry(&mut self, token: uuid::Uuid) {
        if self.overload_recovery.token != Some(token) {
            return;
        }
        self.overload_recovery.ready = true;
        self.maybe_send_next_queued_input();
    }

    /// Runs only after the normal queue guards have accepted delivery.
    pub(super) fn take_overload_resume(&mut self) -> bool {
        if !self.overload_recovery.ready {
            return false;
        }
        self.overload_recovery.ready = false;
        self.overload_recovery.token = None;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chatwidget::tests::make_chatwidget_manual_with_sender;

    #[tokio::test]
    async fn codext_overload_recovery_waits_ten_seconds_ten_times_before_backoff() {
        let (mut chat, _, mut events, _) = make_chatwidget_manual_with_sender().await;
        assert!(chat.local_settings.tui.server_overloaded_resume);
        tokio::time::pause();
        for seconds in [10_u64; 10].into_iter().chain([20, 40, 80, 160, 240]) {
            chat.schedule_overload_recovery();
            let expected_token = chat.overload_recovery.token.unwrap();
            while events.try_recv().is_ok() {}
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_millis(seconds * 1000 - 1)).await;
            tokio::task::yield_now().await;
            while let Ok(event) = events.try_recv() {
                assert!(!matches!(event, AppEvent::ServerOverloadedRetry { .. }));
            }
            tokio::time::advance(Duration::from_millis(1)).await;
            tokio::task::yield_now().await;
            let mut retry_token = None;
            while let Ok(event) = events.try_recv() {
                if let AppEvent::ServerOverloadedRetry { token } = event {
                    assert!(retry_token.replace(token).is_none());
                }
            }
            assert_eq!(retry_token, Some(expected_token));
            chat.on_server_overloaded_retry(expected_token);
            assert!(chat.take_overload_resume());
        }
        chat.schedule_overload_recovery();
        assert!(!chat.overload_recovery.pending());
    }

    #[tokio::test]
    async fn codext_overload_recovery_respects_explicit_disable() {
        let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
        chat.local_settings.tui.server_overloaded_resume = false;
        chat.schedule_overload_recovery();
        assert!(!chat.overload_recovery.pending());
    }

    #[tokio::test]
    async fn codext_overload_recovery_is_bounded_and_cancelable() {
        let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
        chat.local_settings.tui.server_overloaded_resume = true;
        for _ in 0..15 {
            chat.schedule_overload_recovery();
            assert!(chat.overload_recovery.waiting());
        }
        let stale = chat.overload_recovery.token.unwrap();
        chat.schedule_overload_recovery();
        assert!(!chat.overload_recovery.waiting());
        chat.on_server_overloaded_retry(stale);
        assert!(!chat.take_overload_resume());
        chat.reset_overload_recovery();
        chat.schedule_overload_recovery();
        let stale = chat.overload_recovery.token.unwrap();
        chat.on_ctrl_c();
        chat.on_server_overloaded_retry(stale);
        assert!(!chat.take_overload_resume());
    }

    #[tokio::test]
    async fn codext_overload_recovery_respects_manual_pause() {
        let (mut chat, _, _, mut ops) = make_chatwidget_manual_with_sender().await;
        chat.thread_id = Some(codex_protocol::ThreadId::new());
        chat.local_settings.tui.server_overloaded_resume = true;
        chat.input_queue.suppress_queue_autosend = true;
        chat.schedule_overload_recovery();
        chat.on_server_overloaded_retry(chat.overload_recovery.token.unwrap());
        assert!(chat.overload_recovery.ready);
        assert!(ops.try_recv().is_err());
        chat.input_queue.suppress_queue_autosend = false;
        assert!(chat.maybe_send_next_queued_input());
        assert!(matches!(
            ops.try_recv(),
            Ok(crate::app_command::AppCommand::UserTurn { .. })
        ));
        assert!(!chat.take_overload_resume());
    }
}

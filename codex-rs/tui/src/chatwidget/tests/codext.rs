use super::*;

#[tokio::test]
async fn cc_shift_up_edits_selection_without_changing_reasoning() {
    let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
    chat.thread_id = Some(ThreadId::new());
    chat.bottom_pane
        .set_composer_text("first\nsecond".into(), Vec::new(), Vec::new());
    let effort = chat.effective_reasoning_effort();
    chat.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::SHIFT));
    assert_eq!(chat.effective_reasoning_effort(), effort);
    chat.handle_key_event(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
    assert_eq!(chat.bottom_pane.composer_text_with_pending(), "first");
}

#[tokio::test]
async fn codext_copy_draft_preserves_text_and_does_not_interrupt() {
    let (mut chat, _, _, mut ops) = make_chatwidget_manual_with_sender().await;
    let key = KeyEvent::new(
        KeyCode::Char('C'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert!(matches!(chat.handle_key_event(key), KeyEventAction::None));
    assert!(chat.quit_shortcut_expires_at.is_none());
    let draft = "first line\nsecond line  ";
    chat.bottom_pane
        .set_composer_text(draft.into(), Vec::new(), Vec::new());
    match chat.handle_key_event(key) {
        KeyEventAction::CopyComposerDraft(text) => assert_eq!(text.as_ref(), draft),
        other => panic!("unexpected copy action: {other:?}"),
    }
    assert_eq!(chat.bottom_pane.composer_text_with_pending(), draft);
    assert!(ops.try_recv().is_err());
}

fn quota(used_percent: i32) -> RateLimitSnapshot {
    RateLimitSnapshot {
        limit_id: Some("codex".into()),
        primary: Some(RateLimitWindow {
            used_percent,
            window_duration_mins: Some(300),
            resets_at: None,
        }),
        limit_name: None,
        normal_model_slug: None,
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    }
}

#[tokio::test]
async fn codext_quota_recovery_keeps_manual_pause_and_queued_message() {
    let (mut chat, _, _, mut ops) = make_chatwidget_manual_with_sender().await;
    chat.thread_id = Some(ThreadId::new());
    set_chatgpt_auth(&mut chat);
    chat.on_rate_limit_error(RateLimitErrorKind::UsageLimit, "Usage exhausted".into());
    chat.on_rate_limit_snapshot(Some(quota(100)));
    chat.finish_rate_limit_recovery();
    chat.submit_user_message(UserMessage::from("after reset"));
    assert!(chat.input_queue.quota_exhausted);
    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    assert!(ops.try_recv().is_err());
    chat.input_queue.suppress_queue_autosend = true;
    chat.on_rate_limit_snapshot(Some(quota(10)));
    assert!(!chat.input_queue.quota_exhausted);
    assert!(chat.input_queue.suppress_queue_autosend);
    assert_eq!(chat.input_queue.queued_user_messages.len(), 1);
    chat.input_queue.suppress_queue_autosend = false;
    assert!(chat.maybe_send_next_queued_input());
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn codext_sparse_or_unrelated_quota_does_not_release_hold() {
    let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
    set_chatgpt_auth(&mut chat);
    chat.on_rate_limit_error(RateLimitErrorKind::UsageLimit, "Usage exhausted".into());
    chat.on_rate_limit_snapshot(Some(quota(100)));
    let mut sparse = quota(0);
    sparse.primary = None;
    chat.on_rate_limit_snapshot(Some(sparse));
    assert!(chat.input_queue.quota_exhausted);
    let mut unrelated = quota(10);
    unrelated.limit_id = Some("other-model".into());
    chat.on_rate_limit_snapshot(Some(unrelated));
    assert!(chat.input_queue.quota_exhausted);
}

#[tokio::test]
async fn codext_historical_failures_do_not_schedule_recovery_or_hold_quota() {
    let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
    chat.local_settings.tui.server_overloaded_resume = true;
    for error in [
        CodexErrorInfo::ServerOverloaded,
        CodexErrorInfo::UsageLimitExceeded,
    ] {
        chat.handle_non_retry_error("historical error".into(), Some(error), true);
        assert!(!chat.overload_recovery.pending());
        assert!(!chat.input_queue.quota_exhausted);
    }
}

#[tokio::test]
async fn cc_usage_footer_persists_during_typing_and_reports_actual_limits() {
    let (mut chat, _, _, _) = make_chatwidget_manual_with_sender().await;
    chat.thread_id = Some(ThreadId::new());
    set_chatgpt_auth(&mut chat);
    chat.warning_display_state.count = 6;
    chat.on_rate_limit_snapshot(Some(quota(1)));
    for running in [false, true] {
        chat.bottom_pane.set_task_running(running);
        for draft in ["", "draft line\nsecond line"] {
            chat.bottom_pane
                .set_composer_text(draft.into(), Vec::new(), Vec::new());
            for width in [40, 100] {
                let rendered = render_bottom_popup(&chat, width);
                assert_eq!(rendered.matches("Usage 99%").count(), 1, "{rendered}");
                assert!(!rendered.contains("f2 to view"), "{rendered}");
                assert!(!rendered.contains("6 warnings"), "{rendered}");
                let last = rendered
                    .lines()
                    .rev()
                    .find(|line| !line.trim().is_empty())
                    .unwrap();
                assert!(last.contains("Usage 99%"), "{rendered}");
            }
        }
    }
    chat.on_rate_limit_error(RateLimitErrorKind::UsageLimit, "Usage limit reached".into());
    assert!(
        chat.usage_notice(100)
            .unwrap()
            .to_string()
            .contains("Usage limit reached")
    );
    chat.on_rate_limit_snapshot(Some(quota(10)));
    assert!(
        chat.usage_notice(100)
            .unwrap()
            .to_string()
            .contains("Usage 90% left")
    );
}

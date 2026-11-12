use super::*;
use crate::render::renderable::Renderable;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

#[tokio::test]
async fn subagents_roster_opens_from_keyboard_and_mouse_without_losing_drafts() -> Result<()> {
    let (mut app, mut events, _) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut server = start_config_write_test_app_server(&app).await?;
    let root = ThreadId::from_u128(1);
    let child = ThreadId::from_u128(2);
    app.enqueue_primary_thread_session(
        test_thread_session(root, app.config.cwd.to_path_buf()),
        Vec::new(),
    )
    .await?;
    app.chat_widget
        .handle_thread_session(test_thread_session(root, app.config.cwd.to_path_buf()));
    app.upsert_agent_picker_thread(child, Some("worker".into()), None, false);
    app.agent_navigation.mark_parent_owned(child);
    app.agent_navigation.mark_running(child);
    app.refresh_agent_picker();
    let rendered = render_bottom_popup(&app.chat_widget, 100);
    assert!(rendered.contains("1 subagent ·"));
    while events.try_recv().is_ok() {}
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::Down.into()))
        .await?;
    assert!(matches!(events.try_recv(), Ok(AppEvent::OpenAgentPicker)));

    app.chat_widget.insert_str("preserve this draft");
    app.handle_tui_event(&mut tui, &mut server, TuiEvent::Key(KeyCode::F(6).into()))
        .await?;
    assert!(matches!(events.try_recv(), Ok(AppEvent::OpenAgentPicker)));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "preserve this draft"
    );
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Key(KeyEvent::new(KeyCode::F(6), KeyModifiers::SHIFT)),
    )
    .await?;
    assert!(render_bottom_popup(&app.chat_widget, 100).contains("[⇧F6 show]"));
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "preserve this draft"
    );
    app.chat_widget.show_subagents();
    app.chat_widget.handle_key_event(KeyCode::Down.into());
    // Snapshot helpers trim blank lines; mouse coordinates must come from the actual paint.
    let area = Rect::new(0, 0, 100, app.chat_widget.desired_height(100));
    let mut buffer = Buffer::empty(area);
    app.chat_widget.render(area, &mut buffer);
    let (row, line) = (0..area.height)
        .map(|row| {
            let line = (0..area.width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>();
            (row, line)
        })
        .find(|(_, line)| line.contains("[x Stop]"))
        .expect("stop control");
    let column = line[..line.find("[x Stop]").expect("stop position")]
        .chars()
        .count();
    app.handle_tui_event(
        &mut tui,
        &mut server,
        TuiEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: column as u16,
            row,
            modifiers: KeyModifiers::NONE,
        }),
    )
    .await?;
    assert!(matches!(events.try_recv(), Ok(AppEvent::StopSubagent(id)) if id == child));
    app.chat_widget.handle_key_event(KeyCode::Esc.into());
    assert_eq!(
        app.chat_widget.composer_text_with_pending(),
        "preserve this draft"
    );
    assert!(app.agent_navigation.is_parent_owned(child));
    server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn subagent_stop_targets_only_the_selected_child_and_keeps_ui_responsive() -> Result<()> {
    use super::session_lifecycle_requests::recorded_params;
    use super::session_lifecycle_requests::start_recording_app_server;
    let (mut app, mut events, _) = make_test_app_with_channels().await;
    let (server, requests, proxy) = start_recording_app_server(&app.config, None, None).await?;
    let root = ThreadId::from_u128(1);
    let child = ThreadId::from_u128(2);
    app.primary_thread_id = Some(root);
    app.upsert_agent_picker_thread(root, None, None, false);
    app.upsert_agent_picker_thread(child, Some("worker".into()), None, false);
    app.agent_navigation.mark_running(root);
    app.agent_navigation.mark_running(child);
    app.agent_navigation.mark_parent_owned(child);
    app.ensure_thread_channel(child)
        .store
        .lock()
        .await
        .set_active_turn_id("child-turn".into());
    app.stop_subagent(&server, root).await;
    app.stop_subagent(&server, ThreadId::new()).await;
    assert!(recorded_params(&requests, "turn/interrupt").is_empty());
    futures::FutureExt::now_or_never(app.stop_subagent(&server, child))
        .expect("stop must not wait on the server");
    app.stop_subagent(&server, child).await; // Coalesce duplicate key presses.
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(AppEvent::SubagentStopFinished {
                root,
                thread_id,
                request_id,
                result,
            }) = events.recv().await
            {
                break (root, thread_id, request_id, result);
            }
        }
    })
    .await?;
    assert_eq!(
        recorded_params(&requests, "turn/interrupt"),
        vec![serde_json::json!({"threadId": child.to_string(), "turnId": "child-turn"})]
    );
    // The intentionally absent child fails on the server; that failure must preserve its state.
    assert!(result.3.is_err());
    app.finish_subagent_stop(result.0, result.1, result.2, result.3);
    assert!(app.agent_navigation.get(&child).expect("child").is_running);
    assert!(app.agent_navigation.get(&root).expect("root").is_running);
    assert!(app.agent_navigation.is_parent_owned(child));
    server.shutdown().await?;
    proxy.await??;
    Ok(())
}

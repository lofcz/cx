use super::*;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;

fn definition(name: &str) -> WorkflowDefinition {
    WorkflowDefinition {
        name: name.to_string(),
        description: "Review code".to_string(),
        origin: "built-in".to_string(),
        path: None,
        editable: true,
        deletable: false,
        error: None,
    }
}

fn library() -> WorkflowDefinitionListResponse {
    WorkflowDefinitionListResponse {
        data: vec![definition("code-review"), definition("deep-research")],
        personal_directory: "/home/test/workflows".to_string(),
        authoring_guide: "Workflow API".to_string(),
    }
}

#[tokio::test]
async fn workflow_named_command_requests_run_without_extra_prompt() {
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    widget.handle_workflow_manager_command(library(), "code-review");
    let mut found = false;
    while let Ok(event) = events.try_recv() {
        if let AppEvent::ReadWorkflowDefinition { name, action, .. } = event {
            assert_eq!(name, "code-review");
            assert!(matches!(action, WorkflowDefinitionAction::Run(args) if args.is_empty()));
            found = true;
        }
    }
    assert!(found);
}

#[tokio::test]
async fn workflow_inline_edit_and_delete_reach_definition_controls() {
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    widget.handle_workflow_manager_command(library(), "edit code-review focus on security");
    widget.handle_workflow_manager_command(library(), "delete code-review");
    let mut actions = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::ReadWorkflowDefinition { action, .. } = event {
            actions.push(action);
        }
    }
    assert_eq!(actions.len(), 2);
    assert!(
        matches!(&actions[0], WorkflowDefinitionAction::Edit(text) if text == "focus on security")
    );
    assert!(matches!(&actions[1], WorkflowDefinitionAction::Delete));
}

#[test]
fn workflow_name_match_preserves_spaces_and_does_not_match_prefixes() {
    let definitions = vec![definition("check"), definition("check all")];
    let (definition, args) = match_definition(&definitions, "check all src/").unwrap();
    assert_eq!(definition.name, "check all");
    assert_eq!(args, "src/");
    assert!(match_definition(&definitions, "checker").is_none());
}

#[test]
fn workflow_code_review_defaults_use_real_workflow_tool() {
    let value = WorkflowDefinitionReadResponse {
        definition: definition("code-review"),
        source: "source".to_string(),
        sha256: "hash".to_string(),
        edit_path: "/home/test/workflows/code-review.js".to_string(),
        authoring_guide: "API".to_string(),
    };
    let context = run_context(&value, Some(""));
    assert_eq!(
        workflow_call(&context),
        serde_json::json!({"name": "code-review", "args": "high"})
    );
    assert!(context.contains("WaitWorkflow"));
    assert_eq!(
        workflow_call(&run_context(&value, Some("max main"))),
        serde_json::json!({"name": "code-review", "args": "max main"})
    );
    let display = ChatWidget::user_message_display_from_parts(
        format!("{context}\n## My request for Codex:\n/workflow code-review"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(display.message, "/workflow code-review");
}

#[tokio::test]
async fn workflow_library_is_actionable_before_any_runs() {
    use crate::chatwidget::tests::render_bottom_popup;
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    widget.handle_workflow_manager_command(library(), "");
    let rendered = render_bottom_popup(&widget, 100);
    for label in [
        "Create workflow",
        "Describe a change",
        "View runs",
        "code-review",
        "deep-research",
    ] {
        assert!(rendered.contains(label), "Missing {label}: {rendered}");
    }
    widget.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(event, AppEvent::OpenWorkflowManager { command, .. } if command == "create")
    }));
}

#[tokio::test]
async fn workflow_create_and_edit_prompts_accept_natural_language() {
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    widget.handle_workflow_manager_command(library(), "create");
    widget.handle_paste("Review changes and verify every finding".to_string());
    widget.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let request = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| {
            if let AppEvent::SubmitWorkflowRequest {
                request, context, ..
            } = event
            {
                Some((request, context))
            } else {
                None
            }
        })
        .expect("create request");
    assert_eq!(
        request.0,
        "/workflow create Review changes and verify every finding"
    );
    assert!(request.1.contains("/home/test/workflows"));
    assert!(request.1.contains("Workflow API"));

    let value = WorkflowDefinitionReadResponse {
        definition: definition("code-review"),
        source: "existing source".to_string(),
        sha256: "hash".to_string(),
        edit_path: "/home/test/workflows/code-review.js".to_string(),
        authoring_guide: "API".to_string(),
    };
    widget.handle_workflow_definition(value, WorkflowDefinitionAction::Edit(String::new()));
    widget.handle_paste("Only report security defects".to_string());
    widget.handle_key_event(KeyEvent::from(KeyCode::Enter));
    let request = std::iter::from_fn(|| events.try_recv().ok())
        .find_map(|event| {
            if let AppEvent::SubmitWorkflowRequest {
                request, context, ..
            } = event
            {
                Some((request, context))
            } else {
                None
            }
        })
        .expect("edit request");
    assert_eq!(
        request.0,
        "/workflow edit code-review Only report security defects"
    );
    assert!(request.1.contains("existing source"));
}

#[tokio::test]
async fn workflow_run_submits_to_owner_and_keeps_generated_context_out_of_history() {
    let (mut widget, _sender, mut events, mut operations) =
        make_chatwidget_manual_with_sender().await;
    widget.thread_id = Some(ThreadId::new());
    let value = WorkflowDefinitionReadResponse {
        definition: definition("code-review"),
        source: "script source".to_string(),
        sha256: "hash".to_string(),
        edit_path: "/home/test/workflows/code-review.js".to_string(),
        authoring_guide: "API".to_string(),
    };
    widget.handle_workflow_definition(value, WorkflowDefinitionAction::Run(String::new()));
    let items = std::iter::from_fn(|| operations.try_recv().ok())
        .find_map(|op| {
            if let AppCommand::UserTurn { items, .. } = op {
                Some(items)
            } else {
                None
            }
        })
        .expect("one owning user turn");
    let text = items
        .iter()
        .find_map(|item| match item {
            codex_app_server_protocol::UserInput::Text { text, .. } => Some(text),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        workflow_call(text),
        serde_json::json!({"name": "code-review", "args": "high"})
    );
    assert!(text.ends_with("/workflow code-review"));
    let history = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| {
            if let AppEvent::InsertHistoryCell(cell) = event {
                Some(cell.display_lines(100))
            } else {
                None
            }
        })
        .flatten()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(history.contains("/workflow code-review"), "{history}");
    assert!(!history.contains("script source"));
}

#[tokio::test]
async fn workflow_delete_requires_explicit_selection_and_preserves_revision() {
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    let mut saved = definition("my-review");
    saved.deletable = true;
    saved.path = Some("/home/test/workflows/my-review.js".to_string());
    let value = WorkflowDefinitionReadResponse {
        definition: saved,
        source: "script".to_string(),
        sha256: "selected-hash".to_string(),
        edit_path: "/home/test/workflows/my-review.js".to_string(),
        authoring_guide: "API".to_string(),
    };
    widget.handle_workflow_definition(value.clone(), WorkflowDefinitionAction::Delete);
    widget.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(
        !std::iter::from_fn(|| events.try_recv().ok())
            .any(|event| matches!(event, AppEvent::DeleteWorkflowDefinition { .. }))
    );
    widget.handle_workflow_definition(value, WorkflowDefinitionAction::Delete);
    widget.handle_key_event(KeyEvent::from(KeyCode::Down));
    widget.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(event, AppEvent::DeleteWorkflowDefinition { definition, .. } if definition.sha256 == "selected-hash")
    }));
}

#[tokio::test]
async fn workflow_slash_command_and_feature_gate_use_singular_name() {
    use crate::bottom_pane::slash_commands::{BuiltinCommandFlags, find_builtin_command};
    assert!(find_builtin_command("workflow", BuiltinCommandFlags::default()).is_none());
    assert_eq!(
        find_builtin_command(
            "workflow",
            BuiltinCommandFlags {
                workflow_command_enabled: true,
                ..Default::default()
            }
        ),
        Some(SlashCommand::Workflow)
    );
    assert!(SlashCommand::Workflow.supports_inline_args());
    let (mut widget, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    widget.dispatch_command_with_args(
        SlashCommand::Workflow,
        "code-review".to_string(),
        Vec::new(),
    );
    assert!(std::iter::from_fn(|| events.try_recv().ok()).any(|event| {
        matches!(event, AppEvent::OpenWorkflowManager { command, .. } if command == "code-review")
    }));
}

fn workflow_call(text: &str) -> serde_json::Value {
    let (_, arguments) = text
        .split_once("Invoke Workflow with ")
        .expect("workflow launch instruction");
    serde_json::Deserializer::from_str(arguments)
        .into_iter::<serde_json::Value>()
        .next()
        .unwrap()
        .unwrap()
}

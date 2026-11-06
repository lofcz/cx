#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT/codex-rs"

# Upstream's resume-picker transcript test already needs nearly all of the 8 MiB
# test-thread stack. CX's added config fields tip it over, so give fork tests headroom.
MIN_STACK="${RUST_MIN_STACK:-16777216}"

cargo fmt --check
cargo check --locked --workspace --lib --bins

# Workflow is a large fork-only surface, so run both crates' complete test suites.
just rust_min_stack="$MIN_STACK" test -p codex-workflow -p codex-workflow-extension

# Fork-only provider and protocol surfaces. These crates are small enough to
# run completely instead of maintaining a fragile list of individual tests.
just rust_min_stack="$MIN_STACK" test \
  -p codex-api \
  -p codex-protocol \
  -p codex-code-mode-runtime \
  -p codex-model-provider-info \
  -p codex-model-provider \
  -p codex-memories-write \
  -p codex-skills \
  -p codex-tools

# CX stores binaries separately from shared authentication and session files.
just rust_min_stack="$MIN_STACK" test -p codex-install-context

# The app-server protocol and daemon package layout changed in this fork.
just rust_min_stack="$MIN_STACK" test -p codex-app-server-protocol -p codex-app-server-daemon

# Keep app-server workflow/transport coverage at the lib level; integration
# tests that spawn the binary are covered by their dedicated CI suites.
just rust_min_stack="$MIN_STACK" test -p codex-app-server --lib \
  -E 'test(workflow_events) | test(transport)'

# Keep the legacy wire/updater/UI spot checks that are not covered above.
just rust_min_stack="$MIN_STACK" test \
  -p codex-cli \
  -p codex-core \
  -p codex-tui \
  -E '
    (package(codex-cli) & test(update::tests)) |
    (package(codex-cli) & test(workflow_cmd::tests)) |
    (package(codex-core) & test(config_schema_matches_fixture)) |
    (package(codex-core) & test(exec_command_enforces_bash_command_clamp_before_execution)) |
    (package(codex-core) & test(exec_command_clamp_forces_a_non_login_default_shell)) |
    (package(codex-core) & test(cx_overload_recovery_defaults_and_cli_overrides)) |
    (package(codex-core) & test(fresh_workflow_subagent_persists_ownership_edge_and_close_status)) |
    (package(codex-core) & test(chat_wire_api_posts_to_chat_completions_and_merges_extra_body)) |
    (package(codex-core) & test(anthropic_wire_api_posts_to_messages_and_merges_extra_body)) |
    (package(codex-core) & test(anthropic_wire_api_tool_call_round_trip_sends_tool_result)) |
    (package(codex-core) & test(responses_request_carries_model_extra_body)) |
    (package(codex-core) & test(responses_request_uses_function_exec_when_provider_rejects_custom_exec)) |
    (package(codex-core) & test(function_transport_extracts_raw_javascript)) |
    (package(codex-core) & test(function_transport_rejects_malformed_arguments)) |
    (package(codex-core) & test(code_mode_handler_accepts_custom_and_function_payloads)) |
    (package(codex-core) & test(multi_agent_config_precedence_overrides_remote_model_selector)) |
    (package(codex-core) & test(claude_file_tools_require_read_track_edits_and_reject_external_changes)) |
    (package(codex-tui) & test(workflow)) |
    (package(codex-tui) & test(slash_command::tests)) |
    (package(codex-tui) & test(cc_)) |
    (package(codex-tui) & test(keymap)) |
    (package(codex-tui) & test(empty_state_animation)) |
    (package(codex-tui) & test(fresh_sparkle_tests)) |
    (package(codex-tui) & test(startup_draft)) |
    (package(codex-tui) & test(usage_notice)) |
    (package(codex-tui) & test(codext_)) |
    (package(codex-tui) & test(server_overloaded)) |
    (package(codex-tui) & test(failed_turn_completion_preserves_queued_review_start)) |
    (package(codex-tui) & test(resume_picker)) |
    (package(codex-tui) & test(rate_limit_recovery)) |
    (package(codex-tui) & test(side_model)) |
    (package(codex-tui) & test(open_agent_picker_selects_path_backed_agent))
  '

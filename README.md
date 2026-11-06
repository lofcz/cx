<p align="center"><strong>CX</strong> is a Codex fork with custom providers, agent workflows, prompt selection, and a compact usage footer. It extends yuguorui/codex-plus with selected Codext features.</p>

<p align="center">Run <code>cx</code> in your shell. The local launcher shares your existing Codex configuration and sessions.</p>

---

## What is CX?

CX extends the official [Codex CLI](https://github.com/openai/codex) with:

- **Bongo Cat, obviously** - our most powerful (and cutest) feature. Pick it from `/pets` and it will drum along while you type. Image-capable terminals get the full sprite animation; everyone else gets a terminal-native ASCII cat.
- **Upstream updates** - yuguorui/codex-plus follows upstream Codex daily. This fork preserves its additions through explicit, tested updates.
- **Dynamic multi-agent workflows** - run deterministic JavaScript orchestration in the background with parallel agents, structured outputs, worktree isolation, retries, resumable runs, and live progress in `/workflows`.
- **Universal provider support** - connect CX to any provider through either native wire protocol:
  - **Chat Completions API** (`wire_api = "chat"`) - works with OpenAI-compatible providers including Ollama, vLLM, LiteLLM, DeepSeek, Mistral, Groq, Together, DashScope, and more, with cross-provider `reasoning_effort` projection (`low`/`high`/`max`) and `thinking_budget` reconciliation.
  - **Anthropic Messages API** (`wire_api = "anthropic"`) - use Claude models directly through Anthropic's native API with `output_config.effort` mapping, automatic ephemeral prompt caching, and configurable `capabilities.max_output_tokens`.
  - **Flexible endpoint & auth controls** - override `base_url` from the environment via `env_base_url`, pick `Bearer` or `x-api-key` auth via `env_key_auth`, and tune Responses-compatible gateway quirks with `[model_providers.<id>.capabilities]` (`supports_custom_exec_tool`, `supports_native_tool_search`, `max_output_tokens`).
  - **Provider-specific request fields** (`extra_body`) - merge arbitrary JSON into request bodies for provider-specific features like `enable_thinking`, `thinking_budget`, or custom `thinking` blocks.
  - **Non-GPT-friendly file tools** - offer hashline editing (`fuzz_view_edit`) alongside `apply_patch` and Claude Code-compatible structured `Read` / `Edit` tools (`apply_patch_tool_type = "claude_code"`), giving models that struggle with freeform patches more reliable alternatives.
- **Sharper TUI & streaming reliability** - cancel pending steers with double-`Esc` (`esc esc`) before the model consumes them, pause queued messages after pre-send validation failures, switch models thread-locally inside side conversations via `/model`, search subagents in `/agents`, and avoid discarding non-interruptible SSE streams when steering mid-turn.
- **More control** - make CX adapt to your setup, not the other way around:
  - **Multi-agent v2 opt-out** (`features.multi_agent_v2 = false`) - honor an explicit local disable even when the model catalog selects v2, so third-party providers are not forced onto its Responses API-specific namespaced and encrypted-schema tool protocol.
  - **Always retry** - automatically apply exponential backoff for HTTP 429, 5xx, `ServerOverloaded`, and transport errors.
- **Third-party memory compatibility** - use the active thread model for memory extraction and consolidation on non-OpenAI providers instead of sending provider-incompatible OpenAI model names.

For a detailed technical breakdown, see [`codex-rs/README.md`](./codex-rs/README.md).

## Local fork additions

See [fork features and maintenance](docs/fork-features.md) for prompt selection, reasoning shortcuts, session renaming, the usage footer, and recovery controls.

## Quickstart

### Use the local installation

Run `cx` in your shell. See [Local launcher](docs/fork-features.md#local-launcher) for installation details.
The command leaves the system C compiler, `cc`, available to builds.

### Install a published package

```shell
curl -fsSL https://cx.lofcz.dev/install.sh | bash
cx update
```

```powershell
irm https://cx.lofcz.dev/install.ps1 | iex
```

The Linux and macOS installer downloads a verified package from Hetzner and installs `cx` into `~/.local/bin`.
It keeps packages under `${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus/standalone`.
Run `cx update` to download later releases without a local Rust build.
The Windows installer stores CX under `%LOCALAPPDATA%\CX` and adds `cx` to your PATH.

Uninstall on Linux or macOS:

```shell
curl -fsSL https://cx.lofcz.dev/uninstall.sh | bash
```

Uninstall on Windows:

```powershell
irm https://cx.lofcz.dev/uninstall.ps1 | iex
```

Uninstalling CX preserves Codex configuration, authentication, and sessions.

Environment variables:
- `CX_INSTALL_DIR` - change the Linux or macOS command directory
- `XDG_DATA_HOME` - change the Linux or macOS package storage root
- `CX_INSTALL_ROOT` - change the Windows installation directory

### Build from source

```shell
CODEX_REPO_ROOT="$PWD" python3 scripts/build_codex_package.py \
  --target "$(rustc -vV | sed -n 's/^host: //p')" \
  --entrypoint-stem codex++ --cargo-profile dev-small \
  --package-dir /tmp/codex-plus-package
/tmp/codex-plus-package/bin/codex++ --no-daemon
```

### Quick usage

With the local launcher installed:

```shell
# Use with a Chat Completions provider
cx -m ollama/qwen3
cx -m deepseek/deepseek-chat
cx -m dashscope/qwen-plus

# Use with Anthropic
cx -m anthropic/claude-sonnet-4-20250514

# Use with reasoning effort control
cx -m deepseek/deepseek-chat --reasoning-effort low
cx -m anthropic/claude-sonnet-4-20250514 --reasoning-effort high
```

## Typing-reactive Bongo Cat

Yes, it types when you type. Run `/pets`, choose **Bongo Cat**, and your new pair-programming companion will settle in beside the composer. Kitty, iTerm2, and Sixel terminals get the full bundled sprite animation; everywhere else, Bongo Cat shows up as terminal-native ASCII art. Your choice is remembered in `config.toml`.

## Dynamic workflows

Workflows are enabled by default. Open `/workflow` to create, edit, delete, or run a workflow.
The library includes `code-review`, `deep-research`, and your saved workflows.

```text
/workflow code-review
/workflow code-review max main
/workflow deep-research compare local-first AI coding tools
/workflow create review changes, verify findings, then summarize the confirmed bugs
/workflow edit code-review focus on security and skip style issues
/workflow delete my-review
```

`/workflow code-review` starts a high-effort review with the default target.
Use **Run with instructions** to describe a different target or task.
**Create workflow** and **Edit** accept plain language. CX writes the saved workflow through its normal file tools.
**Describe a change** accepts other workflow requests in plain language.

Saved `.js` workflows come from `$CODEX_HOME/workflows` (normally `~/.codex/workflows`),
ancestor `.codex/workflows` directories, and active plugins.
Project definitions override personal definitions, which override built-in definitions.
Editing a built-in creates a personal version. Editing a plugin creates a separate personal copy.
Delete moves a saved definition into its directory's `.trash` folder. Any inherited version becomes available again.
Built-in and plugin originals remain read-only.
The library uses the app-server host filesystem. Separate remote execution environments still require inline workflow scripts.

Workflow scripts support parallel agents, structured results, temporary Git worktrees, retries, and resumable runs.
Runs use the existing owning-thread workflow engine and its approval settings.
Open **View runs**, `/workflow runs`, or `/workflows` to inspect progress, stop a run, or skip and retry agents.

Saved workflows are discovered from active plugins, `$CODEX_HOME/workflows`, and personal Claude workflows: `CLAUDE_CONFIG_DIR/workflows` when `CLAUDE_CONFIG_DIR` is set, otherwise `~/.claude/workflows`. Project discovery checks `.codex/workflows` and `.claude/workflows` at every directory from the current working directory up to the filesystem root. Only `.js` files are accepted. For command-line use and quick validation:

```bash
cx workflow run review-changes --args '{"path":"src"}'
cx workflow run ./.claude/workflows/review-changes.js
cx workflow run review-changes --resume wf_abc123
cx workflow validate ./.claude/workflows/review-changes.js --json
```

`workflow run` launches a normal `codex++ exec` turn that calls the existing `Workflow` tool and waits for the terminal result, then verifies the newest local workflow snapshot and exits non-zero unless the run completed. Omit `--args` to leave the script's `args` global `undefined`, or pass `--args null` for an explicit null. `--resume` continues an existing run and, without `--args`, restores the original arguments and their presence. `workflow validate` parses and validates the root script locally without a model call; saved child workflows are resolved and frozen when the root workflow is launched. For concurrent CI jobs, give each invocation a distinct `CODEX_HOME` so snapshot verification cannot observe another job's run. `workflow run` still follows the thread's approval and automatic-review policy; non-interactive workers must configure approval appropriately, or pass the root bypass flag only when that is acceptable.

Workflow agents default to a CPU-derived per-run limit.

To cap (or raise) the aggregate agent concurrency, set in `config.toml`:

```toml
[workflow]
global_concurrency = 32
```

Or set `CODEX_WORKFLOW_GLOBAL_CONCURRENCY=32` in environment variables.

## Configuration

Codex++ uses `config.toml` (same location as upstream: `~/.codex/config.toml`). Add custom provider blocks under `[model_providers.<id>]` (avoiding reserved built-in IDs such as `openai`, `ollama`, and `lmstudio`):

```toml
# Chat Completions provider (local Ollama, vLLM, DeepSeek, etc.)
[model_providers.local_ollama]
name = "Local Ollama"
base_url = "http://localhost:11434/v1"
env_base_url = "OLLAMA_BASE_URL"
env_key = "OLLAMA_API_KEY"
wire_api = "chat"

# Anthropic (Claude)
[model_providers.anthropic]
name = "Anthropic"
base_url = "https://api.anthropic.com/v1"
env_key = "ANTHROPIC_API_KEY"
env_key_auth = "x-api-key"
wire_api = "anthropic"

# DashScope (Qwen) with thinking enabled
[model_providers.dashscope]
name = "DashScope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
env_key = "DASHSCOPE_API_KEY"
wire_api = "chat"
extra_body = { "enable_thinking" = true }
```

### Multi-agent v2 compatibility

Upstream model metadata can select the multi-agent v2 tool protocol. V2 uses Responses API-specific tool namespaces and marks inter-agent message fields as encrypted, which many third-party providers do not support. Codex++ lets an explicit local setting take precedence over that model metadata:

```toml
[features]
multi_agent = true
multi_agent_v2 = false
```

With `multi_agent` left enabled (the default), Codex continues using its regular multi-agent function tools. Set `multi_agent = false` as well to disable multi-agent entirely.

### Third-party memory model selection

For non-OpenAI providers, memory extraction and consolidation use the current thread model by default, including model changes made after the thread starts. Explicit phase overrides still take priority:

```toml
[memories]
extract_model = "provider/model-for-extraction"
consolidation_model = "provider/model-for-consolidation"
```

See [`codex-rs/README.md`](./codex-rs/README.md) for the full configuration reference including `env_base_url`, `[model_providers.<id>.capabilities]`, `extra_body`, `extra_headers`, `env_key_auth`, `request_max_retry_delay_ms`, file tools, TUI features, and reasoning effort mapping.

## Upstream updates

[yuguorui/codex-plus](https://github.com/yuguorui/codex-plus) follows upstream Codex through daily rebases.
This fork pins its parent commit in `.codex-plus-base` and applies updates explicitly.
The inherited automatic rebase is disabled here, so it cannot overwrite the Codext additions.
See [Fork features](./docs/fork-features.md#update-from-codex) for the update procedure.

See the [upstream documentation](https://developers.openai.com/codex) for configuration, MCP, notifications, sandboxing, and execution.

## Docs

- [**Codex++ Technical Reference**](./codex-rs/README.md) - detailed wire API docs, configuration, and code organization
- [**Upstream Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

## License

This repository is licensed under the [Apache-2.0 License](LICENSE).

# CX features

CX extends [Codex++](https://github.com/yuguorui/codex-plus).
It adapts selected features from [Codext](https://github.com/Loongphy/codext), commit `42c669b2472488f2fcd88361b15f2fac3463beb4`.
Both projects use the Apache-2.0 license. Their existing license and notices remain in this repository.

- Press **Ctrl+Shift+C** to copy the composer draft, including pending pasted text. The shortcut keeps the draft intact.
  Empty drafts do not interrupt work. Terminals that intercept the shortcut must pass it through to CX.
- Press **Ctrl+R** in the session picker to rename the selected session. Press **Enter** to save or **Esc** to cancel.
  Archived sessions remain read-only. Existing list key bindings take precedence.
- Press **Shift+Up/Down** to select prompt rows. **Shift+Left/Right/Home/End** also extends the selection.
  Selection follows wrapped rows and preserves image and paste attachments as complete elements.
  Typing, pasting, or deleting replaces the selected text.
- Press **Alt+Up/Down** to increase or decrease reasoning effort.
  Queue and question navigation uses **Ctrl+Alt+Up/Down**.
- The bottom bar shows remaining account usage and its reset time.
  Advance usage warnings and model-switch prompts stay hidden until an actual usage limit occurs.
  The launcher disables the extra header above the prompt.
- The startup title displays **CX**. A dotted, rotating 3D CX mark appears while the prompt is empty.
  Typing pauses the animation. The reduced-motion setting also pauses it.
- Enable `tui.server_overloaded_resume` to resume overloaded turns after 15, 30, 60, 120, and 240 seconds.
  Recovery sends a queued follow-up first, or `Continue` when the queue is empty.
  **Ctrl+C**, manual submission, interruption, and session replacement cancel pending recovery.
  Manual pauses and uncertain delivery continue to block automatic submission.
- ChatGPT usage-limit failures preserve queued messages. A complete account quota refresh releases the quota hold when capacity returns.
  Empty snapshots and unrelated model quotas do not release the hold. Manual pauses remain active.

The local launcher enables overload recovery and disables the extra status header.
You can override these settings with `-c tui.server_overloaded_resume=false` or `-c tui.status_header=true`.
Existing authentication, models, and session storage work without migration.
Codex++ workflow orchestration and provider support remain available.

## Local launcher

`scripts/install/cx-local.sh` launches the separately installed local package.
It uses an embedded server for local sessions.
An explicit `--remote` connection keeps its requested server.
The launcher uses existing Codex configuration, authentication, and history.

Install the launcher as `~/.local/bin/cx`, with `~/.local/bin` on your PATH.
Place the package entrypoint link at `${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus/bin/cx-binary`.
The package retains its internal `bin/codex++` entrypoint for updater compatibility.
The launcher directs binary updates to this separate installation directory.
Source builds do not require a published GitHub release.

The command works in interactive shells and scripts.
It leaves the system C compiler, `cc`, available to builds.
When upgrading from CC, remove the previous `~/.config/fish/functions/cc.fish` function.
Run `functions --erase cc` in existing Fish sessions to unload the previous function.

## Validate changes

Run these commands from `codex-rs`:

```sh
export RUST_MIN_STACK=8388608
cargo fmt --all --check
cargo test --locked -p codex-tui --lib cc_
cargo test --locked -p codex-tui --lib codext_
cargo test --locked -p codex-tui --lib resume_picker
cargo test --locked -p codex-workflow -p codex-workflow-extension
```

The fork's broader checks remain in `.github/scripts/fork-key-tests.sh`.

## Update from Codex++

The `CX daily sync` workflow runs at 04:17 UTC each day.
It rebases the CX commits onto `yuguorui/codex-plus`, using the recorded parent commit.
DeepSeek V4.1 Flash 1017 resolves conflicts through Vercel AI Gateway.
The `CX_AI_GATEWAY_KEY` repository secret supplies its key.
Set the `CX_SYNC_MODEL` repository variable to select another Gateway model.
The local `.env` file can hold the same key; Git ignores that file.

The workflow audits every fork commit and runs `.github/scripts/fork-key-tests.sh` before updating `main`.
A failed rebase or validation leaves `main` and the last published binaries unchanged.
Successful syncs trigger `fork-release.yml` for Linux x64, Apple Silicon macOS, and Windows x64.
Release publication requires successful builds, regression checks, and installer checks.

The CursorPlusPlus `Publish CX binaries to Hetzner` workflow checks for releases hourly at minute 43.
It uses that repository's existing SSH secrets and verifies uploaded checksums.
It publishes release directories before replacing the latest-version manifest.
The server keeps seven releases under `https://gateway.lofcz.dev/dl/cx/releases/`.

Install or migrate a local source build with:

```sh
curl -fsSL https://gateway.lofcz.dev/dl/cx/install-cx.sh | bash
```

Then run `cx update` whenever you want the latest published package.
It preserves Codex configuration, authentication, and sessions.
The native updater and update notice use the Hetzner channel.
GitHub Releases retain the original build artifacts and source commit.

### Manual source sync

The parent rewrites its history during daily rebases. Use the pinned parent commit in `.codex-plus-base`.
Commit local changes before you update. Create an update branch, then run:

```sh
git fetch upstream main
git switch -c update/codex-plus
git rebase --onto upstream/main "$(cat .codex-plus-base)"
git rev-parse upstream/main > .codex-plus-base
git add .codex-plus-base
git commit -m 'chore: update Codex++ base'
```

Resolve conflicts and run the fork checks before updating the main branch with `--force-with-lease`.
The inherited automatic rebase runs only in the parent repository.
CX uses its separate daily workflow and Gateway secret.

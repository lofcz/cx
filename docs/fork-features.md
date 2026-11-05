# Fork features

This fork extends [Codex++](https://github.com/yuguorui/codex-plus).
It adapts selected features from [Codext](https://github.com/Loongphy/codext), commit `42c669b2472488f2fcd88361b15f2fac3463beb4`.
Both projects use the Apache-2.0 license. Their existing license and notices remain in this repository.

- Press **Ctrl+Shift+C** to copy the composer draft, including pending pasted text. The shortcut keeps the draft intact.
  Empty drafts do not interrupt work. Terminals that intercept the shortcut must pass it through to Codex++.
- Press **Ctrl+R** in the session picker to rename the selected session. Press **Enter** to save or **Esc** to cancel.
  Archived sessions remain read-only. Existing list key bindings take precedence.
- Enable `tui.status_header` to show the model, reasoning effort, directory, Git status, quota, and account above the composer.
  Git probes run through the workspace server, including remote workspaces, with a two-second timeout and 15-second refresh interval.
- Enable `tui.server_overloaded_resume` to resume overloaded turns after 15, 30, 60, 120, and 240 seconds.
  Recovery sends a queued follow-up first, or `Continue` when the queue is empty.
  **Ctrl+C**, manual submission, interruption, and session replacement cancel pending recovery.
  Manual pauses and uncertain delivery continue to block automatic submission.
- ChatGPT usage-limit failures preserve queued messages. A complete account quota refresh releases the quota hold when capacity returns.
  Empty snapshots and unrelated model quotas do not release the hold. Manual pauses remain active.

The header and overload recovery default to disabled. Enable them with:

```toml
[tui]
status_header = true
server_overloaded_resume = true
```

You can also pass `-c tui.status_header=true -c tui.server_overloaded_resume=true`.
Existing authentication, models, and session storage work without migration.
Codex++ workflow orchestration and provider support remain available.

## Validate changes

Run these commands from `codex-rs`:

```sh
cargo fmt --all --check
cargo test --locked -p codex-tui --lib codext_
cargo test --locked -p codex-tui --lib resume_picker
cargo test --locked -p codex-workflow -p codex-workflow-extension
```

The fork's broader checks remain in `.github/scripts/fork-key-tests.sh`.

## Update from Codex++

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

Resolve conflicts and run the fork checks before merging the update branch.
The inherited automatic rebase runs only in the parent repository.
This fork does not require the parent's model API secrets.
Release installers and `codex++ update` use `lofcz/codex-plus` releases.

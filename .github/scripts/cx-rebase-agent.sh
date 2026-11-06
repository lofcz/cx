#!/usr/bin/env bash
# Run without GitHub credentials. Only the inference key reaches the agent.
set -euo pipefail
: "${CX_AI_GATEWAY_KEY:?Set the CX_AI_GATEWAY_KEY repository secret}"
: "${CX_OLD_BASE:?}" "${CX_NEW_BASE:?}" "${CX_PRE_REBASE:?}"
cx_model="${CX_SYNC_MODEL:-deepseek/deepseek-v4.1-flash-1017}"
cx_prompt="Rebase this CX fork onto $CX_NEW_BASE, replaying only $CX_OLD_BASE..$CX_PRE_REBASE.
Do not push. Preserve every fork commit and its subject. The parent rewrites history daily.
If a rebase is already active, resolve its conflicts and continue it.
Otherwise run: git rebase --reapply-cherry-picks --empty=stop --onto $CX_NEW_BASE $CX_OLD_BASE
Keep all CX features described in docs/fork-features.md, including selection, Alt reasoning,
persistent quota, suppressed advance warnings, CX logo, provider recovery, and its release channel.
Preserve unrelated upstream changes. Inspect git range-diff to audit every replayed commit.
For empty commits, preserve them with git commit --allow-empty --no-edit and continue.
Run .github/scripts/fork-key-tests.sh. Repair failures with targeted edits, never weaken tests.
Run cargo fmt from codex-rs before committing. Fold repairs into the owning commit with fixups
and autosquash onto $CX_NEW_BASE. Do not modify .codex-plus-base; the workflow updates it later.
Do not change CI permissions, schedules, credentials, publication destinations, or validation gates.
Finish with a clean working tree and no active rebase. Stop if you cannot preserve behavior."
exec codex++ --no-daemon exec --ephemeral --dangerously-bypass-approvals-and-sandbox \
  --model "$cx_model" \
  -c 'model_provider="cx_gateway"' \
  -c 'model_providers.cx_gateway.name="Vercel AI Gateway"' \
  -c 'model_providers.cx_gateway.base_url="https://ai-gateway.vercel.sh/v1"' \
  -c 'model_providers.cx_gateway.env_key="CX_AI_GATEWAY_KEY"' \
  -c 'model_providers.cx_gateway.wire_api="chat"' \
  -c 'features.multi_agent_v2=false' \
  -c 'model_context_window=1048576' \
  "$cx_prompt"

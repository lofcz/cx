#!/usr/bin/env bash
set -euo pipefail
codexpp_root="${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus"
export CODEX_INSTALL_DIR="$codexpp_root/bin"
export CODEX_BIN_NAME='codex++-binary'
codexpp_server=(--no-daemon)
for codexpp_arg in "$@"; do
  case "$codexpp_arg" in
    --) break ;;
    --remote|--remote=*) codexpp_server=() ;;
  esac
done
exec "$CODEX_INSTALL_DIR/$CODEX_BIN_NAME" "${codexpp_server[@]}" \
  -c tui.status_header=true \
  -c tui.server_overloaded_resume=true "$@"

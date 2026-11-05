#!/usr/bin/env bash
set -euo pipefail
cc_root="${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus"
export CODEX_INSTALL_DIR="$cc_root/bin"
export CODEX_BIN_NAME='cc-binary'
cc_server=(--no-daemon)
for cc_arg in "$@"; do
  case "$cc_arg" in
    --) break ;;
    --remote|--remote=*) cc_server=() ;;
  esac
done
exec "$CODEX_INSTALL_DIR/$CODEX_BIN_NAME" "${cc_server[@]}" \
  -c tui.status_header=false \
  -c tui.server_overloaded_resume=true "$@"

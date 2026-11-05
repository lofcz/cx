#!/usr/bin/env bash
set -euo pipefail
cx_root="${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus"
export CODEX_INSTALL_DIR="$cx_root/bin"
export CODEX_BIN_NAME='cx-binary'
cx_server=(--no-daemon)
for cx_arg in "$@"; do
  case "$cx_arg" in
    --) break ;;
    --remote|--remote=*) cx_server=() ;;
  esac
done
exec "$CODEX_INSTALL_DIR/$CODEX_BIN_NAME" "${cx_server[@]}" \
  -c tui.status_header=false \
  -c tui.server_overloaded_resume=true "$@"

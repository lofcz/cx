#!/usr/bin/env bash
# Install the published CX package without a local Rust build.
set -euo pipefail
cx_base="${CX_RELEASE_BASE_URL:-https://gateway.lofcz.dev/dl/cx}"
cx_root="${XDG_DATA_HOME:-$HOME/.local/share}/codex-plus"
cx_bin_dir="${CX_INSTALL_DIR:-$HOME/.local/bin}"
cx_version="${1:-latest}"
if [[ "$cx_version" == --help || "$cx_version" == -h ]]; then
  echo 'Usage: install-cx.sh [latest|YYYYMMDDHHMM]'
  exit 0
fi
if [[ "$cx_version" == latest ]]; then
  cx_version="$(curl --fail --silent --show-error --location "$cx_base/latest.json" |
    sed -n 's/.*"tag_name":[[:space:]]*"rust-v\([0-9]\{12\}\)".*/\1/p')"
fi
[[ "$cx_version" =~ ^[0-9]{12}$ ]] || { echo 'Invalid CX release version.' >&2; exit 1; }
cx_tmp="$(mktemp -d)"
trap 'rm -rf "$cx_tmp"' EXIT
for cx_file in install-fork.sh cx-local.sh; do
  curl --fail --silent --show-error --location \
    "$cx_base/releases/$cx_version/$cx_file" -o "$cx_tmp/$cx_file"
done
CODEX_RELEASE_REPOSITORY=lofcz/codex-plus \
CODEX_RELEASE_BASE_URL="$cx_base/releases/$cx_version" \
CODEX_PACKAGE_ROOT="$cx_root/standalone" \
CODEX_INSTALL_DIR="$cx_root/bin" CODEX_BIN_NAME=cx-binary \
  sh "$cx_tmp/install-fork.sh" "$cx_version"
mkdir -p "$cx_bin_dir"
install -m 755 "$cx_tmp/cx-local.sh" "$cx_bin_dir/.cx-install.$$"
mv -f "$cx_bin_dir/.cx-install.$$" "$cx_bin_dir/cx"
"$cx_bin_dir/cx" --version
echo "Installed cx in $cx_bin_dir. Run cx update for future releases."

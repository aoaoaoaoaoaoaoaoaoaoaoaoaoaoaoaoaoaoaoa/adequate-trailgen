#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
install_root=${1:-${TRAILGEN_INSTALL_ROOT:-$HOME/.local}}

cargo install \
    --path "$root/crates/trailgen-cli" \
    --bin trailgen \
    --root "$install_root" \
    --locked \
    --force

"$install_root/bin/trailgen" --version >/dev/null
printf 'installed %s\n' "$install_root/bin/trailgen"

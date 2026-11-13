#!/usr/bin/env bash
# Todos os gates: o CI roda exatamente isto.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo audit
if command -v systemd-analyze >/dev/null; then
  # a unit precisa do binário e do usuário para o verify completo; aqui só
  # a sintaxe, com o ExecStart apontado para um binário que existe
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  sed "s|/usr/local/bin/carteiro|/bin/true|" deploy/carteiro.service >"$tmp/carteiro.service"
  systemd-analyze verify "$tmp/carteiro.service"
fi

#!/usr/bin/env bash
# Notas do GitHub Release: a seção da versão no CHANGELOG.md, como instalar e
# os sha256 dos artefatos.
#
#   scripts/notas-release.sh <tag> [diretório dos artefatos]
set -euo pipefail
cd "$(dirname "$0")/.."
tag=$1
dist=${2:-dist}
versao=${tag#v}
repo=https://github.com/LLawli/carteiro
baixar="$repo/releases/download/$tag"

secao=$(awk -v cab="## [$versao]" '
  index($0, "## [") == 1 { if (dentro) exit; if (index($0, cab) == 1) { dentro = 1; next } }
  /^\[[^]]+\]: / { next }
  dentro
' CHANGELOG.md)
if [ -z "$(tr -d '[:space:]' <<<"$secao")" ]; then
  echo "CHANGELOG.md não tem a seção [$versao]" >&2
  exit 1
fi

cat <<EOF
$secao

## Instalar

Debian 12+ / Ubuntu 22.04+:

\`\`\`sh
curl -LO $baixar/carteiro_${versao}_amd64.deb
sudo apt install ./carteiro_${versao}_amd64.deb
\`\`\`

Fedora / RHEL:

\`\`\`sh
sudo dnf install $baixar/carteiro-${versao}-1.x86_64.rpm
\`\`\`

Tarball (instalação à mão, ver o README):

\`\`\`sh
curl -LO $baixar/carteiro-v${versao}-x86_64-unknown-linux-gnu.tar.gz
tar -xzf carteiro-v${versao}-x86_64-unknown-linux-gnu.tar.gz
\`\`\`

Do código: \`cargo install --locked --git $repo --tag $tag\`

Em arm64, troque \`amd64\`/\`x86_64\` por \`arm64\`/\`aarch64\`. Depois de
instalar, siga o README: credenciais do Google, config e DNS.

## sha256

| Arquivo | sha256 |
|---|---|
EOF
for f in "$dist"/*.sha256; do
  read -r hash nome <"$f"
  echo "| \`$nome\` | \`$hash\` |"
done

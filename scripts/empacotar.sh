#!/usr/bin/env bash
# Empacota um binário já compilado: tarball, .deb e .rpm, cada um com o seu
# .sha256. Compila uma vez, empacota várias (o release do CI usa este script).
#
#   scripts/empacotar.sh <binário> <versão> <alvo>
#   ex.: scripts/empacotar.sh target/release/carteiro 0.1.0 x86_64-unknown-linux-gnu
#
# Precisa do nfpm no PATH (ou do comando em $NFPM). Saída em dist/.
set -euo pipefail
cd "$(dirname "$0")/.."
binario=$1 versao=$2 alvo=$3
case "$alvo" in
  x86_64-*) arch=amd64 ;;
  aarch64-*) arch=arm64 ;;
  *) echo "alvo sem pacote: $alvo" >&2; exit 1 ;;
esac
read -r -a nfpm <<<"${NFPM:-nfpm}"
nome="carteiro-v$versao-$alvo"
mkdir -p dist
rm -rf "dist/$nome" dist/preparo

# tarball: o binário com as units e a config de exemplo como estão no
# repositório (instalação à mão em /usr/local, como o README descreve)
mkdir -p "dist/$nome"
install -m 0755 "$binario" "dist/$nome/carteiro"
cp -r deploy config README.md LICENSE CHANGELOG.md "dist/$nome/"
tar -C dist -czf "dist/$nome.tar.gz" "$nome"
rm -rf "dist/$nome"

# pacotes: /usr/bin e /usr/lib, então as units apontam para lá
preparo=dist/preparo
mkdir -p "$preparo/systemd"
install -m 0755 "$binario" "$preparo/carteiro"
install -m 0755 deploy/copiar-certificado "$preparo/copiar-certificado"
for u in carteiro.service carteiro-certificado.service carteiro-certificado.path carteiro-certificado.timer; do
  sed -e 's|/usr/local/bin/carteiro|/usr/bin/carteiro|g' \
    -e 's|/usr/local/lib/carteiro|/usr/lib/carteiro|g' \
    "deploy/$u" >"$preparo/systemd/$u"
done
export ARCH=$arch VERSAO=$versao
"${nfpm[@]}" package --config pacote/nfpm.yaml --packager deb --target dist/
"${nfpm[@]}" package --config pacote/nfpm.yaml --packager rpm --target dist/
rm -rf "$preparo"

cd dist
for f in "$nome.tar.gz" carteiro_"$versao"_"$arch".deb carteiro-"$versao"-1.*.rpm; do
  # artefato faltando ou vazio para o release aqui
  [ -s "$f" ] || { echo "faltou $f" >&2; exit 1; }
  sha256sum "$f" >"$f.sha256"
  echo "$(cat "$f.sha256")  ($(($(stat -c %s "$f") / 1024)) KiB)"
done

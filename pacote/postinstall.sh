#!/bin/sh
set -e
if ! getent passwd carteiro >/dev/null; then
  useradd --system --home-dir /var/lib/carteiro --no-create-home \
    --shell /usr/sbin/nologin carteiro
fi
install -d -m 0750 -o root -g carteiro /etc/carteiro/tls
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
fi
cat <<'MSG'
carteiro instalado. Falta:
  1. editar /etc/carteiro/carteiro.toml (domínio, endereços, certificado);
  2. pôr cliente.json e token.json em /etc/carteiro, root 0600
     (carteiro autorizar <client_secret.json> token.json);
  3. systemctl enable --now carteiro
Ver /usr/share/doc/carteiro/README.md.
MSG

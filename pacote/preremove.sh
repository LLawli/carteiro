#!/bin/sh
set -e
# deb: "remove"/"purge"; rpm: 0 = desinstalação (1 = atualização)
case "$1" in
  remove|purge|0)
    if [ -d /run/systemd/system ]; then
      systemctl disable --now carteiro.service carteiro-certificado.path \
        carteiro-certificado.timer 2>/dev/null || true
    fi
    ;;
esac

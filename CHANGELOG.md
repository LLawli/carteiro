# Changelog

Formato [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/).

## [Unreleased]

### Adicionado

- Servidor SMTP de recebimento na porta 25 (IPv4 e IPv6) com STARTTLS,
  PIPELINING, 8BITMIME, SMTPUTF8 e SIZE (25 MiB).
- Import na caixa do Gmail por `users.messages.import`, com `INBOX`, `UNREAD`
  e a label do subendereço (`contato+x@` → `contato/x`), criada se faltar.
- Fila em disco com fsync antes do `250`, primeira tentativa dentro da sessão
  (até 20 s) e novas tentativas por até 5 dias; o que falha vai para
  `falhou/`.
- Verificação SPF, DKIM e DMARC; recusa o que falha com `p=reject`.
- Limites de conexões (total e por IP), de destinatários e de tempo; recusa
  quem fala antes do banner e HELO inválido ou com o nosso nome.
- `carteiro autorizar` (OAuth com loopback e PKCE) e `carteiro verificar`.
- Recarga do certificado sem reiniciar e units para copiar o certificado do
  Caddy.
- Métricas para o textfile collector do node-exporter.
- Unit systemd endurecida, Containerfile e `bin/build`/`bin/deploy`.

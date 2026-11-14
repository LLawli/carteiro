# Changelog

Formato [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/).

## [Unreleased]

### Adicionado

- Várias caixas: `[[caixas]]` na config, cada uma com o seu domínio, os seus
  endereços e a sua conta do Gmail (o seu `token`). Uma mensagem vai para uma
  conta só: um destinatário de outra caixa na mesma transação recebe
  `452 4.5.3` e quem envia o manda numa transação nova.
- `nunca_spam = true` numa caixa: importa com `neverMarkSpam`, e o filtro de
  spam do Gmail não tira nada dela da caixa de entrada.
- `carteiro autorizar --escutar ENDEREÇO:PORTA --retorno https://URL`: o
  login no Google feito em outro aparelho, com o retorno passando por um
  proxy reverso (cliente OAuth "Web application").

### Mudado

- `carteiro_token_ok` ganhou o label `caixa` (uma série por caixa). Um
  alerta com `max(carteiro_token_ok)` deixa de ver uma caixa quebrada ao lado
  de uma boa: use `max by (caixa) (1 - carteiro_token_ok) > 0`.
- A config de uma caixa só (`dominio`, `enderecos`, `cliente_oauth` e `token`
  no topo) continua valendo e vira a caixa `principal`.

## [0.1.0] - 2026-11-13

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
- Pacotes `.deb` e `.rpm`, tarball e sha256 por arquitetura (x86_64 e
  aarch64), publicados por tag.
- Resolvedor DNS com EDNS sempre ligado, mesmo sem `options edns0` no
  `/etc/resolv.conf` (sem ele, registros SPF grandes voltam truncados e o
  DMARC não recusaria remetente falsificado).

[Unreleased]: https://github.com/LLawli/carteiro/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/LLawli/carteiro/releases/tag/v0.1.0

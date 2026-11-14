# Arquitetura e decisões

## Por que um daemon próprio em Rust

Comparativo feito antes de escrever o código (13/11/2026), com RAM medida em
containers Debian trixie, o servidor escutando na 25 e parado (PSS somado de
todos os processos):

| Opção | RAM parada | Observação |
|---|---|---|
| Rust próprio (tokio + rustls + reqwest) | 1 MiB no esqueleto; 3,6 MiB o carteiro pronto | um processo |
| Python (aiosmtpd + httpx) | 34 MiB | passa da meta de 20 MiB só com o interpretador |
| OpenSMTPD + MDA | 18 MiB, 7 processos | o MDA roda depois do `250` e uma falha permanente vira bounce **enviado por SMTP** |
| Postfix + pipe | 7 MiB parado, mais smtpd/cleanup/anvil ao receber | mesmo problema de bounce; muita configuração |
| Stalwart 1.1.0 | 38 MiB em modo bootstrap, sem nada configurado | servidor completo para usar 5% dele |

Crates de servidor SMTP: `samotop` parado desde 2022, `mailin-embedded` sem
versão desde ago/2024 e com uma thread por conexão. O subconjunto necessário
(EHLO/HELO, STARTTLS, MAIL com SIZE, RCPT, DATA, RSET, NOOP, QUIT,
PIPELINING, 8BITMIME) ficou em `src/smtp.rs`.

## Fluxo de uma mensagem

1. **Conexão:** limite total (40) e por IP (4, por /64 no IPv6). Quem fala
   antes do banner (1 s) leva `554` e cai.
2. **EHLO/HELO:** precisa ser FQDN ou literal de IP; HELO com o nosso nome,
   domínio ou IP leva `550`.
3. **RCPT:** só os domínios das caixas e só as bases de `enderecos` de cada
   uma; o resto do domínio leva `550 5.1.1`, outro domínio `550 5.7.1`. Uma
   transação vai para uma caixa só: o destinatário de outra caixa leva
   `452 4.5.3` e volta numa transação nova. Assim cada mensagem tem uma
   conta, um `250` e um destino no spool, sem resposta parcial no DATA (que
   só o LMTP teria).
4. **DATA:** o corpo vai direto para `spool/tmp/<id>.eml`, sem passar inteiro
   pela memória. Só `CRLF.CRLF` encerra (um LF solto não começa linha), o que
   fecha o "SMTP smuggling". Passou do `SIZE`: lê até o fim, descarta, `552`.
5. **Autenticação:** SPF (IP + MAIL FROM), DKIM e DMARC com o `mail-auth`
   (o arquivo é lido por mmap). Recusa (`550 5.7.1`) só quando o domínio do
   From pede `p=reject`, nem DKIM nem SPF deram pass alinhado e nenhum dos
   dois teve erro temporário de DNS. O resultado vira um
   `Authentication-Results:`.
6. **Confirmação:** fsync do `.eml`, `.json` gravado com fsync, os dois
   renomeados para `fila/` (o `.json` por último, é o commit), fsync do
   diretório.
7. **Primeira tentativa:** numa task, com a sessão esperando até 20 s:
   - importou: `250 ... delivered as <id do Gmail>`, sai da fila;
   - o Gmail recusou a mensagem (400 `INVALID_ARGUMENT` que não fala de
     label, ou 413): `554 5.6.0`, sai da fila;
   - erro temporário ou demora: `250 ... queued as <id>`.
8. **Fila:** o trabalhador tenta de novo depois de 1, 2, 5, 10, 30 min e de
   hora em hora, até 5 dias; aí, ou se o Gmail recusar depois do `250`, a
   mensagem vai para `falhou/`.

## Por que a fila em disco (e não só a fila do remetente)

O plano original era responder `451` quando o Gmail falhasse e deixar a fila
do MTA remetente garantir a entrega. Medido o custo da fila em disco, ela
ficou:

- **custo:** a confirmação (fsync do arquivo, do `.json` e do diretório) leva
  3,9 ms numa mensagem de 10 a 100 KB e 15,5 ms numa de 25 MB, no NVMe do
  desktop. A chamada à Gmail API leva centenas de milissegundos;
- **memória:** o corpo vai para o disco enquanto chega, então a RAM não cresce
  com o tamanho da mensagem (4 mensagens de 23 MiB simultâneas: pico de
  8,9 MiB de RSS). Sem disco, cada mensagem ficaria inteira na RAM até a
  resposta do Gmail;
- **ganho:** quem manda não depende de retentar. Remetente mal configurado
  (OTP, script) que desiste no primeiro `4xx` também é entregue, e o Gmail
  fora do ar por horas não gera nenhum bounce.

A recusa do Gmail ainda chega a quem mandou quando acontece dentro dos 20 s;
depois disso, vira `falhou/` e alerta (o carteiro nunca gera bounce).

A primeira tentativa fica reservada (`em_voo`) antes de a mensagem aparecer
na fila, para o trabalhador não a pegar junto e importar duas vezes (há teste
para isso).

## Gmail API

- `users.messages.import` com `internalDateSource=dateHeader` e
  `labelIds = [INBOX, UNREAD, <label>]`: desde a mudança da API, o import sem
  `INBOX` só aparece em "Todos os e-mails". Sem `neverMarkSpam`, a não ser
  na caixa com `nunca_spam = true` (um endereço de suporte, onde perder um
  cliente no spam custa mais que ver spam na caixa de entrada).
- Até 5 MiB, `uploadType=multipart`; acima, `resumable`. O corpo sai do
  arquivo em pedaços de 64 KiB.
- Escopos: `gmail.insert` e `gmail.labels`. O token de acesso é renovado
  sozinho; `invalid_grant` derruba `carteiro_token_ok{caixa}` para 0 e as
  mensagens daquela caixa ficam na fila até um `carteiro autorizar` novo.
- Labels: `contato+x@` vira `contato/x` (o pai é criado antes). Só
  `[a-z0-9._-]`, até 32 caracteres; fora disso fica a label da base. No
  máximo 20 labels de subendereço novas por dia, contra quem quisesse encher
  a conta de labels mandando para `contato+lixo1@`, `contato+lixo2@`...
- Os filtros do Gmail provavelmente não rodam em mensagens importadas (não
  há confirmação oficial): as labels por subendereço fazem esse papel.

## O que ficou de fora de propósito

- Recusar por falta de rDNS: derruba remetente pequeno legítimo. O rDNS
  confirmado (iprev) vai para o `Received:` e o `Authentication-Results:`.
- `postmaster@`: a RFC 5321 pede, mas o carteiro só aceita o que está em
  `enderecos`. Para aceitar, basta pôr `postmaster` lá.
- AUTH, BDAT/CHUNKING, ARC, envio de qualquer tipo.

# carteiro

Para quem tem domínio próprio e lê tudo no Gmail: o carteiro é o servidor de
recebimento (MX) do domínio. Ele aceita o e-mail por SMTP e põe na caixa do
Gmail pela Gmail API (`users.messages.import`). Não encaminha por SMTP, então
não quebra SPF/DMARC e não precisa de SRS. Também **nunca envia e-mail**, então
a reputação do IP da VPS não importa. É um processo de ~6 MiB de RAM.

Nasceu para substituir o Cloudflare Email Routing num domínio pessoal. Um
carteiro atende vários domínios, cada caixa entregando na sua conta do Gmail.

- `contato+banco@exemplo.com` chega na caixa de entrada com a label
  `contato/banco` (criada se não existir).
- Só aceita os domínios e os endereços da config: nunca é relay, e não tem
  catch-all.
- O `250` só sai depois da mensagem gravada em disco. Se o Gmail responder em
  até 20 s, o `250` já traz o id do Gmail. Se o Gmail demorar ou estiver fora,
  a mensagem fica na fila e entra depois. Se o Gmail recusar a mensagem
  durante a sessão, quem mandou recebe `554` e o bounce sai do servidor dele.
- Verifica SPF, DKIM e DMARC (o Gmail não vê o IP de quem mandou) e recusa o
  que falha com `p=reject`. O filtro de spam do Gmail decide o resto.

Arquitetura e decisões em [docs/arquitetura.md](docs/arquitetura.md); consumo
medido em [docs/medicoes.md](docs/medicoes.md).

## Instalar

Linux com systemd. Pacotes de cada versão em
[Releases](https://github.com/LLawli/carteiro/releases), para x86_64 e
arm64:

```sh
# Debian 12+ / Ubuntu 22.04+
sudo apt install ./carteiro_<versão>_amd64.deb
# Fedora / RHEL
sudo dnf install ./carteiro-<versão>-1.x86_64.rpm
# do código
cargo install --locked --git https://github.com/LLawli/carteiro
```

O pacote cria o usuário `carteiro`, instala o binário em `/usr/bin`, as units
systemd e um `/etc/carteiro/carteiro.toml` para editar. Não liga nada
sozinho.

<details>
<summary>À mão, pelo tarball</summary>

```sh
tar -xzf carteiro-v<versão>-x86_64-unknown-linux-gnu.tar.gz
cd carteiro-v<versão>-x86_64-unknown-linux-gnu
sudo useradd --system --home-dir /var/lib/carteiro --shell /usr/sbin/nologin carteiro
sudo install -m 0755 carteiro /usr/local/bin/carteiro
sudo install -d -m 0755 /etc/carteiro /usr/local/lib/carteiro
sudo install -d -m 0750 -g carteiro /etc/carteiro/tls
sudo install -m 0644 config/carteiro.toml.example /etc/carteiro/carteiro.toml
sudo install -m 0755 deploy/copiar-certificado /usr/local/lib/carteiro/
sudo install -m 0644 deploy/*.service deploy/*.path deploy/*.timer /etc/systemd/system/
sudo systemctl daemon-reload
```

</details>

## Configurar

### 1. Credenciais do Google

1. Em <https://console.cloud.google.com>, crie um projeto e ative a **Gmail
   API**.
2. **Google Auth Platform**: público **External**. Em **Data access**,
   adicione os escopos `https://www.googleapis.com/auth/gmail.insert` e
   `https://www.googleapis.com/auth/gmail.labels`.
3. **Audience → Publish app**: o app precisa ficar **In production**. Em
   *Testing*, o refresh token expira em 7 dias e o carteiro para de
   entregar. Não precisa da verificação do Google: na autorização aparece o
   aviso de app não verificado, e é só seguir (*Avançado → Acessar*).
4. **Clients → Create client → Desktop app**. Baixe o JSON.
5. Numa máquina com navegador (o retorno vai para `127.0.0.1`):

   ```sh
   carteiro autorizar client_secret_*.json token.json
   ```

   Abra a URL e autorize com a conta do Gmail de destino. O `token.json`
   (0600) tem o refresh token.

   **Login em outro aparelho** (a conta é de outra pessoa): crie um client
   **Web application** com `https://mx.exemplo.com/carteiro/oauth` em
   *Authorized redirect URIs*, encaminhe esse caminho no proxy reverso para
   o carteiro e mande a URL para a pessoa:

   ```sh
   carteiro autorizar client_secret_web.json token-outra.json \
     --escutar 0.0.0.0:8765 --retorno https://mx.exemplo.com/carteiro/oauth
   ```

   ```caddy
   mx.exemplo.com {
     handle /carteiro/oauth { reverse_proxy 127.0.0.1:8765 }
     respond 404
   }
   ```

   O refresh token só vale com o client que o gerou: a caixa dessa conta usa
   o JSON do client Web em `cliente_oauth`. Tire o `handle` depois.
6. No servidor, os dois arquivos ficam em `/etc/carteiro`, `root` 0600. A unit
   os entrega ao serviço por `LoadCredential=`:

   ```sh
   sudo install -m 0600 client_secret_*.json /etc/carteiro/cliente.json
   sudo install -m 0600 token.json /etc/carteiro/token.json
   ```

### 2. Config

`/etc/carteiro/carteiro.toml`: `hostname` (o nome do MX), `dominio` e
`enderecos` (as partes locais aceitas). O resto tem padrão; ver
[config/carteiro.toml.example](config/carteiro.toml.example).

Para mais de uma conta do Gmail, troque `dominio`, `enderecos`,
`cliente_oauth` e `token` por uma `[[caixas]]` para cada conta:

```toml
[[caixas]]
nome = "eu"
dominio = "exemplo.com"
enderecos = ["contato", "pix"]
cliente_oauth = "cliente.json"
token = "token.json"

[[caixas]]
nome = "loja"
dominio = "loja.app"
enderecos = ["suporte"]
cliente_oauth = "cliente-web.json"
token = "token-loja.json"
# o filtro de spam do Gmail não mexe nesta caixa (neverMarkSpam)
nunca_spam = true
```

Cada arquivo novo entra na unit com mais um `LoadCredential=` (`systemctl
edit carteiro`). O MX de cada domínio aponta para o mesmo `hostname`. Uma
mensagem para endereços de duas caixas vira duas entregas: o segundo
destinatário recebe `452 4.5.3` e o servidor de quem enviou o manda numa
transação nova, como a RFC 5321 manda.

### 3. Certificado

O STARTTLS usa o certificado do nome do MX. Com o Caddy, basta um site
`mx.exemplo.com { respond 404 }` para ele emitir e renovar. A
`carteiro-certificado.path` copia o par renovado para `/etc/carteiro/tls`
(`root:carteiro` 0640), e o carteiro recarrega no próximo STARTTLS, sem
reiniciar. Um timer diário repete a cópia por garantia. O caminho padrão é o
do Caddy do Debian; para outro, use `systemctl edit`:

```ini
# carteiro-certificado.service
[Service]
Environment=ORIGEM=/caminho/certificates/acme-v02.api.letsencrypt.org-directory/mx.exemplo.com
Environment=NOME=mx.exemplo.com
# carteiro-certificado.path
[Path]
PathChanged=
PathChanged=/caminho/certificates/acme-v02.api.letsencrypt.org-directory/mx.exemplo.com
```

O Caddy grava a chave 0600; por isso a cópia, e não uma ACL (a ACL não
sobrevive à renovação). Sem certificado, o carteiro recebe sem STARTTLS.

### 4. Subir

```sh
sudo systemctl enable --now carteiro-certificado.path carteiro-certificado.timer
sudo systemctl start carteiro-certificado.service
sudo CREDENTIALS_DIRECTORY=/etc/carteiro carteiro verificar /etc/carteiro/carteiro.toml
sudo systemctl enable --now carteiro
```

`carteiro verificar` testa config, certificado e token, e cria as labels
base no Gmail. Se outro MTA ocupar a porta 25 (o exim4 do Debian escuta em
`127.0.0.1:25`), desligue o daemon dele: `systemctl disable --now exim4`. A
entrega local (cron) continua pelo `sendmail`.

## Testar antes de trocar o MX

Com o MX antigo no ar, nada externo chega no carteiro. Teste direto contra o
IP do servidor, de uma máquina **fora de rede residencial** (provedor
doméstico costuma bloquear a saída na porta 25):

```sh
# entrega com STARTTLS e label de subendereço
swaks -s <IP> --helo teste.exemplo.com -tls \
  -f teste@example.invalid -t contato+teste@exemplo.com --h-Subject "carteiro 1"
# relay: 550 5.7.1
swaks -s <IP> --helo teste.exemplo.com -f a@example.invalid -t alguem@gmail.com --quit-after RCPT
# endereço que não existe: 550 5.1.1
swaks -s <IP> --helo teste.exemplo.com -f a@example.invalid -t admin@exemplo.com --quit-after RCPT
# remetente falso de domínio com DMARC p=reject: 550 5.7.1
swaks -s <IP> --helo teste.exemplo.com -f service@paypal.com \
  -t contato@exemplo.com --header "From: service@paypal.com"
journalctl -u carteiro -o cat -n 20
```

O primeiro chega no Gmail, provavelmente no **spam** (remetente inventado,
sem DKIM), com a label `contato/teste` e o `Received:` do carteiro em
"Mostrar original".

## Trocar o MX

Registros **sem proxy** (MX não passa por proxy HTTP):

| Tipo | Nome | Valor |
|---|---|---|
| A | `mx` | IPv4 do servidor |
| AAAA | `mx` | IPv6 do servidor |
| MX | `@` | `mx.exemplo.com`, prioridade 10, TTL curto na troca |
| TXT | `@` | o SPF só de quem **envia** pelo domínio (ex.: `v=spf1 include:_spf.google.com ~all`); o carteiro não envia nada |

Vindo do Cloudflare Email Routing: dá para criar o MX 10 antes de desligar o
Email Routing (os MX dele têm prioridade maior, então ficam de reserva).
Desligá-lo apaga os MX, o SPF e o DKIM que ele criou.

Depois, mande de outra conta e acompanhe `journalctl -u carteiro -f`. De um
Gmail, a linha mostra `spf=pass dkim_alinhado=pass dmarc=pass`.

**Voltar:** apague o MX novo e religue o encaminhamento antigo. Deixe o
carteiro no ar até o TTL acabar; o que estiver na fila ainda entra.

## Operação

- **Log:** uma linha por evento no journald (`evento=recebida de=...
  para=... tamanho=... resultado=importada|fila|recusada_gmail gmail_id=...
  autenticacao="spf=... dmarc=..."`). Nada do corpo.
- **Métricas:** `metricas = "/var/lib/node-exporter/carteiro.prom"` grava a
  cada 30 s para o textfile collector do node-exporter. Vale alertar em
  `carteiro_token_ok{caixa="..."} == 0` (rode `carteiro autorizar` de novo),
  `carteiro_fila_idade_segundos > 3600` e `carteiro_falhou_mensagens > 0`.
- **Fila:** `/var/lib/carteiro/fila/<id>.{eml,json}`. Tentativas depois de
  1, 2, 5, 10 e 30 min, depois de hora em hora; após 5 dias a mensagem vai
  para `falhou/` (o carteiro não manda bounce: isso seria enviar e-mail).
  Para reenfileirar, mova o par de volta para `fila/` com `tentativas` e
  `proxima` zerados e reinicie.

## Contribuir

[CONTRIBUTING.md](CONTRIBUTING.md): gates, testes e release. Falhas de
segurança: [SECURITY.md](SECURITY.md).

## Licença

[GPL-3.0-or-later](LICENSE).

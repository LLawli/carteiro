# carteiro

Recebe o e-mail de um domínio por SMTP e põe na caixa do Gmail pela Gmail API
(`users.messages.import`), sem encaminhar por SMTP. Substitui o Cloudflare
Email Routing de `lukakuuhaku.dev` (`contato@` e `pix@`, com qualquer
`+sufixo`) por um processo de ~5 MiB na VPS que **nunca envia e-mail**.

- `contato+banco@` chega na caixa de entrada com a label `contato/banco`
  (criada se não existir); `pix@` com a label `pix`.
- Só aceita `@lukakuuhaku.dev` e só os endereços da config (nunca é relay).
- O `250` sai depois da mensagem gravada em disco (fsync). Se o Gmail
  responder em até 20 s, o `250` já traz o id do Gmail; se demorar ou estiver
  fora, a mensagem fica na fila e entra depois. Se o Gmail recusar a mensagem
  durante a sessão, quem mandou recebe `554` e o bounce sai do servidor dele.
- Verifica SPF, DKIM e DMARC (o Gmail não vê o IP de quem mandou) e recusa o
  que falha com `p=reject`. O filtro de spam do Gmail continua decidindo o
  resto (sem `neverMarkSpam`).

Arquitetura e decisões em [docs/arquitetura.md](docs/arquitetura.md); consumo
medido em [docs/medicoes.md](docs/medicoes.md).

## 1. Credenciais do Google (uma vez)

1. Em <https://console.cloud.google.com>, crie um projeto e ative a **Gmail
   API**.
2. **Google Auth Platform → Branding/Audience**: tipo **External**, com o seu
   Gmail como usuário. Em **Data access**, adicione os escopos
   `https://www.googleapis.com/auth/gmail.insert` e
   `https://www.googleapis.com/auth/gmail.labels`.
3. **Audience → Publish app**: o app precisa ficar **In production**. Em
   *Testing*, o refresh token expira em 7 dias e o carteiro para de entregar.
   Não precisa passar pela verificação do Google: na autorização aparece o
   aviso de app não verificado, e é só seguir (*Avançado → Acessar*).
4. **Clients → Create client → Desktop app**. Baixe o JSON.
5. No desktop (precisa de navegador):

   ```sh
   bin/build
   dist/carteiro autorizar ~/Downloads/client_secret_*.json token.json
   ```

   Abra a URL, autorize com a conta do Gmail de destino. O `token.json` (0600)
   tem o refresh token. Os dois arquivos vão para o vault do repositório
   `vps` (ver passo 2) e nunca para este repositório.

## 2. Instalar na VPS

O role `carteiro` do repositório `vps` instala tudo: usuário `carteiro`,
binário, `/etc/carteiro/carteiro.toml`, os segredos (`root` 0600, entregues
ao serviço por `LoadCredential=`), as units e a porta 25 no ufw:

```sh
bin/build                       # dist/carteiro, compilado para Debian trixie
cd ../vps
ansible-vault edit inventory/group_vars/all/vault.yml   # carteiro_cliente_oauth, carteiro_refresh_token
ansible-playbook playbooks/site.yml --tags carteiro
```

Sem Ansible, o equivalente à mão:

```sh
sudo useradd --system --home-dir /var/lib/carteiro --shell /usr/sbin/nologin carteiro
sudo install -m 0755 dist/carteiro /usr/local/bin/carteiro
sudo install -d -m 0755 /etc/carteiro /usr/local/lib/carteiro
sudo install -m 0644 config/carteiro.toml.example /etc/carteiro/carteiro.toml   # e edite
sudo install -m 0600 client_secret.json /etc/carteiro/cliente.json
sudo install -m 0600 token.json /etc/carteiro/token.json
sudo install -m 0755 deploy/copiar-certificado /usr/local/lib/carteiro/
sudo install -m 0644 deploy/*.service deploy/*.path deploy/*.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now carteiro-certificado.path carteiro-certificado.timer
sudo systemctl start carteiro-certificado.service
sudo CREDENTIALS_DIRECTORY=/etc/carteiro carteiro verificar /etc/carteiro/carteiro.toml
sudo systemctl enable --now carteiro
```

O certificado é o do Caddy: um site `mx.lukakuuhaku.dev` no Caddyfile faz ele
emitir e renovar. A unit `carteiro-certificado.path` copia o par renovado para
`/etc/carteiro/tls` (`root:carteiro` 0640) e o carteiro recarrega no próximo
STARTTLS, sem reiniciar. Um timer diário repete a cópia por garantia.

`carteiro verificar` testa config, certificado, spool e token, e cria as
labels `contato` e `pix`. Fora da unit, `CREDENTIALS_DIRECTORY=/etc/carteiro`
faz os caminhos relativos dos segredos acharem os arquivos.

Atualizar só o binário depois: `bin/deploy` (lê `config/deploy.env`).

## 3. Testar antes de trocar o MX

Com o MX ainda na Cloudflare, nada externo chega na VPS. Teste direto contra
o IP dela (da própria VPS, porque provedor residencial costuma bloquear a
saída na porta 25):

```sh
ssh vps
sudo apt install swaks
# entrega com STARTTLS e label de subendereço
swaks -s 62.83.47.20 --helo teste.lukakuuhaku.dev -tls \
  -f teste@example.invalid -t contato+teste@lukakuuhaku.dev --h-Subject "carteiro 1"
# relay: tem de dar 550 5.7.1
swaks -s 62.83.47.20 --helo teste.lukakuuhaku.dev -f a@example.invalid -t alguem@gmail.com --quit-after RCPT
# endereço que não existe: 550 5.1.1
swaks -s 62.83.47.20 --helo teste.lukakuuhaku.dev -f a@example.invalid -t admin@lukakuuhaku.dev --quit-after RCPT
# remetente falso de domínio com DMARC p=reject: 550 5.7.1
swaks -s 62.83.47.20 --helo teste.lukakuuhaku.dev -f service@paypal.com \
  -t pix@lukakuuhaku.dev --header "From: service@paypal.com"
journalctl -u carteiro -o cat -n 20
```

O primeiro tem de aparecer na caixa de entrada do Gmail com a label
`contato/teste`, com `Received:` e `Authentication-Results:` do
`mx.lukakuuhaku.dev` no "Mostrar original".

## 4. Trocar o MX

Registros de `lukakuuhaku.dev` (todos **DNS only**, sem o proxy laranja: MX
não passa pelo proxy da Cloudflare):

| Ação | Tipo | Nome | Valor |
|---|---|---|---|
| criar | A | `mx` | `62.83.47.20` |
| criar | AAAA | `mx` | `2a0a:4cc0:60:5522::1` |
| desligar | Email Routing | | *Email → Email Routing → Settings → Disable*: tira os três MX `route{1,2,3}.mx.cloudflare.net` |
| criar | MX | `@` | `mx.lukakuuhaku.dev`, prioridade 10, TTL 300 na troca |
| trocar | TXT | `@` | de `v=spf1 include:_spf.mx.cloudflare.net ~all` para `v=spf1 include:_spf.google.com ~all` |
| apagar | TXT | `cf2024-1._domainkey` | DKIM do Email Routing, depois que tudo estiver estável |
| manter | TXT | `_dmarc` | `p=none` com relatório na Cloudflare: não depende do Email Routing |

Hoje o `mx` já resolve pelo curinga `*.lukakuuhaku.dev`; o registro explícito
deixa o MX independente dele. O SPF de hoje **não** inclui o Google: o que
você envia pelo SMTP do Google já sai com SPF `softfail`, e o SPF novo corrige
isso.

Depois da troca, mande de outra conta (Outlook, outro Gmail) para `contato@`
e `pix+algo@` e acompanhe `journalctl -u carteiro -f`. Volte o TTL do MX para
o automático depois de um dia.

Opcional: o PTR do IPv4 é o padrão da netcup
(`v2202610443087544551.luckysrv.de`). Para receber isso não importa; só pesa
para quem envia.

## 5. Voltar para a Cloudflare

1. Apague o MX `mx.lukakuuhaku.dev`.
2. *Email Routing → Enable*: a Cloudflare recria os três MX e oferece o SPF
   dela (mantenha o `include:_spf.google.com` junto).
3. Deixe o carteiro no ar até o TTL acabar: quem ainda resolve o MX antigo
   continua entregando nele. Depois, `systemctl disable --now carteiro`.

O que estiver em `/var/lib/carteiro/fila` ainda entra no Gmail enquanto o
serviço estiver de pé.

## Operação

- **Log:** uma linha por evento no journald (`evento=recebida de=... para=...
  tamanho=... resultado=importada|fila|recusada_gmail gmail_id=...
  autenticacao="spf=... dmarc=..."`). Nada do corpo.
- **Métricas:** `/var/lib/node-exporter/carteiro.prom` a cada 30 s, para o
  textfile collector do node-exporter. Alertar em `carteiro_token_ok == 0`
  (refresh token revogado: rode `carteiro autorizar` de novo),
  `carteiro_fila_idade_segundos > 3600` e `carteiro_falhou_mensagens > 0`.
- **Fila:** `/var/lib/carteiro/fila/<id>.{eml,json}`. Tentativas depois de 1,
  2, 5, 10 e 30 min e depois de hora em hora; após 5 dias a mensagem vai para
  `falhou/` (o carteiro não manda bounce: isso seria enviar e-mail).
- **`falhou/`:** o `.json` diz o erro. Para reenfileirar, mova o par de volta
  para `fila/` com `tentativas` e `proxima` zerados e reinicie o serviço.

## Desenvolvimento

```sh
scripts/ci.sh      # fmt, clippy -D warnings, testes, cargo audit, systemd-analyze verify
```

Os testes de ponta a ponta (`tests/ponta_a_ponta.rs`) sobem o carteiro de
verdade contra um mock da Gmail API e do OAuth e conversam por SMTP, com
STARTTLS.

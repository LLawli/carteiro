# Alertas

PromQL em [regras.yml](regras.yml), testada com
`promtool test rules testes.yml` (no container `prom/prometheus`). Cada
expressão devolve um valor positivo quando há problema e nada quando está
tudo bem: no Grafana, condição `A > 0` e "sem dados" = OK. As métricas vêm
do arquivo `metricas` da config, lido pelo textfile collector do
node-exporter.

## Carteiro parado

O `carteiro.prom` não é regravado há mais de 3 min (o carteiro o regrava a
cada 30 s), ou sumiu. Enquanto isso, ninguém recebe na porta 25: quem envia
guarda a mensagem e tenta de novo por dias, então nada se perde se for
resolvido em horas.

```sh
systemctl status carteiro
journalctl -u carteiro -n 50 -o cat
```

`evento=fatal` diz o motivo (porta 25 ocupada por outro MTA, config
inválida, credencial faltando). Se o carteiro está de pé, o problema é o
node-exporter ou o `textfile.directory` dele.

## Token do Gmail recusado

O Google recusou o refresh token (`invalid_grant`): senha da conta trocada,
acesso do app revogado, ou o app voltou para *Testing* (token de 7 dias).
O carteiro continua aceitando e guardando na fila, sem importar.

1. Confira em *Google Auth Platform → Audience* que o app está **In
   production**.
2. Numa máquina com navegador: `carteiro autorizar client_secret.json token.json`.
3. Troque o `/etc/carteiro/token.json` (root 0600) e reinicie o carteiro. A
   fila entra sozinha nas tentativas seguintes.

## Fila parada

Há mensagem esperando na fila há mais de 1 h: o Gmail está recusando
temporariamente (5xx, 429, 403) ou não está acessível.

```sh
journalctl -u carteiro -o cat | grep import_adiado | tail
sudo ls /var/lib/carteiro/fila
```

O campo `erro=` mostra a resposta da API. Depois de 5 dias sem conseguir, a
mensagem vai para `falhou/` e dispara o alerta abaixo.

## Mensagem em falhou/

Uma mensagem não vai mais ser importada sozinha: o Gmail a recusou depois do
`250` (mensagem inválida) ou ela passou de 5 dias na fila. O carteiro não
manda bounce, então quem enviou acha que entregou.

```sh
sudo ls /var/lib/carteiro/falhou
sudo cat /var/lib/carteiro/falhou/<id>.json   # remetente, destinatários, ultimo_erro
```

Para tentar de novo, mova o par `<id>.eml` e `<id>.json` para `fila/`, zere
`tentativas` e `proxima` no `.json` e reinicie o carteiro. Se não for
importável, apague o par (o alerta resolve sozinho) e, se importar, avise
quem mandou.

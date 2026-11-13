# Medições

## Carteiro na VM (13/11/2026)

VM QEMU/KVM com Debian 13 (genericcloud, systemd 257), 1 GiB de RAM e 2
vCPUs, rodando a unit endurecida de `deploy/carteiro.service` com a config de
produção (STARTTLS, verificação SPF/DKIM/DMARC ligada, DNS real) contra um
mock local da Gmail API. Carga gerada na própria VM com `smtplib`, uma
conexão por mensagem, sempre com STARTTLS.

| Situação | RSS | PSS | anon do cgroup | CPU |
|---|---|---|---|---|
| Parado, 60 s depois de subir | 5,4 MiB | 3,6 MiB | 0,4 MiB | 1,5 ms por minuto |
| 500 × 50 KiB, 10 conexões paralelas | pico 8,9 MiB | 6,7 MiB | 3,2 MiB | 0,99 s no total (~2 ms por mensagem); 87 mensagens/s |
| 4 × 23 MiB simultâneas | pico 8,9 MiB | 6,7 MiB | 3,2 MiB | 0,42 s para 92 MiB |

O `memory.peak` do cgroup chegou a 49 MiB nas mensagens grandes: é page
cache dos arquivos do spool (descartável, o kernel recupera), não memória do
processo. O `MemoryHigh=48M` da unit só faz o kernel recuperar esse cache
mais cedo.

### Custo da verificação SPF/DKIM/DMARC

Mesma VM, mesma carga, `verificar_autenticacao = false`:

| | ligada | desligada |
|---|---|---|
| RSS parado | 5,4 MiB | 5,4 MiB |
| 500 × 50 KiB | 87 msg/s, 0,99 s de CPU | 96 msg/s, 0,95 s de CPU |

Uns 9% de vazão (consultas DNS) e nenhuma RAM: fica ligada.

### Unit

`systemd-analyze security carteiro`: exposição **1.4 OK**.

## Custo do fsync do spool

`fsync` do `.eml`, `.json` com fsync, dois renames e fsync do diretório,
medido no NVMe do desktop (LVM, ext4), mediana:

| Tamanho | sem fsync | com fsync |
|---|---|---|
| 10 KB | 0,03 ms | 3,85 ms |
| 100 KB | 0,06 ms | 3,90 ms |
| 1 MB | 0,11 ms | 5,00 ms |
| 25 MB | 0,19 ms | 15,50 ms |

No disco da netcup o número absoluto muda; a ordem de grandeza (milissegundos
contra as centenas da Gmail API) não.

## Alternativas (Fase 1)

Containers Debian trixie, servidor escutando, sem conexões, PSS somado:
Rust (esqueleto tokio + rustls + reqwest) 1 MiB; Python 3.13 com aiosmtpd e
httpx 34 MiB; OpenSMTPD 18 MiB em 7 processos; Postfix 7 MiB em 3 processos
(antes de abrir smtpd, cleanup e anvil); Stalwart 1.1.0 38 MiB em modo
bootstrap.

## Como repetir

Os scripts ficaram fora do repositório (sessão de 13/11/2026). A receita: a
VM em `~/vms/carteiro` (ver o `LEIA.md` de lá), o mock da Gmail API
(`ThreadingHTTPServer` respondendo `/token`, `/gmail/v1/users/me/labels` e
`/upload/.../messages/import`), e a leitura de `memory.current`,
`memory.stat` e `cpu.stat` do cgroup `system.slice/carteiro.service` mais
`VmRSS`/`VmHWM` e `Pss` (`/proc/<pid>/smaps_rollup`).

# Instruções para agentes

Receptor SMTP em Rust que importa no Gmail pela API. Leia o README (o quê e
como instalar), `docs/arquitetura.md` (por quê) e `CONTRIBUTING.md` (gates e
release) antes de mudar algo.

- **Gates:** `scripts/ci.sh`. Rode antes de dar algo por pronto.
- **Build para servidor:** `bin/build` (Debian trixie, num container); não
  use o binário compilado no host para outra distro.
- **Pacotes:** `scripts/empacotar.sh`; o release (`.github/workflows/release.yml`)
  usa o mesmo script.
- **Release:** `cargo set-version`, CHANGELOG (`[Unreleased]` → versão com
  data), tag `v<versão>`. Detalhes em `CONTRIBUTING.md`.
- **CHANGELOG:** toda mudança de comportamento visível entra em
  `## [Unreleased]` no mesmo commit.
- **Invariantes:** nunca enviar e-mail (nem bounce); nunca aceitar
  destinatário fora da config; `250` só depois do fsync; erro de DNS nunca
  recusa mensagem; nada do corpo no log.
- Unit systemd e pacotes se testam em VM, não em Docker.

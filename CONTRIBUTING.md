# Contribuir

## Gates

```sh
scripts/ci.sh
```

Roda `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test`, `cargo audit` e `systemd-analyze verify` da unit. O CI roda o
mesmo script em todo push e PR: verde aqui é verde lá.

Os testes de ponta a ponta (`tests/ponta_a_ponta.rs`) sobem o carteiro de
verdade contra um mock da Gmail API e do OAuth e conversam por SMTP, com
STARTTLS. Mudança de comportamento vem com teste; correção de bug, com o
teste que falhava antes.

## O que testar fora do `cargo test`

- **Unit systemd e pacote:** numa VM com systemd (Debian 13 cloud, por
  exemplo). systemd dentro de Docker não serve: o `LoadCredential=` não
  funciona lá.
- **SPF/DKIM/DMARC com DNS real:** `cargo run --example autenticar -- <ip>
  <helo> <remetente>` mostra o que o carteiro decidiria. Rode no servidor de
  destino: o resolvedor de lá importa (ver o comentário sobre EDNS em
  `src/autenticacao.rs`).
- **Porta 25 de fora:** de uma máquina fora de rede residencial.

## Compilar e empacotar

```sh
bin/build                      # dist/carteiro para Debian trixie, num container
scripts/empacotar.sh <binário> <versão> <alvo>   # tarball, .deb, .rpm e sha256 (precisa do nfpm)
```

## Release

1. `cargo set-version <versão>` (do cargo-edit).
2. Em `CHANGELOG.md`, renomeie `## [Unreleased]` para
   `## [<versão>] - <AAAA-MM-DD>` e abra um `## [Unreleased]` vazio em cima.
3. Commit, e a tag: `git tag -a v<versão> -m v<versão>` e
   `git push origin v<versão>`.

O workflow `release` compila em x86_64 e aarch64, testa, empacota e publica
o release com a seção da versão no CHANGELOG, as instruções de instalação e
os sha256. A tag precisa bater com a versão do `Cargo.toml`, ou o workflow
para.

## Revisão de PR

Julga-se o diff, não a descrição: regressões, testes que cobrem a mudança,
docs que acompanham. Depois de várias mudanças: procurar código morto,
duplicação, valores mágicos sem nome e testes faltando.

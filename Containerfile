# Opcional: o caminho recomendado é o binário com a unit systemd
# (deploy/carteiro.service). Em container, publique 25:2525 e confira que o
# carteiro vê o IP real de quem conecta (no log, campo ip=): onde o
# userland-proxy do Docker atende (por exemplo IPv6 sem ip6tables), ele vê o
# IP do bridge, o que estraga SPF/DMARC e o limite por IP.
FROM docker.io/library/rust:1.99-slim-trixie AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked

FROM docker.io/library/debian:trixie-slim
RUN useradd --system --uid 10025 --home-dir /var/lib/carteiro carteiro \
 && install -d -o carteiro -g carteiro -m 0700 /var/lib/carteiro
COPY --from=build /src/target/release/carteiro /usr/local/bin/carteiro
USER carteiro
EXPOSE 2525
# monte a config em /etc/carteiro (com escutar = ["0.0.0.0:2525", "[::]:2525"])
# e os segredos em /run/secrets, com cliente_oauth/token absolutos
ENTRYPOINT ["/usr/local/bin/carteiro"]
CMD ["servir", "/etc/carteiro/carteiro.toml"]

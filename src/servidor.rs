//! Sobe tudo: spool, cliente do Gmail, trabalhador da fila, métricas e os
//! sockets, com limite de conexões no total e por IP (por /64 no IPv6).

use crate::autenticacao::Verificador;
use crate::config::Config;
use crate::entrega::{Contas, Entrega};
use crate::gmail::{self, ClienteOAuth, Gmail};
use crate::log;
use crate::metricas::{self, Metricas, somar};
use crate::smtp::{self, Contexto};
use crate::spool::Spool;
use crate::tls::Tls;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinSet;

pub struct Servidor {
    pub enderecos: Vec<SocketAddr>,
    pub contexto: Arc<Contexto>,
    parar: watch::Sender<bool>,
    tarefas: JoinSet<()>,
    conexoes: Arc<Semaphore>,
    maximo: usize,
}

/// Chave do limite por IP: o endereço no IPv4, o /64 no IPv6.
fn chave_ip(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

struct VagaIp {
    chave: IpAddr,
    mapa: Arc<Mutex<HashMap<IpAddr, usize>>>,
}

impl Drop for VagaIp {
    fn drop(&mut self) {
        let mut m = self.mapa.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = m.get_mut(&self.chave) {
            *n -= 1;
            if *n == 0 {
                m.remove(&self.chave);
            }
        }
    }
}

fn abrir_socket(endereco: SocketAddr) -> std::io::Result<TcpListener> {
    let s = if endereco.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    s.set_reuseaddr(true)?;
    if endereco.is_ipv6() {
        // [::]:25 e 0.0.0.0:25 lado a lado
        let s2 = socket2::SockRef::from(&s);
        s2.set_only_v6(true)?;
    }
    s.bind(endereco)?;
    s.listen(128)
}

pub async fn iniciar(config: Config) -> Result<Servidor, String> {
    let spool = Arc::new(
        Spool::abrir(&config.spool)
            .map_err(|e| format!("spool {}: {e}", config.spool.display()))?,
    );
    let http = gmail::http();
    let mut contas: Contas = Vec::new();
    for c in &config.caixas {
        let erro = |e: String| format!("caixa {}: {e}", c.nome);
        let cliente = ClienteOAuth::ler(&c.cliente_oauth).map_err(erro)?;
        let refresh = gmail::ler_refresh_token(&c.token).map_err(erro)?;
        contas.push((
            c.nome.clone(),
            Arc::new(
                Gmail::new(
                    http.clone(),
                    &config.api_gmail,
                    &config.api_token,
                    cliente,
                    refresh,
                    config.labels_novas_por_dia,
                )
                .com_nunca_spam(c.nunca_spam),
            ),
        ));
    }
    let metricas = Arc::new(Metricas::default());
    let entrega = Arc::new(Entrega::new(
        Arc::clone(&spool),
        contas.clone(),
        Arc::clone(&metricas),
        config.desistir_apos(),
        config.espera_retentativa_s,
    ));
    let tls = match (&config.certificado, &config.chave) {
        (Some(c), Some(k)) => {
            let t = Tls::new(c.clone(), k.clone());
            if t.aceitador().is_none() {
                // sobe mesmo assim: receber sem TLS é melhor que não receber
                log::erro(
                    "sem_tls",
                    &[(
                        "detalhe",
                        "certificado ilegível; STARTTLS desligado até ele aparecer",
                    )],
                );
            }
            Some(t)
        }
        _ => None,
    };
    let verificador = if config.verificar_autenticacao {
        Some(Verificador::new(&config.hostname)?)
    } else {
        None
    };

    let mut ouvintes = Vec::new();
    for e in &config.escutar {
        ouvintes.push(abrir_socket(*e).map_err(|x| format!("escutando em {e}: {x}"))?);
    }
    let enderecos: Vec<SocketAddr> = ouvintes
        .iter()
        .filter_map(|o| o.local_addr().ok())
        .collect();
    let maximo = config.conexoes_maximas;
    let por_ip = config.conexoes_por_ip;
    let metricas_arquivo = config.metricas.clone();
    let contexto = Arc::new(Contexto {
        config,
        tls,
        entrega: Arc::clone(&entrega),
        verificador,
    });

    let (parar, parado) = watch::channel(false);
    let conexoes = Arc::new(Semaphore::new(maximo));
    let por_ip_mapa: Arc<Mutex<HashMap<IpAddr, usize>>> = Arc::default();
    let mut tarefas = JoinSet::new();

    tarefas.spawn(Arc::clone(&entrega).trabalhar());
    if let Some(caminho) = metricas_arquivo {
        tarefas.spawn(metricas::gravar_sempre(
            caminho,
            Arc::clone(&metricas),
            Arc::clone(&spool),
            contas,
        ));
    }
    for ouvinte in ouvintes {
        let ctx = Arc::clone(&contexto);
        let conexoes = Arc::clone(&conexoes);
        let mapa = Arc::clone(&por_ip_mapa);
        let mut parado = parado.clone();
        tarefas.spawn(async move {
            loop {
                let (tcp, remoto) = tokio::select! {
                    r = ouvinte.accept() => match r {
                        Ok(x) => x,
                        Err(e) => {
                            log::aviso("accept", &[("erro", &e.to_string())]);
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                    _ = parado.changed() => return,
                };
                let _ = tcp.set_nodelay(true);
                let Ok(vaga) = Arc::clone(&conexoes).try_acquire_owned() else {
                    somar(&ctx.entrega.metricas.recusadas_protocolo);
                    tokio::spawn(async move {
                        let mut tcp = tcp;
                        let _ = tcp
                            .write_all(b"421 4.3.2 Too many connections, try again later\r\n")
                            .await;
                    });
                    continue;
                };
                let chave = chave_ip(remoto.ip());
                {
                    let mut m = mapa.lock().unwrap_or_else(|e| e.into_inner());
                    let n = m.entry(chave).or_insert(0);
                    if *n >= por_ip {
                        drop(m);
                        somar(&ctx.entrega.metricas.recusadas_protocolo);
                        tokio::spawn(async move {
                            let mut tcp = tcp;
                            let _ = tcp
                                .write_all(b"421 4.7.0 Too many connections from your address\r\n")
                                .await;
                        });
                        continue;
                    }
                    *n += 1;
                }
                let vaga_ip = VagaIp {
                    chave,
                    mapa: Arc::clone(&mapa),
                };
                let ctx = Arc::clone(&ctx);
                tokio::spawn(async move {
                    smtp::atender(tcp, remoto, ctx).await;
                    drop(vaga_ip);
                    drop(vaga);
                });
            }
        });
    }
    log::info(
        "iniciado",
        &[
            ("versao", env!("CARGO_PKG_VERSION")),
            (
                "escutando",
                &enderecos
                    .iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            ("fila", &spool.estatisticas().fila.to_string()),
            (
                "caixas",
                &contexto
                    .config
                    .caixas
                    .iter()
                    .map(|c| c.nome.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ),
        ],
    );
    Ok(Servidor {
        enderecos,
        contexto,
        parar,
        tarefas,
        conexoes,
        maximo,
    })
}

impl Servidor {
    /// Para de aceitar e espera as sessões abertas terminarem, por até
    /// `prazo`. O que já recebeu `250` está no spool e volta na próxima subida.
    pub async fn parar(mut self, prazo: std::time::Duration) {
        let _ = self.parar.send(true);
        let todas = self.conexoes.acquire_many(self.maximo as u32);
        let _ = tokio::time::timeout(prazo, todas).await;
        self.tarefas.abort_all();
        log::info("parado", &[]);
    }
}

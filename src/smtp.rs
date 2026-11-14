//! Uma sessão SMTP de recebimento (RFC 5321): EHLO/HELO, STARTTLS, MAIL,
//! RCPT, DATA, RSET, NOOP, QUIT. Sem AUTH, sem BDAT, sem relay.

use crate::autenticacao::Verificador;
use crate::config::Config;
use crate::endereco::{self, Destino, Recusa};
use crate::entrega::{Entrega, Resultado};
use crate::log;
use crate::metricas::somar;
use crate::spool::{Meta, Spool};
use crate::tls::Tls;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout, timeout_at};
use tokio_rustls::server::TlsStream;

/// Linha de comando: 512 da RFC mais folga para parâmetros ESMTP.
const LINHA_MAXIMA: usize = 2048;
const ERROS_MAXIMOS: u32 = 10;
const TEMPO_HANDSHAKE: Duration = Duration::from_secs(30);
const PEDACO: usize = 64 * 1024;

pub struct Contexto {
    pub config: Config,
    pub tls: Option<Tls>,
    pub entrega: Arc<Entrega>,
    pub verificador: Option<Verificador>,
}

enum Fluxo {
    Tcp(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    Fechado,
}

impl Fluxo {
    async fn ler(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Fluxo::Tcp(s) => s.read(buf).await,
            Fluxo::Tls(s) => s.read(buf).await,
            Fluxo::Fechado => Ok(0),
        }
    }

    async fn escrever(&mut self, dados: &[u8]) -> io::Result<()> {
        match self {
            Fluxo::Tcp(s) => s.write_all(dados).await,
            Fluxo::Tls(s) => {
                s.write_all(dados).await?;
                s.flush().await
            }
            Fluxo::Fechado => Err(io::ErrorKind::NotConnected.into()),
        }
    }
}

#[derive(Debug)]
enum ErroLeitura {
    Fechou,
    Tempo,
    Longa,
    Io,
}

/// Buffer de leitura próprio: o DATA e os comandos dividem os mesmos bytes
/// (PIPELINING), e o STARTTLS precisa saber se sobrou algo nele.
struct Leitor {
    buf: Vec<u8>,
    inicio: usize,
}

impl Leitor {
    fn new() -> Leitor {
        Leitor {
            buf: Vec::with_capacity(4096),
            inicio: 0,
        }
    }

    fn pendente(&self) -> &[u8] {
        &self.buf[self.inicio..]
    }

    fn consumir(&mut self, n: usize) {
        self.inicio += n;
        if self.inicio == self.buf.len() {
            self.buf.clear();
            self.inicio = 0;
        }
    }

    async fn encher(&mut self, f: &mut Fluxo, prazo: Instant) -> Result<(), ErroLeitura> {
        if self.inicio > 0 {
            self.buf.drain(..self.inicio);
            self.inicio = 0;
        }
        let tam = self.buf.len();
        self.buf.resize(tam + PEDACO, 0);
        let r = timeout_at(prazo, f.ler(&mut self.buf[tam..])).await;
        let n = match r {
            Err(_) => {
                self.buf.truncate(tam);
                return Err(ErroLeitura::Tempo);
            }
            Ok(Err(_)) => {
                self.buf.truncate(tam);
                return Err(ErroLeitura::Io);
            }
            Ok(Ok(n)) => n,
        };
        self.buf.truncate(tam + n);
        if n == 0 {
            return Err(ErroLeitura::Fechou);
        }
        Ok(())
    }

    /// Uma linha de comando, sem o CRLF (ou LF).
    async fn linha(&mut self, f: &mut Fluxo, prazo: Instant) -> Result<String, ErroLeitura> {
        loop {
            if let Some(p) = self.pendente().iter().position(|&b| b == b'\n') {
                let mut l = &self.pendente()[..p];
                if l.last() == Some(&b'\r') {
                    l = &l[..l.len() - 1];
                }
                let s = String::from_utf8_lossy(l).into_owned();
                self.consumir(p + 1);
                return Ok(s);
            }
            if self.pendente().len() > LINHA_MAXIMA {
                return Err(ErroLeitura::Longa);
            }
            self.encher(f, prazo).await?;
        }
    }
}

/// Fim do DATA e desfazer o ponto duplicado (RFC 5321 4.5.2). Só
/// `CRLF . CRLF` termina: um LF solto nunca começa linha para esse fim, o que
/// fecha a porta do "SMTP smuggling".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EstadoData {
    InicioLinha,
    Ponto,
    PontoCr,
    Meio,
    Cr,
}

struct Desempacotador {
    estado: EstadoData,
}

impl Desempacotador {
    fn new() -> Self {
        Desempacotador {
            estado: EstadoData::InicioLinha,
        }
    }

    /// Processa `entrada`, empurrando o conteúdo em `saida`. Devolve quantos
    /// bytes consumiu e se achou o fim.
    fn processar(&mut self, entrada: &[u8], saida: &mut Vec<u8>) -> (usize, bool) {
        use EstadoData::*;
        for (i, &b) in entrada.iter().enumerate() {
            self.estado = match (self.estado, b) {
                (InicioLinha, b'.') => Ponto,
                (Ponto, b'\r') => PontoCr,
                (Ponto, b'.') => {
                    saida.push(b'.');
                    Meio
                }
                (PontoCr, b'\n') => return (i + 1, true),
                (PontoCr, b'\r') => {
                    saida.extend_from_slice(b".\r");
                    Cr
                }
                (PontoCr, x) => {
                    saida.extend_from_slice(b".\r");
                    saida.push(x);
                    Meio
                }
                (Ponto, x) => {
                    saida.push(b'.');
                    saida.push(x);
                    Meio
                }
                (Cr, b'\n') => {
                    saida.push(b'\n');
                    InicioLinha
                }
                (_, b'\r') => {
                    saida.push(b'\r');
                    Cr
                }
                (_, x) => {
                    saida.push(x);
                    Meio
                }
            };
        }
        (entrada.len(), false)
    }
}

struct Transacao {
    de: String,
    destinos: Vec<Destino>,
}

struct Sessao<'a> {
    ctx: &'a Contexto,
    fluxo: Fluxo,
    leitor: Leitor,
    ip: IpAddr,
    local: SocketAddr,
    helo: Option<String>,
    tls: Option<String>,
    transacao: Option<Transacao>,
    erros: u32,
}

/// Atende uma conexão até o QUIT, um erro ou o tempo acabar.
pub async fn atender(tcp: TcpStream, remoto: SocketAddr, ctx: Arc<Contexto>) {
    let ip = remoto.ip().to_canonical();
    let local = match tcp.local_addr() {
        Ok(l) => l,
        Err(_) => return,
    };
    somar(&ctx.entrega.metricas.conexoes);
    let ipt = ip.to_string();

    // Quem fala antes do banner é robô de spam que não espera o servidor.
    let espera = ctx.config.espera_banner();
    if !espera.is_zero() {
        let mut b = [0u8; 1];
        if let Ok(Ok(n)) = timeout(espera, tcp.peek(&mut b)).await
            && n > 0
        {
            somar(&ctx.entrega.metricas.recusadas_protocolo);
            log::info(
                "recusada",
                &[("ip", &ipt), ("motivo", "falou antes do banner")],
            );
            let mut tcp = tcp;
            let _ = tcp
                .write_all(b"554 5.5.0 Protocol violation: talked before greeting\r\n")
                .await;
            return;
        }
    }

    let mut s = Sessao {
        ctx: &ctx,
        fluxo: Fluxo::Tcp(tcp),
        leitor: Leitor::new(),
        ip,
        local,
        helo: None,
        tls: None,
        transacao: None,
        erros: 0,
    };
    let banner = format!("220 {} ESMTP carteiro\r\n", ctx.config.hostname);
    if s.fluxo.escrever(banner.as_bytes()).await.is_err() {
        return;
    }
    s.laco().await;
}

impl Sessao<'_> {
    async fn responder(&mut self, r: &str) -> io::Result<()> {
        let mut t = String::with_capacity(r.len() + 2);
        t.push_str(r);
        t.push_str("\r\n");
        self.fluxo.escrever(t.as_bytes()).await
    }

    async fn laco(&mut self) {
        loop {
            let prazo = Instant::now() + self.ctx.config.tempo_comando();
            let linha = match self.leitor.linha(&mut self.fluxo, prazo).await {
                Ok(l) => l,
                Err(ErroLeitura::Tempo) => {
                    let _ = self
                        .responder("421 4.4.2 Timeout, closing connection")
                        .await;
                    return;
                }
                Err(ErroLeitura::Longa) => {
                    let _ = self.responder("500 5.5.2 Line too long").await;
                    return;
                }
                Err(_) => return,
            };
            let (verbo, arg) = match linha.split_once(' ') {
                Some((v, a)) => (v.to_ascii_uppercase(), a.trim().to_string()),
                None => (linha.trim().to_ascii_uppercase(), String::new()),
            };
            let continuar = match self.comando(&verbo, &arg).await {
                Ok(c) => c,
                Err(_) => return,
            };
            if !continuar {
                return;
            }
            if self.erros >= ERROS_MAXIMOS {
                somar(&self.ctx.entrega.metricas.recusadas_protocolo);
                let _ = self
                    .responder("421 4.7.0 Too many errors, closing connection")
                    .await;
                return;
            }
        }
    }

    async fn erro(&mut self, r: &str) -> io::Result<bool> {
        self.erros += 1;
        self.responder(r).await?;
        Ok(true)
    }

    /// `Ok(false)` encerra a sessão.
    async fn comando(&mut self, verbo: &str, arg: &str) -> io::Result<bool> {
        match verbo {
            "EHLO" | "HELO" => self.helo(verbo == "EHLO", arg).await,
            "STARTTLS" => self.starttls().await,
            "MAIL" => self.mail(arg).await,
            "RCPT" => self.rcpt(arg).await,
            "DATA" => self.data(arg).await,
            "RSET" => {
                self.transacao = None;
                self.responder("250 2.0.0 OK").await?;
                Ok(true)
            }
            "NOOP" => {
                self.responder("250 2.0.0 OK").await?;
                Ok(true)
            }
            "QUIT" => {
                self.responder("221 2.0.0 Bye").await?;
                Ok(false)
            }
            "VRFY" => {
                self.responder("252 2.5.0 Cannot VRFY user").await?;
                Ok(true)
            }
            "HELP" => {
                self.responder("214 2.0.0 See RFC 5321").await?;
                Ok(true)
            }
            // um navegador ou proxy HTTP na porta 25
            "GET" | "POST" | "HEAD" | "CONNECT" | "PUT" => {
                somar(&self.ctx.entrega.metricas.recusadas_protocolo);
                self.responder("421 4.7.0 Not an HTTP server").await?;
                Ok(false)
            }
            "AUTH" | "BDAT" | "ETRN" | "EXPN" | "TURN" => {
                self.erro("502 5.5.1 Command not implemented").await
            }
            _ => self.erro("500 5.5.1 Command unrecognized").await,
        }
    }

    async fn helo(&mut self, estendido: bool, arg: &str) -> io::Result<bool> {
        let nome = arg.split_ascii_whitespace().next().unwrap_or("");
        if !endereco::helo_valido(nome) {
            return self.erro("501 5.5.4 Invalid HELO/EHLO argument").await;
        }
        let nome_l = nome.trim_end_matches('.').to_ascii_lowercase();
        let literal_local = format!("[{}]", self.local.ip().to_canonical());
        if nome_l == self.ctx.config.hostname
            || self.ctx.config.e_nosso_dominio(&nome_l)
            || nome_l == literal_local
        {
            somar(&self.ctx.entrega.metricas.recusadas_protocolo);
            log::info(
                "recusada",
                &[
                    ("ip", &self.ip.to_string()),
                    ("motivo", "HELO com o nosso nome"),
                    ("helo", nome),
                ],
            );
            return self.erro("550 5.7.1 You are not me").await;
        }
        self.helo = Some(nome.to_string());
        self.transacao = None;
        if !estendido {
            let r = format!("250 {}", self.ctx.config.hostname);
            self.responder(&r).await?;
            return Ok(true);
        }
        let mut linhas = vec![
            format!("{} Hello {}", self.ctx.config.hostname, self.ip),
            format!("SIZE {}", self.ctx.config.tamanho_maximo),
            "8BITMIME".into(),
            "PIPELINING".into(),
            "ENHANCEDSTATUSCODES".into(),
            "SMTPUTF8".into(),
        ];
        if self.tls.is_none() && self.ctx.tls.as_ref().and_then(|t| t.aceitador()).is_some() {
            linhas.push("STARTTLS".into());
        }
        let ultima = linhas.len() - 1;
        let r: Vec<String> = linhas
            .iter()
            .enumerate()
            .map(|(i, l)| format!("250{}{l}", if i == ultima { ' ' } else { '-' }))
            .collect();
        let r = r.join("\r\n");
        self.responder(&r).await?;
        Ok(true)
    }

    async fn starttls(&mut self) -> io::Result<bool> {
        if self.tls.is_some() {
            return self.erro("503 5.5.1 TLS already active").await;
        }
        let Some(aceitador) = self.ctx.tls.as_ref().and_then(|t| t.aceitador()) else {
            return self.erro("454 4.7.0 TLS not available").await;
        };
        // Comandos enviados junto com o STARTTLS, em claro, seriam
        // executados depois do handshake como se viessem cifrados.
        if !self.leitor.pendente().is_empty() {
            somar(&self.ctx.entrega.metricas.recusadas_protocolo);
            self.responder("554 5.5.0 Pipelining not allowed after STARTTLS")
                .await?;
            return Ok(false);
        }
        self.responder("220 2.0.0 Ready to start TLS").await?;
        let Fluxo::Tcp(tcp) = std::mem::replace(&mut self.fluxo, Fluxo::Fechado) else {
            return Ok(false);
        };
        match timeout(TEMPO_HANDSHAKE, aceitador.accept(tcp)).await {
            Ok(Ok(t)) => {
                let (_, c) = t.get_ref();
                let versao = c
                    .protocol_version()
                    .and_then(|v| v.as_str())
                    .unwrap_or("TLS");
                let cifra = c
                    .negotiated_cipher_suite()
                    .and_then(|s| s.suite().as_str())
                    .unwrap_or("?");
                self.tls = Some(format!("version={versao} cipher={cifra}"));
                self.fluxo = Fluxo::Tls(Box::new(t));
                self.helo = None;
                self.transacao = None;
                Ok(true)
            }
            _ => {
                log::info("tls_falhou", &[("ip", &self.ip.to_string())]);
                Ok(false)
            }
        }
    }

    async fn mail(&mut self, arg: &str) -> io::Result<bool> {
        if self.helo.is_none() {
            return self.erro("503 5.5.1 Send EHLO/HELO first").await;
        }
        if self.transacao.is_some() {
            return self.erro("503 5.5.1 Nested MAIL command").await;
        }
        let Some(c) = endereco::caminho(arg, "FROM:") else {
            return self.erro("501 5.1.7 Bad sender address syntax").await;
        };
        for (k, v) in &c.parametros {
            if k == "SIZE" {
                match v.as_deref().and_then(|v| v.parse::<u64>().ok()) {
                    Some(n) if n > self.ctx.config.tamanho_maximo => {
                        self.responder("552 5.3.4 Message size exceeds fixed limit")
                            .await?;
                        return Ok(true);
                    }
                    Some(_) => {}
                    None => return self.erro("501 5.5.4 Bad SIZE parameter").await,
                }
            }
        }
        self.transacao = Some(Transacao {
            de: c.endereco,
            destinos: Vec::new(),
        });
        self.responder("250 2.1.0 OK").await?;
        Ok(true)
    }

    async fn rcpt(&mut self, arg: &str) -> io::Result<bool> {
        let Some(t) = self.transacao.as_ref() else {
            return self.erro("503 5.5.1 Send MAIL first").await;
        };
        if t.destinos.len() >= self.ctx.config.destinatarios_maximos {
            self.responder("452 4.5.3 Too many recipients").await?;
            return Ok(true);
        }
        let Some(c) = endereco::caminho(arg, "TO:") else {
            return self.erro("501 5.1.3 Bad recipient address syntax").await;
        };
        match endereco::destino(&c.endereco, &self.ctx.config.caixas) {
            // Uma mensagem vai para uma conta só. O 452 (RFC 5321, 4.5.3.1.10)
            // faz quem envia mandar os destinatários restantes numa nova
            // transação, que cai na outra caixa.
            Ok(d) if t.destinos.first().is_some_and(|p| p.caixa != d.caixa) => {
                log::info(
                    "rcpt_adiado",
                    &[
                        ("ip", &self.ip.to_string()),
                        ("para", &c.endereco),
                        ("motivo", "outra caixa na mesma transação"),
                    ],
                );
                self.responder("452 4.5.3 Too many recipients, send the rest in a new transaction")
                    .await?;
                Ok(true)
            }
            Ok(d) => {
                let t = self.transacao.as_mut().expect("verificado acima");
                if !t.destinos.contains(&d) {
                    t.destinos.push(d);
                }
                self.responder("250 2.1.5 OK").await?;
                Ok(true)
            }
            Err(r) => {
                somar(&self.ctx.entrega.metricas.recusadas_rcpt);
                let (motivo, resposta) = match r {
                    Recusa::ForaDoDominio => ("relay", "550 5.7.1 Relaying denied"),
                    Recusa::Desconhecido => ("desconhecido", "550 5.1.1 No such user here"),
                };
                log::info(
                    "rcpt_recusado",
                    &[
                        ("ip", &self.ip.to_string()),
                        ("para", &c.endereco),
                        ("motivo", motivo),
                    ],
                );
                self.erro(resposta).await
            }
        }
    }

    async fn data(&mut self, arg: &str) -> io::Result<bool> {
        if !arg.is_empty() {
            return self.erro("501 5.5.4 DATA takes no arguments").await;
        }
        match &self.transacao {
            None => return self.erro("503 5.5.1 Send MAIL first").await,
            Some(t) if t.destinos.is_empty() => {
                return self.erro("554 5.5.1 No valid recipients").await;
            }
            _ => {}
        }
        let id = Spool::novo_id();
        let spool = Arc::clone(&self.ctx.entrega.spool);
        let arquivo = match spool.criar_tmp(&id) {
            Ok(f) => tokio::fs::File::from_std(f),
            Err(e) => {
                log::erro("spool_criar", &[("erro", &e.to_string())]);
                self.responder("451 4.3.0 Local error, try again later")
                    .await?;
                return Ok(true);
            }
        };
        self.responder("354 End data with <CR><LF>.<CR><LF>")
            .await?;

        let recebido = self.receber_corpo(arquivo).await;
        let tamanho = match recebido {
            Ok(Some(n)) => n,
            Ok(None) => {
                spool.descartar_tmp(&id);
                self.transacao = None;
                self.responder("552 5.3.4 Message size exceeds fixed limit")
                    .await?;
                return Ok(true);
            }
            Err(ErroLeitura::Tempo) => {
                spool.descartar_tmp(&id);
                let _ = self
                    .responder("421 4.4.2 Timeout, closing connection")
                    .await;
                return Ok(false);
            }
            Err(ErroLeitura::Io) if !matches!(self.fluxo, Fluxo::Fechado) => {
                spool.descartar_tmp(&id);
                self.responder("451 4.3.0 Local error, try again later")
                    .await?;
                return Ok(false);
            }
            Err(_) => {
                spool.descartar_tmp(&id);
                return Ok(false);
            }
        };
        let t = self.transacao.take().expect("verificado acima");
        let r = self.finalizar(id, t, tamanho).await;
        self.responder(&r).await?;
        Ok(true)
    }

    /// Lê o DATA para o arquivo. `Ok(None)`: passou do limite (o resto foi
    /// lido e descartado, para a sessão continuar em sincronia).
    async fn receber_corpo(
        &mut self,
        arquivo: tokio::fs::File,
    ) -> Result<Option<u64>, ErroLeitura> {
        let prazo = Instant::now() + self.ctx.config.tempo_dados();
        let mut saida = BufWriter::with_capacity(PEDACO, arquivo);
        let mut d = Desempacotador::new();
        let mut pedaco = Vec::with_capacity(PEDACO);
        let mut tamanho: u64 = 0;
        let limite = self.ctx.config.tamanho_maximo;
        loop {
            if self.leitor.pendente().is_empty() {
                self.leitor.encher(&mut self.fluxo, prazo).await?;
            }
            pedaco.clear();
            let (n, fim) = d.processar(self.leitor.pendente(), &mut pedaco);
            self.leitor.consumir(n);
            tamanho += pedaco.len() as u64;
            if tamanho <= limite {
                saida
                    .write_all(&pedaco)
                    .await
                    .map_err(|_| ErroLeitura::Io)?;
            }
            if fim {
                break;
            }
        }
        if tamanho > limite {
            return Ok(None);
        }
        saida.flush().await.map_err(|_| ErroLeitura::Io)?;
        Ok(Some(tamanho))
    }

    fn cabecalhos(
        &self,
        id: &str,
        t: &Transacao,
        autenticacao: &str,
        rdns: Option<&str>,
    ) -> String {
        let mut h = format!("Return-Path: <{}>\r\n", t.de);
        for d in &t.destinos {
            h.push_str(&format!("Delivered-To: {}\r\n", d.endereco));
        }
        h.push_str(autenticacao);
        let helo = self.helo.as_deref().unwrap_or("unknown");
        let protocolo = if self.tls.is_some() {
            "ESMTPS"
        } else {
            "ESMTP"
        };
        h.push_str(&format!(
            "Received: from {helo} ({} [{}])\r\n\tby {} (carteiro) with {protocolo} id {id}",
            rdns.unwrap_or("unknown"),
            self.ip,
            self.ctx.config.hostname
        ));
        if let Some(tls) = &self.tls {
            h.push_str(&format!("\r\n\t({tls})"));
        }
        if let [d] = t.destinos.as_slice() {
            h.push_str(&format!("\r\n\tfor <{}>", d.endereco));
        }
        h.push_str(&format!(
            "; {}\r\n",
            crate::data::rfc5322(crate::data::agora_unix())
        ));
        h
    }

    async fn finalizar(&mut self, id: String, t: Transacao, tamanho: u64) -> String {
        let spool = Arc::clone(&self.ctx.entrega.spool);
        let metricas = Arc::clone(&self.ctx.entrega.metricas);
        let para: Vec<String> = t.destinos.iter().map(|d| d.endereco.clone()).collect();
        let mut labels: Vec<String> = Vec::new();
        for d in &t.destinos {
            let l = d.label();
            if !labels.contains(&l) {
                labels.push(l);
            }
        }
        let ip = self.ip.to_string();
        let tam = tamanho.to_string();
        let para_log = para.join(",");

        let (auth_cab, recusa, resumo, rdns) = match &self.ctx.verificador {
            Some(v) => {
                let caminho = spool.tmp(&id);
                let mapa = std::fs::File::open(&caminho).and_then(|f| {
                    if tamanho == 0 {
                        return Ok(None);
                    }
                    // SAFETY: o arquivo é nosso, em tmp/ (0700), e ninguém mais
                    // escreve nele enquanto o mapa existe.
                    unsafe { memmap2::Mmap::map(&f) }.map(Some)
                });
                match mapa {
                    Ok(m) => {
                        let bruto: &[u8] = m.as_deref().unwrap_or(&[]);
                        let helo = self.helo.clone().unwrap_or_default();
                        let ver = v.verificar(bruto, self.ip, &helo, &t.de).await;
                        (ver.cabecalho, ver.recusa, ver.resumo, ver.rdns)
                    }
                    Err(e) => {
                        log::erro("spool_ler", &[("id", &id), ("erro", &e.to_string())]);
                        spool.descartar_tmp(&id);
                        return "451 4.3.0 Local error, try again later".into();
                    }
                }
            }
            None => (String::new(), None, String::new(), None),
        };

        if let Some(motivo) = recusa {
            spool.descartar_tmp(&id);
            somar(&metricas.recusadas_dmarc);
            log::info(
                "recusada",
                &[
                    ("id", &id),
                    ("ip", &ip),
                    ("de", &t.de),
                    ("para", &para_log),
                    ("tamanho", &tam),
                    ("motivo", &motivo),
                    ("autenticacao", &resumo),
                ],
            );
            return format!("550 5.7.1 Message rejected: {motivo}");
        }

        let caixa = t
            .destinos
            .first()
            .and_then(|d| self.ctx.config.caixas.get(d.caixa))
            .map(|c| c.nome.clone())
            .unwrap_or_default();
        let meta = Meta {
            id: id.clone(),
            caixa: caixa.clone(),
            cabecalhos: self.cabecalhos(&id, &t, &auth_cab, rdns.as_deref()),
            de: t.de.clone(),
            para,
            labels,
            ip: ip.clone(),
            tamanho,
            recebida: crate::data::agora_unix(),
            tentativas: 0,
            proxima: 0,
            ultimo_erro: None,
        };
        let r = self
            .ctx
            .entrega
            .aceitar(meta, self.ctx.config.espera_gmail())
            .await;
        let (resposta, resultado, gmail_id) = match r {
            Err(e) => {
                log::erro("spool_confirmar", &[("id", &id), ("erro", &e.to_string())]);
                spool.descartar_tmp(&id);
                (
                    "451 4.3.0 Local error, try again later".to_string(),
                    "erro_local",
                    String::new(),
                )
            }
            Ok(Resultado::Importada(g)) => {
                (format!("250 2.0.0 OK: delivered as {g}"), "importada", g)
            }
            Ok(Resultado::NaFila) => {
                somar(&metricas.enfileiradas);
                (
                    format!("250 2.0.0 OK: queued as {id}"),
                    "fila",
                    String::new(),
                )
            }
            Ok(Resultado::Recusada(m)) => {
                let curto: String = m.chars().filter(|c| !c.is_control()).take(120).collect();
                (
                    format!("554 5.6.0 Message rejected by mailbox: {curto}"),
                    "recusada_gmail",
                    String::new(),
                )
            }
        };
        log::info(
            "recebida",
            &[
                ("id", &id),
                ("ip", &ip),
                ("helo", self.helo.as_deref().unwrap_or("")),
                ("tls", if self.tls.is_some() { "sim" } else { "nao" }),
                ("de", &t.de),
                ("para", &para_log),
                ("caixa", &caixa),
                ("tamanho", &tam),
                ("resultado", resultado),
                ("gmail_id", &gmail_id),
                ("autenticacao", &resumo),
            ],
        );
        resposta
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn desempacotar(entrada: &[u8], em_pedacos_de: usize) -> (Vec<u8>, usize, bool) {
        let mut d = Desempacotador::new();
        let mut saida = Vec::new();
        let mut consumido = 0;
        for c in entrada.chunks(em_pedacos_de) {
            let (n, fim) = d.processar(c, &mut saida);
            consumido += n;
            if fim {
                return (saida, consumido, true);
            }
        }
        (saida, consumido, false)
    }

    #[test]
    fn fim_e_ponto_duplicado_em_qualquer_corte() {
        let entrada = b"Subject: x\r\n\r\n..linha com ponto\r\n.\r\nQUIT\r\n";
        for p in 1..entrada.len() {
            let (saida, n, fim) = desempacotar(entrada, p);
            assert!(fim, "pedaço {p}");
            assert_eq!(
                saida, b"Subject: x\r\n\r\n.linha com ponto\r\n",
                "pedaço {p}"
            );
            assert_eq!(&entrada[n..], b"QUIT\r\n", "pedaço {p}");
        }
    }

    #[test]
    fn lf_solto_nao_termina() {
        // smuggling: \n.\n e \r\n.\n não podem encerrar a mensagem
        let entrada = b"a\n.\nMAIL FROM:<x@y.z>\r\nb\r\n.\nc\r\n.\r\n";
        let (saida, n, fim) = desempacotar(entrada, 4);
        assert!(fim);
        assert_eq!(n, entrada.len());
        assert_eq!(saida, b"a\n.\nMAIL FROM:<x@y.z>\r\nb\r\n.\nc\r\n");
    }

    #[test]
    fn mensagem_vazia_e_ponto_cr_solto() {
        assert_eq!(desempacotar(b".\r\n", 1), (Vec::new(), 3, true));
        let (s, _, fim) = desempacotar(b".\rx\r\n.\r\n", 2);
        assert!(fim);
        assert_eq!(s, b".\rx\r\n");
    }
}

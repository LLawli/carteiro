//! Ponta a ponta: um cliente SMTP real (com STARTTLS) fala com o carteiro, e
//! o carteiro fala com um mock da Gmail API e do endpoint de token do OAuth.

use axum::extract::{Path as Caminho, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Form, Json, Router};
use carteiro::config::{Caixa, Config};
use carteiro::servidor::{self, Servidor};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufStream};
use tokio::net::TcpStream;
use tokio_rustls::rustls;

// ---------------------------------------------------------------- mock

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Modo {
    Ok,
    Erro500,
    Invalida400,
    LabelInvalida400,
}

#[derive(Debug, Clone)]
struct Import {
    /// refresh token da conta que importou
    conta: String,
    nunca_spam: bool,
    tipo: String,
    labels: Vec<String>,
    bruto: Vec<u8>,
}

struct Estado {
    modo: Modo,
    atraso: Duration,
    /// atrasos só dos próximos pedidos de import, na ordem
    atrasos: std::collections::VecDeque<Duration>,
    imports: Vec<Import>,
    labels: Vec<(String, String)>,
    tokens: u32,
    /// refresh token (a conta) → access token válido
    token_valido: HashMap<String, String>,
    sessoes: HashMap<String, (String, bool, Vec<String>)>,
    endereco: Option<SocketAddr>,
    pedidos_import: u32,
}

type Mock = Arc<Mutex<Estado>>;

/// A conta dona do access token, se ele vale.
fn autorizado(m: &Mock, h: &HeaderMap) -> Option<String> {
    let e = m.lock().unwrap();
    let dado = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    e.token_valido
        .iter()
        .find(|(_, t)| dado == format!("Bearer {t}"))
        .map(|(conta, _)| conta.clone())
}

async fn token(State(m): State<Mock>, Form(f): Form<HashMap<String, String>>) -> Response {
    let conta = f.get("refresh_token").cloned().unwrap_or_default();
    if f.get("grant_type").map(String::as_str) != Some("refresh_token")
        || !["rt-teste", "rt-outra"].contains(&conta.as_str())
        || f.get("client_secret").map(String::as_str) != Some("segredo")
    {
        return (StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#).into_response();
    }
    let mut e = m.lock().unwrap();
    e.tokens += 1;
    let t = format!("at-{}", e.tokens);
    e.token_valido.insert(conta, t.clone());
    Json(serde_json::json!({"access_token": t, "expires_in": 3600, "token_type": "Bearer"}))
        .into_response()
}

async fn listar_labels(State(m): State<Mock>, h: HeaderMap) -> Response {
    if autorizado(&m, &h).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let e = m.lock().unwrap();
    let mut v = vec![
        serde_json::json!({"id": "INBOX", "name": "INBOX"}),
        serde_json::json!({"id": "UNREAD", "name": "UNREAD"}),
    ];
    for (id, nome) in &e.labels {
        v.push(serde_json::json!({"id": id, "name": nome}));
    }
    Json(serde_json::json!({ "labels": v })).into_response()
}

async fn criar_label(
    State(m): State<Mock>,
    h: HeaderMap,
    Json(c): Json<serde_json::Value>,
) -> Response {
    if autorizado(&m, &h).is_none() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut e = m.lock().unwrap();
    let nome = c["name"].as_str().unwrap().to_string();
    if e.labels.iter().any(|(_, n)| n.eq_ignore_ascii_case(&nome)) {
        return (StatusCode::CONFLICT, "exists").into_response();
    }
    let id = format!("Label_{}", e.labels.len() + 1);
    e.labels.push((id.clone(), nome.clone()));
    Json(serde_json::json!({"id": id, "name": nome})).into_response()
}

fn resposta_import(
    m: &Mock,
    conta: String,
    nunca_spam: bool,
    tipo: &str,
    labels: Vec<String>,
    bruto: Vec<u8>,
) -> Response {
    let mut e = m.lock().unwrap();
    match e.modo {
        Modo::Erro500 => {
            return (StatusCode::INTERNAL_SERVER_ERROR, "backend error").into_response();
        }
        Modo::Invalida400 => {
            return (
                StatusCode::BAD_REQUEST,
                r#"{"error":{"code":400,"message":"Invalid message","status":"INVALID_ARGUMENT"}}"#,
            )
                .into_response();
        }
        Modo::LabelInvalida400 => {
            e.modo = Modo::Ok;
            return (
                StatusCode::BAD_REQUEST,
                r#"{"error":{"code":400,"message":"Invalid label: Label_1","status":"INVALID_ARGUMENT"}}"#,
            )
                .into_response();
        }
        Modo::Ok => {}
    }
    let id = format!("gm{}", e.imports.len() + 1);
    e.imports.push(Import {
        conta,
        nunca_spam,
        tipo: tipo.into(),
        labels,
        bruto,
    });
    Json(serde_json::json!({"id": id, "threadId": id, "labelIds": []})).into_response()
}

fn partes_multipart(tipo_conteudo: &str, corpo: &[u8]) -> (Vec<String>, Vec<u8>) {
    let fronteira = tipo_conteudo.split("boundary=").nth(1).unwrap();
    let sep = format!("--{fronteira}");
    let texto = corpo;
    let achar = |de: usize, agulha: &[u8]| {
        texto[de..]
            .windows(agulha.len())
            .position(|w| w == agulha)
            .map(|p| p + de)
            .unwrap()
    };
    let p1 = achar(0, sep.as_bytes()) + sep.len();
    let j_ini = achar(p1, b"\r\n\r\n") + 4;
    let p2 = achar(j_ini, sep.as_bytes());
    let json: serde_json::Value = serde_json::from_slice(&texto[j_ini..p2 - 2]).unwrap();
    let m_ini = achar(p2, b"\r\n\r\n") + 4;
    let fim = format!("\r\n--{fronteira}--\r\n");
    assert!(texto.ends_with(fim.as_bytes()));
    let bruto = texto[m_ini..texto.len() - fim.len()].to_vec();
    let labels = json["labelIds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    (labels, bruto)
}

async fn importar(
    State(m): State<Mock>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
    corpo: axum::body::Bytes,
) -> Response {
    let atraso = {
        let mut e = m.lock().unwrap();
        e.pedidos_import += 1;
        e.atrasos.pop_front().unwrap_or(e.atraso)
    };
    tokio::time::sleep(atraso).await;
    let Some(conta) = autorizado(&m, &h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    assert_eq!(
        q.get("internalDateSource").map(String::as_str),
        Some("dateHeader")
    );
    let nunca_spam = match q.get("neverMarkSpam").map(String::as_str) {
        None => false,
        Some("true") => true,
        outro => panic!("neverMarkSpam={outro:?}"),
    };
    let tipo_c = h
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    match q.get("uploadType").map(String::as_str) {
        Some("multipart") => {
            let (labels, bruto) = partes_multipart(&tipo_c, &corpo);
            resposta_import(&m, conta, nunca_spam, "multipart", labels, bruto)
        }
        Some("resumable") => {
            assert_eq!(h.get("X-Upload-Content-Type").unwrap(), "message/rfc822");
            let json: serde_json::Value = serde_json::from_slice(&corpo).unwrap();
            let labels: Vec<String> = json["labelIds"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_string())
                .collect();
            let mut e = m.lock().unwrap();
            let id = format!("s{}", e.sessoes.len() + 1);
            e.sessoes.insert(id.clone(), (conta, nunca_spam, labels));
            let local = e.endereco.unwrap();
            (
                StatusCode::OK,
                [(header::LOCATION, format!("http://{local}/sessao/{id}"))],
            )
                .into_response()
        }
        outro => panic!("uploadType inesperado: {outro:?}"),
    }
}

async fn sessao(
    State(m): State<Mock>,
    Caminho(id): Caminho<String>,
    corpo: axum::body::Bytes,
) -> Response {
    let (conta, nunca_spam, labels) = m
        .lock()
        .unwrap()
        .sessoes
        .remove(&id)
        .expect("sessão resumable");
    resposta_import(&m, conta, nunca_spam, "resumable", labels, corpo.to_vec())
}

async fn subir_mock() -> (Mock, SocketAddr) {
    let m: Mock = Arc::new(Mutex::new(Estado {
        modo: Modo::Ok,
        atraso: Duration::ZERO,
        atrasos: Default::default(),
        imports: Vec::new(),
        labels: Vec::new(),
        tokens: 0,
        token_valido: HashMap::new(),
        sessoes: HashMap::new(),
        endereco: None,
        pedidos_import: 0,
    }));
    let app = Router::new()
        .route("/token", post(token))
        .route(
            "/gmail/v1/users/me/labels",
            get(listar_labels).post(criar_label),
        )
        .route("/upload/gmail/v1/users/me/messages/import", post(importar))
        .route("/sessao/{id}", put(sessao))
        .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024))
        .with_state(Arc::clone(&m));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let end = l.local_addr().unwrap();
    m.lock().unwrap().endereco = Some(end);
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (m, end)
}

// ---------------------------------------------------------------- ambiente

struct Ambiente {
    mock: Mock,
    servidor: Option<Servidor>,
    smtp: SocketAddr,
    dir: PathBuf,
    raiz_tls: rustls::pki_types::CertificateDer<'static>,
    config: Config,
}

fn dir_teste() -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "carteiro-e2e-{}-{}",
        std::process::id(),
        carteiro::spool::Spool::novo_id()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn ambiente(ajuste: impl FnOnce(&mut Config)) -> Ambiente {
    let (mock, end_mock) = subir_mock().await;
    let dir = dir_teste();
    let ck = rcgen::generate_simple_self_signed(vec!["mx.teste.local".into()]).unwrap();
    std::fs::write(dir.join("cert.pem"), ck.cert.pem()).unwrap();
    std::fs::write(dir.join("chave.pem"), ck.signing_key.serialize_pem()).unwrap();
    std::fs::write(
        dir.join("cliente.json"),
        r#"{"installed":{"client_id":"id-teste","client_secret":"segredo"}}"#,
    )
    .unwrap();
    std::fs::write(dir.join("token.json"), r#"{"refresh_token":"rt-teste"}"#).unwrap();
    let texto = format!(
        r#"
hostname = "mx.teste.local"
dominio = "exemplo.com"
enderecos = ["contato", "pix"]
escutar = ["127.0.0.1:0"]
espera_banner_ms = 0
espera_gmail_s = 5
espera_retentativa_s = 1
tamanho_maximo = 10485760
verificar_autenticacao = false
spool = "{d}/spool"
certificado = "{d}/cert.pem"
chave = "{d}/chave.pem"
cliente_oauth = "{d}/cliente.json"
token = "{d}/token.json"
metricas = "{d}/carteiro.prom"
api_gmail = "http://{end_mock}"
api_token = "http://{end_mock}/token"
"#,
        d = dir.display()
    );
    let mut config: Config = toml::from_str(&texto).unwrap();
    ajuste(&mut config);
    config.normalizar().unwrap();
    let servidor = servidor::iniciar(config.clone()).await.unwrap();
    let smtp = servidor.enderecos[0];
    Ambiente {
        mock,
        servidor: Some(servidor),
        smtp,
        dir,
        raiz_tls: ck.cert.der().clone(),
        config,
    }
}

impl Ambiente {
    fn imports(&self) -> Vec<Import> {
        self.mock.lock().unwrap().imports.clone()
    }

    fn fila(&self) -> usize {
        std::fs::read_dir(self.dir.join("spool/fila"))
            .unwrap()
            .count()
    }

    fn falhou(&self) -> usize {
        std::fs::read_dir(self.dir.join("spool/falhou"))
            .unwrap()
            .count()
    }

    async fn cliente(&self) -> Cliente {
        Cliente::conectar(self.smtp).await
    }

    async fn reiniciar(&mut self) {
        self.servidor
            .take()
            .unwrap()
            .parar(Duration::from_secs(2))
            .await;
        let s = servidor::iniciar(self.config.clone()).await.unwrap();
        self.smtp = s.enderecos[0];
        self.servidor = Some(s);
    }

    fn label_id(&self, nome: &str) -> Option<String> {
        self.mock
            .lock()
            .unwrap()
            .labels
            .iter()
            .find(|(_, n)| n == nome)
            .map(|(i, _)| i.clone())
    }
}

/// Espera até `cond` valer, por no máximo `s` segundos.
async fn ate(s: u64, mut cond: impl FnMut() -> bool) -> bool {
    for _ in 0..s * 20 {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

// ---------------------------------------------------------------- cliente SMTP

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

struct Cliente {
    s: BufStream<Box<dyn Io>>,
}

impl Cliente {
    async fn conectar(end: SocketAddr) -> Cliente {
        let tcp = TcpStream::connect(end).await.unwrap();
        let mut c = Cliente {
            s: BufStream::new(Box::new(tcp)),
        };
        let (cod, _) = c.resposta().await;
        assert_eq!(cod, 220);
        c
    }

    async fn resposta(&mut self) -> (u16, String) {
        let mut tudo = String::new();
        loop {
            let mut l = String::new();
            let n = tokio::time::timeout(Duration::from_secs(30), self.s.read_line(&mut l))
                .await
                .expect("resposta a tempo")
                .unwrap();
            if n == 0 {
                return (0, tudo);
            }
            tudo.push_str(&l);
            if l.as_bytes().get(3) != Some(&b'-') {
                return (l[..3].parse().unwrap(), tudo);
            }
        }
    }

    async fn enviar(&mut self, bytes: &[u8]) {
        self.s.write_all(bytes).await.unwrap();
        self.s.flush().await.unwrap();
    }

    async fn cmd(&mut self, l: &str) -> (u16, String) {
        self.enviar(format!("{l}\r\n").as_bytes()).await;
        self.resposta().await
    }

    async fn starttls(self, raiz: &rustls::pki_types::CertificateDer<'static>) -> Cliente {
        let mut raizes = rustls::RootCertStore::empty();
        raizes.add(raiz.clone()).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(raizes)
        .with_no_client_auth();
        let conector = tokio_rustls::TlsConnector::from(Arc::new(cfg));
        let nome = rustls::pki_types::ServerName::try_from("mx.teste.local").unwrap();
        let tls = conector.connect(nome, self.s.into_inner()).await.unwrap();
        Cliente {
            s: BufStream::new(Box::new(tls)),
        }
    }

    /// EHLO, MAIL, RCPTs e DATA. Devolve a resposta final do DATA.
    async fn mandar(&mut self, de: &str, para: &[&str], corpo: &[u8]) -> (u16, String) {
        assert_eq!(self.cmd("EHLO cliente.exemplo.com").await.0, 250);
        assert_eq!(self.cmd(&format!("MAIL FROM:<{de}>")).await.0, 250);
        for p in para {
            assert_eq!(self.cmd(&format!("RCPT TO:<{p}>")).await.0, 250, "{p}");
        }
        assert_eq!(self.cmd("DATA").await.0, 354);
        self.enviar(corpo).await;
        self.enviar(b".\r\n").await;
        self.resposta().await
    }
}

fn mensagem(assunto: &str) -> Vec<u8> {
    format!(
        "From: Remetente <alguem@exemplo.com>\r\nTo: contato@exemplo.com\r\nSubject: {assunto}\r\nDate: Fri, 13 Nov 2026 10:00:00 +0000\r\nMessage-ID: <{assunto}@exemplo.com>\r\n\r\nCorpo.\r\n..linha que começava com ponto\r\n"
    )
    .into_bytes()
}

fn cabecalho<'a>(bruto: &'a str, nome: &str) -> Vec<&'a str> {
    bruto
        .split("\r\n")
        .take_while(|l| !l.is_empty())
        .filter(|l| {
            l.to_ascii_lowercase()
                .starts_with(&format!("{}:", nome.to_ascii_lowercase()))
        })
        .collect()
}

// ---------------------------------------------------------------- testes

#[tokio::test]
async fn entrega_com_starttls_e_label_do_subendereco() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    let (_, ehlo) = c.cmd("EHLO cliente.exemplo.com").await;
    assert!(ehlo.contains("STARTTLS"), "{ehlo}");
    assert!(ehlo.contains("SIZE 10485760"));
    assert_eq!(c.cmd("STARTTLS").await.0, 220);
    let mut c = c.starttls(&a.raiz_tls).await;
    let (_, ehlo) = c.cmd("EHLO cliente.exemplo.com").await;
    assert!(!ehlo.contains("STARTTLS"), "{ehlo}");
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["Contato+Banco@Exemplo.COM"],
            &mensagem("um"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("delivered as gm1"), "{r}");
    assert_eq!(c.cmd("QUIT").await.0, 221);

    let i = a.imports();
    assert_eq!(i.len(), 1);
    assert_eq!(i[0].tipo, "multipart");
    let id_label = a.label_id("contato/banco").expect("label criada");
    assert!(a.label_id("contato").is_some(), "pai criado antes do filho");
    assert_eq!(i[0].labels, ["INBOX", "UNREAD", id_label.as_str()]);

    let bruto = String::from_utf8(i[0].bruto.clone()).unwrap();
    let original = String::from_utf8(mensagem("um")).unwrap();
    assert!(
        bruto.ends_with(&original.replace("\r\n..linha", "\r\n.linha")),
        "{bruto}"
    );
    assert_eq!(
        cabecalho(&bruto, "Return-Path"),
        ["Return-Path: <alguem@exemplo.com>"]
    );
    assert_eq!(
        cabecalho(&bruto, "Delivered-To"),
        ["Delivered-To: contato+banco@exemplo.com"]
    );
    assert!(bruto.starts_with("Return-Path:"));
    let received = &bruto[bruto.find("Received:").unwrap()..bruto.find("From:").unwrap()];
    assert!(
        received.contains("from cliente.exemplo.com (unknown [127.0.0.1])"),
        "{received}"
    );
    assert!(
        received.contains("by mx.teste.local (carteiro) with ESMTPS"),
        "{received}"
    );
    assert!(received.contains("version=TLSv1_3"), "{received}");
    assert!(
        received.contains("for <contato+banco@exemplo.com>"),
        "{received}"
    );
    assert_eq!(a.fila(), 0);
}

#[tokio::test]
async fn dois_destinatarios_um_import_com_as_duas_labels() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    let (cod, _) = c
        .mandar(
            "alguem@exemplo.com",
            &[
                "contato@exemplo.com",
                "pix+nubank@exemplo.com",
                "contato@exemplo.com",
            ],
            &mensagem("dois"),
        )
        .await;
    assert_eq!(cod, 250);
    let i = a.imports();
    assert_eq!(i.len(), 1);
    let contato = a.label_id("contato").unwrap();
    let pix = a.label_id("pix/nubank").unwrap();
    assert_eq!(
        i[0].labels,
        ["INBOX", "UNREAD", contato.as_str(), pix.as_str()]
    );
    let bruto = String::from_utf8(i[0].bruto.clone()).unwrap();
    assert_eq!(cabecalho(&bruto, "Delivered-To").len(), 2);
    assert!(
        !bruto.contains("\tfor <"),
        "sem `for` com dois destinatários"
    );
}

#[tokio::test]
async fn nunca_relay_e_so_os_enderecos_da_config() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
    assert_eq!(c.cmd("RCPT TO:<contato@exemplo.com>").await.0, 503);
    assert_eq!(c.cmd("MAIL FROM:<alguem@exemplo.com>").await.0, 250);
    for (para, esperado) in [
        ("alguem@gmail.com", "550 5.7.1"),
        ("contato@exemplo.com.evil.com", "550 5.7.1"),
        ("contato@sub.exemplo.com", "550 5.7.1"),
        ("admin@exemplo.com", "550 5.1.1"),
        ("postmaster@exemplo.com", "550 5.1.1"),
        ("contato%gmail.com@exemplo.com", "550 5.1.1"),
    ] {
        let (_, r) = c.cmd(&format!("RCPT TO:<{para}>")).await;
        assert!(r.starts_with(esperado), "{para}: {r}");
    }
    assert_eq!(c.cmd("DATA").await.0, 554, "sem destinatário válido");
    assert_eq!(c.cmd("VRFY contato").await.0, 252);
    assert_eq!(c.cmd("AUTH PLAIN AGFAYg==").await.0, 502);
    assert_eq!(c.cmd("RSET").await.0, 250);
    assert_eq!(c.cmd("QUIT").await.0, 221);
    assert!(a.imports().is_empty());
}

#[tokio::test]
async fn helo_invalido_ou_com_o_nosso_nome() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO localhost").await.0, 501);
    assert_eq!(c.cmd("EHLO").await.0, 501);
    assert_eq!(c.cmd("EHLO mx.teste.local").await.0, 550);
    assert_eq!(c.cmd("HELO exemplo.com").await.0, 550);
    assert_eq!(c.cmd("EHLO [127.0.0.1]").await.0, 550, "o nosso próprio IP");
    assert_eq!(c.cmd("MAIL FROM:<a@b.c>").await.0, 503, "sem HELO aceito");
    assert_eq!(c.cmd("HELO [192.0.2.1]").await.0, 250);
}

#[tokio::test]
async fn limite_de_tamanho() {
    let a = ambiente(|c| c.tamanho_maximo = 1000).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
    let (_, r) = c.cmd("MAIL FROM:<a@exemplo.com> SIZE=5000").await;
    assert!(r.starts_with("552 5.3.4"), "{r}");
    // sem SIZE declarado: o corpo passa do limite no meio do DATA
    assert_eq!(c.cmd("MAIL FROM:<a@exemplo.com>").await.0, 250);
    assert_eq!(c.cmd("RCPT TO:<pix@exemplo.com>").await.0, 250);
    assert_eq!(c.cmd("DATA").await.0, 354);
    c.enviar(&vec![b'x'; 3000]).await;
    c.enviar(b"\r\n.\r\n").await;
    let (cod, _) = c.resposta().await;
    assert_eq!(cod, 552);
    // a sessão continua em sincronia
    assert_eq!(c.cmd("NOOP").await.0, 250);
    assert!(a.imports().is_empty());
    assert_eq!(
        std::fs::read_dir(a.dir.join("spool/tmp")).unwrap().count(),
        0
    );
}

#[tokio::test]
async fn pipelining() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
    c.enviar(
        b"MAIL FROM:<a@exemplo.com>\r\nRCPT TO:<pix@exemplo.com>\r\nRCPT TO:<x@outro.com>\r\nDATA\r\n",
    )
    .await;
    assert_eq!(c.resposta().await.0, 250);
    assert_eq!(c.resposta().await.0, 250);
    assert_eq!(c.resposta().await.0, 550);
    assert_eq!(c.resposta().await.0, 354);
    let mut tudo = mensagem("pipe");
    tudo.extend_from_slice(b".\r\nQUIT\r\n");
    c.enviar(&tudo).await;
    assert_eq!(c.resposta().await.0, 250);
    assert_eq!(c.resposta().await.0, 221);
    assert_eq!(a.imports().len(), 1);
}

#[tokio::test]
async fn gmail_fora_do_ar_fica_na_fila_e_entra_depois() {
    let a = ambiente(|_| {}).await;
    a.mock.lock().unwrap().modo = Modo::Erro500;
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("fila"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("queued as"), "{r}");
    assert_eq!(a.fila(), 2, ".eml e .json");
    a.mock.lock().unwrap().modo = Modo::Ok;
    assert!(
        ate(10, || a.imports().len() == 1).await,
        "o trabalhador tenta de novo"
    );
    assert!(ate(2, || a.fila() == 0).await);
    let prom = std::fs::read_to_string(a.dir.join("carteiro.prom")).unwrap();
    assert!(
        prom.contains("carteiro_token_ok{caixa=\"principal\"} 1"),
        "{prom}"
    );
}

#[tokio::test]
async fn gmail_recusa_a_mensagem_durante_a_sessao() {
    let a = ambiente(|_| {}).await;
    a.mock.lock().unwrap().modo = Modo::Invalida400;
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("ruim"),
        )
        .await;
    assert_eq!(cod, 554, "{r}");
    assert!(r.contains("5.6.0"), "{r}");
    assert_eq!(a.fila(), 0);
    assert_eq!(
        a.falhou(),
        0,
        "recusado na sessão: quem mandou recebe o bounce"
    );
}

#[tokio::test]
async fn label_invalida_e_temporaria_e_recarrega_as_labels() {
    let a = ambiente(|_| {}).await;
    a.mock.lock().unwrap().modo = Modo::LabelInvalida400;
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("label"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("queued"), "{r}");
    assert!(ate(10, || a.imports().len() == 1).await);
}

#[tokio::test]
async fn gmail_lento_responde_250_e_nao_duplica() {
    let a = ambiente(|c| c.espera_gmail_s = 1).await;
    a.mock
        .lock()
        .unwrap()
        .atrasos
        .push_back(Duration::from_millis(3000));
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("lenta"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("queued"), "{r}");
    // Uma segunda mensagem, rápida, termina e acorda o trabalhador enquanto
    // a primeira ainda está no ar: ele não pode tentá-la de novo.
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("rapida"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("delivered"), "{r}");
    assert!(ate(10, || a.imports().len() == 2).await);
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(a.mock.lock().unwrap().pedidos_import, 2);
    assert_eq!(a.imports().len(), 2);
    assert_eq!(a.fila(), 0);
}

#[tokio::test]
async fn recusa_depois_do_250_vai_para_falhou() {
    let a = ambiente(|c| c.espera_gmail_s = 1).await;
    {
        let mut m = a.mock.lock().unwrap();
        m.atraso = Duration::from_millis(1500);
        m.modo = Modo::Invalida400;
    }
    let mut c = a.cliente().await;
    let (cod, _) = c
        .mandar(
            "alguem@exemplo.com",
            &["pix@exemplo.com"],
            &mensagem("tarde"),
        )
        .await;
    assert_eq!(cod, 250);
    assert!(
        ate(10, || a.falhou() == 2).await,
        "o carteiro nunca manda bounce"
    );
    assert_eq!(a.fila(), 0);
}

#[tokio::test]
async fn token_renovado_depois_de_401() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    assert_eq!(
        c.mandar("a@exemplo.com", &["pix@exemplo.com"], &mensagem("t1"))
            .await
            .0,
        250
    );
    assert_eq!(a.mock.lock().unwrap().tokens, 1);
    // o Google invalida o access token antes da validade
    a.mock.lock().unwrap().token_valido.clear();
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar("a@exemplo.com", &["pix@exemplo.com"], &mensagem("t2"))
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("delivered"), "{r}");
    assert_eq!(a.mock.lock().unwrap().tokens, 2);
    assert_eq!(a.imports().len(), 2);
}

#[tokio::test]
async fn mensagem_grande_vai_por_resumable() {
    let a = ambiente(|_| {}).await;
    let mut corpo = mensagem("grande");
    // linhas de 998 bytes até passar dos 5 MiB do multipart
    let linha = [b'a'; 998];
    while corpo.len() < 6 * 1024 * 1024 {
        corpo.extend_from_slice(&linha);
        corpo.extend_from_slice(b"\r\n");
    }
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar("a@exemplo.com", &["contato@exemplo.com"], &corpo)
        .await;
    assert_eq!(cod, 250, "{r}");
    let i = a.imports();
    assert_eq!(i[0].tipo, "resumable");
    assert!(
        !i[0].nunca_spam,
        "sem nunca_spam na config, sem neverMarkSpam"
    );
    assert!(i[0].bruto.starts_with(b"Return-Path:"));
    let esperado = String::from_utf8(corpo)
        .unwrap()
        .replace("\r\n..linha", "\r\n.linha")
        .into_bytes();
    assert!(i[0].bruto.ends_with(&esperado));
}

#[tokio::test]
async fn fila_sobrevive_a_reinicio() {
    let mut a = ambiente(|c| c.espera_gmail_s = 1).await;
    a.mock.lock().unwrap().modo = Modo::Erro500;
    let mut c = a.cliente().await;
    assert_eq!(
        c.mandar("a@exemplo.com", &["pix@exemplo.com"], &mensagem("reinicio"))
            .await
            .0,
        250
    );
    drop(c);
    a.reiniciar().await;
    assert_eq!(a.fila(), 2);
    a.mock.lock().unwrap().modo = Modo::Ok;
    assert!(ate(10, || a.imports().len() == 1).await);
    assert!(ate(2, || a.fila() == 0).await);
}

#[tokio::test]
async fn falar_antes_do_banner_e_recusado() {
    let a = ambiente(|c| c.espera_banner_ms = 400).await;
    let mut tcp = TcpStream::connect(a.smtp).await.unwrap();
    tcp.write_all(b"EHLO apressado.exemplo.com\r\n")
        .await
        .unwrap();
    let mut s = String::new();
    tokio::io::BufReader::new(tcp)
        .read_line(&mut s)
        .await
        .unwrap();
    assert!(s.starts_with("554 5.5.0"), "{s}");
    // quem espera o banner é atendido
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
}

#[tokio::test]
async fn comando_em_claro_junto_do_starttls_e_recusado() {
    let a = ambiente(|_| {}).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
    c.enviar(b"STARTTLS\r\nRSET\r\n").await;
    let (cod, _) = c.resposta().await;
    assert_eq!(cod, 554);
}

#[tokio::test]
async fn limite_de_conexoes_por_ip() {
    let a = ambiente(|c| c.conexoes_por_ip = 1).await;
    let _primeira = a.cliente().await;
    let tcp = TcpStream::connect(a.smtp).await.unwrap();
    let mut s = String::new();
    tokio::io::BufReader::new(tcp)
        .read_line(&mut s)
        .await
        .unwrap();
    assert!(s.starts_with("421"), "{s}");
}

#[tokio::test]
async fn limite_diario_de_labels_novas() {
    let a = ambiente(|c| c.labels_novas_por_dia = 1).await;
    for (para, assunto) in [
        ("contato+a@exemplo.com", "a"),
        ("contato+b@exemplo.com", "b"),
    ] {
        let mut c = a.cliente().await;
        assert_eq!(
            c.mandar("x@exemplo.com", &[para], &mensagem(assunto))
                .await
                .0,
            250
        );
    }
    assert!(a.label_id("contato/a").is_some());
    assert!(a.label_id("contato/b").is_none(), "passou do limite do dia");
    let i = a.imports();
    assert_eq!(i[1].labels[2], a.label_id("contato").unwrap());
}

/// A forma curta da config do [`ambiente`] vira a caixa `eu`, e `outra`
/// (outro.app, conta `rt-outra`) recebe `suporte@`.
fn duas_caixas(c: &mut Config) {
    let token = c.token.take().unwrap();
    let outra = token.with_file_name("token-outra.json");
    std::fs::write(&outra, r#"{"refresh_token":"rt-outra"}"#).unwrap();
    let cliente = c.cliente_oauth.take().unwrap();
    c.caixas = vec![
        Caixa {
            nome: "eu".into(),
            dominio: c.dominio.take().unwrap(),
            enderecos: c.enderecos.take().unwrap(),
            cliente_oauth: cliente.clone(),
            token,
            nunca_spam: false,
        },
        Caixa {
            nome: "outra".into(),
            dominio: "outro.app".into(),
            enderecos: vec!["suporte".into()],
            cliente_oauth: cliente,
            token: outra,
            nunca_spam: true,
        },
    ];
}

#[tokio::test]
async fn cada_caixa_na_sua_conta() {
    let a = ambiente(duas_caixas).await;
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["Suporte+Loja@Outro.App"],
            &mensagem("suporte"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["contato@exemplo.com"],
            &mensagem("eu"),
        )
        .await;
    assert_eq!(cod, 250, "{r}");
    let i = a.imports();
    assert_eq!(i.len(), 2);
    assert_eq!(i[0].conta, "rt-outra");
    assert_eq!(i[1].conta, "rt-teste");
    assert!(i[0].nunca_spam, "a caixa outra importa com neverMarkSpam");
    assert!(
        !i[1].nunca_spam,
        "a caixa eu deixa o filtro de spam decidir"
    );
    let suporte = a.label_id("suporte/loja").unwrap();
    assert_eq!(i[0].labels, ["INBOX", "UNREAD", suporte.as_str()]);
    let bruto = String::from_utf8(i[0].bruto.clone()).unwrap();
    assert_eq!(
        cabecalho(&bruto, "Delivered-To"),
        ["Delivered-To: suporte+loja@outro.app"]
    );

    // endereço de uma caixa no domínio da outra, e domínio de ninguém
    assert_eq!(c.cmd("MAIL FROM:<alguem@exemplo.com>").await.0, 250);
    let (_, r) = c.cmd("RCPT TO:<contato@outro.app>").await;
    assert!(r.starts_with("550 5.1.1"), "{r}");
    let (_, r) = c.cmd("RCPT TO:<suporte@terceiro.app>").await;
    assert!(r.starts_with("550 5.7.1"), "{r}");
    assert_eq!(c.cmd("RSET").await.0, 250);
    let (_, r) = c.cmd("EHLO outro.app").await;
    assert!(r.starts_with("550"), "HELO com o domínio de uma caixa: {r}");

    // gravado na subida
    let prom = std::fs::read_to_string(a.dir.join("carteiro.prom")).unwrap();
    assert!(prom.contains("carteiro_token_ok{caixa=\"eu\"} 1"), "{prom}");
    assert!(
        prom.contains("carteiro_token_ok{caixa=\"outra\"} 1"),
        "{prom}"
    );
}

#[tokio::test]
async fn duas_caixas_na_mesma_transacao_viram_duas_entregas() {
    let a = ambiente(duas_caixas).await;
    let mut c = a.cliente().await;
    assert_eq!(c.cmd("EHLO cliente.exemplo.com").await.0, 250);
    assert_eq!(c.cmd("MAIL FROM:<alguem@exemplo.com>").await.0, 250);
    assert_eq!(c.cmd("RCPT TO:<contato@exemplo.com>").await.0, 250);
    let (cod, r) = c.cmd("RCPT TO:<suporte@outro.app>").await;
    assert_eq!(cod, 452, "{r}");
    assert!(r.starts_with("452 4.5.3"), "{r}");
    assert_eq!(
        c.cmd("RCPT TO:<pix@exemplo.com>").await.0,
        250,
        "a mesma caixa segue"
    );
    assert_eq!(c.cmd("DATA").await.0, 354);
    c.enviar(&mensagem("primeira")).await;
    c.enviar(b".\r\n").await;
    assert_eq!(c.resposta().await.0, 250);
    // o que quem envia faz depois do 452: o resto numa transação nova
    let (cod, _) = c
        .mandar(
            "alguem@exemplo.com",
            &["suporte@outro.app"],
            &mensagem("resto"),
        )
        .await;
    assert_eq!(cod, 250);
    let i = a.imports();
    assert_eq!(i.len(), 2);
    assert_eq!(i[0].conta, "rt-teste");
    assert_eq!(i[1].conta, "rt-outra");
    let bruto = String::from_utf8(i[0].bruto.clone()).unwrap();
    assert_eq!(cabecalho(&bruto, "Delivered-To").len(), 2);
}

#[tokio::test]
async fn token_recusado_numa_caixa_nao_para_a_outra() {
    let a = ambiente(|c| {
        duas_caixas(c);
        std::fs::write(&c.caixas[1].token, r#"{"refresh_token":"rt-revogado"}"#).unwrap();
    })
    .await;
    let mut c = a.cliente().await;
    let (cod, r) = c
        .mandar("alguem@exemplo.com", &["suporte@outro.app"], &mensagem("x"))
        .await;
    assert_eq!(cod, 250, "{r}");
    assert!(r.contains("queued as"), "fica na fila: {r}");
    let (cod, r) = c
        .mandar(
            "alguem@exemplo.com",
            &["contato@exemplo.com"],
            &mensagem("y"),
        )
        .await;
    assert!(r.contains("delivered as"), "{r}");
    assert_eq!(cod, 250);
    assert_eq!(a.imports().len(), 1);
    assert_eq!(a.fila(), 2, "só a da caixa outra");
    let entrega = &a.servidor.as_ref().unwrap().contexto.entrega;
    assert!(!entrega.conta("outra").unwrap().token_ok());
    assert!(entrega.conta("eu").unwrap().token_ok());
}

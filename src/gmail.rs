//! Cliente da Gmail API: token OAuth renovado sozinho, labels e
//! `users.messages.import`.
//!
//! O corpo nunca fica inteiro na memória: o upload lê o `.eml` do spool em
//! pedaços. Até [`LIMITE_MULTIPART`] vai num POST `multipart/related`; acima
//! disso, no upload `resumable` (o Google recomenda multipart só até 5 MB).

use crate::spool::Meta;
use futures_util::StreamExt;
use reqwest::{Body, Client, Response, StatusCode, header};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const LIMITE_MULTIPART: u64 = 5 * 1024 * 1024;
pub const ESCOPOS: &str =
    "https://www.googleapis.com/auth/gmail.insert https://www.googleapis.com/auth/gmail.labels";
const TEMPO_PEDIDO: Duration = Duration::from_secs(300);
const TEMPO_CONEXAO: Duration = Duration::from_secs(15);
const MARGEM_TOKEN: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Falha {
    /// Vale tentar de novo (rede, 5xx, 429, token, label).
    Temporaria(String),
    /// O Gmail recusou a mensagem em si: tentar de novo dá o mesmo.
    Permanente(String),
}

impl std::fmt::Display for Falha {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Falha::Temporaria(m) => write!(f, "temporária: {m}"),
            Falha::Permanente(m) => write!(f, "permanente: {m}"),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClienteOAuth {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "auth_uri_padrao")]
    pub auth_uri: String,
    #[serde(default = "token_uri_padrao")]
    pub token_uri: String,
}

fn auth_uri_padrao() -> String {
    "https://accounts.google.com/o/oauth2/v2/auth".into()
}
fn token_uri_padrao() -> String {
    "https://oauth2.googleapis.com/token".into()
}

impl ClienteOAuth {
    /// Lê o JSON baixado do Google Cloud (`{"installed": {...}}`).
    pub fn ler(caminho: &Path) -> Result<ClienteOAuth, String> {
        #[derive(Deserialize)]
        struct Arquivo {
            installed: Option<ClienteOAuth>,
            web: Option<ClienteOAuth>,
        }
        let b = std::fs::read(caminho).map_err(|e| format!("lendo {}: {e}", caminho.display()))?;
        let a: Arquivo =
            serde_json::from_slice(&b).map_err(|e| format!("em {}: {e}", caminho.display()))?;
        a.installed
            .or(a.web)
            .ok_or_else(|| format!("{}: sem `installed` nem `web`", caminho.display()))
    }
}

pub fn ler_refresh_token(caminho: &Path) -> Result<String, String> {
    #[derive(Deserialize)]
    struct T {
        refresh_token: String,
    }
    let b = std::fs::read(caminho).map_err(|e| format!("lendo {}: {e}", caminho.display()))?;
    serde_json::from_slice::<T>(&b)
        .map(|t| t.refresh_token)
        .map_err(|e| format!("em {}: {e}", caminho.display()))
}

/// Cliente HTTP com rustls (ring) e as raízes da Mozilla embutidas: não
/// depende do repositório de certificados do sistema.
pub fn http() -> Client {
    let raizes = tokio_rustls::rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let tls = tokio_rustls::rustls::ClientConfig::builder_with_provider(Arc::new(
        tokio_rustls::rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("versões de TLS do ring")
    .with_root_certificates(raizes)
    .with_no_client_auth();
    Client::builder()
        .tls_backend_preconfigured(tls)
        .connect_timeout(TEMPO_CONEXAO)
        .timeout(TEMPO_PEDIDO)
        .pool_idle_timeout(Duration::from_secs(60))
        .user_agent(concat!("carteiro/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("cliente HTTP")
}

struct Labels {
    carregadas: bool,
    /// nome em minúsculas → id
    ids: HashMap<String, String>,
    dia: u64,
    criadas_hoje: u32,
}

pub struct Gmail {
    http: Client,
    api: String,
    token_url: String,
    cliente: ClienteOAuth,
    refresh: String,
    acesso: Mutex<Option<(String, Instant)>>,
    labels: Mutex<Labels>,
    labels_por_dia: u32,
    token_ok: AtomicBool,
}

#[derive(Deserialize)]
struct RespostaToken {
    access_token: String,
    expires_in: u64,
}

#[derive(Deserialize)]
struct Label {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct ListaLabels {
    #[serde(default)]
    labels: Vec<Label>,
}

#[derive(Deserialize)]
struct Importada {
    id: String,
}

fn erro_rede(e: reqwest::Error) -> Falha {
    Falha::Temporaria(format!("rede: {e}"))
}

/// Só a mensagem inválida (400 INVALID_ARGUMENT que não fala de label) e a
/// grande demais (413) são permanentes. Qualquer outra coisa (inclusive 401,
/// 403 e 404, que são problema de credencial ou configuração) fica na fila.
pub fn classificar(status: StatusCode, corpo: &str) -> Falha {
    let resumo: String = corpo.chars().take(300).collect();
    let msg = format!(
        "HTTP {}: {}",
        status.as_u16(),
        resumo.replace(['\n', '\r'], " ")
    );
    match status.as_u16() {
        413 => Falha::Permanente(msg),
        400 if corpo.contains("INVALID_ARGUMENT") && !corpo.to_lowercase().contains("label") => {
            Falha::Permanente(msg)
        }
        _ => Falha::Temporaria(msg),
    }
}

fn dia_atual() -> u64 {
    crate::data::agora_unix() / 86_400
}

impl Gmail {
    pub fn new(
        http: Client,
        api: &str,
        token_url: &str,
        cliente: ClienteOAuth,
        refresh: String,
        labels_por_dia: u32,
    ) -> Gmail {
        Gmail {
            http,
            api: api.trim_end_matches('/').to_string(),
            token_url: token_url.to_string(),
            cliente,
            refresh,
            acesso: Mutex::new(None),
            labels: Mutex::new(Labels {
                carregadas: false,
                ids: HashMap::new(),
                dia: dia_atual(),
                criadas_hoje: 0,
            }),
            labels_por_dia,
            token_ok: AtomicBool::new(true),
        }
    }

    /// Falso depois de o Google recusar o refresh token (revogado, expirado):
    /// nada mais entra até alguém rodar `carteiro autorizar` de novo.
    pub fn token_ok(&self) -> bool {
        self.token_ok.load(Ordering::Relaxed)
    }

    async fn token(&self) -> Result<String, Falha> {
        let mut acesso = self.acesso.lock().await;
        if let Some((t, validade)) = acesso.as_ref()
            && Instant::now() < *validade
        {
            return Ok(t.clone());
        }
        let r = self
            .http
            .post(&self.token_url)
            .form(&[
                ("client_id", self.cliente.client_id.as_str()),
                ("client_secret", self.cliente.client_secret.as_str()),
                ("refresh_token", self.refresh.as_str()),
                ("grant_type", "refresh_token"),
            ])
            .send()
            .await
            .map_err(erro_rede)?;
        let status = r.status();
        let corpo = r.text().await.map_err(erro_rede)?;
        if !status.is_success() {
            if corpo.contains("invalid_grant") {
                self.token_ok.store(false, Ordering::Relaxed);
                crate::log::erro(
                    "token_recusado",
                    &[(
                        "detalhe",
                        "invalid_grant: rode `carteiro autorizar` de novo",
                    )],
                );
            }
            return Err(Falha::Temporaria(format!(
                "token: HTTP {}",
                status.as_u16()
            )));
        }
        let t: RespostaToken = serde_json::from_str(&corpo)
            .map_err(|e| Falha::Temporaria(format!("token ilegível: {e}")))?;
        self.token_ok.store(true, Ordering::Relaxed);
        let validade =
            Instant::now() + Duration::from_secs(t.expires_in).saturating_sub(MARGEM_TOKEN);
        *acesso = Some((t.access_token.clone(), validade));
        Ok(t.access_token)
    }

    async fn esquecer_token(&self) {
        *self.acesso.lock().await = None;
    }

    async fn carregar_labels(&self, l: &mut Labels) -> Result<(), Falha> {
        let token = self.token().await?;
        let r = self
            .http
            .get(format!("{}/gmail/v1/users/me/labels", self.api))
            .bearer_auth(&token)
            .send()
            .await
            .map_err(erro_rede)?;
        if r.status() == StatusCode::UNAUTHORIZED {
            self.esquecer_token().await;
        }
        let lista: ListaLabels = json_ou_falha(r).await?;
        l.ids = lista
            .labels
            .into_iter()
            .map(|x| (x.name.to_lowercase(), x.id))
            .collect();
        l.carregadas = true;
        Ok(())
    }

    async fn criar_label(&self, nome: &str) -> Result<String, Falha> {
        let token = self.token().await?;
        let r = self
            .http
            .post(format!("{}/gmail/v1/users/me/labels", self.api))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "name": nome,
                "labelListVisibility": "labelShow",
                "messageListVisibility": "show",
            }))
            .send()
            .await
            .map_err(erro_rede)?;
        let l: Label = json_ou_falha(r).await?;
        crate::log::info("label_criada", &[("label", nome), ("id", &l.id)]);
        Ok(l.id)
    }

    /// Ids das labels pelo nome, criando as que faltam (o pai antes do
    /// filho). Passado o limite do dia, um subendereço novo fica com a label
    /// da base.
    pub async fn ids_labels(&self, nomes: &[String]) -> Result<Vec<String>, Falha> {
        let mut l = self.labels.lock().await;
        if !l.carregadas {
            self.carregar_labels(&mut l).await?;
        }
        let hoje = dia_atual();
        if l.dia != hoje {
            l.dia = hoje;
            l.criadas_hoje = 0;
        }
        let mut ids = Vec::new();
        for nome in nomes {
            let mut alvo = nome.as_str();
            if !l.ids.contains_key(alvo)
                && alvo.contains('/')
                && l.criadas_hoje >= self.labels_por_dia
            {
                crate::log::aviso("label_limite_do_dia", &[("label", alvo)]);
                alvo = alvo.split('/').next().unwrap_or(alvo);
            }
            let mut parcial = String::new();
            for parte in alvo.split('/') {
                if !parcial.is_empty() {
                    parcial.push('/');
                }
                parcial.push_str(parte);
                if l.ids.contains_key(&parcial) {
                    continue;
                }
                match self.criar_label(&parcial).await {
                    Ok(id) => {
                        // as labels base vêm da config (finitas); só as de
                        // subendereço contam para o limite do dia
                        if parcial.contains('/') {
                            l.criadas_hoje += 1;
                        }
                        l.ids.insert(parcial.clone(), id);
                    }
                    Err(e) => {
                        // 409: outra sessão criou; a lista recarrega na próxima
                        l.carregadas = false;
                        return Err(e);
                    }
                }
            }
            if let Some(id) = l.ids.get(alvo)
                && !ids.contains(id)
            {
                ids.push(id.clone());
            }
        }
        Ok(ids)
    }

    async fn invalidar_labels(&self) {
        self.labels.lock().await.carregadas = false;
    }

    /// Importa `cabeçalhos + .eml`. Devolve o id da mensagem no Gmail.
    pub async fn importar(&self, meta: &Meta, eml: &Path) -> Result<String, Falha> {
        let mut label_ids = vec!["INBOX".to_string(), "UNREAD".to_string()];
        label_ids.extend(self.ids_labels(&meta.labels).await?);
        let metadados = serde_json::json!({ "labelIds": label_ids }).to_string();
        let tamanho_eml = tokio::fs::metadata(eml)
            .await
            .map_err(|e| Falha::Temporaria(format!("spool: {e}")))?
            .len();
        let total = meta.cabecalhos.len() as u64 + tamanho_eml;

        let mut tentativa_401 = false;
        loop {
            let token = self.token().await?;
            let r = if total <= LIMITE_MULTIPART {
                self.multipart(&token, &metadados, meta, eml, total).await
            } else {
                self.resumable(&token, &metadados, meta, eml, total).await
            }?;
            let status = r.status();
            if status == StatusCode::UNAUTHORIZED && !tentativa_401 {
                tentativa_401 = true;
                self.esquecer_token().await;
                continue;
            }
            if status.is_success() {
                let i: Importada = r
                    .json()
                    .await
                    .map_err(|e| Falha::Temporaria(format!("resposta ilegível: {e}")))?;
                return Ok(i.id);
            }
            let corpo = r.text().await.unwrap_or_default();
            if corpo.to_lowercase().contains("label") {
                self.invalidar_labels().await;
            }
            return Err(classificar(status, &corpo));
        }
    }

    fn url_import(&self, tipo: &str) -> String {
        format!(
            "{}/upload/gmail/v1/users/me/messages/import?uploadType={tipo}&internalDateSource=dateHeader",
            self.api
        )
    }

    async fn multipart(
        &self,
        token: &str,
        metadados: &str,
        meta: &Meta,
        eml: &Path,
        total: u64,
    ) -> Result<Response, Falha> {
        let fronteira = format!("carteiro-{}", aleatorio_hex(16));
        let inicio = format!(
            "--{fronteira}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadados}\r\n\
             --{fronteira}\r\nContent-Type: message/rfc822\r\n\r\n{}",
            meta.cabecalhos
        );
        let fim = format!("\r\n--{fronteira}--\r\n");
        let tamanho =
            inicio.len() as u64 + (total - meta.cabecalhos.len() as u64) + fim.len() as u64;
        let corpo = corpo_em_fluxo(inicio.into_bytes(), eml, fim.into_bytes()).await?;
        self.http
            .post(self.url_import("multipart"))
            .bearer_auth(token)
            .header(
                header::CONTENT_TYPE,
                format!("multipart/related; boundary={fronteira}"),
            )
            .header(header::CONTENT_LENGTH, tamanho)
            .body(corpo)
            .send()
            .await
            .map_err(erro_rede)
    }

    async fn resumable(
        &self,
        token: &str,
        metadados: &str,
        meta: &Meta,
        eml: &Path,
        total: u64,
    ) -> Result<Response, Falha> {
        let r = self
            .http
            .post(self.url_import("resumable"))
            .bearer_auth(token)
            .header(header::CONTENT_TYPE, "application/json; charset=UTF-8")
            .header("X-Upload-Content-Type", "message/rfc822")
            .header("X-Upload-Content-Length", total)
            .body(metadados.to_string())
            .send()
            .await
            .map_err(erro_rede)?;
        if !r.status().is_success() {
            return Ok(r);
        }
        let sessao = r
            .headers()
            .get(header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| Falha::Temporaria("resumable sem Location".into()))?
            .to_string();
        let corpo = corpo_em_fluxo(meta.cabecalhos.clone().into_bytes(), eml, Vec::new()).await?;
        self.http
            .put(sessao)
            .header(header::CONTENT_TYPE, "message/rfc822")
            .header(header::CONTENT_LENGTH, total)
            .body(corpo)
            .send()
            .await
            .map_err(erro_rede)
    }
}

async fn json_ou_falha<T: serde::de::DeserializeOwned>(r: Response) -> Result<T, Falha> {
    let status = r.status();
    if !status.is_success() {
        let corpo = r.text().await.unwrap_or_default();
        return Err(Falha::Temporaria(format!(
            "HTTP {}: {}",
            status.as_u16(),
            corpo.chars().take(200).collect::<String>()
        )));
    }
    r.json()
        .await
        .map_err(|e| Falha::Temporaria(format!("resposta ilegível: {e}")))
}

/// `inicio`, o arquivo em pedaços de 64 KiB, `fim`.
async fn corpo_em_fluxo(inicio: Vec<u8>, eml: &Path, fim: Vec<u8>) -> Result<Body, Falha> {
    use tokio::io::AsyncReadExt;
    let arquivo = tokio::fs::File::open(eml)
        .await
        .map_err(|e| Falha::Temporaria(format!("spool: {e}")))?;
    let meio = futures_util::stream::unfold(arquivo, |mut f| async move {
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok::<_, std::io::Error>(buf), f))
            }
            Err(e) => Some((Err(e), f)),
        }
    });
    let fluxo = futures_util::stream::once(async move { Ok(inicio) })
        .chain(meio)
        .chain(futures_util::stream::once(async move { Ok(fim) }));
    Ok(Body::wrap_stream(fluxo))
}

pub fn aleatorio_hex(bytes: usize) -> String {
    use ring::rand::SecureRandom;
    let mut b = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut b)
        .expect("gerador aleatório do sistema");
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn classificacao_dos_erros() {
        let p = |s: u16, c: &str| classificar(StatusCode::from_u16(s).unwrap(), c);
        assert!(matches!(
            p(
                400,
                r#"{"error":{"status":"INVALID_ARGUMENT","message":"Invalid message"}}"#
            ),
            Falha::Permanente(_)
        ));
        assert!(matches!(p(413, ""), Falha::Permanente(_)));
        assert!(matches!(
            p(
                400,
                r#"{"error":{"status":"INVALID_ARGUMENT","message":"Invalid label: Label_9"}}"#
            ),
            Falha::Temporaria(_)
        ));
        assert!(matches!(
            p(400, r#"{"error":{"status":"FAILED_PRECONDITION"}}"#),
            Falha::Temporaria(_)
        ));
        for s in [401, 403, 404, 429, 500, 502, 503] {
            assert!(matches!(p(s, "x"), Falha::Temporaria(_)), "{s}");
        }
    }

    #[test]
    fn hex_aleatorio() {
        let a = aleatorio_hex(16);
        assert_eq!(a.len(), 32);
        assert_ne!(a, aleatorio_hex(16));
    }
}

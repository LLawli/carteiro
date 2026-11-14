//! `carteiro autorizar`: fluxo OAuth com código de autorização e PKCE. Mostra
//! a URL, recebe o código no retorno e grava o refresh token (0600).
//!
//! - Padrão (cliente OAuth "Desktop"): o retorno é o loopback,
//!   `http://127.0.0.1:<porta>`, então o navegador tem de estar na mesma
//!   máquina (ou usar `ssh -L <porta>:127.0.0.1:<porta>` com `--porta`).
//! - Login feito em outro aparelho (cliente OAuth "Web application"):
//!   `--retorno https://host/caminho` é a URL pública que um proxy reverso
//!   encaminha para `--escutar`. A URL tem de estar nas "Authorized redirect
//!   URIs" do cliente.
//!
//! O device flow não serve: o Google não libera os escopos do Gmail nele.

use crate::gmail::{ClienteOAuth, ESCOPOS, aleatorio_hex};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Percent-encoding da RFC 3986 (só os não reservados passam).
pub fn codificar(s: &str) -> String {
    let mut r = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            r.push(b as char);
        } else {
            r.push_str(&format!("%{b:02X}"));
        }
    }
    r
}

pub fn decodificar(s: &str) -> String {
    let b = s.as_bytes();
    let mut r = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(x) => {
                        r.push(x);
                        i += 3;
                        continue;
                    }
                    Err(_) => r.push(b'%'),
                }
            }
            b'+' => r.push(b' '),
            x => r.push(x),
        }
        i += 1;
    }
    String::from_utf8_lossy(&r).into_owned()
}

/// Parâmetros da query de `GET /?a=1&b=2 HTTP/1.1`.
pub fn parametros(linha_pedido: &str) -> Vec<(String, String)> {
    let alvo = linha_pedido.split_ascii_whitespace().nth(1).unwrap_or("");
    let Some((_, q)) = alvo.split_once('?') else {
        return Vec::new();
    };
    q.split('&')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (decodificar(k), decodificar(v)))
        .collect()
}

pub fn desafio_pkce(verificador: &str) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, verificador.as_bytes());
    URL_SAFE_NO_PAD.encode(d.as_ref())
}

#[derive(Deserialize)]
struct RespostaCodigo {
    refresh_token: Option<String>,
    scope: Option<String>,
}

/// Onde esperar o retorno e para onde o Google manda o navegador.
#[derive(Debug, PartialEq, Eq)]
pub struct Opcoes {
    pub escutar: SocketAddr,
    /// `None`: o loopback, `http://127.0.0.1:<porta>`.
    pub retorno: Option<String>,
}

impl Opcoes {
    /// Lê `[--porta N] [--escutar ENDEREÇO:PORTA] [--retorno URL]`.
    pub fn ler(args: &[&str]) -> Result<Opcoes, String> {
        let mut o = Opcoes {
            escutar: SocketAddr::from(([127, 0, 0, 1], 0)),
            retorno: None,
        };
        let mut i = args.iter();
        while let Some(a) = i.next() {
            let valor = i.next().ok_or_else(|| format!("{a} sem valor"))?;
            match *a {
                "--porta" => {
                    let p: u16 = valor
                        .parse()
                        .map_err(|_| format!("porta inválida: {valor}"))?;
                    o.escutar.set_port(p);
                }
                "--escutar" => {
                    o.escutar = valor
                        .parse()
                        .map_err(|_| format!("endereço inválido: {valor} (ex.: 0.0.0.0:8765)"))?;
                }
                "--retorno" => {
                    if !valor.starts_with("https://") {
                        return Err(format!("--retorno precisa ser https: {valor}"));
                    }
                    o.retorno = Some(valor.to_string());
                }
                _ => return Err(format!("opção desconhecida: {a}")),
            }
        }
        if o.retorno.is_some() && o.escutar.port() == 0 {
            return Err("--retorno precisa de uma porta fixa em --escutar ou --porta".into());
        }
        Ok(o)
    }
}

pub async fn executar(cliente: &Path, saida: &Path, opcoes: Opcoes) -> Result<(), String> {
    let c = ClienteOAuth::ler(cliente)?;
    let ouvinte = TcpListener::bind(opcoes.escutar)
        .await
        .map_err(|e| format!("{}: {e}", opcoes.escutar))?;
    let porta = ouvinte.local_addr().map_err(|e| e.to_string())?.port();
    let redirecionar = opcoes
        .retorno
        .unwrap_or_else(|| format!("http://127.0.0.1:{porta}"));
    let verificador = URL_SAFE_NO_PAD.encode(aleatorio_hex(32));
    let estado = aleatorio_hex(16);
    let url = format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent&code_challenge={}&code_challenge_method=S256&state={}",
        c.auth_uri,
        codificar(&c.client_id),
        codificar(&redirecionar),
        codificar(ESCOPOS),
        desafio_pkce(&verificador),
        estado
    );
    eprintln!("Abra no navegador e autorize com a conta do Gmail de destino:\n\n{url}\n");
    eprintln!(
        "Esperando o retorno em {redirecionar} (escutando em {}:{porta}) ...",
        opcoes.escutar.ip()
    );

    let codigo = loop {
        let (mut s, _) = ouvinte.accept().await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        let n = s.read(&mut buf).await.map_err(|e| e.to_string())?;
        let pedido = String::from_utf8_lossy(&buf[..n]);
        let primeira = pedido.lines().next().unwrap_or("");
        let p = parametros(primeira);
        let pega = |k: &str| p.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
        let (texto, resultado) = match (pega("state"), pega("code"), pega("error")) {
            (_, _, Some(e)) => (format!("Autorização recusada: {e}"), Some(Err(e))),
            (Some(st), Some(cod), _) if st == estado => {
                ("Pronto. Pode fechar esta aba.".to_string(), Some(Ok(cod)))
            }
            (Some(_), Some(_), _) => ("state não confere.".to_string(), None),
            _ => ("Nada aqui.".to_string(), None),
        };
        let corpo = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{texto}",
            texto.len()
        );
        let _ = s.write_all(corpo.as_bytes()).await;
        match resultado {
            Some(Ok(c)) => break c,
            Some(Err(e)) => return Err(format!("autorização recusada: {e}")),
            None => continue,
        }
    };

    let r = crate::gmail::http()
        .post(&c.token_uri)
        .form(&[
            ("code", codigo.as_str()),
            ("client_id", c.client_id.as_str()),
            ("client_secret", c.client_secret.as_str()),
            ("redirect_uri", redirecionar.as_str()),
            ("grant_type", "authorization_code"),
            ("code_verifier", verificador.as_str()),
        ])
        .send()
        .await
        .map_err(|e| format!("trocando o código: {e}"))?;
    let status = r.status();
    let corpo = r.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("trocando o código: HTTP {status}"));
    }
    let t: RespostaCodigo = serde_json::from_str(&corpo).map_err(|e| e.to_string())?;
    let refresh = t
        .refresh_token
        .ok_or("o Google não devolveu refresh_token (revogue o acesso do app e tente de novo)")?;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(saida)
        .map_err(|e| format!("{}: {e}", saida.display()))?;
    f.write_all(
        serde_json::json!({ "refresh_token": refresh })
            .to_string()
            .as_bytes(),
    )
    .and_then(|_| f.sync_all())
    .map_err(|e| format!("{}: {e}", saida.display()))?;
    eprintln!(
        "Refresh token gravado em {} (0600). Escopos: {}",
        saida.display(),
        t.scope.unwrap_or_default()
    );
    Ok(())
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn codifica_e_decodifica() {
        assert_eq!(codificar("a b/c:d"), "a%20b%2Fc%3Ad");
        assert_eq!(decodificar("4%2F0Ab-x_y"), "4/0Ab-x_y");
        assert_eq!(decodificar("a+b%"), "a b%");
        assert_eq!(decodificar("%zz"), "%zz");
    }

    #[test]
    fn le_a_query_do_retorno() {
        let p = parametros("GET /?state=abc&code=4%2F0A&scope=x HTTP/1.1");
        assert_eq!(p[0], ("state".into(), "abc".into()));
        assert_eq!(p[1], ("code".into(), "4/0A".into()));
        assert!(parametros("GET / HTTP/1.1").is_empty());
    }

    #[test]
    fn opcoes_do_retorno() {
        assert_eq!(
            Opcoes::ler(&[]).unwrap(),
            Opcoes {
                escutar: "127.0.0.1:0".parse().unwrap(),
                retorno: None
            }
        );
        assert_eq!(
            Opcoes::ler(&["--porta", "8765"]).unwrap().escutar,
            "127.0.0.1:8765".parse().unwrap()
        );
        let o = Opcoes::ler(&[
            "--escutar",
            "0.0.0.0:8765",
            "--retorno",
            "https://mx.exemplo.com/carteiro/oauth",
        ])
        .unwrap();
        assert_eq!(o.escutar, "0.0.0.0:8765".parse().unwrap());
        assert_eq!(
            o.retorno.as_deref(),
            Some("https://mx.exemplo.com/carteiro/oauth")
        );
        assert!(
            Opcoes::ler(&["--retorno", "https://x.y/z"]).is_err(),
            "sem porta fixa"
        );
        assert!(Opcoes::ler(&["--retorno", "http://x.y/z", "--porta", "1"]).is_err());
        assert!(Opcoes::ler(&["--porta"]).is_err());
        assert!(Opcoes::ler(&["--porta", "x"]).is_err());
        assert!(Opcoes::ler(&["--outra", "1"]).is_err());
    }

    #[test]
    fn retorno_por_proxy_com_caminho() {
        let p = parametros("GET /carteiro/oauth?state=abc&code=4%2F0A HTTP/1.1");
        assert_eq!(p[1], ("code".into(), "4/0A".into()));
    }

    #[test]
    fn pkce_s256() {
        // conferido com: base64.urlsafe_b64encode(hashlib.sha256(v).digest())
        assert_eq!(
            desafio_pkce("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gXFWthAAA"),
            "_R08BHS4O_8f38E5XtOWzQyfm4TT54n6MqMGI0fUZIw"
        );
    }
}

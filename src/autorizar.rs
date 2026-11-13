//! `carteiro autorizar`: fluxo OAuth de app instalado com redirecionamento
//! para o loopback e PKCE. Abre a URL no navegador, recebe o código em
//! `http://127.0.0.1:<porta>` e grava o refresh token (0600).
//!
//! O device flow não serve: o Google não libera os escopos do Gmail nele.
//! Numa máquina sem navegador, rode aqui e copie o arquivo, ou use
//! `ssh -L <porta>:127.0.0.1:<porta>` com `--porta`.

use crate::gmail::{ClienteOAuth, ESCOPOS, aleatorio_hex};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use std::io::Write;
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

pub async fn executar(cliente: &Path, saida: &Path, porta: u16) -> Result<(), String> {
    let c = ClienteOAuth::ler(cliente)?;
    let ouvinte = TcpListener::bind(("127.0.0.1", porta))
        .await
        .map_err(|e| format!("127.0.0.1:{porta}: {e}"))?;
    let porta = ouvinte.local_addr().map_err(|e| e.to_string())?.port();
    let redirecionar = format!("http://127.0.0.1:{porta}");
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
    eprintln!("Esperando o retorno em {redirecionar} ...");

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
    fn pkce_s256() {
        // conferido com: base64.urlsafe_b64encode(hashlib.sha256(v).digest())
        assert_eq!(
            desafio_pkce("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gXFWthAAA"),
            "_R08BHS4O_8f38E5XtOWzQyfm4TT54n6MqMGI0fUZIw"
        );
    }
}

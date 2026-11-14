//! Caminhos do `MAIL FROM`/`RCPT TO`, validação do destinatário e a label do
//! subendereço.

use crate::config::Caixa;

/// Caminho e parâmetros ESMTP de `FROM:<a@b> SIZE=10 BODY=8BITMIME`.
#[derive(Debug, PartialEq, Eq)]
pub struct Caminho {
    pub endereco: String,
    pub parametros: Vec<(String, Option<String>)>,
}

const TAMANHO_MAXIMO_ENDERECO: usize = 320;

/// Lê o argumento de MAIL (`prefixo = "FROM:"`) ou RCPT (`"TO:"`).
pub fn caminho(arg: &str, prefixo: &str) -> Option<Caminho> {
    let arg = arg.trim();
    if arg.len() < prefixo.len() || !arg[..prefixo.len()].eq_ignore_ascii_case(prefixo) {
        return None;
    }
    let resto = arg[prefixo.len()..].trim_start();
    let resto = resto.strip_prefix('<')?;
    let fim = resto.find('>')?;
    let mut endereco = &resto[..fim];
    // rota de origem obsoleta: <@a,@b:usuario@dominio>
    if endereco.starts_with('@') {
        endereco = &endereco[endereco.find(':')? + 1..];
    }
    if endereco.len() > TAMANHO_MAXIMO_ENDERECO
        || endereco.chars().any(|c| c.is_control() || c == ' ')
    {
        return None;
    }
    if !endereco.is_empty() {
        let (local, dominio) = endereco.rsplit_once('@')?;
        if local.is_empty() || dominio.is_empty() {
            return None;
        }
    }
    let parametros = resto[fim + 1..]
        .split_ascii_whitespace()
        .map(|p| match p.split_once('=') {
            Some((k, v)) => (k.to_ascii_uppercase(), Some(v.to_string())),
            None => (p.to_ascii_uppercase(), None),
        })
        .collect();
    Some(Caminho {
        endereco: endereco.to_string(),
        parametros,
    })
}

/// Um destinatário aceito: `base` está nos `enderecos` da caixa de índice
/// `caixa` e `sub` é o que vem depois do `+`, se houver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destino {
    pub caixa: usize,
    pub endereco: String,
    pub base: String,
    pub sub: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recusa {
    /// Domínio de nenhuma caixa: aceitar seria open relay.
    ForaDoDominio,
    /// Domínio certo, caixa que não existe.
    Desconhecido,
}

pub fn destino(endereco: &str, caixas: &[Caixa]) -> Result<Destino, Recusa> {
    let (local, dom) = endereco.rsplit_once('@').ok_or(Recusa::ForaDoDominio)?;
    let dom = dom.trim_end_matches('.').to_ascii_lowercase();
    if !caixas.iter().any(|c| c.dominio == dom) {
        return Err(Recusa::ForaDoDominio);
    }
    if local.starts_with('"') {
        return Err(Recusa::Desconhecido);
    }
    let local = local.to_ascii_lowercase();
    let (base, sub) = match local.split_once('+') {
        Some((b, s)) => (b.to_string(), Some(s.to_string())),
        None => (local.clone(), None),
    };
    let caixa = caixas
        .iter()
        .position(|c| c.dominio == dom && c.enderecos.contains(&base))
        .ok_or(Recusa::Desconhecido)?;
    Ok(Destino {
        caixa,
        endereco: format!("{local}@{dom}"),
        base,
        sub: sub.filter(|s| !s.is_empty()),
    })
}

const TAMANHO_MAXIMO_SUB: usize = 32;

impl Destino {
    /// `contato`, ou `contato/servico` para `contato+servico@`. Um sufixo com
    /// caractere fora de `[a-z0-9._-]` ou longo demais fica só com a base:
    /// a mensagem chega do mesmo jeito.
    pub fn label(&self) -> String {
        match &self.sub {
            Some(s)
                if s.len() <= TAMANHO_MAXIMO_SUB
                    && s.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    }) =>
            {
                format!("{}/{}", self.base, s)
            }
            _ => self.base.clone(),
        }
    }
}

/// HELO/EHLO aceitável: nome com ponto (FQDN) ou literal de endereço.
pub fn helo_valido(arg: &str) -> bool {
    if let Some(literal) = arg.strip_prefix('[').and_then(|a| a.strip_suffix(']')) {
        let ip = literal
            .strip_prefix("IPv6:")
            .or_else(|| literal.strip_prefix("ipv6:"))
            .unwrap_or(literal);
        return ip.parse::<std::net::IpAddr>().is_ok();
    }
    let nome = arg.trim_end_matches('.');
    nome.len() <= 253
        && nome.contains('.')
        && nome.split('.').all(|r| {
            !r.is_empty()
                && r.len() <= 63
                && !r.starts_with('-')
                && !r.ends_with('-')
                && r.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}

#[cfg(test)]
mod testes {
    use super::*;

    fn caixa(nome: &str, dominio: &str, enderecos: &[&str]) -> Caixa {
        Caixa {
            nome: nome.into(),
            dominio: dominio.into(),
            enderecos: enderecos.iter().map(|e| e.to_string()).collect(),
            cliente_oauth: "c.json".into(),
            token: "t.json".into(),
            nunca_spam: false,
        }
    }

    fn end() -> Vec<Caixa> {
        vec![caixa("eu", "exemplo.com", &["contato", "pix"])]
    }

    #[test]
    fn le_caminho_e_parametros() {
        let c = caminho("FROM:<a@b.c> SIZE=100 BODY=8BITMIME", "FROM:").unwrap();
        assert_eq!(c.endereco, "a@b.c");
        assert_eq!(
            c.parametros,
            [
                ("SIZE".into(), Some("100".into())),
                ("BODY".into(), Some("8BITMIME".into()))
            ]
        );
        assert_eq!(caminho("from: <>", "FROM:").unwrap().endereco, "");
        assert_eq!(
            caminho("TO:<@x.y,@z.w:u@d.e>", "TO:").unwrap().endereco,
            "u@d.e"
        );
        assert_eq!(
            caminho("TO:<u@d.e> SMTPUTF8", "TO:").unwrap().parametros,
            [("SMTPUTF8".into(), None)]
        );
        assert!(caminho("FROM:a@b.c", "FROM:").is_none());
        assert!(caminho("TO:<a@b.c>", "FROM:").is_none());
        assert!(caminho("FROM:<semarroba>", "FROM:").is_none());
        assert!(caminho("FROM:<a b@c.d>", "FROM:").is_none());
        assert!(caminho("FROM:<@c.d>", "FROM:").is_none());
    }

    #[test]
    fn destino_aceita_so_o_dominio_e_os_enderecos() {
        let d = destino("Contato+Banco@Exemplo.COM", &end()).unwrap();
        assert_eq!(d.base, "contato");
        assert_eq!(d.sub.as_deref(), Some("banco"));
        assert_eq!(d.endereco, "contato+banco@exemplo.com");
        assert_eq!(d.label(), "contato/banco");
        assert_eq!(destino("pix@exemplo.com.", &end()).unwrap().label(), "pix");
        assert_eq!(destino("pix+@exemplo.com", &end()).unwrap().sub, None);
        assert_eq!(
            destino("contato@gmail.com", &end()),
            Err(Recusa::ForaDoDominio)
        );
        assert_eq!(
            destino("contato@sub.exemplo.com", &end()),
            Err(Recusa::ForaDoDominio)
        );
        assert_eq!(
            destino("admin@exemplo.com", &end()),
            Err(Recusa::Desconhecido)
        );
        assert_eq!(
            destino("\"contato\"@exemplo.com", &end()),
            Err(Recusa::Desconhecido)
        );
        assert_eq!(destino("semarroba", &end()), Err(Recusa::ForaDoDominio));
    }

    #[test]
    fn cada_endereco_na_sua_caixa() {
        let caixas = vec![
            caixa("eu", "exemplo.com", &["contato", "pix"]),
            caixa("outra", "outro.app", &["suporte"]),
            caixa("terceira", "exemplo.com", &["vendas"]),
        ];
        let c = |e: &str| destino(e, &caixas).map(|d| d.caixa);
        assert_eq!(c("contato@exemplo.com"), Ok(0));
        assert_eq!(c("Suporte+Loja@Outro.App"), Ok(1));
        assert_eq!(c("vendas@exemplo.com"), Ok(2));
        assert_eq!(c("contato@outro.app"), Err(Recusa::Desconhecido));
        assert_eq!(c("suporte@exemplo.com"), Err(Recusa::Desconhecido));
        assert_eq!(c("suporte@terceiro.app"), Err(Recusa::ForaDoDominio));
        assert_eq!(
            destino("suporte@outro.app.", &caixas).unwrap().endereco,
            "suporte@outro.app"
        );
    }

    #[test]
    fn sufixo_estranho_fica_com_a_label_base() {
        let d = |e: &str| destino(e, &end()).unwrap().label();
        assert_eq!(d("contato+a.b_c-1@exemplo.com"), "contato/a.b_c-1");
        assert_eq!(d("contato+a/b@exemplo.com"), "contato");
        assert_eq!(d("contato+çã@exemplo.com"), "contato");
        assert_eq!(
            d(&format!("contato+{}@exemplo.com", "a".repeat(33))),
            "contato"
        );
    }

    #[test]
    fn helo() {
        assert!(helo_valido("mail.exemplo.com"));
        assert!(helo_valido("mail.exemplo.com."));
        assert!(helo_valido("[192.0.2.1]"));
        assert!(helo_valido("[IPv6:2001:db8::1]"));
        assert!(!helo_valido("localhost"));
        assert!(!helo_valido("WIN-ABC123"));
        assert!(!helo_valido(""));
        assert!(!helo_valido("a..b"));
        assert!(!helo_valido("-a.b"));
        assert!(!helo_valido("[999.1.1.1]"));
        assert!(!helo_valido("a b.c"));
    }
}

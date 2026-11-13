//! Log de uma linha por evento, em `chave=valor`, no stderr. Sob o systemd
//! (`JOURNAL_STREAM` definido) cada linha leva o prefixo `<N>` de prioridade,
//! que o journald lê sozinho. Nada do corpo das mensagens passa por aqui.

use std::fmt::Write as _;
use std::sync::OnceLock;

pub const ERRO: u8 = 3;
pub const AVISO: u8 = 4;
pub const INFO: u8 = 6;

fn no_journal() -> bool {
    static J: OnceLock<bool> = OnceLock::new();
    *J.get_or_init(|| std::env::var_os("JOURNAL_STREAM").is_some())
}

pub fn emitir(nivel: u8, linha: &str) {
    if no_journal() {
        eprintln!("<{nivel}>{linha}");
    } else {
        let rotulo = match nivel {
            ERRO => "ERRO",
            AVISO => "AVISO",
            _ => "INFO",
        };
        eprintln!("{rotulo} {linha}");
    }
}

/// Um valor seguro para o log: sem caracteres de controle, entre aspas quando
/// tem espaço, aspas ou `=`, e cortado em 200 caracteres.
pub fn valor(v: &str) -> String {
    let limpo: String = v
        .chars()
        .take(200)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect();
    if limpo.is_empty() {
        return "\"\"".into();
    }
    if limpo.contains([' ', '"', '=']) {
        let mut s = String::with_capacity(limpo.len() + 2);
        s.push('"');
        for c in limpo.chars() {
            if c == '"' {
                s.push('\\');
            }
            s.push(c);
        }
        s.push('"');
        s
    } else {
        limpo
    }
}

/// Monta `evento=<evento> k1=v1 k2=v2`, passando cada valor por [`valor`].
pub fn linha(evento: &str, campos: &[(&str, &str)]) -> String {
    let mut s = format!("evento={evento}");
    for (k, v) in campos {
        let _ = write!(s, " {k}={}", valor(v));
    }
    s
}

pub fn info(evento: &str, campos: &[(&str, &str)]) {
    emitir(INFO, &linha(evento, campos));
}

pub fn aviso(evento: &str, campos: &[(&str, &str)]) {
    emitir(AVISO, &linha(evento, campos));
}

pub fn erro(evento: &str, campos: &[(&str, &str)]) {
    emitir(ERRO, &linha(evento, campos));
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn valor_sem_controle_e_com_aspas() {
        assert_eq!(valor("a@b.c"), "a@b.c");
        assert_eq!(valor("a b"), "\"a b\"");
        assert_eq!(valor("x\r\ny"), "x??y");
        assert_eq!(valor("a\"b c"), "\"a\\\"b c\"");
        assert_eq!(valor(""), "\"\"");
        assert_eq!(valor(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn linha_em_chave_valor() {
        assert_eq!(
            linha("recebida", &[("de", "a@b"), ("resultado", "fila")]),
            "evento=recebida de=a@b resultado=fila"
        );
    }
}

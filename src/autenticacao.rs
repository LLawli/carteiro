//! SPF, DKIM e DMARC do remetente, com o crate `mail-auth`.
//!
//! O Gmail recebe a mensagem pela API e não vê o IP de quem conectou aqui,
//! então não consegue avaliar SPF nem DMARC como faria numa entrega SMTP. Este
//! módulo faz essa parte: recusa o que falha no DMARC com `p=reject` (o que o
//! próprio Gmail faria) e grava o resultado num `Authentication-Results:`.
//! Erro de DNS nunca recusa nada.

use mail_auth::dmarc::Policy;
use mail_auth::dmarc::verify::DmarcParameters;
use mail_auth::hickory_resolver::system_conf::read_system_conf;
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{
    AuthenticatedMessage, AuthenticationResults, DmarcResult, IprevResult, MessageAuthenticator,
};
use std::net::IpAddr;

pub struct Verificador {
    auth: MessageAuthenticator,
    hostname: String,
}

pub struct Veredito {
    /// `Authentication-Results: ...\r\n`
    pub cabecalho: String,
    /// Motivo da recusa, quando o DMARC manda rejeitar.
    pub recusa: Option<String>,
    /// `spf=pass dkim=pass dmarc=pass`, para o log.
    pub resumo: String,
    /// Nome do PTR confirmado (iprev), para o `Received:`.
    pub rdns: Option<String>,
}

/// Rejeitar quando o domínio do From tem registro DMARC com `p=reject` e o
/// resultado geral é `fail`: nem DKIM nem SPF deram pass alinhado, e nenhum
/// esbarrou em erro de DNS (`DmarcOutput::result` põe `temperror` e
/// `permerror` acima de `fail`). Sem registro, o resultado é `none`.
pub fn deve_recusar(resultado: &DmarcResult, politica: Policy) -> bool {
    matches!(resultado, DmarcResult::Fail(_)) && politica == Policy::Reject
}

fn nome(r: &DmarcResult) -> &'static str {
    match r {
        DmarcResult::Pass => "pass",
        DmarcResult::Fail(_) => "fail",
        DmarcResult::TempError(_) => "temperror",
        DmarcResult::PermError(_) => "permerror",
        DmarcResult::None => "none",
    }
}

impl Verificador {
    pub fn new(hostname: &str) -> Result<Verificador, String> {
        // Os servidores do /etc/resolv.conf, mas sempre com EDNS: sem
        // `options edns0` lá (um resolv.conf estático, como o da netcup, não tem), o
        // hickory consulta sem EDNS, o TXT de SPF grande (o do paypal.com tem
        // 1120 bytes) volta truncado em 512 e vira `temperror`, e aí nada é
        // recusado por DMARC. A nova tentativa por TCP cobre o que passar até
        // do limite do EDNS.
        let (servidores, mut opcoes) =
            read_system_conf().map_err(|e| format!("lendo /etc/resolv.conf: {e}"))?;
        opcoes.edns0 = true;
        opcoes.try_tcp_on_error = true;
        let auth = MessageAuthenticator::new(servidores, opcoes)
            .map_err(|e| format!("resolvedor DNS: {e}"))?;
        Ok(Verificador {
            auth,
            hostname: hostname.to_string(),
        })
    }

    pub async fn verificar(&self, bruto: &[u8], ip: IpAddr, helo: &str, de: &str) -> Veredito {
        let iprev = self.auth.verify_iprev(ip).await;
        let rdns = match iprev.result() {
            IprevResult::Pass => iprev
                .ptr
                .as_ref()
                .and_then(|p| p.first())
                .map(|n| n.trim_end_matches('.').to_string()),
            _ => None,
        };
        let spf_params = SpfParameters::mail_from(ip, helo, &self.hostname, de);
        let spf = self.auth.verify_spf(spf_params).await;
        let mut resultados =
            AuthenticationResults::new(&self.hostname).with_spf_result(&spf, &spf_params);
        resultados = resultados.with_iprev_result(&iprev, ip);

        let Some(msg) = AuthenticatedMessage::parse(bruto) else {
            return Veredito {
                cabecalho: format!("Authentication-Results: {resultados}\r\n"),
                recusa: None,
                resumo: "dmarc=none (mensagem ilegível)".into(),
                rdns,
            };
        };
        let dkim = self.auth.verify_dkim(&msg).await;
        let de_cabecalho = msg.first_from_address().to_string();
        let dominio_envelope = match de.rsplit_once('@') {
            Some((_, d)) => d.to_string(),
            None => helo.to_string(),
        };
        let dmarc = self
            .auth
            .verify_dmarc(DmarcParameters::new(&msg, &dkim, &dominio_envelope, &spf))
            .await;
        resultados = resultados
            .with_dkim_results(&dkim, &de_cabecalho)
            .with_dmarc_result(&dmarc);

        let geral = dmarc.result();
        let resumo = format!(
            "spf={} dkim_alinhado={} spf_alinhado={} dmarc={} politica={}",
            spf.result(),
            nome(dmarc.dkim_result()),
            nome(dmarc.spf_result()),
            nome(&geral),
            dmarc.policy()
        );
        let recusa = deve_recusar(&geral, dmarc.policy())
            .then(|| format!("DMARC policy of {} is reject", dmarc.domain()));
        Veredito {
            cabecalho: format!("Authentication-Results: {resultados}\r\n"),
            recusa,
            resumo,
            rdns,
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;
    use mail_auth::Error;

    #[test]
    fn recusa_so_fail_com_reject() {
        let f = || DmarcResult::Fail(Error::NotAligned);
        assert!(deve_recusar(&f(), Policy::Reject));
        assert!(!deve_recusar(&f(), Policy::Quarantine));
        assert!(!deve_recusar(&f(), Policy::None));
        assert!(!deve_recusar(&DmarcResult::Pass, Policy::Reject));
        assert!(!deve_recusar(
            &DmarcResult::TempError(Error::NotAligned),
            Policy::Reject
        ));
        assert!(!deve_recusar(
            &DmarcResult::PermError(Error::NotAligned),
            Policy::Reject
        ));
        assert!(!deve_recusar(&DmarcResult::None, Policy::Reject));
    }
}

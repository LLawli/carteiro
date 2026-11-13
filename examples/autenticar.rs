//! Diagnóstico: roda o SPF e o DMARC do mail-auth com o resolvedor do sistema,
//! como o carteiro faz, e mostra o resultado detalhado.
//!
//!   autenticar <ip> <helo> <remetente>   (ex.: 192.0.2.1 teste.exemplo.com service@paypal.com)
use mail_auth::dmarc::verify::DmarcParameters;
use mail_auth::spf::verify::SpfParameters;
use mail_auth::{AuthenticatedMessage, MessageAuthenticator};
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (ip, helo, de) = (a[0].parse().unwrap(), a[1].as_str(), a[2].as_str());
    let (config, mut opcoes) =
        mail_auth::hickory_resolver::system_conf::read_system_conf().unwrap();
    println!(
        "opções do sistema: edns0={} try_tcp_on_error={} timeout={:?} attempts={}",
        opcoes.edns0, opcoes.try_tcp_on_error, opcoes.timeout, opcoes.attempts
    );
    // como o carteiro (src/autenticacao.rs); SEM_EDNS=1 reproduz o defeito
    if std::env::var_os("SEM_EDNS").is_none() {
        opcoes.edns0 = true;
        opcoes.try_tcp_on_error = true;
    }
    println!(
        "usando edns0={} try_tcp_on_error={}",
        opcoes.edns0, opcoes.try_tcp_on_error
    );
    let auth = MessageAuthenticator::new(config, opcoes).unwrap();
    for rodada in 1..=3 {
        let t0 = Instant::now();
        let p = SpfParameters::mail_from(ip, helo, "mx.lukakuuhaku.dev", de);
        let spf = auth.verify_spf(p).await;
        println!(
            "rodada {rodada}: spf={} ({:?}) em {:?}",
            spf.result(),
            spf,
            t0.elapsed()
        );
    }
    let bruto = format!("From: {de}\r\nSubject: x\r\n\r\nx\r\n");
    let msg = AuthenticatedMessage::parse(bruto.as_bytes()).unwrap();
    let dkim = auth.verify_dkim(&msg).await;
    let spf = auth
        .verify_spf(SpfParameters::mail_from(ip, helo, "mx.lukakuuhaku.dev", de))
        .await;
    let dominio = de.rsplit_once('@').unwrap().1;
    let dmarc = auth
        .verify_dmarc(DmarcParameters::new(&msg, &dkim, dominio, &spf))
        .await;
    println!("dmarc={:?} politica={}", dmarc.result(), dmarc.policy());
}

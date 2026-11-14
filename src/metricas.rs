//! Contadores e o arquivo `.prom` para o textfile collector do node-exporter
//! (sem servidor HTTP próprio). Regravado a cada [`INTERVALO`] por troca
//! atômica (grava ao lado e renomeia).

use crate::spool::Spool;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

pub const INTERVALO: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Metricas {
    pub conexoes: AtomicU64,
    pub importadas: AtomicU64,
    pub enfileiradas: AtomicU64,
    pub recusadas_rcpt: AtomicU64,
    pub recusadas_dmarc: AtomicU64,
    pub recusadas_gmail: AtomicU64,
    pub recusadas_protocolo: AtomicU64,
    pub falhas_import: AtomicU64,
    pub desistencias: AtomicU64,
    pub ultimo_import_ok: AtomicU64,
}

pub fn somar(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

impl Metricas {
    /// `tokens`: o nome de cada caixa e se o refresh token dela funciona.
    pub fn texto(&self, spool: &Spool, tokens: &[(String, bool)], agora: u64) -> String {
        let l = |c: &AtomicU64| c.load(Ordering::Relaxed);
        let e = spool.estatisticas();
        let mut s = String::new();
        let mut m = |nome: &str, tipo: &str, ajuda: &str, valor: String| {
            let _ = writeln!(s, "# HELP carteiro_{nome} {ajuda}");
            let _ = writeln!(s, "# TYPE carteiro_{nome} {tipo}");
            let _ = writeln!(s, "carteiro_{nome} {valor}");
        };
        m(
            "conexoes_total",
            "counter",
            "Conexões SMTP aceitas.",
            l(&self.conexoes).to_string(),
        );
        m(
            "importadas_total",
            "counter",
            "Mensagens importadas no Gmail.",
            l(&self.importadas).to_string(),
        );
        m(
            "enfileiradas_total",
            "counter",
            "Mensagens aceitas que foram para a fila.",
            l(&self.enfileiradas).to_string(),
        );
        m(
            "recusadas_rcpt_total",
            "counter",
            "RCPT recusados (outro domínio ou caixa inexistente).",
            l(&self.recusadas_rcpt).to_string(),
        );
        m(
            "recusadas_dmarc_total",
            "counter",
            "Mensagens recusadas por DMARC p=reject.",
            l(&self.recusadas_dmarc).to_string(),
        );
        m(
            "recusadas_gmail_total",
            "counter",
            "Mensagens recusadas pelo Gmail durante a sessão.",
            l(&self.recusadas_gmail).to_string(),
        );
        m(
            "recusadas_protocolo_total",
            "counter",
            "Conexões cortadas (fala antes do banner, limites, HELO).",
            l(&self.recusadas_protocolo).to_string(),
        );
        m(
            "falhas_import_total",
            "counter",
            "Tentativas de import com erro temporário.",
            l(&self.falhas_import).to_string(),
        );
        m(
            "desistencias_total",
            "counter",
            "Mensagens movidas para falhou/.",
            l(&self.desistencias).to_string(),
        );
        m(
            "ultimo_import_ok_timestamp_seconds",
            "gauge",
            "Hora do último import bem-sucedido.",
            l(&self.ultimo_import_ok).to_string(),
        );
        m(
            "fila_mensagens",
            "gauge",
            "Mensagens esperando import.",
            e.fila.to_string(),
        );
        m(
            "fila_idade_segundos",
            "gauge",
            "Idade da mensagem mais antiga da fila.",
            e.mais_antiga
                .map_or(0, |r| agora.saturating_sub(r))
                .to_string(),
        );
        m(
            "falhou_mensagens",
            "gauge",
            "Mensagens em falhou/ (olhar à mão).",
            e.falhou.to_string(),
        );
        // uma série por caixa: o alerta usa min() para pegar qualquer uma
        let _ = writeln!(
            s,
            "# HELP carteiro_token_ok 1 se o refresh token da caixa funciona."
        );
        let _ = writeln!(s, "# TYPE carteiro_token_ok gauge");
        for (caixa, ok) in tokens {
            let _ = writeln!(
                s,
                "carteiro_token_ok{{caixa=\"{caixa}\"}} {}",
                u8::from(*ok)
            );
        }
        s
    }
}

pub async fn gravar_sempre(
    caminho: PathBuf,
    metricas: Arc<Metricas>,
    spool: Arc<Spool>,
    contas: crate::entrega::Contas,
) {
    let tmp = caminho.with_extension("prom.tmp");
    loop {
        let tokens: Vec<(String, bool)> = contas
            .iter()
            .map(|(n, g)| (n.clone(), g.token_ok()))
            .collect();
        let texto = metricas.texto(&spool, &tokens, crate::data::agora_unix());
        // 0644: o node-exporter roda como nobody e o UMask do serviço é 0077
        let r = async {
            tokio::fs::write(&tmp, texto).await?;
            tokio::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o644))
                .await?;
            tokio::fs::rename(&tmp, &caminho).await
        }
        .await;
        if let Err(e) = r {
            crate::log::aviso(
                "metricas_sem_gravar",
                &[
                    ("arquivo", &caminho.display().to_string()),
                    ("erro", &e.to_string()),
                ],
            );
        }
        tokio::time::sleep(INTERVALO).await;
    }
}

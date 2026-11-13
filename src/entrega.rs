//! Do spool para o Gmail.
//!
//! No fim do DATA a sessão chama [`Entrega::entregar_agora`]: a primeira
//! tentativa roda numa task e a sessão espera até `espera_gmail`.
//! - importou a tempo: `250` com o id do Gmail, e sai da fila;
//! - o Gmail recusou a mensagem a tempo: `554`, e sai da fila (quem mandou
//!   recebe o bounce do próprio servidor dele);
//! - erro temporário ou demora: `250` com a mensagem na fila, e o
//!   [`Entrega::trabalhar`] tenta de novo com espera crescente até
//!   `desistir_apos`. Uma recusa depois do `250` vai para `falhou/`.
//!
//! A task da primeira tentativa continua depois que a sessão desiste de
//! esperar; o conjunto `em_voo` impede que o trabalhador tente a mesma
//! mensagem ao mesmo tempo.

use crate::gmail::{Falha, Gmail};
use crate::log;
use crate::metricas::{Metricas, somar};
use crate::spool::{Meta, Spool};
use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, oneshot};

/// Espera antes da tentativa n+1, em múltiplos de `espera_retentativa_s`
/// (com o padrão de 60 s: 1, 2, 5, 10, 30 min e depois a cada hora).
const MULTIPLOS: [u64; 6] = [1, 2, 5, 10, 30, 60];
/// Varredura da fila mesmo sem aviso, para o caso de algo escapar.
const VARREDURA: u64 = 300;

pub fn espera(tentativas: u32, base: u64) -> u64 {
    let i = (tentativas.max(1) - 1) as usize;
    MULTIPLOS[i.min(MULTIPLOS.len() - 1)] * base
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resultado {
    Importada(String),
    NaFila,
    Recusada(String),
}

pub struct Entrega {
    pub spool: Arc<Spool>,
    pub gmail: Arc<Gmail>,
    pub metricas: Arc<Metricas>,
    desistir_apos: Duration,
    espera_base: u64,
    em_voo: Mutex<HashSet<String>>,
    aviso: Notify,
}

/// Quem ainda espera o resultado da primeira tentativa. Trancado pela task ao
/// decidir e pela sessão ao desistir: só um dos dois vence.
struct Espera {
    abandonada: bool,
    resposta: Option<oneshot::Sender<Resultado>>,
}

impl Entrega {
    pub fn new(
        spool: Arc<Spool>,
        gmail: Arc<Gmail>,
        metricas: Arc<Metricas>,
        desistir_apos: Duration,
        espera_base: u64,
    ) -> Entrega {
        Entrega {
            spool,
            gmail,
            metricas,
            desistir_apos,
            espera_base,
            em_voo: Mutex::new(HashSet::new()),
            aviso: Notify::new(),
        }
    }

    fn em_voo(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.em_voo.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Confirma a mensagem no spool (fsync) e faz a primeira tentativa, com a
    /// sessão SMTP esperando no máximo `espera`. Erro aqui é erro de disco:
    /// nada foi aceito.
    pub async fn aceitar(
        self: &Arc<Self>,
        meta: Meta,
        espera: Duration,
    ) -> std::io::Result<Resultado> {
        // Reservado antes de aparecer na fila, senão o trabalhador poderia
        // pegá-lo junto com a primeira tentativa e importar duas vezes.
        self.em_voo().insert(meta.id.clone());
        let spool = Arc::clone(&self.spool);
        let m = meta.clone();
        let confirmado = tokio::task::spawn_blocking(move || spool.confirmar(&m))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(e)));
        if let Err(e) = confirmado {
            self.em_voo().remove(&meta.id);
            return Err(e);
        }
        let (tx, rx) = oneshot::channel();
        let estado = Arc::new(Mutex::new(Espera {
            abandonada: false,
            resposta: Some(tx),
        }));
        let eu = Arc::clone(self);
        let estado_task = Arc::clone(&estado);
        tokio::spawn(async move { eu.tentar(meta, Some(estado_task)).await });

        let mut rx = rx;
        Ok(match tokio::time::timeout(espera, &mut rx).await {
            Ok(Ok(r)) => r,
            _ => {
                let mut e = estado.lock().unwrap_or_else(|e| e.into_inner());
                e.abandonada = true;
                drop(e);
                // a task pode ter decidido entre o timeout e o lock
                rx.try_recv().unwrap_or(Resultado::NaFila)
            }
        })
    }

    /// Uma tentativa. Com `espera`, é a primeira (a sessão talvez ainda
    /// esteja esperando); sem, é do trabalhador e o `250` já foi dado.
    async fn tentar(self: Arc<Self>, mut meta: Meta, aguardando: Option<Arc<Mutex<Espera>>>) {
        let eml = self.spool.eml(&meta.id);
        let r = self.gmail.importar(&meta, &eml).await;

        let mut guarda = aguardando
            .as_ref()
            .map(|e| e.lock().unwrap_or_else(|x| x.into_inner()));
        let sessao_esperando = guarda.as_ref().is_some_and(|g| !g.abandonada);
        let resultado = match r {
            Ok(id_gmail) => {
                if let Err(e) = self.spool.remover(&meta.id) {
                    log::erro(
                        "spool_remover",
                        &[("id", &meta.id), ("erro", &e.to_string())],
                    );
                }
                somar(&self.metricas.importadas);
                self.metricas
                    .ultimo_import_ok
                    .store(crate::data::agora_unix(), Ordering::Relaxed);
                log::info(
                    "importada",
                    &[
                        ("id", &meta.id),
                        ("gmail_id", &id_gmail),
                        ("tentativas", &(meta.tentativas + 1).to_string()),
                    ],
                );
                Resultado::Importada(id_gmail)
            }
            Err(Falha::Permanente(m)) if sessao_esperando => {
                if let Err(e) = self.spool.remover(&meta.id) {
                    log::erro(
                        "spool_remover",
                        &[("id", &meta.id), ("erro", &e.to_string())],
                    );
                }
                somar(&self.metricas.recusadas_gmail);
                log::aviso("recusada_gmail", &[("id", &meta.id), ("erro", &m)]);
                Resultado::Recusada(m)
            }
            Err(Falha::Permanente(m)) => {
                meta.ultimo_erro = Some(m.clone());
                self.desistir(&meta, "recusada pelo Gmail depois do 250");
                Resultado::Recusada(m)
            }
            Err(Falha::Temporaria(m)) => {
                somar(&self.metricas.falhas_import);
                meta.tentativas += 1;
                let agora = crate::data::agora_unix();
                meta.proxima = agora + espera(meta.tentativas, self.espera_base);
                meta.ultimo_erro = Some(m.clone());
                if agora.saturating_sub(meta.recebida) >= self.desistir_apos.as_secs() {
                    self.desistir(&meta, "prazo esgotado");
                } else {
                    log::aviso(
                        "import_adiado",
                        &[
                            ("id", &meta.id),
                            ("tentativas", &meta.tentativas.to_string()),
                            (
                                "proxima_em_s",
                                &espera(meta.tentativas, self.espera_base).to_string(),
                            ),
                            ("erro", &m),
                        ],
                    );
                    if let Err(e) = self.spool.atualizar(&meta) {
                        log::erro(
                            "spool_atualizar",
                            &[("id", &meta.id), ("erro", &e.to_string())],
                        );
                    }
                }
                Resultado::NaFila
            }
        };
        if let Some(g) = guarda.as_mut()
            && let Some(tx) = g.resposta.take()
            && !g.abandonada
        {
            let _ = tx.send(resultado);
        }
        drop(guarda);
        self.em_voo().remove(&meta.id);
        self.aviso.notify_one();
    }

    fn desistir(&self, meta: &Meta, motivo: &str) {
        somar(&self.metricas.desistencias);
        log::erro(
            "desistencia",
            &[
                ("id", &meta.id),
                ("motivo", motivo),
                ("de", &meta.de),
                ("para", &meta.para.join(",")),
                ("erro", meta.ultimo_erro.as_deref().unwrap_or("")),
            ],
        );
        if let Err(e) = self.spool.mover_para_falhou(meta) {
            log::erro(
                "spool_falhou",
                &[("id", &meta.id), ("erro", &e.to_string())],
            );
        }
    }

    /// Laço das novas tentativas. Uma de cada vez: a fila só cresce quando o
    /// Gmail está fora, e aí paralelizar não ajuda.
    pub async fn trabalhar(self: Arc<Self>) {
        loop {
            let agora = crate::data::agora_unix();
            let mut proxima = agora + VARREDURA;
            let itens = self.spool.listar().unwrap_or_else(|e| {
                log::erro("spool_listar", &[("erro", &e.to_string())]);
                Vec::new()
            });
            for meta in itens {
                if meta.proxima > agora {
                    proxima = proxima.min(meta.proxima);
                    continue;
                }
                if !self.em_voo().insert(meta.id.clone()) {
                    continue;
                }
                Arc::clone(&self).tentar(meta, None).await;
            }
            let dormir = Duration::from_secs(proxima.saturating_sub(crate::data::agora_unix()));
            tokio::select! {
                _ = self.aviso.notified() => {}
                _ = tokio::time::sleep(dormir) => {}
            }
        }
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    #[test]
    fn esperas_crescem_e_param_em_uma_hora() {
        assert_eq!(espera(0, 60), 60);
        assert_eq!(espera(1, 60), 60);
        assert_eq!(espera(2, 60), 120);
        assert_eq!(espera(6, 60), 3600);
        assert_eq!(espera(50, 60), 3600);
        assert_eq!(espera(3, 1), 5);
    }
}

//! Certificado do STARTTLS, recarregado quando o arquivo muda (o Caddy
//! renova sozinho): a cada STARTTLS compara o mtime do certificado e da
//! chave, sem reiniciar o serviço. Se a recarga falhar (por exemplo, o
//! certificado novo já gravado e a chave ainda não), fica o anterior.

use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::crypto::ring;

pub struct Tls {
    certificado: PathBuf,
    chave: PathBuf,
    estado: Mutex<Option<Carregado>>,
}

struct Carregado {
    mtimes: (SystemTime, SystemTime),
    aceitador: TlsAcceptor,
}

fn mtime(p: &Path) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

pub fn carregar(certificado: &Path, chave: &Path) -> Result<TlsAcceptor, String> {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_file_iter(certificado)
        .map_err(|e| format!("{}: {e}", certificado.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", certificado.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: nenhum certificado", certificado.display()));
    }
    let chave =
        PrivateKeyDer::from_pem_file(chave).map_err(|e| format!("{}: {e}", chave.display()))?;
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, chave)
        .map_err(|e| format!("certificado e chave: {e}"))?;
    Ok(TlsAcceptor::from(Arc::new(config)))
}

impl Tls {
    pub fn new(certificado: PathBuf, chave: PathBuf) -> Tls {
        Tls {
            certificado,
            chave,
            estado: Mutex::new(None),
        }
    }

    /// O aceitador atual, recarregado se os arquivos mudaram. `None` se nunca
    /// carregou (o EHLO então não anuncia STARTTLS).
    pub fn aceitador(&self) -> Option<TlsAcceptor> {
        let atuais = (mtime(&self.certificado)?, mtime(&self.chave)?);
        let mut estado = self.estado.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(c) = estado.as_ref()
            && c.mtimes == atuais
        {
            return Some(c.aceitador.clone());
        }
        match carregar(&self.certificado, &self.chave) {
            Ok(aceitador) => {
                crate::log::info(
                    "certificado_carregado",
                    &[("arquivo", &self.certificado.display().to_string())],
                );
                *estado = Some(Carregado {
                    mtimes: atuais,
                    aceitador: aceitador.clone(),
                });
                Some(aceitador)
            }
            Err(e) => {
                crate::log::erro("certificado_invalido", &[("detalhe", &e)]);
                estado.as_ref().map(|c| c.aceitador.clone())
            }
        }
    }
}

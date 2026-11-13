//! Configuração em TOML. Ver `config/carteiro.toml.example`.

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Nome do servidor no banner, no EHLO e no `Received:` (o alvo do MX).
    pub hostname: String,
    /// Único domínio aceito no RCPT TO.
    pub dominio: String,
    /// Partes locais aceitas (sem o `+sufixo`). Não há catch-all.
    pub enderecos: Vec<String>,
    #[serde(default = "padrao::escutar")]
    pub escutar: Vec<SocketAddr>,
    #[serde(default = "padrao::tamanho_maximo")]
    pub tamanho_maximo: u64,
    #[serde(default = "padrao::conexoes_maximas")]
    pub conexoes_maximas: usize,
    #[serde(default = "padrao::conexoes_por_ip")]
    pub conexoes_por_ip: usize,
    #[serde(default = "padrao::destinatarios_maximos")]
    pub destinatarios_maximos: usize,
    /// Quanto esperar antes do banner: quem fala antes dele é recusado.
    #[serde(default = "padrao::espera_banner_ms")]
    pub espera_banner_ms: u64,
    #[serde(default = "padrao::tempo_comando_s")]
    pub tempo_comando_s: u64,
    #[serde(default = "padrao::tempo_dados_s")]
    pub tempo_dados_s: u64,
    /// Quanto a resposta do DATA espera o Gmail antes de responder `250` com
    /// a mensagem na fila.
    #[serde(default = "padrao::espera_gmail_s")]
    pub espera_gmail_s: u64,
    /// Espera antes da segunda tentativa; as seguintes crescem até 60 vezes
    /// isto (1, 2, 5, 10, 30, 60 vezes).
    #[serde(default = "padrao::espera_retentativa_s")]
    pub espera_retentativa_s: u64,
    /// Depois disso sem conseguir importar, a mensagem vai para `falhou/`.
    #[serde(default = "padrao::desistir_apos_h")]
    pub desistir_apos_h: u64,
    pub spool: PathBuf,
    pub certificado: Option<PathBuf>,
    pub chave: Option<PathBuf>,
    /// JSON do cliente OAuth baixado do Google Cloud.
    pub cliente_oauth: PathBuf,
    /// JSON com o `refresh_token`, gravado por `carteiro autorizar`.
    pub token: PathBuf,
    /// Arquivo `.prom` para o textfile collector do node-exporter.
    pub metricas: Option<PathBuf>,
    /// SPF, DKIM e DMARC do remetente; recusa o que falha com `p=reject`.
    #[serde(default = "padrao::sim")]
    pub verificar_autenticacao: bool,
    /// Labels de subendereço (`contato+x`) criadas por dia, no máximo.
    #[serde(default = "padrao::labels_novas_por_dia")]
    pub labels_novas_por_dia: u32,
    #[serde(default = "padrao::api_gmail")]
    pub api_gmail: String,
    #[serde(default = "padrao::api_token")]
    pub api_token: String,
}

mod padrao {
    use std::net::SocketAddr;

    pub fn escutar() -> Vec<SocketAddr> {
        vec!["0.0.0.0:25".parse().unwrap(), "[::]:25".parse().unwrap()]
    }
    pub fn tamanho_maximo() -> u64 {
        25 * 1024 * 1024
    }
    pub fn conexoes_maximas() -> usize {
        40
    }
    pub fn conexoes_por_ip() -> usize {
        4
    }
    pub fn destinatarios_maximos() -> usize {
        20
    }
    pub fn espera_banner_ms() -> u64 {
        1000
    }
    pub fn tempo_comando_s() -> u64 {
        120
    }
    pub fn tempo_dados_s() -> u64 {
        600
    }
    pub fn espera_gmail_s() -> u64 {
        20
    }
    pub fn espera_retentativa_s() -> u64 {
        60
    }
    pub fn desistir_apos_h() -> u64 {
        120
    }
    pub fn sim() -> bool {
        true
    }
    pub fn labels_novas_por_dia() -> u32 {
        20
    }
    pub fn api_gmail() -> String {
        "https://gmail.googleapis.com".into()
    }
    pub fn api_token() -> String {
        "https://oauth2.googleapis.com/token".into()
    }
}

impl Config {
    pub fn carregar(caminho: &Path) -> Result<Config, String> {
        let texto = std::fs::read_to_string(caminho)
            .map_err(|e| format!("lendo {}: {e}", caminho.display()))?;
        let mut c: Config =
            toml::from_str(&texto).map_err(|e| format!("em {}: {e}", caminho.display()))?;
        c.normalizar()?;
        Ok(c)
    }

    pub fn normalizar(&mut self) -> Result<(), String> {
        self.dominio = self.dominio.trim_end_matches('.').to_ascii_lowercase();
        self.hostname = self.hostname.trim_end_matches('.').to_ascii_lowercase();
        for e in &mut self.enderecos {
            *e = e.to_ascii_lowercase();
            if e.is_empty() || e.contains(['@', '+', ' ']) {
                return Err(format!("endereço inválido em `enderecos`: {e:?}"));
            }
        }
        if self.enderecos.is_empty() {
            return Err("`enderecos` vazio: nenhum destinatário seria aceito".into());
        }
        if self.certificado.is_some() != self.chave.is_some() {
            return Err("`certificado` e `chave` vão juntos".into());
        }
        // Sob o systemd, LoadCredential= entrega os segredos em
        // $CREDENTIALS_DIRECTORY: caminho relativo é relativo a ele.
        if let Some(dir) = std::env::var_os("CREDENTIALS_DIRECTORY") {
            let dir = PathBuf::from(dir);
            for p in [&mut self.cliente_oauth, &mut self.token] {
                if p.is_relative() {
                    *p = dir.join(&*p);
                }
            }
        }
        Ok(())
    }

    pub fn espera_banner(&self) -> Duration {
        Duration::from_millis(self.espera_banner_ms)
    }
    pub fn tempo_comando(&self) -> Duration {
        Duration::from_secs(self.tempo_comando_s)
    }
    pub fn tempo_dados(&self) -> Duration {
        Duration::from_secs(self.tempo_dados_s)
    }
    pub fn espera_gmail(&self) -> Duration {
        Duration::from_secs(self.espera_gmail_s)
    }
    pub fn desistir_apos(&self) -> Duration {
        Duration::from_secs(self.desistir_apos_h * 3600)
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    fn base() -> String {
        r#"
hostname = "MX.Exemplo.test."
dominio = "Exemplo.test"
enderecos = ["Contato", "pix"]
spool = "/var/lib/carteiro"
cliente_oauth = "/etc/carteiro/cliente.json"
token = "/etc/carteiro/token.json"
"#
        .into()
    }

    #[test]
    fn padroes_e_normalizacao() {
        let mut c: Config = toml::from_str(&base()).unwrap();
        c.normalizar().unwrap();
        assert_eq!(c.hostname, "mx.exemplo.test");
        assert_eq!(c.dominio, "exemplo.test");
        assert_eq!(c.enderecos, ["contato", "pix"]);
        assert_eq!(c.tamanho_maximo, 25 * 1024 * 1024);
        assert_eq!(c.escutar.len(), 2);
        assert!(c.verificar_autenticacao);
    }

    #[test]
    fn recusa_campo_desconhecido_e_par_incompleto() {
        let r: Result<Config, _> = toml::from_str(&(base() + "catch_all = true\n"));
        assert!(r.is_err());
        let mut c: Config = toml::from_str(&(base() + "certificado = \"/x.pem\"\n")).unwrap();
        assert!(c.normalizar().is_err());
        let mut c: Config =
            toml::from_str(&base().replace(r#"["Contato", "pix"]"#, r#"["a+b"]"#)).unwrap();
        assert!(c.normalizar().is_err());
    }
}

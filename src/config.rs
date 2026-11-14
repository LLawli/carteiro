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
    /// Cada caixa é uma conta do Gmail com o seu domínio e os seus endereços.
    #[serde(default)]
    pub caixas: Vec<Caixa>,
    /// Forma curta para uma caixa só: `dominio`, `enderecos`, `cliente_oauth`
    /// e `token` no topo. [`Config::normalizar`] a transforma na caixa
    /// [`CAIXA_PRINCIPAL`].
    #[serde(default)]
    pub dominio: Option<String>,
    #[serde(default)]
    pub enderecos: Option<Vec<String>>,
    #[serde(default)]
    pub cliente_oauth: Option<PathBuf>,
    #[serde(default)]
    pub token: Option<PathBuf>,
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
    /// Arquivo `.prom` para o textfile collector do node-exporter.
    pub metricas: Option<PathBuf>,
    /// SPF, DKIM e DMARC do remetente; recusa o que falha com `p=reject`.
    #[serde(default = "padrao::sim")]
    pub verificar_autenticacao: bool,
    /// Labels de subendereço (`contato+x`) criadas por dia, no máximo, em
    /// cada caixa.
    #[serde(default = "padrao::labels_novas_por_dia")]
    pub labels_novas_por_dia: u32,
    #[serde(default = "padrao::api_gmail")]
    pub api_gmail: String,
    #[serde(default = "padrao::api_token")]
    pub api_token: String,
}

/// Uma conta do Gmail e os endereços que caem nela.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Caixa {
    /// Nome curto (`[a-z0-9_-]`), nos logs, nas métricas e no spool. Renomear
    /// uma caixa com mensagens na fila as deixa sem destino até o prazo.
    pub nome: String,
    /// Domínio aceito no RCPT TO.
    pub dominio: String,
    /// Partes locais aceitas (sem o `+sufixo`). Não há catch-all.
    pub enderecos: Vec<String>,
    /// JSON do cliente OAuth baixado do Google Cloud. O refresh token só vale
    /// com o cliente que o gerou.
    pub cliente_oauth: PathBuf,
    /// JSON com o `refresh_token` desta conta, gravado por `carteiro
    /// autorizar`.
    pub token: PathBuf,
    /// Importa com `neverMarkSpam`: nada desta caixa vai para o spam do
    /// Gmail (para um endereço de suporte, onde perder um cliente custa mais
    /// que ver spam). As recusas do próprio carteiro continuam valendo.
    #[serde(default)]
    pub nunca_spam: bool,
}

/// Nome da caixa criada pela forma curta da config.
pub const CAIXA_PRINCIPAL: &str = "principal";

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

fn nome_valido(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 32
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
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

    /// Valida, põe em minúsculas e transforma a forma curta na caixa
    /// [`CAIXA_PRINCIPAL`]. Depois disto só `caixas` é usado.
    pub fn normalizar(&mut self) -> Result<(), String> {
        self.hostname = self.hostname.trim_end_matches('.').to_ascii_lowercase();
        let curta = (
            self.dominio.take(),
            self.enderecos.take(),
            self.cliente_oauth.take(),
            self.token.take(),
        );
        match curta {
            (None, None, None, None) => {}
            (Some(dominio), Some(enderecos), Some(cliente_oauth), Some(token)) => {
                if !self.caixas.is_empty() {
                    return Err("use a forma curta (`dominio` no topo) ou `[[caixas]]`, \
                                não as duas"
                        .into());
                }
                self.caixas.push(Caixa {
                    nome: CAIXA_PRINCIPAL.into(),
                    dominio,
                    enderecos,
                    cliente_oauth,
                    token,
                    nunca_spam: false,
                });
            }
            _ => {
                return Err(
                    "`dominio`, `enderecos`, `cliente_oauth` e `token` vão juntos no topo".into(),
                );
            }
        }
        if self.caixas.is_empty() {
            return Err("nenhuma caixa: nenhum destinatário seria aceito".into());
        }
        // Sob o systemd, LoadCredential= entrega os segredos em
        // $CREDENTIALS_DIRECTORY: caminho relativo é relativo a ele.
        let credenciais = std::env::var_os("CREDENTIALS_DIRECTORY").map(PathBuf::from);
        for c in &mut self.caixas {
            c.nome = c.nome.to_ascii_lowercase();
            if !nome_valido(&c.nome) {
                return Err(format!("nome de caixa inválido: {:?}", c.nome));
            }
            c.dominio = c.dominio.trim_end_matches('.').to_ascii_lowercase();
            if !c.dominio.contains('.') {
                return Err(format!(
                    "caixa {}: domínio inválido {:?}",
                    c.nome, c.dominio
                ));
            }
            if c.enderecos.is_empty() {
                return Err(format!(
                    "caixa {}: `enderecos` vazio, nenhum destinatário seria aceito",
                    c.nome
                ));
            }
            for e in &mut c.enderecos {
                *e = e.to_ascii_lowercase();
                if e.is_empty() || e.contains(['@', '+', ' ']) {
                    return Err(format!("caixa {}: endereço inválido {e:?}", c.nome));
                }
            }
            if let Some(dir) = &credenciais {
                for p in [&mut c.cliente_oauth, &mut c.token] {
                    if p.is_relative() {
                        *p = dir.join(&*p);
                    }
                }
            }
        }
        for (i, a) in self.caixas.iter().enumerate() {
            for b in &self.caixas[i + 1..] {
                if a.nome == b.nome {
                    return Err(format!("duas caixas com o nome {}", a.nome));
                }
                // qual conta recebe seria um acaso
                if a.dominio == b.dominio
                    && let Some(e) = a.enderecos.iter().find(|e| b.enderecos.contains(e))
                {
                    return Err(format!(
                        "{e}@{} está nas caixas {} e {}",
                        a.dominio, a.nome, b.nome
                    ));
                }
            }
        }
        if self.certificado.is_some() != self.chave.is_some() {
            return Err("`certificado` e `chave` vão juntos".into());
        }
        Ok(())
    }

    /// Um dos domínios das caixas (HELO com ele é de quem finge ser nós).
    pub fn e_nosso_dominio(&self, nome: &str) -> bool {
        self.caixas.iter().any(|c| c.dominio == nome)
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

    fn duas(segunda: &str) -> String {
        format!(
            r#"
hostname = "mx.exemplo.test"
spool = "/var/lib/carteiro"

[[caixas]]
nome = "Eu"
dominio = "exemplo.test"
enderecos = ["contato", "pix"]
cliente_oauth = "/etc/carteiro/cliente.json"
token = "/etc/carteiro/token.json"

[[caixas]]
{segunda}
cliente_oauth = "/etc/carteiro/cliente.json"
token = "/etc/carteiro/token-outra.json"
"#
        )
    }

    fn normalizado(texto: &str) -> Result<Config, String> {
        let mut c: Config = toml::from_str(texto).map_err(|e| e.to_string())?;
        c.normalizar()?;
        Ok(c)
    }

    #[test]
    fn padroes_e_forma_curta() {
        let c = normalizado(&base()).unwrap();
        assert_eq!(c.hostname, "mx.exemplo.test");
        assert_eq!(
            c.caixas,
            [Caixa {
                nome: CAIXA_PRINCIPAL.into(),
                dominio: "exemplo.test".into(),
                enderecos: vec!["contato".into(), "pix".into()],
                cliente_oauth: "/etc/carteiro/cliente.json".into(),
                token: "/etc/carteiro/token.json".into(),
                nunca_spam: false,
            }]
        );
        assert_eq!(c.dominio, None);
        assert_eq!(c.tamanho_maximo, 25 * 1024 * 1024);
        assert_eq!(c.escutar.len(), 2);
        assert!(c.verificar_autenticacao);
        assert!(c.e_nosso_dominio("exemplo.test"));
    }

    #[test]
    fn varias_caixas() {
        let c = normalizado(&duas(
            "nome = \"outra\"\ndominio = \"Outro.test.\"\nenderecos = [\"suporte\"]\nnunca_spam = true",
        ))
        .unwrap();
        let nomes: Vec<_> = c.caixas.iter().map(|c| c.nome.as_str()).collect();
        assert_eq!(nomes, ["eu", "outra"]);
        assert_eq!(c.caixas[1].dominio, "outro.test");
        assert!(!c.caixas[0].nunca_spam);
        assert!(c.caixas[1].nunca_spam);
        assert!(c.e_nosso_dominio("outro.test"));
        assert!(!c.e_nosso_dominio("terceiro.test"));
        // o mesmo domínio em duas caixas, com endereços diferentes
        assert!(
            normalizado(&duas(
                "nome = \"outra\"\ndominio = \"exemplo.test\"\nenderecos = [\"suporte\"]"
            ))
            .is_ok()
        );
    }

    #[test]
    fn recusa_caixas_ambiguas_ou_invalidas() {
        let erro = |segunda: &str| normalizado(&duas(segunda)).unwrap_err();
        assert!(
            erro("nome = \"eu\"\ndominio = \"outro.test\"\nenderecos = [\"a\"]").contains("nome")
        );
        assert!(
            erro("nome = \"outra\"\ndominio = \"exemplo.test\"\nenderecos = [\"Pix\"]")
                .contains("pix@exemplo.test")
        );
        assert!(
            erro("nome = \"a b\"\ndominio = \"outro.test\"\nenderecos = [\"a\"]").contains("nome")
        );
        assert!(
            erro("nome = \"outra\"\ndominio = \"semponto\"\nenderecos = [\"a\"]")
                .contains("domínio")
        );
        assert!(
            erro("nome = \"outra\"\ndominio = \"outro.test\"\nenderecos = []").contains("vazio")
        );
        // forma curta junto com [[caixas]]
        let misto = duas("nome = \"outra\"\ndominio = \"outro.test\"\nenderecos = [\"a\"]")
            .replacen("spool", "dominio = \"x.test\"\nspool", 1);
        assert!(normalizado(&misto).is_err());
        // forma curta pela metade
        assert!(normalizado(&base().replace("token = \"/etc/carteiro/token.json\"", "")).is_err());
        let vazia = "hostname = \"mx.x.test\"\nspool = \"/s\"\n";
        assert!(normalizado(vazia).unwrap_err().contains("nenhuma caixa"));
    }

    #[test]
    fn exemplo_da_config_e_valido_nas_duas_formas() {
        let exemplo = include_str!("../config/carteiro.toml.example");
        let curta = normalizado(exemplo).unwrap();
        assert_eq!(curta.caixas.len(), 1);
        // a forma com [[caixas]]: tira as quatro chaves do topo e descomenta
        // o bloco do fim
        let (topo, fim) = exemplo.split_once("# [[caixas]]").unwrap();
        let topo: String = topo
            .lines()
            .filter(|l| {
                !["dominio =", "enderecos =", "cliente_oauth =", "token ="]
                    .iter()
                    .any(|k| l.starts_with(k))
            })
            .map(|l| format!("{l}\n"))
            .collect();
        let fim: String = format!("# [[caixas]]{fim}")
            .lines()
            .map(|l| format!("{}\n", l.strip_prefix("# ").unwrap_or(l)))
            .collect();
        let varias = normalizado(&(topo + &fim)).unwrap();
        let nomes: Vec<_> = varias.caixas.iter().map(|c| c.nome.as_str()).collect();
        assert_eq!(nomes, ["eu", "loja"]);
    }

    #[test]
    fn recusa_campo_desconhecido_e_par_incompleto() {
        let r: Result<Config, _> = toml::from_str(&(base() + "catch_all = true\n"));
        assert!(r.is_err());
        assert!(normalizado(&(base() + "certificado = \"/x.pem\"\n")).is_err());
        assert!(normalizado(&base().replace(r#"["Contato", "pix"]"#, r#"["a+b"]"#)).is_err());
    }
}

use carteiro::config::Config;
use carteiro::{autorizar, gmail, servidor, spool, tls};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

const USO: &str = "\
uso:
  carteiro [servir] <config.toml>     recebe na porta 25 e importa no Gmail
  carteiro verificar <config.toml>    testa config, certificado, spool e token
  carteiro autorizar <cliente.json> <token.json> [--porta N]
                                      autoriza a conta e grava o refresh token
  carteiro --versao";

/// Sessões abertas têm este tempo para terminar quando o systemd para o
/// serviço (abaixo do TimeoutStopSec da unit).
const PRAZO_PARADA: Duration = Duration::from_secs(20);

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(4)
        .build()
        .expect("runtime do tokio")
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match a.as_slice() {
        ["--versao" | "--version" | "-V"] => {
            println!("carteiro {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        ["verificar", c] => verificar(Path::new(c)),
        ["autorizar", cliente, token] => autorizar(cliente, token, 0),
        ["autorizar", cliente, token, "--porta", p] => match p.parse() {
            Ok(p) => autorizar(cliente, token, p),
            Err(_) => Err(format!("porta inválida: {p}")),
        },
        ["servir", c] => servir(Path::new(c)),
        [c] if !c.starts_with('-') => servir(Path::new(c)),
        _ => {
            eprintln!("{USO}");
            return ExitCode::from(2);
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            carteiro::log::erro("fatal", &[("erro", &e)]);
            ExitCode::FAILURE
        }
    }
}

fn servir(caminho: &Path) -> Result<(), String> {
    let config = Config::carregar(caminho)?;
    runtime().block_on(async {
        let s = servidor::iniciar(config).await?;
        esperar_sinal().await;
        s.parar(PRAZO_PARADA).await;
        Ok(())
    })
}

async fn esperar_sinal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("SIGINT");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

fn autorizar(cliente: &str, token: &str, porta: u16) -> Result<(), String> {
    runtime().block_on(autorizar::executar(
        Path::new(cliente),
        &PathBuf::from(token),
        porta,
    ))
}

fn verificar(caminho: &Path) -> Result<(), String> {
    let config = Config::carregar(caminho)?;
    println!(
        "config: ok ({} @ {})",
        config.enderecos.join(", "),
        config.dominio
    );
    match (&config.certificado, &config.chave) {
        (Some(c), Some(k)) => {
            tls::carregar(c, k)?;
            println!("certificado: ok ({})", c.display());
        }
        _ => println!("certificado: nenhum (STARTTLS desligado)"),
    }
    // Só lê: rodado como root fora da unit, criar o spool aqui o deixaria
    // com o dono errado (quem cria é o StateDirectory= da unit).
    if config.spool.is_dir() {
        let e = spool::Spool::so_leitura(&config.spool).estatisticas();
        println!("spool: ok ({} na fila, {} em falhou/)", e.fila, e.falhou);
    } else {
        println!(
            "spool: {} ainda não existe (a unit cria ao subir)",
            config.spool.display()
        );
    }
    let cliente = gmail::ClienteOAuth::ler(&config.cliente_oauth)?;
    let refresh = gmail::ler_refresh_token(&config.token)?;
    let g = gmail::Gmail::new(
        gmail::http(),
        &config.api_gmail,
        &config.api_token,
        cliente,
        refresh,
        0,
    );
    // As labels base (`contato`, `pix`) saem da config: criá-las aqui adianta
    // o que o primeiro e-mail faria e prova que o escopo gmail.labels veio.
    let labels = config.enderecos.clone();
    runtime()
        .block_on(g.ids_labels(&labels))
        .map_err(|e| format!("Gmail API: {e}"))?;
    println!(
        "gmail: ok (token renovado; labels {} prontas)",
        labels.join(", ")
    );
    Ok(())
}

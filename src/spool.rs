//! Fila em disco. Cada mensagem aceita é `fila/<id>.eml` (o corpo como veio
//! no DATA) mais `fila/<id>.json` (envelope, cabeçalhos a acrescentar,
//! tentativas). O `.json` é renomeado por último: ele é o commit. O `250` só
//! sai depois do fsync do arquivo e do diretório.
//!
//! `tmp/` recebe o DATA em andamento e é limpo ao subir; `falhou/` guarda o
//! que o Gmail recusou depois do `250` ou que passou do prazo, para olhar à
//! mão (o carteiro nunca devolve bounce: isso seria enviar e-mail).

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Meta {
    pub id: String,
    /// Nome da caixa (a conta do Gmail). Vazio no que foi gravado antes das
    /// várias caixas: vai para a primeira.
    #[serde(default)]
    pub caixa: String,
    /// Cabeçalhos acrescentados no topo (Return-Path, Delivered-To,
    /// Authentication-Results, Received), já com CRLF.
    pub cabecalhos: String,
    pub de: String,
    pub para: Vec<String>,
    pub labels: Vec<String>,
    pub ip: String,
    pub tamanho: u64,
    pub recebida: u64,
    pub tentativas: u32,
    pub proxima: u64,
    pub ultimo_erro: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Estatisticas {
    pub fila: u64,
    pub mais_antiga: Option<u64>,
    pub falhou: u64,
}

#[derive(Debug)]
pub struct Spool {
    raiz: PathBuf,
}

static CONTADOR: AtomicU32 = AtomicU32::new(0);

fn criar_dir(p: &Path) -> io::Result<()> {
    fs::create_dir_all(p)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(0o700))
}

fn sincronizar_dir(p: &Path) -> io::Result<()> {
    File::open(p)?.sync_all()
}

fn gravar_sincronizado(caminho: &Path, conteudo: &[u8]) -> io::Result<()> {
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(caminho)?;
    f.write_all(conteudo)?;
    f.sync_all()
}

impl Spool {
    pub fn abrir(raiz: &Path) -> io::Result<Spool> {
        for d in ["tmp", "fila", "falhou"] {
            criar_dir(&raiz.join(d))?;
        }
        let s = Spool {
            raiz: raiz.to_path_buf(),
        };
        s.limpar()?;
        Ok(s)
    }

    /// Só para ler (estatísticas): não cria nem limpa nada.
    pub fn so_leitura(raiz: &Path) -> Spool {
        Spool {
            raiz: raiz.to_path_buf(),
        }
    }

    /// Apaga o DATA que estava em andamento numa parada e os pares
    /// incompletos da fila (um `.eml` sem `.json` não chegou a ter `250`).
    fn limpar(&self) -> io::Result<()> {
        for e in fs::read_dir(self.raiz.join("tmp"))? {
            let _ = fs::remove_file(e?.path());
        }
        let fila = self.raiz.join("fila");
        for e in fs::read_dir(&fila)? {
            let p = e?.path();
            let par = match p.extension().and_then(|x| x.to_str()) {
                Some("eml") => p.with_extension("json"),
                Some("json") => p.with_extension("eml"),
                _ => continue,
            };
            if !par.exists() {
                let _ = fs::remove_file(&p);
            }
        }
        Ok(())
    }

    pub fn novo_id() -> String {
        let agora = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        format!(
            "{:x}{:05x}{:04x}",
            agora.as_secs(),
            agora.subsec_micros(),
            CONTADOR.fetch_add(1, Ordering::Relaxed) & 0xffff
        )
    }

    pub fn tmp(&self, id: &str) -> PathBuf {
        self.raiz.join("tmp").join(format!("{id}.eml"))
    }

    pub fn eml(&self, id: &str) -> PathBuf {
        self.raiz.join("fila").join(format!("{id}.eml"))
    }

    fn json(&self, dir: &str, id: &str) -> PathBuf {
        self.raiz.join(dir).join(format!("{id}.json"))
    }

    /// Cria o arquivo do DATA em `tmp/`.
    pub fn criar_tmp(&self, id: &str) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.tmp(id))
    }

    pub fn descartar_tmp(&self, id: &str) {
        let _ = fs::remove_file(self.tmp(id));
    }

    /// Passa a mensagem de `tmp/` para a fila de forma durável.
    pub fn confirmar(&self, meta: &Meta) -> io::Result<()> {
        let tmp_eml = self.tmp(&meta.id);
        File::open(&tmp_eml)?.sync_all()?;
        let tmp_json = self.json("tmp", &meta.id);
        gravar_sincronizado(&tmp_json, &serde_json::to_vec(meta)?)?;
        fs::rename(&tmp_eml, self.eml(&meta.id))?;
        fs::rename(&tmp_json, self.json("fila", &meta.id))?;
        sincronizar_dir(&self.raiz.join("fila"))
    }

    /// Regrava o `.json` depois de uma tentativa. Sem fsync: perder isto numa
    /// queda só repete uma tentativa.
    pub fn atualizar(&self, meta: &Meta) -> io::Result<()> {
        let tmp = self.json("tmp", &meta.id);
        fs::write(&tmp, serde_json::to_vec(meta)?)?;
        fs::rename(&tmp, self.json("fila", &meta.id))
    }

    /// Tira da fila depois do import (o `.json` primeiro: sem ele, o `.eml`
    /// que sobrar é apagado ao subir).
    pub fn remover(&self, id: &str) -> io::Result<()> {
        fs::remove_file(self.json("fila", id))?;
        let _ = fs::remove_file(self.eml(id));
        Ok(())
    }

    pub fn mover_para_falhou(&self, meta: &Meta) -> io::Result<()> {
        let destino_eml = self.raiz.join("falhou").join(format!("{}.eml", meta.id));
        fs::rename(self.eml(&meta.id), destino_eml)?;
        fs::write(self.json("falhou", &meta.id), serde_json::to_vec(meta)?)?;
        let _ = fs::remove_file(self.json("fila", &meta.id));
        sincronizar_dir(&self.raiz.join("falhou"))
    }

    pub fn listar(&self) -> io::Result<Vec<Meta>> {
        let mut v = Vec::new();
        for e in fs::read_dir(self.raiz.join("fila"))? {
            let p = e?.path();
            if p.extension().and_then(|x| x.to_str()) != Some("json") {
                continue;
            }
            match fs::read(&p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Meta>(&b).ok())
            {
                Some(m) => v.push(m),
                None => {
                    crate::log::erro("spool_ilegivel", &[("arquivo", &p.display().to_string())])
                }
            }
        }
        v.sort_by_key(|m| m.recebida);
        Ok(v)
    }

    pub fn estatisticas(&self) -> Estatisticas {
        let mut e = Estatisticas::default();
        if let Ok(v) = self.listar() {
            e.fila = v.len() as u64;
            e.mais_antiga = v.iter().map(|m| m.recebida).min();
        }
        if let Ok(d) = fs::read_dir(self.raiz.join("falhou")) {
            e.falhou = d
                .filter_map(|x| x.ok())
                .filter(|x| x.path().extension().and_then(|s| s.to_str()) == Some("json"))
                .count() as u64;
        }
        e
    }
}

#[cfg(test)]
mod testes {
    use super::*;

    pub fn dir_teste(nome: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "carteiro-{nome}-{}-{}",
            std::process::id(),
            Spool::novo_id()
        ));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn meta(id: &str) -> Meta {
        Meta {
            id: id.into(),
            caixa: "eu".into(),
            cabecalhos: "Received: x\r\n".into(),
            de: "a@b.c".into(),
            para: vec!["contato@x.y".into()],
            labels: vec!["contato".into()],
            ip: "192.0.2.1".into(),
            tamanho: 5,
            recebida: 1,
            tentativas: 0,
            proxima: 0,
            ultimo_erro: None,
        }
    }

    #[test]
    fn ciclo_completo() {
        let d = dir_teste("spool");
        let s = Spool::abrir(&d).unwrap();
        let id = Spool::novo_id();
        s.criar_tmp(&id).unwrap().write_all(b"corpo").unwrap();
        let m = meta(&id);
        s.confirmar(&m).unwrap();
        assert_eq!(s.listar().unwrap(), vec![m.clone()]);
        assert_eq!(fs::read(s.eml(&id)).unwrap(), b"corpo");
        assert_eq!(s.estatisticas().fila, 1);

        let mut m2 = m.clone();
        m2.tentativas = 3;
        s.atualizar(&m2).unwrap();
        assert_eq!(s.listar().unwrap()[0].tentativas, 3);

        s.mover_para_falhou(&m2).unwrap();
        assert!(s.listar().unwrap().is_empty());
        assert_eq!(s.estatisticas().falhou, 1);
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn meta_sem_caixa_le_com_caixa_vazia() {
        let mut m = meta("antigo");
        m.caixa = String::new();
        let mut v = serde_json::to_value(&m).unwrap();
        v.as_object_mut().unwrap().remove("caixa");
        let lida: Meta = serde_json::from_value(v).unwrap();
        assert_eq!(lida, m);
    }

    #[test]
    fn ids_unicos() {
        let a: std::collections::HashSet<_> = (0..1000).map(|_| Spool::novo_id()).collect();
        assert_eq!(a.len(), 1000);
    }

    #[test]
    fn ao_subir_limpa_tmp_e_pares_incompletos() {
        let d = dir_teste("limpar");
        let s = Spool::abrir(&d).unwrap();
        s.criar_tmp("emandamento").unwrap();
        fs::write(s.eml("orfao"), b"x").unwrap();
        let id = Spool::novo_id();
        s.criar_tmp(&id).unwrap();
        s.confirmar(&meta(&id)).unwrap();
        drop(s);
        let s = Spool::abrir(&d).unwrap();
        assert!(!s.tmp("emandamento").exists());
        assert!(!s.eml("orfao").exists());
        assert_eq!(s.listar().unwrap().len(), 1);
        s.remover(&id).unwrap();
        assert!(s.listar().unwrap().is_empty());
        assert!(!s.eml(&id).exists());
        fs::remove_dir_all(&d).unwrap();
    }
}

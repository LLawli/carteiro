//! Recebe e-mail por SMTP e entrega na caixa do Gmail pela Gmail API
//! (`users.messages.import`). Não envia e-mail por SMTP em hipótese alguma.

pub mod autenticacao;
pub mod autorizar;
pub mod config;
pub mod data;
pub mod endereco;
pub mod entrega;
pub mod gmail;
pub mod log;
pub mod metricas;
pub mod servidor;
pub mod smtp;
pub mod spool;
pub mod tls;

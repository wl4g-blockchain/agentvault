use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid master key: {0}")]
    InvalidMasterKey(String),
    #[error("wallet key already exists: {0}")]
    KeyExists(String),
    #[error("wallet key not found: {0}")]
    KeyNotFound(String),
    #[error("invalid wallet key: {0}")]
    InvalidKey(String),
    #[error("invalid signing request: {0}")]
    InvalidRequest(String),
    #[error("signing request expired")]
    RequestExpired,
    #[error("request ID was reused for different signing data")]
    RequestConflict,
    #[error("unsupported signing scheme: {0}")]
    UnsupportedScheme(String),
    #[error("cryptographic operation failed: {0}")]
    Crypto(String),
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    WriteFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("MQTT error: {0}")]
    Mqtt(String),
}

pub type Result<T> = std::result::Result<T, Error>;

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("invalid wallet key: {0}")]
    InvalidKey(String),
    #[error("invalid signing request: {0}")]
    InvalidRequest(String),
    #[error("authentication is required")]
    Unauthenticated,
    #[error("principal is not authorized for this resource")]
    Forbidden,
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("payment conflict: {0}")]
    PaymentConflict(String),
    #[error("payment policy denied the operation: {0}")]
    PolicyDenied(String),
    #[error("invalid payment state transition: {0}")]
    InvalidTransition(String),
    #[error("payment protocol error: {0}")]
    PaymentProtocol(String),
    #[error("settlement error: {0}")]
    Settlement(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("required dependency is unavailable: {0}")]
    DependencyUnavailable(String),
    #[error("cryptographic operation failed: {0}")]
    Crypto(String),
    #[error("failed to read {path}: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
#[path = "../../tests/utils/error_test.rs"]
mod tests;

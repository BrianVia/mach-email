use thiserror::Error;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid action: {0}")]
    InvalidAction(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("remote error: {0}")]
    Remote(String),

    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

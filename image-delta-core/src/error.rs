use thiserror::Error;

/// All errors that can be returned by `image-delta-core`.
#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("encode error: {0}")]
    Encode(String),

    #[error("decode error: {0}")]
    Decode(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("format error: {0}")]
    Format(String),

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;

// Error variants are self-documenting via their `thiserror` messages.

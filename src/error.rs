use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("unsupported ELF: {0}")]
    Unsupported(String),

    #[error("parse error: {0}")]
    Parse(String),

    #[error("{0}")]
    Missing(String),

    #[error("serialize error: {0}")]
    Serialize(String),

    #[error("constraint violated: {0}")]
    Constraint(String),

    #[error("{0}")]
    Cli(String),
}

pub type Result<T> = std::result::Result<T, Error>;

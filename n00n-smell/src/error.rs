#![allow(clippy::missing_errors_doc)]

#[derive(Debug, thiserror::Error)]
pub enum SmellError {
    #[error("I/O error: {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },

    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("search error: {message}")]
    Search { message: String },

    #[error("config error: {message}")]
    Config { message: String },

    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),

    #[error("index is locked by another process")]
    Locked,
}

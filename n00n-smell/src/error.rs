#![allow(clippy::missing_errors_doc)]

#[derive(Debug, thiserror::Error)]
pub enum SmellError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("tantivy error: {0}")]
    Tantivy(#[from] tantivy::TantivyError),

    #[error("search error: {message}")]
    Search { message: String },

    #[error("config error: {message}")]
    Config { message: String },

    #[error("git scan error: {0}")]
    Git(#[from] n00n_git::GitError),

    #[error("index is locked by another process")]
    Locked,
}

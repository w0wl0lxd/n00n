#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("search index error: {message}")]
    Index { message: String },

    #[error("search configuration error: {message}")]
    Config { message: String },

    #[error("search I/O error: {source}")]
    Io { source: std::io::Error },

    #[error("search index is locked by another process")]
    Locked,

    #[error("semantic search is not configured")]
    NotSupported,

    #[error("tantivy error: {source}")]
    Tantivy { source: tantivy::TantivyError },
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io { source }
    }
}

impl From<tantivy::TantivyError> for Error {
    fn from(source: tantivy::TantivyError) -> Self {
        Self::Tantivy { source }
    }
}

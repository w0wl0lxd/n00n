use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("repository not found at path: {0}")]
    RepoNotFound(String),

    #[error("not a git repository: {0}")]
    NotAGitRepo(String),

    #[error("git operation failed: {0}")]
    GitOperation(String),

    #[error("invalid reference: {0}")]
    InvalidReference(String),

    #[error("file not found in repository: {0}")]
    FileNotFound(String),

    #[error("bare repository does not support working tree operations")]
    BareRepo,

    #[error("merge conflict detected")]
    MergeConflict,

    #[error("repository is locked by another operation")]
    RepositoryLocked,
}

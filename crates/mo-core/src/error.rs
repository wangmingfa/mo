use thiserror::Error;

use crate::directory::DirectoryError;

/// Mo 的统一错误类型（核心层）。
#[derive(Debug, Error)]
pub enum MoError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("directory error: {0:?}")]
    Directory(DirectoryError),

    #[error("operation cancelled")]
    Cancelled,

    #[error("{0}")]
    Other(String),
}

impl From<DirectoryError> for MoError {
    fn from(e: DirectoryError) -> Self {
        MoError::Directory(e)
    }
}

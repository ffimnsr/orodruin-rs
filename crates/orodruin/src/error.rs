use std::io;

use thiserror::Error;

use crate::{backend::BackendError, config::ConfigError};

#[derive(Debug, Error)]
pub enum OrodruinError {
    #[error("{0}")]
    Message(String),
    #[error("`{command}` failed with exit status {status:?}")]
    CommandFailed {
        command: String,
        status: Option<i32>,
    },
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Backend(#[from] BackendError),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl OrodruinError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Backend(error) => error.exit_code(),
            Self::CommandFailed {
                status: Some(code), ..
            } => *code,
            _ => 1,
        }
    }
}

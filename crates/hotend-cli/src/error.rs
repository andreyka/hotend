//! The one error type of the command line: what to print and which exit code to use.

use hotend_core::CoreError;

/// Why a command did not end with exit code 0.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The command ran and its outcome is a failure the command already printed in full
    /// (a model that does not build, a failed check, a failed doctor requirement).
    #[error("")]
    Reported,
    /// The command ran and failed; the message says what happened and what to do.
    #[error("{0}")]
    Failed(String),
    /// The command could not run: an argument is wrong or something is not set up.
    #[error("{0}")]
    Setup(String),
    #[error(transparent)]
    Core(#[from] CoreError),
}

impl CliError {
    /// 1 for a failed outcome, 2 when the command could not run.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::Reported | Self::Failed(_) => 1,
            Self::Setup(_) => 2,
            Self::Core(error) => match error {
                CoreError::InvalidInput(_)
                | CoreError::NotConfigured(_)
                | CoreError::Runtime(_)
                | CoreError::Path(_)
                | CoreError::Setup(_) => 2,
                _ => 1,
            },
        }
    }

    /// The text for standard error; `None` when the command already printed everything.
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Reported => None,
            other => Some(other.to_string()),
        }
    }

    pub fn setup(message: impl Into<String>) -> Self {
        Self::Setup(message.into())
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::Failed(error.to_string())
    }
}

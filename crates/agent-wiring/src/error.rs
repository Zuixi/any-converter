//! The single error type that crosses the `agent-wiring` crate boundary.
//!
//! Messages intentionally describe the failure without exposing internal
//! implementation paths or module names; callers map the variants to their own
//! user-facing error codes.

use std::fmt;
use std::io;

use crate::backup::BackupError;
use crate::edit::EditError;

/// Errors produced while wiring an agent onto the gateway.
#[derive(Debug)]
pub enum WiringError {
    /// The requested agent is not installed on this machine.
    AgentAbsent(String),
    /// A recovery store could not be read or written.
    Backup(BackupError),
    /// A configuration file could not be parsed or edited.
    Edit(EditError),
    /// A filesystem operation failed.
    Io(io::Error),
    /// Disconnect ran without the pre-connect values it needed.
    StashMissing(String),
    /// The requested model is not in the catalog.
    ModelNotFound(String),
    /// The catalog contains no usable models.
    NoCatalog,
    /// Connecting would conflict with an existing manual configuration.
    Refused(String),
}

impl fmt::Display for WiringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AgentAbsent(agent) => write!(f, "agent not installed: {agent}"),
            Self::Backup(_) => write!(f, "recovery data could not be used"),
            Self::Edit(error) => write!(f, "configuration edit failed: {error}"),
            Self::Io(error) => write!(f, "io error: {error}"),
            Self::StashMissing(agent) => write!(f, "missing stash for {agent}"),
            Self::ModelNotFound(model) => write!(f, "model not found: {model}"),
            Self::NoCatalog => write!(f, "no models are available in the catalog"),
            Self::Refused(reason) => write!(f, "refused to modify agent configuration: {reason}"),
        }
    }
}

impl std::error::Error for WiringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Backup(error) => Some(error),
            Self::Edit(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::AgentAbsent(_)
            | Self::StashMissing(_)
            | Self::ModelNotFound(_)
            | Self::NoCatalog
            | Self::Refused(_) => None,
        }
    }
}

impl From<BackupError> for WiringError {
    fn from(error: BackupError) -> Self {
        Self::Backup(error)
    }
}

impl From<EditError> for WiringError {
    fn from(error: EditError) -> Self {
        Self::Edit(error)
    }
}

impl From<io::Error> for WiringError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

//! Canonical on-disk locations for a server installation.
//!
//! The desktop app, CLI and embedded server must agree on where the
//! authoritative configuration, secrets and instance metadata live. Keeping
//! that resolution in one place prevents each surface from inventing its own
//! default directory or accidentally writing a second config file.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Overrides the root directory used for configuration, secrets, logs and
/// instance metadata.
pub const HOME_ENV: &str = "ANY_CONVERTER_HOME";

/// Overrides only the secrets store path. This remains supported for
/// compatibility with existing CLI tests and deployments.
pub const SECRETS_ENV: &str = "ANY_CONVERTER_SECRETS";

/// A resolved any-converter home.
///
/// `Home` never creates directories. Callers create them only when they are
/// about to write, so a read-only status check leaves the filesystem
/// untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Home {
    root: PathBuf,
    config_path: PathBuf,
    secrets_path: PathBuf,
}

/// Failure to resolve a usable home directory.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum HomeError {
    #[error(
        "cannot determine any-converter home directory; set {HOME_ENV}, or set APPDATA (Windows) or HOME/XDG_CONFIG_HOME (Unix)"
    )]
    NoHomeDirectory,
}

impl Home {
    /// Build a home rooted at an explicit directory.
    ///
    /// This is the deterministic constructor used by tests and by callers
    /// that already resolved a user-selected directory.
    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            config_path: root.join("config.toml"),
            secrets_path: root.join("secrets.json"),
            root,
        }
    }

    /// Resolve the default home from the environment.
    ///
    /// `ANY_CONVERTER_HOME` has highest precedence. The platform defaults
    /// match the historical locations used by the server:
    ///
    /// - Windows: `%APPDATA%\any-converter`
    /// - Unix: `$XDG_CONFIG_HOME/any-converter`, else
    ///   `$HOME/.config/any-converter`
    ///
    /// `ANY_CONVERTER_SECRETS`, when non-empty, overrides only
    /// [`Self::secrets_path`].
    pub fn discover() -> Result<Self, HomeError> {
        let root = env_path(HOME_ENV)
            .or_else(default_root)
            .ok_or(HomeError::NoHomeDirectory)?;
        let mut home = Self::from_root(root);
        if let Some(secrets_path) = env_path(SECRETS_ENV) {
            home.secrets_path = secrets_path;
        }
        Ok(home)
    }

    /// The root directory containing all persistent server data.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn uses_default_root(&self) -> bool {
        default_root().is_some_and(|root| root == self.root)
    }

    /// The authoritative human-editable TOML file.
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// The OAuth account store. Provider/inference API keys do not belong
    /// here; they are stored in the OS keychain.
    pub fn secrets_path(&self) -> &Path {
        &self.secrets_path
    }

    /// The request/usage log directory.
    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Metadata describing the active managed server instance.
    pub fn instance_path(&self) -> PathBuf {
        self.root.join("instance.json")
    }

    /// Cross-process lock preventing two supervised instances from starting.
    pub fn instance_lock_path(&self) -> PathBuf {
        self.root.join("instance.lock")
    }

    /// The page key file for browser sessions, created by `daemon install`.
    pub fn web_key_path(&self) -> PathBuf {
        self.root.join("web-key")
    }

    /// Kernel-backed lock serializing first creation of the page key.
    pub(crate) fn web_key_lock_path(&self) -> PathBuf {
        self.root.join("web-key.lock")
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(windows)]
fn default_root() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join("AppData").join("Roaming"))
        })
        .map(|base| base.join("any-converter"))
}

#[cfg(not(windows))]
fn default_root() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|home| home.join(".config"))
        })
        .map(|base| base.join("any-converter"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn explicit_root_derives_all_paths() {
        let root = PathBuf::from("tmp").join("any-converter-home");
        let home = Home::from_root(&root);

        assert_eq!(home.root(), root);
        assert_eq!(home.config_path(), root.join("config.toml"));
        assert_eq!(home.secrets_path(), root.join("secrets.json"));
        assert_eq!(home.logs_dir(), root.join("logs"));
        assert_eq!(home.instance_path(), root.join("instance.json"));
        assert_eq!(home.instance_lock_path(), root.join("instance.lock"));
        assert_eq!(home.web_key_path(), root.join("web-key"));
    }
}

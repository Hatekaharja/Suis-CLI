//! Shared error types for the entire Suis project.
//!
//! [`Error`] is the crate-wide error enum. The sub-error enums ([`ConfigError`],
//! [`WorkspaceError`], [`FilesystemError`], [`ProviderError`]) group related
//! failures and convert into [`Error`] via `From`. [`ProviderError`] is defined
//! here but is primarily consumed by `suis-providers`.

use std::path::PathBuf;
use thiserror::Error;

/// Convenience alias for results carrying the crate-wide [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// The top-level error type for Suis.
#[derive(Debug, Error)]
pub enum Error {
    /// A configuration file could not be read, parsed, or written.
    #[error(transparent)]
    Config(#[from] ConfigError),

    /// A workspace was invalid or a path violated its boundary.
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),

    /// An action was blocked by the permission/safety layer.
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    /// A filesystem operation failed.
    #[error(transparent)]
    Filesystem(#[from] FilesystemError),

    /// A provider request failed (used by `suis-providers`).
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

/// Failures relating to loading or persisting configuration files.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// A required config file did not exist.
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    /// The config file could not be read from disk.
    #[error("failed to read config {path}: {source}")]
    ReadFailure {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The config file contained invalid JSON.
    #[error("failed to parse config {path}: {source}")]
    ParseFailure {
        path: PathBuf,
        source: serde_json::Error,
    },

    /// The config file could not be written to disk.
    #[error("failed to write config {path}: {source}")]
    WriteFailure {
        path: PathBuf,
        source: std::io::Error,
    },

    /// The config value could not be serialized to JSON.
    #[error("failed to serialize config: {source}")]
    SerializeFailure { source: serde_json::Error },
}

/// Failures relating to workspace detection and boundary enforcement.
#[derive(Debug, Error)]
pub enum WorkspaceError {
    /// A path resolved outside the workspace root.
    #[error("path escapes workspace boundary: {0}")]
    BoundaryViolation(PathBuf),

    /// The workspace root was missing or not a directory.
    #[error("invalid workspace: {0}")]
    Invalid(String),
}

/// Filesystem operation failures.
#[derive(Debug, Error)]
pub enum FilesystemError {
    /// An underlying `std::io` error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The target path did not exist.
    #[error("file not found: {0}")]
    NotFound(PathBuf),

    /// The target path already existed.
    #[error("file already exists: {0}")]
    AlreadyExists(PathBuf),
}

/// Provider/transport failures. Defined here so all crates can share it.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The provider endpoint refused the connection / was not reachable.
    #[error("provider not running: {0}")]
    NotRunning(String),

    /// The provider endpoint accepted (or never refused) the connection but did
    /// not answer within the deadline — a connect- or request-timeout. Kept
    /// distinct from [`NotRunning`] so the UI can tell a silently-unreachable
    /// host (e.g. a sleeping LAN box) apart from a port that is simply closed.
    #[error("provider timed out: {0}")]
    Timeout(String),

    /// The provider rejected the credentials (HTTP 401/403). Carries the
    /// provider id and, when known, the name of the env var that holds the key
    /// (never the key itself) so the message can name what to check.
    #[error("auth failed for {provider}{}", key_hint(.key_env))]
    AuthFailed {
        /// The provider whose credentials were rejected.
        provider: String,
        /// The configured key env-var name (not the key), for the action hint.
        key_env: Option<String>,
    },

    /// The provider throttled the request (HTTP 429). Carries the provider id;
    /// no retry is attempted — this is a report, not a policy.
    #[error("rate limited by {0} — wait and retry")]
    RateLimited(String),

    /// The provider did not recognize the requested model (HTTP 404). Carries
    /// the provider id and the model so the hint can point at `/model`.
    #[error("model {model} not found on {provider} — pick another with /model")]
    ModelNotFound {
        /// The provider that rejected the model.
        provider: String,
        /// The model id that was not found.
        model: String,
    },

    /// The provider returned a response that could not be parsed.
    #[error("failed to parse provider response: {0}")]
    ParseError(String),

    /// The provider request failed for some other reason.
    #[error("provider request failed: {0}")]
    RequestError(String),
}

impl ProviderError {
    /// Whether this failure is worth re-sending the identical request for.
    ///
    /// Transient failures are network/availability blips that often clear on a
    /// retry: a timeout, a refused/unreachable endpoint, a throttle, or a
    /// generic request failure (which, after status classification, is a 5xx or
    /// a transport-level error). Permanent failures — bad credentials, an
    /// unknown model, an unparseable response — will fail the same way every
    /// time, so they are not retried.
    pub fn is_transient(&self) -> bool {
        match self {
            ProviderError::Timeout(_)
            | ProviderError::NotRunning(_)
            | ProviderError::RateLimited(_)
            | ProviderError::RequestError(_) => true,
            ProviderError::AuthFailed { .. }
            | ProviderError::ModelNotFound { .. }
            | ProviderError::ParseError(_) => false,
        }
    }
}

impl Error {
    /// Whether this failure is worth re-sending the identical request for. Only
    /// provider failures can be transient; config/workspace/filesystem/
    /// permission errors are deterministic and never retried.
    pub fn is_transient(&self) -> bool {
        match self {
            Error::Provider(e) => e.is_transient(),
            _ => false,
        }
    }
}

/// Render the auth-failure action hint: name the env var to check when one is
/// configured, otherwise advise setting a key. Never includes key material.
fn key_hint(key_env: &Option<String>) -> String {
    match key_env {
        Some(env) => format!(" — check ${env}"),
        None => " — check the API key".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn error_is_send_sync() {
        assert_send_sync::<Error>();
        assert_send_sync::<ConfigError>();
        assert_send_sync::<WorkspaceError>();
        assert_send_sync::<FilesystemError>();
        assert_send_sync::<ProviderError>();
    }

    #[test]
    fn config_error_displays() {
        let err = ConfigError::NotFound(PathBuf::from("/tmp/x.json"));
        assert_eq!(err.to_string(), "config file not found: /tmp/x.json");
    }

    #[test]
    fn workspace_error_displays() {
        let err = WorkspaceError::BoundaryViolation(PathBuf::from("/etc/passwd"));
        assert_eq!(
            err.to_string(),
            "path escapes workspace boundary: /etc/passwd"
        );
    }

    #[test]
    fn permission_denied_displays() {
        let err = Error::PermissionDenied("nope".into());
        assert_eq!(err.to_string(), "permission denied: nope");
    }

    #[test]
    fn filesystem_error_displays() {
        let err = FilesystemError::AlreadyExists(PathBuf::from("/tmp/y"));
        assert_eq!(err.to_string(), "file already exists: /tmp/y");
    }

    #[test]
    fn provider_error_displays() {
        let err = ProviderError::NotRunning("http://localhost:11434".into());
        assert_eq!(
            err.to_string(),
            "provider not running: http://localhost:11434"
        );
    }

    #[test]
    fn auth_failed_names_the_env_var_when_known() {
        let err = ProviderError::AuthFailed {
            provider: "openrouter".into(),
            key_env: Some("OPENROUTER_API_KEY".into()),
        };
        let text = err.to_string();
        assert!(text.contains("openrouter"), "names the provider: {text}");
        assert!(
            text.contains("$OPENROUTER_API_KEY"),
            "names the env var: {text}"
        );
    }

    #[test]
    fn auth_failed_without_env_still_hints() {
        let err = ProviderError::AuthFailed {
            provider: "work".into(),
            key_env: None,
        };
        let text = err.to_string();
        assert!(text.contains("work"));
        assert!(text.contains("API key"));
        // No key material is ever rendered.
        assert!(!text.contains('$'), "no env var to show: {text}");
    }

    #[test]
    fn rate_limited_and_not_found_carry_provider_and_hint() {
        let limited = ProviderError::RateLimited("openrouter".into());
        assert!(limited.to_string().contains("openrouter"));
        assert!(limited.to_string().contains("wait and retry"));

        let missing = ProviderError::ModelNotFound {
            provider: "openrouter".into(),
            model: "gpt-9".into(),
        };
        let text = missing.to_string();
        assert!(text.contains("gpt-9"));
        assert!(text.contains("openrouter"));
        assert!(text.contains("/model"));
    }

    #[test]
    fn transient_classification_covers_every_provider_variant() {
        // Network/availability blips: retry.
        assert!(ProviderError::Timeout("x".into()).is_transient());
        assert!(ProviderError::NotRunning("x".into()).is_transient());
        assert!(ProviderError::RateLimited("x".into()).is_transient());
        assert!(ProviderError::RequestError("x".into()).is_transient());
        // Deterministic rejections: don't retry.
        assert!(!ProviderError::AuthFailed {
            provider: "p".into(),
            key_env: None,
        }
        .is_transient());
        assert!(!ProviderError::ModelNotFound {
            provider: "p".into(),
            model: "m".into(),
        }
        .is_transient());
        assert!(!ProviderError::ParseError("x".into()).is_transient());
    }

    #[test]
    fn only_provider_errors_are_transient_at_top_level() {
        let provider: Error = ProviderError::Timeout("x".into()).into();
        assert!(provider.is_transient());
        let permanent: Error = ProviderError::AuthFailed {
            provider: "p".into(),
            key_env: None,
        }
        .into();
        assert!(!permanent.is_transient());
        // Non-provider errors are never retried.
        assert!(!Error::PermissionDenied("nope".into()).is_transient());
        let cfg: Error = ConfigError::NotFound(PathBuf::from("a")).into();
        assert!(!cfg.is_transient());
    }

    #[test]
    fn sub_errors_convert_into_top_level() {
        let e: Error = ConfigError::NotFound(PathBuf::from("a")).into();
        assert!(matches!(e, Error::Config(_)));
        let e: Error = WorkspaceError::Invalid("x".into()).into();
        assert!(matches!(e, Error::Workspace(_)));
        let e: Error = FilesystemError::NotFound(PathBuf::from("b")).into();
        assert!(matches!(e, Error::Filesystem(_)));
        let e: Error = ProviderError::ParseError("y".into()).into();
        assert!(matches!(e, Error::Provider(_)));
    }
}

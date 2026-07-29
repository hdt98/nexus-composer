use std::path::Path;
use std::sync::PoisonError;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Configuration error: {0}")]
    Config(String),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("I/O error: {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{context}: {source}")]
    IoContext {
        context: String,
        #[source]
        source: std::io::Error,
    },
    #[error("JSON parse error: {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("JSON serialization failed: {source}")]
    JsonSerialize {
        #[source]
        source: serde_json::Error,
    },
    #[error("TOML parse error: {path}: {source}")]
    Toml {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("Failed to acquire lock: {0}")]
    Lock(String),
    #[error("MCP validation failed: {0}")]
    McpValidation(String),
    #[error("{0}")]
    Message(String),
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    #[error("{message}")]
    Localized { key: &'static str, message: String },
    #[error("Database error: {0}")]
    Database(String),
    #[error("OMO configuration file does not exist")]
    OmoConfigNotFound,
    #[error("Every provider has an open circuit; no channel is available")]
    AllProvidersCircuitOpen,
    #[error("No provider is configured")]
    NoProvidersConfigured,
}

impl AppError {
    pub fn io(path: impl AsRef<Path>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn json(path: impl AsRef<Path>, source: serde_json::Error) -> Self {
        Self::Json {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn toml(path: impl AsRef<Path>, source: toml::de::Error) -> Self {
        Self::Toml {
            path: path.as_ref().display().to_string(),
            source,
        }
    }

    pub fn localized(
        key: &'static str,
        _legacy_message: impl Into<String>,
        english_message: impl Into<String>,
    ) -> Self {
        Self::Localized {
            key,
            message: english_message.into(),
        }
    }
}

impl<T> From<PoisonError<T>> for AppError {
    fn from(err: PoisonError<T>) -> Self {
        Self::Lock(err.to_string())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        Self::Database(err.to_string())
    }
}

impl From<AppError> for String {
    fn from(err: AppError) -> Self {
        err.to_string()
    }
}

impl serde::Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Formats the error as JSON so the renderer can parse structured error data.
pub fn format_skill_error(
    code: &str,
    context: &[(&str, &str)],
    suggestion: Option<&str>,
) -> String {
    use serde_json::json;

    let mut ctx_map = serde_json::Map::new();
    for (key, value) in context {
        ctx_map.insert(key.to_string(), json!(value));
    }

    let error_obj = json!({
        "code": code,
        "context": ctx_map,
        "suggestion": suggestion,
    });

    serde_json::to_string(&error_obj).unwrap_or_else(|_| {
        // Fall back to a simple representation if JSON serialization fails.
        format!("ERROR:{code}")
    })
}

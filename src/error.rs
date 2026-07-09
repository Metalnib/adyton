//! Error type with the exit-code contract from specification.md §2.1.
//! Variants are added story-by-story; each maps to exactly one exit code.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    /// Transport/provider failure before or during streaming (exit 1).
    Provider(String),
    /// Non-2xx response with the provider's error body (exit 1). Kept
    /// structured so adapters can react to specific statuses (401, 429, …).
    Http { status: u16, body: String },
    /// Bad invocation (exit 2).
    Usage(String),
    /// Broken or missing configuration (exit 3).
    Config(String),
    /// API key resolution failed (exit 4).
    ApiKey(String),
    /// `fix` has no usable failure state (exit 5) — absent or older than
    /// the §5.3 staleness window.
    StaleState(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::Provider(_) | Error::Http { .. } => 1,
            Error::Usage(_) => 2,
            Error::Config(_) => 3,
            Error::ApiKey(_) => 4,
            Error::StaleState(_) => 5,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Provider(msg) | Error::Usage(msg) => write!(f, "{msg}"),
            Error::Http { status, body } if body.is_empty() => write!(f, "HTTP {status}"),
            Error::Http { status, body } => write!(f, "HTTP {status}: {body}"),
            Error::Config(msg) => write!(f, "config: {msg}"),
            Error::ApiKey(msg) => write!(f, "api key: {msg}"),
            Error::StaleState(msg) => write!(f, "fix: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<lexopt::Error> for Error {
    fn from(err: lexopt::Error) -> Self {
        Error::Usage(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn exit_codes_match_spec_table() {
        assert_eq!(Error::Provider(String::new()).exit_code(), 1);
        assert_eq!(
            Error::Http {
                status: 429,
                body: String::new()
            }
            .exit_code(),
            1
        );
        assert_eq!(Error::Usage(String::new()).exit_code(), 2);
        assert_eq!(Error::Config(String::new()).exit_code(), 3);
        assert_eq!(Error::ApiKey(String::new()).exit_code(), 4);
        assert_eq!(Error::StaleState(String::new()).exit_code(), 5);
    }

    #[test]
    fn http_error_display_includes_status_and_body() {
        let err = Error::Http {
            status: 429,
            body: "{\"error\":\"slow down\"}".to_owned(),
        };
        assert_eq!(err.to_string(), "HTTP 429: {\"error\":\"slow down\"}");
        assert_eq!(
            Error::Http {
                status: 502,
                body: String::new()
            }
            .to_string(),
            "HTTP 502"
        );
    }
}

//! User agent validation and utilities for Bitcoin p2p protocol.
//!
//! This module provides validation for Bitcoin Core-style user agent strings
//! and utilities for creating properly formatted user agents.

use std::fmt;

/// Errors that can occur during user agent validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserAgentError {
    /// The user agent format is invalid (must be `/name:version/`).
    InvalidFormat,
    /// The name component is missing or empty.
    MissingName,
    /// The version component is missing or empty.
    MissingVersion,
}

impl fmt::Display for UserAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UserAgentError::InvalidFormat => {
                write!(f, "User agent must follow format '/name:version/'")
            }
            UserAgentError::MissingName => {
                write!(f, "User agent name component cannot be empty")
            }
            UserAgentError::MissingVersion => {
                write!(f, "User agent version component cannot be empty")
            }
        }
    }
}

impl std::error::Error for UserAgentError {}

/// Validates Bitcoin Core-style user agent format: `/name:version/`.
///
/// Bitcoin Core and most other bitcoin implementations use a specific format
/// for user agent strings in version messages. This function validates that
/// a user agent string follows this convention.
///
/// # Arguments
///
/// * `user_agent` - The user agent string to validate.
///
/// # Returns
///
/// * `Ok(())` - If the user agent is valid.
/// * `Err(UserAgentError)` - If the user agent format is invalid.
///
/// # Example
///
/// ```
/// use bitcoin_peers_connection::user_agent::validate_bitcoin_core_format;
///
/// // Valid user agent
/// assert!(validate_bitcoin_core_format("/bitcoin-peers:0.1.0/").is_ok());
///
/// // Invalid formats
/// assert!(validate_bitcoin_core_format("bitcoin-peers:0.1.0").is_err());
/// assert!(validate_bitcoin_core_format("/bitcoin-peers/").is_err());
/// assert!(validate_bitcoin_core_format("/:0.1.0/").is_err());
/// ```
pub fn validate_bitcoin_core_format(user_agent: &str) -> Result<(), UserAgentError> {
    // Check basic format: must start and end with '/' and contain ':'.
    if !user_agent.starts_with('/') || !user_agent.ends_with('/') {
        return Err(UserAgentError::InvalidFormat);
    }

    if !user_agent.contains(':') {
        return Err(UserAgentError::InvalidFormat);
    }

    // Extract the content between the slashes.
    let contents = &user_agent[1..user_agent.len() - 1];
    let parts: Vec<&str> = contents.split(':').collect();

    if parts.len() != 2 {
        return Err(UserAgentError::InvalidFormat);
    }

    if parts[0].is_empty() {
        return Err(UserAgentError::MissingName);
    }

    if parts[1].is_empty() {
        return Err(UserAgentError::MissingVersion);
    }

    Ok(())
}

/// Creates a Bitcoin Core-style user agent string.
///
/// This function creates a properly formatted user agent string following
/// the Bitcoin Core convention of `/name:version/`.
///
/// # Arguments
///
/// * `name` - The application name
/// * `version` - The application version
///
/// # Returns
///
/// A formatted user agent string
///
/// # Example
///
/// ```
/// use bitcoin_peers_connection::user_agent::bitcoin_core_format;
///
/// let user_agent = bitcoin_core_format("bitcoin-peers", "0.1.0");
/// assert_eq!(user_agent, "/bitcoin-peers:0.1.0/");
/// ```
pub fn bitcoin_core_format(name: &str, version: &str) -> String {
    format!("/{name}:{version}/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_user_agents() {
        assert!(validate_bitcoin_core_format("/bitcoin-peers:0.1.0/").is_ok());
        assert!(validate_bitcoin_core_format("/Bitcoin Core:26.0.0/").is_ok());
        assert!(validate_bitcoin_core_format("/Satoshi:0.21.0/").is_ok());
        assert!(validate_bitcoin_core_format("/my-app:1.2.3-beta/").is_ok());
    }

    #[test]
    fn test_validate_invalid_format() {
        // Missing leading slash
        assert_eq!(
            validate_bitcoin_core_format("bitcoin-peers:0.1.0/"),
            Err(UserAgentError::InvalidFormat)
        );

        // Missing trailing slash
        assert_eq!(
            validate_bitcoin_core_format("/bitcoin-peers:0.1.0"),
            Err(UserAgentError::InvalidFormat)
        );

        // Missing both slashes
        assert_eq!(
            validate_bitcoin_core_format("bitcoin-peers:0.1.0"),
            Err(UserAgentError::InvalidFormat)
        );

        // Missing colon
        assert_eq!(
            validate_bitcoin_core_format("/bitcoin-peers/"),
            Err(UserAgentError::InvalidFormat)
        );

        // Multiple colons
        assert_eq!(
            validate_bitcoin_core_format("/bitcoin:peers:0.1.0/"),
            Err(UserAgentError::InvalidFormat)
        );
    }

    #[test]
    fn test_validate_missing_name() {
        assert_eq!(
            validate_bitcoin_core_format("/:0.1.0/"),
            Err(UserAgentError::MissingName)
        );
    }

    #[test]
    fn test_validate_missing_version() {
        assert_eq!(
            validate_bitcoin_core_format("/bitcoin-peers:/"),
            Err(UserAgentError::MissingVersion)
        );
    }

    #[test]
    fn test_bitcoin_core_format() {
        assert_eq!(
            bitcoin_core_format("bitcoin-peers", "0.1.0"),
            "/bitcoin-peers:0.1.0/"
        );
        assert_eq!(
            bitcoin_core_format("Bitcoin Core", "26.0.0"),
            "/Bitcoin Core:26.0.0/"
        );
        assert_eq!(bitcoin_core_format("test", "1.0"), "/test:1.0/");
    }

    #[test]
    fn test_error_display() {
        assert_eq!(
            UserAgentError::InvalidFormat.to_string(),
            "User agent must follow format '/name:version/'"
        );
        assert_eq!(
            UserAgentError::MissingName.to_string(),
            "User agent name component cannot be empty"
        );
        assert_eq!(
            UserAgentError::MissingVersion.to_string(),
            "User agent version component cannot be empty"
        );
    }

    #[test]
    fn test_roundtrip() {
        let name = "bitcoin-peers";
        let version = "0.1.0";
        let user_agent = bitcoin_core_format(name, version);
        assert!(validate_bitcoin_core_format(&user_agent).is_ok());
    }
}

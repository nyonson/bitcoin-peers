//! User agent validation and utilities for bitcoin p2p protocol.
//!
//! This module provides a validated `UserAgent` type and utilities for creating
//! properly formatted user agents following Bitcoin Core conventions.

use std::fmt;
use std::str::FromStr;

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

/// A validated Bitcoin Core-style user agent string.
///
/// This type ensures that user agent strings follow the Bitcoin Core convention
/// of `/name:version/` and are valid at compile time rather than runtime.
///
/// # Example
///
/// ```
/// use bitcoin_peers_connection::UserAgent;
/// use std::str::FromStr;
///
/// // Valid user agent
/// let user_agent = UserAgent::from_str("/bitcoin-peers:0.1.0/").unwrap();
/// assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
///
/// // Create with validation
/// let user_agent = UserAgent::new("/bitcoin-peers:0.1.0/").unwrap();
///
/// // Create from components
/// let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
/// assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UserAgent(String);

impl UserAgent {
    /// Creates a new validated user agent from a string.
    ///
    /// # Arguments
    ///
    /// * `user_agent` - The user agent string to validate
    ///
    /// # Returns
    ///
    /// * `Ok(UserAgent)` - If the user agent is valid
    /// * `Err(UserAgentError)` - If the user agent format is invalid
    ///
    /// # Example
    ///
    /// ```
    /// use bitcoin_peers_connection::UserAgent;
    ///
    /// let user_agent = UserAgent::new("/bitcoin-peers:0.1.0/").unwrap();
    /// assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    /// ```
    pub fn new<S: AsRef<str>>(user_agent: S) -> Result<Self, UserAgentError> {
        let user_agent = user_agent.as_ref();
        validate_bitcoin_core_format(user_agent)?;
        Ok(UserAgent(user_agent.to_string()))
    }

    /// Creates a user agent from name and version components.
    ///
    /// This constructor cannot fail since it creates the string in the correct format.
    ///
    /// # Arguments
    ///
    /// * `name` - The application name
    /// * `version` - The application version
    ///
    /// # Returns
    ///
    /// A validated `UserAgent`
    ///
    /// # Example
    ///
    /// ```
    /// use bitcoin_peers_connection::UserAgent;
    ///
    /// let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
    /// assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    /// ```
    pub fn from_name_version<S1: AsRef<str>, S2: AsRef<str>>(name: S1, version: S2) -> Self {
        let formatted = bitcoin_core_format(name.as_ref(), version.as_ref());
        UserAgent(formatted)
    }

    /// Returns the user agent string.
    ///
    /// # Example
    ///
    /// ```
    /// use bitcoin_peers_connection::UserAgent;
    ///
    /// let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
    /// assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    /// ```
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for UserAgent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for UserAgent {
    type Err = UserAgentError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        UserAgent::new(s)
    }
}

impl AsRef<str> for UserAgent {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

fn validate_bitcoin_core_format(user_agent: &str) -> Result<(), UserAgentError> {
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

fn bitcoin_core_format(name: &str, version: &str) -> String {
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

        let validated = UserAgent::new(&user_agent).unwrap();
        assert_eq!(validated.as_str(), "/bitcoin-peers:0.1.0/");
    }

    #[test]
    fn test_user_agent_new_valid() {
        let user_agent = UserAgent::new("/bitcoin-peers:0.1.0/").unwrap();
        assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    }

    #[test]
    fn test_user_agent_new_invalid() {
        assert!(UserAgent::new("invalid").is_err());
        assert!(UserAgent::new("/invalid/").is_err());
        assert!(UserAgent::new("/:version/").is_err());
        assert!(UserAgent::new("/name:/").is_err());
    }

    #[test]
    fn test_user_agent_from_name_version() {
        let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
        assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    }

    #[test]
    fn test_user_agent_from_str() {
        let user_agent: UserAgent = "/bitcoin-peers:0.1.0/".parse().unwrap();
        assert_eq!(user_agent.as_str(), "/bitcoin-peers:0.1.0/");
    }

    #[test]
    fn test_user_agent_display() {
        let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
        assert_eq!(format!("{user_agent}"), "/bitcoin-peers:0.1.0/");
    }

    #[test]
    fn test_user_agent_equality() {
        let ua1 = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
        let ua2 = UserAgent::new("/bitcoin-peers:0.1.0/").unwrap();
        assert_eq!(ua1, ua2);
    }

    #[test]
    fn test_user_agent_as_ref() {
        let user_agent = UserAgent::from_name_version("bitcoin-peers", "0.1.0");
        let s: &str = user_agent.as_ref();
        assert_eq!(s, "/bitcoin-peers:0.1.0/");
    }
}

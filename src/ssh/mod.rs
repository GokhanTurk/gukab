pub mod client;
pub mod highlight;
pub mod macro_picker;
pub mod session_log;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum SshError {
    #[error("Connection error: {0}")]
    Russh(#[from] russh::Error),
    #[error("Authentication failed")]
    AuthFailed,
    #[error("Keyring error: {0}")]
    Keyring(String),
    #[error("Automation config error: {0}")]
    Automation(String),
    #[error("SSH key error: {0}")]
    Key(String),
    #[error("Serial error: {0}")]
    Serial(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Host {
    pub name: String,
    pub hostname: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub username: String,
    /// Keyring entry name for this host's secret: a login password, or the
    /// passphrase of `identity_file`. Optional — a key-only host with an
    /// unencrypted key (or a switch that logs in in-band) needs no secret.
    #[serde(default)]
    pub credential_ref: String,
    /// Path to a private key file (e.g. `~/.ssh/id_ed25519`). Empty = password auth.
    /// The key material is never copied into config or keyring — only this path is
    /// stored; a passphrase (if any) is read from the `credential_ref` keyring entry.
    #[serde(default)]
    pub identity_file: String,
    /// Group this host belongs to (matches a `[[groups]]` name); `None` = ungrouped.
    #[serde(default)]
    pub group: Option<String>,
    /// Named command shortcuts fired via the Ctrl+A escape prefix during a session.
    #[serde(default)]
    pub macros: Vec<Macro>,
    /// Regex-triggered auto-responses scanned against the session output stream.
    #[serde(default)]
    pub expects: Vec<Expect>,
    /// Macro keys to auto-run right after connecting (e.g. `["en"]` for enable mode).
    #[serde(default)]
    pub on_connect: Vec<String>,
}

/// A keyboard macro: `key` is typed at the `[gukab] macro>` prompt, `send` is
/// transmitted to the remote (a trailing newline is appended at send time).
/// A macro may carry its own `expects`: when the macro is run on connect (its key
/// is in a host's `on_connect`), those expect rules are armed for the session — so
/// e.g. the "en" macro can own the enable-password rule, and it only applies to
/// hosts that actually use "en".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Macro {
    pub key: String,
    pub send: String,
    #[serde(default)]
    pub expects: Vec<Expect>,
}

/// An expect rule: when `pattern` (a regex) matches the incoming output, gukab
/// auto-sends either `send` (literal) or the keyring password named by
/// `send_credential`, followed by a newline. A rule fires at most once per
/// arming — armed at connect (host expects / `on_connect` macros) or each time
/// its owning macro is run — so a wrong credential is never retried into a
/// lockout and a stale rule never answers an unrelated later prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Expect {
    pub pattern: String,
    #[serde(default)]
    pub send: Option<String>,
    #[serde(default)]
    pub send_credential: Option<String>,
}

impl Default for Host {
    fn default() -> Self {
        Self {
            name: String::new(),
            hostname: String::new(),
            port: 22,
            username: String::new(),
            credential_ref: String::new(),
            identity_file: String::new(),
            group: None,
            macros: Vec::new(),
            expects: Vec::new(),
            on_connect: Vec::new(),
        }
    }
}

/// A host group with an icon shown on every host in it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Group {
    pub name: String,
    #[serde(default)]
    pub icon: String,
}

/// Global automations loaded from `automations.toml`: reusable macros (each of
/// which may carry its own expect rules). A macro's expects are armed only when a
/// host runs that macro via `on_connect`, so expects are opt-in per host rather
/// than firing globally.
#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Automations {
    #[serde(default)]
    pub macros: Vec<Macro>,
}

/// App preferences persisted in `settings.toml` (owner-only, like the other
/// config files). Unknown fields are ignored so future settings stay compatible.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub notes: NotesSettings,
}

/// Preferences for the notes section of the right-hand panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotesSettings {
    /// Display label of the section header (the on-disk folder stays `notes`).
    #[serde(default = "default_notes_label")]
    pub label: String,
    /// Hide the section entirely; `Ctrl+O` un-hides it.
    #[serde(default)]
    pub hidden: bool,
}

impl Default for NotesSettings {
    fn default() -> Self {
        Self {
            label: default_notes_label(),
            hidden: false,
        }
    }
}

fn default_notes_label() -> String {
    "Notes".to_string()
}

#[derive(Debug, Deserialize, Serialize, Default)]
struct HostsFile {
    #[serde(default)]
    groups: Vec<Group>,
    #[serde(default)]
    hosts: Vec<Host>,
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
}

fn default_port() -> u16 {
    22
}

/// The user's home directory: `$HOME` on Unix, `%USERPROFILE%` (falling back to
/// `$HOME`) on Windows. `None` if none is set.
fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let vars: &[&str] = &["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    let vars: &[&str] = &["HOME"];
    for var in vars {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            return Some(PathBuf::from(v));
        }
    }
    None
}

/// Expand a leading `~` or `$HOME` in a path to the user's home directory.
/// Only a leading `~`/`~/` (and `$HOME` prefix) is expanded; embedded `~` is left
/// alone. Returns the input unchanged if the home directory is unknown.
pub fn expand_tilde(path: &str) -> PathBuf {
    let home = match home_dir() {
        Some(h) => h,
        None => return PathBuf::from(path),
    };
    if path == "~" {
        return home;
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    if let Some(rest) = path.strip_prefix("$HOME/") {
        return home.join(rest);
    }
    if path == "$HOME" {
        return home;
    }
    PathBuf::from(path)
}

/// Base config directory: `%APPDATA%\gukab` on Windows (falling back to
/// `%USERPROFILE%\.config\gukab`).
#[cfg(windows)]
fn config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA")
        && !appdata.is_empty()
    {
        return PathBuf::from(appdata).join("gukab");
    }
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("gukab")
}

/// Base config directory: `~/.config/gukab` on Unix.
#[cfg(not(windows))]
fn config_dir() -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".config")
        .join("gukab")
}

pub fn config_path() -> PathBuf {
    config_dir().join("hosts.toml")
}

pub fn automations_path() -> PathBuf {
    config_dir().join("automations.toml")
}

pub fn log_dir() -> PathBuf {
    config_dir().join("log")
}

/// Directory holding the panel notes (one `.md` file per note). The folder name
/// is always `notes`, regardless of the section's configurable display label.
pub fn notes_dir() -> PathBuf {
    config_dir().join("notes")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}

/// File recording each server's SSH host-key fingerprint (trust-on-first-use).
pub fn known_hosts_path() -> PathBuf {
    config_dir().join("known_hosts")
}

pub fn load_automations() -> Result<Automations, ConfigError> {
    let path = automations_path();
    if !path.exists() {
        return Ok(Automations::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

/// Persist the global macros (with their nested expects) to `automations.toml`.
/// Written owner-only; like `save_hosts`, this rewrites the file and does not
/// preserve hand-written comments.
pub fn save_automations(automations: &Automations) -> Result<(), ConfigError> {
    let path = automations_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string(automations)?;
    std::fs::write(&path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn load_settings() -> Result<Settings, ConfigError> {
    let path = settings_path();
    if !path.exists() {
        return Ok(Settings::default());
    }
    let content = std::fs::read_to_string(path)?;
    Ok(toml::from_str(&content)?)
}

pub fn save_settings(settings: &Settings) -> Result<(), ConfigError> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string(settings)?;
    std::fs::write(&path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn load_hosts() -> Result<(Vec<Host>, Vec<Group>), ConfigError> {
    let path = config_path();
    if !path.exists() {
        return Ok((Vec::new(), Vec::new()));
    }
    let content = std::fs::read_to_string(path)?;
    let file: HostsFile = toml::from_str(&content)?;
    Ok((file.hosts, file.groups))
}

pub fn save_hosts(hosts: &[Host], groups: &[Group]) -> Result<(), ConfigError> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string(&HostsFile {
        groups: groups.to_vec(),
        hosts: hosts.to_vec(),
    })?;
    std::fs::write(&path, content)?;
    // Topology (hostnames/IPs/usernames) is mildly sensitive — keep it owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod settings_tests {
    use super::Settings;

    #[test]
    fn settings_default_and_roundtrip() {
        let d = Settings::default();
        assert_eq!(d.notes.label, "Notes");
        assert!(!d.notes.hidden);

        // An empty file yields the defaults; a custom label + hidden round-trips.
        let empty: Settings = toml::from_str("").expect("empty settings parse");
        assert_eq!(empty.notes.label, "Notes");

        let mut s = Settings::default();
        s.notes.label = "Notlar".into();
        s.notes.hidden = true;
        let text = toml::to_string(&s).expect("serialize");
        let back: Settings = toml::from_str(&text).expect("parse");
        assert_eq!(back.notes.label, "Notlar");
        assert!(back.notes.hidden);
    }
}

// Unix-only: asserts against `/home/...` paths and drives `HOME` (on Windows the
// home dir comes from `%USERPROFILE%`).
#[cfg(all(test, unix))]
mod tests {
    use super::expand_tilde;
    use std::path::PathBuf;

    #[test]
    fn expand_tilde_resolves_home_prefixes() {
        // SAFETY: single-threaded test; we set HOME for the duration of this test.
        unsafe { std::env::set_var("HOME", "/home/gukab") };
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/gukab"));
        assert_eq!(
            expand_tilde("~/.ssh/id_ed25519"),
            PathBuf::from("/home/gukab/.ssh/id_ed25519")
        );
        assert_eq!(
            expand_tilde("$HOME/.ssh/id_rsa"),
            PathBuf::from("/home/gukab/.ssh/id_rsa")
        );
        // Absolute and embedded-tilde paths are left untouched.
        assert_eq!(expand_tilde("/etc/keys/k"), PathBuf::from("/etc/keys/k"));
        assert_eq!(expand_tilde("/a/~/b"), PathBuf::from("/a/~/b"));
    }
}

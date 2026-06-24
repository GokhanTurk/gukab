//! `gukab --trust-keychain` (macOS only).
//!
//! Gives the installed binary a **stable, per-machine code-signing identity** so
//! the macOS keychain's "Always Allow" sticks across app restarts *and* updates —
//! ending the repeated per-host keychain prompts.
//!
//! How it works: unsigned binaries are identified to the keychain ACL by their
//! content hash, which changes on every update, invalidating "Always Allow". A
//! binary signed with a fixed certificate is identified by that certificate, so
//! re-signing each new release with the *same* local cert keeps the grant valid.
//!
//! The certificate is **self-signed and never leaves this machine** (no Apple
//! Developer ID, no money). The private key is generated in a temp dir that is
//! wiped immediately after import; only `/usr/bin/codesign` is authorised to use
//! the key (no "allow all apps"). Host secrets keep living in their own keychain
//! entries — this changes nothing about how they are stored.

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

/// Keychain identity name and the binary's stable signing identifier. Both must
/// stay constant across releases so the keychain grant keeps matching.
const IDENTITY: &str = "gukab-local-signing";
const SIGN_IDENTIFIER: &str = "gukab";

/// macOS ships LibreSSL here; its PKCS#12 output imports cleanly via `security`,
/// unlike Homebrew's OpenSSL 3 (whose p12 MAC trips `SecKeychainItemImport`).
const OPENSSL: &str = "/usr/bin/openssl";

/// Transient password for the PKCS#12 bundle. It only has to match between export
/// and import; the bundle is generated in a 0600 temp dir and wiped immediately
/// after import, so it never protects anything at rest. (A non-empty password is
/// required — `security import` rejects empty-password p12 bundles.)
const P12_PASS: &str = "gukab-transient";

/// Sign this binary with the local identity (creating it on first run).
pub fn run() -> Result<(), String> {
    let binary = std::env::current_exe().map_err(|e| format!("cannot locate own binary: {e}"))?;
    ensure_identity()?;
    sign(&binary)?;
    verify(&binary)?;
    Ok(())
}

/// Run a command, returning stdout on success or a descriptive error (with
/// stderr) on failure.
fn cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            format!("`{program}` not found (is it installed?)")
        } else {
            format!("failed to run `{program}`: {e}")
        }
    })?;
    if !output.status.success() {
        return Err(format!(
            "`{program}` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// True if our certificate already exists in the keychain. We match on the
/// certificate (not `find-identity -p codesigning`): a self-signed cert is not
/// *trusted* for the code-signing policy, so `find-identity` omits it even though
/// `codesign --sign <name>` signs with it fine.
fn identity_exists() -> bool {
    Command::new("security")
        .args(["find-certificate", "-c", IDENTITY])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_identity() -> Result<(), String> {
    if identity_exists() {
        return Ok(());
    }
    create_identity()
}

/// Path to the user's login keychain (handling both modern and legacy names).
fn login_keychain() -> Result<String, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    let modern = format!("{home}/Library/Keychains/login.keychain-db");
    let legacy = format!("{home}/Library/Keychains/login.keychain");
    if Path::new(&modern).exists() {
        Ok(modern)
    } else if Path::new(&legacy).exists() {
        Ok(legacy)
    } else {
        // Fall back to the modern name; `security import` will report if wrong.
        Ok(modern)
    }
}

/// Generate a self-signed code-signing certificate and import it (key + cert)
/// into the login keychain, authorising only `codesign` to use the key.
fn create_identity() -> Result<(), String> {
    // Unique temp dir, wiped (key material!) on every exit path.
    let dir = std::env::temp_dir().join(format!("gukab-trust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create temp dir: {e}"))?;
    let result = create_identity_in(&dir);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn create_identity_in(dir: &Path) -> Result<(), String> {
    let cfg = dir.join("openssl.cnf");
    let key = dir.join("key.pem");
    let cert = dir.join("cert.pem");
    let p12 = dir.join("id.p12");

    // Config-file extensions (not `-addext`) for LibreSSL/macOS compatibility.
    let cfg_body = format!(
        "[req]\ndistinguished_name = dn\nx509_extensions = ext\nprompt = no\n\
         [dn]\nCN = {IDENTITY}\n\
         [ext]\nbasicConstraints = critical,CA:FALSE\n\
         keyUsage = critical,digitalSignature\n\
         extendedKeyUsage = critical,codeSigning\n"
    );
    write_private(&cfg, cfg_body.as_bytes())?;

    cmd(
        OPENSSL,
        &[
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650", "-keyout",
            path(&key)?, "-out", path(&cert)?, "-config", path(&cfg)?,
        ],
    )?;
    cmd(
        OPENSSL,
        &[
            "pkcs12", "-export", "-inkey", path(&key)?, "-in", path(&cert)?, "-out", path(&p12)?,
            "-name", IDENTITY, "-passout", &format!("pass:{P12_PASS}"),
        ],
    )?;
    // Restrict the freshly-written key/p12 to the owner before import.
    set_owner_only(&key);
    set_owner_only(&p12);

    let keychain = login_keychain()?;
    cmd(
        "security",
        &[
            "import", path(&p12)?, "-k", &keychain, "-P", P12_PASS, "-T", "/usr/bin/codesign",
        ],
    )?;
    Ok(())
}

fn sign(binary: &Path) -> Result<(), String> {
    cmd(
        "codesign",
        &[
            "--force", "--sign", IDENTITY, "--identifier", SIGN_IDENTIFIER, path(binary)?,
        ],
    )?;
    Ok(())
}

fn verify(binary: &Path) -> Result<(), String> {
    cmd("codesign", &["--verify", "--strict", path(binary)?])?;
    Ok(())
}

/// Borrow a path as `&str`, erroring on non-UTF-8 (vanishingly rare here).
fn path(p: &Path) -> Result<&str, String> {
    p.to_str()
        .ok_or_else(|| format!("non-UTF-8 path: {}", p.display()))
}

/// Write a file owner-only (0600).
fn write_private(p: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::write(p, bytes).map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    set_owner_only(p);
    Ok(())
}

fn set_owner_only(p: &Path) {
    let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
}

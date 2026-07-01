//! Per-session transcript logging.
//!
//! The interactive loop never touches the disk: it sends output chunks to an
//! unbounded channel, and a dedicated OS thread owns the file and writes them.
//! This keeps typing/echo latency unaffected even on slow storage.

use std::fs;
use std::io::{BufWriter, Write};

use chrono::Local;
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::config::log_dir;

/// Start logging this session's remote output. Returns a sender the caller pushes
/// output chunks into; dropping it flushes and closes the log. Returns `None`
/// (after a stderr warning) if the log file can't be created — logging must never
/// abort a connection. `label` names the per-session folder (host name/hostname for
/// SSH, device name for serial).
pub fn start(label: &str) -> Option<UnboundedSender<Vec<u8>>> {
    let dir = log_dir().join(sanitize(label));
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("[gukab] logging disabled (cannot create {}): {e}", dir.display());
        return None;
    }
    // Logs can contain sensitive command output (e.g. `show running-config`), so
    // keep the directory owner-only.
    set_owner_only(&dir, 0o700);

    let now = Local::now();
    let path = dir.join(format!("{}.log", now.format("%Y-%m-%d_%H-%M-%S")));
    let mut opts = fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600); // owner read/write only
    }
    let file = match opts.open(&path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[gukab] logging disabled (cannot open {}): {e}", path.display());
            return None;
        }
    };

    let mut writer = BufWriter::new(file);
    let _ = writeln!(
        writer,
        "==== gukab session {} — {} ====",
        now.format("%Y-%m-%d %H:%M:%S"),
        label
    );
    let _ = writer.flush();

    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        // Drain-then-flush: write the first chunk, sweep up anything already queued,
        // then flush once per burst so disk syscalls are batched off the hot path.
        while let Some(chunk) = rx.blocking_recv() {
            if writer.write_all(&chunk).is_err() {
                break;
            }
            while let Ok(more) = rx.try_recv() {
                if writer.write_all(&more).is_err() {
                    return;
                }
            }
            let _ = writer.flush();
        }
        let _ = writer.flush();
    });

    Some(tx)
}

/// Set owner-only permissions on `path` (no-op on non-unix). Best-effort.
#[cfg(unix)]
fn set_owner_only(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}
#[cfg(not(unix))]
fn set_owner_only(_path: &std::path::Path, _mode: u32) {}

/// Replace anything outside `[A-Za-z0-9._-]` with `_` so the host name is a safe
/// single path component. `.`/`..`/empty collapse to `host` so a crafted name
/// can never escape the log directory.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "host".to_string()
    } else {
        cleaned
    }
}

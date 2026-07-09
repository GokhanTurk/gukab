//! Plain-file notes shown in the right-hand panel.
//!
//! Each note is a `.md` file in `~/.config/gukab/notes/` (the folder name is
//! always `notes`, regardless of the section's display label). The title is the
//! file name (stem), so notes can be created and edited outside gukab too.
//! Editing opens the user's editor: `$VISUAL`/`$EDITOR` (falling back to
//! nano/vim/vi) on Linux and macOS, Notepad on Windows.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum NotesError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Invalid(String),
}

/// A note on disk: display title (file stem), path, and mtime for sorting.
pub struct NoteMeta {
    pub title: String,
    pub path: PathBuf,
    pub modified: SystemTime,
}

/// Make a title safe as a file name on every platform: strip path separators and
/// Windows-forbidden characters, control characters, and leading dots (hidden
/// files / `..`). Unicode (e.g. Turkish) titles pass through untouched.
pub fn sanitize_title(title: &str) -> String {
    let cleaned: String = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    // Windows also rejects trailing dots/spaces; leading dots would hide the file.
    cleaned
        .trim_start_matches('.')
        .trim_end_matches(['.', ' '])
        .trim()
        .to_string()
}

/// List the notes in `dir`, most recently modified first. A missing directory is
/// an empty list, not an error (the folder is created on the first note).
fn list_notes_in(dir: &Path) -> Vec<NoteMeta> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut notes: Vec<NoteMeta> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                return None;
            }
            let title = path.file_stem()?.to_string_lossy().into_owned();
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some(NoteMeta { title, path, modified })
        })
        .collect();
    notes.sort_by(|a, b| b.modified.cmp(&a.modified).then(a.title.cmp(&b.title)));
    notes
}

pub fn list_notes() -> Vec<NoteMeta> {
    list_notes_in(&crate::config::notes_dir())
}

pub fn read_note(path: &Path) -> Result<String, NotesError> {
    Ok(std::fs::read_to_string(path)?)
}

/// Resolve a validated, non-clashing path for `title` inside `dir`.
fn note_path(dir: &Path, title: &str) -> Result<PathBuf, NotesError> {
    let name = sanitize_title(title);
    if name.is_empty() {
        return Err(NotesError::Invalid("Title cannot be empty".into()));
    }
    let path = dir.join(format!("{name}.md"));
    if path.exists() {
        return Err(NotesError::Invalid(format!("A note named '{name}' already exists")));
    }
    Ok(path)
}

/// Create an empty note file for `title` in `dir` (created 0700 if missing) and
/// return its path. Notes may hold sensitive details, so files are owner-only,
/// like the rest of the config directory.
fn create_note_in(dir: &Path, title: &str) -> Result<PathBuf, NotesError> {
    let path = note_path(dir, title)?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    std::fs::write(&path, "")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

pub fn create_note(title: &str) -> Result<PathBuf, NotesError> {
    create_note_in(&crate::config::notes_dir(), title)
}

/// Rename a note file to `new_title` (same directory) and return the new path.
pub fn rename_note(path: &Path, new_title: &str) -> Result<PathBuf, NotesError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let new_path = note_path(dir, new_title)?;
    std::fs::rename(path, &new_path)?;
    Ok(new_path)
}

pub fn delete_note(path: &Path) -> Result<(), NotesError> {
    Ok(std::fs::remove_file(path)?)
}

/// The editor command (program + leading args) used to open a note.
///
/// Unix: `$VISUAL`, then `$EDITOR` (either may carry arguments, e.g.
/// "code --wait"), then the first of nano/vim/vi found on `PATH`. Windows:
/// Notepad — always present, so editing works out of the box.
fn resolve_editor() -> Result<Vec<String>, NotesError> {
    #[cfg(windows)]
    {
        Ok(vec!["notepad".into()])
    }
    #[cfg(not(windows))]
    {
        for var in ["VISUAL", "EDITOR"] {
            if let Ok(v) = std::env::var(var) {
                let parts: Vec<String> = v.split_whitespace().map(str::to_owned).collect();
                if !parts.is_empty() {
                    return Ok(parts);
                }
            }
        }
        for candidate in ["nano", "vim", "vi"] {
            if let Ok(paths) = std::env::var("PATH")
                && std::env::split_paths(&paths).any(|p| p.join(candidate).is_file())
            {
                return Ok(vec![candidate.into()]);
            }
        }
        Err(NotesError::Invalid(
            "no editor found — set $EDITOR or install nano/vim".into(),
        ))
    }
}

/// Open `path` in the user's editor and wait for it to close. The caller must
/// have released the terminal (left raw mode / alt screen) beforehand.
pub fn open_in_editor(path: &Path) -> Result<(), NotesError> {
    let cmd = resolve_editor()?;
    let status = std::process::Command::new(&cmd[0])
        .args(&cmd[1..])
        .arg(path)
        .status()?;
    if !status.success() {
        return Err(NotesError::Invalid(format!("editor exited with {status}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh per-test directory under the system tmp dir, removed on drop.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("gukab-notes-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sanitize_keeps_unicode_and_strips_forbidden() {
        assert_eq!(sanitize_title("yapılacaklar"), "yapılacaklar");
        assert_eq!(sanitize_title("a/b\\c:d*e"), "a_b_c_d_e");
        assert_eq!(sanitize_title("  .hidden  "), "hidden");
        assert_eq!(sanitize_title("dots..."), "dots");
        assert_eq!(sanitize_title("   "), "");
        assert_eq!(sanitize_title("vlan? \"listesi\""), "vlan_ _listesi_");
    }

    #[test]
    fn create_rename_delete_roundtrip() {
        let tmp = TempDir::new("crud");
        let path = create_note_in(&tmp.0, "switch şifreleri").expect("create");
        assert!(path.exists());
        assert!(matches!(
            create_note_in(&tmp.0, "switch şifreleri"),
            Err(NotesError::Invalid(_))
        ));
        std::fs::write(&path, "vlan 750").unwrap();
        assert_eq!(read_note(&path).unwrap(), "vlan 750");

        let renamed = rename_note(&path, "core notları").expect("rename");
        assert!(!path.exists());
        assert_eq!(read_note(&renamed).unwrap(), "vlan 750");

        delete_note(&renamed).expect("delete");
        assert!(!renamed.exists());
    }

    #[test]
    fn list_sorts_newest_first_and_ignores_non_md() {
        let tmp = TempDir::new("list");
        let old = tmp.0.join("old.md");
        let new = tmp.0.join("new.md");
        std::fs::write(&old, "").unwrap();
        std::fs::write(&new, "").unwrap();
        std::fs::write(tmp.0.join("ignored.txt"), "").unwrap();
        // Make mtimes unambiguous regardless of filesystem timestamp granularity.
        let t0 = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
        let t1 = t0 + std::time::Duration::from_secs(60);
        let set = |p: &Path, t| {
            std::fs::File::options()
                .append(true)
                .open(p)
                .and_then(|f| f.set_modified(t))
                .expect("set mtime");
        };
        set(&old, t0);
        set(&new, t1);

        let notes = list_notes_in(&tmp.0);
        let titles: Vec<&str> = notes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["new", "old"]);
        // Missing directory is just an empty list.
        assert!(list_notes_in(&tmp.0.join("nope")).is_empty());
    }
}

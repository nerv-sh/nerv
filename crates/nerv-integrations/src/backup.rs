use std::path::{Path, PathBuf};

use nerv_util::directories;

pub fn backup_file(
    path: impl AsRef<Path>,
    backup_dir: Option<impl Into<PathBuf>>,
) -> std::io::Result<()> {
    let pathref = path.as_ref();
    if pathref.exists() {
        let name: String = pathref.file_name().unwrap().to_string_lossy().into_owned();
        let dir = match backup_dir {
            Some(dir) => dir.into(),
            None => directories::utc_backup_dir().unwrap(),
        };
        std::fs::create_dir_all(&dir)?;
        std::fs::copy(path, dir.join(name).as_path())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `backup_file` is a no-op when the source path doesn't exist —
    /// the backup dir should NOT be created in that case. Catches a
    /// regression where the function would create empty backup dirs
    /// for every uninstall, even on first-run users.
    #[test]
    fn missing_source_skips_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backups");
        let src = tmp.path().join("nonexistent.zshrc");
        backup_file(&src, Some(&dir)).unwrap();
        assert!(!dir.exists(), "backup dir should not be created");
    }

    /// Existing source is copied into the explicit backup dir under
    /// its original filename. Locks the naming contract uninstall
    /// depends on for the recover hint.
    #[test]
    fn existing_source_copies_under_original_name() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backups");
        let src = tmp.path().join("zshrc.txt");
        std::fs::write(&src, b"hello").unwrap();
        backup_file(&src, Some(&dir)).unwrap();
        let copied = dir.join("zshrc.txt");
        assert!(copied.exists(), "copy missing");
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "hello");
    }

    /// Calling twice with the same source overwrites the prior backup
    /// (std::fs::copy semantics). Verify the second contents win so
    /// repeated uninstall attempts capture the latest .zshrc.
    #[test]
    fn repeated_backup_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("backups");
        let src = tmp.path().join("zshrc.txt");
        std::fs::write(&src, b"first").unwrap();
        backup_file(&src, Some(&dir)).unwrap();
        std::fs::write(&src, b"second").unwrap();
        backup_file(&src, Some(&dir)).unwrap();
        let copied = dir.join("zshrc.txt");
        assert_eq!(std::fs::read_to_string(&copied).unwrap(), "second");
    }
}

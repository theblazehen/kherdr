//! Atomic replacement only. Domain owners retain locking, revisions and limits.
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum DirectorySync {
    /// Fail before commit when the directory cannot be synchronized.
    Required,
    /// Kindle FAT storage may not implement directory synchronization.
    BestEffort,
}

/// Err always means rename did not commit. After rename, durability failures
/// are reported separately so callers still advance their in-memory state.
pub(crate) fn replace(
    path: &Path,
    bytes: &[u8],
    limit: usize,
    sync: DirectorySync,
) -> Result<(), String> {
    if bytes.len() > limit {
        return Err(format!("Store exceeds {limit} byte limit"));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = match File::open(parent) {
        Ok(directory) => Some(directory),
        Err(error) if sync == DirectorySync::Required => {
            return Err(format!("Cannot open {}: {error}", parent.display()));
        }
        Err(_) => None,
    };
    let temporary = parent.join(format!(".kherdr-{:032x}.tmp", rand::random::<u128>()));
    // Only unlink a temporary that this invocation successfully created.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|error| format!("Cannot create {}: {error}", temporary.display()))?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        if sync == DirectorySync::Required {
            if let Some(directory) = &directory {
                directory.sync_all()?;
            }
        }
        fs::rename(&temporary, path)
    })();
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_file(&temporary) {
            eprintln!("Temporary file cleanup {}: {cleanup}", temporary.display());
        }
        return Err(format!("Cannot replace {}: {error}", path.display()));
    }
    if let Some(directory) = directory {
        if let Err(error) = directory.sync_all() {
            // Unsupported FAT directory fsync is expected; genuine I/O failures
            // still deserve a diagnostic, but cannot undo an acknowledged commit.
            if sync == DirectorySync::Required
                || !matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOTSUP))
            {
                eprintln!(
                    "{} committed but directory durability is uncertain: {error}",
                    path.display()
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replacement_is_bounded_private_and_leaves_no_failed_temporary() {
        let root =
            std::env::temp_dir().join(format!("kherdr-atomic-{:032x}", rand::random::<u128>()));
        fs::create_dir(&root).unwrap();
        let path = root.join("state");
        fs::write(&path, b"old").unwrap();
        assert!(replace(&path, b"too long", 3, DirectorySync::Required).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        replace(&path, b"new", 3, DirectorySync::Required).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let directory = root.join("directory");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("kept"), b"unchanged").unwrap();
        assert!(replace(&directory, b"data", 4, DirectorySync::BestEffort).is_err());
        assert_eq!(fs::read(directory.join("kept")).unwrap(), b"unchanged");
        let mut names: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(names, ["directory", "state"]);
        fs::remove_dir_all(root).unwrap();
    }
}

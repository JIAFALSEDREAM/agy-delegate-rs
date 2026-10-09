//! Capability-relative traversal for managed task and conversation storage.
use anyhow::{Result, ensure};
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn reopen(root: &Dir, path: &Path, deadline: Option<Instant>) -> io::Result<Dir> {
    let mut directory = root.try_clone()?;
    for component in path.components() {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Managed storage scan exceeded its deadline",
            ));
        }
        directory = directory.open_dir_nofollow(component)?;
    }
    Ok(directory)
}

/// Sum regular-file lengths without following links; stop early above the limit.
/// File removals during scanning are harmless; unsafe entries fail closed.
pub(crate) fn usage(root: &Dir, limit: u64) -> Result<u64> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut pending = vec![PathBuf::new()];
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    while let Some(path) = pending.pop() {
        ensure!(
            Instant::now() < deadline,
            "Managed storage scan exceeded its deadline"
        );
        let directory = match reopen(root, &path, Some(deadline)) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in directory.entries()? {
            ensure!(
                Instant::now() < deadline,
                "Managed storage scan exceeded its deadline"
            );
            entries += 1;
            ensure!(
                entries <= 100_000,
                "Managed storage contains too many entries"
            );
            let entry = entry?;
            let metadata = match directory.symlink_metadata(entry.file_name()) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if metadata.is_dir() {
                let child = path.join(entry.file_name());
                ensure!(
                    child.as_os_str().len() <= 16_384 && pending.len() < 4096,
                    "Managed storage traversal limit exceeded"
                );
                pending.push(child);
            } else {
                ensure!(
                    metadata.is_file(),
                    "Managed storage contains a link or special file"
                );
                bytes = bytes.saturating_add(metadata.len());
                if bytes > limit {
                    ensure!(
                        Instant::now() < deadline,
                        "Managed storage scan exceeded its deadline"
                    );
                    return Ok(bytes);
                }
            }
        }
    }
    ensure!(
        Instant::now() < deadline,
        "Managed storage scan exceeded its deadline"
    );
    Ok(bytes)
}

/// Remove only descendants reachable through the supplied directory capability.
/// Descriptor and call-stack use stay constant for deeply nested storage.
pub(crate) fn remove(root: Dir) -> io::Result<()> {
    let mut pending = vec![(PathBuf::new(), false)];
    while let Some((path, visited)) = pending.pop() {
        if visited && path.as_os_str().is_empty() {
            continue;
        }
        let directory = reopen(&root, &path, None)?;
        if visited {
            directory.remove_open_dir()?;
        } else {
            pending.push((path.clone(), true));
            for entry in directory.entries()? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    pending.push((path.join(entry.file_name()), false));
                } else {
                    directory.remove_file(entry.file_name())?;
                }
            }
        }
    }
    // Nonrecursive removal preserves contents if the named root was replaced.
    root.remove_open_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn nested_files_count_together_and_stop_above_the_budget() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("one"), b"1234").unwrap();
        fs::write(temp.path().join("nested/two"), b"5678").unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        assert_eq!(usage(&root, 8).unwrap(), 8);
        assert!(usage(&root, 3).unwrap() > 3);
    }

    #[cfg(unix)]
    #[test]
    fn quota_rejects_links_and_removal_preserves_their_targets() {
        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("personal"), b"keep").unwrap();
        std::os::unix::fs::symlink(outside.path(), temp.path().join("escape")).unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        assert!(usage(&root, u64::MAX).is_err());
        remove(root).unwrap();
        assert!(!temp.path().exists());
        assert_eq!(fs::read(outside.path().join("personal")).unwrap(), b"keep");
    }

    #[test]
    fn traversal_does_not_retain_descriptors_for_every_ancestor() {
        let temp = tempfile::tempdir().unwrap();
        let mut path = temp.path().to_owned();
        for _ in 0..96 {
            path.push("d");
            fs::create_dir(&path).unwrap();
        }
        fs::write(path.join("leaf"), b"ok").unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        assert_eq!(usage(&root, 2).unwrap(), 2);
        remove(root).unwrap();
        assert!(!temp.path().exists());
    }
}

//! Capability-relative traversal for managed task and conversation storage.
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::{
    io,
    path::{Path, PathBuf},
    time::Instant,
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

//! Capability-relative traversal for managed task and conversation storage.
use anyhow::{Result, ensure};
use cap_fs_ext::DirExt;
use cap_std::fs::Dir;
use std::{
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug)]
struct ScanDeadline;

impl std::fmt::Display for ScanDeadline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Managed storage scan exceeded its deadline")
    }
}

impl std::error::Error for ScanDeadline {}

fn reopen(
    root: &Dir,
    path: &Path,
    mut check_deadline: impl FnMut() -> io::Result<()>,
) -> io::Result<Dir> {
    let mut directory = root.try_clone()?;
    for component in path.components() {
        check_deadline()?;
        directory = directory.open_dir_nofollow(component)?;
    }
    Ok(directory)
}

/// Sum regular-file lengths without following links; stop early above the limit.
/// File removals during scanning are harmless; unsafe entries fail closed.
/// Retry one scan deadline with fresh counters and the same per-scan bounds.
pub(crate) fn usage(root: &Dir, limit: u64) -> Result<u64> {
    usage_with_clock(root, limit, &mut Instant::now)
}

fn usage_with_clock(root: &Dir, limit: u64, now: &mut impl FnMut() -> Instant) -> Result<u64> {
    match scan_usage(root, limit, now) {
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .is_some_and(|source| source.is::<ScanDeadline>()) =>
        {
            scan_usage(root, limit, now)
        }
        result => result,
    }
}

fn scan_usage(root: &Dir, limit: u64, now: &mut impl FnMut() -> Instant) -> Result<u64> {
    let deadline = now() + Duration::from_secs(2);
    let mut check_deadline = || {
        if now() >= deadline {
            Err(io::Error::new(io::ErrorKind::TimedOut, ScanDeadline))
        } else {
            Ok(())
        }
    };
    let mut pending = vec![PathBuf::new()];
    let mut bytes = 0_u64;
    let mut entries = 0_u64;
    while let Some(path) = pending.pop() {
        check_deadline()?;
        let directory = match reopen(root, &path, &mut check_deadline) {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        for entry in directory.entries()? {
            check_deadline()?;
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
                    check_deadline()?;
                    return Ok(bytes);
                }
            }
        }
    }
    check_deadline()?;
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
        let directory = reopen(&root, &path, || Ok(()))?;
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
    fn transient_deadline_restarts_and_counts_the_complete_tree() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("nested")).unwrap();
        fs::write(temp.path().join("one"), b"1234").unwrap();
        fs::write(temp.path().join("nested/two"), b"56789").unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        let start = Instant::now();
        // Expire during root entries, component reopening, and the final check.
        for jump_at in [4, 6, 8] {
            let mut ticks = 0;
            let result = usage_with_clock(&root, u64::MAX, &mut || {
                ticks += 1;
                start + Duration::from_secs(if ticks >= jump_at { 3 } else { 0 })
            });
            assert_eq!(result.unwrap(), 9, "jump at clock read {jump_at}");
        }
    }

    #[test]
    fn persistent_deadlines_fail_after_two_scans() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("one"), b"1234").unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        let start = Instant::now();
        let mut ticks = 0;
        let error = usage_with_clock(&root, u64::MAX, &mut || {
            assert!(ticks < 4, "deadline retry must be bounded");
            let time = start + Duration::from_secs(ticks * 3);
            ticks += 1;
            time
        })
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Managed storage scan exceeded its deadline"
        );
        assert_eq!(ticks, 4);
    }

    #[test]
    fn rescan_counts_new_files_and_stops_above_quota() {
        let temp = tempfile::tempdir().unwrap();
        fs::write(temp.path().join("one"), b"1234").unwrap();
        fs::write(temp.path().join("two"), b"5678").unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        let start = Instant::now();
        let mut ticks = 0;
        let result = usage_with_clock(&root, 16, &mut || {
            ticks += 1;
            if ticks == 4 {
                fs::write(temp.path().join("late"), b"123456789").unwrap();
            }
            start + Duration::from_secs(if ticks >= 4 { 3 } else { 0 })
        });
        assert_eq!(result.unwrap(), 17);
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_entries_fail_immediately_or_when_added_for_rescan() {
        use std::os::unix::ffi::OsStrExt;

        for (retry, link) in [(false, true), (true, true), (true, false)] {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("one"), b"1234").unwrap();
            fs::write(temp.path().join("two"), b"5678").unwrap();
            let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
            let add_link =
                || std::os::unix::fs::symlink("one", temp.path().join("unsafe")).unwrap();
            if !retry {
                add_link();
            }
            let start = Instant::now();
            let mut ticks = 0;
            let error = usage_with_clock(&root, u64::MAX, &mut || {
                ticks += 1;
                assert!(
                    ticks <= if retry { 10 } else { 5 },
                    "unsafe entries must not retry"
                );
                if retry && ticks == 4 {
                    if link {
                        add_link();
                    } else {
                        let path = temp.path().join("unsafe");
                        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
                        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                    }
                }
                start + Duration::from_secs(if retry && ticks >= 4 { 3 } else { 0 })
            })
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "Managed storage contains a link or special file"
            );
        }
    }

    #[test]
    fn reopening_io_errors_do_not_retry() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir(&nested).unwrap();
        let root = Dir::open_ambient_dir(temp.path(), cap_std::ambient_authority()).unwrap();
        let start = Instant::now();
        let mut ticks = 0;
        let error = usage_with_clock(&root, u64::MAX, &mut || {
            ticks += 1;
            assert!(ticks <= 5, "I/O errors must not retry");
            if ticks == 4 {
                fs::remove_dir(&nested).unwrap();
                fs::write(&nested, b"replaced").unwrap();
            }
            start
        })
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<io::Error>().unwrap().kind(),
            io::ErrorKind::NotADirectory
        );
    }

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

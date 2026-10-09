//! Private agy profiles; shared account files are read only and never cleaned.
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{DirExt, FollowSymlinks, MetadataExt, OpenOptionsFollowExt};
use cap_std::fs::Dir;
use std::{
    io,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(crate) struct Profiles {
    root: Arc<Dir>,
    path: PathBuf,
}
pub(crate) struct Profile {
    pub path: PathBuf,
    pub directory: Arc<Dir>,
}

fn child(parent: &Dir, name: &str) -> io::Result<Dir> {
    match parent.create_dir(name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let directory = parent.open_dir_nofollow(name)?;
    #[cfg(unix)]
    {
        use cap_std::fs::{Permissions, PermissionsExt};
        directory.set_permissions(".", Permissions::from_mode(0o700))?;
    }
    Ok(directory)
}

impl Profiles {
    pub fn new(state: &Path) -> Result<Self> {
        let parent = Dir::open_ambient_dir(state, cap_std::ambient_authority())?;
        Ok(Self {
            root: Arc::new(child(&parent, "conversations")?),
            path: state.join("conversations"),
        })
    }

    pub fn prepare(&self, id: &str) -> Result<Profile> {
        ensure!(
            uuid::Uuid::parse_str(id)?.to_string() == id,
            "Invalid conversation root"
        );
        let current = Dir::open_ambient_dir(&self.path, cap_std::ambient_authority())?;
        let expected = self.root.dir_metadata()?;
        let actual = current.dir_metadata()?;
        ensure!(
            actual.dev() == expected.dev() && actual.ino() == expected.ino(),
            "Conversation storage root changed"
        );
        let directory = child(&self.root, id)?;
        let home = directories::BaseDirs::new().context("Cannot find home directory")?;
        seed(&directory, &home.home_dir().join(".gemini/antigravity-cli"))?;
        Ok(Profile {
            path: self.path.join(id),
            directory: Arc::new(directory),
        })
    }
}

/// Copy only the known login/settings files, once per profile. Refreshes stay private.
/// Never copy host conversations, plugins, hooks or MCP configuration.
fn seed(profile: &Dir, shared: &Path) -> Result<()> {
    let source = match Dir::open_ambient_dir(shared, cap_std::ambient_authority()) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("Cannot open shared agy login directory"),
    };
    let target = child(profile, "antigravity-cli")?;
    for name in ["settings.json", "antigravity-oauth-token"] {
        match target.symlink_metadata(name) {
            Ok(metadata) => {
                ensure!(
                    metadata.is_file(),
                    "Private agy login must be a regular file"
                );
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let mut options = cap_std::fs::OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let mut file = match source.open_with(name, &options) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error).context("Cannot read shared agy login file"),
        };
        ensure!(
            file.metadata()?.is_file(),
            "Shared agy login must be a regular file"
        );
        let mut bytes = Vec::new();
        (&mut file).take(1_048_577).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() <= 1_048_576,
            "Shared agy login file is too large"
        );
        let temporary = format!(".login-{}", uuid::Uuid::new_v4());
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| -> Result<()> {
            let mut file = target.open_with(&temporary, &options)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            target.rename(&temporary, &target, name)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = target.remove_file(&temporary);
        }
        result?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[test]
    fn copies_only_login_files_and_keeps_private_refreshes() {
        let shared = tempfile::tempdir().unwrap();
        let private = tempfile::tempdir().unwrap();
        for name in [
            "settings.json",
            "antigravity-oauth-token",
            "mcp_config.json",
            "private.db",
        ] {
            fs::write(shared.path().join(name), "fixture").unwrap();
        }
        let directory =
            Dir::open_ambient_dir(private.path(), cap_std::ambient_authority()).unwrap();
        seed(&directory, shared.path()).unwrap();
        let target = private.path().join("antigravity-cli");
        assert_eq!(fs::read_dir(&target).unwrap().count(), 2);
        fs::write(target.join("antigravity-oauth-token"), "refreshed").unwrap();
        seed(&directory, shared.path()).unwrap();
        assert_eq!(
            fs::read(target.join("antigravity-oauth-token")).unwrap(),
            b"refreshed"
        );
        assert_eq!(
            fs::read(shared.path().join("antigravity-oauth-token")).unwrap(),
            b"fixture"
        );
    }
}

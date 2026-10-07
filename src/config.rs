use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[arg(long, default_value = "codex")]
    pub codex: PathBuf,
    #[arg(long, default_value = "agy")]
    pub agy: PathBuf,
    #[arg(long)]
    pub agy_env_file: Option<PathBuf>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=7200))]
    pub timeout: u64,
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
}

pub fn private_dir(path: &Path) -> Result<PathBuf> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(path.canonicalize()?)
}

impl Config {
    pub fn state(&self) -> Result<PathBuf> {
        let default = directories::ProjectDirs::from("", "", "agy-delegate-rs")
            .context("Cannot find user data directory")?;
        private_dir(
            self.state_dir
                .as_deref()
                .unwrap_or(default.data_local_dir()),
        )
    }

    pub fn environment(&self) -> Result<BTreeMap<String, String>> {
        let mut env: BTreeMap<_, _> = std::env::vars().collect();
        if let Some(path) = &self.agy_env_file {
            for entry in dotenvy::from_path_iter(path)? {
                let (key, value) = entry?;
                env.entry(key).or_insert(value);
            }
        }
        Ok(env)
    }

    pub fn sandbox(&self, cwd: &Path, tmp: &Path) -> Result<Command> {
        let home = directories::BaseDirs::new().context("Cannot find home directory")?;
        let profile = private_dir(&home.home_dir().join(".gemini"))?;
        let cwd = cwd.canonicalize()?;
        ensure!(cwd.is_dir(), "cwd must be an existing directory");
        let tmp = tmp.canonicalize()?;
        let mut paths = toml::map::Map::new();
        paths.insert(":root".into(), "read".into());
        #[cfg(windows)]
        for drive in b'A'..=b'Z' {
            let root = format!("{}:\\", drive as char);
            if Path::new(&root).is_dir() {
                paths.insert(root, "read".into());
            }
        }
        for writable in [&profile, &tmp] {
            ensure!(
                !cwd.starts_with(writable) && !writable.starts_with(&cwd),
                "cwd overlaps writable agy profile or temporary directory"
            );
            paths.insert(writable.to_string_lossy().into_owned(), "write".into());
        }
        let mut command = Command::new(&self.codex);
        command.args([
            "sandbox",
            "-P",
            "agy_delegate",
            "-c",
            "permissions.agy_delegate.network.enabled=true",
        ]);
        command.arg("-c").arg(format!(
            "permissions.agy_delegate.filesystem={}",
            toml::Value::Table(paths)
        ));
        for setting in [
            "shell_environment_policy.inherit=all",
            "shell_environment_policy.ignore_default_excludes=true",
            "shell_environment_policy.exclude=[]",
            "shell_environment_policy.include_only=[]",
        ] {
            command.args(["-c", setting]);
        }
        #[cfg(windows)]
        command.args(["-c", "windows.sandbox=unelevated"]);
        command.arg("-C").arg(cwd).arg("--");
        command
            .envs(self.environment()?)
            .env("TMPDIR", &tmp)
            .env("GIT_OPTIONAL_LOCKS", "0");
        Ok(command)
    }
}

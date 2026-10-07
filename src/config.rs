use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
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

fn env_key(key: OsString) -> OsString {
    #[cfg(windows)]
    {
        key.to_ascii_uppercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
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

    pub fn environment(&self) -> Result<BTreeMap<OsString, OsString>> {
        let mut env: BTreeMap<_, _> = std::env::vars_os()
            .map(|(key, value)| (env_key(key), value))
            .collect();
        if let Some(path) = &self.agy_env_file {
            let entries = dotenvy::from_path_iter(path)
                .map_err(|_| anyhow::anyhow!("Cannot open agy environment file"))?;
            for entry in entries {
                let (key, value) = entry.map_err(|_| {
                    anyhow::anyhow!("Invalid agy environment file; contents are not logged")
                })?;
                env.entry(env_key(key.into())).or_insert(value.into());
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
        let environment = self.environment()?;
        let executable = self.executable(&self.codex, &environment, &std::env::current_dir()?);
        let mut command = Command::new(executable);
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
            .envs(environment)
            .env("TMPDIR", &tmp)
            .env("TMP", &tmp)
            .env("TEMP", &tmp)
            .env("GIT_OPTIONAL_LOCKS", "0");
        Ok(command)
    }

    pub fn executable(
        &self,
        program: &Path,
        environment: &BTreeMap<OsString, OsString>,
        cwd: &Path,
    ) -> PathBuf {
        let paths = environment.get(&OsString::from("PATH"));
        #[cfg(windows)]
        let paths = paths.or_else(|| {
            environment.iter().find_map(|(key, value)| {
                key.to_string_lossy()
                    .eq_ignore_ascii_case("PATH")
                    .then_some(value)
            })
        });
        which::which_in(program, paths, cwd).unwrap_or_else(|_| program.to_owned())
    }
}

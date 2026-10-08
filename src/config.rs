use anyhow::{Context, Result, ensure};
use clap::{ArgMatches, Args, parser::ValueSource};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tokio::process::Command;

#[derive(Args, Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[arg(long, default_value = "codex")]
    pub codex: PathBuf,
    #[arg(long, default_value = "agy")]
    pub agy: PathBuf,
    /// Arguments before agy's managed headless options; repeat for wrapper commands.
    #[arg(long = "agy-arg", allow_hyphen_values = true)]
    pub agy_args: Vec<String>,
    #[arg(long)]
    pub agy_env_file: Option<PathBuf>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value_t = 600, value_parser = clap::value_parser!(u64).range(1..=7200))]
    pub timeout: u64,
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
    #[arg(long, default_value_t = 4000, value_parser = clap::value_parser!(u64).range(256..=6000))]
    pub report_chars: u64,
    #[arg(long, default_value_t = 14, value_parser = clap::value_parser!(u64).range(1..=365))]
    pub retention_days: u64,
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(10..=10000))]
    pub max_history_jobs: u64,
    #[arg(long, default_value_t = 4_000_000, value_parser = clap::value_parser!(u64).range(4096..=32_000_000))]
    pub log_bytes: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            codex: "codex".into(),
            agy: "agy".into(),
            agy_args: Vec::new(),
            agy_env_file: None,
            model: None,
            timeout: 600,
            state_dir: None,
            report_chars: 4000,
            retention_days: 14,
            max_history_jobs: 1000,
            log_bytes: 4_000_000,
        }
    }
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
    pub fn file_path() -> Result<PathBuf> {
        Ok(directories::ProjectDirs::from("", "", "agy-delegate-rs")
            .context("Cannot find user configuration directory")?
            .config_dir()
            .join("config.toml"))
    }

    fn resolve_paths(&mut self, base: &Path) {
        for executable in [&mut self.codex, &mut self.agy] {
            if executable.is_relative() && executable.components().count() > 1 {
                *executable = base.join(&*executable);
            }
        }
        for path in [&mut self.agy_env_file, &mut self.state_dir]
            .into_iter()
            .flatten()
        {
            if path.is_relative() && !path.as_os_str().is_empty() {
                *path = base.join(&*path);
            }
        }
    }

    pub fn absolutize(&mut self) -> Result<()> {
        self.validate()?;
        let cwd = std::env::current_dir()?;
        self.resolve_paths(&cwd);
        self.state_dir = Some(self.state()?);
        let environment = self.lookup_environment()?;
        self.codex = self.executable(&self.codex, &environment, &cwd);
        self.agy = self.executable(&self.agy, &environment, &cwd);
        ensure!(
            self.codex.is_absolute() && self.agy.is_absolute(),
            "Cannot resolve Codex and agy executables to absolute paths"
        );
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (4096..=32_000_000).contains(&self.log_bytes),
            "log_bytes must be 4096–32000000"
        );
        ensure!((1..=7200).contains(&self.timeout), "timeout must be 1–7200");
        ensure!(
            (256..=6000).contains(&self.report_chars),
            "report_chars must be 256–6000"
        );
        ensure!(
            (1..=365).contains(&self.retention_days),
            "retention_days must be 1–365"
        );
        ensure!(
            (10..=10000).contains(&self.max_history_jobs),
            "max_history_jobs must be 10–10000"
        );
        for path in [
            Some(&self.codex),
            Some(&self.agy),
            self.agy_env_file.as_ref(),
            self.state_dir.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                !path.as_os_str().is_empty(),
                "Configured paths must not be empty"
            );
        }
        Ok(())
    }

    pub fn load(path: Option<&Path>, overrides: Self, matches: &ArgMatches) -> Result<Self> {
        let selected = match path {
            Some(path) => path.to_owned(),
            None => Self::file_path()?,
        };
        let mut config = match fs::read_to_string(&selected) {
            Ok(text) => toml::from_str::<Self>(&text).map_err(|_| {
                anyhow::anyhow!(
                    "Invalid TOML configuration or unknown setting; contents are not logged"
                )
            })?,
            Err(error) if path.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
                Self::default()
            }
            Err(error) => return Err(error).context("Cannot read configuration file"),
        };
        let absolute = std::env::current_dir()?.join(&selected);
        config.resolve_paths(
            absolute
                .parent()
                .context("Configuration file needs a parent directory")?,
        );
        macro_rules! override_fields {
            ($($field:ident),*) => { $(
                if matches.value_source(stringify!($field)) == Some(ValueSource::CommandLine) {
                    config.$field = overrides.$field;
                }
            )* };
        }
        override_fields!(
            codex,
            agy,
            agy_args,
            agy_env_file,
            model,
            timeout,
            state_dir,
            report_chars,
            retention_days,
            max_history_jobs,
            log_bytes
        );
        config.validate()?;
        Ok(config)
    }

    pub fn initialize(mut self, path: &Path) -> Result<()> {
        self.validate()?;
        self.resolve_paths(&std::env::current_dir()?);
        let text = format!(
            "# Credentials belong in the inherited environment or a separate agy_env_file.\n{}\n# agy_env_file = \"/path/to/api.env\"\n# model = \"your-model\"\n# state_dir = \"/path/to/state\"\n",
            toml::to_string_pretty(&self)?
        );
        let absolute = std::env::current_dir()?.join(path);
        let parent = absolute
            .parent()
            .context("Configuration file needs a parent directory")?;
        fs::create_dir_all(parent)?;
        let mut file = tempfile::Builder::new()
            .prefix(".config-")
            .tempfile_in(parent)?;
        file.write_all(text.as_bytes())?;
        file.as_file().sync_all()?;
        file.persist_noclobber(absolute)
            .context("Cannot create configuration; existing files are never overwritten")?;
        Ok(())
    }

    pub fn state(&self) -> Result<PathBuf> {
        match &self.state_dir {
            Some(path) => private_dir(path),
            None => private_dir(
                directories::ProjectDirs::from("", "", "agy-delegate-rs")
                    .context("Cannot find user data directory")?
                    .data_local_dir(),
            ),
        }
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

    /// Executable lookup uses only PATH from an available environment file.
    /// Missing credentials must not prevent registration or removal.
    pub fn lookup_environment(&self) -> Result<BTreeMap<OsString, OsString>> {
        let mut inherited = self.clone();
        inherited.agy_env_file = None;
        let mut environment = inherited.environment()?;
        let key = env_key("PATH".into());
        if !environment.contains_key(&key)
            && let Ok(effective) = self.environment()
            && let Some(path) = effective.get(&key)
        {
            environment.insert(key, path.clone());
        }
        Ok(environment)
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
    pub fn secrets(environment: &BTreeMap<OsString, OsString>) -> Result<Vec<String>> {
        environment
            .iter()
            .filter(|(key, value)| {
                let key = key.to_string_lossy().to_ascii_uppercase();
                value.len() >= 8
                    && ["KEY", "TOKEN", "SECRET"]
                        .iter()
                        .any(|marker| key.contains(marker))
            })
            .map(|(_, value)| {
                value
                    .to_str()
                    .map(str::to_owned)
                    .context("Credential environment values must be UTF-8")
            })
            .collect()
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

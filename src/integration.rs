use crate::{
    config::{Config, private_dir},
    doctor,
};
use anyhow::{Context, Result, ensure};
use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::process::Command;

const NAME: &str = "agy-delegate-rs";

fn write(path: &Path, bytes: &[u8], create: bool) -> Result<()> {
    let parent = path.parent().context("Missing file parent")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if create {
        file.persist_noclobber(path)?;
    } else {
        file.persist(path)?;
    }
    Ok(())
}

fn read(path: &Path) -> Result<String> {
    let root = cap_std::fs::Dir::open_ambient_dir(
        path.parent().context("Missing integration file parent")?,
        cap_std::ambient_authority(),
    )?;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match root.open_with(
        path.file_name().context("Missing integration filename")?,
        &options,
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error.into()),
    };
    ensure!(
        file.metadata()?.is_file(),
        "Integration file must be a regular file"
    );
    let mut text = String::new();
    file.take(1_048_577).read_to_string(&mut text)?;
    ensure!(text.len() <= 1_048_576, "Integration file exceeds 1 MB");
    Ok(text)
}

pub fn home(selected: Option<&Path>) -> Result<PathBuf> {
    let chosen = selected
        .map(Path::to_owned)
        .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from));
    let path = match chosen {
        Some(path) => path,
        None => directories::BaseDirs::new()
            .context("Cannot find home directory")?
            .home_dir()
            .join(".codex"),
    };
    ensure!(!path.as_os_str().is_empty(), "Codex home must not be empty");
    private_dir(&path)
}

pub struct Integration {
    config: Config,
    home: PathBuf,
    snapshot: PathBuf,
    // A stable lease serializes install/register/uninstall in this Codex home.
    _lease: fs::File,
}

impl Integration {
    pub fn new(config: Config, selected: Option<&Path>) -> Result<Self> {
        let home = home(selected)?;
        let root = cap_std::fs::Dir::open_ambient_dir(&home, cap_std::ambient_authority())?;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NONBLOCK);
        }
        let lease = root
            .open_with(".agy-delegate-rs.lock", &options)?
            .into_std();
        ensure!(
            lease.metadata()?.is_file(),
            "Integration lock must be a regular file"
        );
        fs2::FileExt::try_lock_exclusive(&lease)
            .context("Another integration command is active")?;
        let state = config.state()?;
        let identity = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            home.to_str()
                .context("Codex home must be UTF-8")?
                .as_bytes(),
        );
        Ok(Self {
            config,
            snapshot: state
                .join("registrations")
                .join(identity.to_string())
                .join("registered.toml"),
            home,
            _lease: lease,
        })
    }

    fn argv(&self) -> Result<Vec<String>> {
        Ok(vec![
            "--config".into(),
            self.snapshot
                .to_str()
                .context("Configuration path must be UTF-8")?
                .into(),
            "mcp".into(),
        ])
    }

    fn registrations(&self) -> Result<toml::Value> {
        let text = read(&self.home.join("config.toml"))?;
        toml::from_str(&text).map_err(|_| {
            anyhow::anyhow!("Cannot parse Codex configuration; contents are not logged")
        })
    }

    fn check_registration(&self) -> Result<bool> {
        let registrations = self.registrations()?;
        let servers = &registrations.get("mcp_servers");
        let existing = servers.and_then(|s| s.get(NAME));
        if let Some(entry) = existing {
            let argv = self.argv()?;
            let exe = std::env::current_exe()?;
            ensure!(
                entry.get("command").and_then(toml::Value::as_str) == exe.to_str()
                    && entry
                        .get("args")
                        .and_then(toml::Value::as_array)
                        .is_some_and(|args| args.len() == argv.len()
                            && args
                                .iter()
                                .filter_map(toml::Value::as_str)
                                .eq(argv.iter().map(String::as_str))),
                "An unrelated agy-delegate-rs MCP registration already exists"
            );
        }
        Ok(existing.is_some())
    }

    async fn codex(&self, args: &[String]) -> Result<()> {
        let environment = self.config.lookup_environment()?;
        let mut command = Command::new(self.config.executable(
            &self.config.codex,
            &environment,
            &std::env::current_dir()?,
        ));
        command
            .envs(environment)
            .env("CODEX_HOME", &self.home)
            .args(args);
        doctor::capture(command)
            .await
            .context("Codex MCP operation failed; inspect Codex configuration")?;
        Ok(())
    }

    pub async fn register(&mut self) -> Result<Value> {
        ensure!(
            self.registrations()?
                .get("mcp_servers")
                .and_then(|s| s.get("agy-delegate"))
                .is_none(),
            "Python agy-delegate is still registered; migrate its registration before installing Rust tools"
        );
        let existing = self.check_registration()?;
        let mut config = self.config.clone();
        // Persist effective CLI overrides; resolve paths before Codex starts from another cwd.
        config.absolutize()?;
        write(
            &self.snapshot,
            toml::to_string_pretty(&config)?.as_bytes(),
            false,
        )?;
        if !existing {
            let mut args = vec![
                "mcp".into(),
                "add".into(),
                NAME.into(),
                "--".into(),
                std::env::current_exe()?
                    .to_str()
                    .context("Executable path must be UTF-8")?
                    .into(),
            ];
            args.extend(self.argv()?);
            self.codex(&args).await?;
        }
        Ok(json!({"registered":true,"name":NAME,"config":self.snapshot,"codex_home":self.home}))
    }

    pub async fn uninstall(&mut self) -> Result<Value> {
        self.check_registration()?;
        let mut config = self.config.clone();
        config.agy_env_file = None;
        let shutdown = crate::jobs::Jobs::new(config)?.shutdown()?;
        shutdown.stop_all().await?;
        if self.check_registration()? {
            self.codex(&["mcp".into(), "remove".into(), NAME.into()])
                .await?;
        }
        Ok(json!({"uninstalled":true,"data_preserved":self.config.state()?}))
    }
}

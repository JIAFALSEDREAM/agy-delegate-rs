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

fn sync_parent(path: &Path) -> Result<()> {
    #[cfg(unix)]
    fs::File::open(path.parent().context("Missing file parent")?)?.sync_all()?;
    // Windows has no portable directory fsync; file sync and atomic replacement
    // support process-interruption recovery, not a power-loss durability guarantee.
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

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
    sync_parent(path)
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

fn restoring(mut backup: Value, text: &str) -> Result<Value> {
    // Added rules are reconstructible; omit their duplicate to bound recovery state.
    backup
        .as_object_mut()
        .context("Invalid policy backup")?
        .remove("added");
    backup["restored"] = text.into();
    ensure!(
        backup.to_string().len() <= 1_048_576,
        "Restoration backup must fit within 1 MB"
    );
    Ok(backup)
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
    backup: PathBuf,
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
            backup: home.join(".agy-delegate-rs-policy.json"),
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
        self.policy_plan(true)?;
        let mut config = self.config.clone();
        config.agy_env_file = None;
        let shutdown = crate::jobs::Jobs::new(config)?.shutdown()?;
        shutdown.stop_all().await?;
        // Re-read after waiting so outside rules changed during shutdown survive.
        self.policy_plan(true)?;
        if self.check_registration()? {
            self.codex(&["mcp".into(), "remove".into(), NAME.into()])
                .await?;
        }
        if let Some((text, _)) = self.policy_plan(true)? {
            let backup = restoring(serde_json::from_str(&read(&self.backup)?)?, &text)?;
            write(&self.backup, backup.to_string().as_bytes(), false)?;
            let agents = self.home.join("AGENTS.md");
            if text.is_empty() && backup["existed"] == false {
                if agents.try_exists()? {
                    fs::remove_file(&agents)?;
                    sync_parent(&agents)?;
                }
            } else {
                write(&agents, text.as_bytes(), false)?;
            }
            fs::remove_file(&self.backup)?;
            sync_parent(&self.backup)?;
        }
        Ok(json!({"uninstalled":true,"data_preserved":self.config.state()?}))
    }
}
const START: &str = "<!-- agy-delegate-rs:start -->";
const END: &str = "<!-- agy-delegate-rs:end -->";
pub const POLICY: &str = "<!-- agy-delegate-rs:start -->\n## Subagent delegation\n- Use agy-delegate-rs MCP tools by default for bounded, read-only repository searches, inventories, counts, configuration checks, and mechanical comparisons. Submit a precise task and absolute directory with agy_submit.\n- Continue independent work while the task runs. Use agy_wait or agy_status, and agy_followup for targeted follow-up in the same conversation. Use agy_list to recover IDs.\n- Check returned status. Failed, incomplete, blocked, interrupted, cancelled, and timed-out tasks are not completed investigations. Surface the limitation, then retry or use the fallback. Do not duplicate active investigations; cancel abandoned jobs.\n- Use quick_explorer as the native fallback when MCP is unavailable. Use explorer only when explicitly requested or for unusually large read-heavy investigations that materially benefit from separate context.\n- Reports should contain Conclusion, Essential evidence with file/line references, and Uncertainty or the smallest useful next check. Omit transcripts and unrelated inventories.\n<!-- agy-delegate-rs:end -->";

impl Integration {
    fn policy_plan(&self, remove: bool) -> Result<Option<(String, Option<Value>)>> {
        let text = read(&self.home.join("AGENTS.md"))?;
        let backup_text = read(&self.backup)?;
        let backup = if self.backup.try_exists()? {
            let backup: Value =
                serde_json::from_str(&backup_text).context("Invalid managed policy backup")?;
            let original = backup["original"]
                .as_str()
                .context("Invalid policy backup")?;
            ensure!(
                backup["existed"].is_boolean()
                    && !original.contains(START)
                    && !original.contains(END)
                    && (backup["added"].as_str()
                        == Some(format!("{original}\n\n{POLICY}\n").as_str())
                        || (backup.get("added").is_none() && backup.get("restored").is_some())),
                "Invalid managed policy backup"
            );
            if let Some(restored) = backup.get("restored") {
                let restored = restored.as_str().context("Invalid restoration state")?;
                ensure!(
                    !restored.contains(START) && !restored.contains(END),
                    "Invalid restoration state"
                );
            }
            Some(backup)
        } else {
            None
        };
        ensure!(
            remove
                || backup
                    .as_ref()
                    .is_none_or(|value| value.get("restored").is_none()),
            "Uninstall was interrupted; retry uninstall before installing"
        );
        if let Some(start) = text.find(START) {
            let end = text[start..]
                .find(END)
                .context("Managed policy end marker is missing")?
                + start
                + END.len();
            ensure!(
                text.matches(START).count() == 1
                    && text.matches(END).count() == 1
                    && &text[start..end] == POLICY,
                "Managed policy was edited; preserve it and restore manually"
            );
            let backup = backup.context("Managed policy backup is missing")?;
            if remove {
                let next =
                    if text == format!("{}\n\n{POLICY}\n", backup["original"].as_str().unwrap()) {
                        backup["original"].as_str().unwrap().to_owned()
                    } else {
                        format!("{}{}", &text[..start], &text[end..])
                    };
                restoring(backup.clone(), &next)?;
                return Ok(Some((next, None)));
            }
            return Ok(None);
        }
        ensure!(
            !text.contains(END),
            "Managed policy start marker is missing"
        );
        if let Some(backup) = backup {
            if let Some(restored) = backup["restored"].as_str() {
                ensure!(
                    text == restored,
                    "Pending restoration rules changed; preserve the backup and restore manually"
                );
                return Ok(Some((text, None)));
            }
            // Recover an interrupted install before AGENTS was written; preserve outside edits.
            ensure!(
                text == backup["original"].as_str().unwrap(),
                "Managed policy marker is missing and rules changed; restore manually"
            );
            return Ok(Some((
                if remove {
                    text
                } else {
                    format!("{}\n\n{POLICY}\n", backup["original"].as_str().unwrap())
                },
                if remove { None } else { Some(backup) },
            )));
        }
        if remove {
            return Ok(None);
        }
        ensure!(
            !text.contains("<!-- agy-delegate:start -->")
                && !text.contains("<!-- agy-delegate:end -->"),
            "Python delegation policy is still installed; migrate it before installing Rust policy"
        );
        let added = format!("{text}\n\n{POLICY}\n");
        let backup = json!({"original":text,"added":added,"existed":self.home.join("AGENTS.md").try_exists()?});
        ensure!(
            added.len() <= 1_048_576 && backup.to_string().len() <= 1_048_576,
            "Managed policy and backup must fit within 1 MB"
        );
        Ok(Some((added, Some(backup))))
    }

    /// CLI install runs doctor before calling this integration operation.
    pub async fn install(&mut self) -> Result<Value> {
        self.check_registration()?;
        self.policy_plan(false)?;
        self.register().await?;
        if let Some((text, backup)) = self.policy_plan(false)? {
            if let Some(backup) = backup
                && !self.backup.try_exists()?
            {
                write(&self.backup, backup.to_string().as_bytes(), true)?;
            }
            write(&self.home.join("AGENTS.md"), text.as_bytes(), false)?;
        }
        Ok(
            json!({"installed":true,"policy":self.home.join("AGENTS.md"),"config":self.snapshot,"note":"Start a new Codex session to load MCP tools and policy"}),
        )
    }
}

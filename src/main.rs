use agy_delegate_rs::{config::Config, jobs::Jobs};
use clap::{Args, CommandFactory, FromArgMatches, Parser, Subcommand};
use std::{
    io::{Read, Write},
    path::PathBuf,
};

#[derive(Args, Clone)]
#[group(required = true, multiple = false)]
struct TaskInput {
    #[arg(long)]
    task: Option<String>,
    /// UTF-8 task file; use - to read stdin.
    #[arg(long)]
    task_file: Option<PathBuf>,
}
impl TaskInput {
    fn read(self) -> anyhow::Result<String> {
        let text = match (self.task, self.task_file) {
            (Some(text), None) => text,
            (None, Some(path)) => {
                let reader: Box<dyn Read> = if path == std::path::Path::new("-") {
                    Box::new(std::io::stdin())
                } else {
                    Box::new(std::fs::File::open(path)?)
                };
                let mut bytes = Vec::new();
                reader.take(96001).read_to_end(&mut bytes)?;
                anyhow::ensure!(bytes.len() <= 96000, "Task file exceeds 96000 bytes");
                String::from_utf8(bytes)?
            }
            _ => anyhow::bail!("Provide exactly one task or task file"),
        };
        anyhow::ensure!(
            !text.trim().is_empty() && text.chars().count() <= 24000,
            "Task must contain 1–24000 characters"
        );
        Ok(text)
    }
}

#[derive(Parser)]
#[command(
    version,
    about = "Read-only agy investigations through the Codex sandbox"
)]
struct Cli {
    #[arg(long = "config", global = true)]
    config_file: Option<PathBuf>,
    #[command(flatten)]
    config: Config,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    Init,
    Register {
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Stop all tasks in the configured state and remove this Codex home's integration.
    Uninstall {
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    Doctor,
    Cleanup {
        #[arg(long)]
        dry_run: bool,
    },
    #[command(hide = true)]
    SandboxProbe {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        nonce: String,
    },
    Run {
        #[arg(long)]
        cwd: PathBuf,
        #[command(flatten)]
        input: TaskInput,
    },
    Submit {
        #[arg(long)]
        cwd: PathBuf,
        #[command(flatten)]
        input: TaskInput,
    },
    Status {
        job_id: String,
    },
    Wait {
        job_id: String,
        #[arg(long, default_value_t = 50)]
        seconds: u64,
    },
    Followup {
        job_id: String,
        #[arg(required_unless_present = "task_file", conflicts_with = "task_file")]
        message: Option<String>,
        #[arg(long)]
        task_file: Option<PathBuf>,
    },
    /// Print the complete stored report or export to a new file.
    Report {
        job_id: String,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Cancel {
        job_id: String,
    },
    List {
        #[arg(long, default_value_t = 10)]
        limit: u64,
    },
    Mcp,
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        job_id: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(unix)]
    unsafe {
        libc::umask(0o077);
    }
    let matches = Cli::command().get_matches();
    let mut cli = Cli::from_arg_matches(&matches)?;
    if let Action::SandboxProbe { file, nonce } = &cli.action {
        println!("{}", agy_delegate_rs::doctor::probe(file, nonce)?);
        return Ok(());
    }
    if matches!(cli.action, Action::Init) {
        let path = match cli.config_file {
            Some(path) => path,
            None => Config::file_path()?,
        };
        cli.config.initialize(&path)?;
        println!("{}", serde_json::json!({"config_file":path}));
        return Ok(());
    }
    if !matches!(cli.action, Action::Worker { .. }) {
        cli.config = match Config::load(cli.config_file.as_deref(), cli.config, &matches) {
            Ok(config) => config,
            Err(error) if matches!(cli.action, Action::Doctor) => {
                println!(
                    "{}",
                    serde_json::json!({"ok":false,"checks":[{"name":"configuration","ok":false,"detail":error.to_string()}]})
                );
                anyhow::bail!("Doctor checks failed");
            }
            Err(error) => return Err(error),
        };
    }
    let task = match &cli.action {
        Action::Run { input, .. } | Action::Submit { input, .. } => Some(input.clone().read()?),
        Action::Followup {
            message, task_file, ..
        } => Some(
            TaskInput {
                task: message.clone(),
                task_file: task_file.clone(),
            }
            .read()?,
        ),
        _ => None,
    };
    match cli.action {
        Action::Register { codex_home } => {
            let value =
                agy_delegate_rs::integration::Integration::new(cli.config, codex_home.as_deref())?
                    .register()
                    .await?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Action::Uninstall { codex_home } => {
            let value =
                agy_delegate_rs::integration::Integration::new(cli.config, codex_home.as_deref())?
                    .uninstall()
                    .await?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
        Action::Doctor => {
            let report = agy_delegate_rs::doctor::diagnose(&cli.config).await;
            println!("{}", serde_json::to_string_pretty(&report)?);
            anyhow::ensure!(report["ok"] == true, "Doctor checks failed");
        }
        Action::Run { cwd, .. } => {
            let task = task.unwrap();
            let result = Jobs::new(cli.config)?.run(&task, &cwd).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            anyhow::ensure!(
                result["status"] == "succeeded",
                "Investigation did not succeed"
            );
        }
        action => {
            let cleaning = matches!(action, Action::Cleanup { .. });
            let jobs = Jobs::new(cli.config)?;
            let value = match action {
                Action::Cleanup { dry_run } => jobs.cleanup(dry_run)?,
                Action::Submit { cwd, .. } => jobs.submit(task.as_deref().unwrap(), &cwd, None)?,
                Action::Status { job_id } => jobs.get(&job_id)?,
                Action::Wait { job_id, seconds } => jobs.wait(&job_id, seconds).await?,
                Action::Followup { job_id, .. } => {
                    jobs.followup(&job_id, task.as_deref().unwrap())?
                }
                Action::Report { job_id, output } => {
                    let report = jobs.report(&job_id)?;
                    if let Some(path) = output {
                        let parent = path
                            .parent()
                            .filter(|p| !p.as_os_str().is_empty())
                            .unwrap_or(std::path::Path::new("."));
                        let mut file = tempfile::NamedTempFile::new_in(parent)?;
                        file.write_all(report.as_bytes())?;
                        file.as_file().sync_all()?;
                        file.persist_noclobber(path)?;
                    } else {
                        std::io::stdout().write_all(report.as_bytes())?;
                    }
                    return Ok(());
                }
                Action::Cancel { job_id } => jobs.cancel(&job_id)?,
                Action::List { limit } => jobs.list(limit)?,
                Action::Worker { job_id } => {
                    jobs.worker(&job_id).await?;
                    return Ok(());
                }
                Action::Mcp => {
                    agy_delegate_rs::mcp::serve(jobs).await?;
                    return Ok(());
                }
                Action::Run { .. }
                | Action::Init
                | Action::Register { .. }
                | Action::Uninstall { .. }
                | Action::Doctor
                | Action::SandboxProbe { .. } => unreachable!(),
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
            if cleaning {
                anyhow::ensure!(
                    value["ok"] == true,
                    "Some task logs could not be cleaned; task records were retained"
                );
            }
        }
    }
    Ok(())
}

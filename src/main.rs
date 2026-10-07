use agy_delegate_rs::{
    config::{Config, private_dir},
    jobs::Jobs,
    runner,
};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Read-only agy investigations through the Codex sandbox"
)]
struct Cli {
    #[command(flatten)]
    config: Config,
    #[command(subcommand)]
    action: Action,
}
#[derive(Subcommand)]
enum Action {
    Run {
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        task: String,
    },
    Submit {
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        task: String,
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
        message: String,
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
    let cli = Cli::parse();
    match cli.action {
        Action::Run { cwd, task } => {
            let dir = private_dir(
                &cli.config
                    .state()?
                    .join(format!("run-{}", uuid::Uuid::new_v4())),
            )?;
            let result =
                runner::run(&cli.config, &cwd, &task, None, &dir, async |_| Ok(())).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            anyhow::ensure!(
                result["status"] == "succeeded",
                "Investigation did not succeed"
            );
        }
        action => {
            let jobs = Jobs::new(cli.config)?;
            let value = match action {
                Action::Submit { cwd, task } => jobs.submit(&task, &cwd, None)?,
                Action::Status { job_id } => jobs.get(&job_id)?,
                Action::Wait { job_id, seconds } => jobs.wait(&job_id, seconds).await?,
                Action::Followup { job_id, message } => jobs.followup(&job_id, &message)?,
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
                Action::Run { .. } => unreachable!(),
            };
            println!("{}", serde_json::to_string_pretty(&value)?);
        }
    }
    Ok(())
}

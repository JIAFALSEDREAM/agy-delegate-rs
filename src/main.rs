use agy_delegate_rs::{
    config::{Config, private_dir},
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
                    .join(format!("run-{}", std::process::id())),
            )?;
            let result = runner::run(&cli.config, &cwd, &task, None, &dir, |_| Ok(())).await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            anyhow::ensure!(
                result["status"] == "succeeded",
                "Investigation did not succeed"
            );
        }
    }
    Ok(())
}

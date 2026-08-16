use anyhow::Result;
use rgsp_host::daemon::PidFile;
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let pid_path = PathBuf::from("/tmp/rgsp/daemon.pid");
    let pidfile = match PidFile::acquire(&pid_path) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            std::process::exit(1);
        }
    };

    tracing::info!("rgsp-host starting");

    tokio::signal::ctrl_c().await?;
    tracing::info!("shutting down");
    pidfile.release();
    Ok(())
}

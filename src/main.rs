//! Entry point for the `SzBot` maintenance bot.

use std::{env, future};

use mwbot::{Bot, Result, init_logging};
#[cfg(unix)]
use tokio::signal::unix;
use tokio::{signal, sync::watch};
use tracing::info;
use wikipedia_sz_bot::{server, tasks};

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
    let bot = Bot::from_default_config()
        .await
        .map_err(|error| mwbot::Error::Unknown(error.to_string()))?;
    info!("Connected to {}", bot.server_name());

    let dry_run = env::args().any(|arg| arg == "--dry-run");
    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    let scheduler = tasks::run_forever(&bot, dry_run, shutdown_rx.clone());
    let server = server::run(bot.clone(), shutdown_rx.clone());

    tokio::select! {
        () = scheduler => {}
        result = server => result.map_err(mwbot::Error::IoError)?,
        () = shutdown_signal() => {
            info!("Shutdown signal received");
            let _ = shutdown_tx.send(true);
        }
    }

    Ok(())
}

/// Wait for SIGINT or SIGTERM.
async fn shutdown_signal() {
    let ctrl_c = signal::ctrl_c();

    #[cfg(unix)]
    let terminate = async {
        let mut signal =
            unix::signal(unix::SignalKind::terminate()).expect("failed to install SIGTERM handler");
        signal.recv().await;
        Ok::<(), ()>(())
    };

    #[cfg(not(unix))]
    let terminate = async {
        future::pending::<()>().await;
        Ok::<(), ()>(())
    };

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

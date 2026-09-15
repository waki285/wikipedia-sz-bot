//! Entry point for the `SzBot` maintenance bot.

#[cfg(not(unix))]
use std::future;
use std::{env, time::Duration};

use mwbot::{Bot, Result, init_logging};
#[cfg(unix)]
use tokio::signal::unix;
use tokio::{signal, sync::watch, time};
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

    let server_bot = bot.clone();
    let scheduler_shutdown = shutdown_rx.clone();
    let mut scheduler = tokio::spawn(async move {
        tasks::run_forever(&bot, dry_run, scheduler_shutdown).await;
    });
    let mut server = tokio::spawn(server::run(server_bot, shutdown_rx.clone()));

    // Track which handle the select! already drove to completion: polling a
    // finished JoinHandle again panics.
    let mut scheduler_done = false;
    let mut server_done = false;

    tokio::select! {
        _ = &mut scheduler => {
            scheduler_done = true;
            info!("All tasks finished");
            let _ = shutdown_tx.send(true);
        }
        result = &mut server => {
            server_done = true;
            let result = result.map_err(|error| mwbot::Error::Unknown(error.to_string()))?;
            result.map_err(mwbot::Error::IoError)?;
        }
        () = shutdown_signal() => {
            info!("Shutdown signal received");
            let _ = shutdown_tx.send(true);
        }
    }

    if !scheduler_done {
        drop(time::timeout(Duration::from_secs(30), scheduler).await);
    }
    if !server_done {
        drop(time::timeout(Duration::from_secs(30), server).await);
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

//! Entry point for the `SzBot` maintenance bot.

use std::env;

use mwbot::{Bot, Result, init_logging};
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
    let scheduler = tasks::run_forever(&bot, dry_run);
    let server = server::run(bot.clone());

    tokio::select! {
        () = scheduler => {}
        result = server => result.map_err(mwbot::Error::IoError)?,
    }

    Ok(())
}

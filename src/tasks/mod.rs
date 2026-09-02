//! Periodic maintenance tasks and their scheduler.

pub mod templatedata;

use std::time::Duration;

use mwbot::{Bot, Result};
use tokio::{
    sync::watch,
    time::{Instant, sleep_until},
};
use tracing::{error, info};

/// A periodic maintenance task.
#[derive(Debug, Clone, Copy)]
pub enum Task {
    /// Maintains the list of most-transcluded templates that lack
    /// `TemplateData`.
    Templatedata,
}

impl Task {
    /// Look up a task by its [`Task::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "templatedata" => Some(Self::Templatedata),
            _ => None,
        }
    }

    /// Human-readable name for logging.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Templatedata => "templatedata",
        }
    }

    /// Interval between runs.
    #[must_use]
    pub const fn interval(self) -> Duration {
        match self {
            Self::Templatedata => templatedata::INTERVAL,
        }
    }

    /// Run the task once.
    pub async fn run(self, bot: &Bot, dry_run: bool) -> Result<()> {
        match self {
            Self::Templatedata => templatedata::run(bot, dry_run).await,
        }
    }
}

/// Run all tasks forever, respecting each task's interval.
///
/// Tasks run immediately on startup and then every [`Task::interval`]. If
/// `dry_run` is true, each task runs once and the function returns. The loop
/// stops when `shutdown` is signalled.
pub async fn run_forever(bot: &Bot, dry_run: bool, mut shutdown: watch::Receiver<bool>) {
    let tasks = [Task::Templatedata];
    let mut next_runs: Vec<Instant> = tasks.iter().map(|_| Instant::now()).collect();

    loop {
        let now = Instant::now();
        for (task, next_run) in tasks.iter().zip(&mut next_runs) {
            if *next_run > now {
                continue;
            }
            match task.run(bot, dry_run).await {
                Ok(()) => {
                    let interval = task.interval();
                    info!(
                        "{}: completed, next run in {}h {:02}m",
                        task.name(),
                        interval.as_secs() / 3600,
                        (interval.as_secs() % 3600) / 60
                    );
                }
                Err(error) => error!("{}: {error}", task.name()),
            }
            *next_run = now + task.interval();
        }

        if dry_run {
            break;
        }

        let earliest = next_runs.iter().copied().min().unwrap_or(now);
        tokio::select! {
            () = sleep_until(earliest) => {}
            _ = shutdown.changed() => {
                info!("shutdown requested, stopping scheduler");
                break;
            }
        }
    }
}

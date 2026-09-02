//! Periodic maintenance tasks and their scheduler.

pub mod recentchanges;
pub mod templatedata;

use std::time::Duration;

use mwbot::{Bot, Result};
use tokio::{
    sync::watch,
    time::{self, Instant, sleep_until},
};
use tracing::info;

/// A periodic maintenance task.
#[derive(Debug, Clone, Copy)]
pub enum Task {
    /// Maintains the list of most-transcluded templates that lack
    /// `TemplateData`.
    Templatedata,
    /// Reports edits that add `utm_source` tracking parameters.
    Recentchanges,
}

impl Task {
    /// Look up a task by its [`Task::name`].
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "templatedata" => Some(Self::Templatedata),
            "recentchanges" => Some(Self::Recentchanges),
            _ => None,
        }
    }

    /// Human-readable name for logging.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Templatedata => "templatedata",
            Self::Recentchanges => "recentchanges",
        }
    }

    /// Interval between runs; `None` means the task runs continuously until
    /// shutdown.
    #[must_use]
    pub const fn interval(self) -> Option<Duration> {
        match self {
            Self::Templatedata => Some(templatedata::INTERVAL),
            Self::Recentchanges => None,
        }
    }

    /// Whether the task runs continuously instead of on a fixed interval.
    #[must_use]
    pub const fn is_resident(self) -> bool {
        self.interval().is_none()
    }

    /// Run the task once.
    pub async fn run(
        self,
        bot: &Bot,
        dry_run: bool,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        match self {
            Self::Templatedata => templatedata::run(bot, dry_run, shutdown).await,
            Self::Recentchanges => recentchanges::run(bot, dry_run, shutdown).await,
        }
    }
}

/// Run all tasks in parallel.
///
/// Batch tasks (with a finite interval) run immediately on startup and then
/// every [`Task::interval`]; resident tasks (with [`Task::interval`] ==
/// `None`) run continuously until `shutdown` is signalled. If `dry_run` is
/// true, each task runs once. The whole loop stops when `shutdown` is
/// signalled.
pub async fn run_forever(bot: &Bot, dry_run: bool, mut shutdown: watch::Receiver<bool>) {
    let tasks = [Task::Templatedata, Task::Recentchanges];
    let mut handles = Vec::with_capacity(tasks.len());

    for task in tasks {
        let bot = bot.clone();
        let mut task_shutdown = shutdown.clone();
        handles.push(tokio::spawn(async move {
            if let Some(interval) = task.interval() {
                if dry_run {
                    drop(task.run(&bot, true, task_shutdown).await);
                } else {
                    loop {
                        drop(task.run(&bot, false, task_shutdown.clone()).await);
                        tokio::select! {
                            () = sleep_until(Instant::now() + interval) => {}
                            _ = task_shutdown.changed() => {
                                info!("{}: shutdown requested, stopping", task.name());
                                break;
                            }
                        }
                    }
                }
            } else {
                drop(task.run(&bot, dry_run, task_shutdown).await);
            }
        }));
    }

    if !dry_run {
        drop(shutdown.changed().await);
    }
    // Wait for tasks to finish, but don't block shutdown indefinitely.
    for handle in handles {
        drop(time::timeout(Duration::from_secs(30), handle).await);
    }
}

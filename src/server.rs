//! HTTP server that triggers tasks on demand via POST requests.

use std::{env, io};

use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    serve,
};
use mwbot::Bot;
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::tasks::Task;

/// Environment variable for the listening port.
const PORT_ENV: &str = "SZ_BOT_PORT";
/// Default listening port when `SZ_BOT_PORT` is not set.
const DEFAULT_PORT: u16 = 8080;

/// Read the listening port from the `SZ_BOT_PORT` environment variable.
///
/// Falls back to [`DEFAULT_PORT`] when the variable is unset or invalid.
#[must_use]
pub fn port() -> u16 {
    parse_port(env::var(PORT_ENV).ok())
}

/// Parse a port number, falling back to [`DEFAULT_PORT`] when invalid.
#[must_use]
fn parse_port(value: Option<String>) -> u16 {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Start the HTTP server and run it until shutdown.
pub async fn run(bot: Bot) -> io::Result<()> {
    let app = Router::new()
        .route("/run/{task}", post(run_task))
        .with_state(bot);
    let listener = TcpListener::bind(("0.0.0.0", port())).await?;
    info!("Listening on {}", listener.local_addr()?);
    serve(listener, app).await
}

/// Run the task named by the path segment and report the outcome.
async fn run_task(State(bot): State<Bot>, Path(task_name): Path<String>) -> Response {
    match Task::from_name(&task_name) {
        Some(task) => {
            info!("Triggering task {task_name}");
            match task.run(&bot, false).await {
                Ok(()) => (StatusCode::OK, format!("{task_name}: done")).into_response(),
                Err(error) => {
                    error!("Task {task_name} failed: {error}");
                    (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("{task_name}: {error}"),
                    )
                        .into_response()
                }
            }
        }
        None => (StatusCode::NOT_FOUND, format!("unknown task: {task_name}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_port() {
        assert_eq!(parse_port(Some("9000".to_string())), 9000);
    }

    #[test]
    fn ignores_invalid_port() {
        assert_eq!(parse_port(Some("not-a-port".to_string())), DEFAULT_PORT);
    }

    #[test]
    fn falls_back_to_default_port() {
        assert_eq!(parse_port(None), DEFAULT_PORT);
    }
}

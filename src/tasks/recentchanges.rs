//! Maintenance task: edits that add `utm_source` tracking parameters.
//!
//! Scans recent changes, keeps edits by low-edit-count users whose diff adds
//! a URL with an `utm_source` parameter, and saves a table with diff links to
//! a maintenance page.

use std::{collections::HashSet, fmt::Write, time::Duration};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use mwbot::{Bot, Result, SaveOptions};
use tokio::{sync::watch, time::sleep};
use tracing::{error, info};

/// How often to save the accumulated report.
const SAVE_INTERVAL: Duration = Duration::from_hours(2);
/// How often to poll `recentchanges`.
const POLL_INTERVAL: Duration = Duration::from_secs(30);
/// Target page on the Japanese Wikipedia.
const PAGE_TITLE: &str = "利用者:SzBot/メンテナンス/検知した編集";
/// Edit summary used when saving.
const EDIT_SUMMARY: &str = "Bot: 検知した編集を更新";
/// Maximum number of edits a user may have to be considered a suspect.
const EDIT_COUNT_LIMIT: u64 = 100;
/// How many recent changes to fetch per poll.
const RC_PAGE_SIZE: u64 = 50;
/// How many edits to check per request when fetching diffs.
const BATCH_SIZE: usize = 50;

/// A recent edit with the metadata needed to check and report it.
#[derive(Debug, Clone)]
struct Edit {
    title: String,
    user: Option<String>,
    revid: u64,
    old_revid: Option<u64>,
    timestamp: DateTime<Utc>,
}

/// Run the monitor continuously, polling `recentchanges` every
/// [`POLL_INTERVAL`] and saving the accumulated report every
/// [`SAVE_INTERVAL`]. Stops promptly when `shutdown` is signalled.
///
/// If `dry_run` is true, performs a single poll and prints the report.
pub async fn run(bot: &Bot, dry_run: bool, mut shutdown: watch::Receiver<bool>) -> Result<()> {
    let mut since = Utc::now() - ChronoDuration::hours(2);
    let mut matching = Vec::new();
    // Ensure the report is saved on the first pass even when nothing matches.
    let save_interval = ChronoDuration::from_std(SAVE_INTERVAL).unwrap_or(ChronoDuration::hours(2));
    let mut last_save = Utc::now() - save_interval;

    loop {
        let edits = tokio::select! {
            result = recent_changes(bot, since) => {
                match result {
                    Ok(edits) => edits,
                    Err(error) => {
                        error!("recentchanges poll failed: {error}");
                        Vec::new()
                    }
                }
            }
            _ = shutdown.changed() => {
                info!("shutdown requested, stopping monitor");
                break;
            }
        };
        if let Some(last) = edits.last() {
            since = last.timestamp;
        }
        let suspect_edits = tokio::select! {
            result = filter_by_edit_count(bot, edits) => {
                match result {
                    Ok(edits) => edits,
                    Err(error) => {
                        error!("edit count filter failed: {error}");
                        Vec::new()
                    }
                }
            }
            _ = shutdown.changed() => {
                info!("shutdown requested, stopping monitor");
                break;
            }
        };
        let new_matching = tokio::select! {
            result = check_diffs(bot, suspect_edits) => {
                match result {
                    Ok(edits) => edits,
                    Err(error) => {
                        error!("diff check failed: {error}");
                        Vec::new()
                    }
                }
            }
            _ = shutdown.changed() => {
                info!("shutdown requested, stopping monitor");
                break;
            }
        };
        for edit in new_matching {
            if !matching.iter().any(|e: &Edit| e.revid == edit.revid) {
                matching.push(edit);
            }
        }

        if dry_run {
            info!("Dry run: {} matching edits", matching.len());
            println!("{}", build_report(&matching, Utc::now()));
            return Ok(());
        }

        let now = Utc::now();
        let elapsed = now.signed_duration_since(last_save);
        if elapsed >= save_interval
            || !matching.is_empty() && matching_since_save(&matching, last_save)
        {
            let wikitext = build_report(&matching, now);
            let page = bot.page(PAGE_TITLE)?;
            page.save(wikitext, &SaveOptions::summary(EDIT_SUMMARY))
                .await?;
            info!(
                "Saved {} matching edits to [[{}]]",
                matching.len(),
                PAGE_TITLE
            );
            last_save = now;
        }

        tokio::select! {
            () = sleep(POLL_INTERVAL) => {}
            _ = shutdown.changed() => {
                info!("shutdown requested, stopping monitor");
                break;
            }
        }
    }

    Ok(())
}

/// Whether any edit was found after `last_save`.
fn matching_since_save(edits: &[Edit], last_save: DateTime<Utc>) -> bool {
    edits.iter().any(|e| e.timestamp > last_save)
}

/// Fetch recent changes since `since`, newest first, at most [`RC_PAGE_SIZE`].
async fn recent_changes(bot: &Bot, since: DateTime<Utc>) -> Result<Vec<Edit>> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("list", "recentchanges".to_string()),
            ("rcprop", "ids|title|user|timestamp".to_string()),
            ("rclimit", RC_PAGE_SIZE.to_string()),
            ("rcdir", "newer".to_string()),
            ("rcstart", since.format("%Y-%m-%dT%H:%M:%SZ").to_string()),
        ])
        .await?;

    let mut edits = Vec::new();
    for item in resp["query"]["recentchanges"]
        .as_array()
        .map_or(&[][..], |items| items.as_slice())
    {
        let Some(revid) = item["revid"].as_u64() else {
            continue;
        };
        let Some(title) = item["title"].as_str() else {
            continue;
        };
        edits.push(Edit {
            title: title.to_string(),
            user: item["user"].as_str().map(str::to_string),
            revid,
            old_revid: item["old_revid"].as_u64(),
            timestamp: parse_timestamp(item["timestamp"].as_str()),
        });
    }

    Ok(edits)
}

/// Parse a `MediaWiki` timestamp into a UTC timestamp.
fn parse_timestamp(value: Option<&str>) -> DateTime<Utc> {
    value
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map_or_else(Utc::now, |dt| dt.with_timezone(&Utc))
}

/// Keep only edits by users with few total edits.
async fn filter_by_edit_count(bot: &Bot, edits: Vec<Edit>) -> Result<Vec<Edit>> {
    let users: Vec<&str> = edits
        .iter()
        .filter_map(|e| e.user.as_deref())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let mut low_count_users = HashSet::new();
    for chunk in users.chunks(BATCH_SIZE) {
        let resp = bot
            .api()
            .get_value(vec![
                ("action", "query".to_string()),
                ("list", "users".to_string()),
                ("ususers", chunk.join("|")),
                ("usprop", "editcount".to_string()),
            ])
            .await?;
        for user in resp["query"]["users"]
            .as_array()
            .map_or(&[][..], |u| u.as_slice())
        {
            let name = user["name"].as_str().unwrap_or_default();
            if user["editcount"].as_u64().unwrap_or(u64::MAX) <= EDIT_COUNT_LIMIT {
                low_count_users.insert(name.to_string());
            }
        }
    }

    Ok(edits
        .into_iter()
        .filter(|e| {
            e.user
                .as_deref()
                .is_some_and(|u| low_count_users.contains(u))
        })
        .collect())
}

/// Keep only edits whose diff adds a URL with an `utm_source` parameter.
///
/// For new page creations (`old_revid` is `0`) the initial revision is
/// checked directly, since there is no previous version to diff against.
async fn check_diffs(bot: &Bot, edits: Vec<Edit>) -> Result<Vec<Edit>> {
    let mut matching = Vec::new();
    for edit in edits {
        let has_utm = match edit.old_revid {
            Some(0) => match initial_revision_has_utm(bot, edit.revid).await {
                Ok(value) => value,
                Err(error) => {
                    error!("skipping new page {}: {error}", edit.title);
                    continue;
                }
            },
            Some(old_revid) => match diff_has_utm(bot, old_revid, edit.revid).await {
                Ok(value) => value,
                Err(error) => {
                    error!("skipping diff for {}: {error}", edit.title);
                    continue;
                }
            },
            None => continue,
        };
        if has_utm {
            matching.push(edit);
        }
    }
    Ok(matching)
}

/// Whether the given revision's wikitext contains `utm_source=chatgpt.com`.
async fn initial_revision_has_utm(bot: &Bot, revid: u64) -> Result<bool> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("prop", "revisions".to_string()),
            ("rvprop", "content".to_string()),
            ("rvslots", "main".to_string()),
            ("revids", revid.to_string()),
            ("formatversion", "2".to_string()),
        ])
        .await?;
    let content = resp["query"]["pages"][0]["revisions"][0]["slots"]["main"]["content"]
        .as_str()
        .unwrap_or_default();
    Ok(content
        .to_ascii_lowercase()
        .contains("utm_source=chatgpt.com"))
}

/// Whether the diff between the given revisions adds `utm_source=chatgpt.com`.
async fn diff_has_utm(bot: &Bot, old_revid: u64, revid: u64) -> Result<bool> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "compare".to_string()),
            ("fromrev", old_revid.to_string()),
            ("torev", revid.to_string()),
            ("formatversion", "2".to_string()),
        ])
        .await?;
    let body = resp["compare"]["body"].as_str().unwrap_or_default();
    Ok(added_line_has_utm(body))
}

/// Whether any added line in a diff body contains `utm_source=chatgpt.com`.
fn added_line_has_utm(body: &str) -> bool {
    body.split("class=\"diff-addedline").skip(1).any(|segment| {
        let line = segment.split("</div>").next().unwrap_or_default();
        line.to_ascii_lowercase().contains("utm_source=chatgpt.com")
    })
}

/// Build the wikitext report listing matching edits.
#[must_use]
fn build_report(edits: &[Edit], updated: DateTime<Utc>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "検知した編集です。");
    let _ = writeln!(out);
    let _ = writeln!(out, "最終更新: {} (UTC)", updated.format("%Y-%m-%d %H:%M"));
    let _ = writeln!(out);
    let _ = writeln!(out, "{{| class=\"wikitable sortable\"");
    let _ = writeln!(out, "|-");
    let _ = writeln!(out, "! ページ");
    let _ = writeln!(out, "! 利用者");
    let _ = writeln!(out, "! 差分");
    for edit in edits {
        let _ = writeln!(out, "|-");
        let _ = writeln!(out, "| [[:{}]]", edit.title);
        let _ = writeln!(out, "| {}", edit.user.as_deref().unwrap_or("(匿名)"));
        let _ = writeln!(
            out,
            "| {} [[Special:Diff/{}|差分]]",
            edit.timestamp.format("%H:%M"),
            edit.revid
        );
    }
    let _ = writeln!(out, "|}}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_utm_in_added_lines() {
        let body = r#"<td class="diff-addedline diff-side-added"><div>[https://example.com/?utm_source=chatgpt.com foo]</div></td>"#;
        assert!(added_line_has_utm(body));
    }

    #[test]
    fn ignores_utm_in_context_lines() {
        let body = r#"<td class="diff-context diff-side-added"><div>[https://example.com/?utm_source=chatgpt.com foo]</div></td>"#;
        assert!(!added_line_has_utm(body));
    }

    #[test]
    fn ignores_non_utm_additions() {
        let body = r#"<td class="diff-addedline diff-side-added"><div>plain text</div></td>"#;
        assert!(!added_line_has_utm(body));
    }

    #[test]
    fn ignores_utm_without_chatgpt() {
        let body = r#"<td class="diff-addedline diff-side-added"><div>[https://example.com/?utm_source=foo bar]</div></td>"#;
        assert!(!added_line_has_utm(body));
    }
}

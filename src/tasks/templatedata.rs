//! Maintenance task: most-transcluded templates that lack `TemplateData`.
//!
//! Collects the most-transcluded templates that have no `TemplateData` from
//! the `Mostlinkedtemplates` query page, and saves the top 50 to a
//! maintenance page.

use std::{env, fmt::Write, time::Duration};

use chrono::{DateTime, Utc};
use mwbot::{Bot, Error, Result, SaveOptions};
use tokio::sync::watch;
use tracing::{error, info};

use crate::api::{
    QueryPageItem, has_high_limits, main_namespace_transclusion_count, query_page,
    titles_with_templatedata, transclusion_limit,
};

/// Interval between updates (every other day).
pub const INTERVAL: Duration = Duration::from_hours(48);
/// Target page on the Japanese Wikipedia.
const PAGE_TITLE: &str =
    "利用者:SzBot/メンテナンス/多数使用されているTemplateDataがないテンプレート";
/// Edit summary used when saving.
const EDIT_SUMMARY: &str = "Bot: TemplateDataのないテンプレート一覧を更新";
/// How many templates to report.
const TARGET_COUNT: usize = 50;
/// How many most-transcluded templates to scan as candidates.
const CANDIDATE_COUNT: usize = 1000;
/// Environment variable to override [`CANDIDATE_COUNT`] for quick dry runs.
const CANDIDATE_COUNT_ENV: &str = "SZ_BOT_CANDIDATES";
/// Number of titles to check per `templatedata` request.
const TEMPLATEDATA_BATCH: usize = 50;

/// Run a single update: fetch, render, and save the maintenance page.
///
/// If `dry_run` is true, prints the generated wikitext instead of saving.
/// `shutdown` is accepted for signature consistency but ignored, as this
/// task runs to completion in one pass.
pub async fn run(bot: &Bot, dry_run: bool, _shutdown: watch::Receiver<bool>) -> Result<()> {
    let templates = most_transcluded_without_templatedata(bot).await?;
    if templates.is_empty() {
        return Err(Error::Unknown(
            "no templates without TemplateData found".to_string(),
        ));
    }
    let wikitext = build_report(&templates, Utc::now());
    if dry_run {
        info!("Dry run: {} templates", templates.len());
        println!("{wikitext}");
        return Ok(());
    }
    let page = bot.page(PAGE_TITLE)?;
    page.save(wikitext, &SaveOptions::summary(EDIT_SUMMARY))
        .await?;
    info!("Saved {} templates to [[{}]]", templates.len(), PAGE_TITLE);
    Ok(())
}

/// Collect the templates that lack `TemplateData`, ranked by main-namespace
/// usage.
///
/// The top [`CANDIDATE_COUNT`] templates by total transclusion count are
/// scanned, filtered by the `templatedata` API, and then sorted by direct
/// main-namespace usage descending. Returns up to [`TARGET_COUNT`] results.
async fn most_transcluded_without_templatedata(bot: &Bot) -> Result<Vec<QueryPageItem>> {
    let candidate_count = candidate_count();
    let mut candidates = Vec::with_capacity(candidate_count);
    let mut offset = 0u64;

    while candidates.len() < candidate_count {
        let (templates, next) = query_page(bot, "Mostlinkedtemplates", offset).await?;
        if templates.is_empty() {
            break;
        }
        let remaining = candidate_count - candidates.len();
        candidates.extend(templates.into_iter().take(remaining));
        match next {
            Some(next) => offset = next,
            None => break,
        }
    }

    let mut without_templatedata = Vec::new();
    for chunk in candidates.chunks(TEMPLATEDATA_BATCH) {
        let with_templatedata = titles_with_templatedata(bot, chunk).await?;
        for template in chunk {
            if !with_templatedata.contains(&template.title) {
                without_templatedata.push(template.clone());
            }
        }
    }
    info!(
        "Scanned {candidate_count} candidates, {} lack TemplateData",
        without_templatedata.len()
    );

    fill_main_namespace_counts(bot, &mut without_templatedata).await?;
    sort_by_main_namespace_count(&mut without_templatedata);
    without_templatedata.truncate(TARGET_COUNT);

    Ok(without_templatedata)
}

/// Number of most-transcluded templates to scan as candidates.
///
/// Overridable with the `SZ_BOT_CANDIDATES` environment variable for quick
/// dry runs; falls back to [`CANDIDATE_COUNT`].
#[must_use]
fn candidate_count() -> usize {
    candidate_count_from(env::var(CANDIDATE_COUNT_ENV).ok())
}

/// Parse a candidate count override, falling back to [`CANDIDATE_COUNT`].
#[must_use]
fn candidate_count_from(value: Option<String>) -> usize {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(CANDIDATE_COUNT)
}

/// Fill `main_namespace_transclusions` for each template.
///
/// The `transcludedin` limit per request is chosen based on the user's
/// `apihighlimits` right, so the API never clamps the requested `tilimit`
/// (which would emit an `outofrange` warning). With the right, up to
/// [`crate::api::HIGH_LIMIT`] pages are examined per template; without it,
/// only [`crate::api::LOW_LIMIT`]. Each template is queried individually and
/// sequentially to respect the Wikimedia API rate limits; the `mwapi` client
/// retries automatically on `429` using the `Retry-After` header.
async fn fill_main_namespace_counts(bot: &Bot, templates: &mut [QueryPageItem]) -> Result<()> {
    let per_request = transclusion_limit(has_high_limits(bot).await?);
    let total_count = templates.len();
    for (index, template) in templates.iter_mut().enumerate() {
        match main_namespace_transclusion_count(bot, &template.title, per_request).await {
            Ok((count, truncated)) => {
                template.main_namespace_transclusions = count;
                template.main_namespace_truncated = truncated;
            }
            Err(error) => {
                error!(
                    "{}: failed to count main-namespace usage: {error}",
                    template.title
                );
            }
        }
        if (index + 1).is_multiple_of(10) {
            info!(
                "Counted main-namespace usage for {}/{} templates",
                index + 1,
                total_count
            );
        }
    }
    Ok(())
}

/// Sort templates by main-namespace direct transclusion count descending,
/// tie-breaking by total transclusion count descending.
fn sort_by_main_namespace_count(templates: &mut [QueryPageItem]) {
    templates.sort_by(|a, b| {
        b.main_namespace_transclusions
            .cmp(&a.main_namespace_transclusions)
            .then(b.value.cmp(&a.value))
    });
}

/// Build the full wikitext of the maintenance report page.
///
/// Produces a sortable wikitable listing each template with its rank and
/// transclusion count, prefixed with an explanatory header and the update
/// timestamp.
#[must_use]
fn build_report(templates: &[QueryPageItem], updated: DateTime<Utc>) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "使用数が多いにもかかわらずTemplateDataが存在しないテンプレートの上位{}件です。",
        templates.len()
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "最終更新: {} (UTC)", updated.format("%Y-%m-%d %H:%M"));
    let _ = writeln!(out);
    let _ = writeln!(out, "{{| class=\"wikitable sortable\"");
    let _ = writeln!(out, "|-");
    let _ = writeln!(out, "! 順位");
    let _ = writeln!(out, "! テンプレート");
    let _ = writeln!(out, "! 使用数");
    let _ = writeln!(out, "! 標準名前空間での直接使用数");
    for (index, template) in templates.iter().enumerate() {
        let _ = writeln!(out, "|-");
        let _ = writeln!(out, "| {}", index + 1);
        let _ = writeln!(out, "| [[{}]]", template.title);
        let _ = writeln!(out, "| {}", format_count(template.value));
        let _ = writeln!(out, "| {}", format_main_namespace_count(template));
    }
    let _ = writeln!(out, "|}}");
    out
}

/// Format the main-namespace direct transclusion count, appending `+` when
/// the count was truncated at the API limit.
#[must_use]
fn format_main_namespace_count(template: &QueryPageItem) -> String {
    let mut text = format_count(template.main_namespace_transclusions);
    if template.main_namespace_truncated {
        text.push('+');
    }
    text
}

/// Format a number with thousands separators, e.g. `1013254` -> `1,013,254`.
#[must_use]
fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut result = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_template(title: &str, value: u64) -> QueryPageItem {
        QueryPageItem {
            title: title.to_string(),
            value,
            main_namespace_transclusions: 0,
            main_namespace_truncated: false,
        }
    }

    #[test]
    fn formats_counts() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1_013_254), "1,013,254");
    }

    #[test]
    fn builds_table() {
        let templates = vec![
            sample_template("Template:Reflist", 1_013_254),
            sample_template("Template:Infobox", 804_377),
        ];
        let updated: DateTime<Utc> = "2026-09-02T12:34:56Z".parse().unwrap();
        let wikitext = build_report(&templates, updated);

        assert!(wikitext.contains("上位2件"));
        assert!(wikitext.contains("最終更新: 2026-09-02 12:34 (UTC)"));
        assert!(wikitext.contains("| 1\n| [[Template:Reflist]]\n| 1,013,254\n| 0"));
        assert!(wikitext.contains("| 2\n| [[Template:Infobox]]\n| 804,377\n| 0"));
        assert!(wikitext.contains("標準名前空間での直接使用数"));
        assert!(wikitext.ends_with("|}\n"));
    }

    #[test]
    fn formats_capped_main_namespace_count() {
        let mut template = sample_template("Template:Infobox", 804_377);
        template.main_namespace_transclusions = 500;
        template.main_namespace_truncated = true;
        assert_eq!(format_main_namespace_count(&template), "500+");

        let exact = sample_template("Template:Reflist", 1_013_254);
        assert_eq!(format_main_namespace_count(&exact), "0");
    }

    #[test]
    fn sorts_by_main_namespace_count() {
        let mut a = sample_template("Template:A", 100);
        a.main_namespace_transclusions = 10;
        let mut b = sample_template("Template:B", 200);
        b.main_namespace_transclusions = 30;
        let mut c = sample_template("Template:C", 300);
        c.main_namespace_transclusions = 20;

        let mut templates = vec![a, c, b];
        sort_by_main_namespace_count(&mut templates);

        assert_eq!(templates[0].title, "Template:B");
        assert_eq!(templates[1].title, "Template:C");
        assert_eq!(templates[2].title, "Template:A");
    }

    #[test]
    fn breaks_ties_by_total_count() {
        let mut a = sample_template("Template:A", 100);
        a.main_namespace_transclusions = 50;
        let mut b = sample_template("Template:B", 200);
        b.main_namespace_transclusions = 50;

        let mut templates = vec![a, b];
        sort_by_main_namespace_count(&mut templates);

        assert_eq!(templates[0].title, "Template:B");
        assert_eq!(templates[1].title, "Template:A");
    }

    #[test]
    fn candidate_count_defaults_and_parses() {
        assert_eq!(candidate_count_from(None), CANDIDATE_COUNT);
        assert_eq!(candidate_count_from(Some("10".to_string())), 10);
        assert_eq!(
            candidate_count_from(Some("not-a-number".to_string())),
            CANDIDATE_COUNT
        );
    }
}

//! Maintenance task: most-transcluded templates that lack `TemplateData`.
//!
//! Collects the most-transcluded templates that have no `TemplateData` from
//! the `Mostlinkedtemplates` query page, and saves the top 50 to a
//! maintenance page.

use std::{fmt::Write, time::Duration};

use chrono::{DateTime, Utc};
use mwbot::{Bot, Error, Result, SaveOptions};
use tracing::info;

use crate::api::{QueryPageItem, query_page, titles_with_templatedata};

/// Interval between updates (every other day).
pub const INTERVAL: Duration = Duration::from_hours(48);
/// Target page on the Japanese Wikipedia.
const PAGE_TITLE: &str =
    "利用者:SzBot/メンテナンス/多数使用されているTemplateDataがないテンプレート";
/// Edit summary used when saving.
const EDIT_SUMMARY: &str = "Bot: TemplateDataのないテンプレート一覧を更新";
/// How many templates without `TemplateData` to collect.
const TARGET_COUNT: usize = 50;
/// Number of titles to check per `templatedata` request.
const TEMPLATEDATA_BATCH: usize = 50;

/// Run a single update: fetch, render, and save the maintenance page.
///
/// If `dry_run` is true, prints the generated wikitext instead of saving.
pub async fn run(bot: &Bot, dry_run: bool) -> Result<()> {
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
    info!("Saved {} templates", templates.len());
    Ok(())
}

/// Collect the most-transcluded templates that have no `TemplateData`.
///
/// Templates are taken from the `Mostlinkedtemplates` query page, which is
/// ordered by transclusion count descending, and filtered by checking the
/// `templatedata` API. Returns up to [`TARGET_COUNT`] results.
async fn most_transcluded_without_templatedata(bot: &Bot) -> Result<Vec<QueryPageItem>> {
    let mut found = Vec::with_capacity(TARGET_COUNT);
    let mut offset = 0u64;

    loop {
        let (templates, next) = query_page(bot, "Mostlinkedtemplates", offset).await?;

        for chunk in templates.chunks(TEMPLATEDATA_BATCH) {
            let with_templatedata = titles_with_templatedata(bot, chunk).await?;
            for template in chunk {
                if !with_templatedata.contains(&template.title) {
                    found.push(template.clone());
                    if found.len() >= TARGET_COUNT {
                        return Ok(found);
                    }
                }
            }
        }

        match next {
            Some(next) => offset = next,
            None => break,
        }
    }

    Ok(found)
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
        "被引用数が多いにもかかわらずTemplateDataが存在しないテンプレートの上位{}件です。",
        templates.len()
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "最終更新: {} (UTC)", updated.format("%Y-%m-%d %H:%M"));
    let _ = writeln!(out);
    let _ = writeln!(out, "{{| class=\"wikitable sortable\"");
    let _ = writeln!(out, "|-");
    let _ = writeln!(out, "! 順位");
    let _ = writeln!(out, "! テンプレート");
    let _ = writeln!(out, "! 被引用数");
    for (index, template) in templates.iter().enumerate() {
        let _ = writeln!(out, "|-");
        let _ = writeln!(out, "| {}", index + 1);
        let _ = writeln!(out, "| [[{}]]", template.title);
        let _ = writeln!(out, "| {}", format_count(template.value));
    }
    let _ = writeln!(out, "|}}");
    out
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
        assert!(wikitext.contains("| 1\n| [[Template:Reflist]]\n| 1,013,254"));
        assert!(wikitext.contains("| 2\n| [[Template:Infobox]]\n| 804,377"));
        assert!(wikitext.ends_with("|}\n"));
    }
}

//! `MediaWiki` API helpers shared by maintenance tasks.

use std::{collections::HashSet, time::Duration};

use mwbot::{Bot, Error, Result};
use tokio::time::sleep;
use tracing::error;

/// Namespace ID of the `Template` namespace.
const TEMPLATE_NAMESPACE: i64 = 10;
/// Namespace ID of the main (article) namespace.
pub const MAIN_NAMESPACE: i64 = 0;
/// How many results to request from a query page at once.
const PAGE_SIZE: u64 = 500;
/// How many pages may be requested at once when the response includes page
/// content, which the API caps lower than metadata-only requests.
pub const CONTENT_BATCH: usize = 50;
/// Maximum `transcludedin` results for authenticated users with the
/// `apihighlimits` right (bot flag). Also the overall cap on pages examined
/// for a single template, reached by paging when the right is absent.
pub const HIGH_LIMIT: u64 = 5000;
/// Maximum `transcludedin` results for anonymous and non-bot users.
const LOW_LIMIT: u64 = 500;

/// A single result from a query page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPageItem {
    pub title: String,
    /// Numeric value attached to the result, e.g. a transclusion count.
    pub value: u64,
    /// Number of direct transclusions in the main namespace.
    pub main_namespace_transclusions: u64,
    /// Whether the main-namespace count was truncated at the API limit.
    pub main_namespace_truncated: bool,
}

/// Fetch one page of results from a query page, ordered by value descending.
///
/// Returns the items and the offset for the next page, if any. Items outside
/// the `Template` namespace are skipped.
pub async fn query_page(
    bot: &Bot,
    page: &str,
    offset: u64,
) -> Result<(Vec<QueryPageItem>, Option<u64>)> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("list", "querypage".to_string()),
            ("qppage", page.to_string()),
            ("qplimit", PAGE_SIZE.to_string()),
            ("qpoffset", offset.to_string()),
        ])
        .await?;

    let results = resp["query"]["querypage"]["results"]
        .as_array()
        .ok_or_else(|| Error::Unknown("querypage results are missing".to_string()))?;

    let items = results
        .iter()
        .filter(|item| item["ns"].as_i64() == Some(TEMPLATE_NAMESPACE))
        .filter_map(|item| {
            let title = item["title"].as_str()?;
            let value = item["value"].as_str()?.parse::<u64>().ok()?;
            Some(QueryPageItem {
                title: title.to_string(),
                value,
                main_namespace_transclusions: 0,
                main_namespace_truncated: false,
            })
        })
        .collect();

    let next = resp["continue"]["qpoffset"].as_u64();
    Ok((items, next))
}

/// Return the subset of given template titles that have `TemplateData`.
///
/// The `templatedata` API only returns pages that have a valid `TemplateData`
/// block stored on them (via the `page_props` table), so any requested title
/// absent from the response has no usable `TemplateData`.
pub async fn titles_with_templatedata(
    bot: &Bot,
    templates: &[QueryPageItem],
) -> Result<HashSet<String>> {
    let titles: Vec<String> = templates.iter().map(|t| t.title.clone()).collect();
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "templatedata".to_string()),
            ("titles", titles.join("|")),
        ])
        .await?;

    let mut with_templatedata = HashSet::with_capacity(titles.len());
    if let Some(pages) = resp["pages"].as_object() {
        for page in pages.values() {
            if let Some(title) = page["title"].as_str() {
                with_templatedata.insert(title.to_string());
            }
        }
    }
    Ok(with_templatedata)
}

/// Whether the current user has the `apihighlimits` right (bot flag).
///
/// The `apihighlimits` right raises the maximum number of results that can be
/// requested with a single API call, including `transcludedin`.
pub async fn has_high_limits(bot: &Bot) -> Result<bool> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("meta", "userinfo".to_string()),
            ("uiprop", "rights".to_string()),
        ])
        .await?;
    let rights = resp["query"]["userinfo"]["rights"].as_array();
    Ok(rights.is_some_and(|rights| rights.iter().any(|r| r == "apihighlimits")))
}

/// Count direct transclusions of the given template in the main namespace.
///
/// `per_request` is the number of pages requested per API call (`tilimit`),
/// which is [`LOW_LIMIT`] without the `apihighlimits` right and [`HIGH_LIMIT`]
/// with it. Only a single page is examined; the returned flag indicates
/// whether more results exist beyond that cap (i.e. the count is truncated).
pub async fn main_namespace_transclusion_count(
    bot: &Bot,
    title: &str,
    per_request: u64,
) -> Result<(u64, bool)> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("prop", "transcludedin".to_string()),
            ("titles", title.to_string()),
            ("tinamespace", MAIN_NAMESPACE.to_string()),
            ("tilimit", per_request.to_string()),
        ])
        .await?;

    let items = resp["query"]["pages"][0]["transcludedin"]
        .as_array()
        .map_or(&[][..], |items| items.as_slice());
    let pageids: Vec<u64> = items
        .iter()
        .filter_map(|item| item["pageid"].as_u64())
        .collect();
    let truncated = resp["continue"]["ticontinue"].as_str().is_some();

    let title = strip_template_prefix(title);
    let count = count_direct_invocations(bot, &pageids, title).await?;
    Ok((count, truncated))
}

/// Return the number of pages whose wikitext invokes `{{template}}` or
/// `{{template|...}}` directly, fetching the wikitext in batches.
///
/// A batch that keeps failing is logged and skipped so one bad response
/// does not abort the whole scan.
async fn count_direct_invocations(bot: &Bot, pageids: &[u64], template: &str) -> Result<u64> {
    let mut count = 0u64;
    for chunk in pageids.chunks(CONTENT_BATCH) {
        let ids: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let resp = match page_contents(bot, &ids).await {
            Ok(resp) => resp,
            Err(error) => {
                error!("skipping batch: {error}");
                continue;
            }
        };
        for page in resp["query"]["pages"]
            .as_array()
            .map_or(&[][..], |p| p.as_slice())
        {
            let content = page["revisions"][0]["slots"]["main"]["content"]
                .as_str()
                .unwrap_or_default();
            if has_template_invocation(content, template) {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// Fetch the wikitext of a batch of pages, retrying transient errors with
/// backoff.
///
/// At most [`CONTENT_BATCH`] page ids may be passed, which is the API limit
/// for requests that include page content.
pub async fn page_contents(bot: &Bot, ids: &[String]) -> Result<serde_json::Value> {
    let mut delay = Duration::from_secs(1);
    let mut last_error: Option<Error> = None;
    for _ in 0..3 {
        let resp = bot
            .api()
            .get_value(vec![
                ("action", "query".to_string()),
                ("prop", "revisions".to_string()),
                ("rvprop", "content".to_string()),
                ("rvslots", "main".to_string()),
                ("pageids", ids.join("|")),
                ("formatversion", "2".to_string()),
            ])
            .await;
        match resp {
            Ok(value) => return Ok(value),
            Err(error) => {
                let error: Error = error.into();
                last_error = Some(error);
                sleep(delay).await;
                delay *= 2;
            }
        }
    }
    Err(last_error.unwrap_or_else(|| Error::Unknown("failed to fetch page wikitext".to_string())))
}

/// Whether `content` contains a direct invocation of `{{template}}` or
/// `{{template|...}}`. Template names are matched exactly (case-insensitive,
/// leading/trailing whitespace ignored) and subpage or parser-function calls
/// such as `{{template/foo}}` do not count.
fn has_template_invocation(content: &str, template: &str) -> bool {
    content.split("{{").skip(1).any(|rest| {
        rest.split(['|', '}', '<', '\n'])
            .next()
            .unwrap_or_default()
            .trim()
            .eq_ignore_ascii_case(template)
    })
}

/// Strip the `Template:` namespace prefix, if present.
fn strip_template_prefix(title: &str) -> &str {
    title.strip_prefix("Template:").unwrap_or(title)
}

/// The `transcludedin` result limit per request for the current user.
///
/// Returns [`HIGH_LIMIT`] when the user has the `apihighlimits` right,
/// otherwise [`LOW_LIMIT`]. This matches the API's per-request maximum so
/// that requesting `tilimit` never trips an `outofrange` warning.
#[must_use]
pub const fn transclusion_limit(high_limits: bool) -> u64 {
    if high_limits { HIGH_LIMIT } else { LOW_LIMIT }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_limit_by_high_limits() {
        assert_eq!(transclusion_limit(true), HIGH_LIMIT);
        assert_eq!(transclusion_limit(false), LOW_LIMIT);
    }

    #[test]
    fn strips_template_prefix() {
        assert_eq!(strip_template_prefix("Template:Infobox"), "Infobox");
        assert_eq!(
            strip_template_prefix("Template:Hlist/styles.css"),
            "Hlist/styles.css"
        );
        assert_eq!(strip_template_prefix("Infobox"), "Infobox");
    }

    #[test]
    fn detects_direct_invocations() {
        assert!(has_template_invocation("foo {{Infobox}} bar", "Infobox"));
        assert!(has_template_invocation(
            "foo {{Infobox|title=x}} bar",
            "Infobox"
        ));
        assert!(has_template_invocation(
            "foo {{ infobox |x}} bar",
            "Infobox"
        ));
        assert!(has_template_invocation("{{Infobox}}", "Infobox"));
    }

    #[test]
    fn ignores_non_invocations() {
        assert!(!has_template_invocation(
            "foo {{Infobox/row}} bar",
            "Infobox"
        ));
        assert!(!has_template_invocation(
            "foo {{Infobox-sub}} bar",
            "Infobox"
        ));
        assert!(!has_template_invocation("foo [[Infobox]] bar", "Infobox"));
        assert!(!has_template_invocation("foo {{#if:x|y}} bar", "Infobox"));
        assert!(!has_template_invocation("no template here", "Infobox"));
        assert!(!has_template_invocation("foo {{Info}} bar", "Infobox"));
    }
}

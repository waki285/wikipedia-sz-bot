//! `MediaWiki` API helpers shared by maintenance tasks.

use std::collections::HashSet;

use mwbot::{Bot, Error, Result};

/// Namespace ID of the `Template` namespace.
const TEMPLATE_NAMESPACE: i64 = 10;
/// Namespace ID of the main (article) namespace.
const MAIN_NAMESPACE: i64 = 0;
/// How many results to request from a query page at once.
const PAGE_SIZE: u64 = 500;
/// Maximum `transcludedin` results for authenticated users with the
/// `apihighlimits` right (bot flag).
const HIGH_LIMIT: u64 = 5000;
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
/// Requests are capped at `limit` results, which should be [`HIGH_LIMIT`] for
/// bots or [`LOW_LIMIT`] otherwise. The returned flag indicates whether more
/// results exist beyond the cap (i.e. the count is truncated).
pub async fn main_namespace_transclusion_count(
    bot: &Bot,
    title: &str,
    limit: u64,
) -> Result<(u64, bool)> {
    let resp = bot
        .api()
        .get_value(vec![
            ("action", "query".to_string()),
            ("prop", "transcludedin".to_string()),
            ("titles", title.to_string()),
            ("tinamespace", MAIN_NAMESPACE.to_string()),
            ("tilimit", limit.to_string()),
        ])
        .await?;

    let count = resp["query"]["pages"][0]["transcludedin"]
        .as_array()
        .map_or(0, |items| items.len() as u64);
    let truncated = resp["continue"]["ticontinue"].as_str().is_some();

    Ok((count, truncated))
}

/// The `transcludedin` result limit for the current user.
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
}

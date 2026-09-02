//! `MediaWiki` API helpers shared by maintenance tasks.

use std::collections::HashSet;

use mwbot::{Bot, Error, Result};

/// Namespace ID of the `Template` namespace.
const TEMPLATE_NAMESPACE: i64 = 10;
/// How many results to request from a query page at once.
const PAGE_SIZE: u64 = 500;

/// A single result from a query page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryPageItem {
    pub title: String,
    /// Numeric value attached to the result, e.g. a transclusion count.
    pub value: u64,
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

//! Page-state lookups on the Japanese Wikipedia.

use std::collections::{HashMap, HashSet};

use mwbot::{Bot, Result};

/// Redirect hops followed before giving up, which bounds the work done on a
/// redirect loop the API did not resolve.
const MAX_HOPS: usize = 4;

/// State of a Japanese article title referenced by an interlanguage link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageState {
    /// A regular article exists under that title.
    Exists,
    /// No page exists under that title.
    Missing,
    /// The title is a redirect, resolving to the given target. The target may
    /// itself be missing, i.e. a broken redirect.
    Redirect(String),
}

/// Look up the state of each title, keyed by the title as passed in.
///
/// `titles` must not exceed the API's limit for a single request; the caller
/// batches accordingly. Sent as a POST, since a batch of percent-encoded
/// titles can exceed the server's URI length limit.
pub async fn page_states(bot: &Bot, titles: &[String]) -> Result<HashMap<String, PageState>> {
    let resp = bot
        .api()
        .post_value(vec![
            ("action", "query".to_string()),
            ("prop", "info".to_string()),
            ("titles", titles.join("|")),
            ("redirects", "1".to_string()),
            ("formatversion", "2".to_string()),
        ])
        .await?;

    let normalized = pairs(&resp["query"]["normalized"]);
    let redirects = pairs(&resp["query"]["redirects"]);
    let missing: HashSet<&str> = resp["query"]["pages"]
        .as_array()
        .map_or(&[][..], |pages| pages.as_slice())
        .iter()
        .filter(|page| page["missing"].as_bool() == Some(true))
        .filter_map(|page| page["title"].as_str())
        .collect();

    let states = titles
        .iter()
        .map(|title| {
            let start = normalized
                .get(title.as_str())
                .copied()
                .unwrap_or(title.as_str());
            let state = resolve(start, &redirects, &missing);
            (title.clone(), state)
        })
        .collect();
    Ok(states)
}

/// Read a list of `{from, to}` objects into a lookup table.
fn pairs(value: &serde_json::Value) -> HashMap<&str, &str> {
    value
        .as_array()
        .map_or(&[][..], |items| items.as_slice())
        .iter()
        .filter_map(|item| Some((item["from"].as_str()?, item["to"].as_str()?)))
        .collect()
}

/// Classify a normalised title by following its redirect chain.
fn resolve(title: &str, redirects: &HashMap<&str, &str>, missing: &HashSet<&str>) -> PageState {
    let Some(&first) = redirects.get(title) else {
        return if missing.contains(title) {
            PageState::Missing
        } else {
            PageState::Exists
        };
    };

    let mut target = first;
    for _ in 0..MAX_HOPS {
        match redirects.get(target) {
            Some(&next) => target = next,
            None => break,
        }
    }
    PageState::Redirect(target.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table<'a>(entries: &[(&'a str, &'a str)]) -> HashMap<&'a str, &'a str> {
        entries.iter().copied().collect()
    }

    #[test]
    fn classifies_existing_title() {
        let state = resolve("Foo", &table(&[]), &HashSet::new());
        assert_eq!(state, PageState::Exists);
    }

    #[test]
    fn classifies_missing_title() {
        let missing = HashSet::from(["Foo"]);
        assert_eq!(resolve("Foo", &table(&[]), &missing), PageState::Missing);
    }

    #[test]
    fn follows_redirect_chain() {
        let redirects = table(&[("Foo", "Bar"), ("Bar", "Baz")]);
        assert_eq!(
            resolve("Foo", &redirects, &HashSet::new()),
            PageState::Redirect("Baz".to_string())
        );
    }

    #[test]
    fn reports_broken_redirect_as_redirect() {
        let redirects = table(&[("Foo", "Bar")]);
        let missing = HashSet::from(["Bar"]);
        assert_eq!(
            resolve("Foo", &redirects, &missing),
            PageState::Redirect("Bar".to_string())
        );
    }

    #[test]
    fn stops_on_redirect_loop() {
        let redirects = table(&[("Foo", "Bar"), ("Bar", "Foo")]);
        assert!(matches!(
            resolve("Foo", &redirects, &HashSet::new()),
            PageState::Redirect(_)
        ));
    }

    #[test]
    fn reads_pair_tables() {
        let value = serde_json::json!([{"from": "foo", "to": "Foo"}, {"from": "bar"}]);
        let table = pairs(&value);
        assert_eq!(table.len(), 1);
        assert_eq!(table.get("foo").copied(), Some("Foo"));
    }
}

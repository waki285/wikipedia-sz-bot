//! Wikidata lookups that map a foreign article to its Japanese counterpart.
//!
//! Titles are resolved with `wbgetentities`, which is asked for both the
//! source site and `jawiki` so that each returned item can be matched back to
//! the title it was requested for. Sitelinks point at real articles, so a
//! foreign title that is itself a redirect resolves to no item and is skipped.

use std::collections::{HashMap, HashSet};

use mwapi::Builder;
use mwbot::{ApiClient, Error, Result};
use serde_json::Value;
use tracing::warn;

/// Wikidata API endpoint.
const API_URL: &str = "https://www.wikidata.org/w/api.php";
/// How many titles or ids to request per call. The bot holds no `apihighlimits`
/// right on Wikidata even when authenticated, so the lower limit applies.
pub const BATCH: usize = 50;
/// Site identifier of the Japanese Wikipedia.
const JA_SITE: &str = "jawiki";
/// Language codes whose Wikipedia database name does not follow the usual
/// "replace hyphens with underscores and append `wiki`" rule.
const SITE_EXCEPTIONS: [(&str, &str); 9] = [
    ("be-tarask", "be_x_oldwiki"),
    ("gsw", "alswiki"),
    ("lzh", "zh_classicalwiki"),
    ("nan", "zh_min_nanwiki"),
    ("nb", "nowiki"),
    ("rup", "roa_rupwiki"),
    ("sgs", "bat_smgwiki"),
    ("vro", "fiu_vrowiki"),
    ("yue", "zh_yuewiki"),
];
/// User agent for the Wikidata client, which cannot inherit the one configured
/// for the Japanese Wikipedia bot.
const USER_AGENT: &str = concat!(
    env!("CARGO_PKG_NAME"),
    "/",
    env!("CARGO_PKG_VERSION"),
    " (https://ja.wikipedia.org/wiki/User:SzBot)"
);
/// Pseudo language code naming a Wikidata item directly instead of an article.
pub const WIKIDATA_LANG: &str = "wikidata";

/// Read-only Wikidata client.
#[derive(Debug)]
pub struct Wikidata {
    client: ApiClient,
    /// Site identifiers `wbgetentities` accepts, so that an unknown language
    /// code is skipped instead of failing a whole batch.
    sites: HashSet<String>,
}

impl Wikidata {
    /// Connect to Wikidata and learn which site identifiers it accepts.
    ///
    /// The bot's credentials are reused, since an owner-only `OAuth2` consumer
    /// registered on Meta is valid across Wikimedia wikis and authenticated
    /// requests are rate-limited far less aggressively. If those credentials
    /// are not accepted here, the scan falls back to anonymous access.
    pub async fn connect(bot_api: &ApiClient) -> Result<Self> {
        if let Some(wikidata) = Self::connect_as_bot(bot_api).await {
            return Ok(wikidata);
        }
        warn!("Wikidata: falling back to anonymous access");
        let client = ApiClient::builder(API_URL)
            .set_user_agent(USER_AGENT)
            .build()
            .await?;
        let sites = known_sites(&client).await?;
        Ok(Self { client, sites })
    }

    /// Connect reusing the bot's credentials, or `None` when they do not work.
    async fn connect_as_bot(bot_api: &ApiClient) -> Option<Self> {
        let client = Builder::from_client_with_url(bot_api, API_URL)
            .set_user_agent(USER_AGENT)
            .build()
            .await
            .inspect_err(|error| warn!("Wikidata: cannot build an authenticated client: {error}"))
            .ok()?;
        let sites = known_sites(&client)
            .await
            .inspect_err(|error| warn!("Wikidata: authenticated request rejected: {error}"))
            .ok()?;
        Some(Self { client, sites })
    }

    /// Japanese counterparts of the given Wikidata items, keyed by item id.
    pub async fn ja_titles_for_ids(&self, ids: &[String]) -> Result<HashMap<String, String>> {
        let resp = self
            .client
            .get_value(vec![
                ("action", "wbgetentities".to_string()),
                ("ids", ids.join("|")),
                ("props", "sitelinks".to_string()),
                ("sitefilter", JA_SITE.to_string()),
                ("formatversion", "2".to_string()),
            ])
            .await?;

        let mut found = HashMap::new();
        if let Some(entities) = resp["entities"].as_object() {
            for (id, entity) in entities {
                if let Some(title) = entity["sitelinks"][JA_SITE]["title"].as_str() {
                    found.insert(id.to_ascii_uppercase(), title.to_string());
                }
            }
        }
        Ok(found)
    }

    /// Japanese counterparts of the given titles on `site`, keyed by the
    /// normalised source title.
    ///
    /// Titles without a Wikidata item, or whose item has no Japanese sitelink,
    /// are absent from the result.
    pub async fn ja_titles_for_site(
        &self,
        site: &str,
        titles: &[String],
    ) -> Result<HashMap<String, String>> {
        let resp = self
            .client
            .get_value(vec![
                ("action", "wbgetentities".to_string()),
                ("sites", site.to_string()),
                ("titles", titles.join("|")),
                ("props", "sitelinks".to_string()),
                ("sitefilter", format!("{site}|{JA_SITE}")),
                ("formatversion", "2".to_string()),
            ])
            .await?;
        Ok(collect_sitelinks(&resp, site))
    }

    /// Database name of the Wikipedia for `lang`, or `None` when Wikidata does
    /// not know such a site.
    #[must_use]
    pub fn site_for(&self, lang: &str) -> Option<String> {
        let site = site_name(lang);
        self.sites.contains(&site).then_some(site)
    }
}

/// Pair each returned item's source title with its Japanese sitelink.
fn collect_sitelinks(resp: &Value, site: &str) -> HashMap<String, String> {
    let mut found = HashMap::new();
    let Some(entities) = resp["entities"].as_object() else {
        return found;
    };

    for entity in entities.values() {
        let sitelinks = &entity["sitelinks"];
        let (Some(source), Some(ja)) = (
            sitelinks[site]["title"].as_str(),
            sitelinks[JA_SITE]["title"].as_str(),
        ) else {
            continue;
        };
        found.insert(normalize_title(source), ja.to_string());
    }

    found
}

/// Site identifiers accepted by the `sites` parameter of `wbgetentities`.
async fn known_sites(client: &ApiClient) -> Result<HashSet<String>> {
    let resp = client
        .get_value(vec![
            ("action", "paraminfo".to_string()),
            ("modules", "wbgetentities".to_string()),
            ("formatversion", "2".to_string()),
        ])
        .await?;

    let sites = resp["paraminfo"]["modules"][0]["parameters"]
        .as_array()
        .map_or(&[][..], |params| params.as_slice())
        .iter()
        .find(|param| param["name"] == "sites")
        .and_then(|param| param["type"].as_array())
        .ok_or_else(|| Error::Unknown("wbgetentities site list is missing".to_string()))?
        .iter()
        .filter_map(|site| site.as_str().map(str::to_string))
        .collect();
    Ok(sites)
}

/// Database name of the Wikipedia for a language code.
///
/// The result is not guaranteed to exist; [`Wikidata::site_for`] checks it
/// against the site list reported by the API.
fn site_name(lang: &str) -> String {
    let lang = lang.trim().to_ascii_lowercase();
    SITE_EXCEPTIONS
        .iter()
        .find(|(code, _)| *code == lang)
        .map_or_else(
            || format!("{}wiki", lang.replace('-', "_")),
            |(_, site)| (*site).to_string(),
        )
}

/// Normalise a page title the way `MediaWiki` does, so that a title written in
/// an article matches the one Wikidata reports.
#[must_use]
pub fn normalize_title(title: &str) -> String {
    let mut normalized = title.trim().replace('_', " ").trim().to_string();
    if let Some(first) = normalized.get(..1) {
        let upper = first.to_ascii_uppercase();
        normalized.replace_range(..1, &upper);
    }
    normalized
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn normalizes_titles() {
        assert_eq!(normalize_title(" foo_bar "), "Foo bar");
        assert_eq!(normalize_title("Foo bar"), "Foo bar");
        assert_eq!(normalize_title("アンパサンド"), "アンパサンド");
    }

    #[test]
    fn collects_sitelink_pairs() {
        let resp = json!({
            "entities": {
                "Q51500": {
                    "sitelinks": {
                        "enwiki": {"site": "enwiki", "title": "Ampère's circuital law"},
                        "jawiki": {"site": "jawiki", "title": "アンペールの法則"}
                    }
                },
                "Q1": {
                    "sitelinks": {
                        "enwiki": {"site": "enwiki", "title": "Universe"}
                    }
                },
                "-1": {"missing": "", "title": "Nonexistent"}
            }
        });

        let found = collect_sitelinks(&resp, "enwiki");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found.get("Ampère's circuital law").map(String::as_str),
            Some("アンペールの法則")
        );
    }

    #[test]
    fn maps_language_codes_to_sites() {
        assert_eq!(site_name("en"), "enwiki");
        assert_eq!(site_name("EN"), "enwiki");
        assert_eq!(site_name("zh-min-nan"), "zh_min_nanwiki");
        assert_eq!(site_name("be-tarask"), "be_x_oldwiki");
        assert_eq!(site_name("nb"), "nowiki");
        assert_eq!(site_name("gsw"), "alswiki");
    }
}

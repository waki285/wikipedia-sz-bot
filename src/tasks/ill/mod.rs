//! Maintenance task: interlanguage links whose target already has a Japanese
//! article.
//!
//! Scans every main-namespace article that transcludes `Template:仮リンク`
//! (redirects to it included, since `MediaWiki` records both the redirect and
//! its target in `templatelinks`) and reports each call whose Japanese article
//! name is missing or a redirect, while the Wikidata item of the linked
//! foreign article does carry a Japanese sitelink. Such a call can be replaced
//! with a plain wikilink to that article.

mod jawiki;
mod parse;
mod wikidata;

use std::{
    collections::{HashMap, HashSet},
    env,
    fmt::Write,
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use mwbot::{Bot, Result, SaveOptions};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use self::{
    jawiki::{PageState, page_states},
    parse::{IllCall, LangTarget, find_calls},
    wikidata::{BATCH as WIKIDATA_BATCH, WIKIDATA_LANG, Wikidata, normalize_title},
};
use crate::{
    api::{CONTENT_BATCH, MAIN_NAMESPACE, has_high_limits, page_contents},
    format,
};

/// Interval between updates (weekly). A full scan is expensive, so it is run
/// no more often than this.
pub const INTERVAL: Duration = Duration::from_hours(168);
/// Edit summary used when saving.
const EDIT_SUMMARY: &str = "Bot: 日本語版記事が存在する仮リンク一覧を更新";
/// How many titles to look up per `prop=info` request with the
/// `apihighlimits` right.
const HIGH_INFO_BATCH: usize = 500;
/// The template whose transclusions are scanned.
const ILL_TEMPLATE: &str = "Template:仮リンク";
/// How many titles to look up per `prop=info` request without the
/// `apihighlimits` right.
const LOW_INFO_BATCH: usize = 50;
/// Environment variable capping how many articles are scanned, for dry runs.
const PAGE_LIMIT_ENV: &str = "SZ_BOT_ILL_PAGES";
/// Target page on the Japanese Wikipedia.
const PAGE_TITLE: &str = "利用者:SzBot/メンテナンス/日本語版記事が存在する仮リンク";
/// How often to log scan progress. Time-based rather than article-based, so
/// the log keeps a steady pace even when the API slows the scan down.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(300);
/// How many findings to list on the report page.
const REPORT_LIMIT: usize = 1000;
/// Longest template call reproduced verbatim in the report.
const RAW_LIMIT: usize = 200;
/// How many articles to resolve in one round. Larger rounds fill the lookup
/// batches better; smaller ones keep memory down and progress steady.
const SCAN_CHUNK: usize = 500;
/// Average seconds per API request above which the scan is assumed to be
/// rate-limited: a normal round trip takes well under a second, so anything
/// this slow is time spent waiting out a `429`.
const SLOW_REQUEST_SECS: f32 = 3.0;

/// Lookup results kept for the whole scan, since the same titles recur across
/// articles.
#[derive(Debug, Default)]
struct Caches {
    /// State of each Japanese article name seen so far.
    ja: HashMap<String, PageState>,
    /// Language codes Wikidata does not know, already warned about.
    unknown_langs: HashSet<String>,
    /// Japanese counterpart of each `(site, normalised title)` pair, or `None`
    /// when the pair has no Japanese article.
    sitelinks: HashMap<(String, String), Option<String>>,
}

/// An interlanguage link that could be replaced with a plain wikilink.
#[derive(Debug, Clone)]
struct Finding {
    /// Article containing the call.
    article: String,
    /// Japanese article name given in the call.
    ja_title: String,
    /// Japanese article linked from the target's Wikidata item.
    linked: String,
    /// The call as written in the article.
    raw: String,
    /// State of `ja_title` on the Japanese Wikipedia.
    state: PageState,
    /// The language target that resolved to a Japanese article.
    target: LangTarget,
}

/// How much work one round's lookups took, used to notice rate limiting.
#[derive(Debug, Clone, Copy)]
struct RoundStats {
    /// Time spent looking up Japanese article names.
    ja_elapsed: Duration,
    /// Requests made looking up Japanese article names.
    ja_requests: usize,
    /// Time spent looking up Wikidata sitelinks.
    sitelink_elapsed: Duration,
    /// Requests made looking up Wikidata sitelinks.
    sitelink_requests: usize,
}

impl RoundStats {
    /// A warning when requests took far longer than a round trip should, which
    /// in practice means the API is making the bot wait out a rate limit.
    ///
    /// Reports whichever API is slower, so one warning names the real culprit.
    fn throttle_warning(self) -> Option<String> {
        let (api, per_request) = [
            ("jawiki", self.ja_requests, self.ja_elapsed),
            ("Wikidata", self.sitelink_requests, self.sitelink_elapsed),
        ]
        .into_iter()
        .filter(|&(_, requests, _)| requests > 0)
        .map(|(api, requests, elapsed)| (api, elapsed.as_secs_f32() / requests as f32))
        .filter(|&(_, per_request)| per_request >= SLOW_REQUEST_SECS)
        .max_by(|(_, a), (_, b)| a.total_cmp(b))?;
        Some(format!(
            "{api}: {per_request:.1}s per request, far above a normal round trip. \
             The API is rate-limiting the bot, so this scan will take much longer than usual."
        ))
    }
}

/// Run a single scan: collect, resolve, render and save the report.
///
/// If `dry_run` is true, prints the generated wikitext instead of saving. The
/// scan is long-running, so `shutdown` is checked between rounds and stops it
/// without saving.
pub async fn run(bot: &Bot, dry_run: bool, shutdown: watch::Receiver<bool>) -> Result<()> {
    let wikidata = Wikidata::connect(bot.api()).await?;
    let info_batch = if has_high_limits(bot).await? {
        HIGH_INFO_BATCH
    } else {
        LOW_INFO_BATCH
    };

    let articles = collect_articles(bot, &shutdown).await?;
    info!(
        "Scanning {} articles for interlanguage links",
        articles.len()
    );

    let mut caches = Caches::default();
    let mut findings = Vec::new();
    let mut total = 0usize;
    let mut scanned = 0usize;
    let started = Instant::now();
    let mut last_progress = Instant::now();
    let mut last_warning: Option<Instant> = None;

    for chunk in articles.chunks(SCAN_CHUNK) {
        if is_shutdown(&shutdown) {
            info!("ill: shutdown requested, stopping without saving");
            return Ok(());
        }
        let calls = fetch_calls(bot, chunk).await;
        let (found, stats) = evaluate(bot, &wikidata, &mut caches, calls, info_batch).await;
        total += found.len();
        findings.extend(
            found
                .into_iter()
                .take(REPORT_LIMIT - findings.len().min(REPORT_LIMIT)),
        );
        scanned += chunk.len();

        if last_progress.elapsed() >= PROGRESS_INTERVAL {
            info!(
                "{}",
                progress(scanned, articles.len(), total, started.elapsed())
            );
            last_progress = Instant::now();
        }
        // Throttling persists for a while, so repeat the warning no more often
        // than the progress line.
        if let Some(warning) = stats.throttle_warning()
            && last_warning.is_none_or(|at| at.elapsed() >= PROGRESS_INTERVAL)
        {
            warn!("{warning}");
            last_warning = Some(Instant::now());
        }
    }

    let wikitext = build_report(&findings, total, articles.len(), Utc::now());
    if dry_run {
        info!("Dry run: {total} findings in {} articles", articles.len());
        println!("{wikitext}");
        return Ok(());
    }
    let page = bot.page(PAGE_TITLE)?;
    page.save(wikitext, &SaveOptions::summary(EDIT_SUMMARY))
        .await?;
    info!("Saved {total} findings to [[{PAGE_TITLE}]]");
    Ok(())
}

/// Render the report page.
///
/// The table lists at most [`REPORT_LIMIT`] findings; `total` reports how many
/// were actually found so a truncated list is still honest about its scope.
#[must_use]
fn build_report(
    findings: &[Finding],
    total: usize,
    scanned: usize,
    updated: DateTime<Utc>,
) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "仮リンクのリンク先記事のウィキデータ項目に、日本語版記事がリンクされているものの一覧です。第1引数の日本語版記事名が存在しないか、リダイレクトになっているものを対象としています。"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "最終更新: {} (UTC)", updated.format("%Y-%m-%d %H:%M"));
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "走査した記事: {}件 / 検出: {}件{}",
        format::count(scanned as u64),
        format::count(total as u64),
        if total > findings.len() {
            format!("（うち{}件を表示）", format::count(findings.len() as u64))
        } else {
            String::new()
        }
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "{{| class=\"wikitable sortable\"");
    let _ = writeln!(out, "|-");
    let _ = writeln!(out, "! 記事");
    let _ = writeln!(out, "! 仮リンクの記述");
    let _ = writeln!(out, "! 第1引数の状態");
    let _ = writeln!(out, "! リンク先");
    let _ = writeln!(out, "! 日本語版記事");
    for finding in findings {
        let _ = writeln!(out, "|-");
        let _ = writeln!(out, "| [[{}]]", finding.article);
        let _ = writeln!(
            out,
            "| <code><nowiki>{}</nowiki></code>",
            sanitize_raw(&finding.raw)
        );
        let _ = writeln!(out, "| {}", describe_state(finding));
        let _ = writeln!(
            out,
            "| [[:{}:{}|{}:{}]]",
            finding.target.lang, finding.target.title, finding.target.lang, finding.target.title
        );
        let _ = writeln!(out, "| [[{}]]", finding.linked);
    }
    let _ = writeln!(out, "|}}");
    out
}

/// List the main-namespace articles that transclude [`ILL_TEMPLATE`].
///
/// Redirect pages are excluded, and the list is capped by [`page_limit`].
async fn collect_articles(
    bot: &Bot,
    shutdown: &watch::Receiver<bool>,
) -> Result<Vec<(u64, String)>> {
    let limit = page_limit();
    let mut articles = Vec::new();
    let mut resume: Option<String> = None;

    loop {
        if is_shutdown(shutdown) {
            break;
        }
        let mut params = vec![
            ("action", "query".to_string()),
            ("list", "embeddedin".to_string()),
            ("eititle", ILL_TEMPLATE.to_string()),
            ("einamespace", MAIN_NAMESPACE.to_string()),
            ("eifilterredir", "nonredirects".to_string()),
            ("eilimit", "max".to_string()),
            ("formatversion", "2".to_string()),
        ];
        if let Some(value) = resume {
            params.push(("eicontinue", value));
        }
        let resp = bot.api().get_value(params).await?;

        for item in resp["query"]["embeddedin"]
            .as_array()
            .map_or(&[][..], |items| items.as_slice())
        {
            let (Some(pageid), Some(title)) = (item["pageid"].as_u64(), item["title"].as_str())
            else {
                continue;
            };
            articles.push((pageid, title.to_string()));
            if articles.len() >= limit {
                return Ok(articles);
            }
        }

        match resp["continue"]["eicontinue"].as_str() {
            Some(value) => resume = Some(value.to_string()),
            None => break,
        }
    }

    Ok(articles)
}

/// Describe why the Japanese article name of a finding is unresolved.
fn describe_state(finding: &Finding) -> String {
    match &finding.state {
        PageState::Redirect(target) => {
            format!("[[{}]]は[[{target}]]へのリダイレクト", finding.ja_title)
        }
        PageState::Missing | PageState::Exists => format!("[[{}]]は未作成", finding.ja_title),
    }
}

/// Resolve a round of calls into findings.
///
/// Japanese article names are looked up first so that calls already pointing
/// at an article are dropped before the more expensive Wikidata lookups.
async fn evaluate(
    bot: &Bot,
    wikidata: &Wikidata,
    caches: &mut Caches,
    calls: Vec<(String, IllCall)>,
    info_batch: usize,
) -> (Vec<Finding>, RoundStats) {
    let total_calls = calls.len();
    let titles: Vec<String> = unique(calls.iter().map(|(_, call)| call.ja_title.clone()))
        .into_iter()
        .filter(|title| !caches.ja.contains_key(title))
        .collect();
    let ja_lookups = titles.len();
    let ja_started = Instant::now();
    let ja_requests = fill_ja_states(bot, caches, &titles, info_batch).await;
    let ja_elapsed = ja_started.elapsed();

    let pending: Vec<(String, IllCall, PageState)> = calls
        .into_iter()
        .filter_map(|(article, call)| {
            let state = caches.ja.get(&call.ja_title)?.clone();
            matches!(state, PageState::Missing | PageState::Redirect(_))
                .then_some((article, call, state))
        })
        .collect();

    let sitelink_started = Instant::now();
    let (sitelink_lookups, sitelink_requests) = fill_sitelinks(wikidata, caches, &pending).await;
    let sitelink_elapsed = sitelink_started.elapsed();
    debug!(
        "round: {total_calls} calls, {ja_lookups} titles in {ja_requests} requests ({:.1}s), \
         {sitelink_lookups} sitelinks in {sitelink_requests} requests ({:.1}s)",
        ja_elapsed.as_secs_f32(),
        sitelink_elapsed.as_secs_f32()
    );

    let findings = pending
        .into_iter()
        .filter_map(|(article, call, state)| {
            let (target, linked) = match_target(wikidata, caches, &call, &state)?;
            Some(Finding {
                article,
                ja_title: call.ja_title,
                linked,
                raw: call.raw,
                state,
                target,
            })
        })
        .collect();
    let stats = RoundStats {
        ja_elapsed,
        ja_requests,
        sitelink_elapsed,
        sitelink_requests,
    };
    (findings, stats)
}

/// Fetch the interlanguage-link calls of a batch of articles.
///
/// A batch that cannot be fetched is logged and skipped, so one bad response
/// does not abort the scan.
async fn fetch_calls(bot: &Bot, articles: &[(u64, String)]) -> Vec<(String, IllCall)> {
    let mut calls = Vec::new();

    for chunk in articles.chunks(CONTENT_BATCH) {
        let ids: Vec<String> = chunk.iter().map(|(pageid, _)| pageid.to_string()).collect();
        let resp = match page_contents(bot, &ids).await {
            Ok(resp) => resp,
            Err(error) => {
                error!("skipping batch of {} articles: {error}", ids.len());
                continue;
            }
        };
        for page in resp["query"]["pages"]
            .as_array()
            .map_or(&[][..], |pages| pages.as_slice())
        {
            let Some(title) = page["title"].as_str() else {
                continue;
            };
            let content = page["revisions"][0]["slots"]["main"]["content"]
                .as_str()
                .unwrap_or_default();
            calls.extend(
                find_calls(content)
                    .into_iter()
                    .map(|call| (title.to_string(), call)),
            );
        }
    }

    calls
}

/// Look up and cache the state of each Japanese article name, returning how
/// many requests that took.
async fn fill_ja_states(bot: &Bot, caches: &mut Caches, titles: &[String], batch: usize) -> usize {
    let mut requests = 0;
    for chunk in titles.chunks(batch) {
        requests += 1;
        match page_states(bot, chunk).await {
            Ok(states) => caches.ja.extend(states),
            Err(error) => error!("skipping {} titles: {error}", chunk.len()),
        }
    }
    requests
}

/// Look up and cache the Japanese counterpart of every language target in
/// `pending`, grouping the lookups by site.
///
/// Returns how many titles were looked up and how many requests that took;
/// the two differ because each site is batched separately.
async fn fill_sitelinks(
    wikidata: &Wikidata,
    caches: &mut Caches,
    pending: &[(String, IllCall, PageState)],
) -> (usize, usize) {
    let mut wanted: HashMap<String, Vec<String>> = HashMap::new();
    for (_, call, _) in pending {
        for target in &call.targets {
            let Some(site) = site_for(wikidata, caches, &target.lang) else {
                continue;
            };
            let title = normalize_title(&target.title);
            if caches
                .sitelinks
                .contains_key(&(site.clone(), title.clone()))
            {
                continue;
            }
            wanted.entry(site).or_default().push(title);
        }
    }

    let mut looked_up = 0;
    let mut requests = 0;
    for (site, titles) in wanted {
        let titles = unique(titles.into_iter());
        looked_up += titles.len();
        for chunk in titles.chunks(WIKIDATA_BATCH) {
            requests += 1;
            let found = if site == WIKIDATA_LANG {
                wikidata.ja_titles_for_ids(chunk).await
            } else {
                wikidata.ja_titles_for_site(&site, chunk).await
            };
            let found = match found {
                Ok(found) => found,
                Err(error) => {
                    error!("skipping {} titles on {site}: {error}", chunk.len());
                    continue;
                }
            };
            for title in chunk {
                let linked = found.get(title).cloned();
                caches
                    .sitelinks
                    .insert((site.clone(), title.clone()), linked);
            }
        }
    }
    (looked_up, requests)
}

/// Whether shutdown has been signalled.
fn is_shutdown(shutdown: &watch::Receiver<bool>) -> bool {
    *shutdown.borrow()
}

/// Find the first language target of `call` that has a Japanese counterpart
/// worth reporting.
///
/// A redirect whose target is exactly the Japanese article found through
/// Wikidata is not reported: the interlanguage link already points at the
/// right subject, and those calls are tracked by a template category.
fn match_target(
    wikidata: &Wikidata,
    caches: &Caches,
    call: &IllCall,
    state: &PageState,
) -> Option<(LangTarget, String)> {
    call.targets.iter().find_map(|target| {
        let site = if target.lang.eq_ignore_ascii_case(WIKIDATA_LANG) {
            WIKIDATA_LANG.to_string()
        } else {
            wikidata.site_for(&target.lang)?
        };
        let key = (site, normalize_title(&target.title));
        let linked = caches.sitelinks.get(&key)?.clone()?;
        if matches!(state, PageState::Redirect(to) if *to == linked) {
            return None;
        }
        Some((target.clone(), linked))
    })
}

/// How many articles to scan at most.
///
/// Overridable with the `SZ_BOT_ILL_PAGES` environment variable for quick dry
/// runs; unset means no limit.
fn page_limit() -> usize {
    page_limit_from(env::var(PAGE_LIMIT_ENV).ok())
}

/// Parse an article limit override, falling back to no limit.
fn page_limit_from(value: Option<String>) -> usize {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(usize::MAX)
}

/// Format a progress line: how far the scan has got, and how long it has taken
/// against how long the rest is expected to take.
fn progress(scanned: usize, articles: usize, findings: usize, elapsed: Duration) -> String {
    let percent = (scanned * 100).checked_div(articles).unwrap_or(100);
    let left = if scanned == 0 {
        "unknown".to_string()
    } else {
        let remaining = articles.saturating_sub(scanned) as f64;
        let seconds = elapsed.as_secs_f64() * remaining / scanned as f64;
        format::duration(Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX))
    };
    format!(
        "Scanned {}/{} articles ({percent}%), {} findings, {} elapsed, ~{left} left",
        format::count(scanned as u64),
        format::count(articles as u64),
        format::count(findings as u64),
        format::duration(elapsed)
    )
}

/// Make a template call safe to place inside a `<nowiki>` table cell.
fn sanitize_raw(raw: &str) -> String {
    let flattened: String = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .take(RAW_LIMIT)
        .collect();
    let mut text = flattened.replace("</nowiki>", "</ nowiki>");
    if raw.chars().count() > RAW_LIMIT {
        text.push('…');
    }
    text
}

/// Resolve a language code to a Wikidata site, warning once per unknown code.
fn site_for(wikidata: &Wikidata, caches: &mut Caches, lang: &str) -> Option<String> {
    if lang.eq_ignore_ascii_case(WIKIDATA_LANG) {
        return Some(WIKIDATA_LANG.to_string());
    }
    let site = wikidata.site_for(lang);
    if site.is_none() && caches.unknown_langs.insert(lang.to_string()) {
        warn!("unknown language code in an interlanguage link: {lang}");
    }
    site
}

/// Collect values into a vector without duplicates, preserving first-seen
/// order.
fn unique(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values.filter(|value| seen.insert(value.clone())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(state: PageState) -> Finding {
        Finding {
            article: "アンパサンド".to_string(),
            ja_title: "アンペールの法則 (曖昧さ回避)".to_string(),
            linked: "アンペールの法則".to_string(),
            raw: "{{仮リンク|アンペールの法則 (曖昧さ回避)|en|Ampère's circuital law}}".to_string(),
            state,
            target: LangTarget {
                lang: "en".to_string(),
                title: "Ampère's circuital law".to_string(),
            },
        }
    }

    #[test]
    fn builds_table() {
        let findings = vec![finding(PageState::Missing)];
        let updated: DateTime<Utc> = "2026-09-16T12:34:56Z".parse().unwrap();
        let wikitext = build_report(&findings, 1, 297_792, updated);

        assert!(wikitext.contains("最終更新: 2026-09-16 12:34 (UTC)"));
        assert!(wikitext.contains("走査した記事: 297,792件 / 検出: 1件"));
        assert!(!wikitext.contains("うち"));
        assert!(wikitext.contains("| [[アンパサンド]]"));
        assert!(wikitext.contains("[[:en:Ampère's circuital law|en:Ampère's circuital law]]"));
        assert!(wikitext.contains("| [[アンペールの法則]]"));
        assert!(wikitext.contains("は未作成"));
        assert!(wikitext.ends_with("|}\n"));
    }

    #[test]
    fn reports_truncated_count() {
        let findings = vec![finding(PageState::Missing)];
        let updated: DateTime<Utc> = "2026-09-16T12:34:56Z".parse().unwrap();
        let wikitext = build_report(&findings, 2500, 297_792, updated);

        assert!(wikitext.contains("検出: 2,500件（うち1件を表示）"));
    }

    #[test]
    fn describes_redirect_state() {
        let finding = finding(PageState::Redirect("電磁気学".to_string()));
        assert_eq!(
            describe_state(&finding),
            "[[アンペールの法則 (曖昧さ回避)]]は[[電磁気学]]へのリダイレクト"
        );
    }

    #[test]
    fn sanitizes_raw_calls() {
        assert_eq!(
            sanitize_raw("{{仮リンク|foo\n|en|Foo}}"),
            "{{仮リンク|foo |en|Foo}}"
        );
        assert!(sanitize_raw("a</nowiki>b").contains("</ nowiki>"));

        let long = format!("{{{{仮リンク|{}|en|Foo}}}}", "あ".repeat(RAW_LIMIT));
        let sanitized = sanitize_raw(&long);
        assert_eq!(sanitized.chars().count(), RAW_LIMIT + 1);
        assert!(sanitized.ends_with('…'));
    }

    #[test]
    fn formats_progress_with_estimate() {
        let line = progress(50_000, 297_792, 1234, Duration::from_mins(42));
        assert_eq!(
            line,
            "Scanned 50,000/297,792 articles (16%), 1,234 findings, 42m00s elapsed, ~3h28m left"
        );
    }

    #[test]
    fn formats_progress_before_any_article() {
        let line = progress(0, 100, 0, Duration::from_secs(5));
        assert!(line.contains("(0%)"));
        assert!(line.contains("~unknown left"));
    }

    #[test]
    fn warns_only_when_requests_are_slow() {
        let fast = RoundStats {
            ja_elapsed: Duration::from_secs(10),
            ja_requests: 20,
            sitelink_elapsed: Duration::from_secs(5),
            sitelink_requests: 10,
        };
        assert!(fast.throttle_warning().is_none());

        let throttled = RoundStats {
            ja_elapsed: Duration::from_secs(400),
            ja_requests: 20,
            sitelink_elapsed: Duration::from_secs(5),
            sitelink_requests: 10,
        };
        let warning = throttled.throttle_warning().unwrap();
        assert!(warning.starts_with("jawiki: 20.0s per request"));
    }

    #[test]
    fn warns_about_the_slower_api() {
        let stats = RoundStats {
            ja_elapsed: Duration::from_secs(80),
            ja_requests: 20,
            sitelink_elapsed: Duration::from_secs(180),
            sitelink_requests: 10,
        };
        let warning = stats.throttle_warning().unwrap();
        assert!(warning.starts_with("Wikidata: 18.0s per request"));
    }

    #[test]
    fn ignores_rounds_without_requests() {
        let idle = RoundStats {
            ja_elapsed: Duration::ZERO,
            ja_requests: 0,
            sitelink_elapsed: Duration::ZERO,
            sitelink_requests: 0,
        };
        assert!(idle.throttle_warning().is_none());
    }

    #[test]
    fn page_limit_defaults_and_parses() {
        assert_eq!(page_limit_from(None), usize::MAX);
        assert_eq!(page_limit_from(Some("25".to_string())), 25);
        assert_eq!(page_limit_from(Some("many".to_string())), usize::MAX);
    }

    #[test]
    fn deduplicates_preserving_order() {
        let values = ["b", "a", "b", "c"].into_iter().map(str::to_string);
        assert_eq!(unique(values), vec!["b", "a", "c"]);
    }
}

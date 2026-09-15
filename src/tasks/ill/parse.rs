//! Parser for interlanguage-link template calls in wikitext.
//!
//! `Template:仮リンク` and its redirects take the Japanese article name as the
//! first positional parameter, followed by up to four language code / foreign
//! title pairs (parameters 2/3, 4/5, 6/7 and 8/9).

/// Template names that expand to `Template:仮リンク`, spelled as
/// [`normalize_name`] returns them.
const ILL_TEMPLATES: [&str; 7] = [
    "仮リンク",
    "Ill",
    "Ill2",
    "Illm",
    "Link-interwiki",
    "Interlanguage link",
    "Interlanguage link multi",
];
/// Namespace prefixes stripped from a template name before matching.
const NAMESPACE_PREFIXES: [&str; 2] = ["Template:", "テンプレート:"];
/// Positional parameter indices holding the language code / title pairs.
const PAIR_INDICES: [(usize, usize); 4] = [(2, 3), (4, 5), (6, 7), (8, 9)];
/// Templates that render only the first language pair. Later pairs never reach
/// the reader, so they are not treated as link targets.
const SINGLE_PAIR_TEMPLATES: [&str; 2] = ["Illm", "Interlanguage link multi"];
/// Number of positional parameter slots tracked, covering [`PAIR_INDICES`].
const SLOTS: usize = 10;

/// An interlanguage-link template call found in an article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IllCall {
    /// The call exactly as it appears in the wikitext.
    pub raw: String,
    /// Japanese article name given as the first positional parameter.
    pub ja_title: String,
    /// Language targets, in the order they appear.
    pub targets: Vec<LangTarget>,
}

/// A language code paired with the title of the article on that Wikipedia.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LangTarget {
    /// Language code as written in the template, e.g. `en`. The pseudo-code
    /// `wikidata` names a Wikidata item instead of an article.
    pub lang: String,
    /// Article title on that Wikipedia, or a Q-id when `lang` is `wikidata`.
    pub title: String,
}

/// Collect every interlanguage-link template call in `wikitext`.
///
/// Calls nested inside other templates are found as well, because a template
/// that is not an interlanguage link is descended into rather than skipped.
#[must_use]
pub fn find_calls(wikitext: &str) -> Vec<IllCall> {
    let bytes = wikitext.as_bytes();
    let mut calls = Vec::new();
    let mut index = 0;

    while index + 1 < bytes.len() {
        if bytes[index] == b'{'
            && bytes[index + 1] == b'{'
            && let Some(end) = template_end(bytes, index)
            && let Some(call) = wikitext.get(index..end).and_then(parse_call)
        {
            calls.push(call);
            index = end;
            continue;
        }
        index += 1;
    }

    calls
}

/// Assign the parameters after the template name to positional slots.
///
/// Unnamed parameters fill the next free index; a parameter named with a plain
/// number overrides that index, as `MediaWiki` does. Parameters named anything
/// else never carry a link target and are dropped.
fn collect_positional<'a>(params: impl Iterator<Item = &'a str>) -> Vec<Option<String>> {
    let mut positional = vec![None; SLOTS];
    let mut next = 1usize;

    for param in params {
        let (index, value) = if let Some((name, value)) = split_named(param) {
            match name.parse::<usize>() {
                Ok(index) => (index, value),
                Err(_) => continue,
            }
        } else {
            let index = next;
            next += 1;
            (index, param)
        };
        if let Some(entry) = positional.get_mut(index) {
            *entry = Some(value.trim().to_string());
        }
    }

    positional
}

/// Normalise a template name for comparison against [`ILL_TEMPLATES`].
///
/// Strips a leading colon and namespace prefix, turns underscores into spaces
/// and capitalises the first ASCII letter, matching how `MediaWiki` resolves a
/// template name to a page.
fn normalize_name(name: &str) -> String {
    let name = name.trim().trim_start_matches(':').trim();
    let name = strip_namespace(name);
    let mut normalized = name.replace('_', " ").trim().to_string();
    if let Some(first) = normalized.get(..1) {
        let upper = first.to_ascii_uppercase();
        normalized.replace_range(..1, &upper);
    }
    normalized
}

/// Parse a `{{...}}` call, returning `None` unless it is an interlanguage link
/// with both a Japanese article name and at least one language target.
fn parse_call(raw: &str) -> Option<IllCall> {
    let body = raw.get(2..raw.len().checked_sub(2)?)?;
    let mut params = split_params(body).into_iter();
    let name = normalize_name(params.next()?);
    if !ILL_TEMPLATES.iter().any(|known| name == *known) {
        return None;
    }

    let positional = collect_positional(params);
    let ja_title = slot(&positional, 1)?;
    let pairs = if SINGLE_PAIR_TEMPLATES.iter().any(|single| name == *single) {
        1
    } else {
        PAIR_INDICES.len()
    };
    let targets: Vec<LangTarget> = PAIR_INDICES
        .iter()
        .take(pairs)
        .filter_map(|&(lang, title)| {
            Some(LangTarget {
                lang: slot(&positional, lang)?,
                title: slot(&positional, title)?,
            })
        })
        .collect();
    if targets.is_empty() {
        return None;
    }

    Some(IllCall {
        raw: raw.to_string(),
        ja_title,
        targets,
    })
}

/// Read a positional slot, treating an empty value as absent.
fn slot(positional: &[Option<String>], index: usize) -> Option<String> {
    positional
        .get(index)?
        .as_ref()
        .filter(|value| !value.is_empty())
        .cloned()
}

/// Split a parameter into name and value at its first top-level `=`.
fn split_named(param: &str) -> Option<(&str, &str)> {
    let bytes = param.as_bytes();
    let mut depth = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        match (bytes[index], bytes.get(index + 1).copied()) {
            (b'{', Some(b'{')) | (b'[', Some(b'[')) => {
                depth += 1;
                index += 2;
            }
            (b'}', Some(b'}')) | (b']', Some(b']')) => {
                depth = depth.saturating_sub(1);
                index += 2;
            }
            (b'=', _) if depth == 0 => {
                return Some((param.get(..index)?.trim(), param.get(index + 1..)?));
            }
            _ => index += 1,
        }
    }

    None
}

/// Split a template body on its top-level `|` separators.
///
/// Nested templates and wikilinks are skipped so that their own separators do
/// not split the enclosing call.
fn split_params(body: &str) -> Vec<&str> {
    let bytes = body.as_bytes();
    let mut params = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        match (bytes[index], bytes.get(index + 1).copied()) {
            (b'{', Some(b'{')) | (b'[', Some(b'[')) => {
                depth += 1;
                index += 2;
            }
            (b'}', Some(b'}')) | (b']', Some(b']')) => {
                depth = depth.saturating_sub(1);
                index += 2;
            }
            (b'|', _) if depth == 0 => {
                if let Some(param) = body.get(start..index) {
                    params.push(param);
                }
                index += 1;
                start = index;
            }
            _ => index += 1,
        }
    }
    if let Some(param) = body.get(start..) {
        params.push(param);
    }

    params
}

/// Strip the `Template:` namespace prefix, in either language, if present.
fn strip_namespace(name: &str) -> &str {
    for prefix in NAMESPACE_PREFIXES {
        if name
            .get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
        {
            return name.get(prefix.len()..).unwrap_or(name).trim();
        }
    }
    name
}

/// Find the byte offset just past the `}}` that closes the template starting
/// at `start`, or `None` when the call is unterminated.
const fn template_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut index = start + 2;

    while index + 1 < bytes.len() {
        match (bytes[index], bytes[index + 1]) {
            (b'{', b'{') => {
                depth += 1;
                index += 2;
            }
            (b'}', b'}') => {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => index += 1,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_calls() -> Vec<IllCall> {
        Vec::new()
    }

    fn target(lang: &str, title: &str) -> LangTarget {
        LangTarget {
            lang: lang.to_string(),
            title: title.to_string(),
        }
    }

    #[test]
    fn parses_basic_call() {
        let calls = find_calls("前 {{仮リンク|アンペールの法則|en|Ampère's circuital law}} 後");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].ja_title, "アンペールの法則");
        assert_eq!(
            calls[0].targets,
            vec![target("en", "Ampère's circuital law")]
        );
        assert!(calls[0].raw.starts_with("{{仮リンク|"));
    }

    #[test]
    fn parses_redirect_names() {
        for name in ["Ill", "Ill2", "Link-interwiki", "Interlanguage link"] {
            let calls = find_calls(&format!("{{{{{name}|foo|en|Foo}}}}"));
            assert_eq!(calls.len(), 1, "{name} should be recognised");
            assert_eq!(calls[0].ja_title, "foo");
        }
    }

    #[test]
    fn parses_multiple_pairs() {
        let calls = find_calls("{{仮リンク|foo|en|Foo|de|Foo (Begriff)|fr|Foo (fr)}}");
        assert_eq!(
            calls[0].targets,
            vec![
                target("en", "Foo"),
                target("de", "Foo (Begriff)"),
                target("fr", "Foo (fr)"),
            ]
        );
    }

    #[test]
    fn keeps_only_first_pair_for_multi_templates() {
        let calls = find_calls("{{Illm|foo|en|Foo|de|Foo (de)}}");
        assert_eq!(calls[0].targets, vec![target("en", "Foo")]);
    }

    #[test]
    fn handles_named_parameters() {
        let calls = find_calls("{{仮リンク|foo|en|Foo|label=ふー|preserve=1}}");
        assert_eq!(calls[0].ja_title, "foo");
        assert_eq!(calls[0].targets, vec![target("en", "Foo")]);
    }

    #[test]
    fn handles_numeric_named_parameters() {
        let calls = find_calls("{{仮リンク|3=Foo|1=foo|2=en}}");
        assert_eq!(calls[0].ja_title, "foo");
        assert_eq!(calls[0].targets, vec![target("en", "Foo")]);
    }

    #[test]
    fn trims_whitespace_and_underscores() {
        let calls = find_calls("{{ template:仮_リンク | foo | en | Foo }}");
        assert_eq!(calls, no_calls(), "an unknown name must not match");

        let calls = find_calls("{{ Template:仮リンク | foo | en | Foo }}");
        assert_eq!(calls[0].ja_title, "foo");
        assert_eq!(calls[0].targets, vec![target("en", "Foo")]);
    }

    #[test]
    fn finds_calls_nested_in_other_templates() {
        let calls =
            find_calls("{{Infobox|name={{仮リンク|foo|en|Foo}}|other={{仮リンク|bar|de|Bar}}}}");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].ja_title, "foo");
        assert_eq!(calls[1].ja_title, "bar");
    }

    #[test]
    fn keeps_nested_wikilink_labels_intact() {
        let calls = find_calls("{{仮リンク|foo|en|Foo|label=[[bar|baz]]}}");
        assert_eq!(calls[0].targets, vec![target("en", "Foo")]);
    }

    #[test]
    fn ignores_calls_without_targets() {
        assert_eq!(find_calls("{{仮リンク|foo}}"), no_calls());
        assert_eq!(find_calls("{{仮リンク|foo|en}}"), no_calls());
        assert_eq!(find_calls("{{仮リンク||en|Foo}}"), no_calls());
    }

    #[test]
    fn ignores_other_templates() {
        assert_eq!(find_calls("{{Reflist}}"), no_calls());
        assert_eq!(find_calls("{{仮リンク2|foo|en|Foo}}"), no_calls());
        assert_eq!(find_calls("plain [[foo]] text"), no_calls());
    }

    #[test]
    fn ignores_unterminated_calls() {
        assert_eq!(find_calls("{{仮リンク|foo|en|Foo"), no_calls());
    }

    #[test]
    fn accepts_wikidata_pseudo_language() {
        let calls = find_calls("{{仮リンク|foo|wikidata|Q42}}");
        assert_eq!(calls[0].targets, vec![target("wikidata", "Q42")]);
    }
}

use regex::Regex;
use std::collections::BTreeMap;

pub fn process(
    text: &str,
    remove_fillers: bool,
    fillers: &[String],
    dictionary: &BTreeMap<String, String>,
) -> String {
    let mut out = text.to_string();
    if remove_fillers && !fillers.is_empty() {
        out = strip_fillers(&out, fillers);
    }
    if !dictionary.is_empty() {
        out = apply_dictionary(&out, dictionary);
    }
    normalize(&out)
}

fn bounded_pattern(phrase: &str) -> String {
    let first_is_word = phrase
        .chars()
        .next()
        .map(|c| c.is_alphanumeric() || c == '_')
        .unwrap_or(false);
    let last_is_word = phrase
        .chars()
        .last()
        .map(|c| c.is_alphanumeric() || c == '_')
        .unwrap_or(false);
    format!(
        "{}{}{}",
        if first_is_word { r"\b" } else { "" },
        regex::escape(phrase),
        if last_is_word { r"\b" } else { "" }
    )
}

const SAFE_FILLERS: &[&str] = &[
    "aham", "ahn", "ahm", "ééé", "é é", "éé", "eh", "ehh", "er", "err", "hm", "hmm", "hmmm",
    "hum", "humm", "uh", "uhh", "uhm", "mmm", "ã", "ãã", "ãhã",
];

fn is_safe_filler(word: &str) -> bool {
    let lowered = word.trim().to_lowercase();
    SAFE_FILLERS.iter().any(|safe| *safe == lowered)
}

fn strip_fillers(text: &str, fillers: &[String]) -> String {
    let parts: Vec<String> = fillers
        .iter()
        .map(|f| f.trim())
        .filter(|f| !f.is_empty() && is_safe_filler(f))
        .map(bounded_pattern)
        .collect();
    if parts.is_empty() {
        return text.to_string();
    }
    let pattern = format!("(?i)(?:{})", parts.join("|"));
    match Regex::new(&pattern) {
        Ok(re) => re.replace_all(text, " ").to_string(),
        Err(_) => text.to_string(),
    }
}

fn apply_dictionary(text: &str, dictionary: &BTreeMap<String, String>) -> String {
    let mut entries: Vec<(&str, &str)> = dictionary
        .iter()
        .map(|(k, v)| (k.trim(), v.as_str()))
        .filter(|(k, _)| !k.is_empty())
        .collect();
    if entries.is_empty() {
        return text.to_string();
    }
    entries.sort_by(|a, b| b.0.len().cmp(&a.0.len()));

    let pattern = format!(
        "(?i)(?:{})",
        entries
            .iter()
            .map(|(k, _)| bounded_pattern(k))
            .collect::<Vec<_>>()
            .join("|")
    );
    let lookup: Vec<(String, &str)> = entries
        .iter()
        .map(|(k, v)| (k.to_lowercase(), *v))
        .collect();

    match Regex::new(&pattern) {
        Ok(re) => re
            .replace_all(text, |caps: &regex::Captures| {
                let matched = caps.get(0).map(|m| m.as_str()).unwrap_or("");
                let lower = matched.to_lowercase();
                lookup
                    .iter()
                    .find(|(k, _)| *k == lower)
                    .map(|(_, v)| (*v).to_string())
                    .unwrap_or_else(|| matched.to_string())
            })
            .to_string(),
        Err(_) => text.to_string(),
    }
}

fn normalize(text: &str) -> String {
    let collapsed = match Regex::new(r"\s+") {
        Ok(re) => re.replace_all(text, " ").to_string(),
        Err(_) => text.to_string(),
    };
    let punctuation = match Regex::new(r"\s+([,.;:!?])") {
        Ok(re) => re.replace_all(&collapsed, "$1").to_string(),
        Err(_) => collapsed,
    };
    punctuation.trim().to_string()
}

pub fn word_count(text: &str) -> i64 {
    text.split_whitespace().filter(|w| !w.is_empty()).count() as i64
}

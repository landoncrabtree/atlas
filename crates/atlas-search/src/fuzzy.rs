//! One-shot fuzzy scoring helpers using [`nucleo`].

use nucleo::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32String,
};

/// Score `haystack` against `needle` with nucleo's fuzzy matcher.
#[must_use]
pub fn fuzzy_score(needle: &str, haystack: &str) -> Option<u32> {
    if needle.trim().is_empty() || haystack.is_empty() {
        return None;
    }

    let pattern = Pattern::parse(needle, CaseMatching::Smart, Normalization::Smart);
    let haystack = Utf32String::from(haystack);
    let mut matcher = Matcher::new(Config::DEFAULT);
    pattern.score(haystack.slice(..), &mut matcher)
}

/// Rank `items` by fuzzy score using `key` as the searchable string.
#[must_use]
pub fn fuzzy_rank<T, F: Fn(&T) -> &str>(items: Vec<T>, needle: &str, key: F) -> Vec<(T, u32)> {
    if needle.trim().is_empty() {
        return Vec::new();
    }

    let mut ranked: Vec<(T, u32)> = items
        .into_iter()
        .filter_map(|item| fuzzy_score(needle, key(&item)).map(|score| (item, score)))
        .collect();
    ranked.sort_by(|left, right| right.1.cmp(&left.1));
    ranked
}

#[cfg(test)]
mod tests {
    use super::{fuzzy_rank, fuzzy_score};

    #[test]
    fn fuzzy_score_matches_similar_names() {
        assert!(fuzzy_score("atlas", "atlas-app").is_some());
    }

    #[test]
    fn fuzzy_score_rejects_empty_inputs() {
        assert_eq!(fuzzy_score("", "atlas"), None);
        assert_eq!(fuzzy_score("atlas", ""), None);
    }

    #[test]
    fn fuzzy_rank_orders_best_match_first() {
        let ranked = fuzzy_rank(vec!["atlas-ui", "notes", "atlas"], "atlas", |item| item);

        assert!(!ranked.is_empty());
        assert!(ranked.iter().all(|(item, _)| *item != "notes"));
        assert!(ranked.windows(2).all(|pair| pair[0].1 >= pair[1].1));
    }
}

//! Thin wrapper around [`nucleo::Nucleo`] for palette item matching.

use std::sync::Arc;

use nucleo::{
    pattern::{CaseMatching, Normalization},
    Config, Matcher, Nucleo, Utf32String,
};

use crate::palette::source::PaletteItem;

/// Wraps [`nucleo::Nucleo`] to provide typed fuzzy matching over [`PaletteItem`]s.
pub struct PaletteMatcher {
    inner: Nucleo<PaletteItem>,
    scorer: Matcher,
}

impl Default for PaletteMatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl PaletteMatcher {
    /// Create a new matcher.
    #[must_use]
    pub fn new() -> Self {
        let inner = Nucleo::new(Config::DEFAULT, Arc::new(|| {}), None, 1);
        Self {
            inner,
            scorer: Matcher::new(Config::DEFAULT),
        }
    }

    /// Replace all candidate items.
    pub fn set_items(&mut self, items: impl IntoIterator<Item = PaletteItem>) {
        self.inner.restart(true);
        let injector = self.inner.injector();
        for item in items {
            let title = item.title.clone();
            injector.push(item, move |_item, columns| {
                columns[0] = Utf32String::from(title.as_str());
            });
        }
    }

    /// Run the query and return up to `limit` scored matches in score order.
    pub fn query(&mut self, needle: &str, limit: usize) -> Vec<(PaletteItem, u32)> {
        self.inner
            .pattern
            .reparse(0, needle, CaseMatching::Smart, Normalization::Smart, false);

        loop {
            let status = self.inner.tick(10);
            if !status.running {
                break;
            }
        }

        let snapshot = self.inner.snapshot();
        let count = snapshot.matched_item_count().min(limit as u32);
        let pattern = snapshot.pattern().clone();

        snapshot
            .matched_items(0..count)
            .map(|item| {
                let score = pattern
                    .score(item.matcher_columns, &mut self.scorer)
                    .unwrap_or(0);
                (item.data.clone(), score)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::source::PaletteItemKind;

    fn make_item(id: &str, title: &str) -> PaletteItem {
        PaletteItem {
            id: id.to_owned(),
            title: title.to_owned(),
            subtitle: String::new(),
            kind: PaletteItemKind::Action,
        }
    }

    #[test]
    fn matcher_returns_hits_in_expected_order() {
        let mut matcher = PaletteMatcher::new();
        matcher.set_items(vec![
            make_item("a::Open", "Open File"),
            make_item("a::OpenSettings", "Open Settings"),
            make_item("b::Quit", "Quit"),
        ]);
        let hits = matcher.query("open", 10);
        assert_eq!(hits.len(), 2, "expected 2 hits for 'open'");
        let titles: Vec<&str> = hits.iter().map(|(item, _)| item.title.as_str()).collect();
        assert!(titles.contains(&"Open File"));
        assert!(titles.contains(&"Open Settings"));
    }

    #[test]
    fn matcher_empty_query_returns_all() {
        let mut matcher = PaletteMatcher::new();
        matcher.set_items(vec![make_item("a::A", "Alpha"), make_item("a::B", "Beta")]);
        let hits = matcher.query("", 10);
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn matcher_no_match_returns_empty() {
        let mut matcher = PaletteMatcher::new();
        matcher.set_items(vec![make_item("a::A", "Alpha")]);
        let hits = matcher.query("zzz", 10);
        assert!(hits.is_empty());
    }
}

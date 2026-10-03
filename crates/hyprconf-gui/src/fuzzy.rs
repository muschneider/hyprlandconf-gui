// SPDX-License-Identifier: MIT OR Apache-2.0
//! A tiny, dependency-free fuzzy matcher for the search box, plus the cached
//! [`SearchIndex`] the results pane renders from.
//!
//! Matching is a case-insensitive subsequence test with bonuses for matches at
//! the start of the haystack and for runs of contiguous characters, which is
//! enough to rank option label/path/description hits sensibly.
//!
//! Scoring the whole schema is cheap but *not* free, and `view` runs on every
//! frame — so the result set is computed once per query in `update` and stored
//! in [`SearchIndex`], which borrows from the `'static` schema and therefore
//! allocates nothing per hit.

use hyprconf_core::schema::{CollectionId, OptionSpec, Schema};

/// How many hits the results pane renders. Each hit is a full editor now, so
/// the cap is what keeps a one-letter query from building hundreds of widgets.
pub const MAX_HITS: usize = 40;

/// One matching option and the section it belongs to.
#[derive(Debug, Clone, Copy)]
pub struct OptionHit {
    /// The owning section's id (for the icon and the "jump to section" action).
    pub section: &'static str,
    /// The matched option.
    pub spec: &'static OptionSpec,
}

/// The cached result of scoring one query against the schema.
///
/// Built in `update`, read by `view`. `total` counts every match while
/// [`SearchIndex::options`] holds at most [`MAX_HITS`] of them, so the pane can
/// honestly say "showing 40 of 112".
#[derive(Debug, Default)]
pub struct SearchIndex {
    /// Matching options, best match first (capped at [`MAX_HITS`]).
    pub options: Vec<OptionHit>,
    /// Matching structured collections, best match first.
    pub collections: Vec<CollectionId>,
    /// How many options matched in total, before the cap.
    pub total: usize,
}

impl SearchIndex {
    /// Score `query` against every option and collection in `schema`.
    ///
    /// A blank query yields an empty index (the pane isn't shown then anyway),
    /// which keeps the common case free.
    #[must_use]
    pub fn build(schema: &'static Schema, query: &str) -> Self {
        let query = query.trim();
        if query.is_empty() {
            return Self::default();
        }

        let mut scored: Vec<(i32, OptionHit)> = Vec::new();
        for section in schema.sections() {
            for spec in &section.options {
                if let Some(score) = option_score(query, &spec.label, &spec.path, &spec.description)
                {
                    scored.push((
                        score,
                        OptionHit {
                            section: section.id.as_str(),
                            spec,
                        },
                    ));
                }
            }
        }
        // Best score first; ties broken by path so the order is stable between
        // frames (a list that reshuffles under the cursor is unusable).
        scored.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.spec.path.cmp(&b.1.spec.path))
        });

        let total = scored.len();
        let options = scored
            .into_iter()
            .take(MAX_HITS)
            .map(|(_, hit)| hit)
            .collect();

        // Collections are whole screens with long descriptions, where a
        // subsequence match is nearly meaningless ("blur" ⊂ "…variables (textual
        // macros)"). Their label matches fuzzily; their description only by a
        // real substring.
        let needle = query.trim().to_ascii_lowercase();
        let mut collections: Vec<(i32, CollectionId)> = schema
            .collections()
            .iter()
            .filter_map(|c| {
                let by_label = score(query, &c.label).map(|s| s + 20);
                let by_text = (!needle.is_empty()
                    && c.description.to_ascii_lowercase().contains(&needle))
                .then_some(5);
                by_label.or(by_text).map(|score| (score, c.id))
            })
            .collect();
        collections.sort_by_key(|&(score, _)| std::cmp::Reverse(score));

        Self {
            options,
            collections: collections.into_iter().map(|(_, id)| id).collect(),
            total,
        }
    }

    /// Whether nothing matched at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.options.is_empty() && self.collections.is_empty()
    }

    /// How many matches were hidden by [`MAX_HITS`].
    #[must_use]
    pub fn hidden(&self) -> usize {
        self.total.saturating_sub(self.options.len())
    }
}

/// Score `query` against `haystack`. Returns `None` if `query` is not a
/// subsequence of `haystack`; a higher score is a better match.
#[must_use]
pub fn score(query: &str, haystack: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_ascii_lowercase();
    let haystack = haystack.to_ascii_lowercase();
    let needle = query.as_bytes();

    let mut ni = 0usize;
    let mut total = 0i32;
    let mut previous_match: Option<usize> = None;

    for (hi, &hc) in haystack.as_bytes().iter().enumerate() {
        if ni < needle.len() && hc == needle[ni] {
            total += 1;
            if hi == 0 {
                total += 8;
            }
            if let Some(prev) = previous_match {
                if hi == prev + 1 {
                    total += 4;
                }
            }
            previous_match = Some(hi);
            ni += 1;
        }
    }

    (ni == needle.len()).then_some(total)
}

/// Best score of `query` across an option's fields, with field weighting
/// (label > path > description). `None` if it matches none of them.
#[must_use]
pub fn option_score(query: &str, label: &str, path: &str, description: &str) -> Option<i32> {
    let candidates = [
        score(query, label).map(|s| s + 20),
        score(query, path).map(|s| s + 10),
        score(query, description),
    ];
    candidates.into_iter().flatten().max()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_matches() {
        assert!(score("gap", "gaps_in").is_some());
        assert!(score("gpsn", "gaps_in").is_some()); // non-contiguous subsequence
        assert!(score("xyz", "gaps_in").is_none());
    }

    #[test]
    fn empty_query_matches_everything() {
        assert_eq!(score("", "anything"), Some(0));
    }

    #[test]
    fn prefix_and_contiguous_score_higher() {
        let prefix = score("gap", "gaps_in").unwrap();
        let middle = score("gap", "x_gap").unwrap();
        assert!(
            prefix > middle,
            "prefix {prefix} should beat middle {middle}"
        );
    }

    #[test]
    fn label_is_weighted_above_description() {
        let via_label =
            option_score("round", "Rounding", "decoration:rounding", "corners").unwrap();
        let via_desc =
            option_score("corner", "Rounding", "decoration:rounding", "corners").unwrap();
        assert!(via_label > via_desc);
    }

    #[test]
    fn blank_query_builds_an_empty_index() {
        let index = SearchIndex::build(Schema::shared(), "   ");
        assert!(index.is_empty());
        assert_eq!(index.total, 0);
    }

    #[test]
    fn index_ranks_and_caps_hits() {
        let index = SearchIndex::build(Schema::shared(), "rounding");
        assert!(!index.is_empty());
        assert!(index.options.len() <= MAX_HITS);
        assert!(index.total >= index.options.len());
        // The exact-label match must lead.
        assert_eq!(index.options[0].spec.path, "decoration:rounding");
        // Every hit knows the section it came from.
        assert!(index
            .options
            .iter()
            .all(|h| Schema::shared().section(h.section).is_some()));
    }

    #[test]
    fn collections_do_not_match_by_scattered_letters() {
        let index = SearchIndex::build(Schema::shared(), "blur");
        assert!(
            !index.collections.contains(&CollectionId::Variables),
            "\"blur\" must not match the Variables screen"
        );
        let index = SearchIndex::build(Schema::shared(), "touchpad");
        assert!(index.collections.contains(&CollectionId::Gestures));
    }

    #[test]
    fn index_finds_collections_too() {
        let index = SearchIndex::build(Schema::shared(), "keybind");
        assert!(
            index.collections.contains(&CollectionId::Keybinds),
            "collections must be searchable, not just scalar options"
        );
    }

    #[test]
    fn hit_order_is_stable_for_equal_scores() {
        let a = SearchIndex::build(Schema::shared(), "col");
        let b = SearchIndex::build(Schema::shared(), "col");
        let paths = |i: &SearchIndex| {
            i.options
                .iter()
                .map(|h| h.spec.path.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(&a), paths(&b));
    }
}

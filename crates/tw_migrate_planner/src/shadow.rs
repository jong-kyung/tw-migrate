use std::collections::HashMap;

use tw_migrate_css::{ShadowIndex, index_shadow_selectors};

#[cfg(test)]
thread_local! {
    pub(super) static PARSED_PIECES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Owned by one native batch. Only piece analysis is shared: each request
/// still composes its own corpus, including any retained-sibling overlay.
#[derive(Default)]
pub(super) struct ShadowSelectorCache {
    // The channels distinguish plain selectors from CSS Module selectors,
    // independently of the consuming stylesheet's is_module flag.
    pieces: [HashMap<String, ShadowIndex>; 2],
}

impl ShadowSelectorCache {
    pub(super) fn index(&mut self, pieces: &[String], module_pieces: &[String]) -> ShadowIndex {
        let mut index = ShadowIndex::default();
        for (module, pieces) in [pieces, module_pieces].into_iter().enumerate() {
            let cache = &mut self.pieces[module];
            for piece in pieces {
                let parsed = match cache.get(piece) {
                    Some(parsed) => parsed,
                    None => {
                        #[cfg(test)]
                        PARSED_PIECES.set(PARSED_PIECES.get() + 1);
                        let piece_slice = std::slice::from_ref(piece);
                        let parsed = if module == 0 {
                            index_shadow_selectors(piece_slice, &[])
                        } else {
                            index_shadow_selectors(&[], piece_slice)
                        };
                        cache.entry(piece.clone()).or_insert(parsed)
                    }
                };
                index.classes.extend(parsed.classes.iter().cloned());
                index.ids.extend(parsed.ids.iter().cloned());
                index.types.extend(parsed.types.iter().cloned());
                index.unverifiable |= parsed.unverifiable;
            }
        }
        index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keeps_exact_text_modes_and_corpora_separate() {
        let mut cache = ShadowSelectorCache::default();
        let own = ".card {}".to_string();
        let shared = ".card { color: red; }".to_string();
        let before = PARSED_PIECES.get();
        let first = cache.index(&[own.clone(), own.clone(), shared.clone()], &[own.clone()]);
        assert!(first.classes.contains("card"));
        assert_eq!(PARSED_PIECES.get() - before, 3);

        // A previous plain-channel hit must not leak into a module-only corpus.
        assert!(cache.index(&[], &[own.clone()]).classes.is_empty());
        // Excluding our piece must not remove a sibling's identical selector.
        assert!(cache.index(&[shared], &[]).classes.contains("card"));
        assert!(cache.index(&[], &[]).classes.is_empty());
        assert_eq!(PARSED_PIECES.get() - before, 3);

        cache.index(&[".card{}".to_string()], &[]);
        assert_eq!(PARSED_PIECES.get() - before, 4);
        for _ in 0..2 {
            assert!(cache.index(&["???".to_string()], &[]).unverifiable);
        }
        assert_eq!(PARSED_PIECES.get() - before, 5);
        assert!(!cache.index(&[own], &[]).unverifiable);
    }

    #[test]
    fn cached_piece_union_matches_uncached_selector_analysis() {
        let samples = [
            ".card {}",
            "#card {}",
            "BUTTON {}",
            ":global(.card) {}",
            ":global .card {}",
            "[data-x] {}",
            ".parent > * {}",
            "???",
            "@supports (display: grid) { .note {} }",
            "@keyframes spin { from { opacity: 0; } }",
        ];
        let mut cache = ShadowSelectorCache::default();
        for left in samples {
            for right in samples {
                for left_module in [false, true] {
                    for right_module in [false, true] {
                        let mut pieces = [Vec::new(), Vec::new()];
                        pieces[usize::from(left_module)].push(left.to_string());
                        pieces[usize::from(right_module)].push(right.to_string());
                        let cached = cache.index(&pieces[0], &pieces[1]);
                        let uncached = index_shadow_selectors(&pieces[0], &pieces[1]);
                        assert_eq!(cached.classes, uncached.classes);
                        assert_eq!(cached.ids, uncached.ids);
                        assert_eq!(cached.types, uncached.types);
                        assert_eq!(cached.unverifiable, uncached.unverifiable);
                    }
                }
            }
        }
    }
}

//! Best-first selection of scored chunks, shared by the code and chat searches: higher scores first, ties by key.

use std::cmp::Ordering;

/// A chunk: its source or segment, and its slot there.
type Key = (usize, u32);

/// Best first: the higher score, then the lower key.
pub fn score_order(a: &(Key, f32), b: &(Key, f32)) -> Ordering {
    b.1.total_cmp(&a.1).then(a.0.cmp(&b.0))
}

/// Keeps the `count` best of `scored`, sorted best first.
pub fn top_by_score(scored: &mut Vec<(Key, f32)>, count: usize) {
    if scored.len() > count && count > 0 {
        scored.select_nth_unstable_by(count - 1, score_order);
        scored.truncate(count);
    }
    scored.sort_by(score_order);
}

/// A bounded top-k with the last selection's worst retained candidate as a rejection threshold. That threshold
/// can only improve: most candidates need no insertion or selection, including long runs of equal scores.
#[derive(Debug)]
pub struct TopScores {
    scored: Vec<(Key, f32)>,
    count: usize,
    threshold: Option<(Key, f32)>,
}

impl TopScores {
    pub fn new(count: usize) -> Self {
        Self {
            scored: Vec::new(),
            count,
            threshold: None,
        }
    }

    pub fn push(&mut self, candidate: (Key, f32)) {
        if self
            .threshold
            .is_some_and(|threshold| !score_order(&candidate, &threshold).is_lt())
        {
            return;
        }
        self.scored.push(candidate);
        if self.scored.len() > self.count * 4 {
            top_by_score(&mut self.scored, self.count);
            self.threshold = self.scored.last().copied();
        }
    }

    /// The `count` best candidates pushed, best first.
    pub fn finish(mut self) -> Vec<(Key, f32)> {
        top_by_score(&mut self.scored, self.count);
        self.scored
    }
}

#[cfg(test)]
mod tests {
    use super::{TopScores, score_order};

    #[test]
    fn bounded_selection_matches_a_full_sort_with_score_and_key_ties() {
        for count in [1, 2, 17, 50, 1000] {
            for reversed in [false, true] {
                let mut candidates = (0_u32..1000)
                    .map(|doc| {
                        let score = match doc % 13 {
                            0 => f32::NEG_INFINITY,
                            1 => -0.0,
                            2 => 0.0,
                            3 => f32::INFINITY,
                            4 => f32::NAN,
                            _ => (doc % 7) as f32,
                        };
                        ((doc as usize % 3, doc), score)
                    })
                    .collect::<Vec<_>>();
                if reversed {
                    candidates.reverse();
                }
                let mut bounded = TopScores::new(count);
                for candidate in &candidates {
                    bounded.push(*candidate);
                }
                candidates.sort_by(score_order);
                candidates.truncate(count);
                let bits = |values: Vec<(_, f32)>| {
                    values
                        .into_iter()
                        .map(|(key, score)| (key, score.to_bits()))
                        .collect::<Vec<_>>()
                };
                assert_eq!(bits(bounded.finish()), bits(candidates));
            }
        }
    }
}

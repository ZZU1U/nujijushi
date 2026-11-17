//! Which queued post goes out next: pinned/priority first, then weighted random
//! with an artist cooldown so the feed does not repeat the same author.

use chrono::{DateTime, Utc};
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::StdRng;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub queue_id: i64,
    pub post_id: i64,
    pub author_id: Option<i64>,
    pub tags: Vec<String>,
    pub priority: i64,
    pub pinned: bool,
    pub added_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Selection {
    /// How many recent sends must not repeat the same artist.
    pub artist_cooldown: usize,
    /// Tag overlap with recent sends multiplies the weight by this factor.
    pub tag_penalty: f64,
    /// Authors of recent sends, most recent first.
    pub recent_authors: Vec<i64>,
    /// Tags of recent sends.
    pub recent_tags: Vec<String>,
}

impl Default for Selection {
    fn default() -> Self {
        Self {
            artist_cooldown: 3,
            tag_penalty: 0.5,
            recent_authors: Vec::new(),
            recent_tags: Vec::new(),
        }
    }
}

impl Selection {
    pub fn with_history(mut self, authors: Vec<i64>, tags: Vec<String>) -> Self {
        self.recent_authors = authors;
        self.recent_tags = tags;
        self
    }

    pub fn with_cooldown(mut self, cooldown: usize) -> Self {
        self.artist_cooldown = cooldown;
        self
    }

    /// Index into `candidates`, or None when the queue is empty.
    pub fn choose(&self, candidates: &[Candidate], rng: &mut impl Rng) -> Option<usize> {
        if candidates.is_empty() {
            return None;
        }

        // 1. pinned items always win, highest priority first, then oldest
        let pinned: Vec<usize> = candidates
            .iter()
            .enumerate()
            .filter(|(_, c)| c.pinned)
            .map(|(i, _)| i)
            .collect();
        if !pinned.is_empty() {
            return pinned.into_iter().max_by(|a, b| {
                let (ca, cb) = (&candidates[*a], &candidates[*b]);
                ca.priority
                    .cmp(&cb.priority)
                    .then(cb.added_at.cmp(&ca.added_at))
            });
        }

        // 2. artist cooldown, relaxed when it would starve the queue
        let cooled: Vec<usize> = (0..candidates.len())
            .filter(|index| !self.in_cooldown(candidates[*index].author_id))
            .collect();
        let pool = if cooled.is_empty() {
            (0..candidates.len()).collect()
        } else {
            cooled
        };

        // 3. weighted random
        let weights: Vec<f64> = pool
            .iter()
            .map(|index| self.weight(&candidates[*index]))
            .collect();
        let total: f64 = weights.iter().sum();
        if total <= f64::EPSILON {
            return Some(pool[rng.random_range(0..pool.len())]);
        }
        let mut ticket = rng.random_range(0.0..total);
        for (slot, weight) in pool.iter().zip(weights.iter()) {
            ticket -= weight;
            if ticket <= 0.0 {
                return Some(*slot);
            }
        }
        pool.last().copied()
    }

    fn in_cooldown(&self, author_id: Option<i64>) -> bool {
        let Some(author_id) = author_id else {
            return false;
        };
        if self.artist_cooldown == 0 {
            return false;
        }
        self.recent_authors
            .iter()
            .take(self.artist_cooldown)
            .any(|recent| *recent == author_id)
    }

    pub fn weight(&self, candidate: &Candidate) -> f64 {
        let mut weight = 1.0 + (candidate.priority.max(0) as f64) * 0.1;
        let overlap = candidate
            .tags
            .iter()
            .filter(|tag| self.recent_tags.iter().any(|recent| recent == *tag))
            .count();
        if overlap > 0 {
            weight *= self.tag_penalty.powi(overlap.min(3) as i32);
        }
        weight.max(0.01)
    }

    /// Deterministic simulation used by `nujijushi preview` (seeded per channel).
    pub fn simulate(&self, candidates: &[Candidate], seed: u64, count: usize) -> Vec<i64> {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut remaining: Vec<Candidate> = candidates.to_vec();
        let mut history = self.clone();
        let mut out = Vec::new();

        for _ in 0..count {
            let Some(index) = history.choose(&remaining, &mut rng) else {
                break;
            };
            let picked = remaining.remove(index);
            out.push(picked.queue_id);
            if let Some(author) = picked.author_id {
                history.recent_authors.insert(0, author);
                history
                    .recent_authors
                    .truncate(history.artist_cooldown.max(1));
            }
            history.recent_tags.extend(picked.tags.iter().cloned());
            let keep = history.recent_tags.len().saturating_sub(12);
            history.recent_tags.drain(0..keep);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::StdRng;

    fn candidate(queue_id: i64, author: Option<i64>, tags: &[&str]) -> Candidate {
        Candidate {
            queue_id,
            post_id: queue_id * 10,
            author_id: author,
            tags: tags.iter().map(|t| t.to_string()).collect(),
            priority: 0,
            pinned: false,
            added_at: Utc::now(),
        }
    }

    fn rng(seed: u64) -> StdRng {
        StdRng::seed_from_u64(seed)
    }

    #[test]
    fn empty_queue_selects_nothing() {
        let selection = Selection::default();
        assert_eq!(selection.choose(&[], &mut rng(1)), None);
    }

    #[test]
    fn pinned_beats_everything() {
        let candidates = vec![
            candidate(1, Some(1), &[]),
            Candidate {
                pinned: true,
                priority: 0,
                ..candidate(2, Some(2), &[])
            },
            Candidate {
                priority: 99,
                ..candidate(3, Some(3), &[])
            },
        ];
        let selection = Selection::default();
        for seed in 0..10 {
            assert_eq!(selection.choose(&candidates, &mut rng(seed)), Some(1));
        }
    }

    #[test]
    fn artist_cooldown_avoids_recent_authors() {
        let candidates = vec![
            candidate(1, Some(1), &[]),
            candidate(2, Some(2), &[]),
            candidate(3, Some(3), &[]),
        ];
        let selection = Selection::default().with_history(vec![1, 1], vec![]);
        for seed in 0..25 {
            let picked = selection.choose(&candidates, &mut rng(seed)).unwrap();
            assert_ne!(picked, 0, "author 1 is in cooldown");
        }
    }

    #[test]
    fn cooldown_relaxes_when_everything_is_cooling() {
        let candidates = vec![candidate(1, Some(1), &[]), candidate(2, Some(2), &[])];
        let selection = Selection::default().with_history(vec![1, 2], vec![]);
        let picked = selection.choose(&candidates, &mut rng(7));
        assert!(picked.is_some(), "queue must never starve");
    }

    #[test]
    fn priority_increases_weight() {
        let high = Candidate {
            priority: 20,
            ..candidate(1, Some(1), &[])
        };
        let low = candidate(2, Some(2), &[]);
        let selection = Selection::default().with_cooldown(0);
        assert!(selection.weight(&high) > selection.weight(&low));
    }

    #[test]
    fn recent_tags_are_penalised() {
        let fresh = candidate(1, Some(1), &["landscape"]);
        let stale = candidate(2, Some(2), &["portrait"]);
        let selection = Selection::default().with_history(vec![], vec!["portrait".into()]);
        assert!(selection.weight(&fresh) > selection.weight(&stale));
    }

    #[test]
    fn simulation_is_deterministic_and_covers_the_queue() {
        let candidates: Vec<Candidate> = (1..=5).map(|i| candidate(i, Some(i), &[])).collect();
        let selection = Selection::default();
        let first = selection.simulate(&candidates, 42, 5);
        let second = selection.simulate(&candidates, 42, 5);
        assert_eq!(first, second);
        assert_eq!(first.len(), 5);
        let mut sorted = first.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            vec![1, 2, 3, 4, 5],
            "each item appears exactly once"
        );
    }

    #[test]
    fn simulation_spreads_authors() {
        let candidates = vec![
            Candidate {
                priority: 50,
                ..candidate(1, Some(1), &[])
            },
            Candidate {
                priority: 50,
                ..candidate(2, Some(1), &[])
            },
            candidate(3, Some(2), &[]),
        ];
        let order = Selection::default().simulate(&candidates, 3, 3);
        // the two posts by author 1 must not be adjacent
        let first_author_one = order.iter().position(|q| *q == 1 || *q == 2).unwrap();
        assert_ne!(order.get(first_author_one + 1), Some(&1));
    }
}

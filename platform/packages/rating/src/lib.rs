//! BotStadia's one rating implementation, used everywhere a rating is
//! computed: `botstadia bench`, the keeper and the enclave's warm-ups. Each
//! week's field freezes its ratings, which seed round 1 and break ties in the
//! standings (`botstadia-standings`).
//!
//! The model is OpenSkill's Plackett-Luce (Weng and Lin, 2011, algorithm 4),
//! from the TrueSkill/Plackett-Luce family, for any number of entrants and
//! for ties. A rating is a mean and an uncertainty. Every operation uses
//! `libm`'s pure-Rust maths, pinned, and every result is rounded to
//! [`PRECISION`], so a recomputation matches exactly on any machine.

/// Rating version 1's constants.
pub const VERSION: u32 = 1;
pub const INITIAL_MU: f64 = 25.0;
pub const INITIAL_SIGMA: f64 = 25.0 / 3.0;
/// The performance spread.
pub const BETA: f64 = INITIAL_SIGMA / 2.0;
/// Additive dynamics: each uncertainty widens by this before a session, so a
/// rating never stops moving. OpenSkill's default.
pub const TAU: f64 = 25.0 / 300.0;
/// The floor under an uncertainty's shrink factor.
pub const KAPPA: f64 = 0.0001;
/// Ratings are kept in millionths.
pub const PRECISION: f64 = 1_000_000.0;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Rating {
    pub mu: f64,
    pub sigma: f64,
}

impl Default for Rating {
    fn default() -> Self {
        Self {
            mu: INITIAL_MU,
            sigma: INITIAL_SIGMA,
        }
    }
}

impl Rating {
    /// The conservative single number shown on leaderboards: μ − 3σ.
    pub fn ordinal(&self) -> f64 {
        quantize(self.mu - 3.0 * self.sigma)
    }
}

fn quantize(value: f64) -> f64 {
    libm::round(value * PRECISION) / PRECISION
}

/// New ratings after one session. `ratings[i]` is entrant `i`'s rating
/// before, and `places` lists entrants from first to last, each place holding
/// every entrant that shares it (a tie or draw).
pub fn rate(ratings: &[Rating], places: &[Vec<usize>]) -> Vec<Rating> {
    let count = ratings.len();
    let mut rank = vec![usize::MAX; count];
    for (place, entrants) in places.iter().enumerate() {
        for &entrant in entrants {
            if entrant < count {
                rank[entrant] = place;
            }
        }
    }
    if rank.contains(&usize::MAX) || count < 2 {
        return ratings.to_vec();
    }
    let ratings: Vec<Rating> = ratings
        .iter()
        .map(|r| Rating {
            mu: r.mu,
            sigma: libm::sqrt(r.sigma * r.sigma + TAU * TAU),
        })
        .collect();
    let beta_sq = BETA * BETA;
    let c = libm::sqrt(
        ratings
            .iter()
            .map(|r| r.sigma * r.sigma + beta_sq)
            .sum::<f64>(),
    );
    let strength: Vec<f64> = ratings.iter().map(|r| libm::exp(r.mu / c)).collect();
    // For each entrant q: the strengths of everyone placed at or below q, and
    // how many share q's place.
    let below: Vec<f64> = (0..count)
        .map(|q| {
            (0..count)
                .filter(|i| rank[*i] >= rank[q])
                .map(|i| strength[i])
                .sum()
        })
        .collect();
    let sharing: Vec<f64> = (0..count)
        .map(|q| (0..count).filter(|i| rank[*i] == rank[q]).count() as f64)
        .collect();
    (0..count)
        .map(|i| {
            let mut omega = 0.0;
            let mut delta = 0.0;
            for q in 0..count {
                if rank[q] > rank[i] {
                    continue;
                }
                let quotient = strength[i] / below[q];
                omega += if q == i { 1.0 - quotient } else { -quotient } / sharing[q];
                delta += quotient * (1.0 - quotient) / sharing[q];
            }
            let sigma_sq = ratings[i].sigma * ratings[i].sigma;
            let gamma = ratings[i].sigma / c;
            omega *= sigma_sq / c;
            delta *= gamma * sigma_sq / (c * c);
            Rating {
                mu: quantize(ratings[i].mu + omega),
                sigma: quantize(ratings[i].sigma * libm::sqrt((1.0 - delta).max(KAPPA))),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_win_moves_both_and_shrinks_uncertainty() {
        let after = rate(&[Rating::default(), Rating::default()], &[vec![0], vec![1]]);
        assert!(after[0].mu > INITIAL_MU && after[1].mu < INITIAL_MU);
        assert!((after[0].mu - INITIAL_MU + after[1].mu - INITIAL_MU).abs() < 1e-6);
        assert!(after[0].sigma < INITIAL_SIGMA);
        // What the openskill package's PlackettLuce() gives.
        assert!((after[0].mu - 27.635389).abs() < 1e-5, "{after:?}");
        assert!((after[0].sigma - 8.065901).abs() < 1e-5, "{after:?}");
    }

    #[test]
    fn a_draw_between_equals_leaves_means() {
        let after = rate(&[Rating::default(), Rating::default()], &[vec![0, 1]]);
        assert!((after[0].mu - INITIAL_MU).abs() < 1e-9);
        assert!(after[0].sigma < INITIAL_SIGMA);
    }

    #[test]
    fn a_race_orders_by_place() {
        let before = vec![Rating::default(); 4];
        let after = rate(&before, &[vec![2], vec![0], vec![3], vec![1]]);
        assert!(
            after[2].mu > after[0].mu && after[0].mu > after[3].mu && after[3].mu > after[1].mu
        );
        // The openskill package, for the same race.
        for (seat, mu, sigma) in [
            (2, 27.795253, 8.263572),
            (0, 26.552918, 8.179618),
            (3, 24.689416, 8.084128),
            (1, 20.962413, 8.084128),
        ] {
            assert!(
                (after[seat].mu - mu).abs() < 1e-5 && (after[seat].sigma - sigma).abs() < 1e-5,
                "{seat}: {:?}",
                after[seat]
            );
        }
    }

    #[test]
    fn results_are_quantized() {
        let after = rate(&[Rating::default(), Rating::default()], &[vec![1], vec![0]]);
        for rating in after {
            assert_eq!(rating.mu, quantize(rating.mu));
        }
    }
}

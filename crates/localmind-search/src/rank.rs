//! Deterministic, offline ranking.
//!
//! Three signals combine in a weighted sum: structural (graph proximity or
//! extraction confidence), temporal (half-life recency decay), and intent
//! (query-term match strength). No model, no network; the same inputs always
//! produce the same order.

use crate::SearchError;

/// Weights for the signal blend. Must each be in `0..=1` and sum to 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SearchWeights {
    pub structural: f32,
    pub temporal: f32,
    pub intent: f32,
}

impl SearchWeights {
    pub fn new(structural: f32, temporal: f32, intent: f32) -> Result<Self, SearchError> {
        let parts = [structural, temporal, intent];
        if parts.iter().any(|part| !(0.0..=1.0).contains(part)) {
            return Err(SearchError::InvalidWeights {
                detail: "each weight must be between 0.0 and 1.0".to_string(),
            });
        }
        let sum: f32 = parts.iter().sum();
        if (sum - 1.0).abs() > 0.001 {
            return Err(SearchError::InvalidWeights {
                detail: format!("weights must sum to 1.0, got {sum}"),
            });
        }
        Ok(Self {
            structural,
            temporal,
            intent,
        })
    }
}

impl Default for SearchWeights {
    fn default() -> Self {
        Self {
            structural: 0.4,
            temporal: 0.3,
            intent: 0.3,
        }
    }
}

/// Knobs for one ranked search.
#[derive(Clone, Debug)]
pub struct RankingConfig {
    pub weights: SearchWeights,
    /// Days after which the temporal score halves.
    pub half_life_days: f32,
    /// Per-hop multiplier for the focus-node neighbor boost (`0..=1`).
    pub neighbor_falloff: f32,
    /// How far the focus neighborhood reaches.
    pub max_hops: u32,
}

impl Default for RankingConfig {
    fn default() -> Self {
        Self {
            weights: SearchWeights::default(),
            half_life_days: 30.0,
            neighbor_falloff: 0.5,
            max_hops: 2,
        }
    }
}

/// `exp(-λ · age_days)` with `λ = ln 2 / half_life_days`: edited now ≈ 1.0,
/// edited one half-life ago = 0.5, long ago → 0. Unknown ages score 0.
#[must_use]
pub fn temporal_score(age_days: Option<f32>, half_life_days: f32) -> f32 {
    let Some(age_days) = age_days else {
        return 0.0;
    };
    if age_days <= 0.0 {
        return 1.0;
    }
    let lambda = std::f32::consts::LN_2 / half_life_days.max(f32::EPSILON);
    (-lambda * age_days).exp()
}

/// Focus-node proximity: the focus itself scores 1.0 and each hop multiplies
/// by the falloff. Outside the neighborhood scores 0.
#[must_use]
pub fn proximity_score(hops: Option<u32>, falloff: f32) -> f32 {
    match hops {
        Some(hops) => falloff.clamp(0.0, 1.0).powi(hops as i32),
        None => 0.0,
    }
}

/// The weighted blend of the three signals.
#[must_use]
pub fn combined_score(structural: f32, temporal: f32, intent: f32, weights: SearchWeights) -> f32 {
    weights.structural * structural + weights.temporal * temporal + weights.intent * intent
}

#[cfg(test)]
mod tests {
    use super::{combined_score, proximity_score, temporal_score, SearchWeights};

    #[test]
    fn temporal_score_halves_at_each_half_life() {
        let cases: &[(Option<f32>, f32, f32)] = &[
            (Some(0.0), 30.0, 1.0),
            (Some(30.0), 30.0, 0.5),
            (Some(60.0), 30.0, 0.25),
            (Some(7.0), 7.0, 0.5),
            (None, 30.0, 0.0),
        ];
        for (age, half_life, expected) in cases {
            let score = temporal_score(*age, *half_life);
            assert!(
                (score - expected).abs() < 0.001,
                "age {age:?} half-life {half_life} expected {expected}, got {score}"
            );
        }
    }

    #[test]
    fn fresher_items_always_outrank_staler_ones() {
        let fresh = temporal_score(Some(1.0), 30.0);
        let stale = temporal_score(Some(90.0), 30.0);
        assert!(fresh > stale);
    }

    #[test]
    fn proximity_decays_per_hop_and_dies_outside_the_neighborhood() {
        assert!((proximity_score(Some(0), 0.5) - 1.0).abs() < f32::EPSILON);
        assert!((proximity_score(Some(1), 0.5) - 0.5).abs() < f32::EPSILON);
        assert!((proximity_score(Some(2), 0.5) - 0.25).abs() < f32::EPSILON);
        assert!(proximity_score(None, 0.5) < f32::EPSILON);
    }

    #[test]
    fn weights_validate_range_and_sum() {
        assert!(SearchWeights::new(0.4, 0.3, 0.3).is_ok());
        assert!(SearchWeights::new(0.8, 0.3, 0.3).is_err());
        assert!(SearchWeights::new(-0.1, 0.6, 0.5).is_err());
    }

    #[test]
    fn combined_score_orders_by_the_dominant_signal() -> Result<(), Box<dyn std::error::Error>> {
        let intent_heavy = SearchWeights::new(0.1, 0.1, 0.8)?;
        let strong_intent = combined_score(0.2, 0.2, 0.9, intent_heavy);
        let weak_intent = combined_score(0.9, 0.9, 0.1, intent_heavy);
        assert!(strong_intent > weak_intent);
        Ok(())
    }
}

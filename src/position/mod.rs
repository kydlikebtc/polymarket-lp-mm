use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::{debug, info, warn};

use crate::config::PositionConfig;
use crate::data::state::{PositionRecord, SharedState};

/// Action determined by position manager.
/// Note: Quote skewing and size adjustment are handled directly by PricingEngine
/// via its own compute_skew(iir) — only merge and risk-escalation actions live here.
#[derive(Debug, Clone)]
pub enum PositionAction {
    /// Trigger merge: combine YES + NO into USDC
    TriggerMerge {
        market_id: String,
        amount: Decimal,
    },
    /// Escalate to L2 risk level
    EscalateL2 { market_id: String, iir: Decimal },
    /// Escalate to L3 risk level
    EscalateL3 { market_id: String, iir: Decimal },
}

pub struct PositionManager {
    config: PositionConfig,
    /// Last merge timestamp per market for cooldown enforcement
    last_merge_times: HashMap<String, DateTime<Utc>>,
}

impl PositionManager {
    pub fn new(config: &PositionConfig) -> Self {
        Self {
            config: config.clone(),
            last_merge_times: HashMap::new(),
        }
    }

    /// Record that a merge was executed for a market
    pub fn record_merge(&mut self, market_id: &str) {
        self.last_merge_times.insert(market_id.to_string(), Utc::now());
    }

    /// Check if merge cooldown has elapsed for a market
    fn can_merge(&self, market_id: &str) -> bool {
        match self.last_merge_times.get(market_id) {
            None => true,
            Some(last) => {
                let elapsed = (Utc::now() - *last).num_seconds().max(0) as u64;
                elapsed >= self.config.merge_cooldown_secs
            }
        }
    }

    /// Evaluate position for a single market, return recommended actions
    pub fn evaluate(&self, position: &PositionRecord) -> Vec<PositionAction> {
        let mut actions = Vec::new();
        let iir = position.iir();
        let abs_iir = iir.abs();

        debug!(
            "Position eval: market={}, YES={}, NO={}, IIR={iir}",
            position.market_id, position.yes_shares, position.no_shares
        );

        // Check for merge opportunity first (risk-free), respecting cooldown
        let mergeable = position.mergeable_amount();
        if mergeable >= self.config.min_merge_size && self.can_merge(&position.market_id) {
            info!(
                "Merge opportunity: market={}, amount={mergeable}",
                position.market_id
            );
            actions.push(PositionAction::TriggerMerge {
                market_id: position.market_id.clone(),
                amount: mergeable,
            });
        } else if mergeable >= self.config.min_merge_size {
            debug!(
                "Merge cooldown active for market={}, skipping",
                position.market_id
            );
        }

        // IIR-based actions
        if abs_iir >= self.config.iir_extreme_threshold {
            // |IIR| >= 0.75 → L3
            warn!(
                "Extreme IIR detected: market={}, IIR={iir}",
                position.market_id
            );
            actions.push(PositionAction::EscalateL3 {
                market_id: position.market_id.clone(),
                iir,
            });
        } else if abs_iir >= self.config.iir_medium_threshold {
            // |IIR| >= 0.5 → L2
            warn!(
                "High IIR detected: market={}, IIR={iir}",
                position.market_id
            );
            actions.push(PositionAction::EscalateL2 {
                market_id: position.market_id.clone(),
                iir,
            });
        }
        // Note: lighter IIR levels (|IIR| < 0.5) are handled automatically
        // by PricingEngine::compute_skew(iir) — no action needed here.

        actions
    }

    /// Update position values based on current midpoint
    pub fn update_position_values(
        &self,
        state: &SharedState,
        market_id: &str,
        midpoint: Decimal,
    ) {
        if let Some(mut pos) = state.positions.get_mut(market_id) {
            pos.yes_value = pos.yes_shares * midpoint;
            pos.no_value = pos.no_shares * (dec!(1.0) - midpoint);
            pos.updated_at = chrono::Utc::now();
        }
    }
}

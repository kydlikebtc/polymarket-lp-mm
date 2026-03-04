use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use tracing::debug;

use crate::config::{MarketConfig, PricingConfig};
use crate::data::state::{OrderSide, SharedState};
use crate::risk::RiskLevel;

/// A single order to be placed
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct QuoteOrder {
    pub market_id: String,
    pub token_id: String,
    pub side: OrderSide,
    pub price: Decimal,
    pub size: Decimal,
    pub layer: usize,
}

pub struct PricingEngine {
    config: PricingConfig,
}

impl PricingEngine {
    pub fn new(config: &PricingConfig) -> Self {
        Self {
            config: config.clone(),
        }
    }

    /// Generate full set of ladder orders for a single market.
    ///
    /// Returns (bid_orders, ask_orders) for all layers.
    pub fn generate_quotes(
        &self,
        market: &MarketConfig,
        midpoint: Decimal,
        iir: Decimal,
        vaf: Decimal,
        tf: Decimal,
        per_market_capital: Decimal,
        risk_level: RiskLevel,
        available_yes_shares: Decimal,
    ) -> Vec<QuoteOrder> {
        let mut orders = Vec::new();

        // Risk-level multipliers
        let (size_mult, spread_mult) = match risk_level {
            RiskLevel::L1Normal => (dec!(1.0), dec!(1.0)),
            RiskLevel::L2Warning => (dec!(0.5), dec!(1.5)),
            RiskLevel::L3Emergency => return orders, // No orders in L3
        };

        // Quote skewing: shift both bid and ask in same direction
        let skew = self.compute_skew(iir);

        // Track remaining sellable inventory across all layers
        let mut remaining_ask_shares = available_yes_shares;

        for (i, layer) in self.config.layers.iter().enumerate() {
            let effective_distance = layer.distance * vaf * tf * spread_mult;
            let layer_capital = per_market_capital * layer.capital_fraction * size_mult;

            // Ensure orders stay within [0.01, 0.99]
            let bid_price = (midpoint - effective_distance + skew)
                .max(dec!(0.01))
                .min(dec!(0.99));
            let ask_price = (midpoint + effective_distance + skew)
                .max(dec!(0.01))
                .min(dec!(0.99));

            // Size = capital / price (how many shares we can afford)
            let bid_size = if bid_price > Decimal::ZERO {
                (layer_capital / bid_price).round_dp(2)
            } else {
                Decimal::ZERO
            };
            let raw_ask_size = if ask_price > Decimal::ZERO {
                (layer_capital / ask_price).round_dp(2)
            } else {
                Decimal::ZERO
            };

            // Clamp ask_size to available YES shares to prevent selling more than we own
            let ask_size = raw_ask_size.min(remaining_ask_shares).max(Decimal::ZERO);

            if bid_size > Decimal::ZERO {
                orders.push(QuoteOrder {
                    market_id: market.market_id.clone(),
                    token_id: market.token_id.clone(),
                    side: OrderSide::Buy,
                    price: bid_price.round_dp(2),
                    size: bid_size,
                    layer: i,
                });
            }

            if ask_size > Decimal::ZERO {
                orders.push(QuoteOrder {
                    market_id: market.market_id.clone(),
                    token_id: market.token_id.clone(),
                    side: OrderSide::Sell,
                    price: ask_price.round_dp(2),
                    size: ask_size,
                    layer: i,
                });
                remaining_ask_shares -= ask_size;
            }
        }

        debug!(
            "Generated {} quotes for market={}, mid={midpoint}, iir={iir}, vaf={vaf}, tf={tf}",
            orders.len(),
            market.market_id
        );

        orders
    }

    /// Compute Quote Skewing based on IIR.
    /// Positive IIR (too much YES) → negative skew (shift prices down to sell YES easier)
    fn compute_skew(&self, iir: Decimal) -> Decimal {
        // skew = -IIR × skew_factor
        // Negative because we want to shift prices opposite to our imbalance
        let raw_skew = -iir * self.config.skew_factor;

        // Clamp to reasonable range (max 3 cents shift)
        raw_skew.max(dec!(-0.03)).min(dec!(0.03))
    }

    /// Compute Volatility Adjustment Factor (VAF)
    pub fn compute_vaf(&self, state: &SharedState, market_id: &str) -> Decimal {
        let history = state.price_history.get(market_id);
        let Some(history) = history else {
            return dec!(1.0);
        };

        let now = chrono::Utc::now();

        // Recent volatility: std dev of 1-hour 5-min intervals
        let one_hour_ago = now - chrono::Duration::hours(1);
        let recent_prices: Vec<Decimal> = history
            .iter()
            .filter(|p| p.timestamp >= one_hour_ago)
            .map(|p| p.price)
            .collect();

        if recent_prices.len() < 3 {
            return dec!(1.0); // Not enough data
        }

        let recent_vol = compute_std_dev(&recent_prices);

        // Baseline volatility: use 0.025 as default (2.5% daily)
        // In production, this should be calculated from 7-day history
        let baseline_vol = dec!(0.025);

        if baseline_vol.is_zero() {
            return dec!(1.0);
        }

        let raw_vaf = recent_vol / baseline_vol;

        // Clamp
        raw_vaf
            .max(self.config.vaf_min)
            .min(self.config.vaf_max)
    }

    /// Compute Time Factor (TF) based on hours to settlement
    pub fn compute_tf(&self, hours_to_settlement: Option<f64>) -> Decimal {
        let Some(h) = hours_to_settlement else {
            return dec!(1.0); // No settlement info
        };

        if h <= 2.0 {
            dec!(0.0) // Stop market making (will be caught by caller)
        } else if h <= 6.0 {
            dec!(3.0)
        } else if h <= 12.0 {
            dec!(2.0)
        } else if h <= 24.0 {
            dec!(1.5)
        } else {
            dec!(1.0)
        }
    }

    /// Estimate Q-Score for a set of orders
    pub fn estimate_qscore(
        &self,
        orders: &[QuoteOrder],
        midpoint: Decimal,
        max_spread: Decimal,
    ) -> Decimal {
        if max_spread.is_zero() {
            return Decimal::ZERO;
        }

        let mut total_q = Decimal::ZERO;

        for order in orders {
            let distance = (order.price - midpoint).abs();
            if distance >= max_spread {
                continue;
            }

            // Q = ((max_spread - distance) / max_spread)^2 * size
            let ratio = (max_spread - distance) / max_spread;
            let score = ratio * ratio * order.size;
            total_q += score;
        }

        // Dual-side bonus: if both BID and ASK exist, no penalty
        // Single-side penalty: ÷3
        let has_bids = orders.iter().any(|o| matches!(o.side, OrderSide::Buy));
        let has_asks = orders.iter().any(|o| matches!(o.side, OrderSide::Sell));

        if has_bids && has_asks {
            total_q
        } else {
            total_q / dec!(3.0)
        }
    }
}

/// Simple standard deviation calculation
fn compute_std_dev(values: &[Decimal]) -> Decimal {
    if values.len() < 2 {
        return Decimal::ZERO;
    }

    let n = Decimal::from(values.len() as u64);
    let mean = values.iter().copied().sum::<Decimal>() / n;

    let variance = values
        .iter()
        .map(|v| {
            let diff = *v - mean;
            diff * diff
        })
        .sum::<Decimal>()
        / (n - dec!(1.0));

    // Approximate sqrt via Newton's method (good enough for our purposes)
    decimal_sqrt(variance)
}

/// Approximate square root for Decimal using Newton's method
fn decimal_sqrt(x: Decimal) -> Decimal {
    if x <= Decimal::ZERO {
        return Decimal::ZERO;
    }

    let mut guess = x / dec!(2.0);
    for _ in 0..20 {
        if guess.is_zero() {
            return Decimal::ZERO;
        }
        let new_guess = (guess + x / guess) / dec!(2.0);
        if (new_guess - guess).abs() < dec!(0.000001) {
            return new_guess;
        }
        guess = new_guess;
    }
    guess
}

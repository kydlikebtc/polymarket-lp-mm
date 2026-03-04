use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use std::collections::HashMap;
use tracing::{info, warn};

use crate::config::RiskConfig;

/// Risk levels: L1 (Normal), L2 (Warning), L3 (Emergency)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    L1Normal,
    L2Warning,
    L3Emergency,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::L1Normal => write!(f, "L1-Normal"),
            Self::L2Warning => write!(f, "L2-Warning"),
            Self::L3Emergency => write!(f, "L3-Emergency"),
        }
    }
}

/// Reason for risk level change
#[derive(Debug, Clone)]
pub enum RiskTrigger {
    IirExceeded { market_id: String, iir: Decimal },
    PriceJump { market_id: String, change_pct: Decimal },
    DailyLoss { loss_pct: Decimal },
    WsDisconnect { duration_secs: u64 },
    GhostFills { count: u32 },
    L2Timeout { duration_secs: u64 },
    ManualRecovery,
    ConditionsRecovered,
}

impl std::fmt::Display for RiskTrigger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IirExceeded { market_id, iir } => {
                write!(f, "IIR exceeded: market={market_id}, IIR={iir}")
            }
            Self::PriceJump { market_id, change_pct } => {
                write!(f, "Price jump: market={market_id}, change={change_pct}")
            }
            Self::DailyLoss { loss_pct } => write!(f, "Daily loss: {loss_pct}%"),
            Self::WsDisconnect { duration_secs } => {
                write!(f, "WS disconnect: {duration_secs}s")
            }
            Self::GhostFills { count } => write!(f, "Ghost fills detected: {count}"),
            Self::L2Timeout { duration_secs } => {
                write!(f, "L2 timeout: {duration_secs}s")
            }
            Self::ManualRecovery => write!(f, "Manual recovery"),
            Self::ConditionsRecovered => write!(f, "Conditions recovered"),
        }
    }
}

pub struct RiskController {
    config: RiskConfig,
    level: RiskLevel,
    l2_entered_at: Option<DateTime<Utc>>,
    l2_recovery_started: Option<DateTime<Utc>>,
    /// Ghost fill tracking: timestamps of detected ghost fills
    ghost_fill_times: Vec<DateTime<Utc>>,
    /// Track cancel requests we initiated (order_id → timestamp)
    our_cancel_requests: HashMap<String, DateTime<Utc>>,
    /// Daily PnL tracking
    daily_pnl: Decimal,
    total_capital: Decimal,
}

impl RiskController {
    pub fn new(config: &RiskConfig) -> Self {
        Self {
            config: config.clone(),
            level: RiskLevel::L1Normal,
            l2_entered_at: None,
            l2_recovery_started: None,
            ghost_fill_times: Vec::new(),
            our_cancel_requests: HashMap::new(),
            daily_pnl: Decimal::ZERO,
            total_capital: Decimal::ZERO,
        }
    }

    pub fn set_total_capital(&mut self, capital: Decimal) {
        self.total_capital = capital;
    }

    pub fn level(&self) -> RiskLevel {
        self.level
    }

    /// Register a cancel request we initiated
    pub fn register_cancel(&mut self, order_id: String) {
        self.our_cancel_requests.insert(order_id, Utc::now());
    }

    /// Check if a cancel event was initiated by us
    pub fn is_our_cancel(&mut self, order_id: &str) -> bool {
        self.our_cancel_requests.remove(order_id).is_some()
    }

    /// Record a ghost fill event
    pub fn record_ghost_fill(&mut self) {
        let now = Utc::now();
        self.ghost_fill_times.push(now);

        // Clean old entries outside the window
        let window_start = now
            - chrono::Duration::seconds(self.config.l3_ghost_fill_window_secs as i64);
        self.ghost_fill_times.retain(|t| *t >= window_start);

        let count = self.ghost_fill_times.len() as u32;
        warn!("Ghost fill detected! Count in window: {count}");

        if count >= self.config.l3_ghost_fill_count {
            self.transition_to(
                RiskLevel::L3Emergency,
                RiskTrigger::GhostFills { count },
            );
        }
    }

    /// Update daily PnL
    pub fn update_pnl(&mut self, pnl: Decimal) {
        self.daily_pnl = pnl;
    }

    /// Main evaluation: check all conditions and return current risk level.
    /// Call this periodically (every 5-10 seconds).
    pub fn evaluate(
        &mut self,
        market_iirs: &[(String, Decimal)],
        price_changes_5min: &[(String, Decimal)],
        ws_disconnect_secs: u64,
    ) -> RiskLevel {
        // If already L3, only manual recovery can change it
        if self.level == RiskLevel::L3Emergency {
            return self.level;
        }

        // Check L3 conditions first (highest priority)
        for (market_id, iir) in market_iirs {
            if iir.abs() >= self.config.l3_iir_threshold {
                self.transition_to(
                    RiskLevel::L3Emergency,
                    RiskTrigger::IirExceeded {
                        market_id: market_id.clone(),
                        iir: *iir,
                    },
                );
                return self.level;
            }
        }

        for (market_id, change) in price_changes_5min {
            if change.abs() >= self.config.l3_price_change_5min {
                self.transition_to(
                    RiskLevel::L3Emergency,
                    RiskTrigger::PriceJump {
                        market_id: market_id.clone(),
                        change_pct: *change,
                    },
                );
                return self.level;
            }
        }

        if self.total_capital > Decimal::ZERO {
            let loss_pct = (self.daily_pnl / self.total_capital).abs();
            if self.daily_pnl < Decimal::ZERO && loss_pct >= self.config.l3_daily_loss_pct {
                self.transition_to(
                    RiskLevel::L3Emergency,
                    RiskTrigger::DailyLoss { loss_pct },
                );
                return self.level;
            }
        }

        // L2 timeout → L3
        if self.level == RiskLevel::L2Warning {
            if let Some(entered_at) = self.l2_entered_at {
                let duration = (Utc::now() - entered_at).num_seconds() as u64;
                if duration >= self.config.l2_timeout_to_l3_secs {
                    self.transition_to(
                        RiskLevel::L3Emergency,
                        RiskTrigger::L2Timeout {
                            duration_secs: duration,
                        },
                    );
                    return self.level;
                }
            }
        }

        // Check L2 conditions
        let mut should_be_l2 = false;
        let mut l2_trigger = None;

        for (market_id, iir) in market_iirs {
            if iir.abs() >= self.config.l2_iir_threshold {
                should_be_l2 = true;
                l2_trigger = Some(RiskTrigger::IirExceeded {
                    market_id: market_id.clone(),
                    iir: *iir,
                });
                break;
            }
        }

        if !should_be_l2 {
            for (market_id, change) in price_changes_5min {
                if change.abs() >= self.config.l2_price_change_5min {
                    should_be_l2 = true;
                    l2_trigger = Some(RiskTrigger::PriceJump {
                        market_id: market_id.clone(),
                        change_pct: *change,
                    });
                    break;
                }
            }
        }

        if !should_be_l2 && self.total_capital > Decimal::ZERO {
            let loss_pct = (self.daily_pnl / self.total_capital).abs();
            if self.daily_pnl < Decimal::ZERO && loss_pct >= self.config.l2_daily_loss_pct {
                should_be_l2 = true;
                l2_trigger = Some(RiskTrigger::DailyLoss { loss_pct });
            }
        }

        if !should_be_l2 && ws_disconnect_secs >= self.config.l2_ws_disconnect_secs {
            should_be_l2 = true;
            l2_trigger = Some(RiskTrigger::WsDisconnect {
                duration_secs: ws_disconnect_secs,
            });
        }

        if should_be_l2 && self.level == RiskLevel::L1Normal {
            self.transition_to(RiskLevel::L2Warning, l2_trigger.unwrap());
            return self.level;
        }

        // L2 → L1 recovery check
        if self.level == RiskLevel::L2Warning && !should_be_l2 {
            self.check_l2_recovery(market_iirs, price_changes_5min);
        }

        self.level
    }

    fn check_l2_recovery(
        &mut self,
        market_iirs: &[(String, Decimal)],
        price_changes_5min: &[(String, Decimal)],
    ) {
        // All IIRs below recovery threshold
        let iir_ok = market_iirs
            .iter()
            .all(|(_, iir)| iir.abs() < self.config.l2_recovery_iir);

        // All price changes below recovery threshold
        let price_ok = price_changes_5min
            .iter()
            .all(|(_, change)| change.abs() < self.config.l2_recovery_price_change);

        // Daily PnL must be above L2 loss threshold (recovery condition)
        let pnl_ok = if self.total_capital > Decimal::ZERO {
            let loss_pct = (self.daily_pnl / self.total_capital).abs();
            !(self.daily_pnl < Decimal::ZERO && loss_pct >= self.config.l2_daily_loss_pct)
        } else {
            true
        };

        if iir_ok && price_ok && pnl_ok {
            match self.l2_recovery_started {
                None => {
                    self.l2_recovery_started = Some(Utc::now());
                    info!("L2 recovery conditions met, starting hold period");
                }
                Some(started) => {
                    let hold_secs = (Utc::now() - started).num_seconds() as u64;
                    if hold_secs >= self.config.l2_recovery_hold_secs {
                        self.transition_to(
                            RiskLevel::L1Normal,
                            RiskTrigger::ConditionsRecovered,
                        );
                    }
                }
            }
        } else {
            // Conditions not met, reset recovery timer
            if self.l2_recovery_started.is_some() {
                info!("L2 recovery conditions no longer met, resetting timer");
                self.l2_recovery_started = None;
            }
        }
    }

    /// Manual recovery from L3 (must be called by human operator)
    pub fn manual_recover(&mut self) {
        if self.level == RiskLevel::L3Emergency {
            self.transition_to(RiskLevel::L1Normal, RiskTrigger::ManualRecovery);
        }
    }

    fn transition_to(&mut self, new_level: RiskLevel, trigger: RiskTrigger) {
        let old_level = self.level;
        self.level = new_level;

        match new_level {
            RiskLevel::L2Warning => {
                self.l2_entered_at = Some(Utc::now());
                self.l2_recovery_started = None;
                warn!("RISK LEVEL: {old_level} → {new_level} | Trigger: {trigger}");
            }
            RiskLevel::L3Emergency => {
                self.l2_entered_at = None;
                self.l2_recovery_started = None;
                warn!("RISK LEVEL: {old_level} → {new_level} | Trigger: {trigger}");
            }
            RiskLevel::L1Normal => {
                self.l2_entered_at = None;
                self.l2_recovery_started = None;
                self.ghost_fill_times.clear();
                info!("RISK LEVEL: {old_level} → {new_level} | Trigger: {trigger}");
            }
        }
    }
}

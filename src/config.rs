use clap::Parser;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(author, version, about = "Kalshi 15-minute BTC trading bot")]
pub struct Args {
    #[arg(short, long, default_value_t = true)]
    pub simulation: bool,

    #[arg(long)]
    pub production: bool,

    #[arg(short, long, default_value = "config.json")]
    pub config: PathBuf,
}

impl Args {
    pub fn is_simulation(&self) -> bool {
        if self.production {
            false
        } else {
            self.simulation
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub kalshi: KalshiConfig,
    pub trading: TradingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiConfig {
    /// REST base URL. Demo: https://demo-api.kalshi.co/trade-api/v2
    /// Production: https://trading-api.kalshi.com/trade-api/v2
    pub base_url: String,
    /// Key ID (UUID) from Kalshi dashboard → Profile → API Keys
    pub key_id: String,
    /// Path to your RSA private key PEM file (e.g. "./kalshi_private_key.pem")
    pub private_key_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradingConfig {
    pub check_interval_ms: u64,
    #[serde(default = "default_market_closure_check_interval")]
    pub market_closure_check_interval_seconds: u64,
    #[serde(default = "default_data_source")]
    pub data_source: String,
    #[serde(default = "default_markets")]
    pub markets: Vec<String>,
    /// Timeframes to trade: ["15m"] for Kalshi BTC 15-minute markets.
    #[serde(default = "default_timeframes")]
    pub timeframes: Vec<String>,
    /// Max cost per pair when locking (YES ask + NO ask <= this). e.g. 0.99.
    #[serde(default = "default_cost_per_pair_max")]
    pub cost_per_pair_max: f64,
    /// Never buy a side if its ask price is below this (e.g. 0.05).
    #[serde(default = "default_min_side_price")]
    pub min_side_price: f64,
    /// Never buy a side if its ask price is above this (e.g. 0.99).
    #[serde(default = "default_max_side_price")]
    pub max_side_price: f64,
    /// Min seconds between buys per market. 0 = condition-based only.
    #[serde(default = "default_cooldown_seconds")]
    pub cooldown_seconds: u64,
    /// Min seconds between buys for 1h markets (not used for 15m-only bot).
    #[serde(default = "default_cooldown_seconds_1h")]
    pub cooldown_seconds_1h: u64,
    /// Contracts per order; if unset uses default (BTC 15m = 24).
    pub shares: Option<f64>,
    /// Reduce order size in the last N seconds of a market. Default 300.
    #[serde(default = "default_size_reduce_after_secs")]
    pub size_reduce_after_secs: u64,
    /// Minimum size ratio when reducing. Default 0.5.
    #[serde(default = "default_size_min_ratio")]
    pub size_min_ratio: f64,
    /// Minimum contracts per order when reducing. Default 5.
    #[serde(default = "default_size_min_shares")]
    pub size_min_shares: f64,
}

fn default_market_closure_check_interval() -> u64 { 20 }
fn default_data_source() -> String { "api".to_string() }
fn default_markets() -> Vec<String> { vec!["btc".to_string()] }
fn default_timeframes() -> Vec<String> { vec!["15m".to_string()] }
fn default_cost_per_pair_max() -> f64 { 0.99 }
fn default_min_side_price() -> f64 { 0.05 }
fn default_max_side_price() -> f64 { 0.99 }
fn default_cooldown_seconds() -> u64 { 0 }
fn default_cooldown_seconds_1h() -> u64 { 45 }
fn default_size_reduce_after_secs() -> u64 { 300 }
fn default_size_min_ratio() -> f64 { 0.5 }
fn default_size_min_shares() -> f64 { 5.0 }

impl Default for Config {
    fn default() -> Self {
        Self {
            kalshi: KalshiConfig {
                base_url: "https://demo-api.kalshi.co/trade-api/v2".to_string(),
                key_id: "YOUR_KALSHI_KEY_ID".to_string(),
                private_key_path: "./kalshi_private_key.pem".to_string(),
            },
            trading: TradingConfig {
                check_interval_ms: 1000,
                market_closure_check_interval_seconds: 20,
                data_source: "api".to_string(),
                markets: vec!["btc".to_string()],
                timeframes: default_timeframes(),
                cost_per_pair_max: default_cost_per_pair_max(),
                min_side_price: default_min_side_price(),
                max_side_price: default_max_side_price(),
                cooldown_seconds: default_cooldown_seconds(),
                cooldown_seconds_1h: default_cooldown_seconds_1h(),
                shares: None,
                size_reduce_after_secs: default_size_reduce_after_secs(),
                size_min_ratio: default_size_min_ratio(),
                size_min_shares: default_size_min_shares(),
            },
        }
    }
}

impl Config {
    pub fn load(path: &PathBuf) -> anyhow::Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            let config = Config::default();
            let content = serde_json::to_string_pretty(&config)?;
            std::fs::write(path, content)?;
            Ok(config)
        }
    }
}

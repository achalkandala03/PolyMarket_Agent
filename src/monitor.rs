use crate::api::KalshiApi;
use crate::models::*;
use anyhow::Result;
use log::{debug, warn};
use std::sync::Arc;
use tokio::time::{sleep, Duration};

pub struct MarketMonitor {
    api: Arc<KalshiApi>,
    market_name: String,
    /// Current active market (ticker + timing).
    market: Arc<tokio::sync::Mutex<Market>>,
    check_interval: Duration,
}

#[derive(Debug, Clone)]
pub struct MarketSnapshot {
    pub market_name: String,
    pub btc_market_15m: MarketData,
    pub timestamp: std::time::Instant,
    /// Seconds remaining until the market closes.
    pub btc_15m_time_remaining: u64,
    /// Market open unix timestamp (used as the period key in trader.rs).
    pub btc_15m_period_timestamp: u64,
    /// Market duration in seconds (900 for 15m).
    pub market_duration_secs: u64,
}

impl MarketMonitor {
    pub fn new(
        api: Arc<KalshiApi>,
        market_name: String,
        initial_market: Market,
        check_interval_ms: u64,
    ) -> Self {
        Self {
            api,
            market_name,
            market: Arc::new(tokio::sync::Mutex::new(initial_market)),
            check_interval: Duration::from_millis(check_interval_ms),
        }
    }

    /// Replace the tracked market (called on period rollover).
    pub async fn update_market(&self, new_market: Market) {
        eprintln!(
            "Updating {} market → {}",
            self.market_name, new_market.condition_id
        );
        *self.market.lock().await = new_market;
    }

    /// Open-time unix of the current market (used as period key).
    pub async fn get_current_market_timestamp(&self) -> u64 {
        self.market.lock().await.open_time_unix
    }

    /// Ticker of the current market.
    pub async fn get_current_ticker(&self) -> String {
        self.market.lock().await.condition_id.clone()
    }

    /// Close-time unix of the current market.
    pub async fn get_close_time_unix(&self) -> u64 {
        self.market.lock().await.close_time_unix
    }

    // -----------------------------------------------------------------------
    // Fetch one snapshot
    // -----------------------------------------------------------------------

    pub async fn fetch_market_data(&self) -> Result<MarketSnapshot> {
        let (ticker, open_time, close_time) = {
            let m = self.market.lock().await;
            (
                m.condition_id.clone(),
                m.open_time_unix,
                m.close_time_unix,
            )
        };

        let now_secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let time_remaining = if close_time > now_secs {
            close_time - now_secs
        } else {
            0
        };
        // 15-minute markets: duration is always 900 s
        let market_duration_secs: u64 = 900;

        // Fetch YES and NO prices from the orderbook (single call)
        let (yes_price, no_price) = match self.api.get_orderbook_prices(&ticker).await {
            Ok(prices) => prices,
            Err(e) => {
                warn!(
                    "{}: Failed to fetch orderbook for {}: {}",
                    self.market_name, ticker, e
                );
                // Return a snapshot with no prices so the trader can skip
                return Ok(MarketSnapshot {
                    market_name: self.market_name.clone(),
                    btc_market_15m: MarketData {
                        condition_id: ticker,
                        market_name: self.market_name.clone(),
                        up_token: None,
                        down_token: None,
                    },
                    timestamp: std::time::Instant::now(),
                    btc_15m_time_remaining: time_remaining,
                    btc_15m_period_timestamp: open_time,
                    market_duration_secs,
                });
            }
        };

        // Log price line
        let fmt_price = |p: &TokenPrice| {
            let bid = p.bid.map(|d| d.to_string()).unwrap_or_else(|| "N/A".into());
            let ask = p.ask.map(|d| d.to_string()).unwrap_or_else(|| "N/A".into());
            format!("BID:${} ASK:${}", bid, ask)
        };
        let remaining_str = {
            let m = time_remaining / 60;
            let s = time_remaining % 60;
            if m > 0 {
                format!("{}m {}s", m, s)
            } else {
                format!("{}s", s)
            }
        };
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S");
        let msg = format!(
            "[{}] {} YES(Up) {} NO(Down) {} remaining time:{}\n",
            ts,
            self.market_name,
            fmt_price(&yes_price),
            fmt_price(&no_price),
            remaining_str
        );
        crate::log_to_history(&msg);

        Ok(MarketSnapshot {
            market_name: self.market_name.clone(),
            btc_market_15m: MarketData {
                condition_id: ticker,
                market_name: self.market_name.clone(),
                up_token: Some(yes_price),   // YES = Up
                down_token: Some(no_price),  // NO  = Down
            },
            timestamp: std::time::Instant::now(),
            btc_15m_time_remaining: time_remaining,
            btc_15m_period_timestamp: open_time,
            market_duration_secs,
        })
    }

    // -----------------------------------------------------------------------
    // Polling loop
    // -----------------------------------------------------------------------

    pub async fn start_monitoring<F, Fut>(&self, callback: F)
    where
        F: Fn(MarketSnapshot) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        eprintln!("{}: Starting market monitoring (REST polling)...", self.market_name);
        loop {
            match self.fetch_market_data().await {
                Ok(snapshot) => {
                    debug!("Market snapshot updated");
                    callback(snapshot).await;
                }
                Err(e) => {
                    warn!("{}: Error fetching market data: {}", self.market_name, e);
                }
            }
            sleep(self.check_interval).await;
        }
    }
}

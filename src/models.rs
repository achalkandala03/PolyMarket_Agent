use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Internal trading models (used by monitor.rs and trader.rs)
// ---------------------------------------------------------------------------

/// Represents an active Kalshi market (BTC 15m Up/Down).
/// `condition_id` holds the Kalshi ticker (e.g. "KXBTC15M-26FEB181530")
/// so that monitor.rs and trader.rs don't need to distinguish between
/// the Polymarket and Kalshi identifiers.
#[derive(Debug, Clone)]
pub struct Market {
    /// Kalshi ticker, e.g. "KXBTC15M-26FEB181530"
    pub condition_id: String,
    /// Same as condition_id — kept for interface compatibility.
    pub slug: String,
    pub question: String,
    pub active: bool,
    pub closed: bool,
    /// Unix timestamp (seconds) when the market closes.
    pub close_time_unix: u64,
    /// Unix timestamp (seconds) when the market opened.
    pub open_time_unix: u64,
}

/// YES = Up, NO = Down.
/// `token_id` is "yes" or "no" for Kalshi.
#[derive(Debug, Clone)]
pub struct TokenPrice {
    pub token_id: String,
    pub bid: Option<Decimal>,
    pub ask: Option<Decimal>,
}

impl TokenPrice {
    pub fn mid_price(&self) -> Option<Decimal> {
        match (self.bid, self.ask) {
            (Some(bid), Some(ask)) => Some((bid + ask) / Decimal::from(2)),
            (Some(bid), None) => Some(bid),
            (None, Some(ask)) => Some(ask),
            (None, None) => None,
        }
    }

    pub fn ask_price(&self) -> Decimal {
        self.ask.unwrap_or(Decimal::ZERO)
    }
}

#[derive(Debug, Clone)]
pub struct MarketData {
    /// Kalshi ticker used as the market identifier throughout the bot.
    pub condition_id: String,
    pub market_name: String,
    /// YES side (= "Up").
    pub up_token: Option<TokenPrice>,
    /// NO side (= "Down").
    pub down_token: Option<TokenPrice>,
}

// ---------------------------------------------------------------------------
// Kalshi REST API response models
// ---------------------------------------------------------------------------

/// Raw market object returned by GET /trade-api/v2/markets/{ticker}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiMarket {
    pub ticker: String,
    pub series_ticker: Option<String>,
    pub title: Option<String>,
    /// "open", "closed", "settled"
    pub status: String,
    /// "yes", "no", or "" when not yet settled
    #[serde(default)]
    pub result: String,
    pub close_time: Option<String>,
    pub open_time: Option<String>,
}

/// Wrapper returned by GET /trade-api/v2/markets/{ticker}
#[derive(Debug, Deserialize)]
pub struct KalshiMarketResponse {
    pub market: KalshiMarket,
}

/// Wrapper returned by GET /trade-api/v2/markets (list)
#[derive(Debug, Deserialize)]
pub struct KalshiMarketsListResponse {
    pub markets: Vec<KalshiMarket>,
    #[serde(default)]
    pub cursor: String,
}

/// Raw orderbook response.
/// Each entry is [price_cents (1-99), quantity].
/// The list is sorted ascending — last element = best bid.
#[derive(Debug, Deserialize)]
pub struct KalshiOrderBook {
    pub yes: Vec<[u32; 2]>,
    pub no: Vec<[u32; 2]>,
}

/// Wrapper returned by GET /trade-api/v2/markets/{ticker}/orderbook
#[derive(Debug, Deserialize)]
pub struct KalshiOrderBookResponse {
    pub orderbook: KalshiOrderBook,
}

/// Response from POST /trade-api/v2/portfolio/orders
#[derive(Debug, Deserialize)]
pub struct KalshiOrderResponse {
    pub order: KalshiOrder,
}

#[derive(Debug, Deserialize)]
pub struct KalshiOrder {
    pub order_id: Option<String>,
    /// "resting", "filled", "canceled", "rejected"
    pub status: Option<String>,
    pub filled_count: Option<u64>,
    pub remaining_count: Option<u64>,
}

/// Used by trader.rs when checking if a market has resolved.
#[derive(Debug, Clone)]
pub struct KalshiMarketDetails {
    pub ticker: String,
    /// true when status is "settled" or "closed"
    pub closed: bool,
    /// "yes", "no", or None if still open
    pub result: Option<String>,
}

/// Balance response from GET /trade-api/v2/portfolio/balance
#[derive(Debug, Deserialize)]
pub struct KalshiBalance {
    /// Available cash in cents
    pub balance: u64,
}

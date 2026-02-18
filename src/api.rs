use crate::models::*;
use anyhow::{Context, Result};
use base64::{engine::general_purpose, Engine as _};
use chrono::DateTime;
use rand::thread_rng;
use reqwest::{Client, RequestBuilder};
use rsa::pkcs8::DecodePrivateKey;
use rsa::pss::BlindedSigningKey;
use rsa::signature::{RandomizedSigner, SignatureEncoding};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sha2::Sha256;
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub struct KalshiApi {
    client: Client,
    base_url: String,
    key_id: String,
    /// None when running without credentials (should not happen in practice —
    /// Kalshi requires auth even for market data).
    signing_key: Option<BlindedSigningKey<Sha256>>,
}

impl KalshiApi {
    /// Create a new API client.
    /// `pem_path` = path to RSA PKCS#8 private key PEM file.
    /// Pass `None` for `pem_path` to run without signing (only works if you
    /// expect all requests to fail auth — not useful in practice).
    pub fn new(base_url: String, key_id: String, pem_path: Option<&str>) -> Result<Self> {
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()
            .context("Failed to create HTTP client")?;

        let signing_key = if let Some(path) = pem_path {
            let pem = std::fs::read_to_string(path)
                .with_context(|| format!("Cannot read RSA key from '{}'", path))?;
            let private_key = rsa::RsaPrivateKey::from_pkcs8_pem(&pem)
                .context("Failed to parse RSA private key PEM — make sure it is PKCS#8 format")?;
            Some(BlindedSigningKey::<Sha256>::new(private_key))
        } else {
            None
        };

        Ok(Self {
            client,
            base_url,
            key_id,
            signing_key,
        })
    }

    // -----------------------------------------------------------------------
    // RSA-PSS signing
    // -----------------------------------------------------------------------

    /// Build the three Kalshi authentication headers.
    /// `path` must be the URL path *without* query string.
    fn auth_headers(&self, method: &str, path: &str) -> Option<[(&'static str, String); 3]> {
        let sk = self.signing_key.as_ref()?;
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .to_string();
        let message = format!("{}{}{}", ts_ms, method.to_uppercase(), path);
        let sig = sk.sign_with_rng(&mut thread_rng(), message.as_bytes());
        let sig_bytes = sig.to_bytes();
        let encoded = general_purpose::STANDARD.encode(sig_bytes.as_ref());
        Some([
            ("KALSHI-ACCESS-KEY", self.key_id.clone()),
            ("KALSHI-ACCESS-TIMESTAMP", ts_ms),
            ("KALSHI-ACCESS-SIGNATURE", encoded),
        ])
    }

    /// Attach auth headers to a `RequestBuilder`. Skips headers if no key loaded.
    fn sign(&self, builder: RequestBuilder, method: &str, path: &str) -> RequestBuilder {
        if let Some(headers) = self.auth_headers(method, path) {
            headers
                .into_iter()
                .fold(builder, |b, (k, v)| b.header(k, v))
        } else {
            builder
        }
    }

    /// Build a full URL, returning also the path portion (for signing).
    fn url(&self, path: &str) -> (String, String) {
        // path starts with "/" e.g. "/markets/KXBTC15M-26FEB181530"
        // base_url ends without a trailing slash
        let full = format!("{}{}", self.base_url.trim_end_matches('/'), path);
        // For signing: strip the base host, keep everything from "/trade-api/..."
        // The base_url already contains "/trade-api/v2", so the signing path
        // is the path suffix relative to the host.
        let signing_path = if let Some(idx) = self.base_url.find("/trade-api") {
            format!("{}{}", &self.base_url[idx..], path)
        } else {
            path.to_string()
        };
        (full, signing_path)
    }

    // -----------------------------------------------------------------------
    // ISO timestamp helpers
    // -----------------------------------------------------------------------

    fn parse_iso_unix(s: &str) -> u64 {
        DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.timestamp().max(0) as u64)
            .unwrap_or(0)
    }

    fn kalshi_market_to_market(km: &KalshiMarket) -> Market {
        let close_time_unix = km
            .close_time
            .as_deref()
            .map(Self::parse_iso_unix)
            .unwrap_or(0);
        let open_time_unix = km
            .open_time
            .as_deref()
            .map(Self::parse_iso_unix)
            .unwrap_or(0);
        let active = km.status == "open";
        let closed = km.status == "settled" || km.status == "closed";
        Market {
            condition_id: km.ticker.clone(),
            slug: km.ticker.clone(),
            question: km.title.clone().unwrap_or_else(|| km.ticker.clone()),
            active,
            closed,
            close_time_unix,
            open_time_unix,
        }
    }

    // -----------------------------------------------------------------------
    // Market discovery
    // -----------------------------------------------------------------------

    /// Fetch a single market by its ticker.
    pub async fn get_market_by_ticker(&self, ticker: &str) -> Result<Market> {
        let path = format!("/markets/{}", ticker);
        let (url, signing_path) = self.url(&path);
        let req = self.sign(self.client.get(&url), "GET", &signing_path);
        let resp = req.send().await.context("GET market request failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", url, status, body);
        }
        let data: KalshiMarketResponse = resp.json().await.context("Failed to parse market JSON")?;
        Ok(Self::kalshi_market_to_market(&data.market))
    }

    /// List open markets for a series (e.g. "KXBTC15M").
    /// Returns at most `limit` markets sorted by open_time descending (newest first).
    pub async fn get_markets_for_series(&self, series_ticker: &str, status: &str) -> Result<Vec<Market>> {
        let path = "/markets";
        let query = format!(
            "?series_ticker={}&status={}&limit=5",
            series_ticker, status
        );
        let (base_url, signing_path) = self.url(path);
        let full_url = format!("{}{}", base_url, query);
        let req = self.sign(self.client.get(&full_url), "GET", &signing_path);
        let resp = req.send().await.context("GET markets list failed")?;
        let status_code = resp.status();
        if !status_code.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", full_url, status_code, body);
        }
        let data: KalshiMarketsListResponse = resp
            .json()
            .await
            .context("Failed to parse markets list JSON")?;
        Ok(data.markets.iter().map(Self::kalshi_market_to_market).collect())
    }

    /// Fetch market details for closure check (trader.rs).
    pub async fn get_market_details(&self, ticker: &str) -> Result<KalshiMarketDetails> {
        let path = format!("/markets/{}", ticker);
        let (url, signing_path) = self.url(&path);
        let req = self.sign(self.client.get(&url), "GET", &signing_path);
        let resp = req.send().await.context("GET market details failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", url, status, body);
        }
        let data: KalshiMarketResponse = resp.json().await.context("Parse market details JSON")?;
        let km = &data.market;
        let closed = km.status == "settled" || km.status == "closed";
        let result = if km.result.is_empty() {
            None
        } else {
            Some(km.result.clone())
        };
        Ok(KalshiMarketDetails {
            ticker: km.ticker.clone(),
            closed,
            result,
        })
    }

    // -----------------------------------------------------------------------
    // Orderbook / price feed
    // -----------------------------------------------------------------------

    /// Fetch the YES and NO prices for a market from the order book.
    ///
    /// Kalshi only shows bids. Asks are derived:
    ///   YES ask = 100 - best_NO_bid
    ///   NO  ask = 100 - best_YES_bid
    ///
    /// Prices are returned as floats in [0, 1] (divided by 100).
    pub async fn get_orderbook_prices(&self, ticker: &str) -> Result<(TokenPrice, TokenPrice)> {
        let path = format!("/markets/{}/orderbook", ticker);
        let (url, signing_path) = self.url(&path);
        let req = self.sign(self.client.get(&url), "GET", &signing_path);
        let resp = req.send().await.context("GET orderbook failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", url, status, body);
        }
        let data: KalshiOrderBookResponse = resp.json().await.context("Parse orderbook JSON")?;
        let ob = &data.orderbook;

        // Best bid = highest price = last element of the ascending list
        let best_yes_bid: Option<u32> = ob.yes.last().map(|e| e[0]);
        let best_no_bid: Option<u32> = ob.no.last().map(|e| e[0]);

        // Asks are derived from the opposing best bid
        let yes_ask_cents: Option<u32> = best_no_bid.map(|nb| 100 - nb);
        let no_ask_cents: Option<u32> = best_yes_bid.map(|yb| 100 - yb);

        let cents_to_dec = |c: u32| -> Decimal {
            Decimal::from_str(&format!("{:.2}", c as f64 / 100.0)).unwrap_or(Decimal::ZERO)
        };

        let yes_price = TokenPrice {
            token_id: "yes".to_string(),
            bid: best_yes_bid.map(cents_to_dec),
            ask: yes_ask_cents.map(cents_to_dec),
        };
        let no_price = TokenPrice {
            token_id: "no".to_string(),
            bid: best_no_bid.map(cents_to_dec),
            ask: no_ask_cents.map(cents_to_dec),
        };
        Ok((yes_price, no_price))
    }

    // -----------------------------------------------------------------------
    // Order placement
    // -----------------------------------------------------------------------

    /// Place a Fill-or-Kill order on Kalshi.
    ///
    /// `side`        = "yes" or "no"
    /// `count`       = number of contracts
    /// `price_float` = price as a float [0.0, 1.0] — will be rounded to nearest cent
    pub async fn place_order_fak(
        &self,
        ticker: &str,
        side: &str,   // "yes" or "no"
        count: u64,
        price_float: f64,
    ) -> Result<Value> {
        let price_cents = (price_float * 100.0).round() as u32;
        let price_cents = price_cents.clamp(1, 99);

        let client_order_id = Uuid::new_v4().to_string();
        let body = json!({
            "ticker": ticker,
            "side": side,
            "action": "buy",
            "count": count,
            "yes_price": if side == "yes" { price_cents } else { 100 - price_cents },
            "time_in_force": "fill_or_kill",
            "client_order_id": client_order_id,
            "cancel_order_on_pause": true
        });

        let path = "/portfolio/orders";
        let (url, signing_path) = self.url(path);
        let req = self
            .sign(self.client.post(&url), "POST", &signing_path)
            .json(&body);
        let resp = req.send().await.context("POST order failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body_text = resp.text().await.unwrap_or_default();
            anyhow::bail!("POST {} returned {}: {}", url, status, body_text);
        }
        let data: Value = resp.json().await.context("Parse order response JSON")?;
        Ok(data)
    }

    // -----------------------------------------------------------------------
    // Account info
    // -----------------------------------------------------------------------

    pub async fn get_balance(&self) -> Result<u64> {
        let path = "/portfolio/balance";
        let (url, signing_path) = self.url(path);
        let req = self.sign(self.client.get(&url), "GET", &signing_path);
        let resp = req.send().await.context("GET balance failed")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GET {} returned {}: {}", url, status, body);
        }
        let data: KalshiBalance = resp.json().await.context("Parse balance JSON")?;
        Ok(data.balance)
    }
}

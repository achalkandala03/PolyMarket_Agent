mod api;
mod config;
mod models;
mod monitor;
mod trader;

use anyhow::{Context, Result};
use clap::Parser;
use config::{Args, Config};
use log::warn;
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};
use std::fs::{File, OpenOptions};

use api::KalshiApi;
use monitor::MarketMonitor;
use trader::Trader;

// ---------------------------------------------------------------------------
// Dual logging (stderr + history.toml)
// ---------------------------------------------------------------------------

struct DualWriter {
    stderr: io::Stderr,
    file: Mutex<File>,
}

impl Write for DualWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let _ = self.stderr.write_all(buf);
        let _ = self.stderr.flush();
        let mut file = self.file.lock().unwrap();
        file.write_all(buf)?;
        file.flush()?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stderr.flush()?;
        let mut file = self.file.lock().unwrap();
        file.flush()?;
        Ok(())
    }
}

unsafe impl Send for DualWriter {}
unsafe impl Sync for DualWriter {}

static HISTORY_FILE: OnceLock<Mutex<File>> = OnceLock::new();

fn init_history_file(file: File) {
    HISTORY_FILE.set(Mutex::new(file)).expect("History file already initialized");
}

pub fn log_to_history(message: &str) {
    eprint!("{}", message);
    let _ = io::stderr().flush();
    if let Some(file_mutex) = HISTORY_FILE.get() {
        if let Ok(mut file) = file_mutex.lock() {
            let _ = write!(file, "{}", message);
            let _ = file.flush();
        }
    }
}

#[macro_export]
macro_rules! log_println {
    ($($arg:tt)*) => {{
        let message = format!($($arg)*);
        $crate::log_to_history(&format!("{}\n", message));
    }};
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    let history_path = "history.toml";
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)
        .context("Failed to open history.toml for logging")?;
    let log_file_for_writer = OpenOptions::new()
        .create(true)
        .append(true)
        .open(history_path)
        .context("Failed to open history.toml for writer")?;
    init_history_file(log_file);

    let dual_writer = DualWriter {
        stderr: io::stderr(),
        file: Mutex::new(log_file_for_writer),
    };
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(Box::new(dual_writer)))
        .init();

    let args = Args::parse();
    let config = Config::load(&args.config)?;
    let is_simulation = args.is_simulation();

    // Load API key (pem_path is None in simulation if file doesn't exist, but
    // Kalshi requires auth for all endpoints including orderbook — so we always
    // try to load.  A missing file will produce a clear error at startup.)
    let pem_path_str = config.kalshi.private_key_path.clone();
    let pem_opt: Option<&str> = if pem_path_str.is_empty() || pem_path_str.contains("YOUR_") {
        None
    } else {
        Some(&pem_path_str)
    };

    let api = Arc::new(
        KalshiApi::new(
            config.kalshi.base_url.clone(),
            config.kalshi.key_id.clone(),
            pem_opt,
        )
        .context("Failed to initialise Kalshi API client")?,
    );

    eprintln!("Starting Kalshi BTC 15m Trading Bot");
    eprintln!("Mode: {}", if is_simulation { "SIMULATION" } else { "PRODUCTION" });
    eprintln!("API base: {}", config.kalshi.base_url);
    if is_simulation {
        eprintln!("PnL is calculated after each market closes (simulated trades only).");
    }

    let markets = &config.trading.markets;
    if markets.is_empty() {
        anyhow::bail!("No markets configured. Add \"btc\" to config.trading.markets.");
    }

    let cost_per_pair_max = config.trading.cost_per_pair_max;
    let min_side_price   = config.trading.min_side_price;
    let max_side_price   = config.trading.max_side_price;
    let cooldown         = config.trading.cooldown_seconds;
    let cooldown_1h      = config.trading.cooldown_seconds_1h;
    let shares_override  = config.trading.shares;
    let size_reduce_secs = config.trading.size_reduce_after_secs;
    let size_min_ratio   = config.trading.size_min_ratio;
    let size_min_shares  = config.trading.size_min_shares;

    eprintln!("Strategy: cost-per-pair lock on YES+NO < {:.2}", cost_per_pair_max);
    eprintln!("   Markets: {}", markets.join(", ").to_uppercase());
    eprintln!("   Timeframes: 15m (Kalshi KXBTC15M)");
    eprintln!("   Min/Max side price: ${:.2} – ${:.2}", min_side_price, max_side_price);
    eprintln!("   Cooldown: {}s", cooldown);
    eprintln!("   Contracts per order: {:?} (default 24)", shares_override);
    eprintln!("   Order type: FOK (fill-or-kill)");
    eprintln!();

    let trader = Arc::new(Trader::new(
        api.clone(),
        is_simulation,
        cost_per_pair_max,
        min_side_price,
        max_side_price,
        cooldown,
        cooldown_1h,
        shares_override,
        size_reduce_secs,
        size_min_ratio,
        size_min_shares,
    ));

    // Periodic profit + closure check
    let trader_closure = trader.clone();
    let market_closure_interval = config.trading.market_closure_check_interval_seconds;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            tokio::time::Duration::from_secs(market_closure_interval)
        );
        loop {
            interval.tick().await;
            if let Err(e) = trader_closure.check_market_closure().await {
                warn!("Error checking market closure: {}", e);
            }
            let total  = trader_closure.get_total_profit().await;
            let period = trader_closure.get_period_profit().await;
            if total != 0.0 || period != 0.0 {
                crate::log_println!(
                    "Current Profit — Period: ${:.2} | Total: ${:.2}",
                    period, total
                );
            }
        }
    });

    // Spawn one monitor+trader pair per configured market
    let mut handles = Vec::new();
    for asset in markets {
        let asset_upper  = asset.to_uppercase();
        let market_name  = format!("{} 15m", asset_upper);
        let series = series_ticker_for_asset(asset);

        eprintln!("Discovering {} market (series: {})...", market_name, series);
        let initial_market = discover_kalshi_market(&api, &series, &market_name).await
            .with_context(|| format!("Failed to discover initial {} market", market_name))?;

        let monitor = Arc::new(MarketMonitor::new(
            api.clone(),
            market_name.clone(),
            initial_market,
            config.trading.check_interval_ms,
        ));

        // Period-rollover watcher
        let monitor_pr    = monitor.clone();
        let api_pr        = api.clone();
        let trader_pr     = trader.clone();
        let series_pr     = series.clone();
        let market_name_pr = market_name.clone();

        let handle = tokio::spawn(async move {
            loop {
                // Sleep until close_time + a small buffer
                let close_unix = monitor_pr.get_close_time_unix().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let wait_secs = if close_unix > now {
                    close_unix - now + 5 // +5 s buffer for Kalshi to open next market
                } else {
                    10
                };
                tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;

                eprintln!("{}: Period ended — discovering next market...", market_name_pr);
                match discover_kalshi_market(&api_pr, &series_pr, &market_name_pr).await {
                    Ok(new_market) => {
                        // Only roll over if this is genuinely a new market
                        let current_ticker = monitor_pr.get_current_ticker().await;
                        if new_market.condition_id != current_ticker {
                            monitor_pr.update_market(new_market).await;
                            trader_pr.reset_period().await;
                        }
                    }
                    Err(e) => {
                        warn!("{}: Failed to discover next market: {}", market_name_pr, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                    }
                }
            }
        });
        handles.push(handle);

        // Price-feed + trading loop
        let monitor_trade = monitor.clone();
        let trader_trade  = trader.clone();
        tokio::spawn(async move {
            monitor_trade
                .start_monitoring(move |snapshot| {
                    let t = trader_trade.clone();
                    async move {
                        if let Err(e) = t.process_snapshot(&snapshot).await {
                            warn!("Error processing snapshot: {}", e);
                        }
                    }
                })
                .await;
        });
    }

    if handles.is_empty() {
        anyhow::bail!("No valid markets found. Check market configuration.");
    }

    eprintln!("Started monitoring {} market(s)", handles.len());
    futures::future::join_all(handles).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Market discovery helpers
// ---------------------------------------------------------------------------

/// Map an asset name to its Kalshi 15m series ticker.
fn series_ticker_for_asset(asset: &str) -> String {
    match asset.to_lowercase().as_str() {
        "btc" => "KXBTC15M".to_string(),
        "eth" => "KXETH15M".to_string(),
        other => format!("KX{}15M", other.to_uppercase()),
    }
}

/// Discover the currently-active market for a given Kalshi series.
/// Queries the open market list and picks the one that is open right now
/// (close_time in the future).
async fn discover_kalshi_market(
    api: &KalshiApi,
    series_ticker: &str,
    market_name: &str,
) -> Result<crate::models::Market> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let markets = api
        .get_markets_for_series(series_ticker, "open")
        .await
        .with_context(|| format!("Failed to list open {} markets", series_ticker))?;

    // Find the market whose window covers right now (open_time <= now < close_time),
    // falling back to the first open market if none matches exactly.
    let best = markets
        .iter()
        .find(|m| m.open_time_unix <= now && m.close_time_unix > now)
        .or_else(|| markets.first())
        .ok_or_else(|| anyhow::anyhow!("No open {} markets found on Kalshi", series_ticker))?;

    eprintln!(
        "Found {} market: {} | closes in ~{}s",
        market_name,
        best.condition_id,
        if best.close_time_unix > now { best.close_time_unix - now } else { 0 }
    );
    Ok(best.clone())
}

module.exports = {
  apps: [
    // ── SIMULATION MODE (safe, no real orders) ──────────────────────────────
    // pm2 start ecosystem.config.cjs --only polymarket-bot-sim
    {
      name: "polymarket-bot-sim",
      script: "./target/release/polymarket-arbitrage-bot",
      args: "--simulation --config config.json",
      cwd: __dirname,
      interpreter: "none",
      autorestart: true,
      watch: false,
      max_memory_restart: "500M",
      // Restart if bot crashes, with back-off to avoid tight crash loops
      restart_delay: 5000,
      max_restarts: 20,
      // Log files (append mode, rotate with: pm2 install pm2-logrotate)
      out_file: "./logs/bot-sim-out.log",
      error_file: "./logs/bot-sim-err.log",
      log_date_format: "YYYY-MM-DDTHH:mm:ss",
      merge_logs: true,
    },

    // ── PRODUCTION MODE (real orders — fill in config.json credentials first) ─
    // pm2 start ecosystem.config.cjs --only polymarket-bot-live
    {
      name: "polymarket-bot-live",
      script: "./target/release/polymarket-arbitrage-bot",
      args: "--production --config config.json",
      cwd: __dirname,
      interpreter: "none",
      autorestart: true,
      watch: false,
      max_memory_restart: "500M",
      restart_delay: 10000,
      max_restarts: 10,
      out_file: "./logs/bot-live-out.log",
      error_file: "./logs/bot-live-err.log",
      log_date_format: "YYYY-MM-DDTHH:mm:ss",
      merge_logs: true,
    },
  ],
};

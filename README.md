# Polymarket BTC 5-Min Up/Down Bot (Rust, Paper Mode)

This bot is specifically for **Polymarket 5-minute BTC Up/Down markets** and is designed to run in a **24/7 loop**.

It uses:
- live market data from Polymarket Gamma API,
- a deterministic side sequence (default: `UP,DOWN,UP,DOWN,...`),
- stake sizing with your requested recovery formula:  
  `Stake = (Cumulative Losses + Target Profit) / (Odds - 1)`,
- trade entry guidance: place bets immediately after market opens, but ensure a bet is placed within the active 5-minute trading period,
- and real resolved winners for settlement/PnL.

Default target profit is **$1.00**.

## What it does

1. Polls active Polymarket markets.
2. Filters to BTC + 5-minute + Up/Down questions.
3. Opens one paper position per unseen market using the next side from your sequence.
4. Watches for market resolution and settles against the actual winner.
5. Keeps running until stopped manually, or until an explicit stop time/duration is provided.

> This is a paper-trading simulator. It does not place on-chain orders.

## Run

```bash
cargo run -- --target-profit 1 --poll-seconds 30 --sequence UP,DOWN
```

> Hardcoded fallback sequence in code is `["UP", "DOWN"]`. You can override with `--sequence` or `--sequence-pdf`.

### Useful options

- `--target-profit <USD>`: target used in stake sizing formula (default `1.0`), not a stop condition
- `--max-stake-usd <USD>`: hard cap on computed stake to control risk (default `250`)
- `--poll-seconds <sec>`: polling interval for open/resolved markets (default `30`)
- `--limit <N>`: per-request API market cap (default `200`)
- `--sequence <CSV>`: deterministic side order, e.g. `UP,UP,DOWN`
- `--sequence-pdf <path>`: load sequence tokens (`Up`/`Down`) from a strategy PDF
- `--normalize-prices <bool>`: normalize overround before odds conversion (default `true`)
- `--new-period-reset-minutes <N>`: inactivity timeout to trigger New Trading Period reset (default `180`)
- `--stop-after-minutes <N>`: optional runtime duration after which bot exits
- `--cycles <N>`: optional finite cycles for testing (without this it runs continuously)

## 24/7 behavior

By default, the process loops forever and sleeps between polls. Use a process manager for production-style uptime, for example:
- `systemd`
- `supervisord`
- Docker restart policies

## Notes

- Settlements are based on resolved market winner from Polymarket API.
- Entry timing:
  - bet is recommended immediately after market opens (usually within seconds)
  - if delayed, bot still allows entries as long as market is within the 5-minute trading period
  - if `startDateIso` is unavailable, it approximates start as `endDateIso - 5 minutes`
- Sequence reset:
  - sequence index starts from the top whenever the bot process starts
  - restarting the bot resets the sequence to the first configured side
  - New Trading Period also forces first direction = **Up**
- Recovery behavior:
  - after a **loss**, cumulative losses increase and next computed stake increases
  - after a **win**, cumulative losses reset to `0`
- PnL model:
  - Shares bought = `stake_usd / entry_price`
  - If side wins: payout = `shares * 1.0`
  - Else payout = `0`
  - Trade PnL = `payout - stake_usd`

## Odds conversion clarification (57¢ / 44¢)

Raw implied probabilities and decimal odds:

| Side | Price | Implied Prob | Raw Odds (`1/p`) |
|---|---:|---:|---:|
| Up | 0.57 | 57.00% | 1.754 |
| Down | 0.44 | 44.00% | 2.273 |

These sum to `1.01` (1¢ overround). With normalization enabled (`--normalize-prices true`), the bot uses:

| Side | Normalized Prob | Normalized Odds |
|---|---:|---:|
| Up | 57/101 = 56.44% | 1.772 |
| Down | 44/101 = 43.56% | 2.296 |

This is consistent with Polymarket payout math: paying 57¢ wins $1.00 gross payout, so odds remain `1 / 0.57` in raw mode.

## Windows install (CMD) and run (paper mode)

1. Install prerequisites:
   - Rust (stable): https://www.rust-lang.org/tools/install
   - Python 3.x
   - Git
2. Open **Command Prompt** and clone:
   ```cmd
   git clone <YOUR_REPO_URL>
   cd PolymarketBot
   ```
3. Build:
   ```cmd
   cargo build --release
   ```
4. Run paper bot:
   ```cmd
   cargo run --release -- --target-profit 1 --poll-seconds 30 --sequence UP,DOWN
   ```
5. Optional PDF sequence:
   ```cmd
   cargo run --release -- --sequence-pdf strategy.pdf --poll-seconds 30
   ```

## How to test (paper trading)

1. Start with short loop tests:
   ```cmd
   cargo run -- --cycles 3 --poll-seconds 10
   ```
2. Validate:
   - sequence starts with **UP** on new period,
   - stake uses formula output,
   - position settles before next position opens,
   - PnL/recovery reset behavior is correct.
3. Dry-run longer:
   ```cmd
   cargo run -- --stop-after-minutes 120 --poll-seconds 30
   ```
4. Keep logs:
   ```cmd
   cargo run -- --stop-after-minutes 120 > bot.log 2>&1
   ```

## Host on VPS (Linux)

1. Provision Ubuntu VPS.
2. Install dependencies:
   ```bash
   sudo apt update
   sudo apt install -y curl git python3 python3-pip build-essential
   curl https://sh.rustup.rs -sSf | sh -s -- -y
   source "$HOME/.cargo/env"
   ```
3. Clone and build:
   ```bash
   git clone <YOUR_REPO_URL>
   cd PolymarketBot
   cargo build --release
   ```
4. Run in screen/tmux (simple):
   ```bash
   ./target/release/polymarket_bot --poll-seconds 30 --stop-after-minutes 0
   ```
5. Recommended: create a `systemd` service for auto-restart.

## Telegram notifications (integration outline)

This repo currently has no Telegram sender built in. Add one of these:
- Rust HTTP call to Telegram Bot API (`sendMessage`) on open/close events.
- Or pipe logs to a small Python script that posts to Telegram.

Minimum Telegram setup:
1. Create bot with `@BotFather`.
2. Get `BOT_TOKEN`.
3. Get your `CHAT_ID`.
4. Add notifier call for important events:
   - new trade opened,
   - settlement/win/loss,
   - errors and restart alerts.

## Real-money deployment (after paper mode is proven)

Important: current implementation is **paper-trading only**.
To trade real money, you must add:

1. Polymarket CLOB trading integration (authenticated signed orders).
2. Private key management (secure vault/environment, never hardcoded).
3. Risk controls:
   - max daily loss,
   - max stake hard limit,
   - circuit breaker on API failures/slippage.
4. Audit logging and replayable trade journal.
5. Sandbox/staging rollout before live funds.

Suggested go-live sequence:
1. Pass 1-2 weeks of paper stability.
2. Start with tiny real size.
3. Monitor fills/slippage/latency.
4. Gradually scale only if controls hold.

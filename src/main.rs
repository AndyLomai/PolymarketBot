use std::collections::{HashMap, HashSet};
use std::env;
use std::error::Error;
use std::process::Command;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const DEFAULT_TARGET_PROFIT: f64 = 1.0;
const DEFAULT_MAX_STAKE_USD: f64 = 250.0;
const DEFAULT_POLL_SECONDS: u64 = 30;
const DEFAULT_LIMIT: u32 = 200;
const DEFAULT_SEQUENCE: [&str; 2] = ["UP", "DOWN"];
const DEFAULT_NORMALIZE_PRICES: bool = true;
const DEFAULT_NEW_PERIOD_RESET_MINUTES: u64 = 180;

#[derive(Debug, Clone)]
struct Args {
    target_profit: f64,
    max_stake_usd: f64,
    poll_seconds: u64,
    limit: u32,
    sequence: Vec<String>,
    sequence_pdf: Option<String>,
    normalize_prices: bool,
    new_period_reset_minutes: u64,
    stop_after_minutes: Option<u64>,
    cycles: Option<u32>,
}

#[derive(Debug, Clone)]
struct OpenMarket {
    slug: String,
    question: String,
    end_date: String,
    outcomes: Vec<String>,
    prices: Vec<f64>,
}

#[derive(Debug, Clone)]
struct Position {
    slug: String,
    question: String,
    end_date: String,
    side: String,
    entry_price: f64,
    stake_usd: f64,
    shares: f64,
    opened_cycle: u32,
    recovery_losses_before_trade: f64,
}

#[derive(Debug, Clone)]
struct ClosedMarket {
    winner: String,
    closed_date: String,
}

#[derive(Debug, Clone)]
struct SettledTrade {
    slug: String,
    question: String,
    opened_cycle: u32,
    recovery_losses_before_trade: f64,
    closed_date: String,
    side: String,
    winner: String,
    entry_price: f64,
    stake_usd: f64,
    pnl: f64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = parse_args()?;

    if let Some(path) = &args.sequence_pdf {
        match load_sequence_from_pdf(path) {
            Ok(from_pdf) if !from_pdf.is_empty() => {
                println!("Loaded trade sequence from PDF: {path}");
                args.sequence = from_pdf;
            }
            Ok(_) => {
                eprintln!(
                    "WARN: sequence PDF parsed but no UP/DOWN tokens were found; using --sequence."
                );
            }
            Err(err) => {
                eprintln!("WARN: failed to load sequence from PDF ({path}): {err}");
            }
        }
    }

    println!("Starting BTC 5-minute Up/Down Polymarket bot (paper mode)");
    println!(
        "Runs continuously until manually stopped, unless --cycles/--stop-after-minutes is provided."
    );
    println!("Target daily profit: ${:.2}", args.target_profit);
    println!("Stake formula: (cumulative_losses + target_profit) / (odds - 1)");
    println!("Max allowed stake: ${:.2}", args.max_stake_usd);
    println!("Normalize overround before odds: {}", args.normalize_prices);
    println!(
        "Entry timing guidance: place bet immediately after open; hard limit is within the 5-minute trading period."
    );
    println!(
        "New trading period reset after {} minutes of inactivity",
        args.new_period_reset_minutes
    );
    println!("Sequence: {}", args.sequence.join(" -> "));
    println!("------------------------------------------------------");

    let mut open_positions: HashMap<String, Position> = HashMap::new();
    let mut seen_markets: HashSet<String> = HashSet::new();
    let mut settled: Vec<SettledTrade> = Vec::new();
    let mut cumulative_pnl = 0.0;
    let mut cycle = 0_u32;
    let mut sequence_idx = 0_usize;
    let mut recovery_losses = 0.0_f64;
    let mut force_up_on_next_trade = true;
    let mut last_trade_activity_at: Option<Instant> = None;
    let started_at = Instant::now();

    loop {
        cycle += 1;
        println!("\nCycle #{cycle}");

        let should_reset_period = open_positions.is_empty()
            && last_trade_activity_at
                .map(|last| {
                    last.elapsed().as_secs() > args.new_period_reset_minutes.saturating_mul(60)
                })
                .unwrap_or(false);

        if should_reset_period {
            recovery_losses = 0.0;
            sequence_idx = 0;
            force_up_on_next_trade = true;
            println!("New Trading Period detected after inactivity -> losses reset, first side=UP");
        }

        // 1) Open at most one position (must wait for resolution before next trade).
        if open_positions.is_empty() {
            match fetch_open_btc_5m_markets(args.limit) {
                Ok(markets) => {
                    println!("Found {} candidate active BTC 5m markets.", markets.len());
                    for market in markets {
                        if seen_markets.contains(&market.slug) {
                            continue;
                        }

                        let desired = next_direction(
                            &args.sequence,
                            &mut sequence_idx,
                            &mut force_up_on_next_trade,
                        );
                        if let Some((idx, actual_side)) = pick_side_index(&market.outcomes, desired)
                        {
                            let entry_price = market.prices[idx];
                            if !(0.0..1.0).contains(&entry_price) {
                                continue;
                            }

                            let odds = odds_for_selected_outcome(
                                entry_price,
                                &market.prices,
                                args.normalize_prices,
                            );
                            let mut current_stake = required_stake_for_target(
                                recovery_losses,
                                args.target_profit,
                                odds,
                            );
                            if current_stake > args.max_stake_usd {
                                current_stake = args.max_stake_usd;
                            }
                            if current_stake <= 0.0 {
                                continue;
                            }
                            let shares = current_stake / entry_price;
                            let position = Position {
                                slug: market.slug.clone(),
                                question: market.question.clone(),
                                end_date: market.end_date.clone(),
                                side: actual_side.clone(),
                                entry_price,
                                stake_usd: current_stake,
                                shares,
                                opened_cycle: cycle,
                                recovery_losses_before_trade: recovery_losses,
                            };

                            println!(
                                "OPEN  slug={} side={} losses_before=${:.3} stake=${:.2} entry=${:.3} odds={:.3} shares={:.4} end={}",
                                market.slug,
                                actual_side,
                                recovery_losses,
                                current_stake,
                                entry_price,
                                odds,
                                shares,
                                market.end_date
                            );

                            seen_markets.insert(market.slug.clone());
                            open_positions.insert(market.slug, position);
                            last_trade_activity_at = Some(Instant::now());
                            break;
                        }
                    }
                }
                Err(err) => {
                    eprintln!("WARN: open market fetch failed: {err}");
                }
            }
        } else {
            println!(
                "Waiting for current market to resolve before opening next trade (open positions: {}).",
                open_positions.len()
            );
        }

        // 2) Attempt to settle any open positions by checking resolved winners.
        if !open_positions.is_empty() {
            let slugs: Vec<String> = open_positions.keys().cloned().collect();
            match fetch_closed_results_for_slugs(args.limit * 5, &slugs) {
                Ok(resolved) => {
                    for slug in slugs {
                        let Some(closed) = resolved.get(&slug) else {
                            continue;
                        };

                        let Some(position) = open_positions.remove(&slug) else {
                            continue;
                        };

                        let payout = if same_side(&position.side, &closed.winner) {
                            position.shares * 1.0
                        } else {
                            0.0
                        };
                        let pnl = payout - position.stake_usd;
                        cumulative_pnl += pnl;

                        println!(
                            "CLOSE slug={} side={} losses_before=${:.3} winner={} end={} pnl=${:.3} cumulative=${:.3}",
                            position.slug,
                            position.side,
                            position.recovery_losses_before_trade,
                            closed.winner,
                            position.end_date,
                            pnl,
                            cumulative_pnl
                        );

                        settled.push(SettledTrade {
                            slug: position.slug,
                            question: position.question,
                            opened_cycle: position.opened_cycle,
                            closed_date: closed.closed_date.clone(),
                            side: position.side,
                            winner: closed.winner.clone(),
                            entry_price: position.entry_price,
                            stake_usd: position.stake_usd,
                            pnl,
                            recovery_losses_before_trade: position.recovery_losses_before_trade,
                        });

                        if pnl > 0.0 {
                            recovery_losses = 0.0;
                        } else {
                            recovery_losses += -pnl;
                        }
                        last_trade_activity_at = Some(Instant::now());
                    }
                }
                Err(err) => {
                    eprintln!("WARN: closed market fetch failed: {err}");
                }
            }
        }

        if cumulative_pnl >= args.target_profit {
            println!(
                "✅ Target PnL reached (${:.3}) — continuing to run until manually stopped or stop-time condition.",
                cumulative_pnl
            );
        }

        if let Some(stop_after_minutes) = args.stop_after_minutes {
            if started_at.elapsed().as_secs() >= stop_after_minutes.saturating_mul(60) {
                println!("\nReached --stop-after-minutes={stop_after_minutes}. Stopping.");
                print_final_report(&settled, cumulative_pnl, args.target_profit);
                break;
            }
        }

        if let Some(max_cycles) = args.cycles {
            if cycle >= max_cycles {
                println!("\nReached --cycles={max_cycles}. Stopping.");
                print_final_report(&settled, cumulative_pnl, args.target_profit);
                break;
            }
        }

        println!(
            "Sleeping {}s... (open positions: {})",
            args.poll_seconds,
            open_positions.len()
        );
        thread::sleep(Duration::from_secs(args.poll_seconds));
    }

    Ok(())
}

fn parse_args() -> Result<Args, Box<dyn Error>> {
    let mut args = Args {
        target_profit: DEFAULT_TARGET_PROFIT,
        max_stake_usd: DEFAULT_MAX_STAKE_USD,
        poll_seconds: DEFAULT_POLL_SECONDS,
        limit: DEFAULT_LIMIT,
        sequence: default_sequence_vec(),
        sequence_pdf: None,
        normalize_prices: DEFAULT_NORMALIZE_PRICES,
        new_period_reset_minutes: DEFAULT_NEW_PERIOD_RESET_MINUTES,
        stop_after_minutes: None,
        cycles: None,
    };

    let argv: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        match argv[i].as_str() {
            "--target-profit" => {
                i += 1;
                args.target_profit = argv
                    .get(i)
                    .ok_or("missing value for --target-profit")?
                    .parse()?;
            }
            "--max-stake-usd" => {
                i += 1;
                args.max_stake_usd = argv
                    .get(i)
                    .ok_or("missing value for --max-stake-usd")?
                    .parse()?;
            }
            "--poll-seconds" => {
                i += 1;
                args.poll_seconds = argv
                    .get(i)
                    .ok_or("missing value for --poll-seconds")?
                    .parse()?;
            }
            "--limit" => {
                i += 1;
                args.limit = argv.get(i).ok_or("missing value for --limit")?.parse()?;
            }
            "--sequence" => {
                i += 1;
                let raw = argv.get(i).ok_or("missing value for --sequence")?;
                args.sequence = parse_sequence(raw);
            }
            "--sequence-pdf" => {
                i += 1;
                args.sequence_pdf = Some(
                    argv.get(i)
                        .ok_or("missing value for --sequence-pdf")?
                        .to_string(),
                );
            }
            "--normalize-prices" => {
                i += 1;
                let raw = argv.get(i).ok_or("missing value for --normalize-prices")?;
                args.normalize_prices = parse_bool(raw)?;
            }
            "--new-period-reset-minutes" => {
                i += 1;
                args.new_period_reset_minutes = argv
                    .get(i)
                    .ok_or("missing value for --new-period-reset-minutes")?
                    .parse()?;
            }
            "--stop-after-minutes" => {
                i += 1;
                args.stop_after_minutes = Some(
                    argv.get(i)
                        .ok_or("missing value for --stop-after-minutes")?
                        .parse()?,
                );
            }
            "--cycles" => {
                i += 1;
                args.cycles = Some(argv.get(i).ok_or("missing value for --cycles")?.parse()?);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => return Err(format!("unknown flag: {other}").into()),
        }
        i += 1;
    }

    if args.target_profit <= 0.0 {
        return Err("--target-profit must be > 0".into());
    }
    if args.max_stake_usd <= 0.0 {
        return Err("--max-stake-usd must be > 0".into());
    }
    if args.poll_seconds == 0 {
        return Err("--poll-seconds must be > 0".into());
    }
    if args.sequence.is_empty() {
        return Err("--sequence must include at least one side (UP or DOWN)".into());
    }

    Ok(args)
}

fn print_help() {
    println!("Polymarket BTC 5-minute Up/Down bot (paper mode, 24/7 loop)");
    println!("\nFlags:");
    println!("  --target-profit <USD>   Profit target used in stake sizing formula (default 1.0)");
    println!("  --max-stake-usd <USD>   Hard cap on computed stake to control risk (default 250)");
    println!("  --poll-seconds <sec>    Polling interval for open/resolved markets (default 30)");
    println!("  --limit <N>             Number of markets to fetch per call (default 200)");
    println!("  --sequence <CSV>        Deterministic side sequence, e.g. UP,DOWN,UP");
    println!("  --sequence-pdf <path>   Load sequence tokens (Up/Down) from a strategy PDF");
    println!(
        "  --normalize-prices <bool>  Normalize overround before odds conversion (default true)"
    );
    println!(
        "  --new-period-reset-minutes <N>  Inactivity timeout to trigger New Trading Period reset (default 180)"
    );
    println!("  --stop-after-minutes <N>  Optional runtime duration after which bot exits");
    println!(
        "  --cycles <N>            Optional finite cycles for testing (without this it runs continuously)"
    );
}

fn parse_sequence(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn default_sequence_vec() -> Vec<String> {
    DEFAULT_SEQUENCE.iter().map(|s| s.to_string()).collect()
}

fn parse_bool(value: &str) -> Result<bool, Box<dyn Error>> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" => Ok(true),
        "0" | "false" | "no" | "n" => Ok(false),
        _ => Err(format!("invalid boolean value: {value}").into()),
    }
}

fn load_sequence_from_pdf(path: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let py = r#"
import re, sys
path = sys.argv[1]
text = ''
try:
    from pypdf import PdfReader
    reader = PdfReader(path)
    text = '\n'.join([(p.extract_text() or '') for p in reader.pages])
except Exception:
    try:
        with open(path, 'rb') as f:
            text = f.read().decode('latin-1', errors='ignore')
    except Exception as e:
        print(f'ERR\t{e}')
        raise SystemExit(2)
tokens = re.findall(r'\\b(up|down)\\b', text, flags=re.IGNORECASE)
for t in tokens:
    print(t.upper())
"#;

    let output = Command::new("python")
        .arg("-c")
        .arg(py)
        .arg(path)
        .output()?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("python failed while reading sequence PDF: {err}").into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let sequence = stdout
        .lines()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| s == "UP" || s == "DOWN")
        .collect::<Vec<_>>();

    Ok(sequence)
}

fn next_direction<'a>(
    sequence: &'a [String],
    sequence_idx: &mut usize,
    force_up_on_next_trade: &mut bool,
) -> &'a str {
    if *force_up_on_next_trade {
        *force_up_on_next_trade = false;
        if sequence
            .get(*sequence_idx % sequence.len())
            .map(|s| s.eq_ignore_ascii_case("UP"))
            .unwrap_or(false)
        {
            *sequence_idx = (*sequence_idx + 1) % sequence.len();
        }
        return "UP";
    }

    let direction = sequence
        .get(*sequence_idx % sequence.len())
        .map(String::as_str)
        .unwrap_or("UP");
    *sequence_idx = (*sequence_idx + 1) % sequence.len();
    direction
}

fn pick_side_index(outcomes: &[String], desired: &str) -> Option<(usize, String)> {
    let desired_norm = desired.trim().to_ascii_uppercase();
    for (idx, side) in outcomes.iter().enumerate() {
        let norm = side.trim().to_ascii_uppercase();
        if norm.contains(&desired_norm) {
            return Some((idx, side.clone()));
        }
    }
    None
}

fn same_side(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn fetch_open_btc_5m_markets(limit: u32) -> Result<Vec<OpenMarket>, Box<dyn Error>> {
    let py = r#"
import json, sys, urllib.request, urllib.parse
limit = int(sys.argv[1])
url = 'https://gamma-api.polymarket.com/markets?' + urllib.parse.urlencode({
    'active': 'true',
    'closed': 'false',
    'limit': str(limit),
    'order': 'endDate',
    'ascending': 'true',
})
with urllib.request.urlopen(url, timeout=25) as resp:
    markets = json.loads(resp.read().decode('utf-8'))
for m in markets:
    q = (m.get('question') or '')
    ql = q.lower()
    if 'bitcoin' not in ql and 'btc' not in ql:
        continue
    if '5 min' not in ql and '5m' not in ql:
        continue
    if 'up' not in ql or 'down' not in ql:
        continue
    outcomes = m.get('outcomes')
    prices = m.get('outcomePrices')
    if not outcomes or not prices:
        continue
    try:
        o = [str(x) for x in json.loads(outcomes)]
        p = [float(x) for x in json.loads(prices)]
    except Exception:
        continue
    if len(o) != len(p) or len(o) < 2:
        continue
    if any((x <= 0 or x >= 1) for x in p):
        continue
    slug = str(m.get('slug') or '').replace('\t',' ').replace('\n',' ')
    if not slug:
        continue
    import datetime
    start_iso = str(m.get('startDateIso') or '')
    end_iso = str(m.get('endDateIso') or '')
    if not start_iso and end_iso:
        try:
            end_dt = datetime.datetime.fromisoformat(end_iso.replace('Z', '+00:00'))
            start_dt = end_dt - datetime.timedelta(minutes=5)
            start_iso = start_dt.isoformat()
        except Exception:
            start_iso = ''
    if start_iso:
        try:
            now = datetime.datetime.now(datetime.timezone.utc)
            start_dt = datetime.datetime.fromisoformat(start_iso.replace('Z', '+00:00'))
            age_seconds = (now - start_dt).total_seconds()
            if age_seconds < 0 or age_seconds > 300:
                continue
        except Exception:
            continue
    else:
        continue
    end_date = end_iso
    q = q.replace('\t',' ').replace('\n',' ')
    print(f"{slug}\t{q}\t{end_date}\t{','.join(o)}\t{','.join(str(x) for x in p)}")
"#;

    let output = Command::new("python")
        .arg("-c")
        .arg(py)
        .arg(limit.to_string())
        .output()?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("failed to fetch open markets: {err}").into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(5, '\t');
        let Some(slug) = parts.next() else { continue };
        let Some(question) = parts.next() else {
            continue;
        };
        let Some(end_date) = parts.next() else {
            continue;
        };
        let Some(outcomes_raw) = parts.next() else {
            continue;
        };
        let Some(prices_raw) = parts.next() else {
            continue;
        };

        let outcomes: Vec<String> = outcomes_raw
            .split(',')
            .map(|s| s.trim().to_string())
            .collect();
        let mut prices = Vec::new();
        for p in prices_raw.split(',') {
            if let Ok(v) = p.trim().parse::<f64>() {
                prices.push(v);
            }
        }
        if outcomes.len() != prices.len() || outcomes.is_empty() {
            continue;
        }

        out.push(OpenMarket {
            slug: slug.to_string(),
            question: question.to_string(),
            end_date: end_date.to_string(),
            outcomes,
            prices,
        });
    }

    Ok(out)
}

fn fetch_closed_results_for_slugs(
    limit: u32,
    slugs: &[String],
) -> Result<HashMap<String, ClosedMarket>, Box<dyn Error>> {
    if slugs.is_empty() {
        return Ok(HashMap::new());
    }

    let slug_csv = slugs.join(",");
    let py = r#"
import json, sys, urllib.request, urllib.parse
limit = int(sys.argv[1])
targets = set([x for x in sys.argv[2].split(',') if x])
url = 'https://gamma-api.polymarket.com/markets?' + urllib.parse.urlencode({
    'closed': 'true',
    'limit': str(limit),
    'order': 'endDate',
    'ascending': 'false',
})
with urllib.request.urlopen(url, timeout=25) as resp:
    markets = json.loads(resp.read().decode('utf-8'))
for m in markets:
    slug = str(m.get('slug') or '')
    if slug not in targets:
        continue
    outcomes = m.get('outcomes')
    prices = m.get('outcomePrices')
    if not outcomes or not prices:
        continue
    try:
        o = [str(x) for x in json.loads(outcomes)]
        p = [float(x) for x in json.loads(prices)]
    except Exception:
        continue
    if len(o) != len(p) or not o:
        continue
    best = max(range(len(p)), key=lambda i: p[i])
    if abs(p[best] - 1.0) > 1e-9:
        continue
    winner = o[best].replace('\t',' ').replace('\n',' ')
    closed = str(m.get('endDateIso') or '')
    print(f"{slug}\t{winner}\t{closed}")
"#;

    let output = Command::new("python")
        .arg("-c")
        .arg(py)
        .arg(limit.to_string())
        .arg(slug_csv)
        .output()?;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(format!("failed to fetch closed market results: {err}").into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = HashMap::new();

    for line in stdout.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(slug) = parts.next() else { continue };
        let Some(winner) = parts.next() else { continue };
        let Some(closed_date) = parts.next() else {
            continue;
        };
        out.insert(
            slug.to_string(),
            ClosedMarket {
                winner: winner.to_string(),
                closed_date: closed_date.to_string(),
            },
        );
    }

    Ok(out)
}

fn binary_decimal_odds(entry_price: f64) -> f64 {
    1.0 / entry_price
}

fn normalized_decimal_odds(entry_price: f64, prices: &[f64]) -> f64 {
    let total: f64 = prices.iter().copied().sum();
    if total <= 0.0 {
        return binary_decimal_odds(entry_price);
    }
    let normalized_probability = entry_price / total;
    if normalized_probability <= 0.0 {
        return binary_decimal_odds(entry_price);
    }
    1.0 / normalized_probability
}

fn odds_for_selected_outcome(entry_price: f64, prices: &[f64], normalize: bool) -> f64 {
    if normalize {
        normalized_decimal_odds(entry_price, prices)
    } else {
        binary_decimal_odds(entry_price)
    }
}

fn required_stake_for_target(cumulative_losses: f64, target_profit: f64, odds: f64) -> f64 {
    if odds <= 1.0 {
        return 0.0;
    }
    (cumulative_losses + target_profit) / (odds - 1.0)
}

fn print_final_report(settled: &[SettledTrade], cumulative_pnl: f64, target_profit: f64) {
    println!("\nFinal report");
    println!("----------------------------------------------");
    println!("Settled trades: {}", settled.len());
    for (i, t) in settled.iter().enumerate() {
        println!(
            "{}. cycle={} close={} losses_before=${:.3} side={} winner={} entry=${:.3} stake=${:.2} pnl=${:.3}",
            i + 1,
            t.opened_cycle,
            t.closed_date,
            t.recovery_losses_before_trade,
            t.side,
            t.winner,
            t.entry_price,
            t.stake_usd,
            t.pnl
        );
        println!("   {}", t.question);
        println!("   https://polymarket.com/event/{}", t.slug);
    }
    println!("----------------------------------------------");
    println!("Cumulative PnL: ${:.3}", cumulative_pnl);
    println!("Target PnL:     ${:.3}", target_profit);
}

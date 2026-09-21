/**
 * Wire types for the agent's dashboard API.
 *
 * Decimal-valued fields are typed `string` because `rust_decimal` is compiled
 * with `serde-str`; integer counts are real numbers. Read every one of them
 * through `num()` rather than trusting the type at a call site.
 */

/** Any handler can return this *with HTTP 200*. See `narrow.ts`. */
export interface ApiErrorBody {
  error: string;
}

/**
 * `/api/health`. Public — it answers without a token.
 *
 * `next_cycle_due` and `alerts_delivering` exist only on builds that include
 * the cycle-watchdog work; an older agent omits the keys entirely. Test with
 * `"next_cycle_due" in health`, never for truthiness: on a new build the value
 * is legitimately `null` until the first cycle has been scheduled.
 */
export interface Health {
  status: string;
  agent_state: string;
  cycle_number: number;
  started_at: string;
  last_cycle_at: string | null;
  uptime_seconds: number;
  next_cycle_due?: string | null;
  alerts_delivering?: boolean;
  /**
   * Occurrences per anomaly kind since the agent started, non-zero only.
   * Absent on builds before the circuit breakers.
   */
  anomalies?: Record<string, number>;
  /** Whether new positions are stopped, and why. */
  halted?: boolean;
  halt?: Halt | null;
}

/** A halt in force, as `/api/halt` and `/api/health` report it. */
export interface Halt {
  source: "halt_file" | "api" | "signal" | "circuit_breaker" | "reconciliation";
  /** `rest_of_day` lifts at the UTC rollover; `until_resume` does not. */
  scope: "rest_of_day" | "until_resume";
  detail: string;
  at: string;
  day: string;
}

/** `/api/orders`. What was asked for, and what came back. */
export interface OrderRecord {
  id: number | null;
  client_order_id: string;
  venue_order_id: string | null;
  venue_id: string;
  symbol: string;
  side: string;
  intent: string;
  trade_id: number | null;
  limit_price: string | null;
  qty: string;
  filled_qty: string;
  avg_fill_price: string | null;
  state: string;
  reject_reason: string | null;
  cycle: number | null;
  submitted_at: string | null;
  updated_at: string | null;
  expires_at: string | null;
}

/** `/api/fills`. One execution; an order can have several. */
export interface FillRecord {
  id: number;
  order_id: number;
  client_order_id: string;
  venue_id: string;
  symbol: string;
  side: string;
  intent: string;
  qty: string;
  price: string;
  fee: string | null;
  mid_at_submit: string | null;
  slippage_bps: string | null;
  time_to_fill_ms: number | null;
  filled_at: string | null;
}

/** `/api/equity`. One row per UTC day. */
export interface DailyEquity {
  day: string;
  starting_equity: string;
  high_water_mark: string;
  closing_equity: string | null;
  realized_pnl: string | null;
  updated_at: string | null;
}

/** `/api/reconciliation`. */
export interface ReconciliationRun {
  id: number;
  venue_id: string;
  cycle: number | null;
  balance_delta: string | null;
  positions_missing_locally: number;
  positions_missing_on_venue: number;
  qty_mismatches: number;
  unknown_open_orders: number;
  passed: boolean;
  detail: string | null;
  created_at: string | null;
}

/** `/api/risk`. Headroom under each circuit breaker. */
export interface RiskStatus {
  mode: string;
  day: string;
  starting_equity: string | null;
  current_equity: string | null;
  high_water_mark: string | null;
  trades_today: number;
  consecutive_losses: number;
  limits: {
    max_daily_loss_pct: string;
    max_daily_loss_usd: string;
    max_drawdown_pct: string;
    max_trades_per_day: number;
    max_consecutive_losses: number;
    max_live_notional_per_position_usd: string;
    max_live_total_notional_usd: string;
    live_caps_apply: boolean;
  };
}

/** `POST /api/halt` and `POST /api/resume`. */
export interface HaltResponse {
  halted: boolean;
  newly_halted?: boolean;
  note?: string;
  halt?: Halt | null;
  cleared?: Halt | null;
  error?: string;
}

/** `/api/metrics`. */
export interface Metrics {
  total_trades: number;
  open_trades: number;
  resolved_trades: number;
  wins: number;
  losses: number;
  /** Fraction, not a percent. */
  win_rate: string;
  total_pnl: string;
  realized_pnl: string;
  unrealized_exposure: string;
  /** Fraction. */
  avg_edge_at_entry: string;
  avg_position_size: string;
  total_api_cost: string;
  net_profit: string;
  /** Fraction despite the name — `net_profit / initial_bankroll`. */
  roi_pct: string;
  /** Null until at least two trades have resolved. */
  sharpe_ratio: string | null;
  cycles_completed: number;
  /** A real f64, not a Decimal — so a number or null, not a string. */
  avg_cycle_duration_ms: number | null;
}

/**
 * `/api/trades` and `/api/trades/all`.
 *
 * The `trades` table also carries `venue_id, symbol, asset_class, side,
 * quantity, avg_fill_price, fees, mark_price, unrealized_pnl, realized_pnl,
 * venue_order_id, client_order_id, exit_order_id, stop_price, target_price,
 * horizon_hours, max_hold_until, close_reason, closed_at, marked_at` — but the
 * server's `TradeRecord` declares only the fields below, and sqlx silently
 * drops the rest on the way out. They are NOT available to this UI. Adding
 * columns here will not make them appear; that needs a change in
 * `src/db/store.rs`.
 */
export interface TradeRecord {
  id: number | null;
  cycle: number;
  market_id: string;
  market_question: string | null;
  direction: string;
  entry_price: string;
  size: string;
  /** Fraction. */
  edge_at_entry: string;
  claude_fair_value: string;
  /** Fraction. */
  confidence: string;
  /** Fraction. */
  kelly_raw: string;
  /** Fraction. */
  kelly_adjusted: string;
  status: string;
  pnl: string | null;
  /** SQLite `datetime('now')` — bare UTC, no zone. Use `parseTs`. */
  created_at: string | null;
  resolved_at: string | null;
}

/** Every value the `trades.status` CHECK constraint allows. */
export const TRADE_STATUSES = [
  "PENDING",
  "PARTIAL",
  "OPEN",
  "FILLED",
  "CLOSED",
  "RESOLVED_WIN",
  "RESOLVED_LOSS",
  "CANCELLED",
] as const;
export type TradeStatus = (typeof TRADE_STATUSES)[number];

/** `/api/cycles` (one, or bare null) and `/api/cycles/all`. */
export interface CycleRecord {
  id: number | null;
  /** Starts at 0. Never test this for truthiness. */
  cycle_number: number;
  markets_scanned: number | null;
  opportunities_found: number | null;
  trades_placed: number | null;
  api_cost: string | null;
  bankroll: string | null;
  unrealized_pnl: string | null;
  agent_state: string;
  duration_ms: number | null;
  /** Bare UTC, no zone. Use `parseTs`. */
  created_at: string | null;
}

/** `/api/costs`. */
export interface ApiCostRecord {
  id: number | null;
  provider: string;
  endpoint: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  cost: string;
  cycle: number | null;
  /** Bare UTC, no zone. Use `parseTs`. */
  created_at: string | null;
}

/**
 * `/api/venues`. Which platforms this agent trades, and whether each one is
 * actually working.
 *
 * `enabled` is what the config asked for; `active` is what the registry
 * actually built. When they disagree, `reason` says why — missing
 * credentials, a typo in `kind`, or a refusal to build a live-only venue
 * outside live mode.
 */
export interface VenueStatus {
  id: string;
  kind: string;
  enabled: boolean;
  active: boolean;
  reason: string | null;
  symbols: string[];
  /** Decimal-as-string, like every other money value on this API. */
  fee_pct: string;
  asset_classes: string[];
  session: string;
  reports_equity: boolean;
  paper_trading: boolean;
}
